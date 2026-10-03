//! Tests never reach the desktop session of whoever runs them.
//!
//! Cargo runs every test binary through `tools/cargo-runner.sh`, which starts
//! it inside `tools/with-test-environment.sh`: a fresh HOME and XDG tree, no
//! display, and both D-Bus addresses pointed at a socket nothing listens on.
//! A test binary started by hand skips that runner, so a test that is about
//! to start something that talks to a desktop (a compositor, an app host, a
//! media or portal client) calls [`require_isolated_desktop`] first.

use std::env;
use std::path::{Path, PathBuf};

/// The variable `tools/with-test-environment.sh` sets to its temporary root.
pub const TEST_ENVIRONMENT: &str = "TILO_TEST_ENVIRONMENT";

/// Why this process could reach the caller's desktop session, or `Ok` when
/// it runs inside the test environment.
///
/// Checks what a test inherits and cannot undo by accident: the test
/// environment's marker, no display, a runtime directory that is not a login
/// session's, and D-Bus addresses that are set (so nothing falls back to a
/// login session's default bus) and do not name one. A test may still point
/// either address at a private bus of its own.
///
/// # Errors
///
/// Returns every problem found, one per line.
pub fn desktop_isolation() -> Result<(), String> {
    let mut problems = Vec::new();
    match var(TEST_ENVIRONMENT) {
        Some(root) if Path::new(&root).is_dir() => {}
        Some(root) => problems.push(format!("{TEST_ENVIRONMENT}={root} does not exist")),
        None => problems.push(format!(
            "{TEST_ENVIRONMENT} is unset: run tests through cargo, whose runner \
             (tools/cargo-runner.sh) starts them in tools/with-test-environment.sh, \
             or wrap the command in that script"
        )),
    }
    for name in ["DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"] {
        if let Some(value) = var(name) {
            problems.push(format!("{name}={value} names a display"));
        }
    }
    match var("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(runtime) if runtime.starts_with("/run/user") => problems.push(format!(
            "XDG_RUNTIME_DIR={} is a login session's",
            runtime.display()
        )),
        Some(_) => {}
        None => problems.push("XDG_RUNTIME_DIR is unset, so clients fall back to /run/user".into()),
    }
    for (name, login) in [
        ("DBUS_SESSION_BUS_ADDRESS", "/run/user/"),
        ("DBUS_SYSTEM_BUS_ADDRESS", "/run/dbus/"),
    ] {
        match var(name) {
            None => problems.push(format!(
                "{name} is unset, so clients fall back to the default bus"
            )),
            Some(address) if address.contains(login) => {
                problems.push(format!("{name}={address} is a login session's bus"));
            }
            Some(_) => {}
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Panic unless [`desktop_isolation`] holds.
///
/// # Panics
///
/// When this process could reach the caller's desktop session.
#[track_caller]
#[allow(
    clippy::panic,
    reason = "test support: failing the calling test is the point"
)]
pub fn require_isolated_desktop() {
    if let Err(problems) = desktop_isolation() {
        panic!("refusing to run against the caller's desktop session:\n{problems}");
    }
}

fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}
