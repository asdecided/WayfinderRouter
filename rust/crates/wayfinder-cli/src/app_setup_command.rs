//! Non-interactive configuration bootstrap owned by the packaged macOS app.
//!
//! The bounded command keeps packaged setup independent of the launch working
//! directory and any external runtime.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{EXIT_OK, EXIT_USAGE, write_error, write_output};
use serde_json::Value;
use sha2::{Digest, Sha256};
use wayfinder_config::gateway::{ProviderKind, gateway_config_from_toml};
use wayfinder_config::{TierOrderPolicy, dump_routing_toml, routing_config_from_toml};
use wayfinder_routing_core::calibration::{ThresholdSample, calibrate_min_cost};
use wayfinder_routing_core::{Tier, score_complexity};

pub(crate) const APP_SETUP_HELP: &str =
    "usage: wayfinder-router app-setup-init --preset PRESET --path PATH";

const CHATGPT_MODEL_BLOCK: &str = r#"[gateway.models.chatgpt-sol]
provider = "codex-app-server"
model = "gpt-5.6-sol"
context_window = 1050000
"#;

pub(crate) fn run_configure_chatgpt(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let path = match parse_single_path(arguments, "app-configure-chatgpt") {
        Ok(path) => path,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };
    if !is_app_config_path(&path) {
        write_error(
            stderr,
            "wayfinder-router: app configuration path is outside Wayfinder Application Support",
        );
        return EXIT_USAGE;
    }
    match configure_chatgpt(&path) {
        Ok(changed) => {
            write_output(
                stdout,
                if changed {
                    "wayfinder-router: ChatGPT route configured"
                } else {
                    "wayfinder-router: ChatGPT route already configured"
                },
            );
            EXIT_OK
        }
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            EXIT_USAGE
        }
    }
}

fn parse_single_path(arguments: &[String], command: &str) -> Result<PathBuf, String> {
    if arguments.len() != 2 || arguments[0] != "--path" {
        return Err(format!("{command} requires exactly --path PATH"));
    }
    Ok(PathBuf::from(&arguments[1]))
}

fn is_app_config_path(path: &Path) -> bool {
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    path == PathBuf::from(home).join("Library/Application Support/Wayfinder/wayfinder-router.toml")
}

fn configure_chatgpt(path: &Path) -> Result<bool, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let parsed = gateway_config_from_toml(&source, &path.display().to_string())
        .map_err(|error| error.to_string())?;
    if parsed
        .models
        .values()
        .any(|model| model.provider == ProviderKind::CodexAppServer)
    {
        return Ok(false);
    }
    let separator = if source.is_empty() || source.ends_with("\n\n") {
        ""
    } else if source.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let edited = format!("{source}{separator}{CHATGPT_MODEL_BLOCK}");
    gateway_config_from_toml(&edited, &path.display().to_string())
        .map_err(|error| error.to_string())?;
    let temporary = path.with_extension("toml.wayfinder-new");
    fs::write(&temporary, edited)
        .map_err(|error| format!("cannot stage configuration: {error}"))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("cannot replace configuration: {error}"))?;
    Ok(true)
}

pub(crate) fn run_app_setup(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let options = match parse_options(arguments) {
        Ok(Some(options)) => options,
        Ok(None) => {
            write_output(stdout, APP_SETUP_HELP);
            return EXIT_OK;
        }
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };
    let config = match preset_config(&options.preset) {
        Ok(Some(config)) => config,
        Ok(None) => {
            write_error(
                stderr,
                &format!(
                    "wayfinder-router: unknown app setup preset '{}'",
                    options.preset
                ),
            );
            return EXIT_USAGE;
        }
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };
    if let Err(error) = write_new_config(&options.path, &config) {
        let message = if error.kind() == io::ErrorKind::AlreadyExists {
            format!("{} already exists", options.path.display())
        } else {
            format!("cannot write {}: {error}", options.path.display())
        };
        write_error(stderr, &format!("wayfinder-router: {message}"));
        return EXIT_USAGE;
    }
    write_output(
        stdout,
        &format!(
            "wayfinder-router: wrote {} (preset: {})",
            options.path.display(),
            options.preset
        ),
    );
    EXIT_OK
}

#[derive(Debug, PartialEq, Eq)]
struct AppSetupOptions {
    preset: String,
    path: PathBuf,
}

fn parse_options(arguments: &[String]) -> Result<Option<AppSetupOptions>, String> {
    if arguments
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        return Ok(None);
    }
    let mut preset = None;
    let mut path = None;
    let mut index = 0;
    while index < arguments.len() {
        let flag = &arguments[index];
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--preset" if preset.is_none() => preset = Some(value.clone()),
            "--path" if path.is_none() => path = Some(PathBuf::from(value)),
            "--preset" | "--path" => return Err(format!("{flag} may only be supplied once")),
            _ => return Err(format!("app-setup-init does not accept {flag}")),
        }
        index += 2;
    }
    Ok(Some(AppSetupOptions {
        preset: preset.ok_or_else(|| "app-setup-init needs --preset".to_owned())?,
        path: path.ok_or_else(|| "app-setup-init needs --path".to_owned())?,
    }))
}

pub(crate) fn write_new_config(path: &Path, contents: &str) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration path has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(contents.as_bytes())
}

pub(crate) fn preset_config(name: &str) -> Result<Option<String>, String> {
    match name {
        "apple-local" => Ok(Some(APPLE_LOCAL_CONFIG.to_owned())),
        "hybrid" => calibrated_automatic_preset("hybrid", HYBRID_CONFIG).map(Some),
        "local" => Ok(Some(LOCAL_CONFIG.to_owned())),
        "openai" => calibrated_automatic_preset("openai", OPENAI_CONFIG).map(Some),
        "gemini" => calibrated_automatic_preset("gemini", GEMINI_CONFIG).map(Some),
        _ => Ok(None),
    }
}

const STARTER_CORPUS: &str =
    include_str!("../../../../benchmarks/blind/openai-cross-provider.jsonl");
const STARTER_SEMANTIC_WEIGHT: f64 = 0.05;

fn calibrated_automatic_preset(name: &str, base: &str) -> Result<String, String> {
    let mut routing = routing_config_from_toml(base, name, None, TierOrderPolicy::StrictInput)
        .map_err(|error| format!("built-in {name} preset is invalid: {error}"))?;
    if routing.tiers.len() != 2 {
        return Err(format!("built-in {name} preset is not a two-arm policy"));
    }
    if !routing
        .weights
        .set("reasoning_term_count", STARTER_SEMANTIC_WEIGHT)
    {
        return Err("starter semantic feature mapping is unavailable".to_owned());
    }
    let rows = parse_starter_corpus()?;
    let mut samples = Vec::with_capacity(rows.len());
    for (prompt, requires_high) in &rows {
        let decision = score_complexity(prompt, &routing).map_err(|error| error.to_string())?;
        samples.push(ThresholdSample {
            score: decision.score,
            requires_high: *requires_high,
        });
    }
    let result = calibrate_min_cost(&samples, 0.0, 1.0, 2.0)
        .map_err(|error| format!("starter calibration failed: {error}"))?;
    let low_model = routing
        .tiers
        .first()
        .map(|tier| tier.model.clone())
        .ok_or_else(|| "starter low arm is missing".to_owned())?;
    let high_model = routing
        .tiers
        .get(1)
        .map(|tier| tier.model.clone())
        .ok_or_else(|| "starter high arm is missing".to_owned())?;
    routing.tiers = if result.threshold == 0.0 {
        vec![Tier::new(0.0, high_model)]
    } else {
        vec![
            Tier::new(0.0, low_model),
            Tier::new(result.threshold, high_model),
        ]
    };
    let routing_fragment = dump_routing_toml(&routing);
    let gateway_offset = base
        .find("[gateway")
        .ok_or_else(|| format!("built-in {name} preset has no gateway section"))?;
    let gateway = base
        .get(gateway_offset..)
        .ok_or_else(|| format!("built-in {name} preset boundary is invalid"))?;
    let digest = Sha256::digest(STARTER_CORPUS.as_bytes());
    let corpus_sha256 = format!("{digest:x}");
    let generated = format!(
        "# Generated by wayfinder-router init (preset: {name}).\n\
# wayfinder-starter-calibration: developer-v1\n\
# corpus: benchmarks/blind/openai-cross-provider.jsonl\n\
# corpus-sha256: {corpus_sha256}\n\
# samples: {} (94 hard, 60 easy)\n\
# objective: min-cost low=0 high=1 quality-penalty=2\n\
# quality-recovered: {:.4}; cost-savings: {:.4}\n\
{routing_fragment}\n{gateway}",
        rows.len(),
        result.quality_recovered,
        result.cost_savings,
    );
    routing_config_from_toml(
        &generated,
        &format!("generated {name} preset"),
        None,
        TierOrderPolicy::StrictInput,
    )
    .map_err(|error| format!("generated {name} preset is invalid: {error}"))?;
    gateway_config_from_toml(&generated, &format!("generated {name} preset"))
        .map_err(|error| format!("generated {name} gateway is invalid: {error}"))?;
    Ok(generated)
}

fn parse_starter_corpus() -> Result<Vec<(String, bool)>, String> {
    let mut rows = Vec::new();
    for (offset, line) in STARTER_CORPUS.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(|error| {
            format!(
                "built-in starter corpus line {} is invalid: {error}",
                offset + 1
            )
        })?;
        let prompt = value["prompt"]
            .as_str()
            .ok_or_else(|| format!("built-in starter corpus line {} has no prompt", offset + 1))?;
        let requires_high = match value["difficulty"].as_str() {
            Some("hard") => true,
            Some("easy") => false,
            _ => {
                return Err(format!(
                    "built-in starter corpus line {} has invalid difficulty",
                    offset + 1
                ));
            }
        };
        rows.push((prompt.to_owned(), requires_high));
    }
    if rows.len() != 154 {
        return Err(format!(
            "built-in starter corpus has {} rows, expected 154",
            rows.len()
        ));
    }
    Ok(rows)
}

const APPLE_LOCAL_CONFIG: &str = r#"# Generated by wayfinder-router init (preset: apple-local).
[[routing.tiers]]
min_score = 0.0
model = "apple-local"

[gateway]
offline = true

[gateway.models.apple-local]
provider = "apple-foundation-models"
model = "system-default"
tier = "local"
cost_per_1k = 0.0
"#;

const LOCAL_CONFIG: &str = r#"# Generated by wayfinder-router init (preset: local).
[routing]
threshold = 1.0

[gateway]
offline = true

[gateway.models.local]
base_url = "http://localhost:11434/v1"
model = "llama3.1"
cost_per_1k = 0.0
"#;

const HYBRID_CONFIG: &str = r#"# Generated by wayfinder-router init (preset: hybrid).
[routing]
threshold = 0.01

[gateway.models.local]
base_url = "http://localhost:11434/v1"
model = "llama3.1"
cost_per_1k = 0.0

[gateway.models.cloud]
base_url = "https://api.openai.com/v1"
model = "gpt-4o"
api_key_env = "OPENAI_API_KEY"
cost_per_1k = 0.0075
"#;

const OPENAI_CONFIG: &str = r#"# Generated by wayfinder-router init (preset: openai).
[[routing.tiers]]
min_score = 0.0
model = "small"

[[routing.tiers]]
min_score = 0.01
model = "large"

[gateway.models.small]
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
api_key_env = "OPENAI_API_KEY"
cost_per_1k = 0.0004

[gateway.models.large]
base_url = "https://api.openai.com/v1"
model = "gpt-4o"
api_key_env = "OPENAI_API_KEY"
cost_per_1k = 0.0075
"#;

const GEMINI_CONFIG: &str = r#"# Generated by wayfinder-router init (preset: gemini).
[[routing.tiers]]
min_score = 0.0
model = "flash"

[[routing.tiers]]
min_score = 0.01
model = "pro"

[gateway.models.flash]
base_url = "https://generativelanguage.googleapis.com/v1beta/openai"
model = "gemini-2.5-flash"
api_key_env = "GEMINI_API_KEY"
cost_per_1k = 0.0003

[gateway.models.pro]
base_url = "https://generativelanguage.googleapis.com/v1beta/openai"
model = "gemini-2.5-pro"
api_key_env = "GEMINI_API_KEY"
cost_per_1k = 0.005
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use wayfinder_config::TierOrderPolicy;
    use wayfinder_config::gateway::gateway_config_from_toml;
    use wayfinder_config::routing_config_from_toml;
    use wayfinder_routing_core::{DEFAULT_THRESHOLD, score_complexity};

    #[test]
    fn chatgpt_configuration_preserves_existing_document_and_is_idempotent() -> Result<(), String> {
        let root =
            std::env::temp_dir().join(format!("wayfinder-chatgpt-config-{}", std::process::id()));
        let path = root.join("wayfinder-router.toml");
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let original = "# keep this comment\n[gateway.models.apple-local]\nprovider = \"apple-foundation-models\"\nmodel = \"system-default\"\ntier = \"local\"\n";
        fs::write(&path, original).map_err(|error| error.to_string())?;
        assert!(configure_chatgpt(&path)?);
        let once = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        assert!(once.starts_with(original));
        assert!(once.contains("[gateway.models.chatgpt-sol]"));
        assert!(!configure_chatgpt(&path)?);
        assert_eq!(
            fs::read_to_string(&path).map_err(|error| error.to_string())?,
            once
        );
        fs::remove_dir_all(root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn every_app_preset_is_valid_for_both_config_parsers() -> Result<(), String> {
        for name in ["apple-local", "hybrid", "local", "openai", "gemini"] {
            let text = preset_config(name)?.ok_or_else(|| format!("missing preset {name}"))?;
            gateway_config_from_toml(&text, name).map_err(|error| error.to_string())?;
            routing_config_from_toml(&text, name, None, TierOrderPolicy::StrictInput)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    #[test]
    fn automatic_app_presets_use_the_developer_starter_cut() -> Result<(), String> {
        for name in ["hybrid", "openai", "gemini"] {
            let text = preset_config(name)?.ok_or_else(|| format!("missing preset {name}"))?;
            let routing = routing_config_from_toml(&text, name, None, TierOrderPolicy::StrictInput)
                .map_err(|error| error.to_string())?;
            assert_eq!(
                routing.tiers.get(1).map(|tier| tier.min_score),
                Some(DEFAULT_THRESHOLD),
                "{name}"
            );
            assert_eq!(routing.weights.get("reasoning_term_count"), Some(0.05));
            assert!(text.contains("wayfinder-starter-calibration: developer-v1"));
            assert_eq!(
                score_complexity("Prove the halting problem is undecidable", &routing)
                    .map_err(|error| error.to_string())?
                    .recommendation,
                routing
                    .tiers
                    .get(1)
                    .map(|tier| tier.model.as_str())
                    .unwrap_or("")
            );
        }
        let local =
            routing_config_from_toml(LOCAL_CONFIG, "local", None, TierOrderPolicy::StrictInput)
                .map_err(|error| error.to_string())?;
        assert_eq!(local.tiers.get(1).map(|tier| tier.min_score), Some(1.0));
        Ok(())
    }

    #[test]
    fn parser_requires_exact_bounded_arguments() {
        assert!(parse_options(&["--preset".to_owned()]).is_err());
        assert!(parse_options(&["--unknown".to_owned(), "x".to_owned()]).is_err());
        assert!(
            parse_options(&[
                "--preset".to_owned(),
                "local".to_owned(),
                "--preset".to_owned(),
                "hybrid".to_owned(),
                "--path".to_owned(),
                "/tmp/config".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn command_creates_parent_and_never_clobbers() -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("wayfinder-app-setup-test-{}", uuid::Uuid::new_v4()));
        let path = root.join("nested").join("wayfinder-router.toml");
        let arguments = vec![
            "--preset".to_owned(),
            "apple-local".to_owned(),
            "--path".to_owned(),
            path.display().to_string(),
        ];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(run_app_setup(&arguments, &mut stdout, &mut stderr), EXIT_OK);
        let original = fs::read_to_string(&path)?;
        assert!(original.contains("apple-foundation-models"));
        assert_eq!(
            run_app_setup(&arguments, &mut stdout, &mut stderr),
            EXIT_USAGE
        );
        assert_eq!(fs::read_to_string(&path)?, original);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
