//! OIDC authorization for the bounded operator surface.
//!
//! Operator identity is deliberately separate from virtual-key data-plane
//! authentication. The gateway validates short-lived RS256 JWTs against an
//! explicitly configured JWKS endpoint; it never creates sessions or stores
//! provider/account credentials.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use reqwest::Client;
use ring::signature;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, RwLock};
use wayfinder_config::gateway::GatewayAuthConfig;

use crate::AppState;
use crate::audit::AuditActor;

const JWKS_CACHE_TTL: Duration = Duration::from_secs(300);
const JWKS_REFRESH_COOLDOWN: Duration = Duration::from_secs(5);
const JWKS_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_JWKS_KEYS: usize = 64;
const MAX_JWKS_BYTES: usize = 256 * 1024;
const MAX_JWK_COMPONENT_BYTES: usize = 1024;
const CLOCK_SKEW_SECONDS: f64 = 60.0;

/// The operator auth modes that require this validator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperatorMode {
    Oidc,
    Both,
}

/// Errors while constructing the configured operator authenticator.
#[derive(Debug, Error)]
pub enum OperatorAuthConfigError {
    /// The parser should have required all OIDC fields for these modes.
    #[error("OIDC operator auth requires issuer, audience, and jwks_url")]
    MissingConfiguration,
    /// The JWKS client could not be created.
    #[error("cannot create OIDC JWKS client: {0}")]
    Client(#[from] reqwest::Error),
    /// The JWKS endpoint is not an authenticated HTTPS URL or literal loopback
    /// HTTP development endpoint.
    #[error("OIDC jwks_url must use HTTPS (or HTTP on a literal loopback address)")]
    InvalidJwksUrl,
}

/// An immutable, clone-cheap validator with a short-lived public-key cache.
#[derive(Clone)]
pub struct OperatorAuthenticator {
    mode: OperatorMode,
    issuer: Arc<str>,
    audience: Arc<str>,
    jwks_url: Arc<str>,
    admin_claim: Arc<str>,
    client: Client,
    keys: Arc<RwLock<Option<CachedKeys>>>,
    refresh_attempted: Arc<AsyncMutex<Option<Instant>>>,
}

impl OperatorAuthenticator {
    /// Build the validator when the gateway auth mode includes OIDC.
    pub fn from_config(
        config: &GatewayAuthConfig,
    ) -> Result<Option<Self>, OperatorAuthConfigError> {
        let mode = match config.mode.as_str() {
            "vkeys" => return Ok(None),
            "oidc" => OperatorMode::Oidc,
            "both" => OperatorMode::Both,
            _ => return Ok(None),
        };
        let issuer = config
            .issuer
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or(OperatorAuthConfigError::MissingConfiguration)?;
        let audience = config
            .audience
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or(OperatorAuthConfigError::MissingConfiguration)?;
        let jwks_url = config
            .jwks_url
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or(OperatorAuthConfigError::MissingConfiguration)?;
        let parsed_jwks_url =
            reqwest::Url::parse(jwks_url).map_err(|_| OperatorAuthConfigError::InvalidJwksUrl)?;
        let loopback_http = parsed_jwks_url.scheme() == "http"
            && parsed_jwks_url
                .host_str()
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|address| address.is_loopback());
        if (parsed_jwks_url.scheme() != "https" && !loopback_http)
            || !parsed_jwks_url.username().is_empty()
            || parsed_jwks_url.password().is_some()
            || parsed_jwks_url.fragment().is_some()
        {
            return Err(OperatorAuthConfigError::InvalidJwksUrl);
        }
        let client = Client::builder()
            .connect_timeout(JWKS_TIMEOUT)
            .timeout(JWKS_TIMEOUT)
            .user_agent("wayfinder-router-operator-auth")
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Some(Self {
            mode,
            issuer: Arc::from(issuer.to_owned()),
            audience: Arc::from(audience.to_owned()),
            jwks_url: Arc::from(jwks_url.to_owned()),
            admin_claim: Arc::from(config.admin_claim.clone()),
            client,
            keys: Arc::new(RwLock::new(None)),
            refresh_attempted: Arc::new(AsyncMutex::new(None)),
        }))
    }

    /// Authorize an operator request. A virtual key can satisfy `both`, but
    /// OIDC mode never accepts a data-plane key as an operator identity.
    async fn authorize(
        &self,
        authorization: Option<&str>,
        virtual_key_valid: bool,
        virtual_key_actor: Option<&str>,
    ) -> Result<String, OperatorAuthFailure> {
        if self.mode == OperatorMode::Both && virtual_key_valid {
            return Ok(format!("vkey:{}", virtual_key_actor.unwrap_or("unknown")));
        }
        let token = bearer_token(authorization).ok_or(OperatorAuthFailure::Unauthorized)?;
        self.verify(token).await
    }

    async fn verify(&self, token: &str) -> Result<String, OperatorAuthFailure> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(OperatorAuthFailure::Unauthorized);
        }
        let mut segments = token.split('.');
        let header_segment = segments.next().ok_or(OperatorAuthFailure::Unauthorized)?;
        let claims_segment = segments.next().ok_or(OperatorAuthFailure::Unauthorized)?;
        let signature_segment = segments.next().ok_or(OperatorAuthFailure::Unauthorized)?;
        if segments.next().is_some()
            || header_segment.is_empty()
            || claims_segment.is_empty()
            || signature_segment.is_empty()
            || header_segment.len() > MAX_TOKEN_BYTES
            || claims_segment.len() > MAX_TOKEN_BYTES
            || signature_segment.len() > MAX_TOKEN_BYTES
        {
            return Err(OperatorAuthFailure::Unauthorized);
        }
        let header: JwtHeader = decode_json(header_segment)?;
        if header.alg != "RS256" || header.kid.is_empty() || header.kid.len() > 128 {
            return Err(OperatorAuthFailure::Unauthorized);
        }
        let claims: Value = decode_json(claims_segment)?;
        let actor = self.validate_claims(&claims)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature_segment)
            .map_err(|_| OperatorAuthFailure::Unauthorized)?;
        let keys = self.keys_for(&header.kid).await?;
        let signed = format!("{header_segment}.{claims_segment}");
        for key in keys {
            let verifier = signature::RsaPublicKeyComponents {
                n: key.modulus.as_slice(),
                e: key.exponent.as_slice(),
            };
            if verifier
                .verify(
                    &signature::RSA_PKCS1_2048_8192_SHA256,
                    signed.as_bytes(),
                    &signature,
                )
                .is_ok()
            {
                return Ok(actor);
            }
        }
        Err(OperatorAuthFailure::Unauthorized)
    }

    fn validate_claims(&self, claims: &Value) -> Result<String, OperatorAuthFailure> {
        if claims.get("iss").and_then(Value::as_str) != Some(&self.issuer) {
            return Err(OperatorAuthFailure::Unauthorized);
        }
        let audience_matches = match claims.get("aud") {
            Some(Value::String(value)) => value == self.audience.as_ref(),
            Some(Value::Array(values)) => values
                .iter()
                .filter_map(Value::as_str)
                .any(|value| value == self.audience.as_ref()),
            _ => false,
        };
        if !audience_matches {
            return Err(OperatorAuthFailure::Unauthorized);
        }
        let now = unix_seconds();
        let expires = numeric_claim(claims, "exp")?;
        if expires <= now {
            return Err(OperatorAuthFailure::Unauthorized);
        }
        if let Some(not_before) = claims.get("nbf") {
            let not_before = numeric_value(not_before).ok_or(OperatorAuthFailure::Unauthorized)?;
            if not_before > now + CLOCK_SKEW_SECONDS {
                return Err(OperatorAuthFailure::Unauthorized);
            }
        }
        let subject = claims
            .get("sub")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or(OperatorAuthFailure::Unauthorized)?;
        if !claim_allows_operator(claims.get(&*self.admin_claim)) {
            return Err(OperatorAuthFailure::Forbidden);
        }
        Ok(subject.to_owned())
    }

    async fn keys_for(&self, kid: &str) -> Result<Vec<RsaJwk>, OperatorAuthFailure> {
        if let Some(matching) = self.cached_keys_for(kid).await {
            return Ok(matching);
        }
        let mut refresh_attempted = self.refresh_attempted.lock().await;
        if let Some(matching) = self.cached_keys_for(kid).await {
            return Ok(matching);
        }
        if refresh_attempted
            .as_ref()
            .is_some_and(|attempted| attempted.elapsed() < JWKS_REFRESH_COOLDOWN)
        {
            return if self.keys.read().await.is_some() {
                Err(OperatorAuthFailure::Unauthorized)
            } else {
                Err(OperatorAuthFailure::Unavailable)
            };
        }
        *refresh_attempted = Some(Instant::now());
        self.refresh_keys().await?;
        drop(refresh_attempted);
        let cache = self.keys.read().await;
        cache
            .as_ref()
            .map(|cached| {
                cached
                    .keys
                    .iter()
                    .filter(|key| key.kid == kid)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|keys| !keys.is_empty())
            .ok_or(OperatorAuthFailure::Unauthorized)
    }

    async fn cached_keys_for(&self, kid: &str) -> Option<Vec<RsaJwk>> {
        let cache = self.keys.read().await;
        let cached = cache
            .as_ref()
            .filter(|cache| cache.fetched.elapsed() < JWKS_CACHE_TTL)?;
        let matching = cached
            .keys
            .iter()
            .filter(|key| key.kid == kid)
            .cloned()
            .collect::<Vec<_>>();
        (!matching.is_empty()).then_some(matching)
    }

    async fn refresh_keys(&self) -> Result<(), OperatorAuthFailure> {
        let response = self
            .client
            .get(self.jwks_url.as_ref())
            .send()
            .await
            .map_err(|_| OperatorAuthFailure::Unavailable)?;
        if !response.status().is_success() {
            return Err(OperatorAuthFailure::Unavailable);
        }
        validate_jwks_content_length(response.content_length())?;
        let mut encoded = Vec::with_capacity(
            response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or(0),
        );
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|_| OperatorAuthFailure::Unavailable)?;
            append_jwks_chunk(&mut encoded, &chunk)?;
        }
        let document = serde_json::from_slice::<JwksDocument>(&encoded)
            .map_err(|_| OperatorAuthFailure::Unavailable)?;
        if document.keys.len() > MAX_JWKS_KEYS {
            return Err(OperatorAuthFailure::Unavailable);
        }
        let keys = document
            .keys
            .into_iter()
            .filter_map(RsaJwk::try_from_document)
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return Err(OperatorAuthFailure::Unavailable);
        }
        let mut cache = self.keys.write().await;
        *cache = Some(CachedKeys {
            fetched: Instant::now(),
            keys,
        });
        Ok(())
    }
}

fn validate_jwks_content_length(length: Option<u64>) -> Result<(), OperatorAuthFailure> {
    if length.is_some_and(|length| length > MAX_JWKS_BYTES as u64) {
        return Err(OperatorAuthFailure::Unavailable);
    }
    Ok(())
}

fn append_jwks_chunk(buffer: &mut Vec<u8>, chunk: &[u8]) -> Result<(), OperatorAuthFailure> {
    if buffer.len().saturating_add(chunk.len()) > MAX_JWKS_BYTES {
        return Err(OperatorAuthFailure::Unavailable);
    }
    buffer.extend_from_slice(chunk);
    Ok(())
}

/// Protect the operator router while leaving the data-plane router unchanged.
pub async fn middleware(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Some(authenticator) = state.operator_auth() else {
        return next.run(request).await;
    };
    let authorization = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let request_path = request.uri().path().to_owned();
    let virtual_key_actor = state.access_policy().and_then(|policy| {
        let grant = policy
            .requires_authentication()
            .then(|| policy.authenticate(authorization))
            .flatten()?;
        policy.key_id(grant)
    });
    let virtual_key_valid = virtual_key_actor.is_some();
    match authenticator
        .authorize(authorization, virtual_key_valid, virtual_key_actor)
        .await
    {
        Ok(actor) => {
            request.extensions_mut().insert(AuditActor(actor));
            next.run(request).await
        }
        Err(failure) => {
            let (status, error_type, message, challenge, reason) = match failure {
                OperatorAuthFailure::Unauthorized => (
                    StatusCode::UNAUTHORIZED,
                    "wayfinder_router_operator_unauthorized",
                    "a valid operator bearer token is required",
                    true,
                    "unauthorized",
                ),
                OperatorAuthFailure::Forbidden => (
                    StatusCode::FORBIDDEN,
                    "wayfinder_router_operator_forbidden",
                    "the operator token lacks the configured admin claim",
                    false,
                    "forbidden",
                ),
                OperatorAuthFailure::Unavailable => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "wayfinder_router_operator_auth_unavailable",
                    "operator identity validation is temporarily unavailable",
                    false,
                    "unavailable",
                ),
            };
            if let Err(error) = state
                .audit_log()
                .record(
                    "anonymous",
                    "auth.failure",
                    None,
                    Some(json!({"path": request_path, "reason": reason})),
                )
                .await
            {
                tracing::error!(error = %error, "operator authentication audit was not persisted");
                return operator_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "wayfinder_router_audit_unavailable",
                    "operator audit persistence is unavailable",
                    false,
                );
            }
            operator_error(status, error_type, message, challenge)
        }
    }
}

fn operator_error(
    status: StatusCode,
    error_type: &'static str,
    message: &'static str,
    challenge: bool,
) -> Response {
    let mut headers = HeaderMap::new();
    if challenge {
        headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    (
        status,
        headers,
        axum::Json(json!({
            "error": {"type": error_type, "message": message}
        })),
    )
        .into_response()
}

fn bearer_token(authorization: Option<&str>) -> Option<&str> {
    let value = authorization?.trim();
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
        .filter(|token| !token.is_empty())
}

fn decode_json<T: for<'de> Deserialize<'de>>(segment: &str) -> Result<T, OperatorAuthFailure> {
    let bytes = URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| OperatorAuthFailure::Unauthorized)?;
    serde_json::from_slice(&bytes).map_err(|_| OperatorAuthFailure::Unauthorized)
}

fn numeric_claim(claims: &Value, name: &str) -> Result<f64, OperatorAuthFailure> {
    claims
        .get(name)
        .and_then(numeric_value)
        .filter(|value| value.is_finite())
        .ok_or(OperatorAuthFailure::Unauthorized)
}

fn numeric_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_u64().map(|value| value as f64))
}

fn claim_allows_operator(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "admin" | "operator"
        ),
        Some(Value::Array(values)) => values
            .iter()
            .any(|value| claim_allows_operator(Some(value))),
        _ => false,
    }
}

fn unix_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

#[derive(Clone)]
struct CachedKeys {
    fetched: Instant,
    keys: Vec<RsaJwk>,
}

#[derive(Clone, Debug)]
struct RsaJwk {
    kid: String,
    modulus: Vec<u8>,
    exponent: Vec<u8>,
}

impl RsaJwk {
    fn try_from_document(document: JwkDocument) -> Option<Self> {
        if document.kty != "RSA"
            || document.kid.is_empty()
            || document.kid.len() > 128
            || document.alg.as_deref().is_some_and(|alg| alg != "RS256")
            || document.use_.as_deref().is_some_and(|usage| usage != "sig")
        {
            return None;
        }
        let modulus = URL_SAFE_NO_PAD.decode(document.n).ok()?;
        let exponent = URL_SAFE_NO_PAD.decode(document.e).ok()?;
        if modulus.is_empty()
            || exponent.is_empty()
            || modulus.len() > MAX_JWK_COMPONENT_BYTES
            || exponent.len() > MAX_JWK_COMPONENT_BYTES
        {
            return None;
        }
        Some(Self {
            kid: document.kid,
            modulus,
            exponent,
        })
    }
}

#[derive(Debug, Deserialize)]
struct JwksDocument {
    keys: Vec<JwkDocument>,
}

#[derive(Debug, Deserialize)]
struct JwkDocument {
    kid: String,
    kty: String,
    alg: Option<String>,
    #[serde(rename = "use")]
    use_: Option<String>,
    n: String,
    e: String,
}

#[derive(Debug, Deserialize)]
struct JwtHeader {
    alg: String,
    kid: String,
}

#[derive(Debug)]
enum OperatorAuthFailure {
    Unauthorized,
    Forbidden,
    Unavailable,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    use wayfinder_routing_core::RoutingConfig;

    #[test]
    fn bearer_token_requires_the_bearer_scheme() {
        assert_eq!(bearer_token(Some("Bearer abc")), Some("abc"));
        assert_eq!(bearer_token(Some("bearer    abc")), Some("abc"));
        assert_eq!(bearer_token(Some("abc")), None);
        assert_eq!(bearer_token(Some("Bearer")), None);
    }

    #[test]
    fn admin_claim_accepts_bounded_truthy_shapes_only() {
        assert!(claim_allows_operator(Some(&Value::Bool(true))));
        assert!(claim_allows_operator(Some(&Value::String(
            "operator".to_owned()
        ))));
        assert!(claim_allows_operator(Some(&json!(["viewer", "admin"]))));
        assert!(!claim_allows_operator(Some(&Value::Bool(false))));
        assert!(!claim_allows_operator(Some(&Value::String(
            "owner".to_owned()
        ))));
    }

    #[test]
    fn jwk_filters_non_signing_or_non_rs256_keys() {
        assert!(
            RsaJwk::try_from_document(JwkDocument {
                kid: "key-1".to_owned(),
                kty: "EC".to_owned(),
                alg: Some("ES256".to_owned()),
                use_: Some("sig".to_owned()),
                n: "AQ".to_owned(),
                e: "AQ".to_owned(),
            })
            .is_none()
        );
    }

    #[test]
    fn jwks_declared_and_streamed_bodies_are_bounded_before_parsing()
    -> Result<(), OperatorAuthFailure> {
        assert!(validate_jwks_content_length(Some(MAX_JWKS_BYTES as u64)).is_ok());
        assert!(matches!(
            validate_jwks_content_length(Some(MAX_JWKS_BYTES as u64 + 1)),
            Err(OperatorAuthFailure::Unavailable)
        ));

        let mut body = Vec::new();
        append_jwks_chunk(&mut body, &vec![b'a'; MAX_JWKS_BYTES - 1])?;
        append_jwks_chunk(&mut body, b"b")?;
        assert!(matches!(
            append_jwks_chunk(&mut body, b"c"),
            Err(OperatorAuthFailure::Unavailable)
        ));
        Ok(())
    }

    #[test]
    fn auth_config_only_builds_oidc_validator_for_oidc_modes() {
        let config = GatewayAuthConfig::default();
        assert!(matches!(
            OperatorAuthenticator::from_config(&config),
            Ok(None)
        ));
        let config = GatewayAuthConfig {
            mode: "oidc".to_owned(),
            issuer: Some("https://issuer.example".to_owned()),
            audience: Some("wayfinder".to_owned()),
            jwks_url: Some("https://issuer.example/jwks".to_owned()),
            admin_claim: "admin".to_owned(),
        };
        assert!(matches!(
            OperatorAuthenticator::from_config(&config),
            Ok(Some(_))
        ));

        let insecure_remote = GatewayAuthConfig {
            jwks_url: Some("http://issuer.example/jwks".to_owned()),
            ..config.clone()
        };
        assert!(matches!(
            OperatorAuthenticator::from_config(&insecure_remote),
            Err(OperatorAuthConfigError::InvalidJwksUrl)
        ));
        let loopback_development = GatewayAuthConfig {
            jwks_url: Some("http://127.0.0.1:9876/jwks".to_owned()),
            ..config
        };
        assert!(matches!(
            OperatorAuthenticator::from_config(&loopback_development),
            Ok(Some(_))
        ));
    }

    #[tokio::test]
    async fn unknown_key_refreshes_are_single_flight_and_cooled_down() {
        let config = GatewayAuthConfig {
            mode: "oidc".to_owned(),
            issuer: Some("https://issuer.example".to_owned()),
            audience: Some("wayfinder".to_owned()),
            jwks_url: Some("https://issuer.example/jwks".to_owned()),
            admin_claim: "admin".to_owned(),
        };
        let Some(authenticator) = OperatorAuthenticator::from_config(&config).ok().flatten() else {
            return;
        };
        *authenticator.refresh_attempted.lock().await = Some(Instant::now());
        assert!(matches!(
            authenticator.keys_for("attacker-selected-kid").await,
            Err(OperatorAuthFailure::Unavailable)
        ));
    }

    #[test]
    fn claims_require_identity_time_bounds_and_admin_access() {
        let config = GatewayAuthConfig {
            mode: "oidc".to_owned(),
            issuer: Some("https://issuer.example".to_owned()),
            audience: Some("wayfinder".to_owned()),
            jwks_url: Some("https://issuer.example/jwks".to_owned()),
            admin_claim: "admin".to_owned(),
        };
        let authenticator = match OperatorAuthenticator::from_config(&config) {
            Ok(Some(authenticator)) => authenticator,
            _ => return,
        };
        let claims = json!({
            "iss": "https://issuer.example",
            "aud": ["other", "wayfinder"],
            "sub": "operator-1",
            "exp": unix_seconds() + 300.0,
            "admin": true,
        });
        assert!(authenticator.validate_claims(&claims).is_ok());
        let mut forbidden = claims;
        forbidden["admin"] = Value::Bool(false);
        assert!(matches!(
            authenticator.validate_claims(&forbidden),
            Err(OperatorAuthFailure::Forbidden)
        ));
    }

    #[tokio::test]
    async fn oidc_mode_guards_operator_routes_without_affecting_health()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = GatewayAuthConfig {
            mode: "oidc".to_owned(),
            issuer: Some("https://issuer.example".to_owned()),
            audience: Some("wayfinder".to_owned()),
            jwks_url: Some("https://issuer.example/jwks".to_owned()),
            admin_claim: "admin".to_owned(),
        };
        let path = std::env::temp_dir().join(format!(
            "wayfinder-operator-audit-{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let state = AppState::new(RoutingConfig::binary(0.5), Vec::new(), false, "test")
            .with_audit_log(crate::audit::AuditLog::from_path(&path)?)
            .with_gateway_operator_auth(&wayfinder_config::gateway::GatewayConfig {
                auth: config,
                ..wayfinder_config::gateway::GatewayConfig::default()
            })?;
        let operator = crate::build_router(state.clone())
            .oneshot(Request::get("/router/routes").body(Body::empty())?)
            .await?;
        assert_eq!(operator.status(), StatusCode::UNAUTHORIZED);
        let health = crate::build_router(state)
            .oneshot(Request::get("/healthz").body(Body::empty())?)
            .await?;
        assert_eq!(health.status(), StatusCode::OK);
        let audit = fs::read_to_string(&path)?;
        assert!(audit.contains("auth.failure"));
        assert!(audit.contains("/router/routes"));
        let _ = fs::remove_file(path);
        Ok(())
    }
}
