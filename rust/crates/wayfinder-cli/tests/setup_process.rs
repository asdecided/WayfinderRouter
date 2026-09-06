//! Exercise the actual process boundary used by the QML panel.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use serde_json::Value;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::process::Command;

struct Home(PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn command(home: &Home) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wayfinder-router"));
    command
        .env("HOME", &home.0)
        .env("XDG_CONFIG_HOME", home.0.join("config"))
        .kill_on_drop(true);
    command
}
fn setup_command(home: &Home, action: &str) -> Command {
    let mut command = command(home);
    command
        .args(["setup", action, "--config"])
        .arg(home.0.join("config/wayfinder/wayfinder-router.toml"))
        .args(["--endpoint", "http://127.0.0.1:8088"]);
    command
}
#[tokio::test]
async fn setup_process_cancels_open_stdin_and_releases_lock() {
    let home = Home(
        std::env::temp_dir().join(format!("wayfinder-setup-process-{}", uuid::Uuid::new_v4())),
    );
    let config = home.0.join("config/wayfinder/wayfinder-router.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let init = command(&home)
        .args(["init", "--preset", "local", "--path"])
        .arg(&config)
        .output()
        .await
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let capabilities = command(&home)
        .args(["capabilities", "--json"])
        .output()
        .await
        .unwrap();
    let capabilities: Value = serde_json::from_slice(&capabilities.stdout).unwrap();
    assert_eq!(capabilities["setup_schema_version"], 1);
    let mut child = setup_command(&home, "discover")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "setup exited before acquiring its lock"
            );
            if let Ok(lock) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(config.parent().unwrap().join("omarchy-setup/lock"))
            {
                if rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                    .is_err()
                {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let _open_stdin = child.stdin.take().unwrap();
    let pid = rustix::process::Pid::from_raw(child.id().unwrap() as i32).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert!(report["error"].as_str().unwrap().contains("cancelled"));
    let status = setup_command(&home, "status").output().await.unwrap();
    assert!(status.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap()["stage"],
        "provider"
    );
}
