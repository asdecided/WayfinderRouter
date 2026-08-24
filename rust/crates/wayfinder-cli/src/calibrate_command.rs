use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;
use wayfinder_config::{TierOrderPolicy, dump_routing_toml, routing_config_from_toml};
use wayfinder_routing_core::calibration::{
    MinCostCalibration, ThresholdSample, calibrate_min_cost,
};
use wayfinder_routing_core::{Lexicon, RoutingConfig, Tier, lexical_terms, score_complexity};

use super::{EXIT_CONFIG, EXIT_OK, EXIT_USAGE, write_error, write_output};

const CALIBRATE_HELP: &str = "usage: wayfinder-router calibrate DATASET [--mode threshold] [--objective min-cost] --costs LABEL=COST,LABEL=COST [--quality-penalty COST] [--distill-lexicon] [--config PATH] [--out PATH]\n\nFit a deterministic price-sensitive threshold from JSONL rows with text and label fields. --distill-lexicon learns a bounded static high-arm vocabulary from the labelled dataset; request-time routing remains offline and model-free. TOML is written to stdout unless --out is supplied.";
const MAX_DISTILLED_TERMS: usize = 128;
const MIN_DISTILL_ROWS_PER_ARM: usize = 4;
const SEMANTIC_WEIGHT_CANDIDATES: [f64; 7] = [0.0, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0];

#[derive(Debug, PartialEq)]
struct CalibrateOptions {
    dataset: String,
    costs: String,
    quality_penalty: Option<f64>,
    config: Option<PathBuf>,
    out: Option<PathBuf>,
    distill_lexicon: bool,
}

#[derive(Debug, PartialEq)]
struct LabelledPrompt {
    text: String,
    label: String,
}

pub(crate) fn run_calibrate(
    arguments: &[String],
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let options = match parse_options(arguments) {
        Ok(Some(options)) => options,
        Ok(None) => {
            write_output(stdout, CALIBRATE_HELP);
            return EXIT_OK;
        }
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };

    let dataset_text = match read_dataset(&options.dataset, stdin) {
        Ok(text) => text,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };
    let prompts = match parse_dataset(&dataset_text) {
        Ok(prompts) => prompts,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };
    let labels = prompts
        .iter()
        .map(|prompt| prompt.label.clone())
        .collect::<BTreeSet<_>>();
    if labels.len() != 2 {
        write_error(
            stderr,
            &format!(
                "wayfinder-router: calibrate needs exactly two labels, found {}",
                labels.len()
            ),
        );
        return EXIT_USAGE;
    }

    let costs = match parse_costs(&options.costs) {
        Ok(costs) => costs,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };
    let cost_labels = costs.keys().cloned().collect::<BTreeSet<_>>();
    if cost_labels != labels {
        write_error(
            stderr,
            "wayfinder-router: --costs labels must exactly match the dataset labels",
        );
        return EXIT_USAGE;
    }
    let Some((low_label, low_cost, high_label, high_cost)) = ordered_arms(&costs) else {
        write_error(
            stderr,
            "wayfinder-router: --costs must define two different costs",
        );
        return EXIT_USAGE;
    };
    let quality_penalty = options.quality_penalty.unwrap_or(high_cost);

    let mut scoring_config = match load_scoring_config(options.config.as_deref()) {
        Ok(config) => config,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_CONFIG;
        }
    };
    let mut distilled_terms = 0_usize;
    let result = if options.distill_lexicon {
        let terms = match distill_reasoning_lexicon(&prompts, &high_label) {
            Ok(terms) => terms,
            Err(message) => {
                write_error(stderr, &format!("wayfinder-router: {message}"));
                return EXIT_USAGE;
            }
        };
        distilled_terms = terms.len();
        let constraints = scoring_config
            .lexicon
            .constraint_terms()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        scoring_config.lexicon = Lexicon::new(terms, constraints);
        match select_semantic_weight(
            &prompts,
            &high_label,
            &scoring_config,
            low_cost,
            high_cost,
            quality_penalty,
        ) {
            Ok((result, weight)) => {
                let _ = scoring_config.weights.set("reasoning_term_count", weight);
                result
            }
            Err(message) => {
                write_error(stderr, &format!("wayfinder-router: {message}"));
                return EXIT_USAGE;
            }
        }
    } else {
        match calibrate_config(
            &prompts,
            &high_label,
            &scoring_config,
            low_cost,
            high_cost,
            quality_penalty,
        ) {
            Ok(result) => result,
            Err(message) => {
                write_error(stderr, &format!("wayfinder-router: {message}"));
                return EXIT_USAGE;
            }
        }
    };
    let tiers = if result.threshold == 0.0 {
        vec![Tier::new(0.0, &high_label).with_cost(high_cost)]
    } else {
        vec![
            Tier::new(0.0, &low_label).with_cost(low_cost),
            Tier::new(result.threshold, &high_label).with_cost(high_cost),
        ]
    };
    let output_config = RoutingConfig {
        weights: scoring_config.weights.clone(),
        tiers,
        classifier: None,
        lexicon: scoring_config.lexicon.clone(),
    };
    let toml = dump_routing_toml(&output_config);
    if let Err(error) = routing_config_from_toml(
        &toml,
        "generated calibration",
        None,
        TierOrderPolicy::StrictInput,
    ) {
        write_error(
            stderr,
            &format!("wayfinder-router: generated calibration is invalid: {error}"),
        );
        return EXIT_CONFIG;
    }

    if let Some(path) = options
        .out
        .as_deref()
        .filter(|path| *path != Path::new("-"))
    {
        if let Err(message) = write_new_file(path, toml.as_bytes()) {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_CONFIG;
        }
    } else if stdout.write_all(toml.as_bytes()).is_err() {
        write_error(stderr, "wayfinder-router: cannot write calibration output");
        return EXIT_CONFIG;
    }

    write_error(
        stderr,
        &format!(
            "wayfinder-router: calibrated {} prompts: low={} ({:.6}), high={} ({:.6}), threshold={:.4}, Q={:.6}, expected_cost={:.6}, expected_loss={:.6}, quality_recovered={:.4}, cost_savings={:.4}, semantic_terms={}, semantic_weight={:.6}",
            prompts.len(),
            low_label,
            low_cost,
            high_label,
            high_cost,
            result.threshold,
            quality_penalty,
            result.expected_money_cost,
            result.expected_loss,
            result.quality_recovered,
            result.cost_savings,
            distilled_terms,
            scoring_config
                .weights
                .get("reasoning_term_count")
                .unwrap_or(0.0),
        ),
    );
    EXIT_OK
}

fn parse_options(arguments: &[String]) -> Result<Option<CalibrateOptions>, String> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(None);
    }
    let mut dataset = None;
    let mut costs = None;
    let mut quality_penalty = None;
    let mut config = None;
    let mut out = None;
    let mut distill_lexicon = false;
    let mut index = 0_usize;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--mode" => {
                index = index.saturating_add(1);
                let value = required_value(arguments, index, "--mode")?;
                if value != "threshold" {
                    return Err("native calibrate supports only --mode threshold".to_owned());
                }
            }
            value if value.starts_with("--mode=") => {
                if value.trim_start_matches("--mode=") != "threshold" {
                    return Err("native calibrate supports only --mode threshold".to_owned());
                }
            }
            "--objective" => {
                index = index.saturating_add(1);
                let value = required_value(arguments, index, "--objective")?;
                if value != "min-cost" {
                    return Err("native calibrate supports only --objective min-cost".to_owned());
                }
            }
            value if value.starts_with("--objective=") => {
                if value.trim_start_matches("--objective=") != "min-cost" {
                    return Err("native calibrate supports only --objective min-cost".to_owned());
                }
            }
            "--costs" => {
                index = index.saturating_add(1);
                costs = Some(required_value(arguments, index, "--costs")?.to_owned());
            }
            value if value.starts_with("--costs=") => {
                costs = Some(value.trim_start_matches("--costs=").to_owned());
            }
            "--quality-penalty" => {
                index = index.saturating_add(1);
                let value = required_value(arguments, index, "--quality-penalty")?;
                quality_penalty = Some(parse_non_negative(value, "--quality-penalty")?);
            }
            value if value.starts_with("--quality-penalty=") => {
                quality_penalty = Some(parse_non_negative(
                    value.trim_start_matches("--quality-penalty="),
                    "--quality-penalty",
                )?);
            }
            "--config" => {
                index = index.saturating_add(1);
                config = Some(PathBuf::from(required_value(arguments, index, "--config")?));
            }
            value if value.starts_with("--config=") => {
                config = Some(PathBuf::from(value.trim_start_matches("--config=")));
            }
            "--out" => {
                index = index.saturating_add(1);
                out = Some(PathBuf::from(required_value(arguments, index, "--out")?));
            }
            value if value.starts_with("--out=") => {
                out = Some(PathBuf::from(value.trim_start_matches("--out=")));
            }
            "--distill-lexicon" => distill_lexicon = true,
            value if value.starts_with('-') && value != "-" => {
                return Err(format!("unrecognized calibrate argument: {value}"));
            }
            value => {
                if dataset.replace(value.to_owned()).is_some() {
                    return Err("calibrate accepts exactly one dataset file or '-'".to_owned());
                }
            }
        }
        index = index.saturating_add(1);
    }
    Ok(Some(CalibrateOptions {
        dataset: dataset.ok_or_else(|| "calibrate needs a dataset file or '-'".to_owned())?,
        costs: costs.ok_or_else(|| "calibrate needs --costs LABEL=COST,LABEL=COST".to_owned())?,
        quality_penalty,
        config,
        out,
        distill_lexicon,
    }))
}

fn calibrate_config(
    prompts: &[LabelledPrompt],
    high_label: &str,
    config: &RoutingConfig,
    low_cost: f64,
    high_cost: f64,
    quality_penalty: f64,
) -> Result<MinCostCalibration, String> {
    let mut samples = Vec::with_capacity(prompts.len());
    for prompt in prompts {
        let scored = score_complexity(&prompt.text, config).map_err(|error| error.to_string())?;
        samples.push(ThresholdSample {
            score: scored.score,
            requires_high: prompt.label == high_label,
        });
    }
    calibrate_min_cost(&samples, low_cost, high_cost, quality_penalty)
        .map_err(|error| error.to_string())
}

fn select_semantic_weight(
    prompts: &[LabelledPrompt],
    high_label: &str,
    config: &RoutingConfig,
    low_cost: f64,
    high_cost: f64,
    quality_penalty: f64,
) -> Result<(MinCostCalibration, f64), String> {
    let mut best: Option<(MinCostCalibration, f64)> = None;
    for weight in SEMANTIC_WEIGHT_CANDIDATES {
        let mut candidate = config.clone();
        if !candidate.weights.set("reasoning_term_count", weight) {
            return Err("semantic feature mapping is unavailable".to_owned());
        }
        let result = calibrate_config(
            prompts,
            high_label,
            &candidate,
            low_cost,
            high_cost,
            quality_penalty,
        )?;
        let replace = best.as_ref().is_none_or(|(current, _)| {
            result.expected_loss < current.expected_loss - 1e-12
                || ((result.expected_loss - current.expected_loss).abs() <= 1e-12
                    && result.quality_recovered > current.quality_recovered + 1e-12)
        });
        if replace {
            best = Some((result, weight));
        }
    }
    best.ok_or_else(|| "semantic calibration produced no candidate".to_owned())
}

fn distill_reasoning_lexicon(
    prompts: &[LabelledPrompt],
    high_label: &str,
) -> Result<Vec<String>, String> {
    let high_rows = prompts
        .iter()
        .filter(|prompt| prompt.label == high_label)
        .count();
    let low_rows = prompts.len().saturating_sub(high_rows);
    if high_rows < MIN_DISTILL_ROWS_PER_ARM || low_rows < MIN_DISTILL_ROWS_PER_ARM {
        return Err(format!(
            "--distill-lexicon needs at least {MIN_DISTILL_ROWS_PER_ARM} rows per arm"
        ));
    }
    let mut high_counts = BTreeMap::<String, u64>::new();
    let mut low_counts = BTreeMap::<String, u64>::new();
    for prompt in prompts {
        let terms = lexical_terms(&prompt.text).map_err(|error| error.to_string())?;
        let counts = if prompt.label == high_label {
            &mut high_counts
        } else {
            &mut low_counts
        };
        for term in terms {
            if eligible_distilled_term(&term) {
                let count = counts.entry(term).or_insert(0);
                *count = count.saturating_add(1);
            }
        }
    }
    let mut candidates = high_counts
        .into_iter()
        .filter(|(term, high_count)| {
            *high_count >= 2 && low_counts.get(term).copied().unwrap_or(0) == 0
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    candidates.truncate(MAX_DISTILLED_TERMS);
    if candidates.is_empty() {
        return Err(
            "--distill-lexicon found no term repeated in the high arm and absent from the low arm"
                .to_owned(),
        );
    }
    let mut terms = candidates
        .into_iter()
        .map(|(term, _)| term)
        .collect::<Vec<_>>();
    terms.sort();
    Ok(terms)
}

fn eligible_distilled_term(term: &str) -> bool {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "been", "being", "by", "can", "could",
        "did", "do", "does", "for", "from", "had", "has", "have", "he", "her", "hers",
        "him", "his", "how", "i", "in", "into", "is", "it", "its", "may", "might", "my",
        "no", "not", "of", "on", "or", "our", "ours", "shall", "she", "should", "than",
        "that", "the", "their", "theirs", "them", "then", "there", "these", "they", "this",
        "those", "to", "under", "was", "we", "were", "what", "when", "where", "which", "who",
        "why", "will", "with", "would", "you", "your", "yours",
    ];
    (3..=48).contains(&term.len()) && !STOP_WORDS.contains(&term)
}

fn required_value<'a>(
    arguments: &'a [String],
    index: usize,
    option: &str,
) -> Result<&'a str, String> {
    arguments
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("{option} needs a value"))
}

fn parse_non_negative(raw: &str, name: &str) -> Result<f64, String> {
    let value = raw
        .parse::<f64>()
        .map_err(|_| format!("{name} must be a finite non-negative number"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("{name} must be a finite non-negative number"));
    }
    Ok(value)
}

fn read_dataset(path: &str, stdin: &mut dyn Read) -> Result<String, String> {
    if path == "-" {
        let mut text = String::new();
        stdin
            .read_to_string(&mut text)
            .map_err(|error| format!("cannot read dataset from stdin: {error}"))?;
        return Ok(text);
    }
    fs::read_to_string(path).map_err(|error| format!("cannot read dataset {path}: {error}"))
}

fn parse_dataset(text: &str) -> Result<Vec<LabelledPrompt>, String> {
    let mut prompts = Vec::new();
    for (offset, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_number = offset.saturating_add(1);
        let value: Value = serde_json::from_str(line)
            .map_err(|error| format!("dataset line {line_number} is invalid JSON: {error}"))?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("dataset line {line_number} must be a JSON object"))?;
        let text = object
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("dataset line {line_number} needs a string 'text'"))?;
        let label = object
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| !label.trim().is_empty())
            .ok_or_else(|| {
                format!("dataset line {line_number} needs a non-empty string 'label'")
            })?;
        let label = label.trim();
        prompts.push(LabelledPrompt {
            text: text.to_owned(),
            label: label.to_owned(),
        });
    }
    if prompts.is_empty() {
        return Err("calibration dataset is empty".to_owned());
    }
    Ok(prompts)
}

fn parse_costs(raw: &str) -> Result<BTreeMap<String, f64>, String> {
    let mut costs = BTreeMap::new();
    for assignment in raw.split(',') {
        let (label, raw_cost) = assignment
            .split_once('=')
            .ok_or_else(|| "--costs must use LABEL=COST,LABEL=COST".to_owned())?;
        let label = label.trim();
        if label.is_empty() {
            return Err("--costs labels must be non-empty".to_owned());
        }
        let cost = parse_non_negative(raw_cost.trim(), "--costs values")?;
        if costs.insert(label.to_owned(), cost).is_some() {
            return Err(format!("--costs repeats label {label:?}"));
        }
    }
    if costs.len() != 2 {
        return Err("--costs must define exactly two labels".to_owned());
    }
    Ok(costs)
}

fn ordered_arms(costs: &BTreeMap<String, f64>) -> Option<(String, f64, String, f64)> {
    let mut entries = costs
        .iter()
        .map(|(label, cost)| (label.clone(), *cost))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    let low = entries.first()?;
    let high = entries.get(1)?;
    (low.1 < high.1).then(|| (low.0.clone(), low.1, high.0.clone(), high.1))
}

fn load_scoring_config(path: Option<&Path>) -> Result<RoutingConfig, String> {
    let Some(path) = path else {
        return Ok(RoutingConfig::default());
    };
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read scoring config {}: {error}", path.display()))?;
    routing_config_from_toml(
        &text,
        &path.display().to_string(),
        None,
        TierOrderPolicy::StrictInput,
    )
    .map_err(|error| error.to_string())
}

fn write_new_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create output {}: {error}", path.display()))?;
    file.write_all(contents)
        .map_err(|error| format!("cannot write output {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync output {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn dataset() -> &'static str {
        "{\"text\":\"hello\",\"label\":\"local\"}\n\
         {\"text\":\"# Plan\\n\\n- prove the theorem\\n- preserve every constraint\\n- derive the result\",\"label\":\"cloud\"}\n"
    }

    #[test]
    fn native_min_cost_command_emits_loadable_costed_toml() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut stdin = dataset().as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_calibrate(
            &[
                "-".to_owned(),
                "--mode=threshold".to_owned(),
                "--objective=min-cost".to_owned(),
                "--costs=local=0.2,cloud=1.0".to_owned(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&stderr));
        let rendered = String::from_utf8(stdout)?;
        let parsed =
            routing_config_from_toml(&rendered, "test output", None, TierOrderPolicy::StrictInput)?;
        assert!(parsed.classifier.is_none());
        assert!(parsed.tiers.iter().all(|tier| tier.cost.is_some()));
        let summary = String::from_utf8(stderr)?;
        assert!(summary.contains("Q=1.000000"));
        assert!(summary.contains("expected_loss="));
        Ok(())
    }

    #[test]
    fn zero_threshold_emits_a_single_high_arm_tier() -> Result<(), Box<dyn std::error::Error>> {
        let inverted = "{\"text\":\"# Plan\\n\\n- prove the theorem\\n- preserve every constraint\\n- derive the result\",\"label\":\"local\"}\n\
                        {\"text\":\"hello\",\"label\":\"cloud\"}\n";
        let mut stdin = inverted.as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_calibrate(
            &[
                "-".to_owned(),
                "--costs=local=0.2,cloud=1.0".to_owned(),
                "--quality-penalty=100".to_owned(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&stderr));
        let rendered = String::from_utf8(stdout)?;
        let parsed =
            routing_config_from_toml(&rendered, "test output", None, TierOrderPolicy::StrictInput)?;
        assert_eq!(parsed.tiers.len(), 1);
        assert_eq!(parsed.tiers[0].min_score, 0.0);
        assert_eq!(parsed.tiers[0].model, "cloud");
        Ok(())
    }

    #[test]
    fn command_rejects_non_native_objectives() {
        let mut stdin = dataset().as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_calibrate(
            &[
                "-".to_owned(),
                "--objective=knee".to_owned(),
                "--costs=local=0.2,cloud=1.0".to_owned(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_USAGE);
        assert!(stdout.is_empty());
        assert!(String::from_utf8_lossy(&stderr).contains("only --objective min-cost"));
    }

    #[test]
    fn command_rejects_cost_labels_that_do_not_match_dataset() {
        let mut stdin = dataset().as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_calibrate(
            &["-".to_owned(), "--costs=cheap=0.2,strong=1.0".to_owned()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_USAGE);
        assert!(String::from_utf8_lossy(&stderr).contains("exactly match"));
    }

    #[test]
    fn semantic_distillation_emits_a_static_model_free_signal()
    -> Result<(), Box<dyn std::error::Error>> {
        let semantic_dataset = concat!(
            "{\"text\":\"What is the capital of Canada?\",\"label\":\"local\"}\n",
            "{\"text\":\"Convert 72 Fahrenheit to Celsius.\",\"label\":\"local\"}\n",
            "{\"text\":\"List the days of the week.\",\"label\":\"local\"}\n",
            "{\"text\":\"Define photosynthesis in one sentence.\",\"label\":\"local\"}\n",
            "{\"text\":\"Prove the compactness theorem.\",\"label\":\"cloud\"}\n",
            "{\"text\":\"Prove the fixed point theorem.\",\"label\":\"cloud\"}\n",
            "{\"text\":\"Prove the theorem by contradiction.\",\"label\":\"cloud\"}\n",
            "{\"text\":\"Prove the theorem using induction.\",\"label\":\"cloud\"}\n",
        );
        let mut stdin = semantic_dataset.as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_calibrate(
            &[
                "-".to_owned(),
                "--costs=local=0,cloud=1".to_owned(),
                "--quality-penalty=2".to_owned(),
                "--distill-lexicon".to_owned(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&stderr));
        let rendered = String::from_utf8(stdout)?;
        assert!(rendered.contains("reasoning_terms"));
        assert!(rendered.contains("\"prove\""));
        let parsed = routing_config_from_toml(
            &rendered,
            "semantic output",
            None,
            TierOrderPolicy::StrictInput,
        )?;
        let decision = score_complexity("Prove the halting problem is undecidable", &parsed)?;
        assert_eq!(decision.recommendation, "cloud");
        let summary = String::from_utf8(stderr)?;
        assert!(summary.contains("semantic_terms="));
        assert!(!summary.contains("semantic_terms=0"));
        Ok(())
    }

    #[test]
    fn out_refuses_to_replace_an_existing_file() -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("wayfinder-calibrate-{}", Uuid::new_v4()));
        fs::write(&path, "keep me")?;
        let mut stdin = dataset().as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_calibrate(
            &[
                "-".to_owned(),
                "--costs=local=0.2,cloud=1.0".to_owned(),
                "--out".to_owned(),
                path.display().to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_CONFIG);
        assert_eq!(fs::read_to_string(&path)?, "keep me");
        fs::remove_file(path)?;
        Ok(())
    }
}
