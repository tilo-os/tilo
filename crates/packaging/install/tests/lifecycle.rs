//! The package-manager lifecycle, end to end, against the real binary.
//!
//! install → list → verify → update → uninstall, plus the two things
//! that make it a package manager rather than an unpacker: it knows
//! what it depends on (the runtime bundle), and it distinguishes the
//! publisher's files from the user's data.
//!
//! Every test drives `qos-install` as a subprocess with `XDG_DATA_HOME`
//! and `XDG_CONFIG_HOME` pointed at a tempdir, so nothing here touches
//! the developer's real installs — and so what is asserted is the
//! production code path, not a library call that resembles it.

use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::SigningKey;
use pkg::blobs::{BlobHash, BlobStore};
use pkg::signed::SignedRoot;
use pkg::{Manifest, MemoryBlobStore, SigningIdentity, pack};
use rand::rngs::OsRng;
use serde::Serialize;

fn install_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_qos-install"))
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
id      = "dev.qos.lifecycle"
version = "{version}"
kind    = "app"
{runtime_line}
[publisher]
identity  = "ed25519:{pub_hex}"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp    = "qos-default"
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

/// Build a `.qpkg` with the layout `qos-pkg pack` writes.
fn build_qpkg(qpkg_path: &Path, manifest: &Manifest, sk: &SigningKey, payload_dir: &Path) {
    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(sk.clone());
    let id = pack(payload_dir, manifest, &identity, &mut store).expect("pack");

    #[derive(Serialize)]
    struct QpkgHeader {
        package_id: String,
        format: u32,
    }
    let header_toml = toml::to_string(&QpkgHeader {
        package_id: id.to_string(),
        format: 1,
    })
    .unwrap();

    let signed_bytes = store.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
    let signed = SignedRoot::decode(&signed_bytes).unwrap();
    let manifest_bytes = store.get(&signed.manifest_hash).unwrap().unwrap();
    let collection_bytes = store.get(&signed.payload_root_hash).unwrap().unwrap();

    let file = std::fs::File::create(qpkg_path).unwrap();
    let mut tar = tar::Builder::new(file);
    append(&mut tar, "qpkg.toml", header_toml.as_bytes());
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
    /// What `qos-install` is told the *system* store is. Empty by
    /// default, and never unset: production reads
    /// `/usr/share/qos-apps`, so a test that left this alone would see
    /// the whole preinstalled fleet when run inside a qOS image and
    /// nothing at all on a dev box — the same assertions passing and
    /// failing for reasons that have nothing to do with the test.
    system: PathBuf,
}

impl Env {
    fn new() -> Self {
        let (sk, pub_hex) = fresh_key();
        let home = tempfile::tempdir().unwrap();
        let system = home.path().join("empty-system-store");
        std::fs::create_dir_all(&system).unwrap();
        Self {
            home,
            sk,
            pub_hex,
            system,
        }
    }

    fn xdg_data(&self) -> PathBuf {
        self.home.path().join("data")
    }

    fn qos_apps(&self) -> PathBuf {
        self.xdg_data().join("qos-apps")
    }

    fn payload_root(&self) -> PathBuf {
        self.qos_apps().join("payloads/dev.qos.lifecycle")
    }

    fn registry_toml(&self) -> String {
        std::fs::read_to_string(self.qos_apps().join("registry.toml")).unwrap_or_default()
    }

    /// The read-only store's registry, as it ships in the image.
    fn system_registry(&self) -> PathBuf {
        self.system.join("registry.toml")
    }

    /// Pack a `.qpkg` at `version` whose entrypoint holds `body`.
    fn qpkg(&self, version: &str, body: &[u8], runtime: Option<&str>) -> PathBuf {
        let build = self.home.path().join(format!("build-{version}"));
        let payload = build.join("payload");
        write_payload(&payload, body);
        let manifest =
            Manifest::from_toml_str(&manifest_src(version, &self.pub_hex, runtime)).unwrap();
        let path = build.join("pkg.qpkg");
        build_qpkg(&path, &manifest, &self.sk, &payload);
        path
    }

    fn cmd(&self, args: &[&str]) -> (bool, String, String) {
        let mut c = Command::new(install_bin());
        c.env_clear();
        c.env("PATH", std::env::var("PATH").unwrap_or_default());
        c.env("XDG_CONFIG_HOME", self.home.path().join("config"));
        c.env("XDG_DATA_HOME", self.xdg_data());
        c.env("HOME", self.home.path());
        c.env("QOS_APPS_SYSTEM_DIR", &self.system);
        c.args(args);
        let out = c.output().expect("spawn qos-install");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// Install into the *system* store the way the image build does:
    /// the ordinary installer, with `QOS_APPS_DATA_DIR` pointed at the
    /// directory that ships read-only inside the image. Deliberately
    /// the same binary and the same code path — a fixture that hand-wrote
    /// a registry.toml would prove the reader parses a file this project
    /// happens to write, not that the two halves agree.
    fn install_into_system(&self, qpkg: &Path) -> (bool, String, String) {
        let mut c = Command::new(install_bin());
        c.env_clear();
        c.env("PATH", std::env::var("PATH").unwrap_or_default());
        c.env("XDG_CONFIG_HOME", self.home.path().join("config"));
        c.env("HOME", self.home.path());
        c.env("QOS_APPS_DATA_DIR", &self.system);
        c.args(["install", qpkg.to_str().unwrap()]);
        let out = c.output().expect("spawn qos-install");
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
                .join("qos-rooms")
                .join(room)
                .join("dev.qos.lifecycle");
            std::fs::create_dir_all(dir.join(".local/share")).unwrap();
            std::fs::write(dir.join(".local/share/notes.bin"), b"the user's work").unwrap();
        }
    }

    fn state_exists(&self, room: &str) -> bool {
        self.xdg_data()
            .join("qos-rooms")
            .join(room)
            .join("dev.qos.lifecycle")
            .exists()
    }
}

#[test]
fn install_list_verify_update_uninstall() {
    let env = Env::new();
    let v1 = env.qpkg("1.0.0", b"version one", None);

    // --- install -----------------------------------------------------
    let (ok, out, err) = env.cmd(&["install", v1.to_str().unwrap()]);
    assert!(ok, "install failed: {out}{err}");
    assert!(out.contains("installed: dev.qos.lifecycle 1.0.0"), "{out}");
    assert!(env.payload_root().join("bin/app").exists());
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"version one"
    );
    assert!(env.registry_toml().contains("version = \"1.0.0\""), "{}", env.registry_toml());

    // --- list --------------------------------------------------------
    let (ok, out, _) = env.cmd(&["list"]);
    assert!(ok);
    assert!(out.contains("dev.qos.lifecycle"), "{out}");
    assert!(out.contains("1.0.0"), "{out}");

    // --- verify ------------------------------------------------------
    let (ok, out, err) = env.cmd(&["verify"]);
    assert!(ok, "verify failed: {out}{err}");
    assert!(out.contains("ok       dev.qos.lifecycle 1.0.0"), "{out}");

    // The app saves something. From here on the payload and the state
    // have to be treated differently.
    env.seed_state();

    // --- update ------------------------------------------------------
    let v2 = env.qpkg("1.1.0", b"version two", None);
    let (ok, out, err) = env.cmd(&["update", "dev.qos.lifecycle", "--from", v2.to_str().unwrap()]);
    assert!(ok, "update failed: {out}{err}");
    assert!(out.contains("updated: dev.qos.lifecycle 1.0.0 -> 1.1.0"), "{out}");
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
    let (ok, out, err) = env.cmd(&["verify", "dev.qos.lifecycle"]);
    assert!(ok, "verify after update failed: {out}{err}");
    assert!(out.contains("ok       dev.qos.lifecycle 1.1.0"), "{out}");

    // --- uninstall ---------------------------------------------------
    let (ok, out, err) = env.cmd(&["uninstall", "dev.qos.lifecycle", "--keep-state"]);
    assert!(ok, "uninstall failed: {out}{err}");
    assert!(out.contains("uninstalled: dev.qos.lifecycle 1.1.0"), "{out}");
    assert!(!env.payload_root().exists(), "the payload must be gone");
    assert!(
        !env.registry_toml().contains("dev.qos.lifecycle"),
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
    let v1 = env.qpkg("1.0.0", b"one", None);
    let (ok, _, err) = env.cmd(&["install", v1.to_str().unwrap()]);
    assert!(ok, "{err}");
    env.seed_state();

    let (ok, out, err) = env.cmd(&["uninstall", "dev.qos.lifecycle", "--purge"]);
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
    let v1 = env.qpkg("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);
    env.seed_state();

    // No --purge, no --keep-state, and stdin is not a terminal.
    let (ok, out, err) = env.cmd(&["uninstall", "dev.qos.lifecycle"]);
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
    let v1 = env.qpkg("1.0.0", b"one", None);

    let (ok, out, err) = env.cmd(&["install", v1.to_str().unwrap(), "--dry-run"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("plan: install dev.qos.lifecycle 1.0.0"), "{out}");
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
    let v2 = env.qpkg("2.0.0", b"two", None);
    let (ok, out, _) = env.cmd(&[
        "update",
        "dev.qos.lifecycle",
        "--from",
        v2.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(ok);
    assert!(out.contains("plan: update dev.qos.lifecycle 1.0.0 -> 2.0.0"), "{out}");
    assert!(out.contains("state     : KEPT"), "{out}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"one",
        "--dry-run must not swap the payload"
    );

    let (ok, out, _) = env.cmd(&["uninstall", "dev.qos.lifecycle", "--dry-run"]);
    assert!(ok);
    assert!(out.contains("plan: uninstall dev.qos.lifecycle 1.0.0"), "{out}");
    assert!(out.contains("saved data: 2 room(s)"), "{out}");
    assert!(out.contains("(nothing was removed)"), "{out}");
    assert!(env.payload_root().exists());
}

/// A tampered payload has to be caught by the thing whose job is to
/// catch it, and named precisely enough to act on.
#[test]
fn verify_catches_a_modified_payload() {
    let env = Env::new();
    let v1 = env.qpkg("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);
    assert!(env.cmd(&["verify"]).0);

    std::fs::write(env.payload_root().join("bin/app"), b"malicious").unwrap();

    let (ok, out, err) = env.cmd(&["verify", "dev.qos.lifecycle"]);
    assert!(!ok, "a tampered payload must fail verification: {out}{err}");
    assert!(out.contains("FAILED   dev.qos.lifecycle"), "{out}");
    assert!(out.contains("modified: bin/app"), "{out}");
}

/// Refusing an install whose runtime bundle is absent is what keeps
/// "installed" and "launchable" the same thing. The message has to
/// name the command that fixes it.
#[test]
fn a_missing_runtime_bundle_refuses_the_install_and_says_how_to_fix_it() {
    let env = Env::new();
    let pkg = env.qpkg("1.0.0", b"one", Some("qos-runtime/2025.06"));

    let (ok, out, err) = env.cmd(&["install", pkg.to_str().unwrap()]);
    assert!(!ok, "install must refuse: {out}{err}");
    assert!(err.contains("declares runtime `qos-runtime/2025.06`"), "{err}");
    assert!(
        err.contains("qos-install runtime install --version 2025.06"),
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
    assert!(out.contains("qos-runtime/2025.06 (MISSING)"), "{out}");
}

/// The dry run has to report the dependency too, or the plan is not a
/// plan.
#[test]
fn dry_run_names_a_missing_runtime_dependency() {
    let env = Env::new();
    let pkg = env.qpkg("1.0.0", b"one", Some("qos-runtime/2025.06"));
    let (ok, out, err) = env.cmd(&["install", pkg.to_str().unwrap(), "--dry-run"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("qos-runtime/2025.06 MISSING"), "{out}");
    assert!(out.contains("--with-runtime"), "{out}");
}

/// Installing an older version over a newer one is nearly always a
/// mis-resolve, so it is refused unless asked for.
#[test]
fn update_refuses_a_downgrade_unless_told_to() {
    let env = Env::new();
    let v2 = env.qpkg("2.0.0", b"two", None);
    assert!(env.cmd(&["install", v2.to_str().unwrap()]).0);

    let v1 = env.qpkg("1.0.0", b"one", None);
    let (ok, out, err) = env.cmd(&["update", "dev.qos.lifecycle", "--from", v1.to_str().unwrap()]);
    assert!(!ok, "{out}{err}");
    assert!(err.contains("is not newer"), "{err}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"two",
        "a refused update must not have swapped anything"
    );

    let (ok, out, err) = env.cmd(&[
        "update",
        "dev.qos.lifecycle",
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
    let v1 = env.qpkg("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);
    let (ok, out, err) = env.cmd(&["update", "dev.qos.lifecycle", "--from", v1.to_str().unwrap()]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("is already current"), "{out}");
}

/// A `.qpkg` for a different package must not be swapped into this
/// one's slot, however well signed it is.
#[test]
fn update_refuses_a_qpkg_for_a_different_package() {
    let env = Env::new();
    let v1 = env.qpkg("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);

    // Same signing key, different package id.
    let build = env.home.path().join("other");
    let payload = build.join("payload");
    write_payload(&payload, b"other app");
    let src = manifest_src("9.0.0", &env.pub_hex, None)
        .replace("dev.qos.lifecycle", "dev.qos.impostor");
    let manifest = Manifest::from_toml_str(&src).unwrap();
    let other = build.join("other.qpkg");
    build_qpkg(&other, &manifest, &env.sk, &payload);

    let (ok, out, err) = env.cmd(&[
        "update",
        "dev.qos.lifecycle",
        "--from",
        other.to_str().unwrap(),
    ]);
    assert!(!ok, "{out}{err}");
    assert!(err.contains("dev.qos.impostor"), "{err}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"one"
    );
}

/// `info` is the alias people reach for; it must show the things the
/// other commands act on — the dependency, the evidence, the data.
#[test]
fn info_reports_the_runtime_the_integrity_record_and_the_saved_data() {
    let env = Env::new();
    let v1 = env.qpkg("1.0.0", b"one", None);
    assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);
    env.seed_state();

    let (ok, out, err) = env.cmd(&["info", "dev.qos.lifecycle"]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("runtime       : (none declared)"), "{out}");
    assert!(out.contains("integrity     : recorded"), "{out}");
    assert!(out.contains("saved data    : 2 room(s)"), "{out}");
    assert!(out.contains("personal"), "{out}");
}

/// A machine whose apps are all preinstalled must not report that it
/// has none.
///
/// This is the shape the qOS image actually ships: forty-five packages
/// installed into `/usr/share/qos-apps` at build time and nothing in the
/// user's own store. `qos-launcher` has always merged both stores, so it
/// listed all forty-five; `qos-install list` read the user store alone
/// and printed "(no packages installed)" on the same machine. Two
/// readers of one store disagreeing about what is installed is the bug.
#[test]
fn list_reports_the_preinstalled_system_store() {
    let env = Env::new();
    let v1 = env.qpkg("1.0.0", b"preinstalled", None);
    let (ok, out, err) = env.install_into_system(&v1);
    assert!(ok, "system-store install failed: {out}{err}");

    // Nothing in the user store at all — the fresh-boot case.
    assert!(
        !env.qos_apps().join("registry.toml").exists(),
        "the user store must be untouched for this to be the preinstalled case"
    );

    let (ok, out, err) = env.cmd(&["list"]);
    assert!(ok, "list failed: {out}{err}");
    assert!(
        out.contains("dev.qos.lifecycle"),
        "a preinstalled package must be listed: {out}"
    );
    assert!(
        !out.contains("(no packages installed)"),
        "a machine with a populated system store must not report an empty one: {out}"
    );
    assert!(
        out.contains("system"),
        "the row has to say which store it came from, because that decides \
         whether this machine can remove it: {out}"
    );

    // The same read path decides whether the app can be launched and
    // inspected, so those have to agree with the listing.
    let (ok, out, err) = env.cmd(&["info", "dev.qos.lifecycle"]);
    assert!(ok, "info on a preinstalled package failed: {out}{err}");
    assert!(out.contains("store         : system"), "{out}");
}

/// The system store is inside a read-only `/usr`. A command that would
/// have to write to it must say that, not claim the package is missing.
#[test]
fn uninstalling_a_preinstalled_package_says_why_it_cannot() {
    let env = Env::new();
    let v1 = env.qpkg("1.0.0", b"preinstalled", None);
    assert!(env.install_into_system(&v1).0);

    let (ok, out, err) = env.cmd(&["uninstall", "dev.qos.lifecycle"]);
    assert!(!ok, "uninstalling from a read-only store must fail: {out}");
    let said = format!("{out}{err}");
    assert!(
        said.contains("preinstalled"),
        "the failure has to name the reason: {said}"
    );
    assert!(
        !said.contains("no such package installed"),
        "'not installed' is the wrong answer for a package that is listed: {said}"
    );

    // And a genuinely absent id still gets the plain answer.
    let (ok, out, err) = env.cmd(&["uninstall", "dev.qos.nothing-here"]);
    assert!(!ok);
    assert!(
        format!("{out}{err}").contains("no such package installed"),
        "{out}{err}"
    );
}

/// A user's own build of a preinstalled app replaces it in the listing
/// rather than appearing beside it.
#[test]
fn a_user_install_shadows_the_preinstalled_row() {
    let env = Env::new();
    let sys = env.qpkg("1.0.0", b"preinstalled", None);
    assert!(env.install_into_system(&sys).0);

    let mine = env.qpkg("1.1.0", b"my build", None);
    let (ok, out, err) = env.cmd(&["install", mine.to_str().unwrap()]);
    assert!(ok, "{out}{err}");

    let (ok, out, _) = env.cmd(&["list"]);
    assert!(ok);
    assert_eq!(
        out.matches("dev.qos.lifecycle").count(),
        1,
        "one row per id, not one per store: {out}"
    );
    assert!(out.contains("1.1.0"), "the user's version is the one that runs: {out}");
    assert!(!out.contains("1.0.0"), "the shadowed row must not show: {out}");

    // Shadowed, so it is removable again — and removing it must not
    // damage the system store's registry.
    let sys_registry_before =
        std::fs::read_to_string(env.system_registry()).expect("system registry");
    let (ok, out, err) = env.cmd(&["uninstall", "dev.qos.lifecycle", "--keep-state"]);
    assert!(ok, "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(env.system_registry()).expect("system registry"),
        sys_registry_before,
        "uninstalling the user's copy must not rewrite the read-only store"
    );

    // With the user's copy gone, the preinstalled one is visible again.
    let (ok, out, _) = env.cmd(&["list"]);
    assert!(ok);
    assert!(out.contains("1.0.0"), "the preinstalled row comes back: {out}");
}
