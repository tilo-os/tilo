//! Production-binary coverage for removed run modes.
//!
//! `--portal-server-only` once served the portal bus name from an empty tree
//! with a prompt queue nothing drained (docs/architecture/security.md). The
//! argument parser is otherwise permissive, so the binary must refuse the
//! flag outright: a stale unit that still passes it fails loudly instead of
//! claiming the bus name or starting a second compositor.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

#[test]
fn portal_server_only_is_refused_before_any_setup() {
    let root = TempDir::new().expect("test root");
    let startup_metrics = root.path().join("startup.json");
    let mut child = Command::new(tilo_test_env::package_binary!("shell"))
        .arg("--portal-server-only")
        .env("HOME", root.path().join("home"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("XDG_RUNTIME_DIR", root.path().join("run"))
        // A bus that does not exist: the removed mode connected here and
        // then blocked forever, which the deadline below would catch.
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", root.path().join("no-bus").display()),
        )
        .env("TILO_STARTUP_METRICS_PATH", &startup_metrics)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run production shell binary");

    let deadline = tilo_test_env::hang_deadline();
    while child.try_wait().expect("poll shell").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`shell --portal-server-only` must exit at once, not keep serving");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("collect shell output");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        output.status.code(),
        Some(2),
        "a removed run mode exits with status 2, stderr: {stderr}"
    );
    assert!(
        stderr.contains("--portal-server-only was removed")
            && stderr.contains("docs/architecture/security.md"),
        "the refusal names the flag and where the portal lives now, stderr: {stderr}"
    );
    assert!(
        !startup_metrics.exists(),
        "the refusal comes before startup begins, so no startup metrics are recorded"
    );
}
