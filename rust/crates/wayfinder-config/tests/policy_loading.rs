use std::{error::Error, fs, path::PathBuf};
use wayfinder_config::{ConfigError, TierOrderPolicy, load_routing_config, read_config_source};
use wayfinder_routing_core::{DEFAULT_THRESHOLD, RoutingConfig};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "wayfinder-policy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
    fn config(&self) -> PathBuf {
        self.0.join("wayfinder-router.toml")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn explicit_missing_policy_never_becomes_defaults() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    assert!(read_config_source(&f.0, None)?.is_none());
    assert_eq!(
        load_routing_config(&f.0, None, None, TierOrderPolicy::StrictInput)?,
        RoutingConfig::binary(DEFAULT_THRESHOLD)
    );
    let err = load_routing_config(&f.0, Some(&f.config()), None, TierOrderPolicy::StrictInput)
        .err()
        .ok_or("expected missing error")?;
    assert!(matches!(err, ConfigError::Missing { .. }));
    Ok(())
}

#[test]
fn invalid_and_unreadable_are_distinct_and_errors_omit_source() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    fs::write(f.config(), "[routing]\nthreshold = secret-private-value")?;
    let err = load_routing_config(&f.0, None, None, TierOrderPolicy::StrictInput)
        .err()
        .ok_or("expected invalid")?;
    assert!(matches!(
        err,
        ConfigError::InvalidToml { .. } | ConfigError::InvalidValue { .. }
    ));
    assert!(!err.to_string().contains("secret-private-value"));
    fs::remove_file(f.config())?;
    fs::create_dir(f.config())?;
    let err = load_routing_config(&f.0, None, None, TierOrderPolicy::StrictInput)
        .err()
        .ok_or("expected unreadable")?;
    assert!(matches!(err, ConfigError::Read { .. }));
    Ok(())
}

#[cfg(unix)]
#[test]
fn broken_nearest_symlink_blocks_parent_fallback() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    fs::write(f.config(), "[routing]\nthreshold = 0.9")?;
    let child = f.0.join("child");
    fs::create_dir(&child)?;
    std::os::unix::fs::symlink("missing", child.join("wayfinder-router.toml"))?;
    let err = load_routing_config(&child, None, None, TierOrderPolicy::StrictInput)
        .err()
        .ok_or("expected unreadable")?;
    assert!(matches!(err, ConfigError::Read { .. }));
    Ok(())
}

#[test]
fn subsequent_load_observes_policy_change_or_removal() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    fs::write(f.config(), "[routing]\nthreshold = 0.9")?;
    assert_ne!(
        load_routing_config(&f.0, None, None, TierOrderPolicy::StrictInput)?,
        RoutingConfig::binary(DEFAULT_THRESHOLD)
    );
    let before = load_routing_config(&f.0, Some(&f.config()), None, TierOrderPolicy::StrictInput)?;
    fs::write(f.config(), "[routing]\nthreshold = 0.1")?;
    let after = load_routing_config(&f.0, Some(&f.config()), None, TierOrderPolicy::StrictInput)?;
    assert_ne!(before.tiers, after.tiers);
    fs::remove_file(f.config())?;
    assert!(
        load_routing_config(&f.0, Some(&f.config()), None, TierOrderPolicy::StrictInput).is_err()
    );
    Ok(())
}
