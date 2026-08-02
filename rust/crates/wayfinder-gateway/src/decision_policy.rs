//! Pure per-request routing policy and chat-text extraction.
//!
//! These functions choose which deterministic configuration or delivery pin
//! applies. They never contact a provider and never mutate global state.

use std::collections::BTreeSet;

use serde_json::Value;
use thiserror::Error;
use wayfinder_config::gateway::GatewayConfig;
use wayfinder_routing_core::{
    CoreError, Lexicon, RoutingConfig, Tier, recommend_tier, score_complexity,
};

/// Header carrying a binary threshold override.
pub const THRESHOLD_HEADER: &str = "x-wayfinder-threshold";
/// Header carrying transcript scope.
pub const ROUTE_ON_HEADER: &str = "x-wayfinder-route-on";
/// Header carrying sticky routing enablement.
pub const STICKY_HEADER: &str = "x-wayfinder-sticky";
/// Header carrying sticky decay turns.
pub const STICKY_COOLDOWN_HEADER: &str = "x-wayfinder-sticky-cooldown";
/// Optional JSON body field carrying per-request scoring tuning.
pub const TUNING_FIELD: &str = "wayfinder_tuning";
const MAX_LEXICON_TERMS: usize = 2_000;

/// OpenAI sentinel asking Wayfinder to decide.
pub const AUTO_MODEL: &str = "auto";
/// Directive selecting the first ordered tier.
pub const PREFER_LOW: &str = "prefer-local";
/// Canonical directive selecting the last ordered tier.
pub const PREFER_HIGH: &str = "prefer-hosted";
/// Shipped legacy alias retained for request compatibility.
pub const PREFER_HIGH_ALIAS: &str = "prefer-cloud";

/// Supported portions of the transcript to score.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RouteOn {
    /// Standing system messages and latest user ask.
    #[default]
    Turn,
    /// Latest user ask only.
    LastUser,
    /// Every user ask.
    User,
    /// All textual messages (legacy behavior).
    All,
}

impl RouteOn {
    /// Compatibility string value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::LastUser => "last_user",
            Self::User => "user",
            Self::All => "all",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "turn" => Some(Self::Turn),
            "last_user" => Some(Self::LastUser),
            "user" => Some(Self::User),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

/// Malformed or inapplicable client override.
#[derive(Debug, Error, PartialEq)]
pub enum PolicyError {
    /// A header value was malformed.
    #[error("{header}: {message}")]
    BadHeader {
        header: &'static str,
        message: String,
    },
    /// A threshold has no single cut to move.
    #[error("{THRESHOLD_HEADER} applies only to a binary (two-tier) router")]
    ThresholdRequiresBinary,
    /// Sticky routing requires at least one tier.
    #[error("sticky routing requires at least one tier")]
    MissingTiers,
    /// The deterministic scorer rejected the supplied config.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// A body-scoped scoring override was malformed.
    #[error("{0}")]
    BadTuning(String),
}

/// A recognized slash directive and its cleaned request messages.
#[derive(Clone, Debug, PartialEq)]
pub struct SlashResolution {
    /// Explicit endpoint, or `None` for `/auto`.
    pub pin: Option<String>,
    /// Copy of the message array with only the recognized directive removed.
    pub messages: Value,
}

/// Deterministically join the message text selected for scoring.
#[must_use]
pub fn extract_prompt(messages: &Value, route_on: RouteOn) -> String {
    let Some(messages) = messages.as_array() else {
        return String::new();
    };
    let typed = messages
        .iter()
        .filter_map(Value::as_object)
        .collect::<Vec<_>>();

    let mut chosen = match route_on {
        RouteOn::All => typed.clone(),
        RouteOn::User => typed
            .iter()
            .copied()
            .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            .collect(),
        RouteOn::LastUser => typed
            .iter()
            .rev()
            .copied()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            .into_iter()
            .collect(),
        RouteOn::Turn => {
            let mut selected = typed
                .iter()
                .copied()
                .filter(|message| message.get("role").and_then(Value::as_str) == Some("system"))
                .collect::<Vec<_>>();
            if let Some(last_user) = typed
                .iter()
                .rev()
                .copied()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            {
                selected.push(last_user);
            }
            selected
        }
    };
    if chosen.is_empty() && !typed.is_empty() && route_on != RouteOn::All {
        if let Some(last) = typed.last() {
            chosen.push(last);
        }
    }
    chosen
        .iter()
        .filter_map(|message| message_text(message))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolve an explicit configured endpoint or tier directive from `model`.
#[must_use]
pub fn resolve_pin(
    model_field: &Value,
    routing: &RoutingConfig,
    gateway: &GatewayConfig,
) -> Option<String> {
    let name = model_field.as_str()?.trim();
    if name.is_empty() || name == AUTO_MODEL {
        return None;
    }
    if routing.classifier.is_none() && !routing.tiers.is_empty() {
        if name == PREFER_LOW {
            return routing.tiers.first().map(|tier| tier.model.clone());
        }
        if name == PREFER_HIGH || name == PREFER_HIGH_ALIAS {
            return routing.tiers.last().map(|tier| tier.model.clone());
        }
    }
    gateway.models.contains_key(name).then(|| name.to_owned())
}

/// Recognize and remove a leading directive from the latest textual user message.
#[must_use]
pub fn resolve_slash_directive(
    messages: &Value,
    routing: &RoutingConfig,
    gateway: &GatewayConfig,
) -> Option<SlashResolution> {
    resolve_slash_directive_for_names(messages, routing, gateway.models.keys().map(String::as_str))
}

/// Resolve a slash directive against an arbitrary configured-name view.
///
/// This keeps secret-free immutable gateway state independent of the full
/// configuration document while preserving the same directive semantics.
#[must_use]
pub fn resolve_slash_directive_for_names<'a>(
    messages: &Value,
    routing: &RoutingConfig,
    configured_names: impl IntoIterator<Item = &'a str>,
) -> Option<SlashResolution> {
    let messages = messages.as_array()?;
    let (index, message) = messages.iter().enumerate().rev().find(|(_, message)| {
        message.get("role").and_then(Value::as_str) == Some("user")
            && message.get("content").is_some_and(Value::is_string)
    })?;
    let content = message.get("content")?.as_str()?;
    let stripped = content.trim_start();
    let after_slash = stripped.strip_prefix('/')?;
    if after_slash.is_empty() {
        return None;
    }
    let token_end = after_slash
        .char_indices()
        .find(|(_, character)| character.is_whitespace())
        .map_or(after_slash.len(), |(index, _)| index);
    let token = after_slash.get(..token_end)?;
    if token.is_empty() {
        return None;
    }
    let remainder = after_slash.get(token_end..)?.trim_start();
    let pin = if token == AUTO_MODEL {
        None
    } else if routing.classifier.is_none() && !routing.tiers.is_empty() && token == PREFER_LOW {
        routing.tiers.first().map(|tier| tier.model.clone())
    } else if routing.classifier.is_none()
        && !routing.tiers.is_empty()
        && matches!(token, PREFER_HIGH | PREFER_HIGH_ALIAS)
    {
        routing.tiers.last().map(|tier| tier.model.clone())
    } else if configured_names.into_iter().any(|name| name == token) {
        Some(token.to_owned())
    } else {
        return None;
    };

    let mut cleaned = messages.clone();
    let mut cleaned_message = message.as_object()?.clone();
    cleaned_message.insert("content".to_owned(), Value::String(remainder.to_owned()));
    let slot = cleaned.get_mut(index)?;
    *slot = Value::Object(cleaned_message);
    Some(SlashResolution {
        pin,
        messages: Value::Array(cleaned),
    })
}

/// Parse an optional `0.0..=1.0` threshold header.
pub fn parse_threshold_header(value: Option<&str>) -> Result<Option<f64>, PolicyError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let threshold = value
        .trim()
        .parse::<f64>()
        .map_err(|_| PolicyError::BadHeader {
            header: THRESHOLD_HEADER,
            message: format!("must be a number in 0.0-1.0, got {value:?}"),
        })?;
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(PolicyError::BadHeader {
            header: THRESHOLD_HEADER,
            message: format!("must be in 0.0-1.0, got {threshold}"),
        });
    }
    Ok(Some(threshold))
}

/// Move the cut of a binary router while preserving its endpoint names.
pub fn threshold_tiers(routing: &RoutingConfig, threshold: f64) -> Result<Vec<Tier>, PolicyError> {
    if routing.classifier.is_some() || routing.tiers.len() != 2 {
        return Err(PolicyError::ThresholdRequiresBinary);
    }
    Ok(vec![
        Tier::new(0.0, routing.tiers[0].model.clone()),
        Tier::new(threshold, routing.tiers[1].model.clone()),
    ])
}

/// Parse an optional transcript-scope header.
pub fn parse_route_on_header(value: Option<&str>) -> Result<Option<RouteOn>, PolicyError> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let normalized = value.trim().to_lowercase();
    RouteOn::parse(&normalized)
        .map(Some)
        .ok_or_else(|| PolicyError::BadHeader {
            header: ROUTE_ON_HEADER,
            message: format!("must be one of turn, last_user, user, all, got {value:?}"),
        })
}

/// Resolve an optional sticky boolean over the configured default.
pub fn resolve_sticky(value: Option<&str>, default: bool) -> Result<bool, PolicyError> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(default);
    };
    match value.trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(PolicyError::BadHeader {
            header: STICKY_HEADER,
            message: format!("must be true or false, got {value:?}"),
        }),
    }
}

/// Resolve a non-negative sticky decay count over the configured default.
pub fn resolve_sticky_cooldown(value: Option<&str>, default: u64) -> Result<u64, PolicyError> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(default);
    };
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| PolicyError::BadHeader {
            header: STICKY_COOLDOWN_HEADER,
            message: format!("must be a non-negative integer, got {value:?}"),
        })
}

/// Apply pure request-scoped weight and lexicon changes.
///
/// Unknown top-level and lexicon keys are ignored for Python compatibility.
/// The supplied config is never mutated.
pub fn apply_scoring_overrides(
    routing: &RoutingConfig,
    override_value: Option<&Value>,
) -> Result<RoutingConfig, PolicyError> {
    let Some(override_value) = override_value else {
        return Ok(routing.clone());
    };
    let Some(override_object) = override_value.as_object() else {
        return Err(bad_tuning(format!("{TUNING_FIELD} must be an object")));
    };

    let mut weights = routing.weights.clone();
    if let Some(raw_weights) = override_object.get("weights") {
        let Some(raw_weights) = raw_weights.as_object() else {
            return Err(bad_tuning(format!(
                "{TUNING_FIELD}.weights must be an object"
            )));
        };
        for (name, value) in raw_weights {
            if weights.get(name).is_none() {
                return Err(bad_tuning(format!(
                    "{TUNING_FIELD}.weights: unknown feature '{}'",
                    name.replace('\\', "\\\\").replace('\'', "\\'")
                )));
            }
            let Some(value) = value.as_f64() else {
                return Err(bad_weight(name));
            };
            if !value.is_finite() || value < 0.0 {
                return Err(bad_weight(name));
            }
            let _ = weights.set(name, value);
        }
    }

    let mut reasoning_terms = routing
        .lexicon
        .reasoning_terms()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let mut constraint_terms = routing
        .lexicon
        .constraint_terms()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if let Some(raw_lexicon) = override_object.get("lexicon") {
        let Some(raw_lexicon) = raw_lexicon.as_object() else {
            return Err(bad_tuning(format!(
                "{TUNING_FIELD}.lexicon must be an object"
            )));
        };
        if let Some(value) = raw_lexicon.get("reasoning_terms") {
            reasoning_terms = parse_tuning_terms(value, "reasoning_terms")?;
        }
        if let Some(value) = raw_lexicon.get("constraint_terms") {
            constraint_terms = parse_tuning_terms(value, "constraint_terms")?;
        }
    }

    Ok(RoutingConfig {
        weights,
        tiers: routing.tiers.clone(),
        classifier: routing.classifier.clone(),
        lexicon: Lexicon::new(reasoning_terms, constraint_terms),
    })
}

fn parse_tuning_terms(value: &Value, key: &str) -> Result<BTreeSet<String>, PolicyError> {
    let Some(terms) = value.as_array() else {
        return Err(bad_terms(key));
    };
    let mut cleaned = BTreeSet::new();
    for term in terms {
        let Some(term) = term.as_str() else {
            return Err(bad_terms(key));
        };
        let term = term.trim().to_lowercase();
        if !term.is_empty() {
            cleaned.insert(term);
        }
    }
    if cleaned.len() > MAX_LEXICON_TERMS {
        return Err(bad_tuning(format!(
            "{TUNING_FIELD}.lexicon.{key} exceeds {MAX_LEXICON_TERMS} terms"
        )));
    }
    Ok(cleaned)
}

fn bad_tuning(message: String) -> PolicyError {
    PolicyError::BadTuning(message)
}

fn bad_weight(name: &str) -> PolicyError {
    bad_tuning(format!(
        "{TUNING_FIELD}.weights.{name} must be a non-negative number"
    ))
}

fn bad_terms(key: &str) -> PolicyError {
    bad_tuning(format!(
        "{TUNING_FIELD}.lexicon.{key} must be a list of strings"
    ))
}

/// Highest per-user-turn tier with optional calm-turn decay.
pub fn conversation_high_water(
    messages: &Value,
    routing: &RoutingConfig,
    tiers: &[Tier],
    cooldown: u64,
) -> Result<Option<String>, PolicyError> {
    if tiers.is_empty() {
        return Err(PolicyError::MissingTiers);
    }
    let Some(messages) = messages.as_array() else {
        return Ok(None);
    };
    let systems = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("system"))
        .cloned()
        .collect::<Vec<_>>();
    let mut ranks = Vec::new();
    for message in messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
    {
        let mut turn = systems.clone();
        turn.push(message.clone());
        let text = extract_prompt(&Value::Array(turn), RouteOn::Turn);
        let score = score_complexity(&text, routing)?.score;
        let model = recommend_tier(score, tiers).ok_or(PolicyError::MissingTiers)?;
        ranks.push(tier_rank(model, tiers).unwrap_or(0));
    }
    if ranks.is_empty() {
        return Ok(None);
    }
    let mut latched = 0_usize;
    let mut calm = 0_u64;
    for rank in ranks {
        if rank >= latched {
            latched = rank;
            calm = 0;
        } else {
            calm = calm.saturating_add(1);
            if cooldown != 0 && calm >= cooldown {
                latched = rank;
                calm = 0;
            }
        }
    }
    Ok(tiers.get(latched).map(|tier| tier.model.clone()))
}

fn message_text(message: &serde_json::Map<String, Value>) -> Option<String> {
    match message.get("content")? {
        Value::String(value) => Some(value.clone()),
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>();
            (!text.is_empty()).then(|| text.join("\n"))
        }
        _ => None,
    }
}

fn tier_rank(model: &str, tiers: &[Tier]) -> Option<usize> {
    tiers.iter().position(|tier| tier.model == model)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wayfinder_config::gateway::{GatewayModel, ProviderKind, gateway_config_from_toml};

    use super::*;

    fn gateway() -> Result<GatewayConfig, wayfinder_config::ConfigError> {
        gateway_config_from_toml(
            r#"
[gateway.models.local]
base_url = "http://local/v1"
model = "small"

[gateway.models.cloud]
base_url = "https://cloud.invalid/v1"
model = "large"
"#,
            "fixture",
        )
    }

    #[test]
    fn prompt_scopes_and_parts_match_python() {
        let messages = json!([
            {"role": "system", "content": "standing"},
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": "answer"},
            {"role": "user", "content": [{"type": "text", "text": "latest"}, {"text": "part"}]}
        ]);
        assert_eq!(
            extract_prompt(&messages, RouteOn::Turn),
            "standing\nlatest\npart"
        );
        assert_eq!(extract_prompt(&messages, RouteOn::LastUser), "latest\npart");
        assert_eq!(
            extract_prompt(&messages, RouteOn::User),
            "first\nlatest\npart"
        );
        assert_eq!(
            extract_prompt(&messages, RouteOn::All),
            "standing\nfirst\nanswer\nlatest\npart"
        );
    }

    #[test]
    fn role_filter_falls_back_to_last_typed_message() {
        let messages = json!([null, "x", {"role": "assistant", "content": "fallback"}]);
        assert_eq!(extract_prompt(&messages, RouteOn::LastUser), "fallback");
        assert_eq!(extract_prompt(&json!({}), RouteOn::Turn), "");
    }

    #[test]
    fn explicit_and_prefer_pins_match_tier_mode() -> Result<(), Box<dyn std::error::Error>> {
        let routing = RoutingConfig::binary(0.5);
        let gateway = gateway()?;
        assert_eq!(resolve_pin(&json!("auto"), &routing, &gateway), None);
        assert_eq!(
            resolve_pin(&json!("prefer-local"), &routing, &gateway),
            Some("local".to_owned())
        );
        assert_eq!(
            resolve_pin(&json!("prefer-hosted"), &routing, &gateway),
            Some("cloud".to_owned())
        );
        assert_eq!(
            resolve_pin(&json!("prefer-cloud"), &routing, &gateway),
            Some("cloud".to_owned())
        );
        assert_eq!(
            resolve_pin(&json!(" local "), &routing, &gateway),
            Some("local".to_owned())
        );
        assert_eq!(resolve_pin(&json!("upstream-id"), &routing, &gateway), None);
        Ok(())
    }

    #[test]
    fn classifier_prefer_directive_falls_through() -> Result<(), Box<dyn std::error::Error>> {
        let routing = RoutingConfig {
            classifier: Some(wayfinder_routing_core::ClassifierModel::new(
                vec!["local".to_owned(), "cloud".to_owned()],
                Default::default(),
                vec![0.0, 0.0],
            )?),
            ..RoutingConfig::default()
        };
        assert_eq!(
            resolve_pin(&json!("prefer-local"), &routing, &gateway()?),
            None
        );
        Ok(())
    }

    #[test]
    fn slash_directive_is_recognized_stripped_and_unknown_is_untouched()
    -> Result<(), Box<dyn std::error::Error>> {
        let routing = RoutingConfig::binary(0.5);
        let gateway = gateway()?;
        let messages = json!([
            {"role": "user", "content": "old"},
            {"role": "assistant", "content": "answer"},
            {"role": "user", "content": "  /cloud   do this"}
        ]);
        let resolved = resolve_slash_directive(&messages, &routing, &gateway)
            .ok_or("expected slash resolution")?;
        assert_eq!(resolved.pin.as_deref(), Some("cloud"));
        assert_eq!(resolved.messages[2]["content"], "do this");
        assert!(
            resolve_slash_directive(
                &json!([{"role":"user","content":"/help me"}]),
                &routing,
                &gateway
            )
            .is_none()
        );
        let auto = resolve_slash_directive(
            &json!([{"role":"user","content":"/auto hi"}]),
            &routing,
            &gateway,
        )
        .ok_or("expected auto slash resolution")?;
        assert_eq!(auto.pin, None);
        assert_eq!(auto.messages[0]["content"], "hi");
        Ok(())
    }

    #[test]
    fn override_headers_accept_aliases_and_reject_bad_values() -> Result<(), PolicyError> {
        assert_eq!(parse_threshold_header(None)?, None);
        assert_eq!(parse_threshold_header(Some("0.5"))?, Some(0.5));
        assert_eq!(parse_threshold_header(Some(" 0.25 "))?, Some(0.25));
        assert!(parse_threshold_header(Some("nan")).is_err());
        assert!(parse_threshold_header(Some("1.1")).is_err());
        assert_eq!(parse_route_on_header(Some(" USER "))?, Some(RouteOn::User));
        assert!(parse_route_on_header(Some("future")).is_err());
        assert!(resolve_sticky(Some("yes"), false)?);
        assert!(!resolve_sticky(Some("OFF"), true)?);
        assert!(resolve_sticky(Some("maybe"), false).is_err());
        assert_eq!(resolve_sticky_cooldown(Some(" 2 "), 0)?, 2);
        assert!(resolve_sticky_cooldown(Some("-1"), 0).is_err());
        Ok(())
    }

    #[test]
    fn threshold_override_requires_binary_and_drops_costs() -> Result<(), PolicyError> {
        let routing = RoutingConfig::binary(0.5);
        assert_eq!(
            threshold_tiers(&routing, 0.9)?,
            [Tier::new(0.0, "local"), Tier::new(0.9, "cloud")]
        );
        let mut multi = routing;
        multi.tiers.push(Tier::new(0.95, "frontier"));
        assert_eq!(
            threshold_tiers(&multi, 0.2).err(),
            Some(PolicyError::ThresholdRequiresBinary)
        );
        Ok(())
    }

    #[test]
    fn sticky_high_water_is_monotonic_or_decays() -> Result<(), Box<dyn std::error::Error>> {
        let routing = RoutingConfig::binary(0.2);
        let hard = "# Plan\n".to_owned() + &"- step\n".repeat(20);
        let messages = json!([
            {"role": "user", "content": hard},
            {"role": "assistant", "content": "answer"},
            {"role": "user", "content": "thanks"},
            {"role": "user", "content": "ok"}
        ]);
        assert_eq!(
            conversation_high_water(&messages, &routing, &routing.tiers, 0)?.as_deref(),
            Some("cloud")
        );
        assert_eq!(
            conversation_high_water(&messages, &routing, &routing.tiers, 2)?.as_deref(),
            Some("local")
        );
        Ok(())
    }

    #[test]
    fn scoring_tuning_merges_weights_replaces_terms_and_is_pure() -> Result<(), PolicyError> {
        let routing = RoutingConfig::binary(0.2);
        let original_weight = routing.weights.get("word_count");
        let tuned = apply_scoring_overrides(
            &routing,
            Some(&serde_json::json!({
                "weights": {"word_count": 5.0, "reasoning_term_count": 9},
                "lexicon": {
                    "reasoning_terms": [" FROBNICATE ", "frobnicate", ""],
                    "constraint_terms": ["EXACTLY"]
                },
                "future_field": true
            })),
        )?;
        assert_eq!(tuned.weights.get("word_count"), Some(5.0));
        assert_eq!(tuned.weights.get("reasoning_term_count"), Some(9.0));
        assert_eq!(original_weight, routing.weights.get("word_count"));
        assert_eq!(
            tuned.lexicon.reasoning_terms().collect::<Vec<_>>(),
            ["frobnicate"]
        );
        assert_eq!(
            tuned.lexicon.constraint_terms().collect::<Vec<_>>(),
            ["exactly"]
        );
        Ok(())
    }

    #[test]
    fn scoring_tuning_rejects_python_invalid_shapes() {
        let routing = RoutingConfig::default();
        for (value, message) in [
            (serde_json::json!([]), "wayfinder_tuning must be an object"),
            (
                serde_json::json!({"weights": {"ghost": 1}}),
                "wayfinder_tuning.weights: unknown feature 'ghost'",
            ),
            (
                serde_json::json!({"weights": {"word_count": -1}}),
                "wayfinder_tuning.weights.word_count must be a non-negative number",
            ),
            (
                serde_json::json!({"weights": {"word_count": true}}),
                "wayfinder_tuning.weights.word_count must be a non-negative number",
            ),
            (
                serde_json::json!({"lexicon": {"reasoning_terms": [1]}}),
                "wayfinder_tuning.lexicon.reasoning_terms must be a list of strings",
            ),
        ] {
            assert_eq!(
                apply_scoring_overrides(&routing, Some(&value))
                    .err()
                    .map(|error| error.to_string()),
                Some(message.to_owned())
            );
        }
    }

    #[test]
    fn scoring_tuning_caps_unique_cleaned_terms() {
        let routing = RoutingConfig::default();
        let terms = (0..=MAX_LEXICON_TERMS)
            .map(|index| Value::String(format!("term{index}")))
            .collect::<Vec<_>>();
        assert!(matches!(
            apply_scoring_overrides(
                &routing,
                Some(&serde_json::json!({"lexicon": {"reasoning_terms": terms}}))
            ),
            Err(PolicyError::BadTuning(_))
        ));
    }

    #[test]
    fn configured_gateway_model_type_remains_constructible() {
        let model = GatewayModel {
            provider: ProviderKind::OpenAiCompatible,
            base_url: Some("http://local/v1".to_owned()),
            model: "m".to_owned(),
            tier: None,
            api_key_env: None,
            api_key_cmd: None,
            cost_per_1k: None,
            fallbacks: Vec::new(),
            context_window: None,
            deployments: Vec::new(),
            deployment_selection: Default::default(),
        };
        assert_eq!(model.model, "m");
    }
}
