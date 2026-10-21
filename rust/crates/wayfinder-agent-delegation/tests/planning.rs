use wayfinder_agent_delegation::*;
use wayfinder_routing_core::{
    ExclusionReason, PrivacyPosture, ProviderReadiness, RoutingConfig, RoutingRequest,
    RoutingRequirements, Tier, score_complexity,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn task() -> Task {
    Task {
        kind: TaskKind::Review,
        request: RoutingRequest {
            schema_version: 1,
            request_id: "task-1".into(),
            prompt: "Review this change".into(),
            privacy_posture: PrivacyPosture::HostedAllowed,
            requirements: RoutingRequirements {
                tools: true,
                ..RoutingRequirements::default()
            },
        },
    }
}

fn agent(id: &str, client: AgentClient) -> AgentDeclaration {
    AgentDeclaration {
        id: id.into(),
        client,
        route_tier: "coding".into(),
        enabled: true,
        declared_access: AccessDeclaration::Subscription,
        reported_readiness: ProviderReadiness::Ready,
        task_kinds: vec![TaskKind::Review, TaskKind::Edit],
        context_window: None,
    }
}

fn policy() -> RoutingConfig {
    RoutingConfig {
        tiers: vec![Tier::new(0.0, "coding")],
        ..RoutingConfig::default()
    }
}

#[test]
fn matches_authoritative_policy_and_preserves_stable_order() -> TestResult {
    let task = task();
    let policy = policy();
    let agents = [
        agent("first", AgentClient::Codex),
        agent("second", AgentClient::ClaudeCode),
    ];
    let first = preview(&task, &agents, &policy)?;
    let second = preview(&task, &agents, &policy)?;
    assert_eq!(first.routing, second.routing);
    assert_eq!(
        first.routing.score,
        score_complexity(&task.request.prompt, &policy)?.score
    );
    assert_eq!(
        first.routing.selected_destination_id.as_deref(),
        Some("first")
    );
    assert_eq!(first.routing.fallback_destination_ids, ["second"]);
    assert_eq!(first.proposed_client, Some(AgentClient::Codex));
    Ok(())
}

#[test]
fn api_and_unknown_access_cannot_replace_declared_subscription() -> TestResult {
    let mut api = agent("api", AgentClient::Codex);
    api.declared_access = AccessDeclaration::ApiMetered;
    let mut unknown = agent("unknown", AgentClient::ClaudeCode);
    unknown.declared_access = AccessDeclaration::Unknown;
    let plan = preview(
        &task(),
        &[api, unknown, agent("subscription", AgentClient::ClaudeCode)],
        &policy(),
    )?;
    assert_eq!(
        plan.routing.selected_destination_id.as_deref(),
        Some("subscription")
    );
    assert_eq!(
        plan.agents[0].exclusions,
        [AgentExclusion::SubscriptionNotDeclared]
    );
    assert_eq!(
        plan.agents[1].exclusions,
        [AgentExclusion::SubscriptionNotDeclared]
    );
    Ok(())
}

#[test]
fn unavailable_or_unapproved_agents_are_excluded() -> TestResult {
    for readiness in [
        ProviderReadiness::Checking,
        ProviderReadiness::SignedOut,
        ProviderReadiness::Authorizing,
        ProviderReadiness::UsageLimited,
        ProviderReadiness::ReauthenticationRequired,
        ProviderReadiness::Unavailable,
    ] {
        let mut unavailable = agent("unavailable", AgentClient::Codex);
        unavailable.reported_readiness = readiness;
        let plan = preview(&task(), &[unavailable], &policy())?;
        assert!(plan.routing.selected_destination_id.is_none());
        assert!(
            plan.routing.candidates[0]
                .exclusions
                .contains(&ExclusionReason::ProviderNotReady)
        );
    }
    let mut disabled = agent("disabled", AgentClient::Codex);
    disabled.enabled = false;
    assert!(
        preview(&task(), &[disabled], &policy())?
            .proposed_client
            .is_none()
    );
    Ok(())
}

#[test]
fn task_capabilities_privacy_and_context_remain_hard_requirements() -> TestResult {
    let agents = [agent("codex", AgentClient::Codex)];
    let mut input = task();
    input.kind = TaskKind::Test;
    assert_eq!(
        preview(&input, &agents, &policy())?.agents[0].exclusions,
        [AgentExclusion::TaskKindUnsupported]
    );
    input.kind = TaskKind::Review;
    input.request.privacy_posture = PrivacyPosture::OnDeviceOnly;
    let report = preview(&input, &agents, &policy())?;
    assert!(report.proposed_client.is_none());
    assert!(
        report.routing.candidates[0]
            .exclusions
            .contains(&ExclusionReason::PrivacyBoundaryDenied)
    );
    input.request.privacy_posture = PrivacyPosture::HostedAllowed;
    input.request.requirements.context_tokens = Some(1024);
    let report = preview(&input, &agents, &policy())?;
    assert!(report.proposed_client.is_none());
    assert!(
        report.routing.candidates[0]
            .exclusions
            .contains(&ExclusionReason::ContextWindowUnknown)
    );
    Ok(())
}

#[test]
fn does_not_cross_tiers_when_the_selected_tier_has_no_eligible_agent() -> TestResult {
    let agents = [agent("codex", AgentClient::Codex)];
    let policy = RoutingConfig {
        tiers: vec![Tier::new(0.0, "other")],
        ..policy()
    };
    let plan = preview(&task(), &agents, &policy)?;
    assert_eq!(plan.routing.recommendation, "other");
    assert!(plan.proposed_client.is_none());
    assert!(plan.routing.fallback_destination_ids.is_empty());
    Ok(())
}

#[test]
fn rejects_invalid_contracts_and_non_coding_tasks() {
    let a = agent("duplicate", AgentClient::Codex);
    assert!(preview(&task(), &[a.clone(), a], &policy()).is_err());
    let mut input = task();
    input.request.schema_version = 99;
    assert!(preview(&input, &[], &policy()).is_err());
    input.request.schema_version = 1;
    input.request.requirements.tools = false;
    assert!(matches!(
        preview(&input, &[], &policy()),
        Err(PlanError::InvalidTask)
    ));
}

#[test]
fn preview_does_not_disclose_prompt_or_claim_execution_or_verified_billing() -> TestResult {
    let mut input = task();
    input.request.prompt = "private task sentinel".into();
    let report = preview(&input, &[agent("codex", AgentClient::Codex)], &policy())?;
    let encoded = serde_json::to_string(&report)?;
    assert!(!encoded.contains("private task sentinel"));
    assert_eq!(report.access_evidence, "operator-declaration-only");
    assert_eq!(
        report.dispatch_status,
        "blocked-pending-agent-auth-contract"
    );
    assert!(!encoded.contains("cost_usd"));
    Ok(())
}

#[test]
fn checked_in_fixture_uses_the_real_policy_parser() -> TestResult {
    let policy = wayfinder_config::routing_config_from_toml(
        include_str!("../fixtures/policy.toml"),
        "fixture",
        None,
        wayfinder_config::TierOrderPolicy::StrictInput,
    )?;
    let agents =
        serde_json::from_str::<Vec<AgentDeclaration>>(include_str!("../fixtures/agents.json"))?;
    let input = serde_json::from_str::<Task>(include_str!("../fixtures/task.json"))?;
    let report = preview(&input, &agents, &policy)?;
    assert_eq!(
        report.routing.selected_destination_id.as_deref(),
        Some("personal-codex")
    );
    Ok(())
}
