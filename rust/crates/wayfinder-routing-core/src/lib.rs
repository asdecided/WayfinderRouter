//! Pure, deterministic Wayfinder routing core.
//!
//! This crate is the single authoritative routing implementation shared by
//! gateway and embedded Apple hosts. It deliberately has no network, process,
//! filesystem, async-runtime, platform-framework, or secret dependencies.

#![forbid(unsafe_code)]

pub mod calibration;
pub mod profiles;

use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;
use thiserror::Error;

pub use wayfinder_runtime_contracts::{
    BillingClass, CandidateAssessment, DestinationCapabilities, DestinationSnapshot,
    ExclusionReason, ExecutionBoundary, InferenceSurface, ModalityCapabilities, PrivacyPosture,
    ProviderReadiness, RUNTIME_CONTRACT_VERSION, RouteExplanation, RoutePlan, RouteReceipt,
    RoutingRequest, RoutingRequirements,
};

/// Evidence-backed starter cut for the binary local/cloud router.
///
/// This is a bootstrap policy, not a universal optimum. Operators should use
/// the native calibrator with representative labelled traffic before treating
/// a threshold as settled policy (WF-ADR-0078).
pub const DEFAULT_THRESHOLD: f64 = 0.01;

/// Stable feature order used by reports, classifier vectors, and score accumulation.
pub const FEATURE_ORDER: [&str; 11] = [
    "word_count",
    "heading_count",
    "max_heading_depth",
    "list_item_count",
    "link_count",
    "code_block_count",
    "table_row_count",
    "reasoning_term_count",
    "math_symbol_count",
    "constraint_term_count",
    "question_count",
];

const SATURATION: [f64; 11] = [400.0, 8.0, 4.0, 15.0, 10.0, 4.0, 12.0, 2.0, 6.0, 3.0, 3.0];

// Python sums DEFAULT_WEIGHTS.values() in dict insertion order, which differs
// from FEATURE_ORDER. Parsed config updates preserve this original order.
const WEIGHT_SUM_ORDER: [usize; 11] = [0, 3, 1, 5, 6, 4, 2, 7, 8, 9, 10];

const REASONING_TERMS: &[&str] = &[
    "prove",
    "proof",
    "proofs",
    "proven",
    "derive",
    "derives",
    "derivation",
    "theorem",
    "theorems",
    "lemma",
    "lemmas",
    "corollary",
    "axiom",
    "axioms",
    "irrational",
    "undecidable",
    "undecidability",
    "decidable",
    "infinitely",
    "asymptotic",
    "complexity",
    "invariant",
    "invariants",
    "concurrency",
    "concurrent",
    "deadlock",
    "induction",
    "contradiction",
    "optimal",
    "optimality",
    "optimize",
    "optimise",
    "minimise",
    "minimize",
    "maximise",
    "maximize",
    "recurrence",
    "halting",
    "eigenvalue",
    "eigenvalues",
    "integral",
    "derivative",
    "polynomial",
    "prime",
    "primes",
    "modulo",
    "isomorphism",
    "monotonic",
    "bijection",
    "injective",
    "surjective",
    "combinatorial",
];

const CONSTRAINT_TERMS: &[&str] = &[
    "must",
    "without",
    "only",
    "ensure",
    "exactly",
    "guarantee",
    "constraint",
    "constraints",
    "subject",
    "preserving",
    "preserve",
];

/// Errors from the pure scorer. Constant regex failures are reported rather
/// than converted into a process panic.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CoreError {
    /// A built-in regular expression could not be constructed.
    #[error("built-in feature pattern is invalid: {0}")]
    InvalidPattern(String),
    /// A tiered router was constructed without a tier.
    #[error("tiered routing requires at least one tier")]
    MissingTier,
    /// A classifier shape is invalid.
    #[error("invalid classifier: {0}")]
    InvalidClassifier(String),
    /// A platform-neutral runtime contract is invalid.
    #[error("invalid runtime contract: {0}")]
    InvalidContract(String),
}

#[derive(Debug)]
struct Patterns {
    heading: Regex,
    list: Regex,
    table_row: Regex,
    fence: Regex,
    link: Regex,
    word_token: Regex,
    math_symbol: Regex,
}

impl Patterns {
    fn compile() -> Result<Self, regex::Error> {
        Ok(Self {
            heading: Regex::new(r"^(#{1,6})\s+\S")?,
            list: Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s+\S")?,
            table_row: Regex::new(r"^\s*\|.*\|\s*$")?,
            fence: Regex::new(r"^\s*(?:```|~~~)")?,
            link: Regex::new(r"\[[^\]]+\]\([^)]+\)")?,
            word_token: Regex::new(r"[a-zA-Z][a-zA-Z'\-]*")?,
            math_symbol: Regex::new(r"[∑∫√≤≥≠≈∞∂∈∉∀∃⊆⊂∪∩∇±×÷πθλμσΣΠ]|\\[a-zA-Z]+")?,
        })
    }
}

fn patterns() -> Result<&'static Patterns, CoreError> {
    static PATTERNS: OnceLock<Result<Patterns, regex::Error>> = OnceLock::new();
    match PATTERNS.get_or_init(Patterns::compile) {
        Ok(value) => Ok(value),
        Err(error) => Err(CoreError::InvalidPattern(error.to_string())),
    }
}

/// User-tunable trigger words for the lexical features.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lexicon {
    reasoning_terms: BTreeSet<String>,
    constraint_terms: BTreeSet<String>,
}

impl Lexicon {
    /// Construct a lexicon from already-normalized or mixed-case terms.
    #[must_use]
    pub fn new<I, J, S, T>(reasoning_terms: I, constraint_terms: J) -> Self
    where
        I: IntoIterator<Item = S>,
        J: IntoIterator<Item = T>,
        S: AsRef<str>,
        T: AsRef<str>,
    {
        Self {
            reasoning_terms: reasoning_terms
                .into_iter()
                .map(|term| term.as_ref().trim().to_ascii_lowercase())
                .collect(),
            constraint_terms: constraint_terms
                .into_iter()
                .map(|term| term.as_ref().trim().to_ascii_lowercase())
                .collect(),
        }
    }

    /// Reasoning trigger terms in deterministic order.
    pub fn reasoning_terms(&self) -> impl Iterator<Item = &str> {
        self.reasoning_terms.iter().map(String::as_str)
    }

    /// Constraint trigger terms in deterministic order.
    pub fn constraint_terms(&self) -> impl Iterator<Item = &str> {
        self.constraint_terms.iter().map(String::as_str)
    }

    fn is_reasoning_term(&self, token: &str) -> bool {
        self.reasoning_terms.contains(token)
    }

    fn is_constraint_term(&self, token: &str) -> bool {
        self.constraint_terms.contains(token)
    }
}

impl Default for Lexicon {
    fn default() -> Self {
        Self::new(REASONING_TERMS, CONSTRAINT_TERMS)
    }
}

/// Raw feature values in the stable Python report order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Features {
    /// Whitespace-separated words in the frontmatter-free body.
    pub word_count: u64,
    /// Markdown ATX headings outside fenced code.
    pub heading_count: u64,
    /// Deepest ATX heading outside fenced code.
    pub max_heading_depth: u64,
    /// Markdown list items outside fenced code.
    pub list_item_count: u64,
    /// Markdown links outside fenced code.
    pub link_count: u64,
    /// Opening fenced-code markers.
    pub code_block_count: u64,
    /// Pipe-delimited table rows outside fenced code.
    pub table_row_count: u64,
    /// Lexicon reasoning tokens in the whole body.
    pub reasoning_term_count: u64,
    /// Unicode math glyphs and LaTeX-like commands in the whole body.
    pub math_symbol_count: u64,
    /// Lexicon constraint tokens in the whole body.
    pub constraint_term_count: u64,
    /// Literal question marks in the whole body.
    pub question_count: u64,
}

impl Features {
    /// Feature value by stable index.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<u64> {
        match index {
            0 => Some(self.word_count),
            1 => Some(self.heading_count),
            2 => Some(self.max_heading_depth),
            3 => Some(self.list_item_count),
            4 => Some(self.link_count),
            5 => Some(self.code_block_count),
            6 => Some(self.table_row_count),
            7 => Some(self.reasoning_term_count),
            8 => Some(self.math_symbol_count),
            9 => Some(self.constraint_term_count),
            10 => Some(self.question_count),
            _ => None,
        }
    }

    /// Feature value by compatibility name.
    #[must_use]
    pub fn get_named(&self, name: &str) -> Option<u64> {
        feature_index(name).and_then(|index| self.get(index))
    }
}

/// Scalar-score feature weights in stable feature order.
#[derive(Clone, Debug, PartialEq)]
pub struct Weights {
    values: [f64; 11],
}

impl Weights {
    /// Construct from values aligned with [`FEATURE_ORDER`].
    #[must_use]
    pub const fn from_feature_order(values: [f64; 11]) -> Self {
        Self { values }
    }

    /// Values aligned with [`FEATURE_ORDER`].
    #[must_use]
    pub const fn as_feature_order(&self) -> &[f64; 11] {
        &self.values
    }

    /// Read a weight by compatibility name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<f64> {
        feature_index(name).and_then(|index| self.values.get(index).copied())
    }

    /// Replace a weight by compatibility name. Returns `false` for an unknown feature.
    pub fn set(&mut self, name: &str, value: f64) -> bool {
        let Some(index) = feature_index(name) else {
            return false;
        };
        let Some(slot) = self.values.get_mut(index) else {
            return false;
        };
        *slot = value;
        true
    }

    fn at(&self, index: usize) -> f64 {
        self.values.get(index).copied().unwrap_or(0.0)
    }
}

impl Default for Weights {
    fn default() -> Self {
        Self::from_feature_order([3.0, 1.5, 1.0, 2.0, 1.0, 1.5, 1.0, 0.0, 0.0, 0.0, 0.0])
    }
}

/// One ordered score band.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tier {
    /// Inclusive lower score boundary.
    pub min_score: f64,
    /// Routed model name.
    pub model: String,
    /// Optional informational cost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

impl Tier {
    /// Construct a tier without cost metadata.
    #[must_use]
    pub fn new(min_score: f64, model: impl Into<String>) -> Self {
        Self {
            min_score,
            model: model.into(),
            cost: None,
        }
    }

    /// Attach informational cost metadata.
    #[must_use]
    pub const fn with_cost(mut self, cost: f64) -> Self {
        self.cost = Some(cost);
        self
    }
}

/// Fitted multinomial linear classifier over normalized core features.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassifierModel {
    models: Vec<String>,
    weights: Vec<Vec<f64>>,
    intercepts: Vec<f64>,
}

impl ClassifierModel {
    /// Validate and construct a classifier. `weights` maps compatibility feature
    /// names to vectors aligned with `models`; missing features become zero vectors.
    pub fn new(
        models: Vec<String>,
        weights: BTreeMap<String, Vec<f64>>,
        intercepts: Vec<f64>,
    ) -> Result<Self, CoreError> {
        if models.len() < 2 {
            return Err(CoreError::InvalidClassifier(
                "models must contain at least two entries".to_owned(),
            ));
        }
        if models.iter().any(String::is_empty) {
            return Err(CoreError::InvalidClassifier(
                "model names must be non-empty".to_owned(),
            ));
        }
        let unique: BTreeSet<&str> = models.iter().map(String::as_str).collect();
        if unique.len() != models.len() {
            return Err(CoreError::InvalidClassifier(
                "model names must be unique".to_owned(),
            ));
        }
        if intercepts.len() != models.len() {
            return Err(CoreError::InvalidClassifier(format!(
                "intercepts must contain {} values",
                models.len()
            )));
        }

        let mut normalized_weights = vec![vec![0.0; models.len()]; FEATURE_ORDER.len()];
        for (name, vector) in weights {
            let Some(index) = feature_index(&name) else {
                return Err(CoreError::InvalidClassifier(format!(
                    "unknown feature {name:?}"
                )));
            };
            if vector.len() != models.len() {
                return Err(CoreError::InvalidClassifier(format!(
                    "weight vector for {name:?} must contain {} values",
                    models.len()
                )));
            }
            if let Some(slot) = normalized_weights.get_mut(index) {
                *slot = vector;
            }
        }

        Ok(Self {
            models,
            weights: normalized_weights,
            intercepts,
        })
    }

    /// Configured models in stable order.
    #[must_use]
    pub fn models(&self) -> &[String] {
        &self.models
    }

    /// Intercepts aligned with [`Self::models`].
    #[must_use]
    pub fn intercepts(&self) -> &[f64] {
        &self.intercepts
    }

    /// Weight vector for a compatibility feature name.
    #[must_use]
    pub fn weights_for(&self, name: &str) -> Option<&[f64]> {
        feature_index(name)
            .and_then(|index| self.weights.get(index))
            .map(Vec::as_slice)
    }

    /// Linear logits in stable model order.
    #[must_use]
    pub fn logits(&self, features: &Features) -> Vec<f64> {
        let normalized = normalized_features(features);
        self.intercepts
            .iter()
            .enumerate()
            .map(|(model_index, intercept)| {
                let mut value = *intercept;
                for (feature_index, normalized_value) in normalized.iter().copied().enumerate() {
                    let weight = self
                        .weights
                        .get(feature_index)
                        .and_then(|row| row.get(model_index))
                        .copied()
                        .unwrap_or(0.0);
                    value += weight * normalized_value;
                }
                value
            })
            .collect()
    }

    /// Predict with a stable first-index tie break.
    #[must_use]
    pub fn predict(&self, features: &Features) -> Option<&str> {
        let logits = self.logits(features);
        let mut best_index = 0_usize;
        let mut best_value = *logits.first()?;
        for (index, value) in logits.iter().copied().enumerate().skip(1) {
            if value > best_value {
                best_index = index;
                best_value = value;
            }
        }
        self.models.get(best_index).map(String::as_str)
    }
}

/// Complete deterministic routing configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutingConfig {
    /// Scalar-score weights.
    pub weights: Weights,
    /// Ordered tiers used when `classifier` is absent.
    pub tiers: Vec<Tier>,
    /// Optional classifier, which takes precedence over tiers.
    pub classifier: Option<ClassifierModel>,
    /// Lexical trigger words.
    pub lexicon: Lexicon,
}

impl RoutingConfig {
    /// Binary local/cloud routing at an inclusive threshold.
    #[must_use]
    pub fn binary(threshold: f64) -> Self {
        Self {
            tiers: binary_tiers(threshold),
            ..Self::default()
        }
    }
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            weights: Weights::default(),
            tiers: binary_tiers(DEFAULT_THRESHOLD),
            classifier: None,
            lexicon: Lexicon::default(),
        }
    }
}

/// JSON routing mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutingMode {
    /// Ordered score tiers.
    Tiered,
    /// Fitted multinomial classifier.
    Classifier,
}

/// Stable schema-version-3 routing decision.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ComplexityScore {
    /// Wire schema version. It is intentionally a string for Python parity.
    pub schema_version: &'static str,
    /// Rounded structural score.
    pub score: f64,
    /// Selected configured model.
    pub recommendation: String,
    /// Active routing mode.
    pub mode: RoutingMode,
    /// Raw features.
    pub features: Features,
    /// Tier boundary in tiered mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<Tier>>,
    /// Candidate models in classifier mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
}

/// One feature's contribution to the scalar score.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FeatureContribution {
    /// Compatibility feature name.
    pub name: &'static str,
    /// Raw feature count.
    pub value: u64,
    /// Saturated normalized value, rounded to four decimals.
    pub normalized: f64,
    /// Configured feature weight.
    pub weight: f64,
    /// Share of the unrounded scalar score, rounded to four decimals.
    pub contribution: f64,
}

/// Two-tier local/cloud ladder at `threshold`.
#[must_use]
pub fn binary_tiers(threshold: f64) -> Vec<Tier> {
    vec![Tier::new(0.0, "local"), Tier::new(threshold, "cloud")]
}

/// Strip a leading YAML-style frontmatter block with Python's exact split-by-`\n` semantics.
#[must_use]
pub fn strip_frontmatter(text: &str) -> &str {
    let first_end = text.find('\n').unwrap_or(text.len());
    let Some(first_line) = text.get(..first_end) else {
        return text;
    };
    if first_line.trim() != "---" {
        return text;
    }
    if first_end == text.len() {
        return text;
    }

    let mut start = first_end.saturating_add(1);
    while start <= text.len() {
        let Some(rest) = text.get(start..) else {
            return text;
        };
        let relative_end = rest.find('\n');
        let end = relative_end.map_or(text.len(), |offset| start.saturating_add(offset));
        let Some(line) = text.get(start..end) else {
            return text;
        };
        if matches!(line.trim(), "---" | "...") {
            if end == text.len() {
                return "";
            }
            return text.get(end.saturating_add(1)..).unwrap_or("");
        }
        let Some(_) = relative_end else {
            break;
        };
        start = end.saturating_add(1);
    }
    text
}

/// Extract raw structural and lexical features with the built-in lexicon.
pub fn extract_features(text: &str) -> Result<Features, CoreError> {
    extract_features_with_lexicon(text, &Lexicon::default())
}

/// Extract raw structural and lexical features with a custom lexicon.
pub fn extract_features_with_lexicon(text: &str, lexicon: &Lexicon) -> Result<Features, CoreError> {
    let patterns = patterns()?;
    let body = strip_frontmatter(text);
    let mut features = Features {
        word_count: python_word_count(body),
        ..Features::default()
    };

    let mut in_fence = false;
    for line in python_splitlines(body) {
        if patterns.fence.is_match(line) {
            if !in_fence {
                features.code_block_count = features.code_block_count.saturating_add(1);
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(captures) = patterns.heading.captures(line) {
            features.heading_count = features.heading_count.saturating_add(1);
            let depth = captures
                .get(1)
                .map(|matched| u64::try_from(matched.as_str().len()).unwrap_or(u64::MAX))
                .unwrap_or(0);
            features.max_heading_depth = features.max_heading_depth.max(depth);
        } else if patterns.list.is_match(line) {
            features.list_item_count = features.list_item_count.saturating_add(1);
        } else if patterns.table_row.is_match(line) {
            features.table_row_count = features.table_row_count.saturating_add(1);
        }
        features.link_count = features.link_count.saturating_add(
            u64::try_from(patterns.link.find_iter(line).count()).unwrap_or(u64::MAX),
        );
    }

    let lowered = body.to_ascii_lowercase();
    for token_match in patterns.word_token.find_iter(&lowered) {
        let token = token_match.as_str();
        if lexicon.is_reasoning_term(token) {
            features.reasoning_term_count = features.reasoning_term_count.saturating_add(1);
        }
        if lexicon.is_constraint_term(token) {
            features.constraint_term_count = features.constraint_term_count.saturating_add(1);
        }
    }
    features.math_symbol_count =
        u64::try_from(patterns.math_symbol.find_iter(body).count()).unwrap_or(u64::MAX);
    features.question_count =
        u64::try_from(body.as_bytes().iter().filter(|byte| **byte == b'?').count())
            .unwrap_or(u64::MAX);
    Ok(features)
}

/// Saturate each raw feature into `0.0..=1.0` in [`FEATURE_ORDER`].
#[must_use]
pub fn normalized_features(features: &Features) -> [f64; 11] {
    let mut output = [0.0; 11];
    for (index, saturation) in SATURATION.iter().copied().enumerate() {
        let raw = features.get(index).unwrap_or(0) as f64;
        if let Some(slot) = output.get_mut(index) {
            *slot = (raw / saturation).min(1.0);
        }
    }
    output
}

/// Compute Python-compatible rounded scalar score.
#[must_use]
pub fn scalar_score(features: &Features, weights: &Weights) -> f64 {
    let normalized = normalized_features(features);
    let mut total_weight = 0.0;
    for index in WEIGHT_SUM_ORDER {
        total_weight += weights.at(index);
    }
    if total_weight == 0.0 {
        return 0.0;
    }
    let mut accumulated = 0.0;
    for (index, value) in normalized.iter().copied().enumerate() {
        accumulated += weights.at(index) * value;
    }
    python_round(accumulated / total_weight, 2)
}

/// Select the highest inclusive tier reached by `score`.
#[must_use]
pub fn recommend_tier(score: f64, tiers: &[Tier]) -> Option<&str> {
    let mut chosen = tiers.first()?.model.as_str();
    for tier in tiers {
        if score >= tier.min_score {
            chosen = tier.model.as_str();
        } else {
            break;
        }
    }
    Some(chosen)
}

/// Score text and return the stable schema-version-3 decision.
pub fn score_complexity(text: &str, config: &RoutingConfig) -> Result<ComplexityScore, CoreError> {
    let features = extract_features_with_lexicon(text, &config.lexicon)?;
    let score = scalar_score(&features, &config.weights);
    if let Some(classifier) = &config.classifier {
        let recommendation = classifier.predict(&features).ok_or_else(|| {
            CoreError::InvalidClassifier("classifier has no prediction".to_owned())
        })?;
        return Ok(ComplexityScore {
            schema_version: "3",
            score,
            recommendation: recommendation.to_owned(),
            mode: RoutingMode::Classifier,
            features,
            tiers: None,
            models: Some(classifier.models.clone()),
        });
    }
    let recommendation = recommend_tier(score, &config.tiers).ok_or(CoreError::MissingTier)?;
    Ok(ComplexityScore {
        schema_version: "3",
        score,
        recommendation: recommendation.to_owned(),
        mode: RoutingMode::Tiered,
        features,
        tiers: Some(config.tiers.clone()),
        models: None,
    })
}

/// Explain the scalar score one feature at a time.
#[must_use]
pub fn explain_score(features: &Features, weights: &Weights) -> Vec<FeatureContribution> {
    let normalized = normalized_features(features);
    let mut total_weight = 0.0;
    for index in WEIGHT_SUM_ORDER {
        total_weight += weights.at(index);
    }
    FEATURE_ORDER
        .iter()
        .copied()
        .enumerate()
        .map(|(index, name)| {
            let normalized_value = normalized.get(index).copied().unwrap_or(0.0);
            let weight = weights.at(index);
            let contribution = if total_weight == 0.0 {
                0.0
            } else {
                weight * normalized_value / total_weight
            };
            FeatureContribution {
                name,
                value: features.get(index).unwrap_or(0),
                normalized: python_round(normalized_value, 4),
                weight,
                contribution: python_round(contribution, 4),
            }
        })
        .collect()
}

/// Assess one destination against hard compatibility and privacy requirements.
///
/// Eligibility is decided before complexity scoring and reasons remain in a
/// stable order for portable fixtures and user remediation.
#[must_use]
pub fn assess_destination(
    request: &RoutingRequest,
    destination: &DestinationSnapshot,
) -> CandidateAssessment {
    let mut exclusions = Vec::new();
    if destination.readiness != ProviderReadiness::Ready {
        exclusions.push(ExclusionReason::ProviderNotReady);
    }
    if !request
        .privacy_posture
        .permits(destination.execution_boundary)
    {
        exclusions.push(ExclusionReason::PrivacyBoundaryDenied);
    }
    match request.requirements.surface {
        InferenceSurface::Text => {
            if !destination.capabilities.text {
                exclusions.push(ExclusionReason::TextUnsupported);
            }
        }
        InferenceSurface::Embeddings => {
            if !destination.capabilities.modalities.embeddings {
                exclusions.push(ExclusionReason::EmbeddingsUnsupported);
            }
        }
        InferenceSurface::ImageGeneration => {
            if !destination.capabilities.modalities.image_generation {
                exclusions.push(ExclusionReason::ImageGenerationUnsupported);
            }
        }
        InferenceSurface::Audio => {
            if !destination.capabilities.modalities.audio_input
                && !destination.capabilities.modalities.audio_output
            {
                exclusions.push(ExclusionReason::AudioUnsupported);
            }
        }
        InferenceSurface::Batch => {
            if !destination.capabilities.modalities.batch {
                exclusions.push(ExclusionReason::BatchUnsupported);
            }
        }
    }
    if let Some(required) = request.requirements.context_tokens {
        match destination.context_window {
            Some(available) if available < required => {
                exclusions.push(ExclusionReason::ContextWindowTooSmall);
            }
            None => exclusions.push(ExclusionReason::ContextWindowUnknown),
            Some(_) => {}
        }
    }
    if request.requirements.image_input && !destination.capabilities.image_input {
        exclusions.push(ExclusionReason::ImageInputUnsupported);
    }
    if request.requirements.tools && !destination.capabilities.tools {
        exclusions.push(ExclusionReason::ToolsUnsupported);
    }
    if request.requirements.streaming && !destination.capabilities.streaming {
        exclusions.push(ExclusionReason::StreamingUnsupported);
    }
    if !destination.automatic_eligible {
        exclusions.push(ExclusionReason::AutomaticNotAllowed);
    }
    CandidateAssessment {
        destination_id: destination.id.clone(),
        exclusions,
    }
}

/// Plan an Automatic route over secret-free destination snapshots.
///
/// The configured scorer selects a tier. Hard filters run first, then the
/// first eligible destination in that tier wins with remaining matching
/// candidates preserved as stable pre-output fallback order. This function
/// never crosses to a different tier implicitly.
pub fn plan_automatic_route(
    request: &RoutingRequest,
    candidates: &[DestinationSnapshot],
    config: &RoutingConfig,
) -> Result<RoutePlan, CoreError> {
    validate_runtime_contract(request, candidates)?;
    let scored = score_complexity(&request.prompt, config)?;
    let assessments = candidates
        .iter()
        .map(|candidate| assess_destination(request, candidate))
        .collect::<Vec<_>>();
    let eligible_ids = candidates
        .iter()
        .zip(&assessments)
        .filter(|(candidate, assessment)| {
            assessment.is_eligible() && candidate.route_tier == scored.recommendation
        })
        .map(|(candidate, _)| candidate.id.clone())
        .collect::<Vec<_>>();
    let selected_destination_id = eligible_ids.first().cloned();
    let fallback_destination_ids = eligible_ids.into_iter().skip(1).collect();

    Ok(RoutePlan {
        schema_version: RUNTIME_CONTRACT_VERSION,
        request_id: request.request_id.clone(),
        score: scored.score,
        recommendation: scored.recommendation,
        selected_destination_id,
        fallback_destination_ids,
        candidates: assessments,
    })
}

fn validate_runtime_contract(
    request: &RoutingRequest,
    candidates: &[DestinationSnapshot],
) -> Result<(), CoreError> {
    if request.schema_version != RUNTIME_CONTRACT_VERSION {
        return Err(CoreError::InvalidContract(format!(
            "unsupported schema version {}",
            request.schema_version
        )));
    }
    if request.request_id.trim().is_empty() {
        return Err(CoreError::InvalidContract(
            "request_id must be non-empty".to_owned(),
        ));
    }
    let mut ids = BTreeSet::new();
    for candidate in candidates {
        if candidate.id.trim().is_empty()
            || candidate.provider_id.trim().is_empty()
            || candidate.model_id.trim().is_empty()
            || candidate.route_tier.trim().is_empty()
        {
            return Err(CoreError::InvalidContract(
                "destination identity and route_tier must be non-empty".to_owned(),
            ));
        }
        if !ids.insert(candidate.id.as_str()) {
            return Err(CoreError::InvalidContract(format!(
                "duplicate destination id {:?}",
                candidate.id
            )));
        }
    }
    Ok(())
}

/// Python `round(value, places)` behavior for the bounded numeric paths used by
/// routing. It rounds the formatted true binary value half-to-even, matching the
/// parity-proven JavaScript mirror rather than Rust's integer `round` rule.
#[must_use]
pub fn python_round(value: f64, places: usize) -> f64 {
    if !value.is_finite() || places > 12 {
        return value;
    }
    let negative = value.is_sign_negative();
    let rendered = format!("{:.20}", value.abs());
    let Some((integer, decimals)) = rendered.split_once('.') else {
        return value;
    };
    let Some(scale) = 10_u64.checked_pow(u32::try_from(places).unwrap_or(u32::MAX)) else {
        return value;
    };
    let Some(integer_value) = parse_ascii_digits(integer) else {
        return value;
    };
    let mut retained = match integer_value.checked_mul(scale) {
        Some(base) => base,
        None => return value,
    };
    let mut fractional = 0_u64;
    for byte in decimals.as_bytes().iter().copied().take(places) {
        let Some(digit) = byte.checked_sub(b'0').filter(|digit| *digit <= 9) else {
            return value;
        };
        fractional = match fractional
            .checked_mul(10)
            .and_then(|current| current.checked_add(u64::from(digit)))
        {
            Some(next) => next,
            None => return value,
        };
    }
    for _ in decimals.len().min(places)..places {
        fractional = match fractional.checked_mul(10) {
            Some(next) => next,
            None => return value,
        };
    }
    retained = match retained.checked_add(fractional) {
        Some(next) => next,
        None => return value,
    };

    let tail = decimals.as_bytes().get(places..).unwrap_or_default();
    let first_discarded = tail.first().copied().unwrap_or(b'0');
    let should_increment = match first_discarded.cmp(&b'5') {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => {
            tail.get(1..)
                .unwrap_or_default()
                .iter()
                .any(|byte| *byte != b'0')
                || retained % 2 == 1
        }
    };
    if should_increment {
        retained = retained.saturating_add(1);
    }
    let rounded = retained as f64 / scale as f64;
    if negative { -rounded } else { rounded }
}

fn parse_ascii_digits(value: &str) -> Option<u64> {
    let mut output = 0_u64;
    for byte in value.as_bytes() {
        let digit = byte.checked_sub(b'0').filter(|digit| *digit <= 9)?;
        output = output.checked_mul(10)?.checked_add(u64::from(digit))?;
    }
    Some(output)
}

fn feature_index(name: &str) -> Option<usize> {
    FEATURE_ORDER
        .iter()
        .position(|candidate| *candidate == name)
}

fn python_word_count(text: &str) -> u64 {
    let mut in_word = false;
    let mut count = 0_u64;
    for character in text.chars() {
        if python_whitespace(character) {
            in_word = false;
        } else if !in_word {
            count = count.saturating_add(1);
            in_word = true;
        }
    }
    count
}

fn python_whitespace(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\u{1c}'..='\u{1f}')
}

fn python_line_separator(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r'
            | '\u{0b}'
            | '\u{0c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

fn python_splitlines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut output = Vec::new();
    let mut start = 0_usize;
    let mut characters = text.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        if !python_line_separator(character) {
            continue;
        }
        if let Some(line) = text.get(start..index) {
            output.push(line);
        }
        let mut next_start = index.saturating_add(character.len_utf8());
        if character == '\r' {
            if let Some((next_index, '\n')) = characters.peek().copied() {
                let _ = characters.next();
                next_start = next_index.saturating_add(1);
            }
        }
        start = next_start;
    }
    if start < text.len() {
        if let Some(line) = text.get(start..) {
            output.push(line);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scored(text: &str) -> Result<ComplexityScore, CoreError> {
        score_complexity(text, &RoutingConfig::default())
    }

    fn destination(
        id: &str,
        route_tier: &str,
        execution_boundary: ExecutionBoundary,
    ) -> DestinationSnapshot {
        DestinationSnapshot {
            id: id.to_owned(),
            provider_id: format!("provider-{id}"),
            model_id: format!("model-{id}"),
            display_name: id.to_owned(),
            route_tier: route_tier.to_owned(),
            execution_boundary,
            readiness: ProviderReadiness::Ready,
            billing_class: BillingClass::Unknown,
            context_window: Some(8_192),
            capabilities: DestinationCapabilities {
                text: true,
                streaming: true,
                image_input: false,
                tools: false,
                modalities: ModalityCapabilities::default(),
            },
            automatic_eligible: true,
        }
    }

    fn request(privacy_posture: PrivacyPosture) -> RoutingRequest {
        RoutingRequest {
            schema_version: RUNTIME_CONTRACT_VERSION,
            request_id: "request-1".to_owned(),
            prompt: "hello".to_owned(),
            privacy_posture,
            requirements: RoutingRequirements {
                streaming: true,
                ..RoutingRequirements::default()
            },
        }
    }

    #[test]
    fn modality_capabilities_are_hard_filters_before_scoring() {
        let mut request = request(PrivacyPosture::HostedAllowed);
        request.requirements.surface = InferenceSurface::Embeddings;
        let text_only = destination("text-only", "local", ExecutionBoundary::Hosted);
        let assessment = assess_destination(&request, &text_only);
        assert_eq!(
            assessment.exclusions,
            vec![ExclusionReason::EmbeddingsUnsupported]
        );

        let mut embeddings = destination("embeddings", "local", ExecutionBoundary::Hosted);
        embeddings.capabilities.modalities.embeddings = true;
        assert!(assess_destination(&request, &embeddings).is_eligible());
    }

    #[test]
    fn non_text_surface_contracts_round_trip_with_defaults() -> Result<(), CoreError> {
        let mut request = request(PrivacyPosture::HostedAllowed);
        request.requirements.surface = InferenceSurface::Batch;
        let encoded = serde_json::to_string(&request)
            .map_err(|_| CoreError::InvalidContract("encode".to_owned()))?;
        let decoded: RoutingRequest = serde_json::from_str(&encoded)
            .map_err(|_| CoreError::InvalidContract("decode".to_owned()))?;
        assert_eq!(decoded.requirements.surface, InferenceSurface::Batch);
        Ok(())
    }

    #[test]
    fn python_round_matches_binary_half_even_traps() {
        assert_eq!(python_round(0.005, 2), 0.01);
        assert_eq!(python_round(2.675, 2), 2.67);
        assert_eq!(python_round(0.125, 2), 0.12);
        assert_eq!(python_round(0.135, 2), 0.14);
        assert_eq!(python_round(-0.125, 2), -0.12);
    }

    #[test]
    fn frontmatter_contract_matches_python() {
        let body = "# Task\n\nDo the thing.";
        let wrapped = format!("---\nschema_version: 1\n...\n{body}");
        assert_eq!(strip_frontmatter(&wrapped), body);
        let unterminated = "---\nstill going\n";
        assert_eq!(strip_frontmatter(unterminated), unterminated);
        assert_eq!(strip_frontmatter(" \n---\nbody"), " \n---\nbody");
    }

    #[test]
    fn code_fence_contents_do_not_count_as_structure() -> Result<(), CoreError> {
        let features = extract_features("```\n## no\n- no\n| no |\n[no](x)\n```\n")?;
        assert_eq!(features.heading_count, 0);
        assert_eq!(features.list_item_count, 0);
        assert_eq!(features.table_row_count, 0);
        assert_eq!(features.link_count, 0);
        assert_eq!(features.code_block_count, 1);
        Ok(())
    }

    #[test]
    fn python_splitlines_recognizes_all_documented_boundaries() -> Result<(), CoreError> {
        let text = "# a\r# b\r\n# c\u{0b}# d\u{0c}# e\u{1c}# f\u{1d}# g\u{1e}# h\u{85}# i\u{2028}# j\u{2029}# k";
        let features = extract_features(text)?;
        assert_eq!(features.heading_count, 11);
        Ok(())
    }

    #[test]
    fn lexical_terms_are_whole_word_and_case_insensitive() -> Result<(), CoreError> {
        let features = extract_features("PROVE the theorem; approve the proverbial change")?;
        assert_eq!(features.reasoning_term_count, 2);
        Ok(())
    }

    #[test]
    fn default_golden_cases_match_python() -> Result<(), CoreError> {
        let headings = scored("# Plan\n\n## Steps\n- one\n- two\n- three\n1. first\n2. second")?;
        assert_eq!(headings.score, 0.15);
        assert_eq!(headings.recommendation, "cloud");
        assert_eq!(headings.features.heading_count, 2);
        assert_eq!(headings.features.list_item_count, 5);

        let math = scored("show that ∑ x ≤ ∞ and ∫ f dx ≥ 0 using \\alpha and \\beta")?;
        assert_eq!(math.score, 0.01);
        assert_eq!(math.recommendation, "cloud");
        assert_eq!(math.features.math_symbol_count, 7);
        Ok(())
    }

    #[test]
    fn tier_boundaries_are_inclusive() {
        let tiers = vec![
            Tier::new(0.0, "small"),
            Tier::new(0.3, "medium"),
            Tier::new(0.6, "large"),
        ];
        assert_eq!(recommend_tier(0.0, &tiers), Some("small"));
        assert_eq!(recommend_tier(0.3, &tiers), Some("medium"));
        assert_eq!(recommend_tier(0.6, &tiers), Some("large"));
    }

    #[test]
    fn classifier_ties_choose_first_model() -> Result<(), CoreError> {
        let classifier = ClassifierModel::new(
            vec!["first".to_owned(), "second".to_owned()],
            BTreeMap::new(),
            vec![0.0, 0.0],
        )?;
        let config = RoutingConfig {
            classifier: Some(classifier),
            ..RoutingConfig::default()
        };
        let result = score_complexity("hello", &config)?;
        assert_eq!(result.recommendation, "first");
        assert_eq!(result.mode, RoutingMode::Classifier);
        Ok(())
    }

    #[test]
    fn json_contract_is_schema_three_and_omits_inactive_boundary() -> Result<(), CoreError> {
        let result = scored("hello")?;
        let value = serde_json::to_value(result).map_err(|error| {
            CoreError::InvalidClassifier(format!("test serialization failed: {error}"))
        })?;
        assert_eq!(value.get("schema_version"), Some(&serde_json::json!("3")));
        assert!(value.get("tiers").is_some());
        assert!(value.get("models").is_none());
        Ok(())
    }

    #[test]
    fn explanations_keep_feature_order_and_rounding() -> Result<(), CoreError> {
        let features = extract_features("# Heading\n- one\n- two")?;
        let explanation = explain_score(&features, &Weights::default());
        let names: Vec<&str> = explanation.iter().map(|item| item.name).collect();
        assert_eq!(names, FEATURE_ORDER);
        assert_eq!(explanation.len(), 11);
        Ok(())
    }

    #[test]
    fn automatic_plan_filters_before_scoring_and_preserves_fallback_order() -> Result<(), CoreError>
    {
        let hosted = destination("hosted", "local", ExecutionBoundary::Hosted);
        let local_first = destination("local-first", "local", ExecutionBoundary::OnDevice);
        let local_second = destination("local-second", "local", ExecutionBoundary::OnDevice);

        let plan = plan_automatic_route(
            &request(PrivacyPosture::OnDeviceOnly),
            &[hosted, local_first, local_second],
            &RoutingConfig::default(),
        )?;

        assert_eq!(plan.recommendation, "local");
        assert_eq!(plan.selected_destination_id.as_deref(), Some("local-first"));
        assert_eq!(plan.fallback_destination_ids, ["local-second"]);
        assert_eq!(
            plan.candidates[0].exclusions,
            [ExclusionReason::PrivacyBoundaryDenied]
        );
        assert!(plan.candidates[1].is_eligible());
        assert!(plan.candidates[2].is_eligible());
        Ok(())
    }

    #[test]
    fn automatic_plan_does_not_cross_tiers_when_selected_tier_is_unavailable()
    -> Result<(), CoreError> {
        let hosted = destination("hosted", "cloud", ExecutionBoundary::Hosted);
        let plan = plan_automatic_route(
            &request(PrivacyPosture::HostedAllowed),
            &[hosted],
            &RoutingConfig::default(),
        )?;

        assert_eq!(plan.recommendation, "local");
        assert_eq!(plan.selected_destination_id, None);
        assert!(plan.fallback_destination_ids.is_empty());
        assert!(plan.candidates[0].is_eligible());
        Ok(())
    }

    #[test]
    fn candidate_assessment_fails_closed_for_unknown_context() {
        let mut candidate = destination("local", "local", ExecutionBoundary::OnDevice);
        candidate.context_window = None;
        let mut input = request(PrivacyPosture::OnDeviceOnly);
        input.requirements.context_tokens = Some(1_024);

        assert_eq!(
            assess_destination(&input, &candidate).exclusions,
            [ExclusionReason::ContextWindowUnknown]
        );
    }

    #[test]
    fn automatic_plan_rejects_duplicate_destination_ids() {
        let candidate = destination("same", "local", ExecutionBoundary::OnDevice);
        let result = plan_automatic_route(
            &request(PrivacyPosture::OnDeviceOnly),
            &[candidate.clone(), candidate],
            &RoutingConfig::default(),
        );

        assert!(matches!(result, Err(CoreError::InvalidContract(_))));
    }
}
