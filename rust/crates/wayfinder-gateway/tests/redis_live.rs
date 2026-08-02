//! Live Redis contract tests for replicated gateway state.
//!
//! The dedicated Redis CI job supplies `WAYFINDER_REDIS_TEST_URL`. Regular
//! workspace test runs skip this external-service contract when that variable
//! is absent.

use std::env;
use std::error::Error;
use std::io;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use redis::aio::MultiplexedConnection;
use tokio::time::sleep;
use uuid::Uuid;
use wayfinder_gateway::rate_limit::LimitKind;
use wayfinder_gateway::state::{
    RedisStateBackend, StateBackend, StateBackendError, StateLimitConfig,
};

type TestResult = Result<(), Box<dyn Error>>;

const REDIS_URL_ENV: &str = "WAYFINDER_REDIS_TEST_URL";

#[tokio::test]
async fn replicated_gateway_state_survives_real_redis_failure_modes() -> TestResult {
    let Some(admin_url) = live_redis_url()? else {
        return Ok(());
    };

    let mut admin = admin_connection(&admin_url).await?;
    let suffix = Uuid::new_v4().simple().to_string();
    let username = format!("wayfinder_ci_{suffix}");
    let password = format!("secret_{suffix}");
    let namespace = format!("ci:replicas:{suffix}");
    let authenticated_url = authenticated_url(&admin_url, &username, &password)?;

    configure_gateway_user(&mut admin, &username, &password).await?;

    let first = RedisStateBackend::connect(&authenticated_url, namespace.clone()).await?;
    let second = RedisStateBackend::connect(&authenticated_url, namespace.clone()).await?;
    let limit = StateLimitConfig {
        rpm: Some(2),
        tpm: None,
        window_seconds: 300.0,
    };
    let workspace_id = "team:a";
    let logical_limit_key = format!(
        "workspace:{}",
        URL_SAFE_NO_PAD.encode(workspace_id.as_bytes())
    );

    assert!(first.admit(&logical_limit_key, limit, 0.0).await?.allowed());
    assert!(
        second
            .admit(&logical_limit_key, limit, 0.0)
            .await?
            .allowed()
    );
    assert_eq!(
        first
            .admit(&logical_limit_key, limit, 0.0)
            .await?
            .limited_by,
        Some(LimitKind::Requests),
        "independently constructed backends must enforce one shared RPM window"
    );

    let audit_stream = "operator:events";
    for sequence in 0..5 {
        let backend: &dyn StateBackend = if sequence % 2 == 0 { &first } else { &second };
        backend
            .append_audit(audit_stream, &format!("event-{sequence}"), 3)
            .await?;
    }

    let namespace_component = URL_SAFE_NO_PAD.encode(namespace.as_bytes());
    let expected_limit_key =
        format!("wayfinder:v1:{namespace_component}:limits:{logical_limit_key}");
    let expected_audit_key = format!(
        "wayfinder:v1:{namespace_component}:audit-log:{}",
        URL_SAFE_NO_PAD.encode(audit_stream.as_bytes())
    );
    let retained: Vec<String> = redis::cmd("LRANGE")
        .arg(&expected_audit_key)
        .arg(0)
        .arg(-1)
        .query_async(&mut admin)
        .await?;
    assert_eq!(retained, ["event-2", "event-3", "event-4"]);

    let mut namespace_keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("wayfinder:v1:{namespace_component}:*"))
        .query_async(&mut admin)
        .await?;
    namespace_keys.sort();
    let mut expected_keys = vec![expected_audit_key, expected_limit_key];
    expected_keys.sort();
    assert_eq!(namespace_keys, expected_keys);
    assert!(
        namespace_keys
            .iter()
            .all(|key| !key.contains(":ratelimit:") && !key.contains(":audit:")),
        "live keys must use the collision-safe v1 domains"
    );

    remove_gateway_commands(&mut admin, &username).await?;
    assert_eq!(
        first.admit("outage-probe", limit, 0.0).await,
        Err(StateBackendError::Unavailable)
    );
    assert!(
        first.degraded(),
        "a failed Redis operation must mark the backend degraded"
    );

    restore_gateway_commands(&mut admin, &username).await?;
    let mut recovered = false;
    for _ in 0..20 {
        if first.admit("recovery-probe", limit, 0.0).await.is_ok() {
            recovered = true;
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    assert!(
        recovered,
        "the existing backend must recover after Redis becomes usable"
    );
    assert!(
        !first.degraded(),
        "a successful Redis operation must clear degraded state"
    );

    let _: i64 = redis::cmd("ACL")
        .arg("DELUSER")
        .arg(&username)
        .query_async(&mut admin)
        .await?;
    let _: i64 = redis::cmd("DEL")
        .arg(namespace_keys)
        .arg(format!(
            "wayfinder:v1:{namespace_component}:limits:recovery-probe"
        ))
        .query_async(&mut admin)
        .await?;

    Ok(())
}

fn live_redis_url() -> Result<Option<String>, env::VarError> {
    match env::var(REDIS_URL_ENV) {
        Ok(url) => Ok(Some(url)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn admin_connection(url: &str) -> Result<MultiplexedConnection, redis::RedisError> {
    redis::Client::open(url)?
        .get_multiplexed_async_connection()
        .await
}

fn authenticated_url(base: &str, username: &str, password: &str) -> Result<String, io::Error> {
    let authority = base.strip_prefix("redis://").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "live Redis test URL must use redis://",
        )
    })?;
    if authority.contains('@') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "live Redis test URL must not contain credentials",
        ));
    }
    Ok(format!("redis://{username}:{password}@{authority}"))
}

async fn configure_gateway_user(
    admin: &mut MultiplexedConnection,
    username: &str,
    password: &str,
) -> Result<(), redis::RedisError> {
    redis::cmd("ACL")
        .arg("SETUSER")
        .arg(username)
        .arg("on")
        .arg(format!(">{password}"))
        .arg("resetkeys")
        .arg("~*")
        .arg("resetcommands")
        .arg("+@all")
        .query_async::<String>(admin)
        .await
        .map(|_| ())
}

async fn remove_gateway_commands(
    admin: &mut MultiplexedConnection,
    username: &str,
) -> Result<(), redis::RedisError> {
    redis::cmd("ACL")
        .arg("SETUSER")
        .arg(username)
        .arg("resetcommands")
        .query_async::<String>(admin)
        .await
        .map(|_| ())
}

async fn restore_gateway_commands(
    admin: &mut MultiplexedConnection,
    username: &str,
) -> Result<(), redis::RedisError> {
    redis::cmd("ACL")
        .arg("SETUSER")
        .arg(username)
        .arg("+@all")
        .query_async::<String>(admin)
        .await
        .map(|_| ())
}
