use std::{error::Error, fs, path::PathBuf};
use wayfinder_config::{
    PolicyLoadState, TierOrderPolicy, load_routing_config, load_routing_config_with_state,
    read_config_source,
};
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
        load_routing_config_with_state(&f.0, None, None, TierOrderPolicy::StrictInput)?.0,
        PolicyLoadState::Absent
    );
    let err = load_routing_config(&f.0, Some(&f.config()), None, TierOrderPolicy::StrictInput)
        .err()
        .ok_or("expected missing error")?;
    assert_eq!(err.load_state(), PolicyLoadState::Absent);
    assert!(load_routing_config(&f.0, None, None, TierOrderPolicy::StrictInput).is_ok());
    Ok(())
}

#[test]
fn invalid_and_unreadable_are_distinct_and_errors_omit_source() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    fs::write(f.config(), "[routing]\nthreshold = secret-private-value")?;
    let err = load_routing_config(&f.0, None, None, TierOrderPolicy::StrictInput)
        .err()
        .ok_or("expected invalid")?;
    assert_eq!(err.load_state(), PolicyLoadState::Invalid);
    assert!(!err.to_string().contains("secret-private-value"));
    fs::remove_file(f.config())?;
    fs::create_dir(f.config())?;
    let err = load_routing_config(&f.0, None, None, TierOrderPolicy::StrictInput)
        .err()
        .ok_or("expected unreadable")?;
    assert_eq!(err.load_state(), PolicyLoadState::Unreadable);
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
    assert_eq!(err.load_state(), PolicyLoadState::Unreadable);
    Ok(())
}

#[test]
fn subsequent_load_observes_policy_change_or_removal() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    fs::write(f.config(), "[routing]\nthreshold = 0.9")?;
    assert_eq!(
        load_routing_config_with_state(&f.0, None, None, TierOrderPolicy::StrictInput)?.0,
        PolicyLoadState::Loaded
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
