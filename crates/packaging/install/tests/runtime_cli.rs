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
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tilo_test_env::SpawnUnderEnvLock as _;

fn install_bin() -> PathBuf {
    // Cargo compiles the binary before running integration tests and
    // exposes its path via CARGO_BIN_EXE_<name>.
    tilo_test_env::package_binary!("tilo-install")
}

/// The CLI's output for `args`, against a bundle store of its own under
/// `xdg`.
fn cli(xdg: &Path, args: &[&str]) -> std::process::Output {
    Command::new(install_bin())
        .args(args)
        .env("XDG_DATA_HOME", xdg)
        .env("HOME", xdg)
        .output_locked()
        .unwrap()
}

/// The CLI's output for `args`, producing the bundle from `host`.
fn cli_from(xdg: &Path, host: &Path, args: &[&str]) -> std::process::Output {
    Command::new(install_bin())
        .args(args)
        .arg(host)
        .env("XDG_DATA_HOME", xdg)
        .env("HOME", xdg)
        .output_locked()
        .unwrap()
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
        let out = cli_from(
            xdg.path(),
            host.path(),
            &["runtime", "install", "--version", "v", "--from"],
        );
        assert!(out.status.success());

        // Second install without --force: fails.
        let out = cli_from(
            xdg.path(),
            host.path(),
            &["runtime", "install", "--version", "v", "--from"],
        );
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("already installed"), "stderr: {stderr}");

        // With --force: succeeds.
        let out = cli_from(
            xdg.path(),
            host.path(),
            &["runtime", "install", "--version", "v", "--force", "--from"],
        );
        assert!(out.status.success());
    }
    // Phase `runtime_install_list_verify_remove_roundtrip`.
    {
        let xdg = tempfile::tempdir().unwrap();
        let host = tempfile::tempdir().unwrap();
        fake_host(host.path());

        // install
        let out = cli_from(
            xdg.path(),
            host.path(),
            &["runtime", "install", "--version", "test-1.0.0", "--from"],
        );
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
        let out = cli(xdg.path(), &["runtime", "list"]);
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("test-1.0.0"), "list output: {stdout}");

        // verify
        let out = cli(xdg.path(), &["runtime", "verify", "test-1.0.0"]);
        assert!(
            out.status.success(),
            "verify failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // path
        let out = cli(xdg.path(), &["runtime", "path", "--version", "test-1.0.0"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.trim().ends_with("tilo-runtime/test-1.0.0"));

        // remove
        let out = cli(xdg.path(), &["runtime", "remove", "test-1.0.0"]);
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

        let out = cli_from(
            xdg.path(),
            host.path(),
            &["runtime", "install", "--version", "vv", "--from"],
        );
        assert!(out.status.success());

        let libc = xdg.path().join("tilo-runtime/vv/lib/lib64/libc.so.6");
        let mut bytes = fs::read(&libc).unwrap();
        bytes[0] ^= 0x5a;
        fs::write(&libc, bytes).unwrap();

        let out = cli(xdg.path(), &["runtime", "verify", "vv"]);
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("hash mismatch") || stderr.contains("mismatch"),
            "stderr: {stderr}"
        );
    }
}
