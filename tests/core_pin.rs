//! Frozen core pin: one exact full SHA in metadata, dependencies, and lockfile.

use std::fs;
use std::path::PathBuf;

const FROZEN_REV: &str = "fc8b0c8179640a8ffe460a64b3690e35c2f7eb73";
const STALE_PINS: &[&str] = &["f5f71eb", "f5f71ebc2f38e276c35f2aebaf63f4e2780efe99"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn require_full_git_rev(rev: &str, label: &str) -> Result<(), String> {
    if rev.len() != 40 || !rev.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(format!(
            "{label} must be a full 40-character git SHA, got {rev:?}"
        ));
    }
    if STALE_PINS.contains(&rev) {
        return Err(format!("{label} still pins stale core revision {rev}"));
    }
    Ok(())
}

fn quoted_rev_after<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("{key} = \"");
    let start = source.find(&pattern)? + pattern.len();
    let end = source[start..].find('"')? + start;
    Some(&source[start..end])
}

#[test]
fn rustscript_core_pin_is_frozen_full_sha() {
    let root = repo_root();
    let cargo_toml = fs::read_to_string(root.join("Cargo.toml")).expect("Cargo.toml");
    let cargo_lock = fs::read_to_string(root.join("Cargo.lock")).expect("Cargo.lock");

    let metadata_rev = quoted_rev_after(&cargo_toml, "rustscript-rev").expect("rustscript-rev");
    let host_rev = cargo_toml
        .split("pd-host-function")
        .nth(1)
        .and_then(|rest| quoted_rev_after(rest, "rev"))
        .expect("pd-host-function rev");
    let vm_rev = cargo_toml
        .split("package = \"pd-vm\"")
        .nth(1)
        .and_then(|rest| quoted_rev_after(rest, "rev"))
        .expect("pd-vm rev");

    require_full_git_rev(metadata_rev, "rustscript-rev").unwrap();
    require_full_git_rev(host_rev, "pd-host-function.rev").unwrap();
    require_full_git_rev(vm_rev, "pd-vm.rev").unwrap();
    assert_eq!(metadata_rev, FROZEN_REV);
    assert_eq!(host_rev, FROZEN_REV);
    assert_eq!(vm_rev, FROZEN_REV);

    let expected_source =
        format!("git+https://github.com/rustscript-lang/rustscript?rev={FROZEN_REV}#{FROZEN_REV}");
    assert!(
        cargo_lock.contains(&expected_source),
        "Cargo.lock must resolve pd-vm/pd-host-function to {expected_source}"
    );
    assert!(require_full_git_rev("b1d6cff", "abbreviated").is_err());
    for stale in STALE_PINS {
        assert!(require_full_git_rev(stale, "stale").is_err());
        for (label, source) in [
            ("Cargo.toml", cargo_toml.as_str()),
            ("Cargo.lock", cargo_lock.as_str()),
        ] {
            assert!(
                !source.contains(stale),
                "{label} must not retain stale pin {stale}"
            );
        }
    }
    for public_pin in ["README.md", "docs/design/ngx-scheduler.md"] {
        let source = fs::read_to_string(root.join(public_pin)).expect("read public pin source");
        assert!(
            source.contains(FROZEN_REV),
            "{public_pin} must name {FROZEN_REV}"
        );
        for stale in STALE_PINS {
            assert!(
                !source.contains(stale),
                "{public_pin} must not retain stale pin {stale}"
            );
        }
    }
}
