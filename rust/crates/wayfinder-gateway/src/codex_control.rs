//! Loopback-only account control for the optional Codex app-server provider.
//!
//! The wire surface deliberately contains no credential, token, or filesystem
//! fields. A provider runtime implements [`CodexControl`]; the gateway owns HTTP
//! authorization, request validation, response normalization, and error
//! sanitization.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::AppState;

/// Header required on every local account-control request.
pub const LOCAL_CONTROL_HEADER: &str = "x-wayfinder-local-control";

const LOCAL_CONTROL_VALUE: &[u8] = b"1";
const MAX_LOGIN_ID_BYTES: usize = 128;
const MAX_LOGIN_URL_BYTES: usize = 2_048;
const MAX_USER_CODE_BYTES: usize = 64;
const MAX_EMAIL_BYTES: usize = 320;
const MAX_PLAN_BYTES: usize = 128;
const MAX_MODELS: usize = 128;
const MAX_MODEL_ID_BYTES: usize = 128;
const MAX_MODEL_DISPLAY_NAME_BYTES: usize = 160;
const MAX_CONTROL_RESPONSE_BYTES: usize = 65_536;
const REAUTH_REQUIRED_DETAIL: &str = "Sign in to ChatGPT again.";
const UNAVAILABLE_DETAIL: &str = "ChatGPT account service is unavailable.";

/// Boxed asynchronous result returned by a [`CodexControl`] implementation.
pub type CodexControlFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, CodexControlError>> + Send + 'a>>;

/// Browser flow requested from the managed Codex authentication runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexLoginFlow {
    /// Open the documented local-callback browser flow.
    Browser,
    /// Show the documented device URL and user code.
    DeviceCode,
}

/// Safe account identity returned by the managed runtime.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CodexConnectedAccount {
    /// Bounded account email, when the runtime reports one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Bounded subscription-plan label, when the runtime reports one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

/// Normalized account state, independent of the provider runtime protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexAccountState {
    /// No ChatGPT account is connected.
    SignedOut,
    /// Browser authentication is waiting for its callback.
    AwaitingBrowser {
        /// Opaque, non-secret login operation identifier.
        login_id: String,
        /// Browser URL reported by the managed runtime.
        url: String,
    },
    /// Device-code authentication is waiting for the user.
    AwaitingDeviceCode {
        /// Opaque, non-secret login operation identifier.
        login_id: String,
        /// Verification URL reported by the managed runtime.
        url: String,
        /// Short user code reported by the managed runtime.
        user_code: String,
    },
    /// A ChatGPT account is ready.
    Connected(CodexConnectedAccount),
    /// The stored account must authenticate again.
    ReauthenticationRequired,
    /// The managed runtime cannot currently service account operations.
    Unavailable,
}

/// One model advertised by the managed Codex runtime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CodexModel {
    /// Runtime model identifier used for delivery.
    pub id: String,
    /// Human-readable label, when one is advertised.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Provider-runtime failure. HTTP handlers never expose this diagnostic.
#[derive(Debug, Error)]
#[error("{detail}")]
pub struct CodexControlError {
    detail: String,
}

impl CodexControlError {
    /// Preserve a diagnostic for the owning process while keeping it off wire.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// Provider-agnostic control plane for managed ChatGPT authentication.
///
/// Implementations own their child process and credential lifecycle. They must
/// return only normalized, non-secret values through this trait.
pub trait CodexControl: Send + Sync {
    /// Read current account readiness.
    fn account(&self) -> CodexControlFuture<'_, CodexAccountState>;

    /// Read the current runtime model catalog.
    fn models(&self) -> CodexControlFuture<'_, Vec<CodexModel>>;

    /// Start one managed authentication flow.
    fn login(&self, flow: CodexLoginFlow) -> CodexControlFuture<'_, CodexAccountState>;

    /// Cancel one in-progress authentication flow.
    fn cancel_login<'a>(&'a self, login_id: &'a str) -> CodexControlFuture<'a, CodexAccountState>;

    /// Sign out the isolated Wayfinder account store.
    fn logout(&self) -> CodexControlFuture<'_, CodexAccountState>;
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum AccountResponse {
    SignedOut,
    AwaitingBrowser { login: BrowserLoginResponse },
    AwaitingDeviceCode { login: DeviceCodeLoginResponse },
    Connected { account: CodexConnectedAccount },
    ReauthRequired { detail: &'static str },
    Unavailable { detail: &'static str },
}

#[derive(Serialize)]
struct BrowserLoginResponse {
    id: String,
    flow: &'static str,
    url: String,
}

#[derive(Serialize)]
struct DeviceCodeLoginResponse {
    id: String,
    flow: &'static str,
    url: String,
    user_code: String,
}

impl From<CodexAccountState> for AccountResponse {
    fn from(state: CodexAccountState) -> Self {
        match state {
            CodexAccountState::SignedOut => Self::SignedOut,
            CodexAccountState::AwaitingBrowser { login_id, url } => Self::AwaitingBrowser {
                login: BrowserLoginResponse {
                    id: login_id,
                    flow: "browser",
                    url,
                },
            },
            CodexAccountState::AwaitingDeviceCode {
                login_id,
                url,
                user_code,
            } => Self::AwaitingDeviceCode {
                login: DeviceCodeLoginResponse {
                    id: login_id,
                    flow: "device_code",
                    url,
                    user_code,
                },
            },
            CodexAccountState::Connected(account) => Self::Connected { account },
            CodexAccountState::ReauthenticationRequired => Self::ReauthRequired {
                detail: REAUTH_REQUIRED_DETAIL,
            },
            CodexAccountState::Unavailable => Self::Unavailable {
                detail: UNAVAILABLE_DETAIL,
            },
        }
    }
}

#[derive(Serialize)]
struct ModelsResponse {
    models: Vec<CodexModel>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    flow: LoginRequestFlow,
}

#[derive(Deserialize)]
enum LoginRequestFlow {
    #[serde(rename = "browser")]
    Browser,
    #[serde(rename = "device-code")]
    DeviceCode,
}

impl From<LoginRequestFlow> for CodexLoginFlow {
    fn from(flow: LoginRequestFlow) -> Self {
        match flow {
            LoginRequestFlow::Browser => Self::Browser,
            LoginRequestFlow::DeviceCode => Self::DeviceCode,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelLoginRequest {
    login_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyRequest {}

/// Routes installed by [`crate::build_router`] only for an enabled control.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/router/codex/account", get(account))
        .route("/router/codex/models", get(models))
        .route("/router/codex/login", post(login))
        .route("/router/codex/login/cancel", post(cancel_login))
        .route("/router/codex/logout", post(logout))
}

async fn account(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let control = match authorized_control(&state, &headers) {
        Ok(control) => control,
        Err(response) => return *response,
    };
    account_result(control.account().await)
}

async fn models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let control = match authorized_control(&state, &headers) {
        Ok(control) => control,
        Err(response) => return *response,
    };
    match control.models().await {
        Ok(models) if valid_models(&models) => {
            let payload = ModelsResponse { models };
            if bounded_json(&payload) {
                no_store(Json(payload).into_response())
            } else {
                unavailable_response()
            }
        }
        Err(_error) => unavailable_response(),
        Ok(_) => unavailable_response(),
    }
}

async fn login(State(state): State<AppState>, request: Request<Body>) -> Response {
    let control = match authorized_json_control(&state, request.headers()) {
        Ok(control) => control,
        Err(response) => return *response,
    };
    let payload = match json_request::<LoginRequest>(&state, request).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    account_result(control.login(payload.flow.into()).await)
}

async fn cancel_login(State(state): State<AppState>, request: Request<Body>) -> Response {
    let control = match authorized_json_control(&state, request.headers()) {
        Ok(control) => control,
        Err(response) => return *response,
    };
    let payload = match json_request::<CancelLoginRequest>(&state, request).await {
        Ok(payload) => payload,
        Err(response) => return *response,
    };
    if !valid_text(&payload.login_id, MAX_LOGIN_ID_BYTES) {
        return bad_json_response();
    }
    account_result(control.cancel_login(&payload.login_id).await)
}

async fn logout(State(state): State<AppState>, request: Request<Body>) -> Response {
    let control = match authorized_json_control(&state, request.headers()) {
        Ok(control) => control,
        Err(response) => return *response,
    };
    if let Err(response) = json_request::<EmptyRequest>(&state, request).await {
        return *response;
    }
    account_result(control.logout().await)
}

fn authorized_control(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Arc<dyn CodexControl>, Box<Response>> {
    let Some(control) = state.codex_control() else {
        return Err(Box::new(StatusCode::NOT_FOUND.into_response()));
    };
    let mut values = headers.get_all(LOCAL_CONTROL_HEADER).iter();
    if !matches!(values.next(), Some(value) if value.as_bytes() == LOCAL_CONTROL_VALUE)
        || values.next().is_some()
    {
        return Err(Box::new(forbidden_response()));
    }
    Ok(control)
}

fn authorized_json_control(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Arc<dyn CodexControl>, Box<Response>> {
    let control = authorized_control(state, headers)?;
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return Err(Box::new(unsupported_media_type_response()));
    }
    Ok(control)
}

async fn json_request<T: for<'de> Deserialize<'de>>(
    state: &AppState,
    request: Request<Body>,
) -> Result<T, Box<Response>> {
    let bytes = to_bytes(request.into_body(), state.request_body_limit())
        .await
        .map_err(|_error| Box::new(bad_json_response()))?;
    serde_json::from_slice(&bytes).map_err(|_error| Box::new(bad_json_response()))
}

fn account_result(result: Result<CodexAccountState, CodexControlError>) -> Response {
    no_store(match result {
        Ok(state) if valid_account_state(&state) => {
            let payload = AccountResponse::from(state);
            if bounded_json(&payload) {
                Json(payload).into_response()
            } else {
                unavailable_response()
            }
        }
        Err(_error) => unavailable_response(),
        Ok(_) => unavailable_response(),
    })
}

fn bounded_json(value: &impl Serialize) -> bool {
    serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() <= MAX_CONTROL_RESPONSE_BYTES)
}

fn valid_account_state(state: &CodexAccountState) -> bool {
    match state {
        CodexAccountState::SignedOut
        | CodexAccountState::ReauthenticationRequired
        | CodexAccountState::Unavailable => true,
        CodexAccountState::AwaitingBrowser { login_id, url } => {
            valid_text(login_id, MAX_LOGIN_ID_BYTES) && valid_login_url(url)
        }
        CodexAccountState::AwaitingDeviceCode {
            login_id,
            url,
            user_code,
        } => {
            valid_text(login_id, MAX_LOGIN_ID_BYTES)
                && valid_login_url(url)
                && valid_text(user_code, MAX_USER_CODE_BYTES)
        }
        CodexAccountState::Connected(account) => {
            valid_optional_text(account.email.as_deref(), MAX_EMAIL_BYTES)
                && valid_optional_text(account.plan.as_deref(), MAX_PLAN_BYTES)
        }
    }
}

fn valid_models(models: &[CodexModel]) -> bool {
    if models.len() > MAX_MODELS {
        return false;
    }
    let mut identifiers = HashSet::with_capacity(models.len());
    models.iter().all(|model| {
        valid_text(&model.id, MAX_MODEL_ID_BYTES)
            && identifiers.insert(model.id.as_str())
            && valid_optional_text(model.display_name.as_deref(), MAX_MODEL_DISPLAY_NAME_BYTES)
    })
}

fn valid_optional_text(value: Option<&str>, maximum: usize) -> bool {
    value.is_none_or(|value| valid_text(value, maximum))
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_login_url(value: &str) -> bool {
    if !valid_text(value, MAX_LOGIN_URL_BYTES) {
        return false;
    }
    let Ok(uri) = value.parse::<axum::http::Uri>() else {
        return false;
    };
    if uri.scheme_str() != Some("https")
        || uri
            .authority()
            .is_some_and(|authority| authority.as_str().contains('@'))
    {
        return false;
    }
    matches!(
        uri.host(),
        Some("auth.openai.com" | "chatgpt.com" | "www.chatgpt.com")
    )
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn forbidden_response() -> Response {
    fixed_error_response(
        StatusCode::FORBIDDEN,
        "wayfinder_codex_control_forbidden",
        "local control authorization failed",
    )
}

fn unsupported_media_type_response() -> Response {
    fixed_error_response(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "wayfinder_codex_json_required",
        "application/json is required",
    )
}

fn bad_json_response() -> Response {
    fixed_error_response(
        StatusCode::BAD_REQUEST,
        "wayfinder_codex_bad_request",
        "request body must match the local control contract",
    )
}

fn unavailable_response() -> Response {
    fixed_error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "wayfinder_codex_control_unavailable",
        UNAVAILABLE_DETAIL,
    )
}

fn fixed_error_response(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(json!({
            "error": {
                "code": code,
                "message": message,
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use axum::body::to_bytes;
    use axum::http::{Method, Request};
    use serde_json::{Value, json};
    use tower::ServiceExt;
    use wayfinder_routing_core::RoutingConfig;

    use super::*;
    use crate::{AppState, build_router};

    #[derive(Clone)]
    struct ScriptedControl {
        account: CodexAccountState,
        models: Vec<CodexModel>,
        fail_with: Option<Arc<str>>,
    }

    impl ScriptedControl {
        fn ready() -> Self {
            Self {
                account: CodexAccountState::Connected(CodexConnectedAccount {
                    email: Some("person@example.com".to_owned()),
                    plan: Some("Plus".to_owned()),
                }),
                models: vec![CodexModel {
                    id: "gpt-5.6-sol".to_owned(),
                    display_name: Some("GPT-5.6 Sol".to_owned()),
                }],
                fail_with: None,
            }
        }

        fn result<T>(&self, value: T) -> Result<T, CodexControlError> {
            match &self.fail_with {
                Some(detail) => Err(CodexControlError::new(detail.to_string())),
                None => Ok(value),
            }
        }
    }

    impl CodexControl for ScriptedControl {
        fn account(&self) -> CodexControlFuture<'_, CodexAccountState> {
            Box::pin(async { self.result(self.account.clone()) })
        }

        fn models(&self) -> CodexControlFuture<'_, Vec<CodexModel>> {
            Box::pin(async { self.result(self.models.clone()) })
        }

        fn login(&self, flow: CodexLoginFlow) -> CodexControlFuture<'_, CodexAccountState> {
            Box::pin(async move {
                let state = match flow {
                    CodexLoginFlow::Browser => CodexAccountState::AwaitingBrowser {
                        login_id: "login-browser".to_owned(),
                        url: "https://chatgpt.com/auth".to_owned(),
                    },
                    CodexLoginFlow::DeviceCode => CodexAccountState::AwaitingDeviceCode {
                        login_id: "login-device".to_owned(),
                        url: "https://chatgpt.com/device".to_owned(),
                        user_code: "ABCD-EFGH".to_owned(),
                    },
                };
                self.result(state)
            })
        }

        fn cancel_login<'a>(
            &'a self,
            _login_id: &'a str,
        ) -> CodexControlFuture<'a, CodexAccountState> {
            Box::pin(async { self.result(CodexAccountState::SignedOut) })
        }

        fn logout(&self) -> CodexControlFuture<'_, CodexAccountState> {
            Box::pin(async { self.result(CodexAccountState::SignedOut) })
        }
    }

    fn state_with(control: ScriptedControl, literal_loopback: bool) -> AppState {
        AppState::new(RoutingConfig::default(), Vec::new(), false, "test")
            .with_codex_control(Arc::new(control), literal_loopback)
    }

    fn request(
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Request<Body>, http::Error> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(LOCAL_CONTROL_HEADER, "1");
        let bytes = match body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                value.to_string()
            }
            None => String::new(),
        };
        builder.body(Body::from(bytes))
    }

    async fn response_json(response: Response) -> Result<Value, axum::Error> {
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        serde_json::from_slice(&bytes).map_err(axum::Error::new)
    }

    #[tokio::test]
    async fn enabled_loopback_control_returns_normalized_account_and_models()
    -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router(state_with(ScriptedControl::ready(), true));

        let account_response = app
            .clone()
            .oneshot(request(Method::GET, "/router/codex/account", None)?)
            .await?;
        assert_eq!(account_response.status(), StatusCode::OK);
        assert_eq!(
            account_response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert_eq!(
            response_json(account_response).await?,
            json!({
                "status": "connected",
                "account": {"email": "person@example.com", "plan": "Plus"}
            })
        );

        let models_response = app
            .oneshot(request(Method::GET, "/router/codex/models", None)?)
            .await?;
        assert_eq!(models_response.status(), StatusCode::OK);
        assert_eq!(
            models_response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert_eq!(
            response_json(models_response).await?,
            json!({"models": [{"id": "gpt-5.6-sol", "display_name": "GPT-5.6 Sol"}]})
        );
        Ok(())
    }

    #[tokio::test]
    async fn normalized_control_rejects_unbounded_or_unsafe_runtime_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut invalid_account = ScriptedControl::ready();
        invalid_account.account = CodexAccountState::AwaitingBrowser {
            login_id: "login-id".to_owned(),
            url: "https://example.com/steal".to_owned(),
        };
        let response = build_router(state_with(invalid_account, true))
            .oneshot(request(Method::GET, "/router/codex/account", None)?)
            .await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let mut invalid_models = ScriptedControl::ready();
        invalid_models.models = (0..=MAX_MODELS)
            .map(|index| CodexModel {
                id: format!("model-{index}"),
                display_name: None,
            })
            .collect();
        let response = build_router(state_with(invalid_models, true))
            .oneshot(request(Method::GET, "/router/codex/models", None)?)
            .await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let mut duplicate_models = ScriptedControl::ready();
        let duplicate = duplicate_models.models[0].clone();
        duplicate_models.models.push(duplicate);
        let response = build_router(state_with(duplicate_models, true))
            .oneshot(request(Method::GET, "/router/codex/models", None)?)
            .await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let mut escaping_models = ScriptedControl::ready();
        escaping_models.models = (0..MAX_MODELS)
            .map(|index| CodexModel {
                id: format!("{index:03}{}", "\\".repeat(MAX_MODEL_ID_BYTES - 3)),
                display_name: Some("\\".repeat(MAX_MODEL_DISPLAY_NAME_BYTES)),
            })
            .collect();
        let response = build_router(state_with(escaping_models, true))
            .oneshot(request(Method::GET, "/router/codex/models", None)?)
            .await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        Ok(())
    }

    #[tokio::test]
    async fn absent_or_non_loopback_control_omits_routes() -> Result<(), Box<dyn std::error::Error>>
    {
        let states = [
            AppState::new(RoutingConfig::default(), Vec::new(), false, "test"),
            state_with(ScriptedControl::ready(), false),
        ];
        for state in states {
            let response = build_router(state)
                .oneshot(request(Method::GET, "/router/codex/account", None)?)
                .await?;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        Ok(())
    }

    #[tokio::test]
    async fn every_route_requires_the_exact_single_control_header()
    -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router(state_with(ScriptedControl::ready(), true));
        let routes = [
            (Method::GET, "/router/codex/account", None),
            (Method::GET, "/router/codex/models", None),
            (
                Method::POST,
                "/router/codex/login",
                Some(json!({"flow": "browser"})),
            ),
            (
                Method::POST,
                "/router/codex/login/cancel",
                Some(json!({"login_id": "login-browser"})),
            ),
            (Method::POST, "/router/codex/logout", Some(json!({}))),
        ];
        for (method, path, body) in routes {
            let mut missing = request(method.clone(), path, body.clone())?;
            missing.headers_mut().remove(LOCAL_CONTROL_HEADER);
            let response = app.clone().oneshot(missing).await?;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);

            let mut wrong = request(method, path, body)?;
            wrong
                .headers_mut()
                .insert(LOCAL_CONTROL_HEADER, "true".parse()?);
            let response = app.clone().oneshot(wrong).await?;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        Ok(())
    }

    #[tokio::test]
    async fn mutations_require_json_and_reject_malformed_contracts()
    -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router(state_with(ScriptedControl::ready(), true));
        for path in [
            "/router/codex/login",
            "/router/codex/login/cancel",
            "/router/codex/logout",
        ] {
            let response = app
                .clone()
                .oneshot(request(Method::POST, path, None)?)
                .await?;
            assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }

        for (path, body) in [
            ("/router/codex/login", json!({"flow": "device_code"})),
            ("/router/codex/login/cancel", json!({"login_id": ""})),
            (
                "/router/codex/login/cancel",
                json!({"login_id": " login-browser "}),
            ),
            (
                "/router/codex/login/cancel",
                json!({"login_id": "login\nbrowser"}),
            ),
            (
                "/router/codex/login/cancel",
                json!({"login_id": "x".repeat(MAX_LOGIN_ID_BYTES + 1)}),
            ),
            ("/router/codex/logout", json!({"unexpected": true})),
        ] {
            let response = app
                .clone()
                .oneshot(request(Method::POST, path, Some(body))?)
                .await?;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        Ok(())
    }

    #[tokio::test]
    async fn login_flows_and_mutations_share_the_account_shape()
    -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router(state_with(ScriptedControl::ready(), true));
        let browser = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/router/codex/login",
                Some(json!({"flow": "browser"})),
            )?)
            .await?;
        assert_eq!(
            response_json(browser).await?,
            json!({
                "status": "awaiting_browser",
                "login": {
                    "id": "login-browser",
                    "flow": "browser",
                    "url": "https://chatgpt.com/auth"
                }
            })
        );

        let device = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/router/codex/login",
                Some(json!({"flow": "device-code"})),
            )?)
            .await?;
        assert_eq!(
            response_json(device).await?,
            json!({
                "status": "awaiting_device_code",
                "login": {
                    "id": "login-device",
                    "flow": "device_code",
                    "url": "https://chatgpt.com/device",
                    "user_code": "ABCD-EFGH"
                }
            })
        );

        for (path, body) in [
            (
                "/router/codex/login/cancel",
                json!({"login_id": "login-device"}),
            ),
            ("/router/codex/logout", json!({})),
        ] {
            let response = app
                .clone()
                .oneshot(request(Method::POST, path, Some(body))?)
                .await?;
            assert_eq!(
                response_json(response).await?,
                json!({"status": "signed_out"})
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn runtime_errors_are_fixed_sanitized_503_without_cors()
    -> Result<(), Box<dyn std::error::Error>> {
        let control = ScriptedControl {
            account: CodexAccountState::Unavailable,
            models: Vec::new(),
            fail_with: Some(Arc::from(
                "token sk-secret in /Users/private/.codex/auth.json",
            )),
        };
        let response = build_router(state_with(control, true))
            .oneshot(request(Method::GET, "/router/codex/account", None)?)
            .await?;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
        let body = response_json(response).await?.to_string();
        assert!(!body.contains("sk-secret"));
        assert!(!body.contains("/Users/private"));
        assert!(body.contains("wayfinder_codex_control_unavailable"));
        Ok(())
    }

    #[test]
    fn public_wire_types_have_no_token_or_path_fields() -> Result<(), serde_json::Error> {
        let state = AccountResponse::from(CodexAccountState::Connected(CodexConnectedAccount {
            email: Some("person@example.com".to_owned()),
            plan: Some("Plus".to_owned()),
        }));
        let account = serde_json::to_value(state)?;
        let models = serde_json::to_value(ModelsResponse {
            models: vec![CodexModel {
                id: "gpt-5.6-sol".to_owned(),
                display_name: None,
            }],
        })?;
        let wire = format!("{account}{models}");
        for forbidden in ["token", "credential", "path", "codex_home", "auth_file"] {
            assert!(!wire.contains(forbidden));
        }
        Ok(())
    }

    #[test]
    fn control_types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ScriptedControl>();
        assert_send_sync::<CodexControlError>();
        assert_send_sync::<Infallible>();
    }
}
