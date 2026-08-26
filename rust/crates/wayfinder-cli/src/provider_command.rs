//! Reviewable hosted-provider destination fragments.

use std::io::Write;

use crate::{EXIT_OK, EXIT_USAGE, write_error, write_output};

const HELP: &str = "usage: wayfinder-router provider presets\n       wayfinder-router provider preset PROVIDER --model MODEL [--id ID]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HostedProviderPreset {
    id: &'static str,
    base_url: &'static str,
    api_key_env: &'static str,
}

const HOSTED_PROVIDER_PRESETS: &[HostedProviderPreset] = &[
    HostedProviderPreset {
        id: "openai",
        base_url: "https://api.openai.com/v1",
        api_key_env: "OPENAI_API_KEY",
    },
    HostedProviderPreset {
        id: "gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        api_key_env: "GEMINI_API_KEY",
    },
    HostedProviderPreset {
        id: "openrouter",
        base_url: "https://openrouter.ai/api/v1",
        api_key_env: "OPENROUTER_API_KEY",
    },
    HostedProviderPreset {
        id: "groq",
        base_url: "https://api.groq.com/openai/v1",
        api_key_env: "GROQ_API_KEY",
    },
    HostedProviderPreset {
        id: "deepseek",
        base_url: "https://api.deepseek.com",
        api_key_env: "DEEPSEEK_API_KEY",
    },
    HostedProviderPreset {
        id: "together",
        base_url: "https://api.together.ai/v1",
        api_key_env: "TOGETHER_API_KEY",
    },
    HostedProviderPreset {
        id: "fireworks",
        base_url: "https://api.fireworks.ai/inference/v1",
        api_key_env: "FIREWORKS_API_KEY",
    },
    HostedProviderPreset {
        id: "cerebras",
        base_url: "https://api.cerebras.ai/v1",
        api_key_env: "CEREBRAS_API_KEY",
    },
    HostedProviderPreset {
        id: "xai",
        base_url: "https://api.x.ai/v1",
        api_key_env: "XAI_API_KEY",
    },
    HostedProviderPreset {
        id: "mistral",
        base_url: "https://api.mistral.ai/v1",
        api_key_env: "MISTRAL_API_KEY",
    },
];

pub(crate) fn run_provider(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        write_output(stdout, HELP);
        return EXIT_OK;
    }
    match arguments.first().map(String::as_str) {
        Some("presets") if arguments.len() == 1 => {
            for preset in HOSTED_PROVIDER_PRESETS {
                write_output(stdout, preset.id);
            }
            EXIT_OK
        }
        Some("presets") => usage_error(stderr, "provider presets accepts no arguments"),
        Some("preset") => run_preset(&arguments[1..], stdout, stderr),
        Some(action) => usage_error(stderr, &format!("unsupported provider action: {action}")),
        None => usage_error(stderr, "provider needs the 'preset' or 'presets' action"),
    }
}

fn run_preset(arguments: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Some(provider_id) = arguments.first() else {
        return usage_error(stderr, "provider preset needs a provider");
    };
    let Some(preset) = HOSTED_PROVIDER_PRESETS
        .iter()
        .find(|preset| preset.id == provider_id)
    else {
        return usage_error(
            stderr,
            &format!("unknown hosted provider preset: {provider_id}"),
        );
    };
    let mut model = None;
    let mut destination_id = None;
    let mut index = 1;
    while let Some(argument) = arguments.get(index) {
        let target = match argument.as_str() {
            "--model" if model.is_none() => &mut model,
            "--id" if destination_id.is_none() => &mut destination_id,
            "--model" | "--id" => {
                return usage_error(stderr, &format!("{argument} may only be supplied once"));
            }
            value => {
                return usage_error(stderr, &format!("unrecognized provider argument: {value}"));
            }
        };
        let Some(value) = arguments.get(index + 1) else {
            return usage_error(stderr, &format!("{argument} needs a value"));
        };
        *target = Some(value.clone());
        index += 2;
    }
    let Some(model) = model else {
        return usage_error(stderr, "provider preset needs --model MODEL");
    };
    if !valid_model_id(&model) {
        return usage_error(stderr, "model must be 1-256 URL-safe model-id characters");
    }
    let destination_id = destination_id.unwrap_or_else(|| preset.id.to_owned());
    if !valid_destination_id(&destination_id) {
        return usage_error(
            stderr,
            "destination id must be 1-64 ASCII letters, digits, '.', '_', or '-'",
        );
    }
    let fragment = format!(
        "# Review and merge this destination into wayfinder-router.toml.\n\
# Set {api_key_env} only in the Router service environment.\n\
# This fragment does not add a routing tier or change Automatic.\n\
[gateway.models.\"{destination_id}\"]\n\
provider = \"openai-compatible\"\n\
base_url = \"{base_url}\"\n\
model = \"{model}\"\n\
api_key_env = \"{api_key_env}\"",
        api_key_env = preset.api_key_env,
        base_url = preset.base_url,
    );
    write_output(stdout, &fragment);
    EXIT_OK
}

fn valid_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':')
        })
}

fn valid_destination_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn usage_error(stderr: &mut dyn Write, message: &str) -> i32 {
    write_error(stderr, &format!("wayfinder-router: {message}"));
    EXIT_USAGE
}

#[cfg(test)]
mod tests {
    use super::*;
    use wayfinder_config::gateway::{ProviderKind, gateway_config_from_toml};

    #[test]
    fn every_hosted_preset_prints_a_valid_isolated_destination() -> Result<(), String> {
        for preset in HOSTED_PROVIDER_PRESETS {
            let model = format!("{}/model-v1", preset.id);
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            assert_eq!(
                run_provider(
                    &[
                        "preset".to_owned(),
                        preset.id.to_owned(),
                        "--model".to_owned(),
                        model.clone(),
                    ],
                    &mut stdout,
                    &mut stderr,
                ),
                EXIT_OK,
                "{}",
                preset.id
            );
            assert!(stderr.is_empty());
            let text = String::from_utf8(stdout).map_err(|error| error.to_string())?;
            let parsed =
                gateway_config_from_toml(&text, preset.id).map_err(|error| error.to_string())?;
            let destination = parsed
                .models
                .get(preset.id)
                .ok_or_else(|| format!("missing {} destination", preset.id))?;
            assert_eq!(destination.base_url.as_deref(), Some(preset.base_url));
            assert_eq!(destination.model, model);
            assert_eq!(destination.provider, ProviderKind::OpenAiCompatible);
            assert_eq!(destination.api_key_env.as_deref(), Some(preset.api_key_env));
            assert!(!text.contains("[[routing.tiers]]"));
            assert!(!text.contains("cost_per_1k"));
        }
        Ok(())
    }

    #[test]
    fn preset_allows_an_explicit_destination_id() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_provider(
                &[
                    "preset".to_owned(),
                    "openrouter".to_owned(),
                    "--model".to_owned(),
                    "anthropic/claude-sonnet".to_owned(),
                    "--id".to_owned(),
                    "openrouter-sonnet".to_owned(),
                ],
                &mut stdout,
                &mut stderr,
            ),
            EXIT_OK
        );
        let text = String::from_utf8_lossy(&stdout);
        assert!(text.contains("[gateway.models.\"openrouter-sonnet\"]"));
        assert!(text.contains("model = \"anthropic/claude-sonnet\""));
    }

    #[test]
    fn provider_command_is_bounded_and_lists_the_catalog() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_provider(&["presets".to_owned()], &mut stdout, &mut stderr),
            EXIT_OK
        );
        let names = String::from_utf8_lossy(&stdout);
        assert!(names.contains("openai\n"));
        assert!(names.contains("mistral\n"));

        for arguments in [
            vec!["preset".to_owned(), "unknown".to_owned()],
            vec!["preset".to_owned(), "openai".to_owned()],
            vec![
                "preset".to_owned(),
                "openai".to_owned(),
                "--model".to_owned(),
                "bad model".to_owned(),
            ],
            vec!["presets".to_owned(), "extra".to_owned()],
        ] {
            stdout.clear();
            stderr.clear();
            assert_eq!(
                run_provider(&arguments, &mut stdout, &mut stderr),
                EXIT_USAGE
            );
            assert!(stdout.is_empty());
            assert!(!stderr.is_empty());
        }
    }
}
