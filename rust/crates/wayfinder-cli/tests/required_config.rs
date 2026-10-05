use std::{error::Error, fs, path::PathBuf, process::Command};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let path =
            std::env::temp_dir().join(format!("wayfinder-required-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wayfinder-router"));
        command
            .current_dir(&self.0)
            .env_remove("WAYFINDER_CONFIG")
            .env_remove("WAYFINDER_ROUTER_THRESHOLD");
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn process_rejects_required_missing_policy_for_route_and_serve() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    fs::write(f.0.join("prompt.txt"), "hello")?;
    let route = f
        .command()
        .args(["route", "prompt.txt", "--json"])
        .env("WAYFINDER_CONFIG", f.0.join("missing.toml"))
        .output()?;
    assert!(!route.status.success());
    assert!(String::from_utf8_lossy(&route.stderr).contains("required configuration absent"));
    assert!(route.stdout.is_empty());
    let serve = f
        .command()
        .args(["serve", "--config", "missing.toml"])
        .output()?;
    assert!(!serve.status.success());
    assert!(String::from_utf8_lossy(&serve.stderr).contains("required configuration absent"));
    let optional = f
        .command()
        .args(["route", "prompt.txt", "--json"])
        .output()?;
    assert!(
        optional.status.success(),
        "{}",
        String::from_utf8_lossy(&optional.stderr)
    );
    Ok(())
}
