//! Minimal dependency-free routing microbenchmark.
//!
//! This is not a pass/fail test. It runs the checked-in migration corpus
//! through the authoritative Rust kernel in one process for repeatable
//! warm-loop measurements.

use std::error::Error;
use std::hint::black_box;
use std::time::Instant;

use serde::Deserialize;
use serde_json::json;
use wayfinder_routing_core::{RoutingConfig, score_complexity};

#[derive(Deserialize)]
struct Fixture {
    text: String,
}

fn parse_iterations() -> Result<usize, Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let mut iterations = 5_000_usize;
    while let Some(argument) = arguments.next() {
        if argument != "--iterations" {
            return Err(format!("unknown argument: {argument}").into());
        }
        let value = arguments
            .next()
            .ok_or("--iterations requires a positive integer")?;
        iterations = value.parse()?;
        if iterations == 0 {
            return Err("--iterations must be positive".into());
        }
    }
    Ok(iterations)
}

fn exercise(
    corpus: &[Fixture],
    config: &RoutingConfig,
    iterations: usize,
) -> Result<f64, Box<dyn Error>> {
    let mut checksum = 0.0;
    for _ in 0..iterations {
        for fixture in corpus {
            let decision = score_complexity(black_box(&fixture.text), config)?;
            checksum += black_box(decision.score);
        }
    }
    Ok(checksum)
}

fn main() -> Result<(), Box<dyn Error>> {
    let iterations = parse_iterations()?;
    let corpus: Vec<Fixture> =
        serde_json::from_str(include_str!("../../fixtures/migration-golden.json"))?;
    let config = RoutingConfig::default();
    let _ = exercise(&corpus, &config, 100)?;

    let started = Instant::now();
    let checksum = exercise(&corpus, &config, iterations)?;
    let elapsed = started.elapsed().as_secs_f64();
    let operations = iterations.saturating_mul(corpus.len());
    let operations_per_second = operations as f64 / elapsed;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "implementation": "rust",
            "corpus_cases": corpus.len(),
            "iterations": iterations,
            "operations": operations,
            "elapsed_seconds": elapsed,
            "operations_per_second": operations_per_second,
            "checksum": checksum,
        }))?
    );
    Ok(())
}
