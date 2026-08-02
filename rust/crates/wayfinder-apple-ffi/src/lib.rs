//! Generated Swift bridge for Wayfinder's pure deterministic routing core.
//!
//! The bridge owns only bounded type conversion and error sanitization. It
//! performs no provider execution, networking, persistence, credential access,
//! UI work, or Apple-framework calls.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::sync::Arc;

use thiserror::Error;
use wayfinder_routing_core::{
    BillingClass as CoreBillingClass, ComplexityScore as CoreComplexityScore,
    DestinationCapabilities as CoreDestinationCapabilities,
    DestinationSnapshot as CoreDestinationSnapshot, ExclusionReason as CoreExclusionReason,
    ExecutionBoundary as CoreExecutionBoundary, Features as CoreFeatures,
    PrivacyPosture as CorePrivacyPosture, ProviderReadiness as CoreProviderReadiness,
    RUNTIME_CONTRACT_VERSION, RoutePlan as CoreRoutePlan, RoutingConfig as CoreRoutingConfig,
    RoutingMode as CoreRoutingMode, RoutingRequest as CoreRoutingRequest,
    RoutingRequirements as CoreRoutingRequirements, Tier as CoreTier, plan_automatic_route,
    score_complexity,
};

uniffi::setup_scaffolding!();

/// Stable ABI version for host compatibility checks.
pub const BRIDGE_ABI_VERSION: u32 = 1;

const MAX_PROMPT_BYTES: usize = 256 * 1024;
const MAX_REQUEST_ID_BYTES: usize = 256;
const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_DISPLAY_NAME_BYTES: usize = 512;
const MAX_CANDIDATES: usize = 256;
const MAX_TIERS: usize = 32;

/// Sanitized failures exposed to Swift.
#[derive(Debug, Error, uniffi::Error)]
pub enum RoutingBridgeError {
    /// The host supplied an invalid routing configuration.
    #[error("invalid routing configuration: {0}")]
    InvalidConfiguration(String),
    /// The host supplied an invalid or oversized request.
    #[error("invalid routing request: {0}")]
    InvalidRequest(String),
    /// The authoritative routing core rejected a bounded request.
    #[error("routing failed: {0}")]
    RoutingFailed(String),
}

/// One score threshold and its destination tier name.
#[derive(Clone, Debug, uniffi::Record)]
pub struct RoutingTier {
    /// Inclusive score threshold.
    pub min_score: f64,
    /// Stable tier name matched by destination snapshots.
    pub model: String,
}

/// Immutable configuration held by a routing engine.
#[derive(Clone, Debug, uniffi::Record)]
pub struct RoutingConfiguration {
    /// Ordered inclusive tiers, beginning at `0.0`.
    pub tiers: Vec<RoutingTier>,
}

/// Where prompt content executes.
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum ExecutionBoundary {
    /// The current Apple device executes the request.
    OnDevice,
    /// A trusted local-network host executes the request.
    LocalNetwork,
    /// A hosted provider executes the request.
    Hosted,
}

/// Maximum execution boundary selected by the user.
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum PrivacyPosture {
    /// Content may remain only on the current device.
    OnDeviceOnly,
    /// Current-device and trusted local-network execution are permitted.
    LocalDevices,
    /// Hosted execution is also permitted.
    HostedAllowed,
}

/// Normalized provider readiness before routing.
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum ProviderReadiness {
    Checking,
    SignedOut,
    Authorizing,
    Ready,
    ReauthenticationRequired,
    UsageLimited,
    ModelUnavailable,
    NetworkUnavailable,
    UnsupportedPlatform,
    Unavailable,
    Failed,
}

/// Provider accounting semantics.
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum BillingClass {
    OnDevice,
    Subscription,
    ApiMetered,
    Unknown,
}

/// Capabilities relevant to hard eligibility filtering.
#[derive(Clone, Debug, uniffi::Record)]
pub struct DestinationCapabilities {
    pub text: bool,
    pub streaming: bool,
    pub image_input: bool,
    pub tools: bool,
    pub embeddings: bool,
    pub image_generation: bool,
    pub audio_input: bool,
    pub audio_output: bool,
    pub batch: bool,
}

/// Requirements that must be satisfied before complexity scoring.
#[derive(Clone, Debug, uniffi::Record)]
pub struct RoutingRequirements {
    pub context_tokens: Option<u64>,
    pub image_input: bool,
    pub tools: bool,
    pub streaming: bool,
    pub surface: InferenceSurface,
}

/// Provider execution surface required by a mobile request.
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum InferenceSurface {
    Text,
    Embeddings,
    ImageGeneration,
    Audio,
    Batch,
}

/// Bounded request supplied by an Apple host.
#[derive(Clone, Debug, uniffi::Record)]
pub struct RoutingRequest {
    pub schema_version: u32,
    pub request_id: String,
    pub prompt: String,
    pub privacy_posture: PrivacyPosture,
    pub requirements: RoutingRequirements,
}

/// Secret-free snapshot of one concrete destination.
#[derive(Clone, Debug, uniffi::Record)]
pub struct DestinationSnapshot {
    pub id: String,
    pub provider_id: String,
    pub model_id: String,
    pub display_name: String,
    pub route_tier: String,
    pub execution_boundary: ExecutionBoundary,
    pub readiness: ProviderReadiness,
    pub billing_class: BillingClass,
    pub context_window: Option<u64>,
    pub capabilities: DestinationCapabilities,
    pub automatic_eligible: bool,
}

/// Stable reason a destination was excluded before scoring.
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum ExclusionReason {
    ProviderNotReady,
    PrivacyBoundaryDenied,
    TextUnsupported,
    ContextWindowUnknown,
    ContextWindowTooSmall,
    ImageInputUnsupported,
    ToolsUnsupported,
    StreamingUnsupported,
    EmbeddingsUnsupported,
    ImageGenerationUnsupported,
    AudioUnsupported,
    BatchUnsupported,
    AutomaticNotAllowed,
}

/// Eligibility result for one input destination.
#[derive(Clone, Debug, uniffi::Record)]
pub struct CandidateAssessment {
    pub destination_id: String,
    pub exclusions: Vec<ExclusionReason>,
}

/// Stable scoring mode used by the authoritative core.
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum ScoringMode {
    Tiered,
    Classifier,
}

/// Stable feature vector returned for parity and route explanations.
#[derive(Clone, Debug, uniffi::Record)]
pub struct FeatureVector {
    pub word_count: u64,
    pub heading_count: u64,
    pub max_heading_depth: u64,
    pub list_item_count: u64,
    pub link_count: u64,
    pub code_block_count: u64,
    pub table_row_count: u64,
    pub reasoning_term_count: u64,
    pub math_symbol_count: u64,
    pub constraint_term_count: u64,
    pub question_count: u64,
}

/// Buffered deterministic score returned to Swift.
#[derive(Clone, Debug, uniffi::Record)]
pub struct ComplexityResult {
    pub schema_version: String,
    pub score: f64,
    pub recommendation: String,
    pub mode: ScoringMode,
    pub features: FeatureVector,
}

/// Deterministic route plan returned to Swift.
#[derive(Clone, Debug, uniffi::Record)]
pub struct RoutePlan {
    pub schema_version: u32,
    pub request_id: String,
    pub score: f64,
    pub recommendation: String,
    pub selected_destination_id: Option<String>,
    pub fallback_destination_ids: Vec<String>,
    pub candidates: Vec<CandidateAssessment>,
}

/// Immutable, thread-safe handle to the authoritative routing core.
#[derive(uniffi::Object)]
pub struct RoutingEngine {
    config: CoreRoutingConfig,
}

#[uniffi::export]
impl RoutingEngine {
    /// Validate and retain a typed routing configuration.
    #[uniffi::constructor]
    pub fn new(configuration: RoutingConfiguration) -> Result<Arc<Self>, RoutingBridgeError> {
        Ok(Arc::new(Self {
            config: core_configuration(configuration)?,
        }))
    }

    /// Return the runtime-contract schema supported by this bridge.
    #[must_use]
    pub fn runtime_contract_version(&self) -> u32 {
        RUNTIME_CONTRACT_VERSION
    }

    /// Score one bounded prompt without a provider or model call.
    pub fn score(&self, prompt: String) -> Result<ComplexityResult, RoutingBridgeError> {
        validate_prompt(&prompt)?;
        score_complexity(&prompt, &self.config)
            .map(ComplexityResult::from)
            .map_err(core_error)
    }

    /// Filter, score, and plan one Automatic route.
    pub fn plan(
        &self,
        request: RoutingRequest,
        candidates: Vec<DestinationSnapshot>,
    ) -> Result<RoutePlan, RoutingBridgeError> {
        validate_request(&request, &candidates)?;
        let request = CoreRoutingRequest::from(request);
        let candidates = candidates
            .into_iter()
            .map(CoreDestinationSnapshot::from)
            .collect::<Vec<_>>();
        plan_automatic_route(&request, &candidates, &self.config)
            .map(RoutePlan::from)
            .map_err(core_error)
    }
}

/// Return the generated bridge ABI version without allocating an engine.
#[uniffi::export]
#[must_use]
pub fn bridge_abi_version() -> u32 {
    BRIDGE_ABI_VERSION
}

fn core_configuration(
    configuration: RoutingConfiguration,
) -> Result<CoreRoutingConfig, RoutingBridgeError> {
    if configuration.tiers.is_empty() {
        return Err(invalid_configuration("at least one tier is required"));
    }
    if configuration.tiers.len() > MAX_TIERS {
        return Err(invalid_configuration("too many tiers"));
    }
    if configuration
        .tiers
        .first()
        .is_none_or(|tier| tier.min_score != 0.0)
    {
        return Err(invalid_configuration("the first tier must begin at 0.0"));
    }

    let mut previous = None;
    let mut names = BTreeSet::new();
    let mut tiers = Vec::with_capacity(configuration.tiers.len());
    for tier in configuration.tiers {
        if !tier.min_score.is_finite() || !(0.0..=1.0).contains(&tier.min_score) {
            return Err(invalid_configuration(
                "tier thresholds must be finite values from 0.0 through 1.0",
            ));
        }
        if previous.is_some_and(|value| tier.min_score <= value) {
            return Err(invalid_configuration(
                "tier thresholds must be strictly increasing",
            ));
        }
        validate_identifier(&tier.model)
            .map_err(|_| invalid_configuration("tier names must be non-empty and bounded"))?;
        if !names.insert(tier.model.clone()) {
            return Err(invalid_configuration("tier names must be unique"));
        }
        previous = Some(tier.min_score);
        tiers.push(CoreTier::new(tier.min_score, tier.model));
    }

    Ok(CoreRoutingConfig {
        tiers,
        ..CoreRoutingConfig::default()
    })
}

fn validate_request(
    request: &RoutingRequest,
    candidates: &[DestinationSnapshot],
) -> Result<(), RoutingBridgeError> {
    validate_prompt(&request.prompt)?;
    validate_bounded_nonempty(
        &request.request_id,
        MAX_REQUEST_ID_BYTES,
        "request_id must be non-empty and bounded",
    )?;
    if request.schema_version != RUNTIME_CONTRACT_VERSION {
        return Err(invalid_request("unsupported runtime-contract version"));
    }
    if candidates.len() > MAX_CANDIDATES {
        return Err(invalid_request("too many destination candidates"));
    }
    for candidate in candidates {
        validate_identifier(&candidate.id)
            .and_then(|()| validate_identifier(&candidate.provider_id))
            .and_then(|()| validate_identifier(&candidate.model_id))
            .and_then(|()| validate_identifier(&candidate.route_tier))
            .map_err(|_| {
                invalid_request("destination identifiers must be non-empty and bounded")
            })?;
        validate_bounded_nonempty(
            &candidate.display_name,
            MAX_DISPLAY_NAME_BYTES,
            "destination display names must be non-empty and bounded",
        )?;
    }
    Ok(())
}

fn validate_prompt(prompt: &str) -> Result<(), RoutingBridgeError> {
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err(invalid_request("prompt exceeds the bridge limit"));
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), RoutingBridgeError> {
    validate_bounded_nonempty(
        value,
        MAX_IDENTIFIER_BYTES,
        "identifier must be non-empty and bounded",
    )
}

fn validate_bounded_nonempty(
    value: &str,
    limit: usize,
    reason: &'static str,
) -> Result<(), RoutingBridgeError> {
    if value.trim().is_empty() || value.len() > limit {
        return Err(invalid_request(reason));
    }
    Ok(())
}

fn invalid_configuration(reason: &'static str) -> RoutingBridgeError {
    RoutingBridgeError::InvalidConfiguration(reason.to_owned())
}

fn invalid_request(reason: &'static str) -> RoutingBridgeError {
    RoutingBridgeError::InvalidRequest(reason.to_owned())
}

fn core_error(error: wayfinder_routing_core::CoreError) -> RoutingBridgeError {
    let reason = match error {
        wayfinder_routing_core::CoreError::MissingTier => "routing configuration has no tier",
        wayfinder_routing_core::CoreError::InvalidPattern(_) => {
            "built-in routing feature initialization failed"
        }
        wayfinder_routing_core::CoreError::InvalidClassifier(_) => {
            "routing classifier configuration is invalid"
        }
        wayfinder_routing_core::CoreError::InvalidContract(_) => {
            "runtime routing contract is invalid"
        }
    };
    RoutingBridgeError::RoutingFailed(reason.to_owned())
}

impl From<ExecutionBoundary> for CoreExecutionBoundary {
    fn from(value: ExecutionBoundary) -> Self {
        match value {
            ExecutionBoundary::OnDevice => Self::OnDevice,
            ExecutionBoundary::LocalNetwork => Self::LocalNetwork,
            ExecutionBoundary::Hosted => Self::Hosted,
        }
    }
}

impl From<PrivacyPosture> for CorePrivacyPosture {
    fn from(value: PrivacyPosture) -> Self {
        match value {
            PrivacyPosture::OnDeviceOnly => Self::OnDeviceOnly,
            PrivacyPosture::LocalDevices => Self::LocalDevices,
            PrivacyPosture::HostedAllowed => Self::HostedAllowed,
        }
    }
}

impl From<ProviderReadiness> for CoreProviderReadiness {
    fn from(value: ProviderReadiness) -> Self {
        match value {
            ProviderReadiness::Checking => Self::Checking,
            ProviderReadiness::SignedOut => Self::SignedOut,
            ProviderReadiness::Authorizing => Self::Authorizing,
            ProviderReadiness::Ready => Self::Ready,
            ProviderReadiness::ReauthenticationRequired => Self::ReauthenticationRequired,
            ProviderReadiness::UsageLimited => Self::UsageLimited,
            ProviderReadiness::ModelUnavailable => Self::ModelUnavailable,
            ProviderReadiness::NetworkUnavailable => Self::NetworkUnavailable,
            ProviderReadiness::UnsupportedPlatform => Self::UnsupportedPlatform,
            ProviderReadiness::Unavailable => Self::Unavailable,
            ProviderReadiness::Failed => Self::Failed,
        }
    }
}

impl From<BillingClass> for CoreBillingClass {
    fn from(value: BillingClass) -> Self {
        match value {
            BillingClass::OnDevice => Self::OnDevice,
            BillingClass::Subscription => Self::Subscription,
            BillingClass::ApiMetered => Self::ApiMetered,
            BillingClass::Unknown => Self::Unknown,
        }
    }
}

impl From<RoutingRequirements> for CoreRoutingRequirements {
    fn from(value: RoutingRequirements) -> Self {
        Self {
            context_tokens: value.context_tokens,
            image_input: value.image_input,
            tools: value.tools,
            streaming: value.streaming,
            surface: value.surface.into(),
        }
    }
}

impl From<InferenceSurface> for wayfinder_routing_core::InferenceSurface {
    fn from(value: InferenceSurface) -> Self {
        match value {
            InferenceSurface::Text => Self::Text,
            InferenceSurface::Embeddings => Self::Embeddings,
            InferenceSurface::ImageGeneration => Self::ImageGeneration,
            InferenceSurface::Audio => Self::Audio,
            InferenceSurface::Batch => Self::Batch,
        }
    }
}

impl From<RoutingRequest> for CoreRoutingRequest {
    fn from(value: RoutingRequest) -> Self {
        Self {
            schema_version: value.schema_version,
            request_id: value.request_id,
            prompt: value.prompt,
            privacy_posture: value.privacy_posture.into(),
            requirements: value.requirements.into(),
        }
    }
}

impl From<DestinationCapabilities> for CoreDestinationCapabilities {
    fn from(value: DestinationCapabilities) -> Self {
        Self {
            text: value.text,
            streaming: value.streaming,
            image_input: value.image_input,
            tools: value.tools,
            modalities: wayfinder_routing_core::ModalityCapabilities {
                embeddings: value.embeddings,
                image_generation: value.image_generation,
                audio_input: value.audio_input,
                audio_output: value.audio_output,
                batch: value.batch,
            },
        }
    }
}

impl From<DestinationSnapshot> for CoreDestinationSnapshot {
    fn from(value: DestinationSnapshot) -> Self {
        Self {
            id: value.id,
            provider_id: value.provider_id,
            model_id: value.model_id,
            display_name: value.display_name,
            route_tier: value.route_tier,
            execution_boundary: value.execution_boundary.into(),
            readiness: value.readiness.into(),
            billing_class: value.billing_class.into(),
            context_window: value.context_window,
            capabilities: value.capabilities.into(),
            automatic_eligible: value.automatic_eligible,
        }
    }
}

impl From<CoreFeatures> for FeatureVector {
    fn from(value: CoreFeatures) -> Self {
        Self {
            word_count: value.word_count,
            heading_count: value.heading_count,
            max_heading_depth: value.max_heading_depth,
            list_item_count: value.list_item_count,
            link_count: value.link_count,
            code_block_count: value.code_block_count,
            table_row_count: value.table_row_count,
            reasoning_term_count: value.reasoning_term_count,
            math_symbol_count: value.math_symbol_count,
            constraint_term_count: value.constraint_term_count,
            question_count: value.question_count,
        }
    }
}

impl From<CoreRoutingMode> for ScoringMode {
    fn from(value: CoreRoutingMode) -> Self {
        match value {
            CoreRoutingMode::Tiered => Self::Tiered,
            CoreRoutingMode::Classifier => Self::Classifier,
        }
    }
}

impl From<CoreComplexityScore> for ComplexityResult {
    fn from(value: CoreComplexityScore) -> Self {
        Self {
            schema_version: value.schema_version.to_owned(),
            score: value.score,
            recommendation: value.recommendation,
            mode: value.mode.into(),
            features: value.features.into(),
        }
    }
}

impl From<CoreExclusionReason> for ExclusionReason {
    fn from(value: CoreExclusionReason) -> Self {
        match value {
            CoreExclusionReason::ProviderNotReady => Self::ProviderNotReady,
            CoreExclusionReason::PrivacyBoundaryDenied => Self::PrivacyBoundaryDenied,
            CoreExclusionReason::TextUnsupported => Self::TextUnsupported,
            CoreExclusionReason::ContextWindowUnknown => Self::ContextWindowUnknown,
            CoreExclusionReason::ContextWindowTooSmall => Self::ContextWindowTooSmall,
            CoreExclusionReason::ImageInputUnsupported => Self::ImageInputUnsupported,
            CoreExclusionReason::ToolsUnsupported => Self::ToolsUnsupported,
            CoreExclusionReason::StreamingUnsupported => Self::StreamingUnsupported,
            CoreExclusionReason::EmbeddingsUnsupported => Self::EmbeddingsUnsupported,
            CoreExclusionReason::ImageGenerationUnsupported => Self::ImageGenerationUnsupported,
            CoreExclusionReason::AudioUnsupported => Self::AudioUnsupported,
            CoreExclusionReason::BatchUnsupported => Self::BatchUnsupported,
            CoreExclusionReason::AutomaticNotAllowed => Self::AutomaticNotAllowed,
        }
    }
}

impl From<wayfinder_routing_core::CandidateAssessment> for CandidateAssessment {
    fn from(value: wayfinder_routing_core::CandidateAssessment) -> Self {
        Self {
            destination_id: value.destination_id,
            exclusions: value.exclusions.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<CoreRoutePlan> for RoutePlan {
    fn from(value: CoreRoutePlan) -> Self {
        Self {
            schema_version: value.schema_version,
            request_id: value.request_id,
            score: value.score,
            recommendation: value.recommendation,
            selected_destination_id: value.selected_destination_id,
            fallback_destination_ids: value.fallback_destination_ids,
            candidates: value.candidates.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::thread;

    use super::*;

    fn engine() -> Result<Arc<RoutingEngine>, RoutingBridgeError> {
        RoutingEngine::new(RoutingConfiguration {
            tiers: vec![
                RoutingTier {
                    min_score: 0.0,
                    model: "local".to_owned(),
                },
                RoutingTier {
                    min_score: 0.5,
                    model: "cloud".to_owned(),
                },
            ],
        })
    }

    fn destination(id: &str, boundary: ExecutionBoundary) -> DestinationSnapshot {
        DestinationSnapshot {
            id: id.to_owned(),
            provider_id: "provider".to_owned(),
            model_id: "model".to_owned(),
            display_name: "Model".to_owned(),
            route_tier: "local".to_owned(),
            execution_boundary: boundary,
            readiness: ProviderReadiness::Ready,
            billing_class: BillingClass::OnDevice,
            context_window: Some(4_096),
            capabilities: DestinationCapabilities {
                text: true,
                streaming: true,
                image_input: false,
                tools: false,
                embeddings: false,
                image_generation: false,
                audio_input: false,
                audio_output: false,
                batch: false,
            },
            automatic_eligible: true,
        }
    }

    fn request(prompt: String) -> RoutingRequest {
        RoutingRequest {
            schema_version: RUNTIME_CONTRACT_VERSION,
            request_id: "request-1".to_owned(),
            prompt,
            privacy_posture: PrivacyPosture::OnDeviceOnly,
            requirements: RoutingRequirements {
                context_tokens: Some(1_024),
                image_input: false,
                tools: false,
                streaming: true,
                surface: InferenceSurface::Text,
            },
        }
    }

    #[test]
    fn bridge_score_matches_the_authoritative_core() -> Result<(), Box<dyn Error>> {
        let engine = engine()?;
        let prompt = "Prove the theorem under exactly these constraints.";
        let bridge = engine.score(prompt.to_owned())?;
        let core = score_complexity(prompt, &engine.config)?;

        assert_eq!(bridge.score, core.score);
        assert_eq!(bridge.recommendation, core.recommendation);
        assert_eq!(bridge.features.word_count, core.features.word_count);
        assert_eq!(
            bridge.features.reasoning_term_count,
            core.features.reasoning_term_count
        );
        Ok(())
    }

    #[test]
    fn bridge_plan_enforces_privacy_before_scoring() -> Result<(), Box<dyn Error>> {
        let plan = engine()?.plan(
            request("hello".to_owned()),
            vec![
                destination("hosted", ExecutionBoundary::Hosted),
                destination("device", ExecutionBoundary::OnDevice),
            ],
        )?;

        assert_eq!(plan.selected_destination_id.as_deref(), Some("device"));
        assert!(matches!(
            plan.candidates[0].exclusions.as_slice(),
            [ExclusionReason::PrivacyBoundaryDenied]
        ));
        Ok(())
    }

    #[test]
    fn malformed_and_oversized_values_fail_without_echoing_content() -> Result<(), Box<dyn Error>> {
        let private_marker = "private-prompt-marker";
        let oversized = format!("{private_marker}{}", "x".repeat(MAX_PROMPT_BYTES));
        let error = engine()?.score(oversized).err().ok_or("expected error")?;
        let rendered = error.to_string();

        assert!(!rendered.contains(private_marker));
        assert!(rendered.contains("bridge limit"));

        let bad_config = RoutingEngine::new(RoutingConfiguration {
            tiers: vec![RoutingTier {
                min_score: f64::NAN,
                model: "local".to_owned(),
            }],
        });
        assert!(matches!(
            bad_config,
            Err(RoutingBridgeError::InvalidConfiguration(_))
        ));
        Ok(())
    }

    #[test]
    fn immutable_engine_is_safe_for_concurrent_scores() -> Result<(), Box<dyn Error>> {
        let engine = engine()?;
        let workers = (0..32)
            .map(|_| {
                let engine = Arc::clone(&engine);
                thread::spawn(move || engine.score("hello".to_owned()))
            })
            .collect::<Vec<_>>();

        for worker in workers {
            let result = worker.join().map_err(|_| "score worker panicked")??;
            assert_eq!(result.recommendation, "local");
        }
        Ok(())
    }

    #[test]
    fn bridge_types_do_not_contain_credential_fields() {
        let source = include_str!("lib.rs");
        let public_source = source
            .split_once("#[cfg(test)]")
            .map_or(source, |(public, _)| public);
        for forbidden in [
            "access_token:",
            "refresh_token:",
            "api_key:",
            "authorization_header:",
            "credential_path:",
        ] {
            assert!(!public_source.contains(forbidden), "{forbidden}");
        }
    }
}
