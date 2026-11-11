//! Pure operating-system service-unit rendering.
//!
//! These helpers intentionally perform no filesystem or service-manager I/O.
//! The CLI owns installation and lifecycle operations; this module keeps the
//! generated launchd and systemd contracts deterministic and testable.

use std::env;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Stable launchd identity used by install, status, and uninstall operations.
pub const LAUNCHD_LABEL: &str = "com.wayfinder-router.gateway";
/// Stable systemd user-unit filename.
pub const SYSTEMD_UNIT_NAME: &str = "wayfinder-router.service";
/// Default macOS service log directory.
pub const DEFAULT_LAUNCHD_LOG_DIR: &str = "~/Library/Logs";

/// Normalized service-manager family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServicePlatform {
    /// macOS and launchd.
    MacOs,
    /// Linux and systemd.
    Linux,
    /// A platform for which no service integration is shipped.
    Other,
}

impl ServicePlatform {
    /// Compatibility spelling exposed by the Python implementation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MacOs => "macos",
            Self::Linux => "linux",
            Self::Other => "other",
        }
    }
}

/// A path cannot be derived because the process has no home-directory hint.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum UnitPathError {
    /// Neither `HOME` nor `USERPROFILE` was available.
    #[error("home directory is unavailable")]
    HomeDirectoryUnavailable,
}

/// Map a Python-style platform name to the supported service-manager family.
///
/// When `platform` is absent, Rust's compile-time host spelling is used.
#[must_use]
pub fn detect_platform(platform: Option<&str>) -> ServicePlatform {
    let platform = platform.unwrap_or(env::consts::OS);
    if platform == "darwin" || platform == "macos" {
        ServicePlatform::MacOs
    } else if platform.starts_with("linux") {
        ServicePlatform::Linux
    } else {
        ServicePlatform::Other
    }
}

/// Render a launchd LaunchAgent plist with Python-compatible defaults.
///
/// `log_dir` is expanded before rendering because launchd does not expand a
/// leading tilde in `StandardOutPath` or `StandardErrorPath`. The explicit
/// `home` makes packaging and golden tests independent of the invoking process
/// environment.
#[must_use]
pub fn launchd_plist_with_home(
    program_args: &[String],
    label: Option<&str>,
    log_dir: Option<&str>,
    home: Option<&Path>,
) -> String {
    let args_xml = program_args
        .iter()
        .map(|argument| format!("      <string>{}</string>", xml_escape(argument)))
        .collect::<Vec<_>>()
        .join("\n");
    let logs = expand_user(log_dir.unwrap_or(DEFAULT_LAUNCHD_LOG_DIR), home)
        .trim_end_matches('/')
        .to_owned();
    let label = label.unwrap_or(LAUNCHD_LABEL);
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
            "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n",
            "<dict>\n",
            "  <key>Label</key>\n",
            "  <string>{label}</string>\n",
            "  <key>ProgramArguments</key>\n",
            "  <array>\n",
            "{args_xml}\n",
            "  </array>\n",
            "  <key>RunAtLoad</key>\n",
            "  <true/>\n",
            "  <key>KeepAlive</key>\n",
            "  <true/>\n",
            "  <key>StandardOutPath</key>\n",
            "  <string>{logs}/wayfinder-router.log</string>\n",
            "  <key>StandardErrorPath</key>\n",
            "  <string>{logs}/wayfinder-router.err.log</string>\n",
            "</dict>\n",
            "</plist>\n"
        ),
        label = label,
        args_xml = args_xml,
        logs = logs
    )
}

/// Render a systemd user unit that restarts the gateway after failures.
#[must_use]
pub fn systemd_unit(program_args: &[String], description: Option<&str>) -> String {
    let exec_start = program_args
        .iter()
        .map(|argument| shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ");
    let description = description.unwrap_or("Wayfinder router gateway");
    format!(
        "[Unit]\n\
Description={description}\n\
After=network-online.target\n\
\n\
[Service]\n\
ExecStart={exec_start}\n\
Restart=on-failure\n\
RestartSec=2\n\
\n\
[Install]\n\
WantedBy=default.target\n"
    )
}

/// Return `~/Library/LaunchAgents/<label>.plist` for a supplied or detected home.
pub fn agent_path(home: Option<&Path>) -> Result<PathBuf, UnitPathError> {
    Ok(resolve_home(home)?
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

/// Return `~/.config/systemd/user/<unit>` for a supplied or detected home.
pub fn systemd_unit_path(home: Option<&Path>) -> Result<PathBuf, UnitPathError> {
    Ok(resolve_home(home)?
        .join(".config")
        .join("systemd")
        .join("user")
        .join(SYSTEMD_UNIT_NAME))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn expand_user(value: &str, home: Option<&Path>) -> String {
    if value == "~" || value.starts_with("~/") {
        if let Ok(home) = resolve_home(home) {
            let suffix = value.strip_prefix("~/").unwrap_or("");
            return if suffix.is_empty() {
                home.to_string_lossy().into_owned()
            } else {
                home.join(suffix).to_string_lossy().into_owned()
            };
        }
    }
    value.to_owned()
}

fn resolve_home(home: Option<&Path>) -> Result<PathBuf, UnitPathError> {
    if let Some(home) = home {
        return Ok(home.to_path_buf());
    }
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or(UnitPathError::HomeDirectoryUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn platform_names_match_python() {
        assert_eq!(detect_platform(Some("darwin")).as_str(), "macos");
        assert_eq!(detect_platform(Some("linux")).as_str(), "linux");
        assert_eq!(detect_platform(Some("linux2")).as_str(), "linux");
        assert_eq!(detect_platform(Some("win32")).as_str(), "other");
    }

    #[test]
    fn launchd_plist_is_well_formed_and_uses_absolute_logs() {
        let plist = launchd_plist_with_home(
            &args(&["/usr/local/bin/wayfinder-router", "serve", "--port", "8088"]),
            None,
            None,
            Some(Path::new("/home/tester")),
        );
        assert!(plist.starts_with("<?xml version=\"1.0\""));
        assert!(plist.contains(&format!("<string>{LAUNCHD_LABEL}</string>")));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(plist.contains("<string>/usr/local/bin/wayfinder-router</string>"));
        assert!(plist.contains("<string>/home/tester/Library/Logs/wayfinder-router.log</string>"));
        assert!(!plist.contains("~/Library/Logs"));
    }

    #[test]
    fn launchd_arguments_are_xml_escaped() {
        let plist = launchd_plist_with_home(
            &args(&["/bin/x & <y>", "serve"]),
            None,
            None,
            Some(Path::new("/home/tester")),
        );
        assert!(plist.contains("<string>/bin/x &amp; &lt;y&gt;</string>"));
        assert!(!plist.contains("& <y>"));
    }

    #[test]
    fn systemd_unit_matches_python_shell_quoting() {
        let unit = systemd_unit(
            &args(&["/opt/my router/wayfinder-router", "serve", "it's-ready"]),
            None,
        );
        assert!(
            unit.contains("ExecStart='/opt/my router/wayfinder-router' serve 'it'\"'\"'s-ready'")
        );
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn paths_use_the_supplied_home() -> Result<(), UnitPathError> {
        let home = Path::new("/home/tester");
        assert_eq!(
            agent_path(Some(home))?,
            home.join("Library/LaunchAgents/com.wayfinder-router.gateway.plist")
        );
        assert_eq!(
            systemd_unit_path(Some(home))?,
            home.join(".config/systemd/user/wayfinder-router.service")
        );
        Ok(())
    }

    #[test]
    fn empty_and_non_ascii_shell_arguments_are_quoted() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("café"), "'café'");
        assert_eq!(shell_quote("safe:@%+=,./-_"), "safe:@%+=,./-_");
    }
}
