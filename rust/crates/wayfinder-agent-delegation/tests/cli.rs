use std::process::Command;

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wayfinder-agent-delegation"))
}

fn fixture(name: &str) -> String {
    format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn explicit_inputs_produce_only_a_blocked_preview() -> Result<(), Box<dyn std::error::Error>> {
    let output = command()
        .args([
            fixture("policy.toml"),
            fixture("agents.json"),
            fixture("task.json"),
        ])
        .output()?;
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        value["dispatch_status"],
        "blocked-pending-agent-auth-contract"
    );
    assert_eq!(
        value["routing"]["selected_destination_id"],
        "personal-codex"
    );
    Ok(())
}

#[test]
fn missing_policy_never_uses_discovered_or_default_configuration()
-> Result<(), Box<dyn std::error::Error>> {
    let output = command()
        .args([
            fixture("absent.toml"),
            fixture("agents.json"),
            fixture("task.json"),
        ])
        .output()?;
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)?.contains("cannot open explicit input file"));
    Ok(())
}

#[test]
fn launch_and_permission_overrides_are_not_an_execution_surface()
-> Result<(), Box<dyn std::error::Error>> {
    for flag in ["--launch", "--yolo", "--dangerously-skip-permissions"] {
        let output = command()
            .args([flag, &fixture("agents.json"), &fixture("task.json")])
            .output()?;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    Ok(())
}
