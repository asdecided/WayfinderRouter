use std::collections::BTreeMap;
use std::error::Error;

use serde::Deserialize;
use wayfinder_routing_core::{
    ClassifierModel, ComplexityScore, RoutingConfig, RoutingMode, Tier, Weights, score_complexity,
};

const MIGRATION_GOLDEN: &str = include_str!("../fixtures/migration-golden.json");
const ROUTING_BOUNDARIES: &str = include_str!("../fixtures/routing-boundaries.json");

#[derive(Debug, Deserialize)]
struct ExpectedDecision {
    name: String,
    text: String,
    score: f64,
    recommendation: String,
    features: BTreeMap<String, u64>,
}

#[derive(Debug, Deserialize)]
struct BoundaryDecision {
    name: String,
    text: String,
    config: BoundaryConfig,
    score: f64,
    recommendation: String,
    features: BTreeMap<String, u64>,
}

#[derive(Debug, Deserialize)]
struct BoundaryConfig {
    mode: String,
    threshold: Option<f64>,
    tiers: Option<Vec<TierFixture>>,
    models: Option<Vec<String>>,
    intercepts: Option<Vec<f64>>,
    weights: Option<BTreeMap<String, Vec<f64>>>,
}

#[derive(Debug, Deserialize)]
struct TierFixture {
    min_score: f64,
    model: String,
}

#[test]
fn rust_matches_all_21_migration_golden_decisions() -> Result<(), Box<dyn Error>> {
    let cases: Vec<ExpectedDecision> = serde_json::from_str(MIGRATION_GOLDEN)?;

    assert_eq!(cases.len(), 21);
    for expected in &cases {
        let actual = score_complexity(&expected.text, &RoutingConfig::default())?;
        assert_decision_matches(
            &expected.name,
            expected.score,
            &expected.recommendation,
            &expected.features,
            &actual,
        );
        assert_eq!(actual.mode, RoutingMode::Tiered, "{} mode", expected.name);
    }
    Ok(())
}

#[test]
fn rust_preserves_frozen_tier_and_classifier_boundaries() -> Result<(), Box<dyn Error>> {
    let cases: Vec<BoundaryDecision> = serde_json::from_str(ROUTING_BOUNDARIES)?;

    assert_eq!(cases.len(), 8);
    for expected in &cases {
        let config = config_from_fixture(&expected.config)?;
        let expected_mode = match expected.config.mode.as_str() {
            "classifier" => RoutingMode::Classifier,
            "binary" | "tiers" => RoutingMode::Tiered,
            mode => return Err(format!("unknown boundary mode: {mode}").into()),
        };
        let actual = score_complexity(&expected.text, &config)?;
        assert_decision_matches(
            &expected.name,
            expected.score,
            &expected.recommendation,
            &expected.features,
            &actual,
        );
        assert_eq!(actual.mode, expected_mode, "{} mode", expected.name);
    }
    Ok(())
}

fn config_from_fixture(fixture: &BoundaryConfig) -> Result<RoutingConfig, Box<dyn Error>> {
    match fixture.mode.as_str() {
        "binary" => Ok(RoutingConfig::binary(
            fixture.threshold.ok_or("binary threshold missing")?,
        )),
        "tiers" => Ok(RoutingConfig {
            weights: Weights::default(),
            tiers: fixture
                .tiers
                .as_deref()
                .ok_or("tiers missing")?
                .iter()
                .map(|tier| Tier::new(tier.min_score, &tier.model))
                .collect(),
            ..RoutingConfig::default()
        }),
        "classifier" => Ok(RoutingConfig {
            weights: Weights::default(),
            classifier: Some(ClassifierModel::new(
                fixture.models.clone().ok_or("models missing")?,
                fixture.weights.clone().ok_or("weights missing")?,
                fixture.intercepts.clone().ok_or("intercepts missing")?,
            )?),
            ..RoutingConfig::default()
        }),
        mode => Err(format!("unknown boundary mode: {mode}").into()),
    }
}

fn assert_decision_matches(
    name: &str,
    expected_score: f64,
    expected_recommendation: &str,
    expected_features: &BTreeMap<String, u64>,
    actual: &ComplexityScore,
) {
    assert_eq!(actual.score, expected_score, "{name} score");
    assert_eq!(
        actual.recommendation, expected_recommendation,
        "{name} recommendation"
    );
    assert_eq!(
        expected_features.len(),
        wayfinder_routing_core::FEATURE_ORDER.len(),
        "{name} feature count"
    );
    for (feature, expected_value) in expected_features {
        assert_eq!(
            actual.features.get_named(feature),
            Some(*expected_value),
            "{name} feature {feature}"
        );
    }
}
