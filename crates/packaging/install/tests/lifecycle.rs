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
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::{Manifest, SigningIdentity, pack};

fn install_bin() -> PathBuf {
    tilo_test_env::package_binary!("tilo-install")
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
    let mut rng = UnwrapErr(SysRng);
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    (sk, pub_hex)
}

/// Build a `.tapp` with the layout `tilo-app-package pack` writes.
fn build_tapp(tapp_path: &Path, manifest: &Manifest, sk: &SigningKey, payload_dir: &Path) {
    let identity = SigningIdentity::from_signing_key(sk.clone());
    let packed = pack(payload_dir, manifest, &identity).expect("pack");
    let file = std::fs::File::create(tapp_path).unwrap();
    tilo_app_package::tapp::write(file, &packed.meta, Some(&packed)).unwrap();
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

    /// The installed tree, as the registry records it; a path that does not
    /// exist when the package is not installed.
    fn payload_root(&self) -> PathBuf {
        package_store::Registry::load(&self.tilo_apps().join("registry.toml"))
            .ok()
            .and_then(|registry| {
                registry
                    .find("so.tilo.lifecycle")
                    .map(|row| row.payload_root.clone())
            })
            .unwrap_or_else(|| self.tilo_apps().join("pkgs/not-installed"))
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

    /// A package pinning `optional` as one of its `optional-runtimes`.
    fn tapp_pinning_optional(&self, version: &str, body: &[u8], optional: &str) -> PathBuf {
        let build = self.home.path().join(format!("build-optional-{version}"));
        let payload = build.join("payload");
        write_payload(&payload, body);
        let source = manifest_src(version, &self.pub_hex, None).replace(
            "kind    = \"app\"\n",
            &format!("kind    = \"app\"\noptional-runtimes = [\"{optional}\"]\n"),
        );
        let manifest = Manifest::from_toml_str(&source).unwrap();
        let path = build.join("pkg.tapp");
        build_tapp(&path, &manifest, &self.sk, &payload);
        path
    }

    /// A self-contained fixture with a linked release, never a runtime override.
    fn desktop_tapp(&self, body: &[u8], platform: &str, arch: &str) -> PathBuf {
        let build = self.home.path().join("desktop-build");
        let payload = build.join("payload");
        write_payload(&payload, body);
        let authored = Manifest::from_toml_str(&manifest_src(
            "0.2.0",
            &self.pub_hex,
            Some("tilo-runtime/2026.07"),
        ))
        .unwrap();
        let manifest =
            tilo_app_package::desktop::desktop_manifest(&authored, platform, arch, arch, "bin/app")
                .unwrap();
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
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn installs_list_verify_report_and_serve_thin_packages() {
    // Phase `install_list_verify_update_uninstall`.
    {
        let env = Env::new();
        let v1 = env.tapp("1.0.0", b"version one", None);

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

        let (ok, out, _) = env.cmd(&["list"]);
        assert!(ok);
        assert!(out.contains("so.tilo.lifecycle"), "{out}");
        assert!(out.contains("1.0.0"), "{out}");

        let (ok, out, err) = env.cmd(&["verify"]);
        assert!(ok, "verify failed: {out}{err}");
        assert!(out.contains("ok       so.tilo.lifecycle 1.0.0"), "{out}");

        // The app saves something. From here on the payload and the state
        // have to be treated differently.
        env.seed_state();

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

        let installed_tree = env.payload_root();
        let (ok, out, err) = env.cmd(&["uninstall", "so.tilo.lifecycle", "--keep-state"]);
        assert!(ok, "uninstall failed: {out}{err}");
        assert!(
            out.contains("uninstalled: so.tilo.lifecycle 1.1.0"),
            "{out}"
        );
        assert!(!installed_tree.exists(), "the payload must be gone");
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
    // Phase `verify_catches_a_modified_payload`.
    // A tampered payload has to be caught by the thing whose job is to
    // catch it, and named precisely enough to act on.
    {
        let env = Env::new();
        let v1 = env.tapp("1.0.0", b"one", None);
        assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);
        assert!(env.cmd(&["verify"]).0);

        // Installed files are read-only; whoever tampers makes them writable.
        let target = env.payload_root().join("bin/app");
        let mut permissions = std::fs::metadata(&target).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&target, permissions).unwrap();
        std::fs::write(&target, b"malicious").unwrap();

        let (ok, out, err) = env.cmd(&["verify", "so.tilo.lifecycle"]);
        assert!(!ok, "a tampered payload must fail verification: {out}{err}");
        assert!(out.contains("FAILED   so.tilo.lifecycle"), "{out}");
        assert!(out.contains("modified: bin/app"), "{out}");
    }
    // Phase `info_reports_the_runtime_the_integrity_record_and_the_saved_data`.
    // `info` is the alias people reach for; it must show the things the
    // other commands act on — the dependency, the evidence, the data.
    {
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
    // Phase `a_thin_package_installs_from_what_the_store_holds`.
    // A thin package is metadata only: it installs when the content store
    // already holds its payload, and is refused when nothing can supply it.
    {
        let env = Env::new();
        let full = env.tapp("1.0.0", b"one", None);
        let thin = thin_of(&full);
        assert!(std::fs::metadata(&thin).unwrap().len() < 16 * 1024);

        let (ok, out, err) = env.cmd(&["install", thin.to_str().unwrap()]);
        assert!(
            !ok,
            "a thin package with no payload source must not install: {out}"
        );
        assert!(err.contains("payload blob"), "{err}");
        assert!(
            !env.payload_root().exists(),
            "a refused install leaves nothing behind"
        );

        assert!(env.cmd(&["install", full.to_str().unwrap()]).0);
        let (ok, out, err) = env.cmd(&["install", thin.to_str().unwrap(), "--force"]);
        assert!(ok, "{out}{err}");
        assert!(out.contains("already in the content store"), "{out}");
        let (ok, out, err) = env.cmd(&["verify", "so.tilo.lifecycle"]);
        assert!(ok, "{out}{err}");
    }
}

#[test]
fn uninstall_keeps_state_unless_purged() {
    // Phase `purge_removes_the_state_that_keep_state_would_have_left`.
    // Deleting someone's notes is not undoable, so it takes an explicit
    // instruction — and the uninstall says how much it is about to
    // delete before it does.
    {
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
    // Phase `a_non_interactive_uninstall_keeps_state_by_default`.
    // A non-interactive uninstall must not silently delete user data —
    // there is nobody to ask, so the answer is keep.
    {
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
}

#[test]
fn dry_runs_and_runtime_dependencies_are_reported_before_anything_changes() {
    // Phase `dry_run_reports_the_change_without_making_it`.
    {
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
    // Phase `a_missing_runtime_bundle_refuses_the_install_and_says_how_to_fix_it`.
    // Refusing an install whose runtime bundle is absent is what keeps
    // "installed" and "launchable" the same thing. The message has to
    // name the command that fixes it.
    {
        let env = Env::new();
        let pkg = env.tapp("1.0.0", b"one", Some("tilo-runtime/2025.06"));

        let (ok, out, err) = env.cmd(&["install", pkg.to_str().unwrap()]);
        assert!(!ok, "install must refuse: {out}{err}");
        assert!(
            err.contains("needs runtime `tilo-runtime/2025.06`"),
            "{err}"
        );
        assert!(
            err.contains("app install so.tilo.lifecycle"),
            "the refusal must name the command that fixes it: {err}"
        );
        assert!(err.contains("--ignore-missing-runtime"), "{err}");
        assert!(
            !env.payload_root().exists(),
            "a refused install must leave nothing behind"
        );

        // The escape hatch installs it, and says the package cannot launch.
        let (ok, out, err) =
            env.cmd(&["install", pkg.to_str().unwrap(), "--ignore-missing-runtime"]);
        assert!(ok, "{out}{err}");
        assert!(err.contains("will not launch until it is"), "{err}");

        // And `list` keeps saying so, rather than showing it as ready.
        let (_, out, _) = env.cmd(&["list"]);
        assert!(out.contains("tilo-runtime/2025.06 (MISSING)"), "{out}");
    }
    // Phase `dry_run_names_a_missing_runtime_dependency`.
    // The dry run has to report the dependency too, or the plan is not a
    // plan.
    {
        let env = Env::new();
        let pkg = env.tapp("1.0.0", b"one", Some("tilo-runtime/2025.06"));
        let (ok, out, err) = env.cmd(&["install", pkg.to_str().unwrap(), "--dry-run"]);
        assert!(ok, "{out}{err}");
        assert!(out.contains("tilo-runtime/2025.06 MISSING"), "{out}");
        assert!(out.contains("--ignore-missing-runtime"), "{out}");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn updates_refuse_downgrades_foreign_packages_and_unapproved_publishers() {
    // Phase `update_refuses_a_downgrade_unless_told_to`.
    // Installing an older version over a newer one is nearly always a
    // mis-resolve, so it is refused unless asked for.
    {
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
    // Phase `update_with_the_same_version_is_a_no_op`.
    {
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
    // Phase `update_refuses_a_tapp_for_a_different_package`.
    // A `.tapp` for a different package must not be swapped into this
    // one's slot, however well signed it is.
    {
        let env = Env::new();
        let v1 = env.tapp("1.0.0", b"one", None);
        assert!(env.cmd(&["install", v1.to_str().unwrap()]).0);

        let build = env.home.path().join("other");
        let payload = build.join("payload");
        write_payload(&payload, b"other app");
        let src = manifest_src("9.0.0", &env.pub_hex, None)
            .replace("so.tilo.lifecycle", "so.tilo.impostor");
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
    // Phase `local_update_requires_explicit_authority_for_a_publisher_change`.
    // A local tapp does not have a registry owner's signed publisher
    // authorization. Matching the package id is therefore insufficient: an
    // unrelated key must not silently take over the installed app's update slot.
    {
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
}

/// The thin form of the full package at `full`: metadata only.
fn thin_of(full: &Path) -> PathBuf {
    let file = std::fs::File::open(full).unwrap();
    let tapp = tilo_app_package::tapp::read(file, &mut tilo_app_package::DiscardBlobs).unwrap();
    let thin = full.with_extension("thin.tapp");
    let out = std::fs::File::create(&thin).unwrap();
    tilo_app_package::tapp::write(out, &tapp.meta, None).unwrap();
    thin
}

#[test]
fn desktop_contract_installs_above_thin_limit_without_shared_runtime() {
    let env = Env::new();
    let pkg = env.desktop_tapp(
        &vec![0; 3 * 1024 * 1024],
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    let (ok, out, err) = env.cmd(&[
        "install",
        pkg.to_str().unwrap(),
        "--require-publisher",
        &env.pub_hex,
    ]);
    assert!(ok, "{out}{err}");
    assert_eq!(
        std::fs::metadata(env.payload_root().join("bin/app"))
            .unwrap()
            .len(),
        3 * 1024 * 1024
    );
    assert!(!env.xdg_data().join("tilo-runtime").exists());
}

#[test]
fn desktop_contract_refuses_foreign_platform_and_architecture() {
    for (platform, arch) in [
        (
            if std::env::consts::OS == "linux" {
                "windows"
            } else {
                "linux"
            },
            std::env::consts::ARCH,
        ),
        (
            std::env::consts::OS,
            if std::env::consts::ARCH == "x86_64" {
                "aarch64"
            } else {
                "x86_64"
            },
        ),
    ] {
        let env = Env::new();
        let pkg = env.desktop_tapp(b"foreign", platform, arch);
        let (ok, out, err) = env.cmd(&[
            "install",
            pkg.to_str().unwrap(),
            "--require-publisher",
            &env.pub_hex,
        ]);
        assert!(!ok, "{out}");
        assert!(err.contains("cannot run on"), "{err}");
        assert!(env.registry_toml().is_empty());
    }
}

#[test]
fn desktop_contract_keeps_both_archive_size_limits() {
    for desktop in [false, true] {
        let env = Env::new();
        let limit = if desktop {
            tilo_app_package::MAX_DESKTOP_APP_TAPP_BYTES
        } else {
            tilo_app_package::MAX_APP_TAPP_BYTES
        };
        // Payload is exactly at its ceiling, but the signed tar adds metadata:
        // the production installer must reject that oversized wire archive.
        let body = vec![0; usize::try_from(limit).unwrap() - b"payload data".len()];
        let pkg = if desktop {
            env.desktop_tapp(&body, std::env::consts::OS, std::env::consts::ARCH)
        } else {
            env.tapp("0.2.0", &body, None)
        };
        let (ok, out, err) = env.cmd(&[
            "install",
            pkg.to_str().unwrap(),
            "--require-publisher",
            &env.pub_hex,
        ]);
        assert!(!ok, "{out}");
        assert!(err.contains(&format!("at most {limit} bytes")), "{err}");
        assert!(env.registry_toml().is_empty());
    }
}

#[test]
fn cohort_selection_preserves_user_holds_pins_and_state_through_the_real_installer() {
    let env = Env::new();
    let first = env.tapp("0.1.73018", b"public selected app", None);
    let next = env.tapp("0.1.0", b"paired current cohort app", None);
    let (ok, out, err) = env.cmd(&["install", "--pinned", first.to_str().unwrap()]);
    assert!(ok, "{out}{err}");
    let store = package_store::PackageStore::at(env.tilo_apps());
    store
        .update_row("so.tilo.lifecycle", |row| row.held = true)
        .unwrap();
    let held = store
        .registry()
        .unwrap()
        .find("so.tilo.lifecycle")
        .unwrap()
        .clone();
    let before = env.registry_toml();
    let (ok, out, err) = env.cmd(&[
        "install",
        "--force",
        "--preserve-held",
        next.to_str().unwrap(),
    ]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("\"preserved\":true"), "{out}");
    assert!(out.contains(&held.package_id_hex), "{out}");
    assert_eq!(env.registry_toml(), before);
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"public selected app"
    );
    store
        .update_row("so.tilo.lifecycle", |row| row.held = false)
        .unwrap();
    let (ok, out, err) = env.cmd(&[
        "install",
        "--force",
        "--preserve-held",
        next.to_str().unwrap(),
    ]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("\"preserved\":false"), "{out}");
    let selected = store
        .registry()
        .unwrap()
        .find("so.tilo.lifecycle")
        .unwrap()
        .clone();
    assert!(selected.pinned);
    assert!(!selected.held);
    assert_ne!(selected.package_id_hex, held.package_id_hex);
    assert_eq!(selected.version, "0.1.0");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"paired current cohort app"
    );
    assert!(out.contains(&selected.package_id_hex), "{out}");
    let before = env.registry_toml();
    let wrong = "00".repeat(32);
    let (ok, out, _) = env.cmd(&[
        "install",
        "--force",
        "--preserve-held",
        "--require-publisher",
        &wrong,
        first.to_str().unwrap(),
    ]);
    assert!(!ok);
    assert!(!out.contains("cohort-selection:"));
    assert_eq!(env.registry_toml(), before);
}

/// An app that pins an optional runtime by exact version (the Browser's
/// engine) is installed and updated together with it, on every path that
/// reaches the installer: replacing the app while its engine is absent would
/// leave it saying "not installed".
#[test]
fn an_app_is_not_installed_or_updated_without_the_optional_runtime_it_pins() {
    let env = Env::new();
    let engine = "tilo-runtime/2026.10.9-chromium";

    // Install: refused, naming the fix, leaving nothing behind.
    let v1 = env.tapp_pinning_optional("1.0.0", b"one", engine);
    let (ok, out, err) = env.cmd(&["install", v1.to_str().unwrap()]);
    assert!(!ok, "install must refuse: {out}{err}");
    assert!(err.contains(&format!("pins runtime `{engine}`")), "{err}");
    assert!(err.contains("app install so.tilo.lifecycle"), "{err}");
    assert!(!env.payload_root().exists());

    // The explicit escape hatch still installs it (a developer's own tree).
    let (ok, out, err) = env.cmd(&["install", v1.to_str().unwrap(), "--ignore-missing-runtime"]);
    assert!(ok, "{out}{err}");

    // Update: a build pinning an engine this device lacks does not replace
    // the working one.
    let v2 = env.tapp_pinning_optional("2.0.0", b"two", "tilo-runtime/2026.10.23-chromium");
    let (ok, out, err) = env.cmd(&[
        "update",
        "so.tilo.lifecycle",
        "--from",
        v2.to_str().unwrap(),
    ]);
    assert!(!ok, "update must refuse: {out}{err}");
    assert!(err.contains("2026.10.23-chromium"), "{err}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"one",
        "the working build stays"
    );

    // With the engine installed the same update goes through.
    let bundle = env
        .xdg_data()
        .join("tilo-runtime")
        .join("2026.10.23-chromium");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("bundle.toml"), b"").unwrap();
    let (ok, out, err) = env.cmd(&[
        "update",
        "so.tilo.lifecycle",
        "--from",
        v2.to_str().unwrap(),
    ]);
    assert!(ok, "{out}{err}");
    assert_eq!(
        std::fs::read(env.payload_root().join("bin/app")).unwrap(),
        b"two"
    );
}

#[test]
fn selected_runtime_inventory_uses_the_real_catalog_and_xdg_bundle_path() {
    let env = Env::new();
    let archive = env.tapp(
        "1.0.0",
        b"actual selected payload",
        Some("tilo-runtime/2026.07"),
    );
    let (ok, _, error) = env.cmd(&[
        "install",
        "--allow-self-signed",
        "--ignore-missing-runtime",
        archive.to_str().unwrap(),
    ]);
    assert!(ok, "{error}");
    let host = env.home.path().join("fixture-host");
    std::fs::create_dir_all(&host).unwrap();
    let library = env.home.path().join("library.so");
    std::fs::write(&library, b"selected runtime bytes").unwrap();
    let identity = env.home.path().join("identity.json");
    std::fs::write(
        &identity,
        serde_json::json!({"secret_key_hex":"11".repeat(32)}).to_string(),
    )
    .unwrap();
    let runtime = env.xdg_data().join("tilo-runtime/2026.07");
    runtime_bundle::build_bundle_with(
        &runtime,
        "2026.07",
        &runtime_bundle::BuildOptions {
            from: Some(&host),
            identity: Some(&identity),
            extra_libs: &[library],
            ..Default::default()
        },
    )
    .unwrap();
    let before = env.registry_toml();
    let (ok, output, error) = env.cmd(&["list", "--json", "--runtime-paths"]);
    assert!(ok, "{error}");
    let snapshot: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(snapshot["apps"][0]["id"], "so.tilo.lifecycle");
    assert_eq!(snapshot["apps"][0]["runtime"], "tilo-runtime/2026.07");
    assert_eq!(
        snapshot["runtime_paths"]["2026.07"],
        runtime.to_str().unwrap()
    );
    assert!(
        !runtime.starts_with(env.home.path().join(".local/share")),
        "must honor actual XDG_DATA_HOME"
    );
    let (ok, output, error) = env.cmd(&["list", "--json"]);
    assert!(ok, "{error}");
    assert!(
        serde_json::from_str::<serde_json::Value>(&output)
            .unwrap()
            .is_array()
    );
    assert!(!env.cmd(&["list", "--runtime-paths"]).0);
    assert_eq!(
        env.registry_toml(),
        before,
        "inspection never changes a selection"
    );
}
