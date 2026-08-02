//! Native `wayfinder-router` command surface.

#![forbid(unsafe_code)]

mod app_setup_command;
mod apple_foundation_live;
mod codex_app_server;
mod config_command;
mod service_command;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use uuid::Uuid;
use wayfinder_apple_foundation_xpc::FoundationModelsClient;
use wayfinder_config::gateway::{GatewayConfig, ProviderKind, gateway_config_from_toml};
use wayfinder_config::{
    CONFIG_PATH_ENV, THRESHOLD_ENV, TierOrderPolicy, find_config_file, load_routing_config,
    routing_config_from_toml,
};
use wayfinder_gateway::audit::AuditLog;
use wayfinder_gateway::delivery::{
    AppleFoundationModelDelivery, BufferedProviderDelivery, CodexAppServerDelivery,
    CredentialError, CredentialSource, OpenAiCompatibleDelivery, ProviderDelivery,
    UnavailableCodexDelivery,
};
use wayfinder_gateway::reload::{LastGood, ReloadOutcome};
use wayfinder_gateway::server::{DEFAULT_DRAIN_TIMEOUT, serve_with_shutdown, shutdown_signal};
use wayfinder_gateway::{
    AppState, ConfiguredDeployment, ConfiguredModel, RouteOn, build_reloadable_data_plane_router,
    build_reloadable_router, validate_data_plane_state,
};
use wayfinder_providers::openai_compat::{
    DEFAULT_CONNECT_TIMEOUT, OpenAiProviderClient, ProviderClientConfig, SecretValue,
};
use wayfinder_routing_core::{
    ComplexityScore, FEATURE_ORDER, Lexicon, RoutingConfig, RoutingMode, Weights, binary_tiers,
    explain_score, score_complexity,
};
use wayfinder_service::credentials::{LegacyCommandLimits, resolve_legacy_command};
use wayfinder_service::pricing::{LedgerLoad, SavingsLedger};

const SAVINGS_FILE_ENV: &str = "WAYFINDER_ROUTER_SAVINGS_FILE";
const AUDIT_FILE_ENV: &str = "WAYFINDER_ROUTER_AUDIT_FILE";
/// Successful command.
pub const EXIT_OK: i32 = 0;
/// Invalid configuration or deterministic-core failure.
pub const EXIT_CONFIG: i32 = 1;
/// Invalid arguments or input file.
pub const EXIT_USAGE: i32 = 2;

fn product_version() -> &'static str {
    option_env!("WAYFINDER_PRODUCT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

fn validate_persistence_target(path: &Path, label: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "cannot prepare {label} directory {}: {error}",
            parent.display()
        )
    })?;

    let probe = parent.join(format!(
        ".wayfinder-write-probe-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let mut probe_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|error| {
            format!(
                "cannot write {label} directory {}: {error}",
                parent.display()
            )
        })?;
    probe_file
        .write_all(b"wayfinder persistence probe\n")
        .map_err(|error| format!("cannot write {label} probe {}: {error}", probe.display()))?;
    probe_file
        .sync_all()
        .map_err(|error| format!("cannot sync {label} probe {}: {error}", probe.display()))?;
    drop(probe_file);
    fs::remove_file(&probe).map_err(|error| {
        format!(
            "cannot clean {label} write probe {}: {error}",
            probe.display()
        )
    })?;

    if path.exists() {
        OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|error| format!("cannot write {label} file {}: {error}", path.display()))?;
    }
    Ok(())
}

fn load_savings_ledger(path: &Path) -> Result<(SavingsLedger, LedgerLoad), String> {
    validate_persistence_target(path, "savings")?;
    let (ledger, loaded) = SavingsLedger::load_resilient(path, 400, true)
        .map_err(|error| format!("cannot load savings ledger: {error}"))?;
    ledger
        .save(path)
        .map_err(|error| format!("cannot persist savings ledger: {error}"))?;
    Ok((ledger, loaded))
}

/// Whether the process entry point must select the asynchronous serve path.
#[must_use]
pub fn is_serve_command(arguments: &[OsString]) -> bool {
    arguments.first().and_then(|argument| argument.to_str()) == Some("serve")
}

/// Run with injected streams. Environment and current-directory discovery remain
/// real so subprocess and in-process tests exercise the same config contract.
pub fn run(
    arguments: Vec<OsString>,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let arguments = match arguments
        .into_iter()
        .map(|argument| argument.into_string())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(arguments) => arguments,
        Err(_) => {
            write_error(stderr, "wayfinder-router: arguments must be valid UTF-8");
            return EXIT_USAGE;
        }
    };
    let Some(command) = arguments.first().map(String::as_str) else {
        write_error(stderr, TOP_LEVEL_USAGE);
        return EXIT_USAGE;
    };
    match command {
        "--version" => {
            write_output(stdout, &format!("wayfinder-router {}", product_version()));
            EXIT_OK
        }
        "-h" | "--help" => {
            write_output(stdout, TOP_LEVEL_HELP);
            EXIT_OK
        }
        "route" => run_route(&arguments[1..], stdin, stdout, stderr),
        "keys" => run_keys(&arguments[1..], stdout, stderr),
        "app-setup-init" => app_setup_command::run_app_setup(&arguments[1..], stdout, stderr),
        "config" => config_command::run_config(&arguments[1..], stdin, stdout, stderr),
        "app-configure-chatgpt" => {
            app_setup_command::run_configure_chatgpt(&arguments[1..], stdout, stderr)
        }
        "service" => service_command::run_service(&arguments[1..], stdout, stderr),
        "capabilities" => run_capabilities(&arguments[1..], stdout, stderr),
        "apple-foundation-live-smoke" => {
            apple_foundation_live::run(&arguments[1..], stdout, stderr)
        }
        "serve" => {
            write_error(
                stderr,
                "wayfinder-router: serve must be invoked through the process entry point",
            );
            EXIT_USAGE
        }
        other => {
            write_error(
                stderr,
                &format!("wayfinder-router: unsupported command: {other}"),
            );
            EXIT_USAGE
        }
    }
}

fn run_keys(arguments: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        write_output(stdout, KEYS_HELP);
        return EXIT_OK;
    }
    if arguments.first().map(String::as_str) != Some("new") {
        write_error(stderr, "wayfinder-router: keys requires the 'new' action");
        return EXIT_USAGE;
    }
    let mut id = "default".to_owned();
    let mut workspace = None;
    let mut json_output = false;
    let mut index = 1_usize;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--json" => json_output = true,
            "--id" => {
                index = index.saturating_add(1);
                let Some(value) = arguments.get(index) else {
                    write_error(stderr, "wayfinder-router: --id needs a value");
                    return EXIT_USAGE;
                };
                id.clone_from(value);
            }
            value if value.starts_with("--id=") => {
                id = value.trim_start_matches("--id=").to_owned();
            }
            "--workspace" => {
                index = index.saturating_add(1);
                let Some(value) = arguments.get(index) else {
                    write_error(stderr, "wayfinder-router: --workspace needs a value");
                    return EXIT_USAGE;
                };
                workspace = Some(value.clone());
            }
            value if value.starts_with("--workspace=") => {
                workspace = Some(value.trim_start_matches("--workspace=").to_owned());
            }
            value => {
                write_error(
                    stderr,
                    &format!("wayfinder-router: unrecognized keys argument: {value}"),
                );
                return EXIT_USAGE;
            }
        }
        index = index.saturating_add(1);
    }
    if workspace.as_ref().is_some_and(|workspace| {
        workspace.is_empty()
            || workspace.len() > wayfinder_gateway::access::MAX_KEY_ID_BYTES
            || !workspace.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
    }) {
        write_error(
            stderr,
            "wayfinder-router: workspace id must be 1-128 visible ASCII characters",
        );
        return EXIT_USAGE;
    }
    if id.is_empty()
        || id.len() > wayfinder_gateway::access::MAX_KEY_ID_BYTES
        || id.chars().any(char::is_control)
    {
        write_error(
            stderr,
            "wayfinder-router: key id must be nonempty, bounded, and contain no control characters",
        );
        return EXIT_USAGE;
    }

    let key = format!("wf_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let hash = wayfinder_gateway::auth::hash_key(&key);
    let id_literal = match serde_json::to_string(&id) {
        Ok(value) => value,
        Err(_) => {
            write_error(stderr, "wayfinder-router: cannot render key id");
            return EXIT_CONFIG;
        }
    };
    let workspace_line = workspace
        .as_ref()
        .and_then(|workspace| serde_json::to_string(workspace).ok())
        .map_or_else(String::new, |workspace| {
            format!("\nworkspace = {workspace}")
        });
    let toml = format!("[gateway.keys.{id_literal}]\nhash = \"{hash}\"{workspace_line}");
    if json_output {
        let payload = json!({
            "schema_version": 1,
            "id": id,
            "key": key,
            "hash": hash,
            "workspace": workspace,
            "toml": toml,
        });
        if serde_json::to_writer_pretty(&mut *stdout, &payload).is_err()
            || writeln!(stdout).is_err()
        {
            write_error(stderr, "wayfinder-router: cannot write key output");
            return EXIT_CONFIG;
        }
    } else {
        write_output(
            stdout,
            &format!(
                "Wayfinder virtual key (shown once):\n{key}\n\nAdd this hashed entry to wayfinder-router.toml:\n{toml}"
            ),
        );
    }
    EXIT_OK
}

fn run_capabilities(arguments: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    if arguments
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        write_output(stdout, "usage: wayfinder-router capabilities [--json]");
        return EXIT_OK;
    }
    if arguments.iter().any(|argument| argument != "--json") {
        write_error(stderr, "wayfinder-router: capabilities accepts only --json");
        return EXIT_USAGE;
    }
    let credential_mechanisms = vec!["environment-reference", "legacy-command-reference"];
    #[cfg(target_os = "macos")]
    let credential_mechanisms = {
        let mut mechanisms = credential_mechanisms;
        mechanisms.push("xpc-credential-broker-v1");
        mechanisms
    };
    let payload = json!({
        "schema_version": "1",
        "implementation": "rust",
        "version": product_version(),
        "target_architecture": target_architecture(),
        "commands": ["route", "serve", "service", "keys", "capabilities", "app-setup-init", "app-configure-chatgpt", "config", "apple-foundation-live-smoke"],
        "native_commands": ["route", "serve", "service", "keys new", "capabilities", "app-setup-init", "app-configure-chatgpt", "config read-routing", "config apply-routing", "apple-foundation-live-smoke"],
        "delegated_commands": [],
        "delegation": null,
        "decision_schema_versions": ["3"],
        "credential_mechanisms": credential_mechanisms,
        "gateway_ready": true
    });
    if serde_json::to_writer_pretty(&mut *stdout, &payload).is_err() || writeln!(stdout).is_err() {
        write_error(stderr, "wayfinder-router: cannot write capabilities output");
        return EXIT_CONFIG;
    }
    EXIT_OK
}

fn target_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        architecture => architecture,
    }
}

/// Run the process-owned async gateway path.
///
/// The synchronous [`run`] function remains stream-injectable for ordinary CLI
/// parity tests; only the real binary entry point calls this long-lived path.
pub async fn run_serve_process(
    arguments: &[OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let arguments = match arguments
        .iter()
        .cloned()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(arguments) => arguments,
        Err(_) => {
            write_error(stderr, "wayfinder-router: arguments must be valid UTF-8");
            return EXIT_USAGE;
        }
    };
    let options = match parse_serve_options(&arguments) {
        Ok(Some(options)) => options,
        Ok(None) => {
            write_output(stdout, SERVE_HELP);
            return EXIT_OK;
        }
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };
    let _telemetry = match wayfinder_gateway::otel::init_from_env() {
        Ok(telemetry) => telemetry,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_CONFIG;
        }
    };
    let mut startup_warnings = Vec::new();
    let state = match build_serve_state(&options, &mut startup_warnings).await {
        Ok(state) => state,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_CONFIG;
        }
    };
    let _ = stderr.write_all(&startup_warnings);
    if let Err(error) = validate_serve_security(&options, &state) {
        write_error(stderr, &format!("wayfinder-router: {error}"));
        return EXIT_CONFIG;
    }
    let listener = match tokio::net::TcpListener::bind((options.host.as_str(), options.port)).await
    {
        Ok(listener) => listener,
        Err(error) => {
            write_error(
                stderr,
                &format!(
                    "wayfinder-router: cannot bind {}:{}: {error}",
                    options.host, options.port
                ),
            );
            return EXIT_CONFIG;
        }
    };
    let reload_path = selected_config_path(&options);
    let initial_version = config_source_version(&reload_path);
    let audit_log = state.audit_log().clone();
    let holder = Arc::new(LastGood::new(state, initial_version));
    let reload_holder = Arc::clone(&holder);
    let reload_options = options.clone();
    let reload_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let version = config_source_version(&reload_path);
            if reload_holder.version().ok() == Some(version) {
                continue;
            }
            let mut diagnostics = Vec::new();
            let loaded = match build_serve_state(&reload_options, &mut diagnostics).await {
                Ok(next) => {
                    let validated = if reload_options.surface == ServeSurface::DataPlane {
                        validate_data_plane_state(&next).map_err(|error| error.to_string())
                    } else {
                        Ok(())
                    };
                    match (validated, reload_holder.current()) {
                        (Ok(()), Ok(current)) if current.shared_state_matches(&next) => {
                            Ok(next.with_runtime_state_from(&current))
                        }
                        (Ok(()), Ok(_)) => Err(
                            "shared state backend, endpoint, or namespace changes require restart"
                                .to_owned(),
                        ),
                        (Err(error), _) => Err(error),
                        (_, Err(error)) => Err(error.to_string()),
                    }
                }
                Err(error) => {
                    let diagnostics = String::from_utf8_lossy(&diagnostics);
                    let diagnostics = diagnostics.trim();
                    if diagnostics.is_empty() {
                        Err(error)
                    } else {
                        Err(format!("{error}; {diagnostics}"))
                    }
                }
            };
            let loaded = match loaded {
                Ok(next) => match next
                    .audit_log()
                    .record(
                        "system",
                        "config.reload",
                        None,
                        Some(json!({"version": version.to_string()})),
                    )
                    .await
                {
                    Ok(()) => Ok(next),
                    Err(error) => Err(format!("config reload audit failed: {error}")),
                },
                Err(error) => Err(error),
            };
            let outcome = reload_holder.refresh(version, || loaded);
            match outcome {
                Ok(ReloadOutcome::Reloaded(_)) => {}
                Ok(ReloadOutcome::Retained { current, error }) => {
                    eprintln!("wayfinder-router: config reload rejected: {error}");
                    let _ = current.metrics().record_reload_failure();
                    if let Err(error) = current
                        .audit_log()
                        .record(
                            "system",
                            "config.reload_failed",
                            None,
                            Some(json!({"version": version.to_string()})),
                        )
                        .await
                    {
                        eprintln!("wayfinder-router: config reload failure audit failed: {error}");
                    }
                }
                Ok(ReloadOutcome::Unchanged(_)) | Err(_) => {}
            }
        }
    });
    let shutdown = async {
        let _: Result<(), wayfinder_gateway::server::ServerError> = shutdown_signal().await;
    };
    let application = match options.surface {
        ServeSurface::Local => build_reloadable_router(holder),
        ServeSurface::DataPlane => build_reloadable_data_plane_router(holder),
    };
    let server_result =
        serve_with_shutdown(listener, application, shutdown, DEFAULT_DRAIN_TIMEOUT).await;
    reload_task.abort();
    let _ = reload_task.await;
    let audit_result = tokio::time::timeout(DEFAULT_DRAIN_TIMEOUT, audit_log.shutdown()).await;
    let audit_result = match audit_result {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => Err(format!(
            "audit worker did not drain within {DEFAULT_DRAIN_TIMEOUT:?}"
        )),
    };
    match (server_result, audit_result) {
        (Ok(()), Ok(())) => EXIT_OK,
        (Err(server), Ok(())) => {
            write_error(stderr, &format!("wayfinder-router: {server}"));
            EXIT_CONFIG
        }
        (Ok(()), Err(audit)) => {
            write_error(stderr, &format!("wayfinder-router: {audit}"));
            EXIT_CONFIG
        }
        (Err(server), Err(audit)) => {
            write_error(
                stderr,
                &format!("wayfinder-router: {server}; audit shutdown failed: {audit}"),
            );
            EXIT_CONFIG
        }
    }
}

fn selected_config_path(options: &ServeOptions) -> PathBuf {
    let explicit = options.config.clone().or_else(|| {
        std::env::var_os(CONFIG_PATH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(expand_tilde)
    });
    find_config_file(Path::new("."), explicit.as_deref())
        .or(explicit)
        .unwrap_or_else(|| PathBuf::from("wayfinder-router.toml"))
}

fn config_source_version(path: &Path) -> u128 {
    let Ok(metadata) = fs::metadata(path) else {
        return 0;
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    modified ^ u128::from(metadata.len())
}

#[derive(Clone, Debug, PartialEq)]
struct ServeOptions {
    host: String,
    port: u16,
    surface: ServeSurface,
    dry_run: bool,
    timeout: Option<f64>,
    config: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ServeSurface {
    #[default]
    Local,
    DataPlane,
}

impl ServeSurface {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "local" => Ok(Self::Local),
            "data-plane" => Ok(Self::DataPlane),
            _ => Err("--surface must be 'local' or 'data-plane'".to_owned()),
        }
    }
}

fn parse_serve_options(arguments: &[String]) -> Result<Option<ServeOptions>, String> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(None);
    }
    let mut options = ServeOptions {
        host: "127.0.0.1".to_owned(),
        port: 8_088,
        surface: ServeSurface::Local,
        dry_run: false,
        timeout: None,
        config: None,
    };
    let mut index = 0_usize;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--dry-run" => options.dry_run = true,
            "--host" => {
                index = index.saturating_add(1);
                options.host = arguments
                    .get(index)
                    .ok_or_else(|| "--host needs a value".to_owned())?
                    .clone();
            }
            "--port" => {
                index = index.saturating_add(1);
                let raw = arguments
                    .get(index)
                    .ok_or_else(|| "--port needs a value".to_owned())?;
                options.port = parse_port(raw)?;
            }
            "--surface" => {
                index = index.saturating_add(1);
                let raw = arguments
                    .get(index)
                    .ok_or_else(|| "--surface needs a value".to_owned())?;
                options.surface = ServeSurface::parse(raw)?;
            }
            "--timeout" => {
                index = index.saturating_add(1);
                let raw = arguments
                    .get(index)
                    .ok_or_else(|| "--timeout needs a value".to_owned())?;
                options.timeout = Some(parse_positive_timeout(raw)?);
            }
            "--config" => {
                index = index.saturating_add(1);
                let raw = arguments
                    .get(index)
                    .ok_or_else(|| "--config needs a value".to_owned())?;
                options.config = Some(expand_tilde(PathBuf::from(raw)));
            }
            value if value.starts_with("--host=") => {
                options.host = value.trim_start_matches("--host=").to_owned();
            }
            value if value.starts_with("--port=") => {
                options.port = parse_port(value.trim_start_matches("--port="))?;
            }
            value if value.starts_with("--surface=") => {
                options.surface = ServeSurface::parse(value.trim_start_matches("--surface="))?;
            }
            value if value.starts_with("--timeout=") => {
                options.timeout = Some(parse_positive_timeout(
                    value.trim_start_matches("--timeout="),
                )?);
            }
            value if value.starts_with("--config=") => {
                options.config = Some(expand_tilde(PathBuf::from(
                    value.trim_start_matches("--config="),
                )));
            }
            value => return Err(format!("unrecognized serve argument: {value}")),
        }
        index = index.saturating_add(1);
    }
    if options.host.is_empty() {
        return Err("--host must not be empty".to_owned());
    }
    Ok(Some(options))
}

fn parse_port(raw: &str) -> Result<u16, String> {
    raw.parse::<u16>()
        .map_err(|_| "--port must be an integer from 0 to 65535".to_owned())
}

fn parse_positive_timeout(raw: &str) -> Result<f64, String> {
    let timeout = raw
        .parse::<f64>()
        .map_err(|_| "--timeout must be a positive finite number".to_owned())?;
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err("--timeout must be a positive finite number".to_owned());
    }
    Ok(timeout)
}

fn validate_serve_security(options: &ServeOptions, state: &AppState) -> Result<(), String> {
    if !host_is_loopback(&options.host) && options.surface == ServeSurface::Local {
        return Err(
            "refusing to expose the local operator surface beyond loopback; use --surface data-plane with configured virtual keys"
                .to_owned(),
        );
    }
    if options.surface == ServeSurface::DataPlane {
        validate_data_plane_state(state).map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn build_serve_state<W: Write>(
    options: &ServeOptions,
    stderr: &mut W,
) -> Result<AppState, String> {
    let explicit = options.config.clone().or_else(|| {
        std::env::var_os(CONFIG_PATH_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(expand_tilde)
    });
    let selected = find_config_file(Path::new("."), explicit.as_deref());
    let (text, where_) = if let Some(path) = selected {
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        (text, path.display().to_string())
    } else {
        (
            String::new(),
            explicit.as_ref().map_or_else(
                || "wayfinder-router.toml".to_owned(),
                |path| path.display().to_string(),
            ),
        )
    };
    let threshold_environment = std::env::var(THRESHOLD_ENV).ok();
    let routing = routing_config_from_toml(
        &text,
        &where_,
        threshold_environment.as_deref(),
        development_tier_policy(),
    )
    .map_err(|error| error.to_string())?;
    let gateway = gateway_config_from_toml(&text, &where_).map_err(|error| error.to_string())?;

    let codex_provider_configured = gateway
        .models
        .values()
        .any(|model| model.provider == ProviderKind::CodexAppServer);
    let codex_adapter = match codex_app_server::build_codex_app_server_adapter(
        codex_provider_configured,
        product_version(),
    ) {
        Ok(adapter) => adapter,
        Err(error) => {
            let _ = writeln!(stderr, "wayfinder-router: warning: {error}");
            None
        }
    };

    let mut credentials = StartupCredentialSource::default();
    preload_xpc_credentials(&gateway, &mut credentials, stderr).await;
    preload_command_credentials(&gateway, &mut credentials, stderr).await;
    let models = configured_models(&gateway, &credentials);
    let route_on = match gateway.route_on.as_str() {
        "turn" => RouteOn::Turn,
        "last_user" => RouteOn::LastUser,
        "user" => RouteOn::User,
        "all" => RouteOn::All,
        _ => return Err("validated gateway route_on is unsupported".to_owned()),
    };
    let audit_path = std::env::var_os(AUDIT_FILE_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            selected_config_path(options)
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("wayfinder-audit.jsonl")
        });
    validate_persistence_target(&audit_path, "audit")?;
    let audit_log = AuditLog::from_path(audit_path).map_err(|error| error.to_string())?;
    let mut state = AppState::new(routing, models, gateway.offline, product_version())
        .with_dry_run(options.dry_run)
        .with_route_on(route_on)
        .with_sticky(gateway.sticky, gateway.sticky_cooldown)
        .with_slash_directives(gateway.slash_directives)
        .with_route_presets(gateway.routes.clone())
        .with_gateway_budget(&gateway)
        .with_audit_log(audit_log)
        .with_gateway_operator_auth(&gateway)
        .map_err(|error| error.to_string())?;
    let savings_path = std::env::var_os(SAVINGS_FILE_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("wayfinder-savings.json"));
    let (ledger, loaded) = load_savings_ledger(&savings_path)?;
    if let LedgerLoad::Recovered { quarantine } = loaded {
        let _ = writeln!(
            stderr,
            "wayfinder-router: recovered savings ledger; corrupt state quarantined at {}",
            quarantine.display()
        );
    }
    state = state
        .with_savings_ledger(Arc::new(ledger))
        .with_savings_path(savings_path);
    state = state
        .with_gateway_admission(&gateway)
        .map_err(|error| error.to_string())?
        .with_gateway_reliability(&gateway)
        .map_err(|error| error.to_string())?;
    state = state
        .with_gateway_shared_access(&gateway)
        .await
        .map_err(|error| error.to_string())?;
    state = state
        .with_gateway_cache_async(&gateway)
        .await
        .map_err(|error| error.to_string())?;

    if codex_provider_configured {
        let control: Arc<dyn wayfinder_gateway::codex_control::CodexControl> =
            codex_adapter.as_ref().map_or_else(
                || Arc::new(codex_app_server::UnavailableCodexControl::new()) as Arc<_>,
                |adapter| Arc::new(adapter.clone()) as Arc<_>,
            );
        state = state.with_codex_control(control, host_is_literal_loopback(&options.host));
    }

    if !options.dry_run {
        let timeout = resolve_provider_timeout(options.timeout)?;
        let request_timeout = Duration::try_from_secs_f64(timeout)
            .map_err(|_| "provider timeout is outside the supported duration range".to_owned())?;
        let client = OpenAiProviderClient::new(ProviderClientConfig {
            request_timeout,
            connect_timeout: request_timeout.min(DEFAULT_CONNECT_TIMEOUT),
            ..ProviderClientConfig::default()
        })
        .map_err(|error| error.to_string())?;
        let openai = Arc::new(OpenAiCompatibleDelivery::new(client, credentials));
        let apple_timeout = Duration::from_millis(
            u64::try_from(
                request_timeout
                    .min(wayfinder_apple_foundation_xpc::MAX_TIMEOUT)
                    .as_millis()
                    .max(1),
            )
            .map_err(|_| "provider timeout is outside the supported duration range".to_owned())?,
        );
        let apple = AppleFoundationModelDelivery::new(
            Arc::new(FoundationModelsClient::default()),
            apple_timeout,
            || uuid::Uuid::new_v4().to_string(),
        );
        let codex: Arc<dyn ProviderDelivery> = codex_adapter.as_ref().map_or_else(
            || Arc::new(UnavailableCodexDelivery) as Arc<_>,
            |adapter| Arc::new(CodexAppServerDelivery::new(Arc::new(adapter.manager()))) as Arc<_>,
        );
        state = state.with_provider_delivery(Arc::new(BufferedProviderDelivery::new(
            openai, apple, codex,
        )));
    }
    Ok(state)
}

fn configured_models(
    gateway: &GatewayConfig,
    credentials: &StartupCredentialSource,
) -> Vec<ConfiguredModel> {
    gateway
        .models
        .iter()
        .map(|(name, model)| {
            let key_ready = model
                .api_key_env
                .as_deref()
                .is_none_or(|reference| credentials.is_ready(reference));
            let deployments = model
                .deployments
                .iter()
                .map(|deployment| {
                    ConfiguredDeployment::new(
                        deployment.id.clone(),
                        deployment.base_url.clone(),
                        deployment.model.clone(),
                        deployment.api_key_env.clone(),
                        deployment
                            .api_key_env
                            .as_deref()
                            .is_none_or(|reference| credentials.is_ready(reference)),
                        deployment.weight,
                    )
                })
                .collect();
            ConfiguredModel::from_gateway_model(name, model, key_ready)
                .with_deployments(deployments)
        })
        .collect()
}

type EnvironmentCredential = fn(&str) -> Option<SecretValue>;

struct StartupCredentialSource {
    environment: EnvironmentCredential,
    broker_required: bool,
    broker_values: BTreeMap<String, SecretValue>,
    command_values: BTreeMap<String, SecretValue>,
}

impl StartupCredentialSource {
    fn new(environment: EnvironmentCredential) -> Self {
        Self {
            environment,
            broker_required: bundled_xpc_required(),
            broker_values: BTreeMap::new(),
            command_values: BTreeMap::new(),
        }
    }

    fn is_ready(&self, reference: &str) -> bool {
        self.broker_values.contains_key(reference)
            || (!self.broker_required
                && ((self.environment)(reference).is_some()
                    || self.command_values.contains_key(reference)))
    }
}

impl Default for StartupCredentialSource {
    fn default() -> Self {
        Self::new(environment_credential)
    }
}

impl fmt::Debug for StartupCredentialSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartupCredentialSource")
            .field("broker_required", &self.broker_required)
            .field("broker_value_count", &self.broker_values.len())
            .field("command_value_count", &self.command_values.len())
            .field("values", &"[REDACTED]")
            .finish()
    }
}

impl CredentialSource for StartupCredentialSource {
    fn resolve(&self, reference: Option<&str>) -> Result<Option<SecretValue>, CredentialError> {
        let Some(reference) = reference else {
            return Ok(None);
        };
        if let Some(secret) = self.broker_values.get(reference) {
            return Ok(Some(secret.duplicate_for_request()));
        }
        if self.broker_required {
            return Err(CredentialError::Unavailable);
        }
        if let Some(secret) = (self.environment)(reference) {
            return Ok(Some(secret));
        }
        self.command_values
            .get(reference)
            .map(SecretValue::duplicate_for_request)
            .map(Some)
            .ok_or(CredentialError::Unavailable)
    }
}

#[cfg(target_os = "macos")]
fn bundled_xpc_required() -> bool {
    std::env::current_exe().is_ok_and(|path| {
        path.components()
            .any(|component| component.as_os_str() == "Helpers")
            && path.to_string_lossy().contains(".app/Contents/")
    })
}

#[cfg(not(target_os = "macos"))]
const fn bundled_xpc_required() -> bool {
    false
}

#[cfg(target_os = "macos")]
async fn preload_xpc_credentials<W: Write>(
    gateway: &GatewayConfig,
    credentials: &mut StartupCredentialSource,
    stderr: &mut W,
) {
    if !credentials.broker_required {
        return;
    }
    for (name, model) in &gateway.models {
        let mut references = Vec::with_capacity(1 + model.deployments.len());
        if let Some(reference) = model.api_key_env.as_deref() {
            references.push(reference);
        }
        references.extend(
            model
                .deployments
                .iter()
                .filter_map(|deployment| deployment.api_key_env.as_deref()),
        );
        for reference in references {
            let owned_reference = reference.to_owned();
            match tokio::task::spawn_blocking(move || {
                wayfinder_macos_xpc::resolve_xpc_credential(&owned_reference)
            })
            .await
            {
                Ok(Ok(secret)) => {
                    credentials
                        .broker_values
                        .insert(reference.to_owned(), secret);
                }
                Ok(Err(error)) => write_error(
                    stderr,
                    &format!(
                        "wayfinder-router: warning: model '{}' broker credential unavailable: {error}",
                        diagnostic_label(name)
                    ),
                ),
                Err(_) => write_error(
                    stderr,
                    &format!(
                        "wayfinder-router: warning: model '{}' broker credential task failed",
                        diagnostic_label(name)
                    ),
                ),
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
async fn preload_xpc_credentials<W: Write>(
    _gateway: &GatewayConfig,
    _credentials: &mut StartupCredentialSource,
    _stderr: &mut W,
) {
}

fn environment_credential(reference: &str) -> Option<SecretValue> {
    std::env::var(reference)
        .ok()
        .filter(|value| !value.is_empty())
        .map(SecretValue::new)
}

async fn preload_command_credentials<W: Write>(
    gateway: &GatewayConfig,
    credentials: &mut StartupCredentialSource,
    stderr: &mut W,
) {
    for (name, model) in &gateway.models {
        if credentials.broker_required {
            continue;
        }
        let (Some(reference), Some(command)) =
            (model.api_key_env.as_deref(), model.api_key_cmd.as_deref())
        else {
            continue;
        };
        if credentials.is_ready(reference) {
            continue;
        }
        match resolve_legacy_command(command, LegacyCommandLimits::default()).await {
            Ok(secret) => {
                credentials
                    .command_values
                    .insert(reference.to_owned(), secret);
            }
            Err(error) => write_error(
                stderr,
                &format!(
                    "wayfinder-router: warning: model '{}' credential command failed: {error}",
                    diagnostic_label(name)
                ),
            ),
        }
    }
}

fn diagnostic_label(value: &str) -> String {
    value
        .chars()
        .take(128)
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn resolve_provider_timeout(explicit: Option<f64>) -> Result<f64, String> {
    if let Some(timeout) = explicit {
        return Ok(timeout);
    }
    let timeout = std::env::var("WAYFINDER_ROUTER_TIMEOUT")
        .ok()
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(60.0);
    if timeout > Duration::MAX.as_secs_f64() {
        return Err("provider timeout is outside the supported duration range".to_owned());
    }
    Ok(timeout)
}

fn host_is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host_is_literal_loopback(host)
}

fn host_is_literal_loopback(host: &str) -> bool {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn run_route(
    arguments: &[String],
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let options = match parse_route_options(arguments) {
        Ok(Some(options)) => options,
        Ok(None) => {
            write_output(stdout, ROUTE_HELP);
            return EXIT_OK;
        }
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };

    let (text, start_dir) = if options.prompt == "-" {
        let mut text = String::new();
        if stdin.read_to_string(&mut text).is_err() {
            write_error(stderr, "wayfinder-router: stdin is not valid UTF-8 text");
            return EXIT_USAGE;
        }
        (text, PathBuf::from("."))
    } else {
        let path = PathBuf::from(&options.prompt);
        if !path.is_file() {
            write_error(
                stderr,
                &format!("wayfinder-router: file not found: {}", options.prompt),
            );
            return EXIT_USAGE;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                write_error(
                    stderr,
                    &format!("wayfinder-router: cannot read {}: {error}", options.prompt),
                );
                return EXIT_USAGE;
            }
        };
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                write_error(
                    stderr,
                    &format!(
                        "wayfinder-router: {} is not valid UTF-8 text",
                        options.prompt
                    ),
                );
                return EXIT_USAGE;
            }
        };
        let start = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        (text, start)
    };

    let explicit = std::env::var_os(CONFIG_PATH_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(expand_tilde);
    let threshold_environment = std::env::var(THRESHOLD_ENV).ok();
    let tier_order = development_tier_policy();
    let mut config = match load_routing_config(
        &start_dir,
        explicit.as_deref(),
        threshold_environment.as_deref(),
        tier_order,
    ) {
        Ok(config) => config,
        Err(error) => {
            write_error(stderr, &format!("wayfinder-router: {error}"));
            return EXIT_CONFIG;
        }
    };
    if let Some(threshold) = options.threshold {
        // Match Python's current per-run override, including its reset to the
        // default lexicon while preserving weights.
        config = RoutingConfig {
            weights: config.weights,
            tiers: binary_tiers(threshold),
            classifier: None,
            lexicon: Lexicon::default(),
        };
    }
    let result = match score_complexity(&text, &config) {
        Ok(result) => result,
        Err(error) => {
            write_error(stderr, &format!("wayfinder-router: {error}"));
            return EXIT_CONFIG;
        }
    };
    if options.json {
        if serde_json::to_writer_pretty(&mut *stdout, &result).is_err() || writeln!(stdout).is_err()
        {
            write_error(stderr, "wayfinder-router: cannot write JSON output");
            return EXIT_CONFIG;
        }
    } else {
        let rendered = render_human(&result, options.explain.then_some(&config.weights));
        write_output(stdout, &rendered);
    }
    EXIT_OK
}

#[derive(Debug, PartialEq)]
struct RouteOptions {
    prompt: String,
    threshold: Option<f64>,
    json: bool,
    explain: bool,
}

fn parse_route_options(arguments: &[String]) -> Result<Option<RouteOptions>, String> {
    if arguments
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        return Ok(None);
    }
    let mut prompt = None;
    let mut threshold = None;
    let mut json = false;
    let mut explain = false;
    let mut index = 0_usize;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--json" => json = true,
            "--explain" => explain = true,
            "--threshold" => {
                index = index.saturating_add(1);
                let value = arguments
                    .get(index)
                    .ok_or_else(|| "--threshold needs a value".to_owned())?;
                threshold = Some(parse_threshold(value)?);
            }
            value if value.starts_with("--threshold=") => {
                let raw = value.strip_prefix("--threshold=").unwrap_or_default();
                threshold = Some(parse_threshold(raw)?);
            }
            value if value.starts_with('-') && value != "-" => {
                return Err(format!("unrecognized route argument: {value}"));
            }
            value => {
                if prompt.replace(value.to_owned()).is_some() {
                    return Err("route accepts exactly one prompt file or '-'".to_owned());
                }
            }
        }
        index = index.saturating_add(1);
    }
    let prompt = prompt.ok_or_else(|| "route needs a prompt file or '-'".to_owned())?;
    Ok(Some(RouteOptions {
        prompt,
        threshold,
        json,
        explain,
    }))
}

fn parse_threshold(raw: &str) -> Result<f64, String> {
    let value = raw
        .parse::<f64>()
        .map_err(|_| "--threshold must be a number between 0.0 and 1.0".to_owned())?;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err("--threshold must be a number between 0.0 and 1.0".to_owned());
    }
    Ok(value)
}

fn render_human(result: &ComplexityScore, weights: Option<&Weights>) -> String {
    let mode = match result.mode {
        RoutingMode::Tiered => "tiered",
        RoutingMode::Classifier => "classifier",
    };
    let mut lines = vec![
        format!("Recommended Model: {}", result.recommendation),
        format!("Complexity Score: {:.2}  (mode: {mode})", result.score),
    ];
    if let Some(tiers) = &result.tiers {
        lines.push(String::new());
        lines.push("Tiers:".to_owned());
        for tier in tiers {
            let marker = if tier.model == result.recommendation {
                " <-"
            } else {
                ""
            };
            lines.push(format!(
                "  >= {:.2}  {}{marker}",
                tier.min_score, tier.model
            ));
        }
    }
    if let Some(models) = &result.models {
        lines.push(String::new());
        lines.push(format!("Candidates: {}", models.join(", ")));
    }
    if let Some(weights) = weights {
        lines.push(String::new());
        lines.push("Score Breakdown (feature: value  norm x weight = contribution):".to_owned());
        for contribution in explain_score(&result.features, weights) {
            let weight = general_number(contribution.weight);
            lines.push(format!(
                "  {:<18} {:>5}  {:.2} x {:<4} = {:.3}",
                contribution.name,
                contribution.value,
                contribution.normalized,
                weight,
                contribution.contribution
            ));
        }
    } else {
        lines.push(String::new());
        lines.push("Contributing Features:".to_owned());
        for name in FEATURE_ORDER {
            let label = name
                .split('_')
                .map(title_word)
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(format!(
                "  {label}: {}",
                result.features.get_named(name).unwrap_or(0)
            ));
        }
    }
    lines.join("\n")
}

fn title_word(word: &str) -> String {
    let mut characters = word.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    format!("{}{}", first.to_ascii_uppercase(), characters.as_str())
}

fn general_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn development_tier_policy() -> TierOrderPolicy {
    match std::env::var("WAYFINDER_RUST_TIER_ORDER").as_deref() {
        Ok("compat") => TierOrderPolicy::CompatibilitySort,
        _ => TierOrderPolicy::StrictInput,
    }
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    let rendered = path.to_string_lossy();
    let Some(rest) = rendered.strip_prefix("~/") else {
        return path;
    };
    let Some(home) = std::env::var_os("HOME") else {
        return path;
    };
    PathBuf::from(home).join(rest)
}

fn write_output(stream: &mut dyn Write, message: &str) {
    let _ = writeln!(stream, "{message}");
}

fn write_error(stream: &mut dyn Write, message: &str) {
    let _ = writeln!(stream, "{message}");
}

const TOP_LEVEL_USAGE: &str = "usage: wayfinder-router [-h] [--version] COMMAND ...";
const TOP_LEVEL_HELP: &str = "usage: wayfinder-router [-h] [--version] COMMAND ...\n\nNative deterministic prompt-complexity router.\n\nCommands:\n  route          Score a prompt and recommend a model.\n  serve          Run the bounded HTTP gateway.\n  service        Manage the always-on launchd/systemd user service.\n  keys new       Mint a virtual gateway key and its hashed config entry.\n  capabilities   Emit the versioned helper capability handshake.\n  app-setup-init Create a bounded desktop setup config.\n  app-configure-chatgpt\n                 Add a bounded ChatGPT account route for Desktop.\n  config read-routing | apply-routing\n                 Read or replace the desktop-owned routing fragment.\n  apple-foundation-live-smoke\n                 Exercise the bounded Apple Foundation Models delivery path.";
const ROUTE_HELP: &str = "usage: wayfinder-router route [-h] [--threshold THRESHOLD] [--json] [--explain] prompt\n\nScore a prompt and recommend a model.";
const KEYS_HELP: &str = "usage: wayfinder-router keys new [--id ID] [--workspace ID] [--json]\n\nMint a virtual gateway key. The plaintext is printed once; only its SHA-256 hash belongs in config.";
const SERVE_HELP: &str = "usage: wayfinder-router serve [-h] [--host HOST] [--port PORT] [--surface local|data-plane] [--dry-run] [--timeout TIMEOUT] [--config CONFIG]\n\nRun the bounded HTTP gateway. Non-loopback serving requires the authenticated data-plane surface.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_parser_accepts_python_option_order() -> Result<(), String> {
        let options = parse_route_options(&[
            "--json".to_owned(),
            "-".to_owned(),
            "--threshold=0.0".to_owned(),
            "--explain".to_owned(),
        ])?
        .ok_or_else(|| "route unexpectedly requested help".to_owned())?;
        assert_eq!(
            options,
            RouteOptions {
                prompt: "-".to_owned(),
                threshold: Some(0.0),
                json: true,
                explain: true,
            }
        );
        Ok(())
    }

    #[test]
    fn route_stdin_json_is_schema_three() -> Result<(), Box<dyn std::error::Error>> {
        let mut stdin = "Say hello.".as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            vec!["route".into(), "-".into(), "--json".into()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&stderr));
        let payload: serde_json::Value = serde_json::from_slice(&stdout)?;
        assert_eq!(payload.get("schema_version"), Some(&json!("3")));
        assert_eq!(payload.get("recommendation"), Some(&json!("local")));
        Ok(())
    }

    #[test]
    fn threshold_zero_routes_empty_prompt_up() -> Result<(), Box<dyn std::error::Error>> {
        let mut stdin = "".as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            vec![
                "route".into(),
                "-".into(),
                "--threshold".into(),
                "0.0".into(),
                "--json".into(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&stderr));
        let payload: serde_json::Value = serde_json::from_slice(&stdout)?;
        assert_eq!(payload.get("recommendation"), Some(&json!("cloud")));
        Ok(())
    }

    #[test]
    fn capabilities_report_rust_only_gateway() -> Result<(), Box<dyn std::error::Error>> {
        let mut stdin = "".as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            vec!["capabilities".into(), "--json".into()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_OK);
        let payload: serde_json::Value = serde_json::from_slice(&stdout)?;
        assert_eq!(payload.get("implementation"), Some(&json!("rust")));
        assert_eq!(payload.get("gateway_ready"), Some(&json!(true)));
        let mechanisms = payload["credential_mechanisms"]
            .as_array()
            .ok_or("credential mechanisms must be an array")?;
        assert!(mechanisms.contains(&json!("environment-reference")));
        assert!(mechanisms.contains(&json!("legacy-command-reference")));
        #[cfg(target_os = "macos")]
        assert!(mechanisms.contains(&json!("xpc-credential-broker-v1")));
        assert_eq!(
            payload["native_commands"],
            json!([
                "route",
                "serve",
                "service",
                "keys new",
                "capabilities",
                "app-setup-init",
                "app-configure-chatgpt",
                "config read-routing",
                "config apply-routing",
                "apple-foundation-live-smoke"
            ])
        );
        assert!(payload["delegation"].is_null());
        assert_eq!(payload["delegated_commands"], json!([]));
        Ok(())
    }

    #[test]
    fn capabilities_report_canonical_target_architecture() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut stdin = "".as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            vec!["capabilities".into(), "--json".into()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&stderr));
        let payload: serde_json::Value = serde_json::from_slice(&stdout)?;
        let expected = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            std::env::consts::ARCH
        };
        assert_eq!(payload["target_architecture"], expected);
        Ok(())
    }

    #[test]
    fn removed_legacy_commands_fail_closed() {
        for command in [
            "calibrate",
            "recalibrate",
            "webchat",
            "ui",
            "chat",
            "onboard",
            "judge",
            "init",
            "doctor",
        ] {
            let mut stdin = "".as_bytes();
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let code = run(
                vec![OsString::from(command)],
                &mut stdin,
                &mut stdout,
                &mut stderr,
            );
            assert_eq!(code, EXIT_USAGE, "{command}");
            assert!(stdout.is_empty(), "{command}");
            assert_eq!(
                String::from_utf8_lossy(&stderr),
                format!("wayfinder-router: unsupported command: {command}\n")
            );
        }
    }

    #[test]
    fn keys_new_prints_one_valid_secret_and_only_its_hash_in_toml()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut stdin = "".as_bytes();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run(
            vec![
                "keys".into(),
                "new".into(),
                "--id".into(),
                "team a".into(),
                "--workspace".into(),
                "production".into(),
                "--json".into(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_OK, "{}", String::from_utf8_lossy(&stderr));
        let payload: serde_json::Value = serde_json::from_slice(&stdout)?;
        let key = payload["key"].as_str().ok_or("missing key")?;
        let hash = payload["hash"].as_str().ok_or("missing hash")?;
        let toml = payload["toml"].as_str().ok_or("missing TOML")?;
        assert!(key.starts_with("wf_"));
        assert_eq!(key.len(), 67);
        assert_eq!(hash, wayfinder_gateway::auth::hash_key(key));
        assert!(toml.contains("[gateway.keys.\"team a\"]"));
        assert!(toml.contains(hash));
        assert_eq!(payload["workspace"], "production");
        assert!(toml.contains("workspace = \"production\""));
        assert!(!toml.contains(key));
        Ok(())
    }

    #[test]
    fn serve_parser_covers_python_options_and_process_detection() -> Result<(), String> {
        let options = parse_serve_options(&[
            "--host=localhost".to_owned(),
            "--port".to_owned(),
            "0".to_owned(),
            "--surface=data-plane".to_owned(),
            "--dry-run".to_owned(),
            "--timeout=1.5".to_owned(),
            "--config".to_owned(),
            "fixture.toml".to_owned(),
        ])?
        .ok_or_else(|| "serve unexpectedly requested help".to_owned())?;
        assert_eq!(
            options,
            ServeOptions {
                host: "localhost".to_owned(),
                port: 0,
                surface: ServeSurface::DataPlane,
                dry_run: true,
                timeout: Some(1.5),
                config: Some(PathBuf::from("fixture.toml")),
            }
        );
        assert!(host_is_loopback("localhost"));
        assert!(host_is_loopback("[::1]"));
        assert!(!host_is_loopback("0.0.0.0"));
        assert!(!host_is_literal_loopback("localhost"));
        assert!(host_is_literal_loopback("127.0.0.1"));
        assert!(host_is_literal_loopback("[::1]"));
        assert!(!host_is_literal_loopback("127.evil.example"));
        assert!(is_serve_command(&[OsString::from("serve")]));
        assert!(!is_serve_command(&[OsString::from("route")]));
        Ok(())
    }

    #[test]
    fn serve_parser_rejects_unsafe_numeric_shapes() {
        for arguments in [
            vec!["--port=-1".to_owned()],
            vec!["--port=70000".to_owned()],
            vec!["--timeout=nan".to_owned()],
            vec!["--timeout=0".to_owned()],
            vec!["--host=".to_owned()],
            vec!["--surface=public".to_owned()],
        ] {
            assert!(parse_serve_options(&arguments).is_err());
        }
    }

    #[test]
    fn persistence_target_probe_prepares_parent_without_creating_state_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("wayfinder-persistence-probe-{}", Uuid::new_v4()));
        let target = root.join("nested").join("state.json");
        validate_persistence_target(&target, "test state")?;
        assert!(target.parent().is_some_and(Path::exists));
        assert!(!target.exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn persistence_target_probe_rejects_non_directory_parent()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "wayfinder-persistence-probe-file-{}",
            Uuid::new_v4()
        ));
        fs::write(&root, b"not a directory")?;
        let error = match validate_persistence_target(&root.join("state.json"), "test state") {
            Ok(()) => return Err("a file was accepted as the persistence directory".into()),
            Err(error) => error,
        };
        assert!(error.contains("cannot prepare test state directory"));
        fs::remove_file(root)?;
        Ok(())
    }

    #[test]
    fn savings_startup_rejects_blocked_atomic_temporary_path()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "wayfinder-savings-probe-blocked-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root)?;
        let target = root.join("savings.json");
        fs::create_dir(target.with_extension("json.tmp"))?;
        let error = match load_savings_ledger(&target) {
            Ok(_) => return Err("a blocked atomic-write path was accepted".into()),
            Err(error) => error,
        };
        assert!(error.contains("cannot persist savings ledger"));
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn serve_security_requires_explicit_authenticated_data_plane_for_network_exposure()
    -> Result<(), Box<dyn std::error::Error>> {
        let state = AppState::new(
            RoutingConfig::binary(0.5),
            vec![ConfiguredModel::new(
                "local",
                "http://127.0.0.1:11434/v1",
                "provider-local",
                None,
                true,
            )],
            false,
            "test",
        );
        let local_external = ServeOptions {
            host: "0.0.0.0".to_owned(),
            surface: ServeSurface::Local,
            port: 8_088,
            dry_run: false,
            timeout: None,
            config: None,
        };
        assert!(validate_serve_security(&local_external, &state).is_err());

        let managed_external = ServeOptions {
            surface: ServeSurface::DataPlane,
            ..local_external
        };
        assert!(validate_serve_security(&managed_external, &state).is_err());

        let mut config = GatewayConfig::default();
        config.keys.insert(
            "team-a".to_owned(),
            wayfinder_config::gateway::VirtualKey {
                hash: wayfinder_gateway::auth::hash_key("wf-secret"),
                tags: Vec::new(),
                workspace: None,
                budget: None,
                rate_limit: None,
                models: Vec::new(),
            },
        );
        let state = state.with_gateway_access(&config)?;
        assert!(validate_serve_security(&managed_external, &state).is_ok());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_credentials_are_preloaded_without_environment_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        fn test_environment(reference: &str) -> Option<SecretValue> {
            (reference == "WAYFINDER_TEST_ENV_WINS").then(|| SecretValue::new("environment-secret"))
        }

        let gateway = gateway_config_from_toml(
            r#"
[gateway.models.env]
base_url = "https://env.example/v1"
model = "env-model"
api_key_env = "WAYFINDER_TEST_ENV_WINS"
api_key_cmd = "exit 77"

[gateway.models.command]
base_url = "https://command.example/v1"
model = "command-model"
api_key_env = "WAYFINDER_TEST_COMMAND_FALLBACK"
api_key_cmd = "printf 'command-secret'"
"#,
            "credential-test",
        )?;
        let mut source = StartupCredentialSource::new(test_environment);
        let mut stderr = Vec::new();
        preload_command_credentials(&gateway, &mut source, &mut stderr).await;
        assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
        assert_eq!(source.command_values.len(), 1);
        assert!(
            source
                .command_values
                .contains_key("WAYFINDER_TEST_COMMAND_FALLBACK")
        );

        let env = source
            .resolve(Some("WAYFINDER_TEST_ENV_WINS"))?
            .ok_or_else(|| std::io::Error::other("missing environment credential"))?;
        let command = source
            .resolve(Some("WAYFINDER_TEST_COMMAND_FALLBACK"))?
            .ok_or_else(|| std::io::Error::other("missing command credential"))?;
        assert_eq!(env.len(), "environment-secret".len());
        assert_eq!(command.len(), "command-secret".len());
        let rendered = format!("{source:?}");
        assert!(!rendered.contains("environment-secret"));
        assert!(!rendered.contains("command-secret"));

        let models = configured_models(&gateway, &source);
        assert!(models.iter().all(ConfiguredModel::key_ready));
        Ok(())
    }
}
