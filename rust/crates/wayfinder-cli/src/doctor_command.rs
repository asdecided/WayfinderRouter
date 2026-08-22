//! Bounded, read-only workstation diagnosis for native and Omarchy surfaces.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wayfinder_config::gateway::{GatewayConfig, gateway_config_from_toml};
use wayfinder_config::{
    CONFIG_PATH_ENV, TierOrderPolicy, find_config_file, routing_config_from_toml,
};

use crate::service_command::{ServiceInspection, find_executable, inspect_service};
use crate::{
    EXIT_CONFIG, EXIT_OK, EXIT_USAGE, expand_tilde, product_version, write_error, write_output,
};

const DEFAULT_CONFIG: &str = "wayfinder-router.toml";
const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8088";
const DOCTOR_HELP: &str = "usage: wayfinder-router doctor [--config PATH] [--json]";
const CONFIG_LIMIT: usize = 1024 * 1024;
const DISCOVERY_FILE_LIMIT: usize = 256 * 1024;
const PROVENANCE_LIMIT: usize = 16 * 1024;
const SOCKET_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug)]
struct DoctorOptions {
    explicit: Option<PathBuf>,
    json_output: bool,
}

#[derive(Debug)]
struct DiagnosticInput<'a> {
    path: &'a Path,
    config_valid: bool,
    config_failure: Option<&'a str>,
    gateway: Option<&'a GatewayConfig>,
    missing_environment: &'a [String],
    gateway_reachable: bool,
    service: ServiceInspection,
    service_config_matches: Option<bool>,
    provenance: Value,
    runtimes: Vec<Value>,
    agents: Vec<Value>,
    environment: Value,
}

pub(crate) fn run_doctor(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        write_output(stdout, DOCTOR_HELP);
        return EXIT_OK;
    }
    let options = match parse_options(arguments) {
        Ok(options) => options,
        Err(message) => return usage_error(stderr, &message),
    };
    let path = find_config_file(Path::new("."), options.explicit.as_deref())
        .or(options.explicit)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
    let source = match read_bounded_text(&path, CONFIG_LIMIT) {
        Ok(source) => source,
        Err(error) => {
            return config_failure(
                &path,
                "config-unreadable",
                &format!("cannot read {}: {error}", path.display()),
                options.json_output,
                stdout,
                stderr,
            );
        }
    };
    if let Err(error) = routing_config_from_toml(
        &source,
        &path.display().to_string(),
        None,
        TierOrderPolicy::StrictInput,
    ) {
        return config_failure(
            &path,
            "routing-invalid",
            &error.to_string(),
            options.json_output,
            stdout,
            stderr,
        );
    }
    let gateway = match gateway_config_from_toml(&source, &path.display().to_string()) {
        Ok(gateway) => gateway,
        Err(error) => {
            return config_failure(
                &path,
                "gateway-invalid",
                &error.to_string(),
                options.json_output,
                stdout,
                stderr,
            );
        }
    };

    let missing = missing_environment(&gateway);
    let gateway_reachable = socket_reachable(8_088);
    let evidence = collect_evidence(&gateway);
    let service_config_matches = unit_references_config(&evidence.service, &path);
    let report = build_report(DiagnosticInput {
        path: &path,
        config_valid: true,
        config_failure: None,
        gateway: Some(&gateway),
        missing_environment: &missing,
        gateway_reachable,
        service: evidence.service,
        service_config_matches,
        provenance: evidence.provenance,
        runtimes: evidence.runtimes,
        agents: evidence.agents,
        environment: evidence.environment,
    });
    if options.json_output {
        if write_json(stdout, &report).is_err() {
            return EXIT_CONFIG;
        }
    } else {
        write_text_report(stdout, &report);
    }
    if report["ok"] == true {
        EXIT_OK
    } else {
        EXIT_CONFIG
    }
}

fn parse_options(arguments: &[String]) -> Result<DoctorOptions, String> {
    let mut explicit = env::var_os(CONFIG_PATH_ENV)
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
                None => return Err("--config needs a value".to_owned()),
            },
            value => return Err(format!("unrecognized doctor argument: {value}")),
        }
        index += 1;
    }
    Ok(DoctorOptions {
        explicit,
        json_output,
    })
}

fn config_failure(
    path: &Path,
    failure: &str,
    text_detail: &str,
    json_output: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if !json_output {
        write_error(stderr, &format!("wayfinder-router: {text_detail}"));
        return EXIT_CONFIG;
    }
    let evidence = collect_evidence_without_config();
    let service_config_matches = unit_references_config(&evidence.service, path);
    let report = build_report(DiagnosticInput {
        path,
        config_valid: false,
        config_failure: Some(failure),
        gateway: None,
        missing_environment: &[],
        gateway_reachable: socket_reachable(8_088),
        service: evidence.service,
        service_config_matches,
        provenance: evidence.provenance,
        runtimes: evidence.runtimes,
        agents: evidence.agents,
        environment: evidence.environment,
    });
    if write_json(stdout, &report).is_err() {
        write_error(stderr, "wayfinder-router: could not write doctor report");
    }
    EXIT_CONFIG
}

struct CollectedEvidence {
    service: ServiceInspection,
    provenance: Value,
    runtimes: Vec<Value>,
    agents: Vec<Value>,
    environment: Value,
}

fn collect_evidence(gateway: &GatewayConfig) -> CollectedEvidence {
    collect_evidence_inner(Some(gateway))
}

fn collect_evidence_without_config() -> CollectedEvidence {
    collect_evidence_inner(None)
}

fn collect_evidence_inner(gateway: Option<&GatewayConfig>) -> CollectedEvidence {
    let home = home_dir();
    let config_home = config_home(home.as_deref());
    let executable = env::current_exe().ok();
    let state_home = state_home(home.as_deref());
    let provenance_path = state_home
        .as_deref()
        .map(|path| path.join("wayfinder/omarchy-router-install"));
    let provenance = provenance_report(
        provenance_path.as_deref(),
        executable.as_deref(),
        product_version(),
    );

    let runtime_names = ["ollama", "lms", "vllm", "llama-server"];
    let agent_names = ["codex", "claude", "opencode", "pi"];
    let mut installed = BTreeMap::new();
    for name in runtime_names.into_iter().chain(agent_names) {
        installed.insert(name.to_owned(), find_executable(name));
    }
    let reachable_ports = [11_434, 1_234, 8_000, 8_080]
        .into_iter()
        .filter(|port| socket_reachable(*port))
        .collect::<BTreeSet<_>>();
    let runtimes = runtime_reports(gateway, &installed, &reachable_ports);

    let mut agent_environment = BTreeMap::new();
    agent_environment.insert(
        "ANTHROPIC_BASE_URL".to_owned(),
        env::var("ANTHROPIC_BASE_URL").ok(),
    );
    agent_environment.insert(
        "ANTHROPIC_AUTH_TOKEN".to_owned(),
        env::var_os("ANTHROPIC_AUTH_TOKEN")
            .filter(|value| !value.is_empty())
            .map(|_| "present".to_owned()),
    );
    let agents = agent_reports(
        home.as_deref(),
        config_home.as_deref(),
        &installed,
        &agent_environment,
    );
    let environment = json!({
        "os": env::consts::OS,
        "architecture": env::consts::ARCH,
        "omarchy": find_executable("omarchy").is_some(),
        "omarchy_shell": find_executable("omarchy-shell").is_some(),
        "quickshell": find_executable("quickshell").is_some(),
    });
    CollectedEvidence {
        service: inspect_service(),
        provenance,
        runtimes,
        agents,
        environment,
    }
}

fn build_report(input: DiagnosticInput<'_>) -> Value {
    let destinations = input.gateway.map_or(0, |gateway| gateway.models.len());
    let policy_healthy = input.config_valid && destinations > 0;
    let healthy = policy_healthy && input.missing_environment.is_empty();
    let service_active = input.service.state == "active";
    let workstation_ready = healthy && service_active && input.gateway_reachable;

    let mut checks = Vec::new();
    checks.push(check(
        "config",
        if input.config_valid { "pass" } else { "fail" },
        input.config_failure.unwrap_or("valid"),
    ));
    checks.push(check(
        "destinations",
        if destinations > 0 { "pass" } else { "fail" },
        &format!("{destinations} configured"),
    ));
    checks.push(check(
        "environment",
        if input.missing_environment.is_empty() {
            "pass"
        } else {
            "warn"
        },
        if input.missing_environment.is_empty() {
            "all referenced variables are present".to_owned()
        } else {
            format!("missing {}", input.missing_environment.join(", "))
        }
        .as_str(),
    ));
    checks.push(check(
        "service",
        if service_active {
            "pass"
        } else if input.service.installed {
            "warn"
        } else {
            "fail"
        },
        &input.service.state,
    ));
    checks.push(check(
        "service-config",
        match input.service_config_matches {
            Some(true) => "pass",
            Some(false) => "warn",
            None => "info",
        },
        match input.service_config_matches {
            Some(true) => "unit references the selected policy",
            Some(false) => "unit does not reference the selected policy",
            None => "unit config reference unavailable",
        },
    ));
    checks.push(check(
        "gateway",
        if input.gateway_reachable {
            "pass"
        } else {
            "warn"
        },
        if input.gateway_reachable {
            "loopback socket reachable"
        } else {
            "loopback socket unreachable"
        },
    ));
    let provenance_status = input.provenance["status"].as_str().unwrap_or("unreadable");
    checks.push(check(
        "provenance",
        match provenance_status {
            "verified" => "pass",
            "absent" => "info",
            _ => "warn",
        },
        provenance_status,
    ));

    let mut remediation = Vec::new();
    if !input.config_valid {
        remediation.push(remediation_item(
            "config.repair",
            "Repair or select a valid Wayfinder policy, then run doctor again.",
            None,
            &[],
        ));
    } else if destinations == 0 {
        remediation.push(remediation_item(
            "config.destinations",
            "Add at least one eligible destination to the policy.",
            None,
            &[],
        ));
    }
    if !input.missing_environment.is_empty() {
        remediation.push(remediation_item(
            "environment.configure",
            &format!(
                "Provide the referenced variables without placing their values in QML: {}.",
                input.missing_environment.join(", ")
            ),
            None,
            &[],
        ));
    }
    if !input.service.installed {
        remediation.push(remediation_item(
            "service.install",
            "Install the reviewed user service for this policy.",
            Some("wayfinder-router"),
            &[
                "service",
                "install",
                "--config",
                &input.path.display().to_string(),
            ],
        ));
    } else if input.service_config_matches == Some(false) {
        remediation.push(remediation_item(
            "service.reinstall",
            "Reinstall the user service so it references the selected policy.",
            Some("wayfinder-router"),
            &[
                "service",
                "install",
                "--config",
                &input.path.display().to_string(),
            ],
        ));
    } else if !service_active {
        let program = if input.service.platform == "systemd-user" && input.service.manager_available
        {
            Some("systemctl")
        } else {
            None
        };
        let arguments = if program.is_some() {
            vec!["--user", "start", "wayfinder-router.service"]
        } else {
            Vec::new()
        };
        remediation.push(remediation_item(
            "service.start",
            "Start the installed user service, then run doctor again.",
            program,
            &arguments,
        ));
    } else if !input.gateway_reachable {
        remediation.push(remediation_item(
            "gateway.inspect",
            "The service is active but the configured loopback gateway is unreachable; inspect service status and logs.",
            Some("wayfinder-router"),
            &["service", "status"],
        ));
    }
    if matches!(provenance_status, "mismatch" | "unreadable") {
        remediation.push(remediation_item(
            "provenance.reinstall",
            "Reinstall the Router from a reviewed, checksum-pinned Omarchy plugin release.",
            None,
            &[],
        ));
    }
    for runtime in &input.runtimes {
        if runtime["configured"] == true && runtime["socket_reachable"] == false {
            let id = runtime["id"].as_str().unwrap_or("runtime");
            remediation.push(remediation_item(
                &format!("runtime.{id}.start"),
                &format!("Start the configured {id} runtime or update its explicit endpoint."),
                None,
                &[],
            ));
        }
    }
    for agent in &input.agents {
        if agent["installed"] == true && agent["configured"] == false {
            let id = agent["id"].as_str().unwrap_or("agent");
            remediation.push(remediation_item(
                &format!("agent.{id}.connect"),
                &format!("Review the native connection recipe for {id}; no agent file is changed."),
                Some("wayfinder-router"),
                &["connect", id],
            ));
        }
    }

    json!({
        "schema_version": "1",
        "doctor_contract": "omarchy-workstation-v1",
        "ok": healthy,
        "workstation_ready": workstation_ready,
        "config": input.path,
        "destinations": destinations,
        "missing_environment": input.missing_environment,
        "gateway_reachable": input.gateway_reachable,
        "gateway": DEFAULT_ENDPOINT,
        "router": {
            "version": product_version(),
            "executable": env::current_exe().ok(),
            "os": env::consts::OS,
            "architecture": env::consts::ARCH,
        },
        "provenance": input.provenance,
        "service": {
            "platform": input.service.platform,
            "unit_file": input.service.unit_file,
            "installed": input.service.installed,
            "manager_available": input.service.manager_available,
            "state": input.service.state,
            "config_matches": input.service_config_matches,
        },
        "environment": input.environment,
        "local_runtimes": input.runtimes,
        "agents": input.agents,
        "checks": checks,
        "remediation": remediation,
    })
}

fn missing_environment(gateway: &GatewayConfig) -> Vec<String> {
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
    required
        .into_iter()
        .filter(|name| env::var_os(name).is_none_or(|value| value.is_empty()))
        .collect()
}

fn runtime_reports(
    gateway: Option<&GatewayConfig>,
    installed: &BTreeMap<String, Option<PathBuf>>,
    reachable_ports: &BTreeSet<u16>,
) -> Vec<Value> {
    [
        ("ollama", "ollama", 11_434_u16),
        ("lm-studio", "lms", 1_234_u16),
        ("vllm", "vllm", 8_000_u16),
        ("llama.cpp", "llama-server", 8_080_u16),
    ]
    .into_iter()
    .map(|(id, binary, port)| {
        let configured = gateway.is_some_and(|gateway| {
            gateway.models.values().any(|model| {
                model
                    .base_url
                    .as_deref()
                    .is_some_and(|url| loopback_url_uses_port(url, port))
                    || model
                        .deployments
                        .iter()
                        .any(|deployment| loopback_url_uses_port(&deployment.base_url, port))
            })
        });
        json!({
            "id": id,
            "installed": installed.get(binary).is_some_and(Option::is_some),
            "configured": configured,
            "socket_reachable": reachable_ports.contains(&port),
            "endpoint": format!("http://127.0.0.1:{port}"),
            "identity_verified": false,
        })
    })
    .collect()
}

fn agent_reports(
    home: Option<&Path>,
    config_home: Option<&Path>,
    installed: &BTreeMap<String, Option<PathBuf>>,
    environment: &BTreeMap<String, Option<String>>,
) -> Vec<Value> {
    let codex_config = home
        .map(|path| path.join(".codex/config.toml"))
        .and_then(|path| read_optional_bounded(&path, DISCOVERY_FILE_LIMIT));
    let codex_configured = codex_config
        .as_deref()
        .is_some_and(|text| text.contains("wayfinder") && contains_gateway_endpoint(text));

    let claude_endpoint = environment
        .get("ANTHROPIC_BASE_URL")
        .and_then(Option::as_deref)
        .is_some_and(is_gateway_endpoint);
    let claude_auth_reference = environment
        .get("ANTHROPIC_AUTH_TOKEN")
        .is_some_and(Option::is_some);

    let opencode_configured = config_home.is_some_and(|root| {
        ["opencode/opencode.json", "opencode/opencode.jsonc"]
            .iter()
            .map(|relative| root.join(relative))
            .filter_map(|path| read_optional_bounded(&path, DISCOVERY_FILE_LIMIT))
            .any(|text| text.contains("wayfinder") && contains_gateway_endpoint(&text))
    });

    vec![
        json!({
            "id": "codex",
            "installed": installed.get("codex").is_some_and(Option::is_some),
            "configured": codex_configured,
            "verification": "unverified",
        }),
        json!({
            "id": "claude-code",
            "installed": installed.get("claude").is_some_and(Option::is_some),
            "configured": claude_endpoint && claude_auth_reference,
            "endpoint_configured": claude_endpoint,
            "auth_reference_present": claude_auth_reference,
            "verification": "unverified",
        }),
        json!({
            "id": "opencode",
            "installed": installed.get("opencode").is_some_and(Option::is_some),
            "configured": opencode_configured,
            "verification": "unverified",
        }),
        json!({
            "id": "pi",
            "installed": installed.get("pi").is_some_and(Option::is_some),
            "configured": false,
            "verification": "unverified",
        }),
    ]
}

fn provenance_report(record: Option<&Path>, executable: Option<&Path>, version: &str) -> Value {
    let Some(record) = record else {
        return json!({ "status": "unreadable", "record": Value::Null });
    };
    if !record.exists() {
        return json!({ "status": "absent", "record": record });
    }
    let contents = match read_bounded_text(record, PROVENANCE_LIMIT) {
        Ok(contents) => contents,
        Err(_) => return json!({ "status": "unreadable", "record": record }),
    };
    let values = contents
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let schema_matches = values
        .get("schema_version")
        .is_some_and(|value| value == "1");
    let plugin_matches = values
        .get("plugin_id")
        .is_some_and(|value| value == "io.github.asdecided.wayfinder");
    let version_matches = values
        .get("router_version")
        .is_some_and(|value| value == version);
    let path_matches = values
        .get("router_path")
        .zip(executable)
        .is_some_and(|(recorded, executable)| same_file(Path::new(recorded), executable));
    let binary_sha256_matches = values
        .get("binary_sha256")
        .zip(executable)
        .and_then(|(expected, executable)| hash_file(executable).map(|actual| actual == *expected))
        .unwrap_or(false);
    let verified = schema_matches
        && plugin_matches
        && version_matches
        && path_matches
        && binary_sha256_matches;
    json!({
        "status": if verified { "verified" } else { "mismatch" },
        "record": record,
        "schema_matches": schema_matches,
        "plugin_matches": plugin_matches,
        "version_matches": version_matches,
        "path_matches": path_matches,
        "binary_sha256_matches": binary_sha256_matches,
        "recorded_version": values.get("router_version").map(String::as_str).and_then(safe_token),
        "recorded_target": values.get("router_target").map(String::as_str).and_then(safe_token),
    })
}

fn remediation_item(id: &str, detail: &str, program: Option<&str>, arguments: &[&str]) -> Value {
    json!({
        "id": id,
        "detail": detail,
        "program": program,
        "arguments": arguments,
    })
}

fn check(id: &str, status: &str, detail: &str) -> Value {
    json!({ "id": id, "status": status, "detail": detail })
}

fn write_json(stdout: &mut dyn Write, report: &Value) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *stdout, report)?;
    writeln!(stdout)
}

fn write_text_report(stdout: &mut dyn Write, report: &Value) {
    write_output(
        stdout,
        &format!(
            "policy     ok ({})",
            report["config"].as_str().unwrap_or("unknown")
        ),
    );
    write_output(stdout, &format!("destinations {}", report["destinations"]));
    let missing = report["missing_environment"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
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
            if report["gateway_reachable"] == true {
                "reachable"
            } else {
                "not running"
            }
        ),
    );
    write_output(
        stdout,
        &format!(
            "service    {}",
            report["service"]["state"].as_str().unwrap_or("unknown")
        ),
    );
    write_output(
        stdout,
        &format!(
            "provenance {}",
            report["provenance"]["status"]
                .as_str()
                .unwrap_or("unreadable")
        ),
    );
}

fn read_bounded_text(path: &Path, limit: usize) -> io::Result<String> {
    let file = File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds diagnostic read limit",
        ));
    }
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds diagnostic read limit",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "file is not UTF-8"))
}

fn read_optional_bounded(path: &Path, limit: usize) -> Option<String> {
    read_bounded_text(path, limit).ok()
}

fn unit_references_config(service: &ServiceInspection, config: &Path) -> Option<bool> {
    if !service.installed {
        return None;
    }
    let unit = service.unit_file.as_deref()?;
    let contents = read_bounded_text(unit, DISCOVERY_FILE_LIMIT).ok()?;
    Some(contents.contains("--config") && contents.contains(&config.display().to_string()))
}

fn hash_file(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Some(encoded)
}

fn same_file(left: &Path, right: &Path) -> bool {
    fs::canonicalize(left)
        .ok()
        .zip(fs::canonicalize(right).ok())
        .is_some_and(|(left, right)| left == right)
}

fn safe_token(value: &str) -> Option<String> {
    (value.len() <= 128
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_".contains(character)))
    .then(|| value.to_owned())
}

fn socket_reachable(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), SOCKET_TIMEOUT).is_ok()
}

fn loopback_url_uses_port(url: &str, port: u16) -> bool {
    let normalized = url.trim_end_matches('/').to_ascii_lowercase();
    [
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
        format!("http://[::1]:{port}"),
    ]
    .iter()
    .any(|prefix| {
        normalized == *prefix
            || normalized
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with('/'))
    })
}

fn contains_gateway_endpoint(text: &str) -> bool {
    text.split(|character: char| character.is_whitespace() || "\"'".contains(character))
        .any(is_gateway_endpoint)
}

fn is_gateway_endpoint(value: &str) -> bool {
    let normalized = value.trim_end_matches('/').to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "http://127.0.0.1:8088"
            | "http://127.0.0.1:8088/v1"
            | "http://localhost:8088"
            | "http://localhost:8088/v1"
    )
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn config_home(home: Option<&Path>) -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|path| path.join(".config")))
}

fn state_home(home: Option<&Path>) -> Option<PathBuf> {
    env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|path| path.join(".local/state")))
}

fn usage_error(stderr: &mut dyn Write, message: &str) -> i32 {
    write_error(stderr, &format!("wayfinder-router: {message}"));
    EXIT_USAGE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_setup_command::preset_config;

    #[test]
    fn doctor_reports_a_valid_local_policy_without_calling_a_provider()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = env::temp_dir().join(format!("wayfinder-doctor-{}", uuid::Uuid::new_v4()));
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
        let payload: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(payload["schema_version"], "1");
        assert_eq!(payload["doctor_contract"], "omarchy-workstation-v1");
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["destinations"], 1);
        assert_eq!(payload["missing_environment"], json!([]));
        assert!(payload["local_runtimes"].is_array());
        assert!(payload["agents"].is_array());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn invalid_config_is_structured_in_json_mode() -> Result<(), Box<dyn std::error::Error>> {
        let root = env::temp_dir().join(format!("wayfinder-doctor-{}", uuid::Uuid::new_v4()));
        let path = root.join("broken.toml");
        fs::create_dir_all(&root)?;
        fs::write(&path, "not = [valid")?;
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
            EXIT_CONFIG
        );
        let payload: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["checks"][0]["id"], "config");
        assert_eq!(payload["checks"][0]["status"], "fail");
        assert!(stderr.is_empty());
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn agent_discovery_never_emits_secret_values() -> Result<(), Box<dyn std::error::Error>> {
        let root = env::temp_dir().join(format!("wayfinder-agents-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join(".codex"))?;
        fs::create_dir_all(root.join(".config/opencode"))?;
        fs::write(
            root.join(".codex/config.toml"),
            "model_provider = \"wayfinder\"\nbase_url = \"http://127.0.0.1:8088/v1\"\n",
        )?;
        fs::write(
            root.join(".config/opencode/opencode.json"),
            "{\"provider\":\"wayfinder\",\"baseURL\":\"http://127.0.0.1:8088/v1\"}",
        )?;
        let installed = BTreeMap::from([
            ("codex".to_owned(), Some(PathBuf::from("/bin/codex"))),
            ("claude".to_owned(), Some(PathBuf::from("/bin/claude"))),
            ("opencode".to_owned(), Some(PathBuf::from("/bin/opencode"))),
            ("pi".to_owned(), None),
        ]);
        let environment = BTreeMap::from([
            (
                "ANTHROPIC_BASE_URL".to_owned(),
                Some(DEFAULT_ENDPOINT.to_owned()),
            ),
            (
                "ANTHROPIC_AUTH_TOKEN".to_owned(),
                Some("secret-sentinel-must-not-escape".to_owned()),
            ),
        ]);
        let report = agent_reports(
            Some(&root),
            Some(&root.join(".config")),
            &installed,
            &environment,
        );
        let encoded = serde_json::to_string(&report)?;
        assert!(!encoded.contains("secret-sentinel"));
        assert_eq!(report[0]["configured"], true);
        assert_eq!(report[1]["configured"], true);
        assert_eq!(report[2]["configured"], true);
        assert!(
            report
                .iter()
                .all(|agent| agent["verification"] == "unverified")
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn runtime_signals_do_not_claim_identity() -> Result<(), Box<dyn std::error::Error>> {
        let gateway = gateway_config_from_toml(
            preset_config("local").ok_or("missing local preset")?,
            "fixture.toml",
        )?;
        let installed = BTreeMap::from([
            ("ollama".to_owned(), Some(PathBuf::from("/bin/ollama"))),
            ("lms".to_owned(), None),
            ("vllm".to_owned(), None),
            ("llama-server".to_owned(), None),
        ]);
        let reports = runtime_reports(Some(&gateway), &installed, &BTreeSet::from([11_434]));
        assert_eq!(reports[0]["id"], "ollama");
        assert_eq!(reports[0]["configured"], true);
        assert_eq!(reports[0]["socket_reachable"], true);
        assert_eq!(reports[0]["identity_verified"], false);
        Ok(())
    }

    #[test]
    fn provenance_verifies_exact_path_version_and_digest() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = env::temp_dir().join(format!("wayfinder-provenance-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        let executable = root.join("wayfinder-router");
        let record = root.join("omarchy-router-install");
        fs::write(&executable, b"bounded native router fixture")?;
        let digest = hash_file(&executable).ok_or("missing digest")?;
        fs::write(
            &record,
            format!(
                "schema_version=1\nplugin_id=io.github.asdecided.wayfinder\nrouter_version=2026.8.0\nrouter_target=x86_64-unknown-linux-gnu\nbinary_sha256={digest}\nrouter_path={}\n",
                executable.display()
            ),
        )?;
        let verified = provenance_report(Some(&record), Some(&executable), "2026.8.0");
        assert_eq!(verified["status"], "verified");
        fs::write(&executable, b"modified")?;
        let mismatch = provenance_report(Some(&record), Some(&executable), "2026.8.0");
        assert_eq!(mismatch["status"], "mismatch");
        assert_eq!(mismatch["binary_sha256_matches"], false);
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn service_config_check_is_read_only_and_exact() -> Result<(), Box<dyn std::error::Error>> {
        let root = env::temp_dir().join(format!("wayfinder-service-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        let unit = root.join("wayfinder-router.service");
        let config = root.join("selected.toml");
        fs::write(
            &unit,
            format!(
                "[Service]\nExecStart=/bin/wayfinder-router serve --config {}\n",
                config.display()
            ),
        )?;
        let service = ServiceInspection {
            platform: "systemd-user",
            unit_file: Some(unit),
            installed: true,
            manager_available: true,
            state: "active".to_owned(),
        };
        assert_eq!(unit_references_config(&service, &config), Some(true));
        assert_eq!(
            unit_references_config(&service, &root.join("different.toml")),
            Some(false)
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
