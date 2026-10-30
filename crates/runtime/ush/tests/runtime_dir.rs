#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::process::{Command, Stdio};

use tilo_environment::{DEFAULT_RECORD, Environment, PackageId};
use ush::records::Records;
use ush::store::Store;

use crate::serve_handoff::{RUNTIME, fixture_images, guest_unavailable};

/// A guest has a runtime directory of its own, as a session on a machine
/// does: `XDG_RUNTIME_DIR` names a 0700 tmpfs at `/run/user/<uid>` that the
/// guest's user owns, where programs keep their sockets and locks.
#[test]
fn a_guest_has_a_private_runtime_directory() {
    if let Some(reason) = guest_unavailable() {
        eprintln!("runtime_dir: not exercising a guest here — {reason}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(dir.path().join("store"));
    store.ensure().unwrap();
    Records::at(store.records_root())
        .create(
            DEFAULT_RECORD,
            &Environment::new(
                "Runtime",
                "2026.10",
                tilo_environment::host_arch(),
                &PackageId::from_bytes(RUNTIME),
            ),
        )
        .unwrap();
    let images = dir.path().join("images");
    fixture_images(&images);
    let uid = rustix::process::geteuid().as_raw();
    let script = r#"bash -c 'echo "dir=$XDG_RUNTIME_DIR"; test -O "$XDG_RUNTIME_DIR" && echo owned; test -d "$XDG_RUNTIME_DIR" && test -w "$XDG_RUNTIME_DIR" && echo writable; echo x > "$XDG_RUNTIME_DIR/probe" && cat "$XDG_RUNTIME_DIR/probe"; echo "mode=$(stat -c %a "$XDG_RUNTIME_DIR") fs=$(stat -f -c %T "$XDG_RUNTIME_DIR")"'"#;
    let out = Command::new(tilo_test_env::package_binary!("tilo-ush"))
        .args(["run", "-c", script])
        .env(ush::store::STORE_ENV, store.root())
        .env("TILO_USH_IMAGES", &images)
        .env("SHELL", "/usr/bin/bash")
        .stdin(Stdio::null())
        .output()
        .expect("tilo-ush runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let both = format!(
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "the guest failed:\n{both}");
    assert!(stdout.contains(&format!("dir=/run/user/{uid}\n")), "{both}");
    assert!(
        stdout.contains("owned\n"),
        "the guest's user owns it:\n{both}"
    );
    assert!(stdout.contains("writable\nx\n"), "{both}");
    assert!(stdout.contains("mode=700 fs=tmpfs"), "{both}");
}
