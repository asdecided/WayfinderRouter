use std::error::Error;

use serde::Deserialize;
use serde_json::{Value, json};
use wayfinder_config::gateway::{
    Budget, GatewayConfig, RateLimit, dump_gateway_toml, gateway_config_from_toml,
};

const GATEWAY_CONFIG_VECTORS: &str = include_str!("../fixtures/gateway-config.json");
const WHERE: &str = "gateway-compat-vector";

#[derive(Debug, Deserialize)]
struct GatewayConfigCase {
    name: String,
    compatibility: Compatibility,
    toml: String,
    outcome: ExpectedOutcome,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Compatibility {
    Exact,
    RustRejectsNonfinite,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ExpectedOutcome {
    Valid { summary: Value },
    Invalid { legacy_diagnostic: String },
}

#[test]
fn gateway_config_preserves_frozen_semantics_and_annotated_hardening() -> Result<(), Box<dyn Error>>
{
    let cases: Vec<GatewayConfigCase> = serde_json::from_str(GATEWAY_CONFIG_VECTORS)?;
    let mut exact_valid = 0_usize;
    let mut exact_invalid = 0_usize;
    let mut nonfinite_hardening = 0_usize;

    assert_eq!(cases.len(), 80, "gateway compatibility fixture count");
    for case in &cases {
        let parsed = gateway_config_from_toml(&case.toml, WHERE);
        match (case.compatibility, &case.outcome, parsed) {
            (Compatibility::Exact, ExpectedOutcome::Valid { summary }, Ok(config)) => {
                exact_valid = exact_valid.saturating_add(1);
                assert_summary_is_safe(&case.name, summary);

                let actual = summarize_config(&config);
                assert_summary_is_safe(&case.name, &actual);
                assert_eq!(&actual, summary, "{} semantic summary", case.name);

                let dumped = dump_gateway_toml(&config)?;
                let reparsed = gateway_config_from_toml(&dumped, "gateway dump round-trip")?;
                assert_eq!(
                    summarize_config(&reparsed),
                    actual,
                    "{} dump round-trip semantics",
                    case.name
                );
                assert_eq!(
                    dump_gateway_toml(&reparsed)?,
                    dumped,
                    "{} deterministic second dump",
                    case.name
                );
            }
            (Compatibility::Exact, ExpectedOutcome::Invalid { legacy_diagnostic }, Err(error)) => {
                exact_invalid = exact_invalid.saturating_add(1);
                assert!(
                    !legacy_diagnostic.is_empty(),
                    "{} fixture diagnostic",
                    case.name
                );
                assert!(
                    !error.to_string().is_empty(),
                    "{} current diagnostic",
                    case.name
                );
            }
            (
                Compatibility::RustRejectsNonfinite,
                ExpectedOutcome::Valid { summary },
                Err(error),
            ) => {
                nonfinite_hardening = nonfinite_hardening.saturating_add(1);
                assert!(
                    contains_nonfinite_marker(summary),
                    "{} must preserve frozen non-finite semantic evidence",
                    case.name
                );
                assert!(
                    !error.to_string().is_empty(),
                    "{} current hardening diagnostic",
                    case.name
                );
            }
            (Compatibility::Exact, ExpectedOutcome::Valid { .. }, Err(error)) => {
                return Err(format!(
                    "{}: current parser rejected fixture-valid gateway TOML: {error}",
                    case.name
                )
                .into());
            }
            (Compatibility::Exact, ExpectedOutcome::Invalid { legacy_diagnostic }, Ok(_)) => {
                return Err(format!(
                    "{}: current parser accepted fixture-invalid gateway TOML ({legacy_diagnostic})",
                    case.name
                )
                .into());
            }
            (Compatibility::RustRejectsNonfinite, ExpectedOutcome::Valid { .. }, Ok(_)) => {
                return Err(format!(
                    "{}: Rust accepted a documented non-finite hardening vector",
                    case.name
                )
                .into());
            }
            (
                Compatibility::RustRejectsNonfinite,
                ExpectedOutcome::Invalid { legacy_diagnostic },
                _,
            ) => {
                return Err(format!(
                    "{}: non-finite hardening annotation requires fixture-valid input ({legacy_diagnostic})",
                    case.name
                )
                .into());
            }
        }
    }

    assert_eq!(exact_valid, 14);
    assert_eq!(exact_invalid, 61);
    assert_eq!(nonfinite_hardening, 5);
    Ok(())
}

fn summarize_config(config: &GatewayConfig) -> Value {
    let keys = config
        .keys
        .iter()
        .map(|(name, key)| {
            json!({
                "name": name,
                "credential_digest": {
                    "algorithm": "sha256",
                    "value": "<redacted>",
                    "length": key.hash.len(),
                    "normalized_lowercase": key
                        .hash
                        .bytes()
                        .all(|byte| !byte.is_ascii_uppercase()),
                },
                "tags": &key.tags,
                "budget": budget_summary(key.budget.as_ref()),
                "rate_limit": rate_limit_summary(key.rate_limit.as_ref()),
                "models": &key.models,
            })
        })
        .collect::<Vec<_>>();
    let models = config
        .models
        .iter()
        .map(|(name, model)| {
            json!({
                "name": name,
                "provider": model.provider.as_str(),
                "base_url": &model.base_url,
                "model": &model.model,
                "tier": model.tier.map(|tier| tier.as_str()),
                "credential_reference": {
                    "api_key_env": &model.api_key_env,
                    "api_key_cmd": &model.api_key_cmd,
                },
                "cost_per_1k": model.cost_per_1k,
                "fallbacks": &model.fallbacks,
                "context_window": model.context_window,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "route_on": &config.route_on,
        "sticky": config.sticky,
        "sticky_cooldown": config.sticky_cooldown,
        "slash_directives": config.slash_directives,
        "offline": config.offline,
        "retries": config.retries,
        "breaker_threshold": config.breaker_threshold,
        "breaker_cooldown": config.breaker_cooldown,
        "failover": &config.failover,
        "budget": budget_summary(config.budget.as_ref()),
        "cache": config.cache.as_ref().map(|cache| {
            json!({
                "enabled": cache.enabled,
                "ttl": cache.ttl,
                "max_entries": cache.max_entries,
                "max_bytes": cache.max_bytes,
            })
        }),
        "rate_limit": rate_limit_summary(config.rate_limit.as_ref()),
        "keys": keys,
        "models": models,
    })
}

fn budget_summary(budget: Option<&Budget>) -> Value {
    budget.map_or(Value::Null, |budget| {
        json!({
            "limit": budget.limit,
            "window": &budget.window,
            "on_breach": &budget.on_breach,
        })
    })
}

fn rate_limit_summary(rate_limit: Option<&RateLimit>) -> Value {
    rate_limit.map_or(Value::Null, |rate_limit| {
        json!({
            "rpm": rate_limit.rpm,
            "tpm": rate_limit.tpm,
            "window": rate_limit.window,
        })
    })
}

fn assert_summary_is_safe(case_name: &str, summary: &Value) {
    assert!(
        !contains_sha256_value(summary),
        "{case_name}: semantic summary exposed a virtual-key digest"
    );
}

fn contains_sha256_value(value: &Value) -> bool {
    match value {
        Value::String(value) => {
            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }
        Value::Array(values) => values.iter().any(contains_sha256_value),
        Value::Object(values) => values.values().any(contains_sha256_value),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn contains_nonfinite_marker(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_nonfinite_marker),
        Value::Object(values) => {
            values.contains_key("nonfinite") || values.values().any(contains_nonfinite_marker)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}
