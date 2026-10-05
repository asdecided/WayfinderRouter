use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use wayfinder_decision_experiment::*;

fn case() -> Case {
    Case {
        id: "case-1".into(),
        question: "Which tier?".into(),
        answers: vec!["local".into(), "cloud".into()],
        context: BTreeMap::from([("prompt".into(), "Hi".into())]),
        expected: "local".into(),
    }
}
fn options() -> ExperimentOptions {
    ExperimentOptions {
        allow_external: true,
        context_fields: vec!["prompt".into()],
    }
}
fn answer() -> Answer {
    Answer {
        answer: "local".into(),
        provider_request_id: Some("provider-123".into()),
        model_version: Some("fixture-v1".into()),
        confidence: None,
        distribution: None,
        cost_usd: None,
    }
}
fn double(result: Result<Answer, Failure>) -> ContractDouble {
    ContractDouble {
        name: "fixture".into(),
        response: result,
        delay: Duration::ZERO,
    }
}
fn reason(receipt: &Receipt) -> Option<&Failure> {
    match &receipt.outcome {
        Outcome::Inconclusive { reason } => Some(reason),
        _ => None,
    }
}

struct Spy(AtomicUsize);
impl DecisionEngine for Spy {
    fn identity(&self) -> EngineIdentity {
        double(Ok(answer())).identity()
    }
    fn decide<'a>(&'a self, request: DisclosedRequest) -> EngineFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            assert_eq!(request.context.len(), 1);
            assert_eq!(
                request.context.get("prompt").map(String::as_str),
                Some("Hi")
            );
            Ok(answer())
        })
    }
}

#[tokio::test]
async fn external_io_requires_opt_in_and_complete_context_consent() {
    let spy = Spy(AtomicUsize::new(0));
    let c = case();
    let r = evaluate(
        &spy,
        &c,
        POLICY_VERSION,
        &ExperimentOptions::default(),
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(reason(&r), Some(&Failure::Disabled));
    let denied = ExperimentOptions {
        allow_external: true,
        context_fields: vec![],
    };
    let r = evaluate(&spy, &c, POLICY_VERSION, &denied, Duration::from_secs(1)).await;
    assert_eq!(reason(&r), Some(&Failure::ContextDenied));
    assert_eq!(spy.0.load(Ordering::SeqCst), 0);
    let r = evaluate(&spy, &c, POLICY_VERSION, &options(), Duration::from_secs(1)).await;
    assert_eq!(r.correct, Some(true));
    assert_eq!(spy.0.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn timeout_and_provider_errors_stay_inconclusive_without_fallback() {
    let mut engine = double(Ok(answer()));
    engine.delay = Duration::from_secs(20);
    let r = evaluate(
        &engine,
        &case(),
        POLICY_VERSION,
        &options(),
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(reason(&r), Some(&Failure::Timeout));
    assert_eq!(r.correct, None);
    for failure in [
        Failure::Unavailable,
        Failure::Abstained,
        Failure::MalformedResponse,
        Failure::ProviderError,
    ] {
        let engine = double(Err(failure.clone()));
        let r = evaluate(
            &engine,
            &case(),
            POLICY_VERSION,
            &options(),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(reason(&r), Some(&failure));
    }
}

#[tokio::test]
async fn ids_and_optional_evidence_are_not_fabricated() {
    let engine = double(Ok(answer()));
    let a = evaluate(
        &engine,
        &case(),
        POLICY_VERSION,
        &options(),
        Duration::from_secs(1),
    )
    .await;
    let b = evaluate(
        &engine,
        &case(),
        POLICY_VERSION,
        &options(),
        Duration::from_secs(1),
    )
    .await;
    assert_ne!(a.decision_id, b.decision_id);
    assert_ne!(a.decision_id, "provider-123");
    assert_eq!(a.policy_version, POLICY_VERSION);
    assert!(a.engine.test_double);
    assert!(matches!(a.outcome, Outcome::Answered { .. }));
    if let Outcome::Answered { value } = a.outcome {
        assert_eq!(value.provider_request_id.as_deref(), Some("provider-123"));
        assert_eq!(value.confidence, None);
        assert_eq!(value.distribution, None);
        assert_eq!(value.cost_usd, None);
    }
}

#[tokio::test]
async fn invalid_answers_probabilities_and_cost_are_rejected() {
    let mut variants = Vec::new();
    let mut bad = answer();
    bad.answer = "unknown".into();
    variants.push(bad);
    let mut bad = answer();
    bad.confidence = Some(f64::NAN);
    variants.push(bad);
    let mut bad = answer();
    bad.cost_usd = Some(-1.0);
    variants.push(bad);
    let mut bad = answer();
    bad.distribution = Some(BTreeMap::from([
        ("local".into(), 0.2),
        ("cloud".into(), 0.1),
    ]));
    variants.push(bad);
    for bad in variants {
        let r = evaluate(
            &double(Ok(bad)),
            &case(),
            POLICY_VERSION,
            &options(),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(reason(&r), Some(&Failure::MalformedResponse));
    }
}

#[tokio::test]
async fn comparison_counts_inconclusive_cases_and_keeps_unknown_cost_unknown() {
    let good = evaluate(
        &double(Ok(answer())),
        &case(),
        POLICY_VERSION,
        &options(),
        Duration::from_secs(1),
    )
    .await;
    let bad = evaluate(
        &double(Err(Failure::Unavailable)),
        &case(),
        POLICY_VERSION,
        &options(),
        Duration::from_secs(1),
    )
    .await;
    let summary = summarize(&[good, bad]);
    assert_eq!(summary.accuracy, Some(0.5));
    assert_eq!(summary.answered_accuracy, Some(1.0));
    assert_eq!(summary.inconclusive_rate, Some(0.5));
    assert_eq!(summary.total_cost_usd, None);
    assert_eq!(summarize(&[]).accuracy, None);
}

#[tokio::test]
async fn deterministic_baseline_uses_existing_scorer_and_needs_no_external_consent() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../fixtures/cases.json")).unwrap_or_default();
    assert_eq!(cases.len(), 4);
    for c in cases {
        let r = evaluate(
            &DeterministicEngine,
            &c,
            POLICY_VERSION,
            &ExperimentOptions::default(),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(r.correct, Some(true), "{}", c.id);
        assert!(!r.engine.external);
    }
}
