use serde::{Deserialize, Serialize};
use thiserror::Error;
use wayfinder_routing_core::{
    BillingClass, CoreError, DestinationCapabilities, DestinationSnapshot, ExecutionBoundary,
    ProviderReadiness, RoutePlan, RoutingConfig, RoutingRequest, plan_automatic_route,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentClient {
    Codex,
    ClaudeCode,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum AccessDeclaration {
    Subscription,
    ApiMetered,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    Review,
    Edit,
    Test,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub kind: TaskKind,
    pub request: RoutingRequest,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDeclaration {
    pub id: String,
    pub client: AgentClient,
    pub route_tier: String,
    pub enabled: bool,
    pub declared_access: AccessDeclaration,
    pub reported_readiness: ProviderReadiness,
    pub task_kinds: Vec<TaskKind>,
    pub context_window: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentExclusion {
    SubscriptionNotDeclared,
    TaskKindUnsupported,
}

#[derive(Debug, Serialize)]
pub struct AgentAssessment {
    pub agent_id: String,
    pub exclusions: Vec<AgentExclusion>,
}

#[derive(Debug, Serialize)]
pub struct DelegationPreview {
    pub schema_version: &'static str,
    pub access_evidence: &'static str,
    pub dispatch_status: &'static str,
    pub proposed_client: Option<AgentClient>,
    pub routing: RoutePlan,
    pub agents: Vec<AgentAssessment>,
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("delegation requires a non-empty text task with tool support")]
    InvalidTask,
    #[error(transparent)]
    Routing(#[from] CoreError),
}

pub fn preview(
    task: &Task,
    agents: &[AgentDeclaration],
    policy: &RoutingConfig,
) -> Result<DelegationPreview, PlanError> {
    if task.request.prompt.trim().is_empty()
        || !task.request.requirements.tools
        || task.request.requirements.surface != wayfinder_routing_core::InferenceSurface::Text
    {
        return Err(PlanError::InvalidTask);
    }
    let assessments = agents
        .iter()
        .map(|agent| {
            let mut exclusions = Vec::new();
            if agent.declared_access != AccessDeclaration::Subscription {
                exclusions.push(AgentExclusion::SubscriptionNotDeclared);
            }
            if !agent.task_kinds.contains(&task.kind) {
                exclusions.push(AgentExclusion::TaskKindUnsupported);
            }
            AgentAssessment {
                agent_id: agent.id.clone(),
                exclusions,
            }
        })
        .collect::<Vec<_>>();
    let destinations = agents
        .iter()
        .zip(&assessments)
        .map(|(agent, assessment)| DestinationSnapshot {
            id: agent.id.clone(),
            provider_id: match agent.client {
                AgentClient::Codex => "codex-cli",
                AgentClient::ClaudeCode => "claude-code-cli",
            }
            .to_owned(),
            model_id: "agent-managed-unverified".to_owned(),
            display_name: agent.id.clone(),
            route_tier: agent.route_tier.clone(),
            execution_boundary: ExecutionBoundary::Hosted,
            readiness: agent.reported_readiness,
            billing_class: BillingClass::Unknown,
            context_window: agent.context_window,
            capabilities: DestinationCapabilities {
                text: true,
                tools: true,
                ..DestinationCapabilities::default()
            },
            automatic_eligible: agent.enabled && assessment.exclusions.is_empty(),
        })
        .collect::<Vec<_>>();
    let routing = plan_automatic_route(&task.request, &destinations, policy)?;
    let proposed_client = agents
        .iter()
        .find(|agent| Some(&agent.id) == routing.selected_destination_id.as_ref())
        .map(|agent| agent.client);
    Ok(DelegationPreview {
        schema_version: "wf-agent-delegation-preview-v1",
        access_evidence: "operator-declaration-only",
        dispatch_status: "blocked-pending-agent-auth-contract",
        proposed_client,
        routing,
        agents: assessments,
    })
}
