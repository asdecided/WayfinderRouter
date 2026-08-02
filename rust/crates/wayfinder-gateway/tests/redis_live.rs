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
    RedisStateBackend, SharedBudgetReservation, SharedBudgetScope, SharedCachePut,
    SharedCostObservation, StateBackend, StateBackendError, StateLimitConfig,
};
use wayfinder_service::pricing::TurnCost;

type TestResult = Result<(), Box<dyn Error>>;

const REDIS_URL_ENV: &str = "WAYFINDER_REDIS_TEST_URL";

fn redis_step<T>(result: Result<T, StateBackendError>, step: &str) -> Result<T, io::Error> {
    result.map_err(|error| io::Error::other(format!("{step}: {error:?}")))
}

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

#[tokio::test]
async fn replicated_accounting_admission_and_health_are_atomic() -> TestResult {
    let Some(admin_url) = live_redis_url()? else {
        return Ok(());
    };

    let mut admin = admin_connection(&admin_url).await?;
    let suffix = Uuid::new_v4().simple().to_string();
    let username = format!("wayfinder_ci_{suffix}");
    let password = format!("secret_{suffix}");
    let namespace = format!("ci:fleet:{suffix}");
    let authenticated_url = authenticated_url(&admin_url, &username, &password)?;
    configure_gateway_user(&mut admin, &username, &password).await?;

    let first = RedisStateBackend::connect(&authenticated_url, namespace.clone()).await?;
    let second = RedisStateBackend::connect(&authenticated_url, namespace.clone()).await?;
    let cost = TurnCost {
        route: "cloud".to_owned(),
        realized: 1.25,
        baseline: 2.0,
        savings: 0.75,
        prompt_tokens: 100,
        completion_tokens: 50,
        estimated: false,
    };
    let observation = SharedCostObservation::from_turn(
        "request-1",
        "2026-07-11".to_owned(),
        &cost,
        vec!["global".to_owned(), "workspace:production".to_owned()],
    );
    assert!(redis_step(
        first.record_cost(observation.clone()).await,
        "record first"
    )?);
    assert!(!redis_step(
        second.record_cost(observation).await,
        "record duplicate"
    )?);
    assert!(
        !redis_step(
            first
                .record_cost(SharedCostObservation::from_turn(
                    "request-1",
                    "2026-07-12".to_owned(),
                    &cost,
                    vec!["global".to_owned(), "workspace:production".to_owned()],
                ))
                .await,
            "record cross-date duplicate",
        )?,
        "request idempotency must span UTC ledger boundaries"
    );
    assert_eq!(
        redis_step(
            second.spend("day", "global", "2026-07-11").await,
            "spend global",
        )?
        .realized,
        1.25
    );
    assert_eq!(
        redis_step(
            first
                .spend("day", "workspace:production", "2026-07-11")
                .await,
            "spend workspace",
        )?
        .realized,
        1.25
    );

    let reservation = SharedBudgetReservation {
        request_id: "budget-request".to_owned(),
        date: "2026-07-11".to_owned(),
        amount: 0.5,
        scopes: vec![SharedBudgetScope {
            scope: "global".to_owned(),
            window: "day".to_owned(),
            limit: 2.0,
            blocks: true,
        }],
    };
    assert!(redis_step(
        first.reserve_budget(reservation.clone()).await,
        "reserve first"
    )?);
    assert!(
        redis_step(
            second.reserve_budget(reservation.clone()).await,
            "reserve duplicate"
        )?,
        "same budget request is idempotently accepted"
    );
    redis_step(
        first.release_budget(reservation.clone()).await,
        "release first",
    )?;
    assert!(redis_step(
        second.reserve_budget(reservation.clone()).await,
        "reserve after release"
    )?);
    redis_step(second.settle_budget(reservation).await, "settle")?;

    assert!(redis_step(
        first.acquire_admission(1, "lease-a", 1_000).await,
        "admission first",
    )?);
    assert!(!redis_step(
        second.acquire_admission(1, "lease-b", 1_000).await,
        "admission rejected",
    )?);
    redis_step(
        first.release_admission("lease-a").await,
        "admission release first",
    )?;
    assert!(redis_step(
        second.acquire_admission(1, "lease-b", 1_000).await,
        "admission after release",
    )?);
    redis_step(
        second.release_admission("lease-b").await,
        "admission release second",
    )?;

    assert!(redis_step(
        first
            .begin_provider_health("cloud-eu", 1, 20, "probe-a")
            .await,
        "health first",
    )?);
    assert!(!redis_step(
        second
            .begin_provider_health("cloud-eu", 1, 20, "probe-b")
            .await,
        "health concurrent",
    )?);
    redis_step(
        first
            .complete_provider_health("cloud-eu", 1, 20, "probe-a", false)
            .await,
        "health failure",
    )?;
    assert!(!redis_step(
        second
            .begin_provider_health("cloud-eu", 1, 20, "probe-b")
            .await,
        "health open",
    )?);
    sleep(Duration::from_millis(30)).await;
    assert!(redis_step(
        second
            .begin_provider_health("cloud-eu", 1, 20, "probe-b")
            .await,
        "health probe",
    )?);
    redis_step(
        second
            .complete_provider_health("cloud-eu", 1, 20, "probe-b", true)
            .await,
        "health recovery",
    )?;

    let namespace_component = URL_SAFE_NO_PAD.encode(namespace.as_bytes());
    let keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("wayfinder:v1:{namespace_component}:*"))
        .query_async(&mut admin)
        .await?;
    if !keys.is_empty() {
        let _: i64 = redis::cmd("DEL").arg(keys).query_async(&mut admin).await?;
    }
    let _: i64 = redis::cmd("ACL")
        .arg("DELUSER")
        .arg(&username)
        .query_async(&mut admin)
        .await?;
    Ok(())
}

#[tokio::test]
async fn replicated_shared_response_cache_is_bounded_isolated_and_expiring() -> TestResult {
    let Some(admin_url) = live_redis_url()? else {
        return Ok(());
    };

    let mut admin = admin_connection(&admin_url).await?;
    let suffix = Uuid::new_v4().simple().to_string();
    let username = format!("wayfinder_ci_{suffix}");
    let password = format!("secret_{suffix}");
    let namespace = format!("ci:cache:{suffix}");
    let authenticated_url = authenticated_url(&admin_url, &username, &password)?;
    configure_gateway_user(&mut admin, &username, &password).await?;

    let first = RedisStateBackend::connect(&authenticated_url, namespace.clone()).await?;
    let second = RedisStateBackend::connect(&authenticated_url, namespace.clone()).await?;
    let key_a = "a".repeat(64);
    let key_b = "b".repeat(64);
    let key_c = "c".repeat(64);
    let key_ttl = "d".repeat(64);
    let body_a = b"one".to_vec();
    let body_b = b"two".to_vec();
    let body_c = b"six".to_vec();

    assert!(redis_step(
        first
            .shared_cache_put(SharedCachePut {
                generation: "generation-a".to_owned(),
                key: key_a.clone(),
                value: body_a.clone(),
                body_bytes: body_a.len(),
                ttl_seconds: 0.0,
                max_entries: 2,
                max_bytes: 10,
            })
            .await,
        "cache put generation a",
    )?);
    assert_eq!(
        redis_step(
            second.shared_cache_get("generation-a", &key_a).await,
            "cache get from second replica",
        )?
        .as_deref(),
        Some(body_a.as_slice()),
        "a second replica must see the shared response"
    );

    assert!(
        redis_step(
            second.shared_cache_get("generation-b", &key_a).await,
            "generation isolation",
        )?
        .is_none(),
        "a config-generation change must invalidate the old value"
    );
    assert!(redis_step(
        first
            .shared_cache_put(SharedCachePut {
                generation: "generation-b".to_owned(),
                key: key_b.clone(),
                value: body_b.clone(),
                body_bytes: body_b.len(),
                ttl_seconds: 0.0,
                max_entries: 1,
                max_bytes: 10,
            })
            .await,
        "cache put first bounded value",
    )?);
    sleep(Duration::from_millis(5)).await;
    assert!(redis_step(
        second
            .shared_cache_put(SharedCachePut {
                generation: "generation-b".to_owned(),
                key: key_c.clone(),
                value: body_c.clone(),
                body_bytes: body_c.len(),
                ttl_seconds: 0.0,
                max_entries: 1,
                max_bytes: 10,
            })
            .await,
        "cache put second bounded value",
    )?);
    assert!(
        redis_step(
            first.shared_cache_get("generation-b", &key_b).await,
            "entry-bound eviction",
        )?
        .is_none(),
        "the oldest value must be evicted at max_entries"
    );
    assert_eq!(
        redis_step(
            first.shared_cache_get("generation-b", &key_c).await,
            "newest bounded value",
        )?
        .as_deref(),
        Some(body_c.as_slice())
    );

    assert!(!redis_step(
        first
            .shared_cache_put(SharedCachePut {
                generation: "generation-b".to_owned(),
                key: key_a.clone(),
                value: b"oversized".to_vec(),
                body_bytes: 9,
                ttl_seconds: 0.0,
                max_entries: 2,
                max_bytes: 3,
            })
            .await,
        "byte-bound rejection",
    )?);
    assert!(redis_step(
        second
            .shared_cache_put(SharedCachePut {
                generation: "generation-b".to_owned(),
                key: key_ttl.clone(),
                value: b"ttl".to_vec(),
                body_bytes: 3,
                ttl_seconds: 0.02,
                max_entries: 2,
                max_bytes: 10,
            })
            .await,
        "cache put with ttl",
    )?);
    sleep(Duration::from_millis(40)).await;
    assert!(
        redis_step(
            first.shared_cache_get("generation-b", &key_ttl).await,
            "cache ttl expiry",
        )?
        .is_none(),
        "expired values must not be replayed"
    );

    let namespace_component = URL_SAFE_NO_PAD.encode(namespace.as_bytes());
    let namespace_keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("wayfinder:v1:{namespace_component}:*"))
        .query_async(&mut admin)
        .await?;
    if !namespace_keys.is_empty() {
        let _: i64 = redis::cmd("DEL")
            .arg(namespace_keys)
            .query_async(&mut admin)
            .await?;
    }
    let _: i64 = redis::cmd("ACL")
        .arg("DELUSER")
        .arg(&username)
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
        .arg("-@all")
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
        .arg("-@all")
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
