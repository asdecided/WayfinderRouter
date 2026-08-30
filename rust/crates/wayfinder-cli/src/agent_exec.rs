//! No-write, fail-closed coding-agent launch contract.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use serde_json::{Value, json};

use crate::{EXIT_CONFIG, EXIT_OK, EXIT_USAGE, write_error, write_output};

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8088";
const PLACEHOLDER_TOKEN: &str = "wayfinder-local";
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_BODY_LIMIT: usize = 64 * 1024;
const EXEC_HELP: &str = "usage: wayfinder-router exec codex|claude-code|opencode [--endpoint http://LOOPBACK:PORT] -- PROGRAM [ARG ...]\n\nValidate a ready Wayfinder Router, apply bounded launch-only overrides, and replace this process with the coding agent. No client configuration or credential is read or written.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentClient {
    Codex,
    ClaudeCode,
    OpenCode,
}

impl AgentClient {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude-code" => Ok(Self::ClaudeCode),
            "opencode" => Ok(Self::OpenCode),
            "pi" => Err(
                "pi has no verified no-write endpoint override; review `wayfinder-router connect pi` instead"
                    .to_owned(),
            ),
            _ => Err(format!(
                "unsupported exec client: {value}; expected codex, claude-code, or opencode"
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::OpenCode => "opencode",
        }
    }

    const fn interface_path(self) -> &'static str {
        match self {
            Self::Codex => "/v1/responses",
            Self::ClaudeCode => "/v1/messages",
            Self::OpenCode => "/v1/chat/completions",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Endpoint {
    base: String,
    openai_base: String,
}

impl Endpoint {
    fn parse(value: &str) -> Result<Self, String> {
        let parsed = reqwest::Url::parse(value)
            .map_err(|_| "--endpoint must be an explicit loopback HTTP URL".to_owned())?;
        let host = parsed
            .host_str()
            .ok_or_else(|| "--endpoint must include a loopback host".to_owned())?;
        let numeric_host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        let is_loopback = host.eq_ignore_ascii_case("localhost")
            || numeric_host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if parsed.scheme() != "http"
            || !is_loopback
            || parsed.port().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(
                "--endpoint must be an explicit loopback HTTP URL with a port and no path, credentials, query, or fragment"
                    .to_owned(),
            );
        }
        let base = parsed.as_str().trim_end_matches('/').to_owned();
        Ok(Self {
            openai_base: format!("{base}/v1"),
            base,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

#[derive(Debug)]
struct ExecOptions {
    client: AgentClient,
    endpoint: Endpoint,
    command: Vec<OsString>,
}

enum ParseOutcome {
    Help,
    Options(ExecOptions),
}

#[derive(Debug, Eq, PartialEq)]
struct LaunchPlan {
    client: AgentClient,
    program: OsString,
    arguments: Vec<OsString>,
    environment: Vec<(&'static str, OsString)>,
}

pub(crate) fn run_exec_process(
    arguments: &[OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let options = match parse_options(arguments) {
        Ok(ParseOutcome::Help) => {
            write_output(stdout, EXEC_HELP);
            return EXIT_OK;
        }
        Ok(ParseOutcome::Options(options)) => options,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };
    if let Err(message) = probe_gateway(options.client, &options.endpoint) {
        write_error(
            stderr,
            &format!(
                "wayfinder-router: refusing to launch {}: {message}; no direct fallback was attempted",
                options.client.as_str()
            ),
        );
        return EXIT_CONFIG;
    }
    let plan = match build_launch_plan(options) {
        Ok(plan) => plan,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_CONFIG;
        }
    };
    launch(plan, stderr)
}

fn parse_options(arguments: &[OsString]) -> Result<ParseOutcome, String> {
    let separator = arguments
        .iter()
        .position(|argument| argument == OsStr::new("--"));
    let controls = separator.map_or(arguments, |index| &arguments[..index]);
    if controls
        .iter()
        .any(|argument| argument == OsStr::new("-h") || argument == OsStr::new("--help"))
    {
        return Ok(ParseOutcome::Help);
    }
    let separator = separator.ok_or_else(|| "exec requires `--` before PROGRAM".to_owned())?;
    let command = arguments[separator + 1..].to_vec();
    if command.is_empty() || command[0].is_empty() {
        return Err("exec needs a PROGRAM after `--`".to_owned());
    }
    let mut controls = arguments[..separator].iter();
    let client = controls
        .next()
        .ok_or_else(|| "exec needs a client".to_owned())?
        .to_str()
        .ok_or_else(|| "exec client must be valid UTF-8".to_owned())
        .and_then(AgentClient::parse)?;
    let mut endpoint = DEFAULT_ENDPOINT.to_owned();
    while let Some(argument) = controls.next() {
        let argument = argument
            .to_str()
            .ok_or_else(|| "exec options must be valid UTF-8".to_owned())?;
        match argument {
            "--endpoint" => {
                endpoint = controls
                    .next()
                    .ok_or_else(|| "--endpoint needs a value".to_owned())?
                    .to_str()
                    .ok_or_else(|| "--endpoint must be valid UTF-8".to_owned())?
                    .to_owned();
            }
            value if value.starts_with("--endpoint=") => {
                endpoint = value.trim_start_matches("--endpoint=").to_owned();
            }
            value => return Err(format!("unrecognized exec argument: {value}")),
        }
    }
    validate_command(client, &command)?;
    Ok(ParseOutcome::Options(ExecOptions {
        client,
        endpoint: Endpoint::parse(&endpoint)?,
        command,
    }))
}

fn validate_command(client: AgentClient, command: &[OsString]) -> Result<(), String> {
    let program = Path::new(&command[0])
        .file_name()
        .ok_or_else(|| "exec PROGRAM must name the selected client executable".to_owned())?;
    let expected = match client {
        AgentClient::Codex => &["codex", "codex.exe"][..],
        AgentClient::ClaudeCode => &["claude", "claude.exe", "claude-code", "claude-code.exe"][..],
        AgentClient::OpenCode => &["opencode", "opencode.exe"][..],
    };
    if !expected
        .iter()
        .any(|candidate| program == OsStr::new(candidate))
    {
        return Err(format!(
            "exec {} PROGRAM must be its client executable, not {}",
            client.as_str(),
            program.to_string_lossy()
        ));
    }
    let conflicts = match client {
        AgentClient::Codex => &["-c", "--config", "--oss", "--local-provider"][..],
        AgentClient::ClaudeCode => &[][..],
        AgentClient::OpenCode => &["-m", "--model"][..],
    };
    if command[1..].iter().any(|argument| {
        conflicts.iter().any(|conflict| {
            argument == OsStr::new(conflict)
                || argument.to_str().is_some_and(|argument| {
                    argument
                        .strip_prefix(*conflict)
                        .is_some_and(|suffix| suffix.starts_with('='))
                })
        })
    }) {
        return Err(format!(
            "exec {} rejects child endpoint, provider, or model overrides that could bypass Wayfinder",
            client.as_str()
        ));
    }
    Ok(())
}

fn build_launch_plan(options: ExecOptions) -> Result<LaunchPlan, String> {
    let mut command = options.command.into_iter();
    let program = command
        .next()
        .ok_or_else(|| "validated exec command lost its program".to_owned())?;
    let supplied_arguments = command.collect::<Vec<_>>();
    let (mut arguments, environment) = match options.client {
        AgentClient::Codex => {
            let endpoint_literal = serde_json::to_string(&options.endpoint.openai_base)
                .map_err(|_| "cannot encode the Codex endpoint override".to_owned())?;
            let arguments = [
                "--model".to_owned(),
                "auto".to_owned(),
                "--config".to_owned(),
                "model_provider=\"wayfinder\"".to_owned(),
                "--config".to_owned(),
                "model_providers.wayfinder.name=\"Wayfinder\"".to_owned(),
                "--config".to_owned(),
                format!("model_providers.wayfinder.base_url={endpoint_literal}"),
                "--config".to_owned(),
                "model_providers.wayfinder.env_key=\"WAYFINDER_LOCAL_TOKEN\"".to_owned(),
                "--config".to_owned(),
                "model_providers.wayfinder.wire_api=\"responses\"".to_owned(),
                "--config".to_owned(),
                "model_providers.wayfinder.requires_openai_auth=false".to_owned(),
                "--config".to_owned(),
                "model_providers.wayfinder.supports_websockets=false".to_owned(),
            ]
            .into_iter()
            .map(OsString::from)
            .collect();
            (
                arguments,
                vec![("WAYFINDER_LOCAL_TOKEN", OsString::from(PLACEHOLDER_TOKEN))],
            )
        }
        AgentClient::ClaudeCode => (
            Vec::new(),
            vec![
                ("ANTHROPIC_BASE_URL", OsString::from(&options.endpoint.base)),
                ("ANTHROPIC_AUTH_TOKEN", OsString::from(PLACEHOLDER_TOKEN)),
                ("ANTHROPIC_MODEL", OsString::from("auto")),
                (
                    "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY",
                    OsString::from("1"),
                ),
            ],
        ),
        AgentClient::OpenCode => {
            let config = json!({
                "$schema": "https://opencode.ai/config.json",
                "model": "wayfinder/auto",
                "provider": {
                    "wayfinder": {
                        "npm": "@ai-sdk/openai-compatible",
                        "name": "Wayfinder",
                        "options": {
                            "baseURL": options.endpoint.openai_base,
                            "apiKey": PLACEHOLDER_TOKEN,
                        },
                        "models": {
                            "auto": { "name": "Wayfinder Automatic" }
                        }
                    }
                }
            });
            (
                Vec::new(),
                vec![(
                    "OPENCODE_CONFIG_CONTENT",
                    OsString::from(config.to_string()),
                )],
            )
        }
    };
    arguments.extend(supplied_arguments);
    Ok(LaunchPlan {
        client: options.client,
        program,
        arguments,
        environment,
    })
}

fn probe_gateway(client_kind: AgentClient, endpoint: &Endpoint) -> Result<(), String> {
    let client = Client::builder()
        .connect_timeout(PROBE_TIMEOUT)
        .timeout(PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|_| "cannot prepare the bounded readiness probe".to_owned())?;

    let health = fetch_json(&client, &endpoint.url("/healthz"), false)
        .map_err(|message| format!("health probe failed: {message}"))?;
    validate_health(&health)?;

    let models = fetch_json(&client, &endpoint.url("/v1/models"), true)
        .map_err(|message| format!("model-capability probe failed: {message}"))?;
    validate_models(&models)?;

    let interface_url = endpoint.url(client_kind.interface_path());
    let status = client
        .get(&interface_url)
        .header("authorization", format!("Bearer {PLACEHOLDER_TOKEN}"))
        .send()
        .map_err(|_| {
            format!(
                "{} interface probe could not reach the Router",
                client_kind.as_str()
            )
        })?
        .status();
    if status != StatusCode::METHOD_NOT_ALLOWED {
        return Err(format!(
            "Router does not advertise the required {} interface (expected POST capability, got HTTP {status})",
            client_kind.interface_path()
        ));
    }
    Ok(())
}

fn fetch_json(client: &Client, url: &str, authorize: bool) -> Result<Value, String> {
    let mut request = client.get(url);
    if authorize {
        request = request.header("authorization", format!("Bearer {PLACEHOLDER_TOKEN}"));
    }
    let response = request
        .send()
        .map_err(|_| "Router is unreachable".to_owned())?;
    if !response.status().is_success() {
        return Err(format!("Router returned HTTP {}", response.status()));
    }
    parse_bounded_json(response)
}

fn parse_bounded_json(mut response: Response) -> Result<Value, String> {
    if response
        .content_length()
        .is_some_and(|length| length > PROBE_BODY_LIMIT as u64)
    {
        return Err("Router response exceeded the bounded probe size".to_owned());
    }
    let mut body = Vec::new();
    response
        .by_ref()
        .take((PROBE_BODY_LIMIT + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|_| "could not read the Router response".to_owned())?;
    if body.len() > PROBE_BODY_LIMIT {
        return Err("Router response exceeded the bounded probe size".to_owned());
    }
    serde_json::from_slice(&body).map_err(|_| "Router returned invalid JSON".to_owned())
}

fn validate_health(value: &Value) -> Result<(), String> {
    let status = value["status"]
        .as_str()
        .ok_or_else(|| "health response has no status".to_owned())?;
    if !matches!(status, "ok" | "degraded") {
        return Err(format!("Router health is {status}"));
    }
    let models = bounded_string_set(&value["models"], "health models")?;
    let missing = if value.get("missing_keys").is_some() {
        bounded_string_set(&value["missing_keys"], "health missing_keys")?
    } else {
        BTreeSet::new()
    };
    if models.is_empty() || models.is_subset(&missing) {
        return Err("Router has no ready configured destination".to_owned());
    }
    Ok(())
}

fn bounded_string_set(value: &Value, label: &str) -> Result<BTreeSet<String>, String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{label} must be an array"))?;
    if values.len() > 1_024 {
        return Err(format!("{label} exceeded the bounded item count"));
    }
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= 256)
                .map(str::to_owned)
                .ok_or_else(|| format!("{label} contains an invalid identifier"))
        })
        .collect()
}

fn validate_models(value: &Value) -> Result<(), String> {
    if value["object"] != "list" {
        return Err("model-capability response is not a list".to_owned());
    }
    let models = value["data"]
        .as_array()
        .ok_or_else(|| "model-capability response has no data array".to_owned())?;
    if models.len() > 1_024 {
        return Err("model-capability response exceeded the bounded item count".to_owned());
    }
    let supports_auto = models.iter().any(|model| {
        model["id"] == "auto" && model["object"] == "model" && model["owned_by"] == "wayfinder"
    });
    if !supports_auto {
        return Err("Router does not advertise the Wayfinder-owned `auto` model".to_owned());
    }
    Ok(())
}

#[cfg(unix)]
fn launch(plan: LaunchPlan, stderr: &mut dyn Write) -> i32 {
    use std::os::unix::process::CommandExt;

    let program = plan.program.clone();
    let mut command = Command::new(&plan.program);
    command.args(&plan.arguments);
    for (name, value) in &plan.environment {
        command.env(name, value);
    }
    let error = command.exec();
    write_error(
        stderr,
        &format!(
            "wayfinder-router: cannot launch {} with {}: {error}",
            plan.client.as_str(),
            program.to_string_lossy()
        ),
    );
    EXIT_CONFIG
}

#[cfg(not(unix))]
fn launch(plan: LaunchPlan, stderr: &mut dyn Write) -> i32 {
    let program = plan.program.clone();
    let mut command = Command::new(&plan.program);
    command.args(&plan.arguments);
    for (name, value) in &plan.environment {
        command.env(name, value);
    }
    match command.status() {
        Ok(status) => status.code().unwrap_or(EXIT_CONFIG),
        Err(error) => {
            write_error(
                stderr,
                &format!(
                    "wayfinder-router: cannot launch {} with {}: {error}",
                    plan.client.as_str(),
                    program.to_string_lossy()
                ),
            );
            EXIT_CONFIG
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};

    use super::*;

    type TestError = Box<dyn std::error::Error + Send + Sync>;
    type TestResult<T = ()> = Result<T, TestError>;

    fn options(arguments: &[&str]) -> Result<ExecOptions, String> {
        let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
        match parse_options(&arguments)? {
            ParseOutcome::Help => Err("unexpected help".to_owned()),
            ParseOutcome::Options(options) => Ok(options),
        }
    }

    fn ready_plan(arguments: &[&str]) -> TestResult<LaunchPlan> {
        let options = options(arguments).map_err(std::io::Error::other)?;
        Ok(build_launch_plan(options).map_err(std::io::Error::other)?)
    }

    fn parse_test_endpoint(value: &str) -> TestResult<Endpoint> {
        Ok(Endpoint::parse(value).map_err(std::io::Error::other)?)
    }

    fn expected_error<T>(result: Result<T, String>, context: &str) -> TestResult<String> {
        match result {
            Err(message) => Ok(message),
            Ok(_) => Err(std::io::Error::other(context).into()),
        }
    }

    fn response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn probe_server(responses: Vec<String>) -> TestResult<(String, JoinHandle<TestResult>)> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let endpoint = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let handle = thread::spawn(move || -> TestResult {
            for response in responses {
                let (mut stream, _) = listener.accept()?;
                let mut reader = BufReader::new(stream.try_clone()?);
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line)? == 0 || matches!(line.as_str(), "\r\n" | "\n") {
                        break;
                    }
                }
                stream.write_all(response.as_bytes())?;
            }
            Ok(())
        });
        Ok((endpoint, handle))
    }

    fn finish_server(server: JoinHandle<TestResult>) -> TestResult {
        server
            .join()
            .map_err(|_| std::io::Error::other("probe server thread panicked"))?
    }

    fn healthy_responses() -> Vec<String> {
        vec![
            response(
                "200 OK",
                r#"{"status":"degraded","models":["local","cloud"],"offline":false,"missing_keys":["cloud"]}"#,
            ),
            response(
                "200 OK",
                r#"{"object":"list","data":[{"id":"auto","object":"model","created":0,"owned_by":"wayfinder"}]}"#,
            ),
            response("405 Method Not Allowed", ""),
        ]
    }

    #[test]
    fn options_are_bounded_and_fail_closed() -> TestResult {
        let cases: &[(&[&str], &str)] = &[
            (&["codex", "codex"], "requires `--`"),
            (&["codex", "--"], "needs a PROGRAM"),
            (
                &["pi", "--", "pi"],
                "no verified no-write endpoint override",
            ),
            (&["crush", "--", "crush"], "unsupported exec client"),
            (&["codex", "--", "env", "codex"], "client executable"),
            (&["codex", "--", "codex", "--oss"], "could bypass Wayfinder"),
            (
                &["opencode", "--", "opencode", "--model=other/direct"],
                "could bypass Wayfinder",
            ),
        ];
        for (arguments, expected) in cases {
            let message = expected_error(options(arguments), "options unexpectedly succeeded")?;
            assert!(message.contains(expected), "{message}");
        }
        Ok(())
    }

    #[test]
    fn endpoint_accepts_only_explicit_loopback_http_origins() {
        for valid in [
            "http://127.0.0.1:8088",
            "http://localhost:8088/",
            "http://[::1]:8088",
        ] {
            assert!(Endpoint::parse(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "https://127.0.0.1:8088",
            "http://127.0.0.1",
            "http://0.0.0.0:8088",
            "http://localhost:8088@evil.example",
            "http://localhost:8088/v1",
            "http://localhost:8088?next=evil",
        ] {
            assert!(Endpoint::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn launch_plans_apply_only_bounded_client_overrides() -> TestResult {
        let codex = ready_plan(&["codex", "--", "codex", "exec"])?;
        assert_eq!(codex.program, "codex");
        assert!(codex.arguments.iter().any(|value| value == "auto"));
        assert!(
            codex
                .arguments
                .iter()
                .any(|value| { value == "model_providers.wayfinder.wire_api=\"responses\"" })
        );
        assert_eq!(codex.arguments.last(), Some(&OsString::from("exec")));
        assert_eq!(
            codex.environment,
            vec![("WAYFINDER_LOCAL_TOKEN", OsString::from(PLACEHOLDER_TOKEN))]
        );

        let claude = ready_plan(&["claude-code", "--", "claude", "--verbose"])?;
        assert_eq!(claude.arguments, [OsString::from("--verbose")]);
        assert!(
            claude
                .environment
                .contains(&("ANTHROPIC_BASE_URL", OsString::from(DEFAULT_ENDPOINT)))
        );
        assert!(
            claude
                .environment
                .contains(&("ANTHROPIC_MODEL", OsString::from("auto")))
        );

        let opencode = ready_plan(&[
            "opencode",
            "--endpoint",
            "http://localhost:9088",
            "--",
            "opencode",
        ])?;
        let config_text = opencode
            .environment
            .iter()
            .find(|(name, _)| *name == "OPENCODE_CONFIG_CONTENT")
            .and_then(|(_, value)| value.to_str())
            .ok_or_else(|| std::io::Error::other("missing inline OpenCode config"))?;
        let config: Value = serde_json::from_str(config_text)?;
        assert_eq!(config["model"], "wayfinder/auto");
        assert_eq!(
            config["provider"]["wayfinder"]["options"]["baseURL"],
            "http://localhost:9088/v1"
        );
        assert_eq!(
            config["provider"]["wayfinder"]["options"]["apiKey"],
            PLACEHOLDER_TOKEN
        );
        Ok(())
    }

    #[test]
    fn probe_requires_health_auto_capability_and_client_interface() -> TestResult {
        let (endpoint, server) = probe_server(healthy_responses())?;
        assert_eq!(
            probe_gateway(AgentClient::Codex, &parse_test_endpoint(&endpoint)?),
            Ok(())
        );
        finish_server(server)
    }

    #[test]
    fn probe_rejects_unready_or_incompatible_routers() -> TestResult {
        let (endpoint, server) = probe_server(vec![response(
            "200 OK",
            r#"{"status":"degraded","models":["cloud"],"offline":false,"missing_keys":["cloud"]}"#,
        )])?;
        let error = expected_error(
            probe_gateway(AgentClient::OpenCode, &parse_test_endpoint(&endpoint)?),
            "probe unexpectedly accepted no ready destinations",
        )?;
        assert!(error.contains("no ready configured destination"));
        finish_server(server)?;

        let health = response(
            "200 OK",
            r#"{"status":"ok","models":["local"],"offline":true}"#,
        );
        let models = response(
            "200 OK",
            r#"{"object":"list","data":[{"id":"auto","object":"model","owned_by":"wayfinder"}]}"#,
        );
        let (endpoint, server) = probe_server(vec![health, models, response("404 Not Found", "")])?;
        let error = expected_error(
            probe_gateway(AgentClient::OpenCode, &parse_test_endpoint(&endpoint)?),
            "probe unexpectedly accepted a missing client interface",
        )?;
        assert!(error.contains("required /v1/chat/completions interface"));
        finish_server(server)
    }
}
