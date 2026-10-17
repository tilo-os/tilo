//! Provider-sandboxed machines.
//!
//! A hosting provider's single-tenant VM or sandbox can be the security
//! boundary itself and offer no Landlock (gVisor implements none; stock
//! Firecracker guest kernels leave it out). There the image, never the
//! environment, declares the machine provider-sandboxed, and first-party
//! apps from the image's root-owned system store run without Landlock.
//! Bubblewrap, seccomp and the network namespace still apply. Apps a user
//! installs still require the complete Landlock policy.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

/// The image's declaration. Only root may write it.
pub const CONFIG_FILE: &str = "/etc/tilo/provider-sandbox.toml";

/// Whether a launch of the package at `payload_root` may run with Landlock
/// not (or only partially) enforced.
#[must_use]
pub fn landlock_best_effort(payload_root: &Path) -> bool {
    declared(Path::new(CONFIG_FILE))
        && system_app(payload_root, &package_store::layout::system_payload_dir())
}

/// Whether `config` is a root-owned file no one else can write that sets
/// `provider_sandboxed = true`.
fn declared(config: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(config) else {
        return false;
    };
    if !root_controlled(&metadata) || !metadata.is_file() {
        return false;
    }
    fs::read_to_string(config).is_ok_and(|text| parse(&text))
}

/// The one setting, in TOML's `key = value` form; anything else is ignored.
fn parse(text: &str) -> bool {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter_map(|line| line.split_once('='))
        .any(|(key, value)| key.trim() == "provider_sandboxed" && value.trim() == "true")
}

/// Whether `payload_root` lies in the image's system store: canonically
/// below `system_dir`, with the store and the package root both controlled
/// by root, so no user can place a package there.
fn system_app(payload_root: &Path, system_dir: &Path) -> bool {
    store_controlled_by(payload_root, system_dir, 0)
}

/// Whether `payload_root` lies canonically below `store`, with both owned by
/// `owner` and writable by no one else.
fn store_controlled_by(payload_root: &Path, store: &Path, owner: u32) -> bool {
    let (Ok(payload), Ok(store)) = (fs::canonicalize(payload_root), fs::canonicalize(store)) else {
        return false;
    };
    payload.starts_with(&store)
        && payload != store
        && [&store, &payload]
            .iter()
            .all(|path| fs::metadata(path).is_ok_and(|metadata| controlled_by(&metadata, owner)))
}

fn root_controlled(metadata: &fs::Metadata) -> bool {
    controlled_by(metadata, 0)
}

fn controlled_by(metadata: &fs::Metadata, owner: u32) -> bool {
    metadata.uid() == owner && metadata.permissions().mode() & 0o022 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_explicit_setting_declares_a_provider_sandbox() {
        assert!(parse("provider_sandboxed = true\n"));
        assert!(parse(
            "# hosted guest image\nprovider_sandboxed=true # Maritime\n"
        ));
        assert!(!parse("provider_sandboxed = false\n"));
        assert!(!parse("# provider_sandboxed = true\n"));
        assert!(!parse("provider_sandboxed = \"true\"\n"));
        assert!(!parse(""));
    }

    #[test]
    fn a_declaration_others_can_write_declares_nothing() {
        // Mode bits rather than ownership: the suite may run as root.
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("provider-sandbox.toml");
        fs::write(&config, "provider_sandboxed = true\n").unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(!declared(&config), "a writable declaration must not count");
        assert!(!declared(&dir.path().join("missing.toml")));
    }

    #[test]
    fn system_apps_live_below_a_root_controlled_store() {
        // A store owned by whoever runs the suite stands in for root's: a
        // test job in a user namespace sees the host's `/usr` owned by the
        // overflow uid, not by root.
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let package = store.join("pkg");
        fs::create_dir_all(&package).unwrap();
        for path in [&store, &package] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let owner = fs::metadata(&store).unwrap().uid();
        assert!(store_controlled_by(&package, &store, owner));
        assert!(!store_controlled_by(&store, &store, owner));
        assert!(
            !store_controlled_by(&package, &store, owner.wrapping_add(1)),
            "a store someone else owns is not the system store"
        );
        assert!(!store_controlled_by(dir.path(), &store, owner));
        fs::set_permissions(&package, fs::Permissions::from_mode(0o775)).unwrap();
        assert!(
            !store_controlled_by(&package, &store, owner),
            "a package others can write is not a system app"
        );
        fs::set_permissions(&package, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&store, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            !store_controlled_by(&package, &store, owner),
            "a writable store is not the system store"
        );
        assert!(!system_app(&package, Path::new("/usr")));
    }
}
