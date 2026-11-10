//! Sequential stages of the chat-completions routing pipeline.
//!
//! Each stage is a block lifted out of `chat_completions` unchanged: it takes
//! its inputs explicitly and, where the handler used to return early, returns
//! the exact response it used to return as `Err(Box<Response>)`. Stages run in
//! the handler's original order, so request validation, side effects and the
//! first error a request reports are the same as before the split.

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use serde_json::{Map, Value};
use wayfinder_routing_core::{
    ComplexityScore, PrivacyPosture, RoutingConfig, RoutingRequest, recommend_tier,
    score_complexity,
};
use wayfinder_service::pricing::estimate_tokens;

use crate::access::AccessGrant;
use crate::budget::{BudgetDecision, degrade_target, format_limit};
use crate::canary::CANARY_RESPONSE_HEADER;
use crate::decision_policy::{
    ROUTE_ON_HEADER, RouteOn, STICKY_COOLDOWN_HEADER, STICKY_HEADER, THRESHOLD_HEADER,
    TUNING_FIELD, apply_scoring_overrides, conversation_high_water, extract_prompt,
    parse_route_on_header, parse_threshold_header, resolve_slash_directive_for_names,
    resolve_sticky, resolve_sticky_cooldown, threshold_tiers,
};
use crate::route_mode::RouteMode;
use crate::{
    AppState, ConfiguredModel, DecisionPolicyReceipt, ROUTE_PRESET_PREFIX, ROUTER_BUDGET_HEADER,
    ROUTER_PRIVACY_POSTURE_HEADER, ROUTER_ROUTE_HEADER, ROUTER_WORKSPACE_HEADER, RequestSelection,
    assess_model, bad_override_response, clamp_to_allowed, decision_headers,
    destination_ineligible_response, error_response, exclusion_reason_name, insert_header,
    insert_rate_snapshot_headers, offline_override, parse_privacy_posture, privacy_posture_name,
    privacy_response_headers, request_header, request_id_headers, request_requirements,
    resolve_request_selection, utc_today,
};

/// Per-request routing options resolved from headers, the tuning field and
/// slash directives, together with the routing request they describe.
pub(crate) struct RequestOptions {
    /// Routing configuration after per-request scoring overrides.
    pub(crate) routing: RoutingConfig,
    /// Whether the sticky high-water latch applies.
    pub(crate) sticky: bool,
    /// Sticky latch cooldown, in turns.
    pub(crate) sticky_cooldown: u64,
    /// Request messages, with any recognized slash directive removed.
    pub(crate) messages: Value,
    /// Endpoint pinned by a slash directive, if any.
    pub(crate) slash_pin: Option<String>,
    /// Text selected for scoring.
    pub(crate) prompt: String,
    /// Whether delivery must stay on proven-local endpoints.
    pub(crate) offline: bool,
    /// Requested privacy posture.
    pub(crate) privacy_posture: PrivacyPosture,
    /// Token estimate over all message text.
    pub(crate) prompt_estimate: u64,
    /// The provider-neutral routing request used for eligibility.
    pub(crate) routing_request: RoutingRequest,
}

/// Resolve tuning, route-on, sticky, cooldown, slash, offline and privacy
/// options. Removes the tuning field from `body` and rewrites its messages
/// when a slash directive is recognized.
pub(crate) fn resolve_request_options(
    state: &AppState,
    headers: &HeaderMap,
    body: &mut Map<String, Value>,
    base_routing: &RoutingConfig,
    request_id: &str,
) -> Result<RequestOptions, Box<Response>> {
    let tuning = body.remove(TUNING_FIELD);
    let routing = match apply_scoring_overrides(base_routing, tuning.as_ref()) {
        Ok(routing) => routing,
        Err(error) => {
            return Err(Box::new(bad_override_response(
                request_id,
                error.to_string(),
            )));
        }
    };

    let route_on_header = match request_header(headers, ROUTE_ON_HEADER) {
        Ok(value) => value,
        Err(message) => return Err(Box::new(bad_override_response(request_id, message))),
    };
    let route_on = match parse_route_on_header(route_on_header) {
        Ok(value) => value.unwrap_or(state.route_on()),
        Err(error) => {
            return Err(Box::new(bad_override_response(
                request_id,
                error.to_string(),
            )));
        }
    };
    let sticky_header = match request_header(headers, STICKY_HEADER) {
        Ok(value) => value,
        Err(message) => return Err(Box::new(bad_override_response(request_id, message))),
    };
    let sticky = match resolve_sticky(sticky_header, state.sticky()) {
        Ok(value) => value,
        Err(error) => {
            return Err(Box::new(bad_override_response(
                request_id,
                error.to_string(),
            )));
        }
    };
    let sticky_cooldown_header = match request_header(headers, STICKY_COOLDOWN_HEADER) {
        Ok(value) => value,
        Err(message) => return Err(Box::new(bad_override_response(request_id, message))),
    };
    let sticky_cooldown =
        match resolve_sticky_cooldown(sticky_cooldown_header, state.sticky_cooldown()) {
            Ok(value) => value,
            Err(error) => {
                return Err(Box::new(bad_override_response(
                    request_id,
                    error.to_string(),
                )));
            }
        };

    let mut messages = body.get("messages").cloned().unwrap_or(Value::Null);
    let slash_resolution = state.slash_directives().then(|| {
        resolve_slash_directive_for_names(
            &messages,
            &routing,
            state.models().iter().map(ConfiguredModel::name),
        )
    });
    let slash_resolution = slash_resolution.flatten();
    if let Some(resolution) = &slash_resolution {
        messages = resolution.messages.clone();
        body.insert("messages".to_owned(), messages.clone());
    }
    let slash_pin = slash_resolution.and_then(|resolution| resolution.pin);
    let prompt = extract_prompt(&messages, route_on);
    let offline = state.offline() || offline_override(headers);
    let privacy_posture = match parse_privacy_posture(headers, offline) {
        Ok(value) => value,
        Err(message) => return Err(Box::new(bad_override_response(request_id, message))),
    };
    let prompt_estimate = estimate_tokens(&extract_prompt(&messages, RouteOn::All));
    let requirements = request_requirements(body, &messages, prompt_estimate);
    let routing_request = RoutingRequest {
        schema_version: wayfinder_routing_core::RUNTIME_CONTRACT_VERSION,
        request_id: request_id.to_owned(),
        prompt: prompt.clone(),
        privacy_posture,
        requirements,
    };
    Ok(RequestOptions {
        routing,
        sticky,
        sticky_cooldown,
        messages,
        slash_pin,
        prompt,
        offline,
        privacy_posture,
        prompt_estimate,
        routing_request,
    })
}

/// Score the prompt under the request's routing configuration.
pub(crate) fn score_request(
    prompt: &str,
    routing: &RoutingConfig,
    request_id: &str,
) -> Result<ComplexityScore, Box<Response>> {
    let decision_span = tracing::info_span!("wayfinder.decision");
    match decision_span.in_scope(|| score_complexity(prompt, routing)) {
        Ok(decision) => Ok(decision),
        Err(error) => Err(Box::new(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "wayfinder_router_misconfigured",
            error.to_string(),
            request_id_headers(request_id),
        ))),
    }
}

/// The route the pipeline currently intends to serve, and how it got there.
pub(crate) struct RouteSelection {
    /// Logical route name.
    pub(crate) chosen: String,
    /// How `chosen` was selected.
    pub(crate) mode: RouteMode,
    /// Ordered destinations of the selected routing preset, if any.
    pub(crate) preset_models: Option<Vec<String>>,
    /// Name of the selected routing preset, if any.
    pub(crate) preset_name: Option<String>,
    /// Whether preset delivery semantics still apply.
    pub(crate) preset_route_active: bool,
}

/// Select the initial route: an explicit model or preset, a slash pin, a
/// threshold override or the scored tier, then the sticky latch.
pub(crate) fn select_route(
    state: &AppState,
    headers: &HeaderMap,
    model_field: Option<&Value>,
    options: &RequestOptions,
    decision: &ComplexityScore,
    request_id: &str,
) -> Result<RouteSelection, Box<Response>> {
    let routing = &options.routing;
    let selection = match resolve_request_selection(model_field, state) {
        Ok(selection) => selection,
        Err(message) => {
            return Err(Box::new(error_response(
                StatusCode::BAD_REQUEST,
                "wayfinder_router_unknown_route",
                message,
                request_id_headers(request_id),
            )));
        }
    };
    let (chosen, mode, preset_models, preset_name) = if let Some(selection) = selection {
        match selection {
            RequestSelection::Model(pin) => (pin, RouteMode::Pinned, None, None),
            RequestSelection::Preset { name, models } => {
                let chosen = models[0].clone();
                (chosen, RouteMode::Preset, Some(models), Some(name))
            }
        }
    } else if let Some(slash_pin) = options.slash_pin.clone() {
        (slash_pin, RouteMode::SlashPinned, None, None)
    } else {
        let threshold_header = match request_header(headers, THRESHOLD_HEADER) {
            Ok(value) => value,
            Err(message) => return Err(Box::new(bad_override_response(request_id, message))),
        };
        let threshold = match parse_threshold_header(threshold_header) {
            Ok(value) => value,
            Err(error) => {
                return Err(Box::new(bad_override_response(
                    request_id,
                    error.to_string(),
                )));
            }
        };
        let (mut chosen, mut mode, effective_tiers) = if let Some(threshold) = threshold {
            let tiers = match threshold_tiers(routing, threshold) {
                Ok(tiers) => tiers,
                Err(error) => {
                    return Err(Box::new(bad_override_response(
                        request_id,
                        error.to_string(),
                    )));
                }
            };
            let Some(model) = recommend_tier(decision.score, &tiers) else {
                return Err(Box::new(error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "wayfinder_router_misconfigured",
                    "routing configuration contains no selectable tier".to_owned(),
                    request_id_headers(request_id),
                )));
            };
            (model.to_owned(), RouteMode::ThresholdOverride, tiers)
        } else {
            (
                decision.recommendation.clone(),
                RouteMode::Scored,
                routing.tiers.clone(),
            )
        };
        if options.sticky && routing.classifier.is_none() && effective_tiers.len() >= 2 {
            let latched = match conversation_high_water(
                &options.messages,
                routing,
                &effective_tiers,
                options.sticky_cooldown,
            ) {
                Ok(value) => value,
                Err(error) => {
                    return Err(Box::new(bad_override_response(
                        request_id,
                        error.to_string(),
                    )));
                }
            };
            if let Some(latched) = latched {
                let current_rank = effective_tiers
                    .iter()
                    .position(|tier| tier.model == chosen)
                    .unwrap_or(0);
                let latched_rank = effective_tiers
                    .iter()
                    .position(|tier| tier.model == latched)
                    .unwrap_or(0);
                if latched_rank > current_rank {
                    chosen = latched;
                    mode = RouteMode::Sticky;
                }
            }
        }
        (chosen, mode, None, None)
    };
    let preset_route_active = preset_name.is_some();
    Ok(RouteSelection {
        chosen,
        mode,
        preset_models,
        preset_name,
        preset_route_active,
    })
}

/// Drop preset destinations the key or request cannot use and serve the
/// first eligible one, or reject an explicit pin whose destination is
/// ineligible for this request.
pub(crate) fn enforce_destination_eligibility(
    state: &AppState,
    access_grant: Option<AccessGrant>,
    options: &RequestOptions,
    request_id: &str,
    route: &mut RouteSelection,
) -> Result<(), Box<Response>> {
    let routing_request = &options.routing_request;
    let privacy_posture = options.privacy_posture;
    if let Some(preset_models) = route.preset_models.as_mut() {
        let allowed_models = state
            .access_policy()
            .and_then(|policy| access_grant.map(|grant| policy.allowed_models(grant)))
            .filter(|allowed| !allowed.is_empty());
        let mut reasons = Vec::new();
        preset_models.retain(|name| {
            if allowed_models.is_some_and(|allowed| !allowed.iter().any(|model| model == name)) {
                reasons.push(format!("{name}: not-allowed-by-key"));
                return false;
            }
            if state.dry_run() {
                return true;
            }
            let Some(model) = state.models().iter().find(|model| model.name() == name) else {
                reasons.push(format!("{name}: unknown-model"));
                return false;
            };
            let assessment = assess_model(routing_request, model);
            if assessment.is_eligible() {
                true
            } else {
                reasons.push(format!(
                    "{name}: {}",
                    assessment
                        .exclusions
                        .iter()
                        .map(|reason| exclusion_reason_name(*reason))
                        .collect::<Vec<_>>()
                        .join(",")
                ));
                false
            }
        });
        let Some(first) = preset_models.first().cloned() else {
            return Err(Box::new(error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "wayfinder_router_destination_ineligible",
                format!(
                    "routing preset '{}' has no eligible destination for this request: {}",
                    route.preset_name.as_deref().unwrap_or("unknown"),
                    reasons.join("; ")
                ),
                privacy_response_headers(request_id, privacy_posture),
            )));
        };
        route.chosen = first;
    } else if !state.dry_run() && route.mode.is_pinned() {
        if let Some(model) = state
            .models()
            .iter()
            .find(|model| model.name() == route.chosen)
        {
            let assessment = assess_model(routing_request, model);
            if !assessment.is_eligible() {
                return Err(Box::new(destination_ineligible_response(
                    &route.chosen,
                    &assessment,
                    privacy_response_headers(request_id, privacy_posture),
                    options.offline,
                )));
            }
        }
    }
    Ok(())
}

/// The caller's virtual-key grant and the key and workspace it resolves to.
#[derive(Clone, Copy)]
pub(crate) struct RequestAccess<'a> {
    /// The authenticated grant, when an access policy is configured.
    pub(crate) grant: Option<AccessGrant>,
    /// Virtual-key id of the grant.
    pub(crate) key_id: Option<&'a str>,
    /// Workspace id of the grant.
    pub(crate) workspace_id: Option<&'a str>,
}

/// What the spend budget did to this request's route.
pub(crate) struct BudgetRouting {
    /// Value for the budget response header: `exhausted` or `degraded`.
    pub(crate) state: Option<&'static str>,
    /// The pre-degrade route, kept only when the budget actually rerouted, so
    /// a later key clamp that restores it cannot leave a false `degraded`.
    pub(crate) rerouted_from: Option<String>,
}

/// Evaluate the spend budget: allow, block the request, or degrade the route
/// to a cheaper tier the key may use.
pub(crate) async fn apply_budget(
    state: &AppState,
    access: RequestAccess<'_>,
    options: &RequestOptions,
    decision: &ComplexityScore,
    request_id: &str,
    route: &mut RouteSelection,
) -> Result<BudgetRouting, Box<Response>> {
    let RequestAccess {
        grant: access_grant,
        key_id,
        workspace_id,
    } = access;
    let offline = options.offline;
    let mut budget = BudgetRouting {
        state: None,
        rerouted_from: None,
    };
    if state.budget_policy().active() && state.price_table().priced {
        let today = match utc_today() {
            Ok(today) => today,
            Err(error) => {
                return Err(internal_error(
                    "wayfinder_router_state_error",
                    error.to_string(),
                    request_id,
                ));
            }
        };
        let budget_decision = if let Some(backend) = state
            .shared_state_backend()
            .filter(|backend| backend.backend_name() == "redis")
        {
            match state
                .budget_policy()
                .evaluate_shared_for(backend.as_ref(), true, workspace_id, key_id, today, offline)
                .await
            {
                Ok(decision) => Ok(decision),
                Err(_) => state
                    .budget_policy()
                    .evaluate_for(
                        state.savings_ledger(),
                        true,
                        workspace_id,
                        key_id,
                        today,
                        offline,
                    )
                    .map_err(|error| error.to_string()),
            }
        } else {
            state
                .budget_policy()
                .evaluate_for(
                    state.savings_ledger(),
                    true,
                    workspace_id,
                    key_id,
                    today,
                    offline,
                )
                .map_err(|error| error.to_string())
        };
        match budget_decision {
            Ok(BudgetDecision::Allow) => {}
            Ok(BudgetDecision::Degrade) => {
                // `exhausted` unless the budget really lowers the route below.
                // Offline delivery already converges on the cheapest local
                // tier later, so the budget itself does not reroute there.
                budget.state = Some("exhausted");
                let allowed_models = state
                    .access_policy()
                    .and_then(|policy| access_grant.map(|grant| policy.allowed_models(grant)))
                    .filter(|allowed| !allowed.is_empty());
                let target = (!offline)
                    .then(|| {
                        degrade_target(
                            decision.tiers.as_deref(),
                            decision.models.as_deref(),
                            &state.price_table().costs,
                            allowed_models,
                            &route.chosen,
                        )
                    })
                    .flatten();
                if let Some(target) = target {
                    budget.state = Some("degraded");
                    budget.rerouted_from = Some(std::mem::replace(&mut route.chosen, target));
                    route.mode = RouteMode::BudgetDegraded;
                    route.preset_models = None;
                    route.preset_name = None;
                    route.preset_route_active = false;
                }
            }
            Ok(BudgetDecision::Block { window, limit }) => {
                let mut headers = request_id_headers(request_id);
                headers.insert(ROUTER_BUDGET_HEADER, HeaderValue::from_static("blocked"));
                return Err(Box::new(error_response(
                    StatusCode::PAYMENT_REQUIRED,
                    "wayfinder_router_budget_exhausted",
                    format!("{window} budget of {} reached", format_limit(limit)),
                    headers,
                )));
            }
            Err(error) => {
                return Err(internal_error(
                    "wayfinder_router_state_error",
                    error,
                    request_id,
                ));
            }
        }
    }
    Ok(budget)
}

/// Clamp the route onto the key's model allowlist, walking the tier ladder.
pub(crate) fn clamp_route_to_key(
    state: &AppState,
    access_grant: Option<AccessGrant>,
    decision: &ComplexityScore,
    route: &mut RouteSelection,
) {
    if let (Some(policy), Some(grant)) = (state.access_policy(), access_grant) {
        let allowed = policy.allowed_models(grant);
        if !allowed.is_empty() {
            let mut ladder = decision
                .tiers
                .as_ref()
                .map(|tiers| tiers.iter().collect::<Vec<_>>())
                .unwrap_or_default();
            ladder.sort_by(|left, right| left.min_score.total_cmp(&right.min_score));
            let ladder = ladder
                .into_iter()
                .map(|tier| tier.model.as_str())
                .collect::<Vec<_>>();
            let clamped = clamp_to_allowed(&route.chosen, &ladder, allowed);
            if clamped != route.chosen {
                route.chosen = clamped;
                route.mode = RouteMode::KeyScoped;
            }
        }
    }
}

/// Inputs to the routing response headers.
pub(crate) struct RoutingHeaderInput<'a> {
    /// The final route, after budget, key clamp and canary.
    pub(crate) route: &'a RouteSelection,
    /// Complexity score of the request.
    pub(crate) score: f64,
    /// Request id echoed on every response.
    pub(crate) request_id: &'a str,
    /// Whether the request is served offline.
    pub(crate) offline: bool,
    /// Requested privacy posture.
    pub(crate) privacy_posture: PrivacyPosture,
    /// Resolved policy profile, if any.
    pub(crate) policy_profile_id: Option<&'a str>,
    /// Budget header value, if the budget applied.
    pub(crate) budget_state: Option<&'static str>,
    /// Whether a canary observation guard was assigned.
    pub(crate) canary_assigned: bool,
    /// The caller's virtual-key grant.
    pub(crate) access_grant: Option<AccessGrant>,
}

/// Assemble the routing decision, privacy, budget, canary, preset, workspace
/// and rate-limit headers every routed response carries.
pub(crate) async fn build_routing_headers(
    state: &AppState,
    input: RoutingHeaderInput<'_>,
) -> Result<HeaderMap, Box<Response>> {
    let request_id = input.request_id;
    let misconfigured =
        |message: String| internal_error("wayfinder_router_misconfigured", message, request_id);
    let mut routing_headers = decision_headers(
        &input.route.chosen,
        input.score,
        input.route.mode,
        request_id,
        input.offline,
        DecisionPolicyReceipt {
            version: state.policy_version(),
            snapshot_id: state.policy_snapshot_id(),
            profile_id: input.policy_profile_id,
        },
    )
    .map_err(misconfigured)?;
    insert_header(
        &mut routing_headers,
        ROUTER_PRIVACY_POSTURE_HEADER,
        privacy_posture_name(input.privacy_posture),
    )
    .map_err(misconfigured)?;
    if let Some(budget_state) = input.budget_state {
        routing_headers.insert(ROUTER_BUDGET_HEADER, HeaderValue::from_static(budget_state));
    }
    if input.canary_assigned {
        insert_header(
            &mut routing_headers,
            CANARY_RESPONSE_HEADER,
            &state.canary_config().rollout_id,
        )
        .map_err(misconfigured)?;
    }
    if let Some(route_name) = input.route.preset_name.as_deref() {
        insert_header(
            &mut routing_headers,
            ROUTER_ROUTE_HEADER,
            &format!("{ROUTE_PRESET_PREFIX}{route_name}"),
        )
        .map_err(misconfigured)?;
    }
    if let (Some(policy), Some(grant)) = (state.access_policy(), input.access_grant) {
        if let Some(workspace) = policy.workspace_id(grant) {
            let value = match HeaderValue::from_str(workspace) {
                Ok(value) => value,
                Err(_) => {
                    return Err(misconfigured(
                        "workspace id cannot be represented as a response header".to_owned(),
                    ));
                }
            };
            routing_headers.insert(ROUTER_WORKSPACE_HEADER, value);
        }
        match policy.tightest_snapshot_shared(grant).await {
            Ok(Some(snapshot)) => {
                let _ = state.metrics().set_state_degraded(state.state_degraded());
                if let Err(message) = insert_rate_snapshot_headers(&mut routing_headers, snapshot) {
                    return Err(internal_error(
                        "wayfinder_router_state_error",
                        message,
                        request_id,
                    ));
                }
            }
            Ok(None) => {
                let _ = state.metrics().set_state_degraded(state.state_degraded());
            }
            Err(error) => {
                let _ = state.metrics().set_state_degraded(state.state_degraded());
                return Err(internal_error(
                    "wayfinder_router_state_error",
                    error.to_string(),
                    request_id,
                ));
            }
        }
    }
    Ok(routing_headers)
}

/// A 500 error carrying only the request id header, the convention for
/// failures before the routing headers exist.
fn internal_error(kind: &'static str, message: String, request_id: &str) -> Box<Response> {
    Box::new(error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        kind,
        message,
        request_id_headers(request_id),
    ))
}
