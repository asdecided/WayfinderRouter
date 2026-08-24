//! Wayfinder routing configuration parsing, validation, discovery, and emission.
//!
//! Semantic parsing is separate from future document mutation: serializing a
//! runtime struct must never overwrite an existing hand-edited TOML document.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;
use toml::Value;
use wayfinder_routing_core::{
    ClassifierModel, CoreError, DEFAULT_THRESHOLD, FEATURE_ORDER, Lexicon, RoutingConfig, Tier,
    Weights, binary_tiers, python_round,
};

/// Gateway configuration schema and validation.
pub mod gateway;

/// Default configuration filename.
pub const CONFIG_FILE: &str = "wayfinder-router.toml";
/// Environment variable selecting an explicit configuration file.
pub const CONFIG_PATH_ENV: &str = "WAYFINDER_CONFIG";
/// Environment variable overriding only the binary threshold mode.
pub const THRESHOLD_ENV: &str = "WAYFINDER_ROUTER_THRESHOLD";
const MAX_LEXICON_TERMS: usize = 2_000;

/// Tier-order parsing policy.
///
/// Product-facing callers use [`TierOrderPolicy::StrictInput`], matching the
/// current Python parser. The compatibility variant is retained only so old
/// unordered documents can be inspected and migrated explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TierOrderPolicy {
    /// Sort legacy unordered tiers before validating them. This is not a
    /// product default and must be selected explicitly by migration tooling.
    CompatibilitySort,
    /// Require a mutation fragment to already be strictly ascending.
    StrictInput,
}

/// Configuration error with a path/source label safe for presentation.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// TOML syntax is invalid.
    #[error("{where_}: invalid TOML: {message}")]
    InvalidToml {
        /// File or input label.
        where_: String,
        /// Parser diagnostic.
        message: String,
    },
    /// TOML is syntactically valid but violates the routing schema.
    #[error("{where_}: {message}")]
    InvalidValue {
        /// File or input label.
        where_: String,
        /// Schema diagnostic.
        message: String,
    },
    /// A selected file could not be read.
    #[error("cannot read {path}: {message}")]
    Read {
        /// Selected path.
        path: String,
        /// Sanitized operating-system diagnostic.
        message: String,
    },
}

impl From<CoreError> for ConfigError {
    fn from(error: CoreError) -> Self {
        Self::InvalidValue {
            where_: CONFIG_FILE.to_owned(),
            message: error.to_string(),
        }
    }
}

/// Find an explicit file or the nearest ancestor config.
///
/// An explicit missing file returns `None` and suppresses ancestor discovery,
/// matching the current Python implementation. The caller decides whether that
/// means defaults or a hard error.
#[must_use]
pub fn find_config_file(start_dir: &Path, explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return path.is_file().then(|| path.to_path_buf());
    }
    let resolved = start_dir.canonicalize().ok()?;
    resolved
        .ancestors()
        .map(|directory| directory.join(CONFIG_FILE))
        .find(|candidate| candidate.is_file())
}

/// Load routing configuration from discovery or return the binary defaults.
pub fn load_routing_config(
    start_dir: &Path,
    explicit: Option<&Path>,
    threshold_environment: Option<&str>,
    tier_order: TierOrderPolicy,
) -> Result<RoutingConfig, ConfigError> {
    let Some(path) = find_config_file(start_dir, explicit) else {
        let threshold = parse_environment_threshold(threshold_environment, DEFAULT_THRESHOLD)?;
        return Ok(RoutingConfig::binary(threshold));
    };
    let text = fs::read_to_string(&path).map_err(|error| ConfigError::Read {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    routing_config_from_toml(
        &text,
        &path.display().to_string(),
        threshold_environment,
        tier_order,
    )
}

/// Parse routing TOML with an explicit tier-order policy and optional threshold
/// environment value. `None` and `Some("")` mean no environment override.
pub fn routing_config_from_toml(
    text: &str,
    where_: &str,
    threshold_environment: Option<&str>,
    tier_order: TierOrderPolicy,
) -> Result<RoutingConfig, ConfigError> {
    let root: Value = toml::from_str(text).map_err(|error| ConfigError::InvalidToml {
        where_: where_.to_owned(),
        message: error.to_string(),
    })?;
    let Some(root_table) = root.as_table() else {
        return Err(invalid(where_, "document root must be a table"));
    };
    let routing = match root_table.get("routing") {
        None => None,
        Some(value) => Some(
            value
                .as_table()
                .ok_or_else(|| invalid(where_, "'[routing]' must be a table"))?,
        ),
    };
    let empty = toml::map::Map::new();
    let routing = routing.unwrap_or(&empty);
    let weights = parse_weights(routing.get("weights"), where_)?;
    let lexicon = match routing.get("lexicon") {
        Some(value) => parse_lexicon(value, where_)?,
        None => Lexicon::default(),
    };

    if let Some(value) = routing.get("classifier") {
        let classifier = parse_classifier(value, where_)?;
        return Ok(RoutingConfig {
            weights,
            tiers: binary_tiers(DEFAULT_THRESHOLD),
            classifier: Some(classifier),
            lexicon,
        });
    }
    if let Some(value) = routing.get("tiers") {
        return Ok(RoutingConfig {
            weights,
            tiers: parse_tiers(value, where_, tier_order)?,
            classifier: None,
            lexicon,
        });
    }

    let configured = parse_threshold(routing.get("threshold"), where_, DEFAULT_THRESHOLD)?;
    let threshold = parse_environment_threshold(threshold_environment, configured).map_err(
        |error| match error {
            ConfigError::InvalidValue { message, .. } => ConfigError::InvalidValue {
                where_: where_.to_owned(),
                message,
            },
            other => other,
        },
    )?;
    Ok(RoutingConfig {
        weights,
        tiers: binary_tiers(threshold),
        classifier: None,
        lexicon,
    })
}

/// Deterministically emit a routing fragment. This is suitable for generated
/// output, not for replacing a user's complete document.
#[must_use]
pub fn dump_routing_toml(config: &RoutingConfig) -> String {
    let mut blocks = Vec::new();
    let defaults = Weights::default();
    if config.weights != defaults {
        let items = FEATURE_ORDER
            .iter()
            .map(|name| {
                format!(
                    "{name} = {}",
                    format_number(config.weights.get(name).unwrap_or(0.0))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        blocks.push(format!("[routing]\nweights = {{ {items} }}"));
    }

    let default_lexicon = Lexicon::default();
    if config.lexicon != default_lexicon {
        let mut lines = vec!["[routing.lexicon]".to_owned()];
        let reasoning: Vec<&str> = config.lexicon.reasoning_terms().collect();
        let default_reasoning: Vec<&str> = default_lexicon.reasoning_terms().collect();
        if reasoning != default_reasoning {
            lines.push(format!(
                "reasoning_terms = [{}]",
                reasoning
                    .iter()
                    .map(|term| quote_toml(term))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let constraints: Vec<&str> = config.lexicon.constraint_terms().collect();
        let default_constraints: Vec<&str> = default_lexicon.constraint_terms().collect();
        if constraints != default_constraints {
            lines.push(format!(
                "constraint_terms = [{}]",
                constraints
                    .iter()
                    .map(|term| quote_toml(term))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        blocks.push(lines.join("\n"));
    }

    if let Some(classifier) = &config.classifier {
        let models = classifier
            .models()
            .iter()
            .map(|model| quote_toml(model))
            .collect::<Vec<_>>()
            .join(", ");
        let intercepts = classifier
            .intercepts()
            .iter()
            .copied()
            .map(format_number)
            .collect::<Vec<_>>()
            .join(", ");
        let mut lines = vec![
            "[routing.classifier]".to_owned(),
            format!("models = [{models}]"),
            format!("intercepts = [{intercepts}]"),
            String::new(),
            "[routing.classifier.weights]".to_owned(),
        ];
        for name in FEATURE_ORDER {
            let values = classifier
                .weights_for(name)
                .unwrap_or_default()
                .iter()
                .copied()
                .map(format_number)
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("{name} = [{values}]"));
        }
        blocks.push(lines.join("\n"));
    } else {
        let tiers = config
            .tiers
            .iter()
            .map(|tier| {
                let mut lines = vec![
                    "[[routing.tiers]]".to_owned(),
                    format!("min_score = {}", format_number(tier.min_score)),
                    format!("model = {}", quote_toml(&tier.model)),
                ];
                if let Some(cost) = tier.cost {
                    lines.push(format!("cost = {}", format_number(cost)));
                }
                lines.join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        blocks.push(tiers);
    }
    format!("{}\n", blocks.join("\n\n"))
}

/// Line-preserving boolean edit used by the whitelisted config seam.
///
/// Every existing line outside the selected assignment survives byte-for-byte.
/// A missing key is inserted immediately after an existing top-level table;
/// a missing table is appended.
#[must_use]
pub fn set_toml_bool(text: &str, table: &str, key: &str, value: bool) -> String {
    let rendered = if value { "true" } else { "false" };
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_owned).collect();
    if !text.is_empty() && !text.ends_with('\n') {
        let captured: usize = lines.iter().map(String::len).sum();
        if captured < text.len() {
            if let Some(tail) = text.get(captured..) {
                lines.push(tail.to_owned());
            }
        }
    }
    let mut section = String::new();
    let mut header_index = None;
    for (index, line) in lines.iter_mut().enumerate() {
        let stripped = line.trim();
        if stripped.starts_with('[') && stripped.ends_with(']') {
            section = stripped
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_owned();
            if section == table {
                header_index = Some(index);
            }
            continue;
        }
        if section == table && !stripped.starts_with('#') {
            let Some((name, _)) = stripped.split_once('=') else {
                continue;
            };
            if name.trim() == key {
                let indentation: String = line
                    .chars()
                    .take_while(|character| character.is_whitespace() && *character != '\n')
                    .collect();
                *line = format!("{indentation}{key} = {rendered}\n");
                return lines.concat();
            }
        }
    }
    if let Some(index) = header_index {
        lines.insert(index.saturating_add(1), format!("{key} = {rendered}\n"));
        return lines.concat();
    }
    let tail = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!("{text}{tail}\n[{table}]\n{key} = {rendered}\n")
}

/// Validate and apply a complete routing fragment while preserving every
/// non-routing byte from the existing document.
///
/// The current native routing form supports threshold, weights, and tiers with
/// `min_score`/`model` only. If the existing document contains classifier,
/// lexicon, tier cost, or an unknown routing field, this function refuses the
/// edit instead of silently dropping data.
pub fn apply_supported_routing_fragment(
    existing: &str,
    fragment: &str,
) -> Result<String, ConfigError> {
    validate_routing_fragment_shape(fragment)?;
    ensure_existing_routing_is_supported(existing)?;
    let desired = routing_config_from_toml(fragment, "stdin", None, TierOrderPolicy::StrictInput)?;
    let updated = replace_routing_family(existing, fragment);
    let actual = routing_config_from_toml(
        &updated,
        "updated document",
        None,
        TierOrderPolicy::StrictInput,
    )?;
    if actual != desired {
        return Err(invalid(
            "updated document",
            "routing edit did not take effect",
        ));
    }
    Ok(updated)
}

fn validate_routing_fragment_shape(fragment: &str) -> Result<(), ConfigError> {
    let root: Value = toml::from_str(fragment).map_err(|error| ConfigError::InvalidToml {
        where_: "stdin".to_owned(),
        message: error.to_string(),
    })?;
    let root = root
        .as_table()
        .ok_or_else(|| invalid("stdin", "routing apply needs a [routing] table"))?;
    let unknown_top: Vec<&str> = root
        .keys()
        .map(String::as_str)
        .filter(|key| *key != "routing")
        .collect();
    if !unknown_top.is_empty() {
        return Err(invalid(
            "stdin",
            format!(
                "routing apply may only include [routing] tables, not {}",
                unknown_top.join(", ")
            ),
        ));
    }
    let routing = root
        .get("routing")
        .and_then(Value::as_table)
        .ok_or_else(|| invalid("stdin", "routing apply needs a [routing] table"))?;
    let allowed = ["threshold", "tiers", "weights"];
    let unknown: Vec<&str> = routing
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if !unknown.is_empty() {
        return Err(invalid(
            "stdin",
            format!("routing apply cannot edit {}", unknown.join(", ")),
        ));
    }
    Ok(())
}

fn ensure_existing_routing_is_supported(existing: &str) -> Result<(), ConfigError> {
    let root: Value = toml::from_str(existing).map_err(|error| ConfigError::InvalidToml {
        where_: CONFIG_FILE.to_owned(),
        message: error.to_string(),
    })?;
    let Some(routing) = root
        .as_table()
        .and_then(|table| table.get("routing"))
        .and_then(Value::as_table)
    else {
        return Ok(());
    };
    let allowed = ["threshold", "tiers", "weights"];
    if let Some(field) = routing.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(
            CONFIG_FILE,
            format!("routing config contains unsupported field routing.{field}"),
        ));
    }
    if let Some(tiers) = routing.get("tiers").and_then(Value::as_array) {
        let allowed_tier = ["min_score", "model"];
        for tier in tiers {
            let Some(table) = tier.as_table() else {
                continue;
            };
            if let Some(field) = table
                .keys()
                .find(|key| !allowed_tier.contains(&key.as_str()))
            {
                return Err(invalid(
                    CONFIG_FILE,
                    format!("routing config contains unsupported field routing.tiers.{field}"),
                ));
            }
        }
    }
    Ok(())
}

fn replace_routing_family(existing: &str, fragment: &str) -> String {
    let mut kept = String::new();
    let mut in_routing = false;
    for line in existing.split_inclusive('\n') {
        let stripped = line.trim();
        if stripped.starts_with('[') && stripped.ends_with(']') {
            let header = stripped.trim_matches(['[', ']']).trim();
            in_routing = header == "routing" || header.starts_with("routing.");
        }
        if !in_routing {
            kept.push_str(line);
        }
    }
    if !existing.ends_with('\n') {
        let captured: usize = existing.split_inclusive('\n').map(str::len).sum();
        if captured < existing.len() && !in_routing {
            if let Some(tail) = existing.get(captured..) {
                kept.push_str(tail);
            }
        }
    }
    if !kept.is_empty() && !kept.ends_with('\n') {
        kept.push('\n');
    }
    if !kept.is_empty() && !kept.ends_with("\n\n") {
        kept.push('\n');
    }
    kept.push_str(fragment.trim());
    kept.push('\n');
    kept
}

fn parse_threshold(value: Option<&Value>, where_: &str, default: f64) -> Result<f64, ConfigError> {
    let Some(value) = value else {
        return Ok(default);
    };
    let threshold = number(value)
        .ok_or_else(|| invalid(where_, "'routing.threshold' must be a number in 0.0-1.0"))?;
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(invalid(
            where_,
            "'routing.threshold' must be a number in 0.0-1.0",
        ));
    }
    Ok(threshold)
}

fn parse_environment_threshold(raw: Option<&str>, default: f64) -> Result<f64, ConfigError> {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(default);
    };
    let value = raw.parse::<f64>().map_err(|_| {
        invalid(
            THRESHOLD_ENV,
            format!("{THRESHOLD_ENV} must be a number, got {raw:?}"),
        )
    })?;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(invalid(
            THRESHOLD_ENV,
            format!("{THRESHOLD_ENV} must be between 0.0 and 1.0, got {value}"),
        ));
    }
    Ok(value)
}

fn parse_weights(value: Option<&Value>, where_: &str) -> Result<Weights, ConfigError> {
    let mut weights = Weights::default();
    let Some(value) = value else {
        return Ok(weights);
    };
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'routing.weights' must be a table"))?;
    for (name, raw_weight) in table {
        if !FEATURE_ORDER.contains(&name.as_str()) {
            return Err(invalid(
                where_,
                format!(
                    "'routing.weights.{name}' is not a known feature (one of {})",
                    FEATURE_ORDER.join(", ")
                ),
            ));
        }
        let weight = number(raw_weight).ok_or_else(|| {
            invalid(
                where_,
                format!("'routing.weights.{name}' must be a non-negative number"),
            )
        })?;
        if !weight.is_finite() || weight < 0.0 {
            return Err(invalid(
                where_,
                format!("'routing.weights.{name}' must be a non-negative number"),
            ));
        }
        if !weights.set(name, weight) {
            return Err(invalid(where_, "internal feature mapping is incomplete"));
        }
    }
    Ok(weights)
}

fn parse_lexicon(value: &Value, where_: &str) -> Result<Lexicon, ConfigError> {
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'[routing.lexicon]' must be a table"))?;
    let known = ["constraint_terms", "reasoning_terms"];
    let unknown: Vec<&str> = table
        .keys()
        .map(String::as_str)
        .filter(|key| !known.contains(key))
        .collect();
    if !unknown.is_empty() {
        return Err(invalid(
            where_,
            format!(
                "unknown 'routing.lexicon' keys: {} (known: {})",
                unknown.join(", "),
                known.join(", ")
            ),
        ));
    }
    let defaults = Lexicon::default();
    let reasoning = match table.get("reasoning_terms") {
        Some(value) => parse_term_list(value, where_, "routing.lexicon.reasoning_terms")?,
        None => defaults.reasoning_terms().map(str::to_owned).collect(),
    };
    let constraints = match table.get("constraint_terms") {
        Some(value) => parse_term_list(value, where_, "routing.lexicon.constraint_terms")?,
        None => defaults.constraint_terms().map(str::to_owned).collect(),
    };
    Ok(Lexicon::new(reasoning, constraints))
}

fn parse_term_list(value: &Value, where_: &str, label: &str) -> Result<Vec<String>, ConfigError> {
    let values = value.as_array().ok_or_else(|| {
        invalid(
            where_,
            format!("'{label}' must be a list of non-empty strings"),
        )
    })?;
    if values.len() > MAX_LEXICON_TERMS {
        return Err(invalid(
            where_,
            format!("'{label}' has more than {MAX_LEXICON_TERMS} terms"),
        ));
    }
    values
        .iter()
        .map(|entry| {
            let term = entry.as_str().ok_or_else(|| {
                invalid(
                    where_,
                    format!("'{label}' must be a list of non-empty strings"),
                )
            })?;
            let term = term.trim();
            if term.is_empty() {
                return Err(invalid(
                    where_,
                    format!("'{label}' must be a list of non-empty strings"),
                ));
            }
            Ok(term.to_ascii_lowercase())
        })
        .collect()
}

fn parse_tiers(
    value: &Value,
    where_: &str,
    order: TierOrderPolicy,
) -> Result<Vec<Tier>, ConfigError> {
    let values = value
        .as_array()
        .filter(|values| !values.is_empty())
        .ok_or_else(|| {
            invalid(
                where_,
                "'routing.tiers' must be a non-empty array of tables",
            )
        })?;
    let mut tiers = Vec::with_capacity(values.len());
    for value in values {
        let table = value
            .as_table()
            .ok_or_else(|| invalid(where_, "each '[[routing.tiers]]' must be a table"))?;
        let min_score = table
            .get("min_score")
            .and_then(number)
            .ok_or_else(|| invalid(where_, "tier 'min_score' must be a number in 0.0-1.0"))?;
        if !min_score.is_finite() || !(0.0..=1.0).contains(&min_score) {
            return Err(invalid(
                where_,
                "tier 'min_score' must be a number in 0.0-1.0",
            ));
        }
        let model = table
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
            .ok_or_else(|| invalid(where_, "tier 'model' must be a non-empty string"))?;
        let cost = match table.get("cost") {
            None => None,
            Some(value) => {
                let cost = number(value)
                    .ok_or_else(|| invalid(where_, "tier 'cost' must be a non-negative number"))?;
                if !cost.is_finite() || cost < 0.0 {
                    return Err(invalid(where_, "tier 'cost' must be a non-negative number"));
                }
                Some(cost)
            }
        };
        tiers.push(Tier {
            min_score,
            model: model.to_owned(),
            cost,
        });
    }
    if order == TierOrderPolicy::CompatibilitySort {
        tiers.sort_by(|left, right| left.min_score.total_cmp(&right.min_score));
    }
    if tiers.first().map(|tier| tier.min_score) != Some(0.0) {
        return Err(invalid(where_, "the first tier must have min_score = 0.0"));
    }
    if tiers.windows(2).any(|pair| {
        pair.get(1).map(|tier| tier.min_score).unwrap_or(0.0)
            <= pair.first().map(|tier| tier.min_score).unwrap_or(0.0)
    }) {
        return Err(invalid(
            where_,
            "tier 'min_score' values must be strictly ascending",
        ));
    }
    Ok(tiers)
}

fn parse_classifier(value: &Value, where_: &str) -> Result<ClassifierModel, ConfigError> {
    let table = value
        .as_table()
        .ok_or_else(|| invalid(where_, "'[routing.classifier]' must be a table"))?;
    let models = table
        .get("models")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        })
        .filter(|models| {
            models.len() >= 2
                && models.iter().all(|model| !model.is_empty())
                && models.iter().collect::<BTreeSet<_>>().len() == models.len()
        })
        .ok_or_else(|| {
            invalid(
                where_,
                "'routing.classifier.models' must be 2+ unique non-empty strings",
            )
        })?;
    let intercepts = parse_number_vector(
        table.get("intercepts"),
        where_,
        "routing.classifier.intercepts",
        models.len(),
    )?;
    let raw_weights = table
        .get("weights")
        .and_then(Value::as_table)
        .ok_or_else(|| invalid(where_, "'[routing.classifier.weights]' must be a table"))?;
    let mut weights = BTreeMap::new();
    for name in raw_weights.keys() {
        if !FEATURE_ORDER.contains(&name.as_str()) {
            return Err(invalid(
                where_,
                format!("'routing.classifier.weights.{name}' is not a known feature"),
            ));
        }
    }
    for name in FEATURE_ORDER {
        let vector = match raw_weights.get(name) {
            Some(value) => parse_number_vector(
                Some(value),
                where_,
                &format!("routing.classifier.weights.{name}"),
                models.len(),
            )?,
            None => vec![0.0; models.len()],
        };
        weights.insert(name.to_owned(), vector);
    }
    ClassifierModel::new(models, weights, intercepts).map_err(|error| {
        let rendered = error.to_string();
        let message = rendered
            .strip_prefix("invalid classifier: ")
            .unwrap_or(&rendered)
            .to_owned();
        invalid(where_, message)
    })
}

fn parse_number_vector(
    value: Option<&Value>,
    where_: &str,
    label: &str,
    count: usize,
) -> Result<Vec<f64>, ConfigError> {
    let values = value
        .and_then(Value::as_array)
        .filter(|values| values.len() == count);
    let Some(values) = values else {
        return Err(invalid(
            where_,
            format!("'{label}' must be a list of {count} numbers"),
        ));
    };
    values
        .iter()
        .map(|value| {
            number(value)
                .filter(|number| number.is_finite())
                .ok_or_else(|| {
                    invalid(
                        where_,
                        format!("'{label}' must be a list of {count} numbers"),
                    )
                })
        })
        .collect()
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(number) => Some(*number as f64),
        Value::Float(number) => Some(*number),
        _ => None,
    }
}

fn format_number(value: f64) -> String {
    format!("{:?}", python_round(value, 6))
}

fn quote_toml(value: &str) -> String {
    Value::String(value.to_owned()).to_string()
}

fn invalid(where_: &str, message: impl Into<String>) -> ConfigError {
    ConfigError::InvalidValue {
        where_: where_.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wayfinder_routing_core::{RoutingMode, score_complexity};

    fn parse(text: &str, order: TierOrderPolicy) -> Result<RoutingConfig, ConfigError> {
        routing_config_from_toml(text, "fixture", None, order)
    }

    #[test]
    fn no_routing_config_uses_binary_defaults() -> Result<(), ConfigError> {
        let config = parse(
            "[gateway]\noffline = true\n",
            TierOrderPolicy::CompatibilitySort,
        )?;
        assert_eq!(
            config.tiers.get(1).map(|tier| tier.min_score),
            Some(DEFAULT_THRESHOLD)
        );
        Ok(())
    }

    #[test]
    fn threshold_environment_applies_only_to_binary_mode() -> Result<(), ConfigError> {
        let binary = routing_config_from_toml(
            "[routing]\nthreshold = 0.4\n",
            "fixture",
            Some("0.7"),
            TierOrderPolicy::CompatibilitySort,
        )?;
        assert_eq!(binary.tiers.get(1).map(|tier| tier.min_score), Some(0.7));
        let tiers = routing_config_from_toml(
            "[[routing.tiers]]\nmin_score = 0.0\nmodel = \"a\"\n",
            "fixture",
            Some("0.7"),
            TierOrderPolicy::CompatibilitySort,
        )?;
        assert_eq!(tiers.tiers.len(), 1);
        Ok(())
    }

    #[test]
    fn strict_tier_order_is_product_contract_and_legacy_sort_is_explicit() -> Result<(), ConfigError>
    {
        let text = concat!(
            "[[routing.tiers]]\nmin_score = 0.6\nmodel = \"large\"\n\n",
            "[[routing.tiers]]\nmin_score = 0.0\nmodel = \"small\"\n\n",
            "[[routing.tiers]]\nmin_score = 0.3\nmodel = \"medium\"\n",
        );
        let compatible = parse(text, TierOrderPolicy::CompatibilitySort)?;
        let models: Vec<&str> = compatible
            .tiers
            .iter()
            .map(|tier| tier.model.as_str())
            .collect();
        assert_eq!(models, ["small", "medium", "large"]);
        assert!(parse(text, TierOrderPolicy::StrictInput).is_err());
        Ok(())
    }

    #[test]
    fn bools_unknown_weights_and_nonfinite_numbers_are_rejected() {
        for text in [
            "[routing]\nthreshold = true\n",
            "[routing]\nweights = { surprise = 1 }\n",
            "[routing]\nweights = { word_count = -1 }\n",
            "[routing]\nweights = { word_count = nan }\n",
        ] {
            assert!(parse(text, TierOrderPolicy::CompatibilitySort).is_err());
        }
    }

    #[test]
    fn classifier_precedes_tiers_and_ties_to_first() -> Result<(), ConfigError> {
        let text = concat!(
            "[[routing.tiers]]\nmin_score = 0.0\nmodel = \"tier\"\n\n",
            "[routing.classifier]\nmodels = [\"first\", \"second\"]\n",
            "intercepts = [0, 0]\n\n[routing.classifier.weights]\n",
        );
        let config = parse(text, TierOrderPolicy::CompatibilitySort)?;
        let decision = score_complexity("hello", &config).map_err(ConfigError::from)?;
        assert_eq!(decision.mode, RoutingMode::Classifier);
        assert_eq!(decision.recommendation, "first");
        Ok(())
    }

    #[test]
    fn custom_lexicon_is_lowercased_and_bounded() -> Result<(), ConfigError> {
        let config = parse(
            "[routing.lexicon]\nreasoning_terms = [\"HARD\"]\nconstraint_terms = [\"ONLY\"]\n",
            TierOrderPolicy::CompatibilitySort,
        )?;
        let decision = score_complexity("hard only", &config).map_err(ConfigError::from)?;
        assert_eq!(decision.features.reasoning_term_count, 1);
        assert_eq!(decision.features.constraint_term_count, 1);
        Ok(())
    }

    #[test]
    fn dump_round_trips_weights_lexicon_tiers_and_costs() -> Result<(), ConfigError> {
        let text = concat!(
            "[routing]\nweights = { word_count = 9 }\n\n",
            "[routing.lexicon]\nreasoning_terms = [\"hard\"]\n\n",
            "[[routing.tiers]]\nmin_score = 0.0\nmodel = \"small\"\ncost = 0.2\n\n",
            "[[routing.tiers]]\nmin_score = 0.6\nmodel = \"large\"\ncost = 1.0\n",
        );
        let config = parse(text, TierOrderPolicy::CompatibilitySort)?;
        let dumped = dump_routing_toml(&config);
        let again = parse(&dumped, TierOrderPolicy::CompatibilitySort)?;
        assert_eq!(again, config);
        Ok(())
    }

    #[test]
    fn dump_round_trips_classifier() -> Result<(), ConfigError> {
        let text = concat!(
            "[routing.classifier]\nmodels = [\"small\", \"big\"]\n",
            "intercepts = [1.0, 0.0]\n\n",
            "[routing.classifier.weights]\nword_count = [0.0, 5.0]\n",
        );
        let config = parse(text, TierOrderPolicy::CompatibilitySort)?;
        let again = parse(
            &dump_routing_toml(&config),
            TierOrderPolicy::CompatibilitySort,
        )?;
        assert_eq!(again, config);
        Ok(())
    }

    #[test]
    fn bool_edit_preserves_unrelated_lines() {
        let source = concat!(
            "# keep\n[gateway]\n# note\noffline = false # old\n\n",
            "[gateway.models.local]\nmodel = \"x\"\n",
        );
        let edited = set_toml_bool(source, "gateway", "offline", true);
        assert!(edited.contains("# keep\n[gateway]\n# note\noffline = true\n"));
        assert!(edited.contains("[gateway.models.local]\nmodel = \"x\"\n"));
    }

    #[test]
    fn routing_apply_preserves_gateway_and_rejects_unknown_owned_data() -> Result<(), ConfigError> {
        let source = concat!(
            "# preface\n[gateway]\noffline = true\n\n",
            "[gateway.models.local]\nbase_url = \"http://localhost:11434/v1\"\n",
            "model = \"llama\"\n\n[routing]\nthreshold = 0.2\n",
        );
        let edited = apply_supported_routing_fragment(source, "[routing]\nthreshold = 0.73\n")?;
        assert!(edited.contains("# preface\n[gateway]\noffline = true\n"));
        assert!(
            edited.contains("[gateway.models.local]\nbase_url = \"http://localhost:11434/v1\"")
        );
        assert!(edited.contains("threshold = 0.73"));
        assert!(!edited.contains("threshold = 0.2"));

        let with_unknown = "[routing.lexicon]\nreasoning_terms = [\"hard\"]\n";
        assert!(
            apply_supported_routing_fragment(with_unknown, "[routing]\nthreshold = 0.5\n").is_err()
        );
        let with_cost = "[[routing.tiers]]\nmin_score = 0.0\nmodel = \"local\"\ncost = 0.1\n";
        assert!(
            apply_supported_routing_fragment(with_cost, "[routing]\nthreshold = 0.5\n").is_err()
        );
        Ok(())
    }

    #[test]
    fn routing_apply_preserves_non_routing_bytes_across_varied_documents() -> Result<(), ConfigError>
    {
        let fragment = "[routing]\nthreshold = 0.61\n";
        for (source, preserved) in [
            ("", ""),
            ("# only a comment\n", "# only a comment\n"),
            ("[gateway]\noffline = true", "[gateway]\noffline = true"),
            (
                "# lead\n[routing]\nthreshold = 0.2\n\n[gateway]\n# keep\noffline = false\n",
                "# lead\n[gateway]\n# keep\noffline = false\n",
            ),
            (
                "[gateway.models.\"odd.name\"]\nmodel = \"x\"\n\n[[routing.tiers]]\nmin_score = 0.0\nmodel = \"a\"\n",
                "[gateway.models.\"odd.name\"]\nmodel = \"x\"\n",
            ),
        ] {
            let edited = apply_supported_routing_fragment(source, fragment)?;
            for line in preserved.lines() {
                assert!(
                    edited.contains(line),
                    "missing preserved line {line:?} from {source:?}"
                );
            }
            assert!(edited.ends_with(fragment));
            let reparsed = parse(&edited, TierOrderPolicy::StrictInput)?;
            assert_eq!(reparsed.tiers.get(1).map(|tier| tier.min_score), Some(0.61));
        }
        Ok(())
    }
}
