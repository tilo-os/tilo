//! The system image's apps are installed packages like any other: listed,
//! updated into the user's store, rolled back, and returned to the image's
//! version, through the same `tilo-install` a user's own package goes
//! through.
//!
//! The image's store is built the way the image build builds it (an install
//! with `TILO_APPS_DATA_DIR` naming it), then read as the system store of a
//! user session (`TILO_APPS_SYSTEM_DIR`, honoured by this debug build).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use std::process::Command;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::{Manifest, SigningIdentity, pack};

const APP: &str = "so.tilo.settings";

/// A manifest that, given `system`, requires that version of the system
/// interface.
fn manifest_requiring(version: &str, pub_hex: &str, system: Option<u32>) -> String {
    let services = system
        .map(|system| format!("\n[sandbox.services]\nsystem = \"required@{system}\"\n"))
        .unwrap_or_default();
    format!(
        r#"
[package]
id      = "{APP}"
version = "{version}"
kind    = "app"

[publisher]
identity  = "ed25519:{pub_hex}"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp    = "tilo-default"
landlock   = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
{services}"#
    )
}

struct Machine {
    root: tempfile::TempDir,
    image_key: SigningKey,
    registry_key: SigningKey,
}

impl Machine {
    fn new() -> Self {
        let mut rng = UnwrapErr(SysRng);
        Self {
            root: tempfile::tempdir().unwrap(),
            image_key: SigningKey::generate(&mut rng),
            registry_key: SigningKey::generate(&mut rng),
        }
    }

    fn image_store(&self) -> PathBuf {
        self.root.path().join("usr/share/tilo-apps")
    }

    fn data(&self) -> PathBuf {
        self.root.path().join("home/.local/share")
    }

    fn tapp(&self, version: &str, key: &SigningKey) -> PathBuf {
        self.tapp_requiring(version, key, None)
    }

    fn tapp_requiring(&self, version: &str, key: &SigningKey, system: Option<u32>) -> PathBuf {
        let build = self.root.path().join(format!("build-{version}"));
        let payload = build.join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), version.as_bytes()).unwrap();
        let pub_hex = hex::encode(key.verifying_key().to_bytes());
        let manifest =
            Manifest::from_toml_str(&manifest_requiring(version, &pub_hex, system)).unwrap();
        let identity = SigningIdentity::from_signing_key(key.clone());
        let packed = pack(&payload, &manifest, &identity).expect("pack");
        let path = build.join("pkg.tapp");
        let file = std::fs::File::create(&path).unwrap();
        tilo_app_package::tapp::write(file, &packed.meta, Some(&packed)).unwrap();
        path
    }

    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tilo-install"));
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.root.path().join("home"))
            .env("XDG_CONFIG_HOME", self.root.path().join("home/.config"))
            .env("XDG_DATA_HOME", self.data())
            .env("TILO_SYSTEM_INTERFACES", self.interfaces());
        c
    }

    /// The image's system-interface declaration; absent unless a test
    /// writes one.
    fn interfaces(&self) -> PathBuf {
        self.root
            .path()
            .join("usr/share/tilo/system-interfaces.toml")
    }

    /// The OS now serves exactly `system` of the system interface.
    fn serve_system(&self, system: &[u32]) {
        let path = self.interfaces();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let versions: Vec<String> = system.iter().map(ToString::to_string).collect();
        std::fs::write(
            path,
            format!("[interfaces]\nsystem = [{}]\n", versions.join(", ")),
        )
        .unwrap();
    }

    fn run(mut command: Command, args: &[&str]) -> (bool, String) {
        let out = command.args(args).output().expect("spawn tilo-install");
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    /// `tilo-install` building the image's store.
    fn image_build(&self, args: &[&str]) -> (bool, String) {
        let mut c = self.command();
        c.env("TILO_APPS_DATA_DIR", self.image_store());
        Self::run(c, args)
    }

    /// `tilo-install` in the user's session on the booted image.
    fn session(&self, args: &[&str]) -> (bool, String) {
        let mut c = self.command();
        c.env("TILO_APPS_SYSTEM_DIR", self.image_store()).env(
            "TILO_APPS_SYSTEM_PAYLOAD_DIR",
            self.root.path().join("usr/lib/tilo-apps"),
        );
        Self::run(c, args)
    }

    /// The version `list` reports for the app, and which store it runs from.
    fn listed(&self) -> (String, String) {
        let (ok, out) = self.session(&["list"]);
        assert!(ok, "{out}");
        let row = out
            .lines()
            .find(|line| line.starts_with(APP))
            .unwrap_or_else(|| panic!("{APP} is not listed:\n{out}"));
        let mut columns = row.split_whitespace().skip(1);
        (
            columns.next().unwrap().to_owned(),
            columns.next().unwrap().to_owned(),
        )
    }

    fn running_payload(&self) -> Vec<u8> {
        let catalog = package_store::Catalog::at(
            self.data().join("tilo-apps"),
            Some(package_store::SystemStore {
                data_dir: self.image_store(),
                payload_dir: self.root.path().join("usr/lib/tilo-apps"),
                interfaces: None,
            }),
        );
        let row = catalog.find(APP).unwrap().unwrap();
        std::fs::read(row.payload_root.join("bin/app")).unwrap()
    }
}

fn pub_hex(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

#[test]
fn an_image_app_updates_rolls_back_and_returns_to_the_image() {
    let machine = Machine::new();
    let baked = machine.tapp("0.1.10", &machine.image_key);
    let (ok, out) =
        machine.image_build(&["install", baked.to_str().unwrap(), "--allow-self-signed"]);
    assert!(ok, "{out}");

    // Phase `the_image_app_is_listed_as_installed`.
    assert_eq!(machine.listed(), ("0.1.10".into(), "image".into()));

    // Phase `installing_it_again_says_it_is_installed`.
    let (ok, out) = machine.session(&["install", baked.to_str().unwrap(), "--allow-self-signed"]);
    assert!(!ok && out.contains("already installed"), "{out}");

    // Phase `an_update_from_the_registry_installs_beside_the_image`.
    // The registry signs with its own publisher, which the signed registry
    // entry authorises (`app update` passes it as --require-publisher).
    let newer = machine.tapp("0.1.12", &machine.registry_key);
    let (ok, out) = machine.session(&["update", APP, "--from", newer.to_str().unwrap()]);
    assert!(
        !ok && out.contains("changed publisher"),
        "a local file may not change the publisher unasked: {out}"
    );
    let (ok, out) = machine.session(&[
        "update",
        APP,
        "--from",
        newer.to_str().unwrap(),
        "--require-publisher",
        &pub_hex(&machine.registry_key),
        "--allow-self-signed",
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("0.1.10 -> 0.1.12"), "{out}");
    assert_eq!(machine.listed(), ("0.1.12".into(), "user".into()));
    assert_eq!(machine.running_payload(), b"0.1.12");
    assert!(
        machine.image_store().join("registry.toml").exists(),
        "the image's store is never written"
    );

    // Phase `a_rollback_below_the_image_is_held`.
    let older = machine.tapp("0.1.9", &machine.registry_key);
    let (ok, out) = machine.session(&[
        "update",
        APP,
        "--from",
        older.to_str().unwrap(),
        "--require-publisher",
        &pub_hex(&machine.registry_key),
        "--allow-self-signed",
        "--allow-downgrade",
    ]);
    assert!(ok, "{out}");
    assert_eq!(machine.listed(), ("0.1.9".into(), "held".into()));
    assert_eq!(machine.running_payload(), b"0.1.9");

    // Phase `removing_the_user_copy_returns_to_the_image`.
    let (ok, out) = machine.session(&["uninstall", APP, "--keep-state"]);
    assert!(ok, "{out}");
    assert!(out.contains("from the system image"), "{out}");
    assert_eq!(machine.listed(), ("0.1.10".into(), "image".into()));
    assert_eq!(machine.running_payload(), b"0.1.10");

    // Phase `the_image_copy_itself_cannot_be_removed`.
    let (ok, out) = machine.session(&["uninstall", APP, "--keep-state"]);
    assert!(!ok && out.contains("ships with the system image"), "{out}");
}

#[test]
fn an_image_update_supersedes_an_older_user_copy() {
    let machine = Machine::new();
    let baked = machine.tapp("0.1.10", &machine.image_key);
    assert!(
        machine
            .image_build(&["install", baked.to_str().unwrap(), "--allow-self-signed"])
            .0
    );
    let newer = machine.tapp("0.1.12", &machine.image_key);
    let (ok, out) = machine.session(&[
        "update",
        APP,
        "--from",
        newer.to_str().unwrap(),
        "--allow-self-signed",
    ]);
    assert!(ok, "{out}");
    assert_eq!(machine.listed(), ("0.1.12".into(), "user".into()));

    // The next image ships an even newer version: it runs, not the user's
    // older copy, and the next update clears that copy away.
    let next_image = machine.tapp("0.1.20", &machine.image_key);
    let (ok, out) = machine.image_build(&[
        "install",
        next_image.to_str().unwrap(),
        "--allow-self-signed",
        "--force",
    ]);
    assert!(ok, "{out}");
    assert_eq!(machine.listed(), ("0.1.20".into(), "image".into()));
    assert_eq!(machine.running_payload(), b"0.1.20");

    let (ok, out) = machine.session(&["update"]);
    // No registries are configured, so updating the image's app itself
    // fails; the stale copy is gone either way.
    assert!(
        out.contains("the system image ships a newer version; removed this copy"),
        "{ok} {out}"
    );
    let user_rows =
        package_store::Registry::load(&machine.data().join("tilo-apps/registry.toml")).unwrap();
    assert!(user_rows.find(APP).is_none(), "{user_rows:?}");
    assert_eq!(machine.listed(), ("0.1.20".into(), "image".into()));
}

#[test]
fn an_update_the_os_cannot_serve_is_refused_and_the_image_copy_is_the_fallback() {
    let machine = Machine::new();
    let baked = machine.tapp_requiring("0.1.10", &machine.image_key, Some(1));
    assert!(
        machine
            .image_build(&["install", baked.to_str().unwrap(), "--allow-self-signed"])
            .0
    );
    machine.serve_system(&[1]);
    let update = |tapp: &PathBuf| {
        machine.session(&[
            "update",
            APP,
            "--from",
            tapp.to_str().unwrap(),
            "--require-publisher",
            &pub_hex(&machine.registry_key),
            "--allow-self-signed",
        ])
    };

    // Phase `a_build_for_an_unserved_interface_is_refused`.
    let for_newer_os = machine.tapp_requiring("0.2.0", &machine.registry_key, Some(2));
    let (ok, out) = update(&for_newer_os);
    assert!(!ok, "{out}");
    assert!(
        out.contains("system@2 (this system provides system@1)"),
        "{out}"
    );
    assert_eq!(machine.listed(), ("0.1.10".into(), "image".into()));

    // Phase `an_os_serving_it_installs_and_runs_it`.
    machine.serve_system(&[1, 2]);
    let (ok, out) = update(&for_newer_os);
    assert!(ok, "{out}");
    assert_eq!(machine.listed(), ("0.2.0".into(), "user".into()));

    // Phase `after_an_os_rollback_the_image_copy_runs`.
    machine.serve_system(&[1]);
    assert_eq!(machine.listed(), ("0.1.10".into(), "image".into()));

    // Phase `removing_the_update_keeps_the_image_copy`.
    let (ok, out) = machine.session(&["uninstall", APP, "--keep-state"]);
    assert!(ok, "{out}");
    assert_eq!(machine.listed(), ("0.1.10".into(), "image".into()));
    let user_rows =
        package_store::Registry::load(&machine.data().join("tilo-apps/registry.toml")).unwrap();
    assert!(user_rows.find(APP).is_none(), "{user_rows:?}");
}
