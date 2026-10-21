use std::{fs::File, io::Read, path::Path};
use wayfinder_agent_delegation::{AgentDeclaration, Task, preview};
use wayfinder_config::{TierOrderPolicy, routing_config_from_toml};

const INPUT_LIMIT: u64 = 1024 * 1024;
const HELP: &str = "usage: wayfinder-agent-delegation POLICY.toml AGENTS.json TASK.json\n\nPreview only. Subscription access and readiness are operator declarations, not verified authentication. No agent is launched.";

fn read_bounded(path: &Path) -> Result<String, &'static str> {
    let mut content = String::new();
    File::open(path)
        .map_err(|_| "cannot open explicit input file")?
        .take(INPUT_LIMIT + 1)
        .read_to_string(&mut content)
        .map_err(|_| "cannot read UTF-8 input")?;
    if content.len() as u64 > INPUT_LIMIT {
        return Err("input exceeds 1 MiB limit");
    }
    Ok(content)
}

fn run() -> Result<(), &'static str> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if arguments.len() != 3
        || arguments
            .iter()
            .any(|value| value.to_string_lossy().starts_with('-'))
    {
        return Err(HELP);
    }
    let policy = read_bounded(Path::new(&arguments[0]))?;
    let inventory = read_bounded(Path::new(&arguments[1]))?;
    let input = read_bounded(Path::new(&arguments[2]))?;
    let policy = routing_config_from_toml(
        &policy,
        "delegation policy",
        None,
        TierOrderPolicy::StrictInput,
    )
    .map_err(|_| "invalid explicit routing policy")?;
    let agents: Vec<AgentDeclaration> =
        serde_json::from_str(&inventory).map_err(|_| "invalid agent declarations")?;
    let task: Task = serde_json::from_str(&input).map_err(|_| "invalid text task")?;
    let report =
        preview(&task, &agents, &policy).map_err(|_| "cannot plan task with these declarations")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|_| "cannot encode preview")?
    );
    if report.proposed_client.is_none() {
        return Err("no eligible agent in the selected policy tier; no fallback attempted");
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("wayfinder-agent-delegation: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}
