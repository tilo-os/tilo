//! Every shipped app manifest must parse and validate.
//!
//! A `.qpkg` cannot be built from a manifest that does not decode and pass
//! [`pkg::validate_manifest`], so this walks the whole `apps/` tree and holds
//! the fleet to that bar in one place: a new app with a malformed manifest, or
//! a schema change that invalidates an existing one, fails here rather than at
//! image-build time. It also exercises the new `[capabilities]` block on every
//! manifest that carries one, since validation is where a duplicate or
//! unknown-capability declaration is caught.

use std::path::PathBuf;

use pkg::{validate_manifest, Manifest};

fn apps_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../apps")
}

#[test]
fn every_app_manifest_parses_and_validates() {
    let apps = apps_dir();
    let mut checked = 0;
    let mut failures = Vec::new();

    for entry in std::fs::read_dir(&apps).expect("apps/ dir readable") {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_dir() {
            continue;
        }
        let manifest_path = entry.path().join("qos-app.toml");
        if !manifest_path.exists() {
            // A crate under apps/ with no manifest yet is a gap the audit
            // tracks; it is not this test's job to fail on, since the test is
            // about the manifests that DO exist being valid.
            continue;
        }
        let src = std::fs::read_to_string(&manifest_path).unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();

        match Manifest::from_toml_str(&src) {
            Ok(m) => {
                if let Err(e) = validate_manifest(&m) {
                    failures.push(format!("{name}: validate failed: {e}"));
                } else {
                    checked += 1;
                }
            }
            Err(e) => failures.push(format!("{name}: decode failed: {e}")),
        }
    }

    assert!(
        failures.is_empty(),
        "{} app manifest(s) do not build:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
    assert!(checked >= 40, "expected the whole app fleet, only checked {checked}");
}

/// The privileged system apps must declare their broker capability — a
/// regression guard so a refactor cannot silently drop the declaration that
/// makes the broker refuse them.
#[test]
fn privileged_apps_declare_their_capability() {
    use pkg::PrivilegedCapability;

    let expected = [
        ("firewall", PrivilegedCapability::FirewallAdmin),
        ("disks", PrivilegedCapability::BlockDevice),
        ("imager", PrivilegedCapability::RawDeviceWrite),
        ("containers", PrivilegedCapability::ContainerRuntime),
        ("vpn", PrivilegedCapability::VpnTun),
    ];

    for (app, cap) in expected {
        let path = apps_dir().join(app).join("qos-app.toml");
        let src = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {app} manifest"));
        let m = Manifest::from_toml_str(&src).unwrap_or_else(|e| panic!("{app}: {e}"));
        assert!(
            m.capabilities.declares(cap),
            "{app} must declare the `{}` capability",
            cap.wire()
        );
        assert_eq!(validate_manifest(&m), Ok(()), "{app} manifest must validate");
    }
}
