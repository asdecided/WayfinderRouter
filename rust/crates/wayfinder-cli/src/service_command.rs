//! Service-manager command implementation.
//!
//! Unit rendering is delegated to `wayfinder-service`; this module owns the
//! explicitly-invoked filesystem and process mutations.  The boundary is
//! injectable so tests never touch a real home directory or service manager.

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use wayfinder_service::units::{
    LAUNCHD_LABEL, SYSTEMD_UNIT_NAME, ServicePlatform, agent_path, detect_platform,
    launchd_plist_with_home, systemd_unit, systemd_unit_path,
};

use crate::{EXIT_CONFIG, EXIT_OK, EXIT_USAGE, write_error, write_output};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8_088;
const MANAGER_TIMEOUT: Duration = Duration::from_secs(10);
const MANAGER_OUTPUT_LIMIT: usize = 64 * 1_024;
const DETAIL_LIMIT: usize = 512;
const HEALTH_TIMEOUT: Duration = Duration::from_millis(1_500);
const HEALTH_RESPONSE_LIMIT: u64 = 64 * 1_024;

pub(crate) const SERVICE_HELP: &str = "usage: wayfinder-router service [-h] [--host HOST] [--port PORT] [--config CONFIG] [--print] {install,uninstall,status}\n\nRun the gateway as an always-on local service (macOS launchd / Linux systemd).";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceAction {
    Install,
    Uninstall,
    Status,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ServiceOptions {
    action: ServiceAction,
    host: String,
    port: u16,
    config: Option<String>,
    print: bool,
}

pub(crate) fn run_service(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let mut operations = RealServiceOperations;
    run_service_with(arguments, stdout, stderr, &mut operations)
}

fn run_service_with(
    arguments: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    operations: &mut dyn ServiceOperations,
) -> i32 {
    let options = match parse_service_options(arguments) {
        Ok(Some(options)) => options,
        Ok(None) => {
            write_output(stdout, SERVICE_HELP);
            return EXIT_OK;
        }
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_USAGE;
        }
    };

    let platform = operations.platform();
    if platform == ServicePlatform::Other {
        write_error(
            stderr,
            "wayfinder-router: service supports macOS (launchd) and Linux (systemd user units); elsewhere run `wayfinder-router serve` yourself.",
        );
        return EXIT_USAGE;
    }

    let home = match operations.home_dir() {
        Ok(home) => home,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_CONFIG;
        }
    };
    let executable = match operations.executable() {
        Ok(executable) => executable,
        Err(message) => {
            write_error(stderr, &format!("wayfinder-router: {message}"));
            return EXIT_CONFIG;
        }
    };
    let executable = match executable.to_str() {
        Some(executable) if !executable.is_empty() => executable.to_owned(),
        _ => {
            write_error(
                stderr,
                "wayfinder-router: executable path is not valid UTF-8",
            );
            return EXIT_CONFIG;
        }
    };
    let program_args = resolve_serve_args(&executable, &options);
    let endpoint = format!("http://{}:{}/v1", options.host, options.port);
    let log_dir = home.join("Library").join("Logs");

    let (unit_text, unit_file, manager_name) = match platform {
        ServicePlatform::MacOs => {
            let log_dir = match log_dir.to_str() {
                Some(log_dir) => log_dir,
                None => {
                    write_error(
                        stderr,
                        "wayfinder-router: service log path is not valid UTF-8",
                    );
                    return EXIT_CONFIG;
                }
            };
            let text =
                launchd_plist_with_home(&program_args, None, Some(log_dir), Some(home.as_path()));
            let path = match agent_path(Some(home.as_path())) {
                Ok(path) => path,
                Err(error) => {
                    write_error(stderr, &format!("wayfinder-router: {error}"));
                    return EXIT_CONFIG;
                }
            };
            (text, path, "launchctl")
        }
        ServicePlatform::Linux => {
            let path = match systemd_unit_path(Some(home.as_path())) {
                Ok(path) => path,
                Err(error) => {
                    write_error(stderr, &format!("wayfinder-router: {error}"));
                    return EXIT_CONFIG;
                }
            };
            (systemd_unit(&program_args, None), path, "systemctl")
        }
        ServicePlatform::Other => unreachable!("unsupported platform returned above"),
    };
    let manager = operations.which(manager_name);

    match options.action {
        ServiceAction::Install => install(
            &options,
            platform,
            &unit_text,
            &unit_file,
            &log_dir,
            manager.as_deref(),
            &endpoint,
            stdout,
            stderr,
            operations,
        ),
        ServiceAction::Uninstall => {
            uninstall(platform, &unit_file, manager.as_deref(), stderr, operations)
        }
        ServiceAction::Status => status(
            &options,
            platform,
            &unit_file,
            manager.as_deref(),
            &endpoint,
            stderr,
            operations,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn install(
    options: &ServiceOptions,
    platform: ServicePlatform,
    unit_text: &str,
    unit_file: &Path,
    log_dir: &Path,
    manager: Option<&Path>,
    endpoint: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    operations: &mut dyn ServiceOperations,
) -> i32 {
    if options.print {
        write_output(stdout, unit_text);
        return EXIT_OK;
    }
    let Some(parent) = unit_file.parent() else {
        write_error(stderr, "wayfinder-router: service unit path has no parent");
        return EXIT_CONFIG;
    };
    if let Err(error) = operations.create_dir_all(parent) {
        write_error(
            stderr,
            &format!(
                "wayfinder-router: cannot create service unit directory: {}",
                io_error_category(&error)
            ),
        );
        return EXIT_CONFIG;
    }
    if let Err(error) = operations.write_file(unit_file, unit_text.as_bytes()) {
        write_error(
            stderr,
            &format!(
                "wayfinder-router: cannot write {}: {}",
                unit_file.display(),
                io_error_category(&error)
            ),
        );
        return EXIT_CONFIG;
    }

    match (platform, manager) {
        (ServicePlatform::MacOs, Some(manager)) => {
            if let Err(error) = operations.create_dir_all(log_dir) {
                write_error(
                    stderr,
                    &format!(
                        "wayfinder-router: cannot create service log directory: {}",
                        io_error_category(&error)
                    ),
                );
                return EXIT_CONFIG;
            }
            let uid = match operations.current_uid() {
                Ok(uid) => uid,
                Err(message) => {
                    write_error(stderr, &format!("wayfinder-router: {message}"));
                    return EXIT_CONFIG;
                }
            };
            let domain = format!("gui/{uid}");
            let target = format!("{domain}/{LAUNCHD_LABEL}");
            let mut loaded = match operations.run_manager(
                manager,
                &["bootstrap".to_owned(), domain, display_path(unit_file)],
            ) {
                Ok(output) => output,
                Err(message) => {
                    write_error(stderr, &format!("wayfinder-router: {message}"));
                    return EXIT_CONFIG;
                }
            };
            if !loaded.success {
                loaded = match operations.run_manager(
                    manager,
                    &["load".to_owned(), "-w".to_owned(), display_path(unit_file)],
                ) {
                    Ok(output) => output,
                    Err(message) => {
                        write_error(stderr, &format!("wayfinder-router: {message}"));
                        return EXIT_CONFIG;
                    }
                };
            }
            let probe = match operations.run_manager(manager, &["print".to_owned(), target]) {
                Ok(output) => output,
                Err(message) => {
                    write_error(stderr, &format!("wayfinder-router: {message}"));
                    return EXIT_CONFIG;
                }
            };
            if !probe.success {
                let detail = loaded.detail();
                let suffix = detail
                    .as_deref()
                    .map_or_else(String::new, |detail| format!(": {detail}"));
                write_error(
                    stderr,
                    &format!(
                        "wayfinder-router: launchctl could not load {}{suffix}",
                        unit_file.display()
                    ),
                );
                return EXIT_CONFIG;
            }
            write_error(
                stderr,
                &format!(
                    "wayfinder-router: installed and loaded {}",
                    unit_file.display()
                ),
            );
        }
        (ServicePlatform::Linux, Some(manager)) => {
            let _ =
                operations.run_manager(manager, &["--user".to_owned(), "daemon-reload".to_owned()]);
            let enabled = match operations.run_manager(
                manager,
                &[
                    "--user".to_owned(),
                    "enable".to_owned(),
                    "--now".to_owned(),
                    SYSTEMD_UNIT_NAME.to_owned(),
                ],
            ) {
                Ok(output) => output,
                Err(message) => {
                    write_error(stderr, &format!("wayfinder-router: {message}"));
                    return EXIT_CONFIG;
                }
            };
            if !enabled.success {
                let detail = enabled.detail();
                let suffix = detail
                    .as_deref()
                    .map_or_else(String::new, |detail| format!(": {detail}"));
                write_error(
                    stderr,
                    &format!(
                        "wayfinder-router: systemctl could not enable {SYSTEMD_UNIT_NAME}{suffix}"
                    ),
                );
                return EXIT_CONFIG;
            }
            write_error(
                stderr,
                &format!(
                    "wayfinder-router: installed and started {}",
                    unit_file.display()
                ),
            );
        }
        (ServicePlatform::MacOs, None) => write_error(
            stderr,
            &format!(
                "wayfinder-router: wrote {}; start it with:\n  launchctl bootstrap gui/$(id -u) {}",
                unit_file.display(),
                unit_file.display()
            ),
        ),
        (ServicePlatform::Linux, None) => write_error(
            stderr,
            &format!(
                "wayfinder-router: wrote {}; start it with:\n  systemctl --user enable --now {SYSTEMD_UNIT_NAME}",
                unit_file.display()
            ),
        ),
        (ServicePlatform::Other, _) => unreachable!("unsupported platform returned above"),
    }
    write_error(
        stderr,
        &format!("wayfinder-router: point your apps at OPENAI_BASE_URL={endpoint}"),
    );
    EXIT_OK
}

fn uninstall(
    platform: ServicePlatform,
    unit_file: &Path,
    manager: Option<&Path>,
    stderr: &mut dyn Write,
    operations: &mut dyn ServiceOperations,
) -> i32 {
    let existed = operations.is_file(unit_file);
    if existed {
        match (platform, manager) {
            (ServicePlatform::MacOs, Some(manager)) => {
                let uid = match operations.current_uid() {
                    Ok(uid) => uid,
                    Err(message) => {
                        write_error(stderr, &format!("wayfinder-router: {message}"));
                        return EXIT_CONFIG;
                    }
                };
                let target = format!("gui/{uid}/{LAUNCHD_LABEL}");
                let booted = operations.run_manager(manager, &["bootout".to_owned(), target]);
                if !matches!(booted, Ok(ref output) if output.success) {
                    let _ = operations.run_manager(
                        manager,
                        &[
                            "unload".to_owned(),
                            "-w".to_owned(),
                            display_path(unit_file),
                        ],
                    );
                }
            }
            (ServicePlatform::Linux, Some(manager)) => {
                let _ = operations.run_manager(
                    manager,
                    &[
                        "--user".to_owned(),
                        "disable".to_owned(),
                        "--now".to_owned(),
                        SYSTEMD_UNIT_NAME.to_owned(),
                    ],
                );
            }
            _ => {}
        }
        if let Err(error) = operations.remove_file(unit_file) {
            write_error(
                stderr,
                &format!(
                    "wayfinder-router: cannot remove {}: {}",
                    unit_file.display(),
                    io_error_category(&error)
                ),
            );
            return EXIT_CONFIG;
        }
    }
    let message = if existed {
        format!("wayfinder-router: removed {}", unit_file.display())
    } else {
        format!(
            "wayfinder-router: nothing to remove ({} not present)",
            unit_file.display()
        )
    };
    write_error(stderr, &message);
    EXIT_OK
}

#[allow(clippy::too_many_arguments)]
fn status(
    options: &ServiceOptions,
    platform: ServicePlatform,
    unit_file: &Path,
    manager: Option<&Path>,
    endpoint: &str,
    stderr: &mut dyn Write,
    operations: &mut dyn ServiceOperations,
) -> i32 {
    let installed = operations.is_file(unit_file);
    write_error(
        stderr,
        &format!(
            "unit file: {} ({})",
            unit_file.display(),
            if installed { "present" } else { "absent" }
        ),
    );
    write_error(stderr, &format!("endpoint:  {endpoint}"));
    if installed {
        match (platform, manager) {
            (ServicePlatform::MacOs, Some(manager)) => {
                let uid = match operations.current_uid() {
                    Ok(uid) => uid,
                    Err(message) => {
                        write_error(stderr, &format!("wayfinder-router: {message}"));
                        return EXIT_CONFIG;
                    }
                };
                let target = format!("gui/{uid}/{LAUNCHD_LABEL}");
                let loaded = operations
                    .run_manager(manager, &["print".to_owned(), target])
                    .is_ok_and(|output| output.success);
                write_error(
                    stderr,
                    &format!(
                        "launchd:   {}",
                        if loaded { "loaded" } else { "not loaded" }
                    ),
                );
            }
            (ServicePlatform::Linux, Some(manager)) => {
                let state = operations
                    .run_manager(
                        manager,
                        &[
                            "--user".to_owned(),
                            "is-active".to_owned(),
                            SYSTEMD_UNIT_NAME.to_owned(),
                        ],
                    )
                    .ok()
                    .and_then(|output| output.stdout_detail())
                    .unwrap_or_else(|| "unknown".to_owned());
                write_error(stderr, &format!("systemd:   {state}"));
            }
            _ => {}
        }
    }
    let health = operations.probe_health(&options.host, options.port);
    write_error(stderr, &format!("health:    {health}"));
    if !installed {
        write_error(
            stderr,
            &format!(
                "\ninstall with: wayfinder-router service install --port {}",
                options.port
            ),
        );
    }
    EXIT_OK
}

fn parse_service_options(arguments: &[String]) -> Result<Option<ServiceOptions>, String> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(None);
    }
    let mut action = None;
    let mut host = DEFAULT_HOST.to_owned();
    let mut port = DEFAULT_PORT;
    let mut config = None;
    let mut print = false;
    let mut index = 0_usize;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "install" | "uninstall" | "status" => {
                let parsed = match argument.as_str() {
                    "install" => ServiceAction::Install,
                    "uninstall" => ServiceAction::Uninstall,
                    "status" => ServiceAction::Status,
                    _ => unreachable!(),
                };
                if action.replace(parsed).is_some() {
                    return Err("service accepts exactly one action".to_owned());
                }
            }
            "--host" => {
                index = index.saturating_add(1);
                host = arguments
                    .get(index)
                    .ok_or_else(|| "--host needs a value".to_owned())?
                    .clone();
            }
            "--port" => {
                index = index.saturating_add(1);
                let raw = arguments
                    .get(index)
                    .ok_or_else(|| "--port needs a value".to_owned())?;
                port = parse_service_port(raw)?;
            }
            "--config" => {
                index = index.saturating_add(1);
                config = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--config needs a value".to_owned())?
                        .clone(),
                );
            }
            "--print" => print = true,
            value if value.starts_with("--host=") => {
                host = value.trim_start_matches("--host=").to_owned();
            }
            value if value.starts_with("--port=") => {
                port = parse_service_port(value.trim_start_matches("--port="))?;
            }
            value if value.starts_with("--config=") => {
                config = Some(value.trim_start_matches("--config=").to_owned());
            }
            value => return Err(format!("unrecognized service argument: {value}")),
        }
        index = index.saturating_add(1);
    }
    if host.is_empty() || host.chars().any(char::is_control) {
        return Err("--host must be non-empty and contain no control characters".to_owned());
    }
    let action = action.ok_or_else(|| "service needs install, uninstall, or status".to_owned())?;
    Ok(Some(ServiceOptions {
        action,
        host,
        port,
        config,
        print,
    }))
}

fn parse_service_port(raw: &str) -> Result<u16, String> {
    raw.parse::<u16>()
        .map_err(|_| "--port must be an integer from 0 to 65535".to_owned())
}

fn resolve_serve_args(executable: &str, options: &ServiceOptions) -> Vec<String> {
    let mut arguments = vec![
        executable.to_owned(),
        "serve".to_owned(),
        "--host".to_owned(),
        options.host.clone(),
        "--port".to_owned(),
        options.port.to_string(),
    ];
    if let Some(config) = options.config.as_ref() {
        arguments.extend(["--config".to_owned(), config.clone()]);
    }
    arguments
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ManagerOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ManagerOutput {
    fn detail(&self) -> Option<String> {
        sanitize_detail(if self.stderr.is_empty() {
            &self.stdout
        } else {
            &self.stderr
        })
    }

    fn stdout_detail(&self) -> Option<String> {
        sanitize_detail(&self.stdout)
    }
}

trait ServiceOperations {
    fn platform(&self) -> ServicePlatform;
    fn home_dir(&self) -> Result<PathBuf, String>;
    fn executable(&self) -> Result<PathBuf, String>;
    fn which(&self, name: &str) -> Option<PathBuf>;
    fn current_uid(&mut self) -> Result<u32, String>;
    fn create_dir_all(&mut self, path: &Path) -> io::Result<()>;
    fn write_file(&mut self, path: &Path, contents: &[u8]) -> io::Result<()>;
    fn is_file(&self, path: &Path) -> bool;
    fn remove_file(&mut self, path: &Path) -> io::Result<()>;
    fn run_manager(
        &mut self,
        program: &Path,
        arguments: &[String],
    ) -> Result<ManagerOutput, String>;
    fn probe_health(&mut self, host: &str, port: u16) -> String;
}

struct RealServiceOperations;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServiceInspection {
    pub(crate) platform: &'static str,
    pub(crate) unit_file: Option<PathBuf>,
    pub(crate) installed: bool,
    pub(crate) manager_available: bool,
    pub(crate) state: String,
}

pub(crate) fn inspect_service() -> ServiceInspection {
    let mut operations = RealServiceOperations;
    let platform = operations.platform();
    let platform_name = match platform {
        ServicePlatform::MacOs => "launchd",
        ServicePlatform::Linux => "systemd-user",
        ServicePlatform::Other => "unsupported",
    };
    if platform == ServicePlatform::Other {
        return ServiceInspection {
            platform: platform_name,
            unit_file: None,
            installed: false,
            manager_available: false,
            state: "unsupported".to_owned(),
        };
    }
    let Ok(home) = operations.home_dir() else {
        return ServiceInspection {
            platform: platform_name,
            unit_file: None,
            installed: false,
            manager_available: false,
            state: "home-unavailable".to_owned(),
        };
    };
    let unit_file = match platform {
        ServicePlatform::MacOs => agent_path(Some(home.as_path())).ok(),
        ServicePlatform::Linux => systemd_unit_path(Some(home.as_path())).ok(),
        ServicePlatform::Other => None,
    };
    let installed = unit_file
        .as_deref()
        .is_some_and(|path| operations.is_file(path));
    let manager_name = match platform {
        ServicePlatform::MacOs => "launchctl",
        ServicePlatform::Linux => "systemctl",
        ServicePlatform::Other => unreachable!("unsupported platform returned above"),
    };
    let manager = operations.which(manager_name);
    let manager_available = manager.is_some();
    let state = if !installed {
        "absent".to_owned()
    } else if let Some(manager) = manager {
        match platform {
            ServicePlatform::Linux => operations
                .run_manager(
                    &manager,
                    &[
                        "--user".to_owned(),
                        "is-active".to_owned(),
                        SYSTEMD_UNIT_NAME.to_owned(),
                    ],
                )
                .ok()
                .and_then(|output| output.stdout_detail())
                .unwrap_or_else(|| "unknown".to_owned()),
            ServicePlatform::MacOs => match operations.current_uid() {
                Ok(uid) => {
                    let target = format!("gui/{uid}/{LAUNCHD_LABEL}");
                    if operations
                        .run_manager(&manager, &["print".to_owned(), target])
                        .is_ok_and(|output| output.success)
                    {
                        "active".to_owned()
                    } else {
                        "inactive".to_owned()
                    }
                }
                Err(_) => "unknown".to_owned(),
            },
            ServicePlatform::Other => unreachable!("unsupported platform returned above"),
        }
    } else {
        "manager-unavailable".to_owned()
    };
    ServiceInspection {
        platform: platform_name,
        unit_file,
        installed,
        manager_available,
        state,
    }
}

impl ServiceOperations for RealServiceOperations {
    fn platform(&self) -> ServicePlatform {
        detect_platform(None)
    }

    fn home_dir(&self) -> Result<PathBuf, String> {
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "home directory is unavailable".to_owned())
    }

    fn executable(&self) -> Result<PathBuf, String> {
        env::current_exe().map_err(|error| {
            format!(
                "cannot resolve the wayfinder-router executable: {}",
                io_error_category(&error)
            )
        })
    }

    fn which(&self, name: &str) -> Option<PathBuf> {
        find_executable(name)
    }

    fn current_uid(&mut self) -> Result<u32, String> {
        if let Ok(uid) = env::var("UID") {
            if let Ok(uid) = uid.parse::<u32>() {
                return Ok(uid);
            }
        }
        let id = find_executable("id")
            .ok_or_else(|| "cannot determine the current user id (`id` not found)".to_owned())?;
        let output = self.run_manager(&id, &["-u".to_owned()])?;
        let uid = output.stdout_detail().ok_or_else(|| {
            "cannot determine the current user id (`id -u` returned no output)".to_owned()
        })?;
        uid.parse::<u32>()
            .map_err(|_| "cannot determine the current user id (`id -u` was invalid)".to_owned())
    }

    fn create_dir_all(&mut self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn write_file(&mut self, path: &Path, contents: &[u8]) -> io::Result<()> {
        fs::write(path, contents)
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn remove_file(&mut self, path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn run_manager(
        &mut self,
        program: &Path,
        arguments: &[String],
    ) -> Result<ManagerOutput, String> {
        run_bounded_command(program, arguments)
    }

    fn probe_health(&mut self, host: &str, port: u16) -> String {
        probe_health(host, port)
    }
}

fn run_bounded_command(program: &Path, arguments: &[String]) -> Result<ManagerOutput, String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "service manager could not start: {}",
                io_error_category(&error)
            )
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "service manager stdout was unavailable".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "service manager stderr was unavailable".to_owned())?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout));
    let stderr_reader = thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + MANAGER_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("service manager timed out".to_owned());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "service manager could not be observed: {}",
                    io_error_category(&error)
                ));
            }
        }
    };
    let stdout = join_reader(stdout_reader)?;
    let stderr = join_reader(stderr_reader)?;
    Ok(ManagerOutput {
        success: status.success(),
        stdout,
        stderr,
    })
}

fn read_bounded(mut stream: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    stream
        .by_ref()
        .take((MANAGER_OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut output)?;
    output.truncate(MANAGER_OUTPUT_LIMIT);
    Ok(output)
}

fn join_reader(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| "service manager output reader failed".to_owned())?
        .map_err(|error| {
            format!(
                "service manager output could not be read: {}",
                io_error_category(&error)
            )
        })
}

fn probe_health(host: &str, port: u16) -> String {
    let connect_host = host.trim_start_matches('[').trim_end_matches(']');
    let addresses = match (connect_host, port).to_socket_addrs() {
        Ok(addresses) => addresses.collect::<Vec<_>>(),
        Err(_) => return "unreachable (service not running?)".to_owned(),
    };
    for address in addresses {
        let Ok(mut stream) = TcpStream::connect_timeout(&address, HEALTH_TIMEOUT) else {
            continue;
        };
        if stream.set_read_timeout(Some(HEALTH_TIMEOUT)).is_err()
            || stream.set_write_timeout(Some(HEALTH_TIMEOUT)).is_err()
        {
            continue;
        }
        let request =
            format!("GET /healthz HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
        if stream.write_all(request.as_bytes()).is_err() {
            continue;
        }
        let mut response = Vec::new();
        if stream
            .take(HEALTH_RESPONSE_LIMIT)
            .read_to_end(&mut response)
            .is_err()
        {
            continue;
        }
        return parse_health_response(&response);
    }
    "unreachable (service not running?)".to_owned()
}

fn parse_health_response(response: &[u8]) -> String {
    let Some(headers_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return "unreachable (service not running?)".to_owned();
    };
    let headers = String::from_utf8_lossy(&response[..headers_end]);
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok());
    let Some(status) = status else {
        return "unreachable (service not running?)".to_owned();
    };
    if status != 200 {
        return format!("status {status}");
    }
    let body = &response[headers_end + 4..];
    let offline = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("offline").and_then(Value::as_bool))
        .unwrap_or(false);
    if offline {
        "ok (200, offline routing on)".to_owned()
    } else {
        "ok (200)".to_owned()
    }
}

pub(crate) fn find_executable(name: &str) -> Option<PathBuf> {
    let name = Path::new(name);
    if name.components().count() > 1 {
        return is_executable_file(name).then(|| name.to_path_buf());
    }
    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn sanitize_detail(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let detail = normalized.chars().take(DETAIL_LIMIT).collect::<String>();
    Some(detail)
}

fn io_error_category(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "not found",
        io::ErrorKind::PermissionDenied => "permission denied",
        io::ErrorKind::AlreadyExists => "already exists",
        io::ErrorKind::InvalidInput => "invalid input",
        io::ErrorKind::InvalidData => "invalid data",
        io::ErrorKind::TimedOut => "timed out",
        io::ErrorKind::WriteZero => "write failed",
        io::ErrorKind::Interrupted => "interrupted",
        io::ErrorKind::UnexpectedEof => "unexpected end of input",
        _ => "I/O error",
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet, VecDeque};

    #[derive(Debug)]
    struct FakeOperations {
        platform: ServicePlatform,
        home: PathBuf,
        executable: PathBuf,
        manager: Option<PathBuf>,
        uid: u32,
        files: HashMap<PathBuf, Vec<u8>>,
        directories: HashSet<PathBuf>,
        commands: Vec<(PathBuf, Vec<String>)>,
        outputs: VecDeque<Result<ManagerOutput, String>>,
        health: String,
    }

    impl FakeOperations {
        fn new(platform: ServicePlatform) -> Self {
            Self {
                platform,
                home: PathBuf::from("/isolated/home"),
                executable: PathBuf::from("/isolated/bin/wayfinder-router"),
                manager: Some(PathBuf::from("/isolated/bin/manager")),
                uid: 501,
                files: HashMap::new(),
                directories: HashSet::new(),
                commands: Vec::new(),
                outputs: VecDeque::new(),
                health: "unreachable (service not running?)".to_owned(),
            }
        }
    }

    impl ServiceOperations for FakeOperations {
        fn platform(&self) -> ServicePlatform {
            self.platform
        }

        fn home_dir(&self) -> Result<PathBuf, String> {
            Ok(self.home.clone())
        }

        fn executable(&self) -> Result<PathBuf, String> {
            Ok(self.executable.clone())
        }

        fn which(&self, _name: &str) -> Option<PathBuf> {
            self.manager.clone()
        }

        fn current_uid(&mut self) -> Result<u32, String> {
            Ok(self.uid)
        }

        fn create_dir_all(&mut self, path: &Path) -> io::Result<()> {
            self.directories.insert(path.to_path_buf());
            Ok(())
        }

        fn write_file(&mut self, path: &Path, contents: &[u8]) -> io::Result<()> {
            self.files.insert(path.to_path_buf(), contents.to_vec());
            Ok(())
        }

        fn is_file(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }

        fn remove_file(&mut self, path: &Path) -> io::Result<()> {
            self.files.remove(path);
            Ok(())
        }

        fn run_manager(
            &mut self,
            program: &Path,
            arguments: &[String],
        ) -> Result<ManagerOutput, String> {
            self.commands
                .push((program.to_path_buf(), arguments.to_vec()));
            self.outputs.pop_front().unwrap_or_else(|| {
                Ok(ManagerOutput {
                    success: true,
                    ..ManagerOutput::default()
                })
            })
        }

        fn probe_health(&mut self, _host: &str, _port: u16) -> String {
            self.health.clone()
        }
    }

    fn run_fake(arguments: &[&str], operations: &mut FakeOperations) -> (i32, String, String) {
        let arguments = arguments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_service_with(&arguments, &mut stdout, &mut stderr, operations);
        (
            code,
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
        )
    }

    #[test]
    fn parser_matches_python_options_in_any_order() -> Result<(), String> {
        let options = parse_service_options(&[
            "--host=localhost".to_owned(),
            "--port".to_owned(),
            "9000".to_owned(),
            "install".to_owned(),
            "--config=/etc/wf/router.toml".to_owned(),
            "--print".to_owned(),
        ])?
        .ok_or_else(|| "service unexpectedly requested help".to_owned())?;
        assert_eq!(
            options,
            ServiceOptions {
                action: ServiceAction::Install,
                host: "localhost".to_owned(),
                port: 9_000,
                config: Some("/etc/wf/router.toml".to_owned()),
                print: true,
            }
        );
        Ok(())
    }

    #[test]
    fn print_renders_launchd_without_mutation() {
        let mut operations = FakeOperations::new(ServicePlatform::MacOs);
        let (code, stdout, stderr) = run_fake(
            &["install", "--print", "--config", "/etc/wf/router.toml"],
            &mut operations,
        );
        assert_eq!(code, EXIT_OK, "{stderr}");
        assert!(stdout.starts_with("<?xml version=\"1.0\""));
        assert!(stdout.contains("<string>--config</string>"));
        assert!(stdout.contains("<string>/etc/wf/router.toml</string>"));
        assert!(stdout.contains("/isolated/home/Library/Logs/wayfinder-router.log"));
        assert!(operations.files.is_empty());
        assert!(operations.directories.is_empty());
        assert!(operations.commands.is_empty());
    }

    #[test]
    fn launchd_install_uses_fallback_then_probes_end_state() {
        let mut operations = FakeOperations::new(ServicePlatform::MacOs);
        operations.outputs.extend([
            Ok(ManagerOutput {
                success: false,
                stderr: b"already loaded".to_vec(),
                ..ManagerOutput::default()
            }),
            Ok(ManagerOutput {
                success: true,
                ..ManagerOutput::default()
            }),
            Ok(ManagerOutput {
                success: true,
                ..ManagerOutput::default()
            }),
        ]);
        let (code, _, stderr) = run_fake(&["install"], &mut operations);
        assert_eq!(code, EXIT_OK, "{stderr}");
        assert!(stderr.contains("installed and loaded"));
        assert_eq!(operations.commands.len(), 3);
        assert_eq!(operations.commands[0].1[0], "bootstrap");
        assert_eq!(operations.commands[1].1[0], "load");
        assert_eq!(operations.commands[2].1[0], "print");
        assert!(operations.files.contains_key(&PathBuf::from(
            "/isolated/home/Library/LaunchAgents/com.wayfinder-router.gateway.plist"
        )));
    }

    #[test]
    fn launchd_install_reports_sanitized_probe_failure() {
        let mut operations = FakeOperations::new(ServicePlatform::MacOs);
        operations.outputs.extend([
            Ok(ManagerOutput {
                success: false,
                stderr: b"Bootstrap failed:\n busy\tsecret-free".to_vec(),
                ..ManagerOutput::default()
            }),
            Ok(ManagerOutput {
                success: false,
                stderr: b"legacy failed\n busy".to_vec(),
                ..ManagerOutput::default()
            }),
            Ok(ManagerOutput {
                success: false,
                ..ManagerOutput::default()
            }),
        ]);
        let (code, _, stderr) = run_fake(&["install"], &mut operations);
        assert_eq!(code, EXIT_CONFIG);
        assert!(stderr.contains("could not load"));
        assert!(stderr.contains("legacy failed busy"));
        assert!(!stderr.contains('\t'));
    }

    #[test]
    fn systemd_install_failure_is_not_reported_as_success() {
        let mut operations = FakeOperations::new(ServicePlatform::Linux);
        operations.outputs.extend([
            Ok(ManagerOutput {
                success: true,
                ..ManagerOutput::default()
            }),
            Ok(ManagerOutput {
                success: false,
                stderr: b"permission denied".to_vec(),
                ..ManagerOutput::default()
            }),
        ]);
        let (code, _, stderr) = run_fake(&["install"], &mut operations);
        assert_eq!(code, EXIT_CONFIG);
        assert!(stderr.contains("could not enable"));
        assert!(stderr.contains("permission denied"));
    }

    #[test]
    fn uninstall_and_status_are_isolated_and_compatible() {
        let mut operations = FakeOperations::new(ServicePlatform::Linux);
        let unit = PathBuf::from("/isolated/home/.config/systemd/user/wayfinder-router.service");
        operations.files.insert(unit.clone(), b"unit".to_vec());
        let (code, _, stderr) = run_fake(&["uninstall"], &mut operations);
        assert_eq!(code, EXIT_OK, "{stderr}");
        assert!(!operations.files.contains_key(&unit));
        assert!(stderr.contains("removed"));
        assert!(operations.commands[0].1.contains(&"disable".to_owned()));

        operations.health = "ok (200, offline routing on)".to_owned();
        let (code, _, stderr) = run_fake(&["status", "--port", "9001"], &mut operations);
        assert_eq!(code, EXIT_OK, "{stderr}");
        assert!(stderr.contains("(absent)"));
        assert!(stderr.contains("health:    ok (200, offline routing on)"));
        assert!(stderr.contains("service install --port 9001"));
    }

    #[test]
    fn health_parser_matches_python_status_strings() {
        assert_eq!(
            parse_health_response(
                b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\n{\"offline\":true}"
            ),
            "ok (200, offline routing on)"
        );
        assert_eq!(
            parse_health_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}"),
            "ok (200)"
        );
        assert_eq!(
            parse_health_response(b"HTTP/1.1 503 Service Unavailable\r\n\r\n"),
            "status 503"
        );
    }

    #[test]
    fn parser_rejects_missing_action_and_unsafe_values() {
        for arguments in [
            vec![],
            vec!["install".to_owned(), "status".to_owned()],
            vec!["install".to_owned(), "--port=-1".to_owned()],
            vec!["install".to_owned(), "--port=70000".to_owned()],
            vec!["install".to_owned(), "--host=a\r\nb".to_owned()],
        ] {
            assert!(parse_service_options(&arguments).is_err());
        }
    }
}
