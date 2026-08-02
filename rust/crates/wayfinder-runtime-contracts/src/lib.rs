//! Platform-neutral data contracts shared by Wayfinder routing hosts.
//!
//! These value types contain no credentials, provider payloads, filesystem
//! paths, platform handles, or executable behavior. They are suitable for
//! generated bindings and portable golden fixtures.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Current schema version for embedded runtime values.
pub const RUNTIME_CONTRACT_VERSION: u32 = 1;

/// Where prompt content is executed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionBoundary {
    /// The current device executes the request.
    OnDevice,
    /// A trusted device on the local network receives the request.
    LocalNetwork,
    /// A hosted provider receives the request.
    Hosted,
}

/// User-selected maximum content boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrivacyPosture {
    /// Only the current device may receive content.
    OnDeviceOnly,
    /// The current device or trusted local devices may receive content.
    LocalDevices,
    /// On-device, local-network, and hosted destinations are permitted.
    HostedAllowed,
}

impl PrivacyPosture {
    /// Whether this posture permits a destination boundary.
    #[must_use]
    pub const fn permits(self, boundary: ExecutionBoundary) -> bool {
        match self {
            Self::OnDeviceOnly => matches!(boundary, ExecutionBoundary::OnDevice),
            Self::LocalDevices => {
                matches!(
                    boundary,
                    ExecutionBoundary::OnDevice | ExecutionBoundary::LocalNetwork
                )
            }
            Self::HostedAllowed => true,
        }
    }
}

/// Normalized provider readiness used before scoring.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderReadiness {
    /// Readiness is not yet known.
    Checking,
    /// Account authorization is absent.
    SignedOut,
    /// An authorization flow is active.
    Authorizing,
    /// The destination may execute requests.
    Ready,
    /// Stored authorization must be refreshed by the user.
    ReauthenticationRequired,
    /// The provider reports a current usage limit.
    UsageLimited,
    /// The configured model is not currently available.
    ModelUnavailable,
    /// Required network access is not currently available.
    NetworkUnavailable,
    /// This host platform cannot execute the provider.
    UnsupportedPlatform,
    /// The provider is unavailable for a bounded known reason.
    Unavailable,
    /// Readiness failed with a sanitized error.
    Failed,
}

/// Provider accounting class without fabricated cost semantics.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BillingClass {
    /// Execution uses a model on the current device.
    OnDevice,
    /// Execution is covered by a documented account subscription.
    Subscription,
    /// Execution is metered by an API provider.
    ApiMetered,
    /// Accounting semantics are not known.
    Unknown,
}

/// A normalized execution surface.  Surface selection is a provider
/// capability, not part of the deterministic complexity score.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InferenceSurface {
    /// Text generation (Chat Completions, Messages, or bounded Responses).
    #[default]
    Text,
    /// Vector embeddings.
    Embeddings,
    /// Image generation.
    ImageGeneration,
    /// Audio input or output.
    Audio,
    /// Asynchronous batch execution.
    Batch,
}

/// Explicit provider support for non-text execution surfaces.
///
/// These flags are deliberately separate from the existing text/image/tool
/// fields. A provider must opt into each surface only after its adapter,
/// bounds, accounting, and parity fixtures are complete.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModalityCapabilities {
    /// Vector embeddings are supported.
    pub embeddings: bool,
    /// Image generation is supported.
    pub image_generation: bool,
    /// Audio input is supported.
    pub audio_input: bool,
    /// Audio output is supported.
    pub audio_output: bool,
    /// Durable batch execution is supported.
    pub batch: bool,
}

/// Capabilities that can make a destination ineligible before scoring.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DestinationCapabilities {
    /// Text input and output are supported.
    pub text: bool,
    /// Incremental output is supported.
    pub streaming: bool,
    /// Image input is supported.
    pub image_input: bool,
    /// Reviewed tool calls are supported.
    pub tools: bool,
    /// Explicit non-text surface capabilities.
    #[serde(default)]
    pub modalities: ModalityCapabilities,
}

/// A host-provided, secret-free snapshot of one concrete destination.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DestinationSnapshot {
    /// Stable destination identifier.
    pub id: String,
    /// Stable provider identifier.
    pub provider_id: String,
    /// Provider model identifier.
    pub model_id: String,
    /// User-visible destination name.
    pub display_name: String,
    /// Configured routing tier or route member name.
    pub route_tier: String,
    /// Current content execution boundary.
    pub execution_boundary: ExecutionBoundary,
    /// Current normalized readiness.
    pub readiness: ProviderReadiness,
    /// Accounting classification.
    pub billing_class: BillingClass,
    /// Advertised context limit when known.
    pub context_window: Option<u64>,
    /// Advertised destination capabilities.
    pub capabilities: DestinationCapabilities,
    /// Whether the user has allowed Automatic to consider this destination.
    pub automatic_eligible: bool,
}

/// Hard requirements supplied to eligibility filtering before scoring.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoutingRequirements {
    /// Required context capacity when known.
    pub context_tokens: Option<u64>,
    /// The request includes image input.
    pub image_input: bool,
    /// The request requires tool support.
    pub tools: bool,
    /// The caller requires incremental output.
    pub streaming: bool,
    /// Surface requested by the caller; defaults to text for compatibility.
    #[serde(default)]
    pub surface: InferenceSurface,
}

/// Platform-neutral input to the authoritative routing engine.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoutingRequest {
    /// Contract version; must equal [`RUNTIME_CONTRACT_VERSION`].
    pub schema_version: u32,
    /// Opaque bounded request identifier supplied by the host.
    pub request_id: String,
    /// Text scored by the deterministic routing core.
    pub prompt: String,
    /// Maximum content boundary selected by the user.
    pub privacy_posture: PrivacyPosture,
    /// Hard compatibility requirements.
    pub requirements: RoutingRequirements,
}

/// Stable reasons a candidate was excluded before scoring.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExclusionReason {
    /// Provider readiness is not `ready`.
    ProviderNotReady,
    /// The current privacy posture denies this boundary.
    PrivacyBoundaryDenied,
    /// Text generation is not supported.
    TextUnsupported,
    /// The destination does not advertise a context window.
    ContextWindowUnknown,
    /// The destination context window is too small.
    ContextWindowTooSmall,
    /// Image input is required but unsupported.
    ImageInputUnsupported,
    /// Tools are required but unsupported.
    ToolsUnsupported,
    /// Streaming is required but unsupported.
    StreamingUnsupported,
    /// Embeddings are required but unsupported.
    EmbeddingsUnsupported,
    /// Image generation is required but unsupported.
    ImageGenerationUnsupported,
    /// Audio is required but unsupported.
    AudioUnsupported,
    /// Batch execution is required but unsupported.
    BatchUnsupported,
    /// The user has excluded this destination from Automatic.
    AutomaticNotAllowed,
}

/// Eligibility result for one input destination.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidateAssessment {
    /// Destination being assessed.
    pub destination_id: String,
    /// Empty when the candidate is eligible.
    pub exclusions: Vec<ExclusionReason>,
}

impl CandidateAssessment {
    /// Whether the candidate passed every hard compatibility filter.
    #[must_use]
    pub fn is_eligible(&self) -> bool {
        self.exclusions.is_empty()
    }
}

/// Deterministic output of scoring and destination planning.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RoutePlan {
    /// Contract version.
    pub schema_version: u32,
    /// Input request identifier.
    pub request_id: String,
    /// Rounded deterministic complexity score.
    pub score: f64,
    /// Tier or route member recommended by the configured scorer.
    pub recommendation: String,
    /// Selected concrete destination, absent when no candidate is eligible.
    pub selected_destination_id: Option<String>,
    /// Remaining eligible destinations in stable fallback order.
    pub fallback_destination_ids: Vec<String>,
    /// Assessment for every supplied candidate in input order.
    pub candidates: Vec<CandidateAssessment>,
}

/// Compact deterministic explanation suitable for a route inspector.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RouteExplanation {
    /// Rounded deterministic complexity score.
    pub score: f64,
    /// Stable reason codes emitted by the core.
    pub reason_codes: Vec<String>,
}

/// Persistable, secret-free execution receipt.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RouteReceipt {
    /// Contract version.
    pub schema_version: u32,
    /// Destination selected by the route plan.
    pub destination_id: String,
    /// Provider display identifier at execution time.
    pub provider_id: String,
    /// Model display identifier at execution time.
    pub model_id: String,
    /// Actual content execution boundary.
    pub execution_boundary: ExecutionBoundary,
    /// Deterministic score when Automatic selected the destination.
    pub score: Option<f64>,
    /// Stable routing reason codes.
    pub reason_codes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn privacy_postures_enforce_exact_boundary_ladders() {
        assert!(PrivacyPosture::OnDeviceOnly.permits(ExecutionBoundary::OnDevice));
        assert!(!PrivacyPosture::OnDeviceOnly.permits(ExecutionBoundary::LocalNetwork));
        assert!(!PrivacyPosture::OnDeviceOnly.permits(ExecutionBoundary::Hosted));

        assert!(PrivacyPosture::LocalDevices.permits(ExecutionBoundary::OnDevice));
        assert!(PrivacyPosture::LocalDevices.permits(ExecutionBoundary::LocalNetwork));
        assert!(!PrivacyPosture::LocalDevices.permits(ExecutionBoundary::Hosted));

        assert!(PrivacyPosture::HostedAllowed.permits(ExecutionBoundary::OnDevice));
        assert!(PrivacyPosture::HostedAllowed.permits(ExecutionBoundary::LocalNetwork));
        assert!(PrivacyPosture::HostedAllowed.permits(ExecutionBoundary::Hosted));
    }

    #[test]
    fn route_contract_round_trips_without_secret_fields() -> Result<(), Box<dyn Error>> {
        let request = RoutingRequest {
            schema_version: RUNTIME_CONTRACT_VERSION,
            request_id: "request-1".to_owned(),
            prompt: "Explain this locally".to_owned(),
            privacy_posture: PrivacyPosture::OnDeviceOnly,
            requirements: RoutingRequirements {
                streaming: true,
                ..RoutingRequirements::default()
            },
        };

        let encoded = serde_json::to_string(&request)?;
        let decoded: RoutingRequest = serde_json::from_str(&encoded)?;

        assert_eq!(decoded, request);
        for forbidden in [
            "access_token",
            "refresh_token",
            "api_key",
            "authorization",
            "credential_path",
        ] {
            assert!(!encoded.contains(forbidden));
        }
        Ok(())
    }
}
