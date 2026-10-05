//! Opt-in comparison boundary. Never used by the production routing path.
//! This is our experiment contract, not a provider's API schema.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};
use wayfinder_routing_core::{RoutingConfig, score_complexity};

pub type EngineFuture<'a> = Pin<Box<dyn Future<Output = Result<Answer, Failure>> + Send + 'a>>;

/// Only the explicitly selected fields cross the adapter boundary. Expected
/// answers, local ids, other repository context and credentials are excluded.
#[derive(Clone, Debug, Serialize)]
pub struct DisclosedRequest {
    pub question: String,
    pub answers: Vec<String>,
    pub context: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EngineIdentity {
    pub name: String,
    pub version: String,
    pub external: bool,
    pub test_double: bool,
}

/// Implementations must be cancellation-safe, must not spawn detached requests,
/// and must do no I/O before `decide`. Dropping the future cancels local work;
/// it cannot promise cancellation/billing reversal at a remote provider.
pub trait DecisionEngine {
    fn identity(&self) -> EngineIdentity;
    fn decide<'a>(&'a self, request: DisclosedRequest) -> EngineFuture<'a>;
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub answer: String,
    pub provider_request_id: Option<String>,
    pub model_version: Option<String>,
    pub confidence: Option<f64>,
    pub distribution: Option<BTreeMap<String, f64>>,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    Disabled,
    ContextDenied,
    InvalidCase,
    Timeout,
    Unavailable,
    Abstained,
    MalformedResponse,
    ProviderError,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Answered { value: Answer },
    Inconclusive { reason: Failure },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub question: String,
    pub answers: Vec<String>,
    pub context: BTreeMap<String, String>,
    pub expected: String,
}

/// Default denies every external adapter and discloses no context.
#[derive(Default)]
pub struct ExperimentOptions {
    pub allow_external: bool,
    pub context_fields: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Receipt {
    pub decision_id: String,
    pub case_id: String,
    pub engine: EngineIdentity,
    pub policy_version: String,
    pub elapsed_ms: f64,
    pub outcome: Outcome,
    pub correct: Option<bool>,
}

fn valid_answer(answer: &Answer, allowed: &[String]) -> bool {
    let probability = |n: f64| n.is_finite() && (0.0..=1.0).contains(&n);
    allowed.contains(&answer.answer)
        && answer.confidence.is_none_or(probability)
        && answer.cost_usd.is_none_or(|n| n.is_finite() && n >= 0.0)
        && answer.distribution.as_ref().is_none_or(|distribution| {
            distribution.len() == allowed.len()
                && distribution
                    .iter()
                    .all(|(key, p)| allowed.contains(key) && probability(*p))
                && (distribution.values().sum::<f64>() - 1.0).abs() <= 1e-6
        })
}

/// The timeout includes parsing/validation performed by the adapter future.
/// Inconclusive results are never silently converted into a route or retried.
pub async fn evaluate(
    engine: &dyn DecisionEngine,
    case: &Case,
    policy_version: &str,
    options: &ExperimentOptions,
    deadline: Duration,
) -> Receipt {
    let identity = engine.identity();
    let started = Instant::now();
    let invalid = case.id.is_empty()
        || policy_version.is_empty()
        || case.question.is_empty()
        || case.answers.is_empty()
        || case.answers.iter().any(String::is_empty)
        || case
            .answers
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != case.answers.len()
        || !case.answers.contains(&case.expected)
        || deadline.is_zero();
    let result = if invalid {
        Err(Failure::InvalidCase)
    } else if identity.external && !options.allow_external {
        Err(Failure::Disabled)
    } else if identity.external
        && case
            .context
            .keys()
            .any(|key| !options.context_fields.contains(key))
    {
        // Reject incomplete disclosure consent rather than silently changing
        // the task between engines in a comparison.
        Err(Failure::ContextDenied)
    } else {
        let request = DisclosedRequest {
            question: case.question.clone(),
            answers: case.answers.clone(),
            context: case.context.clone(),
        };
        match tokio::time::timeout(deadline, engine.decide(request)).await {
            Err(_) => Err(Failure::Timeout),
            Ok(Ok(answer)) if !valid_answer(&answer, &case.answers) => {
                Err(Failure::MalformedResponse)
            }
            Ok(result) => result,
        }
    };
    let correct = result
        .as_ref()
        .ok()
        .map(|value| value.answer == case.expected);
    Receipt {
        decision_id: uuid::Uuid::new_v4().to_string(),
        case_id: case.id.clone(),
        engine: identity,
        policy_version: policy_version.to_owned(),
        elapsed_ms: started.elapsed().as_secs_f64() * 1_000.0,
        correct,
        outcome: match result {
            Ok(value) => Outcome::Answered { value },
            Err(reason) => Outcome::Inconclusive { reason },
        },
    }
}

/// Calls the existing scorer; no model, credential or network dependency enters
/// wayfinder-routing-core. The experiment uses a pinned, named binary policy.
pub struct DeterministicEngine;
pub const POLICY_VERSION: &str = "experiment-binary-threshold-0.01-v1";
impl DecisionEngine for DeterministicEngine {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            name: "wayfinder-deterministic".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            external: false,
            test_double: false,
        }
    }
    fn decide<'a>(&'a self, request: DisclosedRequest) -> EngineFuture<'a> {
        Box::pin(async move {
            let prompt = request
                .context
                .get("prompt")
                .ok_or(Failure::MalformedResponse)?;
            let score = score_complexity(prompt, &RoutingConfig::binary(0.01))
                .map_err(|_| Failure::ProviderError)?;
            Ok(Answer {
                answer: score.recommendation,
                provider_request_id: None,
                model_version: None,
                confidence: None,
                distribution: None,
                cost_usd: Some(0.0),
            })
        })
    }
}

/// Local transport double exercising the boundary only. It is deliberately
/// labelled and does not estimate Luna/Decisions quality, latency or API shape.
pub struct ContractDouble {
    pub name: String,
    pub response: Result<Answer, Failure>,
    pub delay: Duration,
}
impl DecisionEngine for ContractDouble {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            name: self.name.clone(),
            version: "contract-double-v1".into(),
            external: true,
            test_double: true,
        }
    }
    fn decide<'a>(&'a self, _request: DisclosedRequest) -> EngineFuture<'a> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            self.response.clone()
        })
    }
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub cases: usize,
    /// Fraction correct over ALL cases, including inconclusive outcomes.
    pub accuracy: Option<f64>,
    pub answered_accuracy: Option<f64>,
    pub inconclusive_rate: Option<f64>,
    pub mean_latency_ms: Option<f64>,
    /// No total is asserted unless every case supplies a known cost.
    pub total_cost_usd: Option<f64>,
    pub known_cost_cases: usize,
}
pub fn summarize(receipts: &[Receipt]) -> Summary {
    let cases = receipts.len();
    let answered = receipts.iter().filter(|r| r.correct.is_some()).count();
    let correct = receipts.iter().filter(|r| r.correct == Some(true)).count();
    let costs: Vec<f64> = receipts
        .iter()
        .filter_map(|r| match &r.outcome {
            Outcome::Answered { value } => value.cost_usd,
            Outcome::Inconclusive { .. } => None,
        })
        .collect();
    Summary {
        cases,
        accuracy: (cases > 0).then(|| correct as f64 / cases as f64),
        answered_accuracy: (answered > 0).then(|| correct as f64 / answered as f64),
        inconclusive_rate: (cases > 0).then(|| (cases - answered) as f64 / cases as f64),
        mean_latency_ms: (cases > 0)
            .then(|| receipts.iter().map(|r| r.elapsed_ms).sum::<f64>() / cases as f64),
        total_cost_usd: (cases > 0 && costs.len() == cases).then(|| costs.iter().sum()),
        known_cost_cases: costs.len(),
    }
}
