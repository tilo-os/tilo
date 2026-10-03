//! Integration tests exercising `tilo-install runtime …` via the built
//! CLI binary.
//!
//! Each test uses an isolated `XDG_DATA_HOME` bundle store, with no side
//! effects on the user's
//! `$XDG_DATA_HOME/tilo-runtime/`.
//!
//! `--from` points the bundle producer at a fixture host root we
//! populate with stub loader / libc files so the tests don't depend on
//! the runner's actual `/lib64` layout.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn install_bin() -> PathBuf {
    // Cargo compiles the binary before running integration tests and
    // exposes its path via CARGO_BIN_EXE_<name>.
    PathBuf::from(env!("CARGO_BIN_EXE_tilo-install"))
}

fn fake_host(root: &Path) {
    for rel in [
        "lib64/ld-linux-x86-64.so.2",
        "lib64/libc.so.6",
        "lib64/libm.so.6",
        "lib64/libdl.so.2",
    ] {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, format!("stub: {rel}\n").as_bytes()).unwrap();
    }
    fs::create_dir_all(root.join("etc")).unwrap();
    fs::write(root.join("etc/ld.so.cache"), b"stub-cache").unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn runtime_bundles_install_verify_and_refuse_overwrites_or_tampering() {
    // Phase `runtime_install_refuses_overwrite_without_force`.
    {
        let xdg = tempfile::tempdir().unwrap();
        let host = tempfile::tempdir().unwrap();
        fake_host(host.path());

        // First install: succeeds.
        let out = Command::new(install_bin())
            .args(["runtime", "install", "--version", "v", "--from"])
            .arg(host.path())
            .env("XDG_DATA_HOME", xdg.path())
            .env("HOME", xdg.path())
            .output()
            .unwrap();
        assert!(out.status.success());

        // Second install without --force: fails.
        let out = Command::new(install_bin())
            .args(["runtime", "install", "--version", "v", "--from"])
            .arg(host.path())
            .env("XDG_DATA_HOME", xdg.path())
            .env("HOME", xdg.path())
            .output()
            .unwrap();
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("already installed"), "stderr: {stderr}");

        // With --force: succeeds.
        let out = Command::new(install_bin())
            .args(["runtime", "install", "--version", "v", "--force", "--from"])
            .arg(host.path())
            .env("XDG_DATA_HOME", xdg.path())
            .env("HOME", xdg.path())
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    // Phase `runtime_install_list_verify_remove_roundtrip`.
    {
        let xdg = tempfile::tempdir().unwrap();
        let host = tempfile::tempdir().unwrap();
        fake_host(host.path());

        // install
        let out = Command::new(install_bin())
            .args(["runtime", "install", "--version", "test-1.0.0", "--from"])
            .arg(host.path())
            .env("XDG_DATA_HOME", xdg.path())
            .env("HOME", xdg.path())
            .output()
            .expect("run tilo-install");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "runtime install failed: {stderr}\nstdout: {stdout}"
        );
        assert!(stdout.contains("test-1.0.0"));
        assert!(stdout.contains("root-hash: blake3:"));
        assert!(
            xdg.path()
                .join("tilo-runtime/test-1.0.0/bundle.toml")
                .exists()
        );

        // list
        let out = Command::new(install_bin())
            .args(["runtime", "list"])
            .env("XDG_DATA_HOME", xdg.path())
            .env("HOME", xdg.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("test-1.0.0"), "list output: {stdout}");

        // verify
        let out = Command::new(install_bin())
            .args(["runtime", "verify", "test-1.0.0"])
            .env("XDG_DATA_HOME", xdg.path())
            .env("HOME", xdg.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "verify failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // path
        let out = Command::new(install_bin())
            .args(["runtime", "path", "--version", "test-1.0.0"])
            .env("XDG_DATA_HOME", xdg.path())
            .env("HOME", xdg.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.trim().ends_with("tilo-runtime/test-1.0.0"));

        // remove
        let out = Command::new(install_bin())
            .args(["runtime", "remove", "test-1.0.0"])
            .env("XDG_DATA_HOME", xdg.path())
            .env("HOME", xdg.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            !xdg.path()
                .join("tilo-runtime/test-1.0.0/bundle.toml")
                .exists()
        );
    }
    // Phase `runtime_verify_detects_tampering`.
    {
        let xdg = tempfile::tempdir().unwrap();
        let host = tempfile::tempdir().unwrap();
        fake_host(host.path());

        let out = Command::new(install_bin())
            .args(["runtime", "install", "--version", "vv", "--from"])
            .arg(host.path())
            .env("XDG_DATA_HOME", xdg.path())
            .env("HOME", xdg.path())
            .output()
            .unwrap();
        assert!(out.status.success());

        // Flip a byte in the installed libc stub.
        let libc = xdg.path().join("tilo-runtime/vv/lib/lib64/libc.so.6");
        let mut bytes = fs::read(&libc).unwrap();
        bytes[0] ^= 0x5a;
        fs::write(&libc, bytes).unwrap();

        let out = Command::new(install_bin())
            .args(["runtime", "verify", "vv"])
            .env("XDG_DATA_HOME", xdg.path())
            .env("HOME", xdg.path())
            .output()
            .unwrap();
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("hash mismatch") || stderr.contains("mismatch"),
            "stderr: {stderr}"
        );
    }
}
