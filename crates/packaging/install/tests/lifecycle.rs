//! End-to-end package-manager lifecycle through the binary.
//!
//! install → list → verify → update → uninstall, plus the two things
//! that make it a package manager rather than an unpacker: it knows
//! what it depends on (the runtime bundle), and it distinguishes the
//! publisher's files from the user's data.
//!
//! Every test drives `tilo-install` as a subprocess with `XDG_DATA_HOME`
//! and `XDG_CONFIG_HOME` pointed at a tempdir, isolating the test data while
//! exercising the command's install path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use serde::Serialize;
use tilo_app_package::blobs::{BlobHash, BlobStore};
use tilo_app_package::signed::SignedRoot;
use tilo_app_package::{Manifest, MemoryBlobStore, SigningIdentity, pack};

fn install_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tilo-install"))
}

/// A package that declares no runtime bundle, so the lifecycle tests
/// exercise install/update/uninstall rather than dependency handling.
fn manifest_src(version: &str, pub_hex: &str, runtime: Option<&str>) -> String {
    let runtime_line = match runtime {
        Some(r) => format!("runtime = \"{r}\"\n"),
        None => String::new(),
    };
    format!(
        r#"
[package]
id      = "so.tilo.lifecycle"
version = "{version}"
kind    = "app"
{runtime_line}
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
"#
    )
}

fn write_payload(dir: &Path, body: &[u8]) {
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/app"), body).unwrap();
    std::fs::write(dir.join("data.txt"), b"payload data").unwrap();
}

fn fresh_key() -> (SigningKey, String) {
    let mut rng = OsRng;
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    (sk, pub_hex)
}

/// Build a `.tapp` with the layout `tilo-app-package pack` writes.
fn build_tapp(tapp_path: &Path, manifest: &Manifest, sk: &SigningKey, payload_dir: &Path) {
    #[derive(Serialize)]
    struct TappHeader {
        package_id: String,
        format: u32,
    }

    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(sk.clone());
    let id = pack(payload_dir, manifest, &identity, &mut store).expect("pack");

    let header_toml = toml::to_string(&TappHeader {
        package_id: id.to_string(),
        format: 1,
    })
    .unwrap();

    let signed_bytes = store.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
    let signed = SignedRoot::decode(&signed_bytes).unwrap();
    let manifest_bytes = store.get(&signed.manifest_hash).unwrap().unwrap();
    let collection_bytes = store.get(&signed.payload_root_hash).unwrap().unwrap();

    let file = std::fs::File::create(tapp_path).unwrap();
    let mut tar = tar::Builder::new(file);
    append(&mut tar, "tapp.toml", header_toml.as_bytes());
    append(&mut tar, "signed-root", &signed_bytes);
    append(&mut tar, "manifest.toml", &manifest_bytes);
    append(&mut tar, "collection.qcl", &collection_bytes);
    for (h, b) in store.iter() {
        append(&mut tar, &format!("blobs/{h}"), b);
    }
    tar.finish().unwrap();
}

fn append(tar: &mut tar::Builder<std::fs::File>, name: &str, bytes: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_path(name).unwrap();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, bytes).unwrap();
}

struct Env {
    home: tempfile::TempDir,
    sk: SigningKey,
    pub_hex: String,
}

impl Env {
    fn new() -> Self {
        let (sk, pub_hex) = fresh_key();
        Self {
            home: tempfile::tempdir().unwrap(),
            sk,
            pub_hex,
        }
    }

    fn xdg_data(&self) -> PathBuf {
        self.home.path().join("data")
    }

    fn tilo_apps(&self) -> PathBuf {
        self.xdg_data().join("tilo-apps")
    }

    fn payload_root(&self) -> PathBuf {
        self.tilo_apps().join("payloads/so.tilo.lifecycle")
    }

    fn registry_toml(&self) -> String {
        std::fs::read_to_string(self.tilo_apps().join("registry.toml")).unwrap_or_default()
    }

    /// Pack a `.tapp` at `version` whose entrypoint holds `body`.
    fn tapp(&self, version: &str, body: &[u8], runtime: Option<&str>) -> PathBuf {
        let build = self.home.path().join(format!("build-{version}"));
        let payload = build.join("payload");
        write_payload(&payload, body);
        let manifest =
            Manifest::from_toml_str(&manifest_src(version, &self.pub_hex, runtime)).unwrap();
        let path = build.join("pkg.tapp");
        build_tapp(&path, &manifest, &self.sk, &payload);
        path
    }

    fn cmd(&self, args: &[&str]) -> (bool, String, String) {
        let mut c = Command::new(install_bin());
        c.env_clear();
        c.env("PATH", std::env::var("PATH").unwrap_or_default());
        c.env("XDG_CONFIG_HOME", self.home.path().join("config"));
        c.env("XDG_DATA_HOME", self.xdg_data());
        c.env("HOME", self.home.path());
        c.args(args);
        let out = c.output().expect("spawn tilo-install");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// Simulate the app having saved something, in two rooms.
    fn seed_state(&self) {
        for room in ["personal", "b7c1-workspace"] {
            let dir = self
                .xdg_data()
                .join("tilo-rooms")
                .join(room)
                .join("so.tilo.lifecycle");
            std::fs::create_dir_all(dir.join(".local/share")).unwrap();
            std::fs::write(dir.join(".local/share/notes.bin"), b"the user's work").unwrap();
        }
    }

    fn state_exists(&self, room: &str) -> bool {
        self.xdg_data()
            .join("tilo-rooms")
            .join(room)
            .join("so.tilo.lifecycle")
            .exists()
    }
}

#[test]
fn install_list_verify_update_uninstall() {
    let env = Env::new();
    let v1 = env.tapp("1.0.0", b"version one", None);

    // --- install -----------------------------------------------------
    let (ok, out, err) = env.cmd(&["install", v1.to_str().unwrap()]);
    assert!(ok, "install failed: {out}{err}");
    assert!(out.contains("installed: so.tilo.lifecycle 1.0.0"), "{out}");
    assert!(env.payload_root().join("bin/app").exists());
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"version one"
    );
    assert!(
        env.registry_toml().contains("version = \"1.0.0\""),
        "{}",
        env.registry_toml()
    );

    // --- list --------------------------------------------------------
    let (ok, out, _) = env.cmd(&["list"]);
    assert!(ok);
    assert!(out.contains("so.tilo.lifecycle"), "{out}");
    assert!(out.contains("1.0.0"), "{out}");

    // --- verify ------------------------------------------------------
    let (ok, out, err) = env.cmd(&["verify"]);
    assert!(ok, "verify failed: {out}{err}");
    assert!(out.contains("ok       so.tilo.lifecycle 1.0.0"), "{out}");

    // The app saves something. From here on the payload and the state
    // have to be treated differently.
    env.seed_state();

    // --- update ------------------------------------------------------
    let v2 = env.tapp("1.1.0", b"version two", None);
    let (ok, out, err) = env.cmd(&[
        "update",
        "so.tilo.lifecycle",
        "--from",
        v2.to_str().unwrap(),
    ]);
    assert!(ok, "update failed: {out}{err}");
    assert!(
        out.contains("updated: so.tilo.lifecycle 1.0.0 -> 1.1.0"),
        "{out}"
    );
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"version two",
        "the payload must be the new version's"
    );
    assert!(env.registry_toml().contains("version = \"1.1.0\""));
    assert!(
        env.state_exists("personal") && env.state_exists("b7c1-workspace"),
        "an update must keep the user's data"
    );

    // Verification has to follow the swap: the recorded evidence is
    // now the new version's, and the old record would fail.
    let (ok, out, err) = env.cmd(&["verify", "so.tilo.lifecycle"]);
    assert!(ok, "verify after update failed: {out}{err}");
    assert!(out.contains("ok       so.tilo.lifecycle 1.1.0"), "{out}");

    // --- uninstall ---------------------------------------------------
    let (ok, out, err) = env.cmd(&["uninstall", "so.tilo.lifecycle", "--keep-state"]);
    assert!(ok, "uninstall failed: {out}{err}");
    assert!(
        out.contains("uninstalled: so.tilo.lifecycle 1.1.0"),
        "{out}"
    );
    assert!(!env.payload_root().exists(), "the payload must be gone");
    assert!(
        !env.registry_toml().contains("so.tilo.lifecycle"),
        "the registry entry must be gone: {}",
        env.registry_toml()
    );
    assert!(
        env.state_exists("personal"),
        "--keep-state must keep the user's data"
    );
}

/// Deleting someone's notes is not undoable, so it takes an explicit
/// instruction — and the uninstall says how much it is about to
/// delete before it does.
#[test]
fn purge_removes_the_state_that_keep_state_would_have_left() {
    let env = Env::new();
    let v1 = env.tapp("1.0.0", b"one", None);
    let (ok, _, err) = env.cmd(&["install", v1.to_str().unwrap()]);
    assert!(ok, "{err}");
    env.seed_state();

    let (ok, out, err) = env.cmd(&["uninstall", "so.tilo.lifecycle", "--purge"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("state    : deleted in 2 room(s)"), "{out}");
    assert!(!env.state_exists("personal"));
    assert!(!env.state_exists("b7c1-workspace"));
}

/// A non-interactive uninstall must not silently delete user data —
/// there is nobody to ask, so the answer is keep.
#[test]
fn a_non_interactive_uninstall_keeps_state_by_default() {
    let env = Env::new();
    let v1 = env.tapp("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);
    env.seed_state();

    // No --purge, no --keep-state, and stdin is not a terminal.
    let (ok, out, err) = env.cmd(&["uninstall", "so.tilo.lifecycle"]);
    assert!(ok, "{out}{err}");
    assert!(
        env.state_exists("personal"),
        "user data must survive an unattended uninstall: {out}"
    );
    assert!(out.contains("no terminal to ask on"), "{out}");
}

#[test]
fn dry_run_reports_the_change_without_making_it() {
    let env = Env::new();
    let v1 = env.tapp("1.0.0", b"one", None);

    let (ok, out, err) = env.cmd(&["install", v1.to_str().unwrap(), "--dry-run"]);
    assert!(ok, "{out}{err}");
    assert!(
        out.contains("plan: install so.tilo.lifecycle 1.0.0"),
        "{out}"
    );
    assert!(out.contains("signature : verified"), "{out}");
    assert!(out.contains("(nothing was written)"), "{out}");
    assert!(
        !env.payload_root().exists(),
        "--dry-run must not unpack anything"
    );
    assert!(
        env.registry_toml().is_empty(),
        "--dry-run must not touch the registry"
    );

    // And the same for an update and an uninstall.
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);
    env.seed_state();
    let v2 = env.tapp("2.0.0", b"two", None);
    let (ok, out, _) = env.cmd(&[
        "update",
        "so.tilo.lifecycle",
        "--from",
        v2.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(ok);
    assert!(
        out.contains("plan: update so.tilo.lifecycle 1.0.0 -> 2.0.0"),
        "{out}"
    );
    assert!(out.contains("state     : KEPT"), "{out}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"one",
        "--dry-run must not swap the payload"
    );

    let (ok, out, _) = env.cmd(&["uninstall", "so.tilo.lifecycle", "--dry-run"]);
    assert!(ok);
    assert!(
        out.contains("plan: uninstall so.tilo.lifecycle 1.0.0"),
        "{out}"
    );
    assert!(out.contains("saved data: 2 room(s)"), "{out}");
    assert!(out.contains("(nothing was removed)"), "{out}");
    assert!(env.payload_root().exists());
}

/// A tampered payload has to be caught by the thing whose job is to
/// catch it, and named precisely enough to act on.
#[test]
fn verify_catches_a_modified_payload() {
    let env = Env::new();
    let v1 = env.tapp("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);
    assert!(env.cmd(&["verify"]).0);

    std::fs::write(env.payload_root().join("bin/app"), b"malicious").unwrap();

    let (ok, out, err) = env.cmd(&["verify", "so.tilo.lifecycle"]);
    assert!(!ok, "a tampered payload must fail verification: {out}{err}");
    assert!(out.contains("FAILED   so.tilo.lifecycle"), "{out}");
    assert!(out.contains("modified: bin/app"), "{out}");
}

/// Refusing an install whose runtime bundle is absent is what keeps
/// "installed" and "launchable" the same thing. The message has to
/// name the command that fixes it.
#[test]
fn a_missing_runtime_bundle_refuses_the_install_and_says_how_to_fix_it() {
    let env = Env::new();
    let pkg = env.tapp("1.0.0", b"one", Some("tilo-runtime/2025.06"));

    let (ok, out, err) = env.cmd(&["install", pkg.to_str().unwrap()]);
    assert!(!ok, "install must refuse: {out}{err}");
    assert!(
        err.contains("declares runtime `tilo-runtime/2025.06`"),
        "{err}"
    );
    assert!(
        err.contains("tilo-install runtime install --version 2025.06"),
        "the refusal must name the command that fixes it: {err}"
    );
    assert!(err.contains("--ignore-missing-runtime"), "{err}");
    assert!(
        !env.payload_root().exists(),
        "a refused install must leave nothing behind"
    );

    // The escape hatch installs it, and says the package cannot launch.
    let (ok, out, err) = env.cmd(&["install", pkg.to_str().unwrap(), "--ignore-missing-runtime"]);
    assert!(ok, "{out}{err}");
    assert!(err.contains("will not launch until it is"), "{err}");

    // And `list` keeps saying so, rather than showing it as ready.
    let (_, out, _) = env.cmd(&["list"]);
    assert!(out.contains("tilo-runtime/2025.06 (MISSING)"), "{out}");
}

/// The dry run has to report the dependency too, or the plan is not a
/// plan.
#[test]
fn dry_run_names_a_missing_runtime_dependency() {
    let env = Env::new();
    let pkg = env.tapp("1.0.0", b"one", Some("tilo-runtime/2025.06"));
    let (ok, out, err) = env.cmd(&["install", pkg.to_str().unwrap(), "--dry-run"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("tilo-runtime/2025.06 MISSING"), "{out}");
    assert!(out.contains("--with-runtime"), "{out}");
}

/// Installing an older version over a newer one is nearly always a
/// mis-resolve, so it is refused unless asked for.
#[test]
fn update_refuses_a_downgrade_unless_told_to() {
    let env = Env::new();
    let v2 = env.tapp("2.0.0", b"two", None);
    assert!(env.cmd(&["install", v2.to_str().unwrap()]).0);

    let v1 = env.tapp("1.0.0", b"one", None);
    let (ok, out, err) = env.cmd(&[
        "update",
        "so.tilo.lifecycle",
        "--from",
        v1.to_str().unwrap(),
    ]);
    assert!(!ok, "{out}{err}");
    assert!(err.contains("is not newer"), "{err}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"two",
        "a refused update must not have swapped anything"
    );

    let (ok, out, err) = env.cmd(&[
        "update",
        "so.tilo.lifecycle",
        "--from",
        v1.to_str().unwrap(),
        "--allow-downgrade",
    ]);
    assert!(ok, "{out}{err}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"one"
    );
}

#[test]
fn update_with_the_same_version_is_a_no_op() {
    let env = Env::new();
    let v1 = env.tapp("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);
    let (ok, out, err) = env.cmd(&[
        "update",
        "so.tilo.lifecycle",
        "--from",
        v1.to_str().unwrap(),
    ]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("is already current"), "{out}");
}

/// A `.tapp` for a different package must not be swapped into this
/// one's slot, however well signed it is.
#[test]
fn update_refuses_a_tapp_for_a_different_package() {
    let env = Env::new();
    let v1 = env.tapp("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);

    // Same signing key, different package id.
    let build = env.home.path().join("other");
    let payload = build.join("payload");
    write_payload(&payload, b"other app");
    let src =
        manifest_src("9.0.0", &env.pub_hex, None).replace("so.tilo.lifecycle", "so.tilo.impostor");
    let manifest = Manifest::from_toml_str(&src).unwrap();
    let other = build.join("other.tapp");
    build_tapp(&other, &manifest, &env.sk, &payload);

    let (ok, out, err) = env.cmd(&[
        "update",
        "so.tilo.lifecycle",
        "--from",
        other.to_str().unwrap(),
    ]);
    assert!(!ok, "{out}{err}");
    assert!(err.contains("so.tilo.impostor"), "{err}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"one"
    );
}

/// A local tapp does not have a registry owner's signed publisher
/// authorization. Matching the package id is therefore insufficient: an
/// unrelated key must not silently take over the installed app's update slot.
#[test]
fn local_update_requires_explicit_authority_for_a_publisher_change() {
    let env = Env::new();
    let v1 = env.tapp("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);

    let (replacement_key, replacement_pub_hex) = fresh_key();
    let build = env.home.path().join("replacement-publisher");
    let payload = build.join("payload");
    write_payload(&payload, b"new publisher");
    let manifest =
        Manifest::from_toml_str(&manifest_src("2.0.0", &replacement_pub_hex, None)).unwrap();
    let replacement = build.join("replacement.tapp");
    build_tapp(&replacement, &manifest, &replacement_key, &payload);

    let (ok, out, err) = env.cmd(&[
        "update",
        "so.tilo.lifecycle",
        "--from",
        replacement.to_str().unwrap(),
    ]);
    assert!(!ok, "publisher takeover must be refused: {out}{err}");
    assert!(err.contains("changed publisher"), "{err}");
    assert!(err.contains("--require-publisher"), "{err}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"one",
        "a refused publisher change must preserve the installed payload"
    );
    assert!(env.registry_toml().contains(&env.pub_hex));

    let (ok, out, err) = env.cmd(&[
        "update",
        "so.tilo.lifecycle",
        "--from",
        replacement.to_str().unwrap(),
        "--require-publisher",
        &replacement_pub_hex,
    ]);
    assert!(ok, "explicit publisher rotation failed: {out}{err}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"new publisher"
    );
    assert!(env.registry_toml().contains(&replacement_pub_hex));
}

/// `info` is the alias people reach for; it must show the things the
/// other commands act on — the dependency, the evidence, the data.
#[test]
fn info_reports_the_runtime_the_integrity_record_and_the_saved_data() {
    let env = Env::new();
    let v1 = env.tapp("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);
    env.seed_state();

    let (ok, out, err) = env.cmd(&["info", "so.tilo.lifecycle"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("runtime       : (none declared)"), "{out}");
    assert!(out.contains("integrity     : recorded"), "{out}");
    assert!(out.contains("saved data    : 2 room(s)"), "{out}");
    assert!(out.contains("personal"), "{out}");
}
