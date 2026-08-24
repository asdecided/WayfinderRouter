use std::collections::BTreeMap;
use std::error::Error;

use serde::Deserialize;
use wayfinder_config::{TierOrderPolicy, routing_config_from_toml};
use wayfinder_routing_core::{FEATURE_ORDER, RoutingConfig, RoutingMode, score_complexity};

const ROUTING_CONFIG_VECTORS: &str = include_str!("../fixtures/routing-config.json");
const WHERE: &str = "compat-vector";
const HEADING_LIST_PROMPT: &str = "# Plan\n\n## Steps\n- one\n- two\n- three\n1. first\n2. second";

#[derive(Debug, Deserialize)]
struct ConfigCase {
    name: String,
    toml: String,
    threshold_environment: Option<String>,
    outcome: ExpectedOutcome,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OutcomeStatus {
    Valid,
    Invalid,
}

#[derive(Debug, Deserialize)]
struct ExpectedOutcome {
    status: OutcomeStatus,
    summary: Option<ConfigSummary>,
    legacy_diagnostic: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ConfigSummary {
    mode: String,
    weights: BTreeMap<String, f64>,
    lexicon: LexiconSummary,
    tiers: Option<Vec<TierSummary>>,
    classifier: Option<ClassifierSummary>,
    decisions: Vec<DecisionSummary>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct LexiconSummary {
    reasoning_terms: Vec<String>,
    constraint_terms: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct TierSummary {
    min_score: f64,
    model: String,
    cost: Option<f64>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ClassifierSummary {
    models: Vec<String>,
    intercepts: Vec<f64>,
    weights: BTreeMap<String, Vec<f64>>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct DecisionSummary {
    name: String,
    score: f64,
    recommendation: String,
    mode: String,
}

#[test]
fn strict_input_preserves_reviewed_outcomes_and_decisions() -> Result<(), Box<dyn Error>> {
    let cases: Vec<ConfigCase> = serde_json::from_str(ROUTING_CONFIG_VECTORS)?;
    let mut valid_count = 0_usize;
    let mut invalid_count = 0_usize;

    assert_eq!(cases.len(), 32);
    for case in &cases {
        let parsed = routing_config_from_toml(
            &case.toml,
            WHERE,
            case.threshold_environment.as_deref(),
            TierOrderPolicy::StrictInput,
        );
        match (case.outcome.status, parsed) {
            (OutcomeStatus::Valid, Ok(config)) => {
                valid_count = valid_count.saturating_add(1);
                let summary = case
                    .outcome
                    .summary
                    .as_ref()
                    .ok_or_else(|| format!("{} valid fixture omitted summary", case.name))?;
                let actual = summarize_config(&config)?;
                assert_eq!(actual, *summary, "{} summary", case.name);
            }
            (OutcomeStatus::Invalid, Err(rust_error)) => {
                invalid_count = invalid_count.saturating_add(1);
                let legacy_diagnostic =
                    case.outcome.legacy_diagnostic.as_deref().ok_or_else(|| {
                        format!("{} invalid fixture omitted diagnostic", case.name)
                    })?;
                assert!(
                    !legacy_diagnostic.is_empty(),
                    "{} fixture diagnostic",
                    case.name
                );
                assert!(
                    !rust_error.to_string().is_empty(),
                    "{} current diagnostic",
                    case.name
                );
            }
            (OutcomeStatus::Valid, Err(error)) => {
                return Err(format!(
                    "{}: current parser rejected fixture-valid TOML: {error}",
                    case.name
                )
                .into());
            }
            (OutcomeStatus::Invalid, Ok(_)) => {
                let legacy_diagnostic = case
                    .outcome
                    .legacy_diagnostic
                    .as_deref()
                    .unwrap_or("missing fixture diagnostic");
                return Err(format!(
                    "{}: current parser accepted fixture-invalid TOML ({legacy_diagnostic})",
                    case.name
                )
                .into());
            }
        }
    }
    assert_eq!(valid_count, 9);
    assert_eq!(invalid_count, 23);
    Ok(())
}

#[test]
fn compatibility_sort_restores_committed_unordered_tier_contract() -> Result<(), Box<dyn Error>> {
    let cases: Vec<ConfigCase> = serde_json::from_str(ROUTING_CONFIG_VECTORS)?;
    let Some(case) = cases
        .iter()
        .find(|case| case.name == "descending_tiers_are_rejected_by_frozen_contract")
    else {
        return Err("descending-tier fixture is missing".into());
    };
    assert_eq!(case.outcome.status, OutcomeStatus::Invalid);

    let config = routing_config_from_toml(
        &case.toml,
        WHERE,
        case.threshold_environment.as_deref(),
        TierOrderPolicy::CompatibilitySort,
    )?;
    let ordered: Vec<(f64, &str)> = config
        .tiers
        .iter()
        .map(|tier| (tier.min_score, tier.model.as_str()))
        .collect();

    assert_eq!(
        ordered,
        vec![(0.0, "small"), (0.3, "medium"), (0.6, "large")]
    );
    let decision = score_complexity(HEADING_LIST_PROMPT, &config)?;
    assert_eq!(decision.score, 0.15);
    assert_eq!(decision.recommendation, "small");
    Ok(())
}

fn summarize_config(config: &RoutingConfig) -> Result<ConfigSummary, Box<dyn Error>> {
    let mut weights = BTreeMap::new();
    for name in FEATURE_ORDER {
        let Some(value) = config.weights.get(name) else {
            return Err(format!("missing weight for {name}").into());
        };
        weights.insert(name.to_owned(), value);
    }

    let classifier = match &config.classifier {
        Some(classifier) => {
            let mut classifier_weights = BTreeMap::new();
            for name in FEATURE_ORDER {
                let Some(values) = classifier.weights_for(name) else {
                    return Err(format!("missing classifier weights for {name}").into());
                };
                classifier_weights.insert(name.to_owned(), values.to_vec());
            }
            Some(ClassifierSummary {
                models: classifier.models().to_vec(),
                intercepts: classifier.intercepts().to_vec(),
                weights: classifier_weights,
            })
        }
        None => None,
    };
    let tiers = config.classifier.is_none().then(|| {
        config
            .tiers
            .iter()
            .map(|tier| TierSummary {
                min_score: tier.min_score,
                model: tier.model.clone(),
                cost: tier.cost,
            })
            .collect()
    });

    let mut decisions = Vec::new();
    for (name, text) in probes() {
        let decision = score_complexity(&text, config)?;
        decisions.push(DecisionSummary {
            name: name.to_owned(),
            score: decision.score,
            recommendation: decision.recommendation,
            mode: mode_name(decision.mode).to_owned(),
        });
    }

    Ok(ConfigSummary {
        mode: if config.classifier.is_some() {
            "classifier".to_owned()
        } else {
            "tiered".to_owned()
        },
        weights,
        lexicon: LexiconSummary {
            reasoning_terms: config
                .lexicon
                .reasoning_terms()
                .map(str::to_owned)
                .collect(),
            constraint_terms: config
                .lexicon
                .constraint_terms()
                .map(str::to_owned)
                .collect(),
        },
        tiers,
        classifier,
        decisions,
    })
}

fn probes() -> Vec<(&'static str, String)> {
    vec![
        ("empty", String::new()),
        ("headings_lists", HEADING_LIST_PROMPT.to_owned()),
        (
            "lexicon",
            "Differential must be exactly correct?".to_owned(),
        ),
        ("classifier_tie", "word ".repeat(80).trim().to_owned()),
        ("classifier_above", "word ".repeat(81).trim().to_owned()),
    ]
}

const fn mode_name(mode: RoutingMode) -> &'static str {
    match mode {
        RoutingMode::Tiered => "tiered",
        RoutingMode::Classifier => "classifier",
    }
}
