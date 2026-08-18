//! Explicit, no-clobber activation commands for the native Router.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::json;
use wayfinder_config::gateway::gateway_config_from_toml;
use wayfinder_config::{
    CONFIG_PATH_ENV, TierOrderPolicy, find_config_file, routing_config_from_toml,
};

use crate::app_setup_command::{preset_config, write_new_config};
use crate::{EXIT_CONFIG, EXIT_OK, EXIT_USAGE, expand_tilde, write_error, write_output};

const DEFAULT_CONFIG: &str = "wayfinder-router.toml";
const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8088";
const INIT_HELP: &str =
    "usage: wayfinder-router init [--preset local|hybrid|openai|gemini|apple-local] [--path PATH]";
const DOCTOR_HELP: &str = "usage: wayfinder-router doctor [--config PATH] [--json]";
const CONNECT_HELP: &str =
    "usage: wayfinder-router connect codex|claude-code|opencode [--endpoint URL]";
const OPEN_HELP: &str = "usage: wayfinder-router open [--print]";

pub(crate) fn run_init(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if wants_help(arguments) {
        write_output(stdout, INIT_HELP);
        return EXIT_OK;
    }
    let mut preset = "local".to_owned();
    let mut path = PathBuf::from(DEFAULT_CONFIG);
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--preset" => match arguments.get(index + 1) {
                Some(value) => {
                    preset.clone_from(value);
                    index += 1;
                }
                None => return usage_error(stderr, "--preset needs a value"),
            },
            "--path" => match arguments.get(index + 1) {
                Some(value) => {
                    path = expand_tilde(PathBuf::from(value));
                    index += 1;
                }
                None => return usage_error(stderr, "--path needs a value"),
            },
            value => return usage_error(stderr, &format!("unrecognized init argument: {value}")),
        }
        index += 1;
    }
    let Some(contents) = preset_config(&preset) else {
        return usage_error(stderr, &format!("unknown init preset: {preset}"));
    };
    match write_new_config(&path, contents) {
        Ok(()) => {
            write_output(
                stdout,
                &format!("Created {} with the {preset} policy.", path.display()),
            );
            write_output(stdout, "Next: wayfinder-router doctor");
            write_output(stdout, "Then: wayfinder-router serve");
            EXIT_OK
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => usage_error(
            stderr,
            &format!("{} already exists; no changes made", path.display()),
        ),
        Err(error) => usage_error(stderr, &format!("cannot write {}: {error}", path.display())),
    }
}

pub(crate) fn run_doctor(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if wants_help(arguments) {
        write_output(stdout, DOCTOR_HELP);
        return EXIT_OK;
    }
    let mut explicit = std::env::var_os(CONFIG_PATH_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(expand_tilde);
    let mut json_output = false;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--json" => json_output = true,
            "--config" => match arguments.get(index + 1) {
                Some(value) => {
                    explicit = Some(expand_tilde(PathBuf::from(value)));
                    index += 1;
                }
                None => return usage_error(stderr, "--config needs a value"),
            },
            value => return usage_error(stderr, &format!("unrecognized doctor argument: {value}")),
        }
        index += 1;
    }
    let path = find_config_file(Path::new("."), explicit.as_deref())
        .or(explicit)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            write_error(
                stderr,
                &format!("wayfinder-router: cannot read {}: {error}", path.display()),
            );
            return EXIT_CONFIG;
        }
    };
    if let Err(error) = routing_config_from_toml(
        &source,
        &path.display().to_string(),
        None,
        TierOrderPolicy::StrictInput,
    ) {
        write_error(stderr, &format!("wayfinder-router: {error}"));
        return EXIT_CONFIG;
    }
    let gateway = match gateway_config_from_toml(&source, &path.display().to_string()) {
        Ok(gateway) => gateway,
        Err(error) => {
            write_error(stderr, &format!("wayfinder-router: {error}"));
            return EXIT_CONFIG;
        }
    };
    let mut required = BTreeSet::new();
    for model in gateway.models.values() {
        if let Some(name) = &model.api_key_env {
            required.insert(name.clone());
        }
        for deployment in &model.deployments {
            if let Some(name) = &deployment.api_key_env {
                required.insert(name.clone());
            }
        }
    }
    let missing = required
        .into_iter()
        .filter(|name| std::env::var_os(name).is_none_or(|value| value.is_empty()))
        .collect::<Vec<_>>();
    let gateway_reachable = SocketAddr::from(([127, 0, 0, 1], 8_088));
    let gateway_reachable =
        TcpStream::connect_timeout(&gateway_reachable, Duration::from_millis(200)).is_ok();
    let healthy = !gateway.models.is_empty() && missing.is_empty();
    if json_output {
        let payload = json!({
            "schema_version": "1",
            "ok": healthy,
            "config": path,
            "destinations": gateway.models.len(),
            "missing_environment": missing,
            "gateway_reachable": gateway_reachable,
            "gateway": DEFAULT_ENDPOINT,
        });
        if serde_json::to_writer_pretty(&mut *stdout, &payload).is_err()
            || writeln!(stdout).is_err()
        {
            return EXIT_CONFIG;
        }
    } else {
        write_output(stdout, &format!("policy     ok ({})", path.display()));
        write_output(stdout, &format!("destinations {}", gateway.models.len()));
        write_output(
            stdout,
            &format!(
                "credentials {}",
                if missing.is_empty() {
                    "ok".to_owned()
                } else {
                    format!("missing {}", missing.join(", "))
                }
            ),
        );
        write_output(
            stdout,
            &format!(
                "gateway    {} ({DEFAULT_ENDPOINT})",
                if gateway_reachable {
                    "reachable"
                } else {
                    "not running"
                }
            ),
        );
    }
    if healthy { EXIT_OK } else { EXIT_CONFIG }
}

pub(crate) fn run_connect(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if wants_help(arguments) {
        write_output(stdout, CONNECT_HELP);
        return EXIT_OK;
    }
    let Some(client) = arguments.first() else {
        return usage_error(stderr, "connect needs a client");
    };
    let mut endpoint = DEFAULT_ENDPOINT.to_owned();
    let mut index = 1;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--endpoint" => match arguments.get(index + 1) {
                Some(value) => {
                    endpoint.clone_from(value);
                    index += 1;
                }
                None => return usage_error(stderr, "--endpoint needs a value"),
            },
            value => {
                return usage_error(stderr, &format!("unrecognized connect argument: {value}"));
            }
        }
        index += 1;
    }
    let endpoint = endpoint.trim_end_matches('/');
    if !is_loopback_endpoint(endpoint) {
        return usage_error(stderr, "--endpoint must be an explicit loopback HTTP URL");
    }
    let recipe = match client.as_str() {
        "codex" => format!(
            "# Add to ~/.codex/config.toml\nmodel_provider = \"wayfinder\"\nmodel = \"auto\"\n\n[model_providers.wayfinder]\nname = \"Wayfinder\"\nbase_url = \"{endpoint}/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false"
        ),
        "claude-code" => format!(
            "# Set these variables before starting Claude Code\nexport ANTHROPIC_BASE_URL=\"{endpoint}\"\nexport ANTHROPIC_AUTH_TOKEN=\"wayfinder-local\"\nexport CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1"
        ),
        "opencode" => format!(
            "{{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  \"provider\": {{\n    \"wayfinder\": {{\n      \"npm\": \"@ai-sdk/openai-compatible\",\n      \"name\": \"Wayfinder\",\n      \"options\": {{ \"baseURL\": \"{endpoint}/v1\" }},\n      \"models\": {{ \"auto\": {{ \"name\": \"Wayfinder Automatic\" }} }}\n    }}\n  }}\n}}"
        ),
        _ => return usage_error(stderr, &format!("unsupported client: {client}")),
    };
    write_output(stdout, &recipe);
    EXIT_OK
}

pub(crate) fn run_open(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if wants_help(arguments) {
        write_output(stdout, OPEN_HELP);
        return EXIT_OK;
    }
    let url = format!("{DEFAULT_ENDPOINT}/router");
    if arguments == ["--print"] {
        write_output(stdout, &url);
        return EXIT_OK;
    }
    if !arguments.is_empty() {
        return usage_error(stderr, "open accepts only --print");
    }
    let status = if cfg!(target_os = "macos") {
        Command::new("open").arg(&url).status()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd").args(["/C", "start", "", &url]).status()
    } else {
        Command::new("xdg-open").arg(&url).status()
    };
    match status {
        Ok(status) if status.success() => EXIT_OK,
        Ok(status) => usage_error(stderr, &format!("browser opener exited with {status}")),
        Err(error) => usage_error(stderr, &format!("cannot open {url}: {error}")),
    }
}

fn wants_help(arguments: &[String]) -> bool {
    arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
}

fn is_loopback_endpoint(endpoint: &str) -> bool {
    ["http://127.0.0.1:", "http://localhost:"]
        .iter()
        .find_map(|prefix| endpoint.strip_prefix(prefix))
        .is_some_and(|port| !port.is_empty() && port.parse::<u16>().is_ok())
}

fn usage_error(stderr: &mut dyn Write, message: &str) -> i32 {
    write_error(stderr, &format!("wayfinder-router: {message}"));
    EXIT_USAGE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_default_local_policy_and_never_clobbers()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("wayfinder-init-{}", uuid::Uuid::new_v4()));
        let path = root.join("wayfinder-router.toml");
        let arguments = vec!["--path".to_owned(), path.display().to_string()];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(run_init(&arguments, &mut stdout, &mut stderr), EXIT_OK);
        let original = fs::read_to_string(&path)?;
        assert!(original.contains("preset: local"));
        assert_eq!(run_init(&arguments, &mut stdout, &mut stderr), EXIT_USAGE);
        assert_eq!(fs::read_to_string(&path)?, original);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn connect_only_accepts_loopback_and_renders_each_client() {
        for (client, marker) in [
            ("codex", "wire_api"),
            ("claude-code", "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"),
            ("opencode", "@ai-sdk/openai-compatible"),
        ] {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            assert_eq!(
                run_connect(&[client.to_owned()], &mut stdout, &mut stderr),
                EXIT_OK
            );
            assert!(String::from_utf8_lossy(&stdout).contains(marker));
        }
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_connect(
                &[
                    "codex".to_owned(),
                    "--endpoint".to_owned(),
                    "https://example.com".to_owned()
                ],
                &mut stdout,
                &mut stderr
            ),
            EXIT_USAGE
        );
        assert_eq!(
            run_connect(
                &[
                    "codex".to_owned(),
                    "--endpoint".to_owned(),
                    "http://localhost:8088@evil.example".to_owned(),
                ],
                &mut stdout,
                &mut stderr,
            ),
            EXIT_USAGE
        );
    }

    #[test]
    fn doctor_reports_a_valid_local_policy_without_calling_a_provider()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("wayfinder-doctor-{}", uuid::Uuid::new_v4()));
        let path = root.join("wayfinder-router.toml");
        fs::create_dir_all(&root)?;
        fs::write(&path, preset_config("local").ok_or("missing local preset")?)?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_doctor(
                &[
                    "--config".to_owned(),
                    path.display().to_string(),
                    "--json".to_owned(),
                ],
                &mut stdout,
                &mut stderr,
            ),
            EXIT_OK
        );
        let payload: serde_json::Value = serde_json::from_slice(&stdout)?;
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["destinations"], 1);
        assert_eq!(payload["missing_environment"], json!([]));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn open_print_is_side_effect_free() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_open(&["--print".to_owned()], &mut stdout, &mut stderr),
            EXIT_OK
        );
        assert_eq!(
            String::from_utf8_lossy(&stdout),
            "http://127.0.0.1:8088/router\n"
        );
    }
}
