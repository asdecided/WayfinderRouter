use std::{error::Error, time::Duration};
use wayfinder_decision_experiment::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut options = ExperimentOptions::default();
    let mut doubles = false;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--contract-doubles" => doubles = true,
            "--allow-external" => options.allow_external = true,
            "--disclose-prompt" => options.context_fields.push("prompt".into()),
            _ => return Err(format!("unknown option {argument}").into()),
        }
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!("../fixtures/cases.json"))?;
    let mut engines: Vec<Box<dyn DecisionEngine>> = vec![Box::new(DeterministicEngine)];
    if doubles {
        for name in ["luna-contract-double", "decisions-contract-double"] {
            engines.push(Box::new(ContractDouble {
                name: name.into(),
                delay: Duration::ZERO,
                response: Ok(Answer {
                    answer: "local".into(),
                    provider_request_id: Some("synthetic-request".into()),
                    model_version: Some("synthetic-v1".into()),
                    confidence: None,
                    distribution: None,
                    cost_usd: None,
                }),
            }));
        }
    }
    let mut reports = Vec::new();
    for engine in engines {
        let mut receipts = Vec::new();
        for case in &cases {
            receipts.push(
                evaluate(
                    engine.as_ref(),
                    case,
                    POLICY_VERSION,
                    &options,
                    Duration::from_secs(2),
                )
                .await,
            );
        }
        reports.push(serde_json::json!({"engine": engine.identity(), "summary": summarize(&receipts), "receipts": receipts}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": "decision-experiment-v1", "fixture_version": "routing-smoke-v1", "reports": reports
        }))?
    );
    Ok(())
}
