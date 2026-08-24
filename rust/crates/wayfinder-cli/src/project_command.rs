//! Canonical, no-clobber local project-profile lifecycle.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use reqwest::blocking::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use wayfinder_config::TierOrderPolicy;
use wayfinder_config::gateway::{GatewayConfig, VirtualKey, Workspace};
use wayfinder_config::routing_config_from_toml;
use wayfinder_routing_core::DEFAULT_THRESHOLD;

use crate::{EXIT_CONFIG, EXIT_OK, EXIT_USAGE, write_error, write_output};

const HELP: &str = "usage: wayfinder-router project setup|status|rollback [--repository OWNER/NAME|URL] [--root PATH] [--json]\n\nsetup also accepts --profile ID and --prompt-token. The project capability is read only from WAYFINDER_PROJECT_TOKEN or the prompt.";
const MANAGED_BY: &str = "wayfinder-router-project-v1";
const MANIFEST_FILE: &str = "manifest.json";
const PROFILE_FILE: &str = "routing.toml";
const PROJECT_TOKEN_ENV: &str = "WAYFINDER_PROJECT_TOKEN";
const PROJECTS_DIR_ENV: &str = "WAYFINDER_ROUTER_PROJECTS_DIR";
const GITHUB_TOKEN_ENV: &str = "WAYFINDER_GITHUB_TOKEN";
const DEFAULT_PROFILE: &str = "project";
const MAX_TOKEN_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CanonicalRepository {
    full_name: String,
    html_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectManifest {
    canonical_repository: String,
    repository_url: String,
    repository_root: PathBuf,
    profile_id: String,
    workspace_id: String,
    key_id: String,
    key_hash: String,
    generated_profile_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TokenSource {
    Environment,
    Prompt,
}

impl TokenSource {
    fn label(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::Prompt => "interactive-prompt",
        }
    }
}

#[derive(Default)]
struct Options {
    repository: Option<String>,
    root: Option<PathBuf>,
    profile: Option<String>,
    json: bool,
    prompt_token: bool,
}

pub(crate) fn run_project(
    arguments: &[String],
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        write_output(stdout, HELP);
        return EXIT_OK;
    }
    let Some(action) = arguments.first().map(String::as_str) else {
        write_error(
            stderr,
            "wayfinder-router: project needs setup, status, or rollback",
        );
        return EXIT_USAGE;
    };
    let options = match parse_options(&arguments[1..], action) {
        Ok(options) => options,
        Err(error) => {
            write_error(stderr, &format!("wayfinder-router: {error}"));
            return EXIT_USAGE;
        }
    };
    let result = match action {
        "setup" => setup(options, stdin, stderr),
        "status" => status(options),
        "rollback" => rollback(options),
        other => Err(format!("unsupported project action: {other}")),
    };
    match result {
        Ok(payload) => {
            if options_json(arguments) {
                if serde_json::to_writer_pretty(&mut *stdout, &payload).is_err()
                    || writeln!(stdout).is_err()
                {
                    write_error(stderr, "wayfinder-router: cannot write project output");
                    return EXIT_CONFIG;
                }
            } else {
                write_human(stdout, &payload);
            }
            EXIT_OK
        }
        Err(error) => {
            write_error(stderr, &format!("wayfinder-router: {error}"));
            EXIT_CONFIG
        }
    }
}

fn parse_options(arguments: &[String], action: &str) -> Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0_usize;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "--json" => options.json = true,
            "--prompt-token" if action == "setup" => options.prompt_token = true,
            "--repository" => {
                index = index.saturating_add(1);
                options.repository = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--repository needs a value".to_owned())?
                        .clone(),
                );
            }
            "--root" => {
                index = index.saturating_add(1);
                options.root = Some(PathBuf::from(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--root needs a value".to_owned())?,
                ));
            }
            "--profile" if action == "setup" => {
                index = index.saturating_add(1);
                options.profile = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| "--profile needs a value".to_owned())?
                        .clone(),
                );
            }
            value => return Err(format!("unrecognized project argument: {value}")),
        }
        index = index.saturating_add(1);
    }
    Ok(options)
}

fn options_json(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| argument == "--json")
}

fn setup(options: Options, stdin: &mut dyn Read, stderr: &mut dyn Write) -> Result<Value, String> {
    let root = canonical_repository_root(options.root.as_deref())?;
    let candidate = repository_candidate(options.repository.as_deref(), &root)?;
    let github_token = github_token();
    let repository = resolve_repository(&candidate, github_token.as_deref())?;
    let profile_name = options.profile.as_deref().unwrap_or(DEFAULT_PROFILE);
    validate_visible_id(profile_name, "profile")?;
    let projects_dir = projects_dir()?;
    let directory_id = directory_id(&repository.full_name, &root);
    let profile_dir = projects_dir.join(&directory_id);

    if profile_dir.exists() {
        let manifest = read_owned_manifest(&profile_dir)?;
        ensure_matches(&manifest, &repository.full_name, &root)?;
        if let Some(requested) = options.profile.as_deref() {
            let suffix = format!(
                "-{}",
                short_digest(&format!("{}\0{}", repository.full_name, root.display()))
            );
            if manifest.profile_id != format!("{requested}{suffix}") {
                return Err(
                    "owned project already uses a different profile id; roll it back explicitly before changing identity"
                        .to_owned(),
                );
            }
        }
        let token_matches = project_token_matches(&manifest);
        if token_matches == Some(false) {
            return Err(
                "WAYFINDER_PROJECT_TOKEN does not match the owned project capability; no changes made"
                    .to_owned(),
            );
        }
        let profile_path = profile_dir.join(PROFILE_FILE);
        let profile = fs::read_to_string(&profile_path)
            .map_err(|error| format!("cannot read {}: {error}", profile_path.display()))?;
        routing_config_from_toml(
            &profile,
            &profile_path.display().to_string(),
            None,
            TierOrderPolicy::StrictInput,
        )
        .map_err(|error| error.to_string())?;
        return Ok(status_payload(
            "unchanged",
            &manifest,
            &profile_dir,
            false,
            project_token_from_environment().map(|_| "environment"),
            token_matches,
            profile_sha256(&profile) != manifest.generated_profile_sha256,
        ));
    }

    let (project_token, token_source) = project_token(options.prompt_token, stdin, stderr)?;
    let suffix = short_digest(&format!("{}\0{}", repository.full_name, root.display()));
    let profile_id = format!("{profile_name}-{suffix}");
    let workspace_id = format!("project-{suffix}");
    let key_id = format!("project-{suffix}");
    let key_hash = wayfinder_gateway::auth::hash_key(&project_token);
    let profile = generated_profile();
    routing_config_from_toml(
        &profile,
        "generated project profile",
        None,
        TierOrderPolicy::StrictInput,
    )
    .map_err(|error| error.to_string())?;
    let manifest = ProjectManifest {
        canonical_repository: repository.full_name,
        repository_url: repository.html_url,
        repository_root: root,
        profile_id,
        workspace_id,
        key_id,
        key_hash,
        generated_profile_sha256: profile_sha256(&profile),
    };
    write_project_atomically(&projects_dir, &profile_dir, &manifest, &profile)?;
    Ok(status_payload(
        "created",
        &manifest,
        &profile_dir,
        false,
        Some(token_source.label()),
        Some(true),
        false,
    ))
}

fn status(options: Options) -> Result<Value, String> {
    let root = canonical_repository_root(options.root.as_deref())?;
    let expected = options
        .repository
        .as_deref()
        .map(parse_repository_input)
        .transpose()?
        .map(|(owner, repository)| format!("{owner}/{repository}"));
    let projects_dir = projects_dir()?;
    let Some((profile_dir, manifest)) = find_project(&projects_dir, &root, expected.as_deref())?
    else {
        return Ok(json!({
            "schema_version": 1,
            "status": "setup-required",
            "canonical_repository": expected,
            "repository_root": root,
            "profile_directory": Value::Null,
            "owned": false,
            "token_source": project_token_from_environment().map(|_| "environment"),
            "token_matches": Value::Null,
            "setup_required": true,
            "profile_modified": false,
        }));
    };
    let profile_path = profile_dir.join(PROFILE_FILE);
    let profile = fs::read_to_string(&profile_path)
        .map_err(|error| format!("cannot read {}: {error}", profile_path.display()))?;
    routing_config_from_toml(
        &profile,
        &profile_path.display().to_string(),
        None,
        TierOrderPolicy::StrictInput,
    )
    .map_err(|error| error.to_string())?;
    Ok(status_payload(
        "ready",
        &manifest,
        &profile_dir,
        false,
        project_token_from_environment().map(|_| "environment"),
        project_token_matches(&manifest),
        profile_sha256(&profile) != manifest.generated_profile_sha256,
    ))
}

fn rollback(options: Options) -> Result<Value, String> {
    let root = canonical_repository_root(options.root.as_deref())?;
    let expected = options
        .repository
        .as_deref()
        .map(parse_repository_input)
        .transpose()?
        .map(|(owner, repository)| format!("{owner}/{repository}"));
    let projects_dir = projects_dir()?;
    let Some((profile_dir, manifest)) = find_project(&projects_dir, &root, expected.as_deref())?
    else {
        return Err("no owned project setup matches this repository; no changes made".to_owned());
    };
    let quarantine = projects_dir.join(format!(".rollback-{}", Uuid::new_v4().simple()));
    fs::rename(&profile_dir, &quarantine).map_err(|error| {
        format!(
            "cannot stage rollback for {}: {error}",
            profile_dir.display()
        )
    })?;
    if let Err(error) = fs::remove_dir_all(&quarantine) {
        let _ = fs::rename(&quarantine, &profile_dir);
        return Err(format!("cannot remove owned project state: {error}"));
    }
    Ok(status_payload(
        "rolled-back",
        &manifest,
        &profile_dir,
        true,
        project_token_from_environment().map(|_| "environment"),
        project_token_matches(&manifest),
        false,
    ))
}

pub(crate) fn merge_owned_projects(gateway: &mut GatewayConfig) -> Result<usize, String> {
    let projects_dir = projects_dir()?;
    merge_owned_projects_from_dir(gateway, &projects_dir)
}

pub(crate) fn projects_source_version() -> u128 {
    let Ok(projects_dir) = projects_dir() else {
        return 0;
    };
    let mut hasher = Sha256::new();
    hasher.update(projects_dir.as_os_str().to_string_lossy().as_bytes());
    let Ok(entries) = fs::read_dir(&projects_dir) else {
        return u128::from_be_bytes(hasher.finalize()[..16].try_into().unwrap_or([0_u8; 16]));
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    for directory in paths {
        hasher.update(directory.as_os_str().to_string_lossy().as_bytes());
        for name in [MANIFEST_FILE, PROFILE_FILE] {
            let path = directory.join(name);
            hasher.update(name.as_bytes());
            match fs::read(path) {
                Ok(contents) => hasher.update(contents),
                Err(error) => hasher.update(error.kind().to_string().as_bytes()),
            }
        }
    }
    u128::from_be_bytes(hasher.finalize()[..16].try_into().unwrap_or([0_u8; 16]))
}

fn merge_owned_projects_from_dir(
    gateway: &mut GatewayConfig,
    projects_dir: &Path,
) -> Result<usize, String> {
    if !projects_dir.exists() {
        return Ok(0);
    }
    let mut directories = fs::read_dir(projects_dir)
        .map_err(|error| format!("cannot read {}: {error}", projects_dir.display()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    directories.sort();
    let mut loaded = 0_usize;
    for profile_dir in directories {
        let manifest = read_owned_manifest(&profile_dir)?;
        let profile_path = profile_dir.join(PROFILE_FILE);
        let profile_text = fs::read_to_string(&profile_path)
            .map_err(|error| format!("cannot read {}: {error}", profile_path.display()))?;
        let routing = routing_config_from_toml(
            &profile_text,
            &profile_path.display().to_string(),
            None,
            TierOrderPolicy::StrictInput,
        )
        .map_err(|error| error.to_string())?;
        if gateway.profiles.contains_key(&manifest.profile_id)
            || gateway.workspaces.contains_key(&manifest.workspace_id)
            || gateway.keys.contains_key(&manifest.key_id)
        {
            return Err(format!(
                "owned project {} collides with user-managed gateway configuration",
                manifest.canonical_repository
            ));
        }
        gateway
            .profiles
            .insert(manifest.profile_id.clone(), routing);
        gateway.workspaces.insert(
            manifest.workspace_id.clone(),
            Workspace {
                models: Vec::new(),
                profile: Some(manifest.profile_id.clone()),
                budget: None,
                rate_limit: None,
            },
        );
        gateway.keys.insert(
            manifest.key_id.clone(),
            VirtualKey {
                hash: manifest.key_hash,
                tags: vec!["local-project".to_owned()],
                workspace: Some(manifest.workspace_id),
                budget: None,
                rate_limit: None,
                models: Vec::new(),
            },
        );
        loaded = loaded.saturating_add(1);
    }
    gateway.allow_unauthenticated_default = loaded > 0;
    Ok(loaded)
}

fn resolve_repository(candidate: &str, token: Option<&str>) -> Result<CanonicalRepository, String> {
    let (owner, repository) = parse_repository_input(candidate)?;
    let endpoint = format!("https://api.github.com/repos/{owner}/{repository}");
    let client = Client::builder()
        .https_only(true)
        .user_agent(format!("wayfinder-router/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("cannot initialize GitHub client: {error}"))?;
    let mut request = client
        .get(endpoint)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .map_err(|error| format!("cannot resolve GitHub repository: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub repository is missing or inaccessible (HTTP {})",
            response.status().as_u16()
        ));
    }
    let payload: Value = response
        .json()
        .map_err(|error| format!("invalid GitHub repository response: {error}"))?;
    repository_from_github_payload(&payload)
}

fn repository_from_github_payload(payload: &Value) -> Result<CanonicalRepository, String> {
    if payload.get("archived").and_then(Value::as_bool) != Some(false) {
        return Err("GitHub repository is archived or its state is ambiguous".to_owned());
    }
    let full_name = payload
        .get("full_name")
        .and_then(Value::as_str)
        .ok_or_else(|| "GitHub repository response has no canonical name".to_owned())?;
    parse_repository_input(full_name)?;
    let html_url = payload
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("https://github.com/"))
        .ok_or_else(|| "GitHub repository response has no canonical URL".to_owned())?;
    Ok(CanonicalRepository {
        full_name: full_name.to_owned(),
        html_url: html_url.to_owned(),
    })
}

fn parse_repository_input(value: &str) -> Result<(String, String), String> {
    let trimmed = value.trim().trim_end_matches('/').trim_end_matches(".git");
    let slug = if let Some(slug) = trimmed.strip_prefix("https://github.com/") {
        slug
    } else if trimmed.contains("://") || trimmed.starts_with("git@") {
        return Err(
            "repository must be OWNER/NAME or an https://github.com/OWNER/NAME URL".to_owned(),
        );
    } else {
        trimmed
    };
    let parts = slug.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || parts.iter().any(|part| !valid_repository_segment(part)) {
        return Err(
            "repository must identify exactly one unambiguous GitHub OWNER/NAME".to_owned(),
        );
    }
    Ok((parts[0].to_owned(), parts[1].to_owned()))
}

fn valid_repository_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn repository_candidate(explicit: Option<&str>, root: &Path) -> Result<String, String> {
    if let Some(explicit) = explicit {
        parse_repository_input(explicit)?;
        return Ok(explicit.to_owned());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|error| format!("cannot inspect git origin: {error}"))?;
    if !output.status.success() {
        return Err("cannot discover repository; pass --repository OWNER/NAME".to_owned());
    }
    let origin =
        String::from_utf8(output.stdout).map_err(|_| "git origin is not valid UTF-8".to_owned())?;
    let origin = origin.trim();
    if let Some(slug) = origin.strip_prefix("git@github.com:") {
        let slug = slug.trim_end_matches(".git");
        parse_repository_input(slug)?;
        return Ok(slug.to_owned());
    }
    parse_repository_input(origin)?;
    Ok(origin.to_owned())
}

fn canonical_repository_root(explicit: Option<&Path>) -> Result<PathBuf, String> {
    let root = if let Some(root) = explicit {
        root.to_path_buf()
    } else {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .map_err(|error| format!("cannot discover repository root: {error}"))?;
        if !output.status.success() {
            return Err("current directory is not a git repository; pass --root PATH".to_owned());
        }
        PathBuf::from(
            String::from_utf8(output.stdout)
                .map_err(|_| "repository root is not valid UTF-8".to_owned())?
                .trim(),
        )
    };
    let canonical = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve repository root {}: {error}", root.display()))?;
    if !canonical.is_dir() {
        return Err("repository root is not a directory".to_owned());
    }
    Ok(canonical)
}

fn projects_dir() -> Result<PathBuf, String> {
    if let Some(value) = std::env::var_os(PROJECTS_DIR_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    if let Some(value) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value).join("wayfinder/projects"));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".config/wayfinder/projects"))
        .ok_or_else(|| {
            "cannot locate project directory; set WAYFINDER_ROUTER_PROJECTS_DIR".to_owned()
        })
}

fn project_token(
    prompt: bool,
    stdin: &mut dyn Read,
    stderr: &mut dyn Write,
) -> Result<(String, TokenSource), String> {
    if let Some(token) = project_token_from_environment() {
        return Ok((token, TokenSource::Environment));
    }
    if !prompt {
        return Err(format!(
            "set {PROJECT_TOKEN_ENV} or use --prompt-token; tokens are never accepted as command-line values"
        ));
    }
    write_error(stderr, "Wayfinder project token (input is not persisted):");
    let mut token = String::new();
    BufReader::new(stdin)
        .take(u64::try_from(MAX_TOKEN_BYTES.saturating_add(2)).unwrap_or(u64::MAX))
        .read_line(&mut token)
        .map_err(|error| format!("cannot read project token: {error}"))?;
    let token = token.trim_end_matches(['\n', '\r']).to_owned();
    validate_token(&token)?;
    Ok((token, TokenSource::Prompt))
}

fn project_token_from_environment() -> Option<String> {
    std::env::var(PROJECT_TOKEN_ENV)
        .ok()
        .filter(|token| validate_token(token).is_ok())
}

fn project_token_matches(manifest: &ProjectManifest) -> Option<bool> {
    project_token_from_environment()
        .map(|token| wayfinder_gateway::auth::hash_key(&token) == manifest.key_hash)
}

fn github_token() -> Option<String> {
    std::env::var(GITHUB_TOKEN_ENV)
        .ok()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .filter(|token| !token.is_empty() && token.len() <= MAX_TOKEN_BYTES)
}

fn validate_token(token: &str) -> Result<(), String> {
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES || token.chars().any(char::is_control) {
        return Err("project token must be 1-512 non-control UTF-8 bytes".to_owned());
    }
    Ok(())
}

fn validate_visible_id(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!(
            "{label} id must use 1-64 ASCII letters, digits, '-' or '_'"
        ));
    }
    Ok(())
}

fn generated_profile() -> String {
    format!(
        "# wayfinder-generated: project-profile-v1\n# safe-to-replace: true\n[routing]\nthreshold = {DEFAULT_THRESHOLD}\n"
    )
}

fn directory_id(repository: &str, root: &Path) -> String {
    format!(
        "project-{}",
        short_digest(&format!(
            "{}\0{}",
            repository.to_ascii_lowercase(),
            root.display()
        ))
    )
}

fn short_digest(value: &str) -> String {
    sha256(value.as_bytes())[..24].to_owned()
}

fn profile_sha256(value: &str) -> String {
    sha256(value.as_bytes())
}

fn sha256(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_project_atomically(
    projects_dir: &Path,
    profile_dir: &Path,
    manifest: &ProjectManifest,
    profile: &str,
) -> Result<(), String> {
    fs::create_dir_all(projects_dir)
        .map_err(|error| format!("cannot create {}: {error}", projects_dir.display()))?;
    let staging = projects_dir.join(format!(".setup-{}", Uuid::new_v4().simple()));
    fs::create_dir(&staging)
        .map_err(|error| format!("cannot create setup staging directory: {error}"))?;
    set_private_directory(&staging)?;
    let result = (|| {
        write_private_file(&staging.join(PROFILE_FILE), profile.as_bytes())?;
        let payload = manifest_json(manifest);
        let mut bytes = serde_json::to_vec_pretty(&payload)
            .map_err(|error| format!("cannot encode project manifest: {error}"))?;
        bytes.push(b'\n');
        write_private_file(&staging.join(MANIFEST_FILE), &bytes)?;
        fs::rename(&staging, profile_dir)
            .map_err(|error| format!("cannot activate {}: {error}", profile_dir.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    file.write_all(contents)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync {}: {error}", path.display()))?;
    set_private_file(path)
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot protect {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot protect {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_private_file(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn manifest_json(manifest: &ProjectManifest) -> Value {
    json!({
        "schema_version": 1,
        "managed_by": MANAGED_BY,
        "safe_to_replace": true,
        "canonical_repository": manifest.canonical_repository,
        "repository_url": manifest.repository_url,
        "repository_root": manifest.repository_root,
        "profile_id": manifest.profile_id,
        "workspace_id": manifest.workspace_id,
        "key_id": manifest.key_id,
        "key_hash": manifest.key_hash,
        "generated_profile_sha256": manifest.generated_profile_sha256,
    })
}

fn read_owned_manifest(profile_dir: &Path) -> Result<ProjectManifest, String> {
    let path = profile_dir.join(MANIFEST_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("invalid project manifest {}: {error}", path.display()))?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || value.get("managed_by").and_then(Value::as_str) != Some(MANAGED_BY)
        || value.get("safe_to_replace").and_then(Value::as_bool) != Some(true)
    {
        return Err(format!(
            "{} is not an owned Wayfinder project directory; no changes made",
            profile_dir.display()
        ));
    }
    let string = |name: &str| -> Result<String, String> {
        value
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("project manifest is missing {name}"))
    };
    let canonical_repository = string("canonical_repository")?;
    parse_repository_input(&canonical_repository)?;
    let repository_url = string("repository_url")?;
    let repository_root = PathBuf::from(string("repository_root")?);
    let profile_id = string("profile_id")?;
    let workspace_id = string("workspace_id")?;
    let key_id = string("key_id")?;
    validate_visible_id(&profile_id, "profile")?;
    validate_visible_id(&workspace_id, "workspace")?;
    validate_visible_id(&key_id, "key")?;
    let key_hash = string("key_hash")?;
    let generated_profile_sha256 = string("generated_profile_sha256")?;
    if key_hash.len() != 64 || !key_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("project manifest contains an invalid key hash".to_owned());
    }
    if generated_profile_sha256.len() != 64
        || !generated_profile_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("project manifest contains an invalid profile hash".to_owned());
    }
    Ok(ProjectManifest {
        canonical_repository,
        repository_url,
        repository_root,
        profile_id,
        workspace_id,
        key_id,
        key_hash,
        generated_profile_sha256,
    })
}

fn ensure_matches(manifest: &ProjectManifest, repository: &str, root: &Path) -> Result<(), String> {
    if !manifest
        .canonical_repository
        .eq_ignore_ascii_case(repository)
        || manifest.repository_root != root
    {
        return Err("owned project directory identity does not match; no changes made".to_owned());
    }
    Ok(())
}

fn find_project(
    projects_dir: &Path,
    root: &Path,
    expected_repository: Option<&str>,
) -> Result<Option<(PathBuf, ProjectManifest)>, String> {
    if !projects_dir.exists() {
        return Ok(None);
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(projects_dir)
        .map_err(|error| format!("cannot read {}: {error}", projects_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("cannot inspect project directory: {error}"))?;
        if !entry.file_type().is_ok_and(|kind| kind.is_dir())
            || entry.file_name().to_string_lossy().starts_with('.')
        {
            continue;
        }
        let manifest = read_owned_manifest(&entry.path())?;
        if manifest.repository_root == root
            && expected_repository
                .is_none_or(|expected| manifest.canonical_repository.eq_ignore_ascii_case(expected))
        {
            matches.push((entry.path(), manifest));
        }
    }
    if matches.len() > 1 {
        return Err(
            "multiple owned project profiles match this repository; refusing ambiguity".to_owned(),
        );
    }
    Ok(matches.pop())
}

fn status_payload(
    status: &str,
    manifest: &ProjectManifest,
    profile_dir: &Path,
    setup_required: bool,
    token_source: Option<&str>,
    token_matches: Option<bool>,
    profile_modified: bool,
) -> Value {
    json!({
        "schema_version": 1,
        "status": status,
        "canonical_repository": manifest.canonical_repository,
        "repository_url": manifest.repository_url,
        "repository_root": manifest.repository_root,
        "profile_directory": profile_dir,
        "profile_id": manifest.profile_id,
        "workspace_id": manifest.workspace_id,
        "key_id": manifest.key_id,
        "owned": !setup_required,
        "token_source": token_source,
        "token_matches": token_matches,
        "setup_required": setup_required,
        "profile_modified": profile_modified,
    })
}

fn write_human(stdout: &mut dyn Write, payload: &Value) {
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    write_output(stdout, &format!("Project status: {status}"));
    for (label, key) in [
        ("Repository", "canonical_repository"),
        ("Profile directory", "profile_directory"),
        ("Profile", "profile_id"),
        ("Token source", "token_source"),
    ] {
        if let Some(value) = payload.get(key).and_then(Value::as_str) {
            write_output(stdout, &format!("{label}: {value}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_project_profile_uses_the_developer_starter_cut()
    -> Result<(), Box<dyn std::error::Error>> {
        let profile = generated_profile();
        let config = routing_config_from_toml(
            &profile,
            "generated-project-profile",
            None,
            TierOrderPolicy::StrictInput,
        )?;
        assert_eq!(
            config.tiers.get(1).map(|tier| tier.min_score),
            Some(DEFAULT_THRESHOLD)
        );
        Ok(())
    }

    #[test]
    fn repository_inputs_are_exact_and_unambiguous() {
        assert_eq!(
            parse_repository_input("https://github.com/asdecided/WayfinderRouter.git"),
            Ok(("asdecided".to_owned(), "WayfinderRouter".to_owned()))
        );
        assert!(parse_repository_input("https://example.com/a/b").is_err());
        assert!(parse_repository_input("a/b/issues/1").is_err());
        assert!(parse_repository_input("a").is_err());
    }

    #[test]
    fn manifest_contains_only_the_project_key_hash() {
        let root = PathBuf::from("/tmp/repository");
        let token = "wf-project-secret";
        let manifest = ProjectManifest {
            canonical_repository: "asdecided/WayfinderRouter".to_owned(),
            repository_url: "https://github.com/asdecided/WayfinderRouter".to_owned(),
            repository_root: root,
            profile_id: "project-abc".to_owned(),
            workspace_id: "project-abc".to_owned(),
            key_id: "project-abc".to_owned(),
            key_hash: wayfinder_gateway::auth::hash_key(token),
            generated_profile_sha256: profile_sha256(&generated_profile()),
        };
        let rendered = manifest_json(&manifest).to_string();
        assert!(!rendered.contains(token));
        assert!(rendered.contains("managed_by"));
        assert!(rendered.contains(&manifest.key_hash));
    }

    #[test]
    fn github_payload_must_be_active_and_canonical() {
        let active = json!({
            "archived": false,
            "full_name": "asdecided/WayfinderRouter",
            "html_url": "https://github.com/asdecided/WayfinderRouter"
        });
        assert_eq!(
            repository_from_github_payload(&active),
            Ok(CanonicalRepository {
                full_name: "asdecided/WayfinderRouter".to_owned(),
                html_url: "https://github.com/asdecided/WayfinderRouter".to_owned(),
            })
        );
        let mut archived = active.clone();
        archived["archived"] = json!(true);
        assert!(repository_from_github_payload(&archived).is_err());
        let mut ambiguous = active;
        ambiguous["full_name"] = json!("asdecided/WayfinderRouter/issues");
        assert!(repository_from_github_payload(&ambiguous).is_err());
    }

    #[test]
    fn atomic_setup_never_clobbers_an_existing_directory() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = std::env::temp_dir().join(format!("wayfinder-no-clobber-{}", Uuid::new_v4()));
        let profile_dir = root.join("project-existing");
        fs::create_dir_all(&profile_dir)?;
        fs::write(profile_dir.join("user.txt"), "keep me")?;
        let manifest = ProjectManifest {
            canonical_repository: "asdecided/WayfinderRouter".to_owned(),
            repository_url: "https://github.com/asdecided/WayfinderRouter".to_owned(),
            repository_root: root.join("repo"),
            profile_id: "project-abc".to_owned(),
            workspace_id: "project-abc".to_owned(),
            key_id: "project-abc".to_owned(),
            key_hash: wayfinder_gateway::auth::hash_key("secret"),
            generated_profile_sha256: profile_sha256(&generated_profile()),
        };
        assert!(
            write_project_atomically(&root, &profile_dir, &manifest, &generated_profile()).is_err()
        );
        assert_eq!(fs::read_to_string(profile_dir.join("user.txt"))?, "keep me");
        assert_eq!(
            fs::read_dir(&root)?
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(".setup-"))
                .count(),
            0
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn owned_project_merge_is_bounded_and_never_overrides_user_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("wayfinder-project-{}", Uuid::new_v4()));
        let profile_dir = root.join("project-one");
        fs::create_dir_all(&profile_dir)?;
        let profile = generated_profile();
        let manifest = ProjectManifest {
            canonical_repository: "asdecided/WayfinderRouter".to_owned(),
            repository_url: "https://github.com/asdecided/WayfinderRouter".to_owned(),
            repository_root: root.join("repo"),
            profile_id: "project-abc".to_owned(),
            workspace_id: "project-abc".to_owned(),
            key_id: "project-abc".to_owned(),
            key_hash: wayfinder_gateway::auth::hash_key("secret"),
            generated_profile_sha256: profile_sha256(&profile),
        };
        fs::write(profile_dir.join(PROFILE_FILE), profile)?;
        fs::write(
            profile_dir.join(MANIFEST_FILE),
            serde_json::to_vec_pretty(&manifest_json(&manifest))?,
        )?;
        let mut gateway = GatewayConfig::default();
        assert_eq!(merge_owned_projects_from_dir(&mut gateway, &root)?, 1);
        assert!(gateway.allow_unauthenticated_default);
        assert_eq!(
            gateway.workspaces["project-abc"].profile.as_deref(),
            Some("project-abc")
        );
        let mut collision = GatewayConfig::default();
        collision.profiles.insert(
            "project-abc".to_owned(),
            routing_config_from_toml("", "test", None, TierOrderPolicy::StrictInput)?,
        );
        assert!(merge_owned_projects_from_dir(&mut collision, &root).is_err());
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
