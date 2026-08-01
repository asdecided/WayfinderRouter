use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

use serde::Deserialize;
use wayfinder_service::units::{
    agent_path, detect_platform, launchd_plist_with_home, systemd_unit, systemd_unit_path,
};

const SERVICE_UNIT_VECTORS: &str = include_str!("../fixtures/service-units.json");

#[derive(Debug, Deserialize)]
struct Corpus {
    schema_version: String,
    home: String,
    platforms: BTreeMap<String, String>,
    paths: Paths,
    launchd: Vec<LaunchdCase>,
    systemd: Vec<SystemdCase>,
}

#[derive(Debug, Deserialize)]
struct Paths {
    launchd: String,
    systemd: String,
}

#[derive(Debug, Deserialize)]
struct LaunchdCase {
    name: String,
    args: Vec<String>,
    label: Option<String>,
    log_dir: Option<String>,
    output: String,
}

#[derive(Debug, Deserialize)]
struct SystemdCase {
    name: String,
    args: Vec<String>,
    description: Option<String>,
    output: String,
}

#[test]
fn service_units_preserve_frozen_bytes() -> Result<(), Box<dyn Error>> {
    let corpus: Corpus = serde_json::from_str(SERVICE_UNIT_VECTORS)?;
    assert_eq!(corpus.schema_version, "1");
    assert_eq!(corpus.launchd.len(), 3);
    assert_eq!(corpus.systemd.len(), 3);

    for (platform, expected) in &corpus.platforms {
        assert_eq!(
            detect_platform(Some(platform)).as_str(),
            expected,
            "{platform}"
        );
    }

    let home = Path::new(&corpus.home);
    assert_eq!(
        agent_path(Some(home))?.to_string_lossy(),
        corpus.paths.launchd
    );
    assert_eq!(
        systemd_unit_path(Some(home))?.to_string_lossy(),
        corpus.paths.systemd
    );

    for case in &corpus.launchd {
        let actual = launchd_plist_with_home(
            &case.args,
            case.label.as_deref(),
            case.log_dir.as_deref(),
            Some(home),
        );
        assert_eq!(actual, case.output, "{} launchd bytes", case.name);
    }
    for case in &corpus.systemd {
        let actual = systemd_unit(&case.args, case.description.as_deref());
        assert_eq!(actual, case.output, "{} systemd bytes", case.name);
    }
    Ok(())
}
