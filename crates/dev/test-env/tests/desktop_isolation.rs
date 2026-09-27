//! Test processes never reach the desktop session of whoever runs them.
//!
//! Cargo starts every test binary through `tools/cargo-runner.sh`, which runs
//! it inside `tools/with-test-environment.sh`. This test fails the suite when
//! that isolation is missing: without it a test that falls back to the
//! default session bus could pause the caller's media, open a file chooser
//! on their desktop or ask logind to end their session.
#![cfg(target_os = "linux")]

use std::env;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

#[test]
fn tests_run_without_the_callers_desktop_session() {
    tilo_test_env::require_isolated_desktop();
    let root = PathBuf::from(var(tilo_test_env::TEST_ENVIRONMENT).expect(
        "test binaries must run inside tools/with-test-environment.sh; \
         .cargo/config.toml sets tools/cargo-runner.sh as the runner for that",
    ));
    let runtime = PathBuf::from(var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR is set"));
    assert_eq!(runtime, root.join("runtime"), "private XDG_RUNTIME_DIR");
    assert_eq!(var("HOME").map(PathBuf::from), Some(root.join("home")));

    // The default addresses zbus, sd-bus and systemctl --user fall back to.
    for default in [runtime.join("bus"), runtime.join("systemd/private")] {
        assert!(!default.exists(), "{} must not exist", default.display());
    }
    for name in ["DBUS_SESSION_BUS_ADDRESS", "DBUS_SYSTEM_BUS_ADDRESS"] {
        let address = var(name).unwrap_or_default();
        let socket = PathBuf::from(
            address
                .strip_prefix("unix:path=")
                .unwrap_or_else(|| panic!("{name}={address} is not the test's dead bus")),
        );
        assert!(
            socket.starts_with(&root),
            "{name} points outside the test environment: {}",
            socket.display()
        );
        assert!(
            UnixStream::connect(&socket).is_err(),
            "{name} ({}) accepted a connection",
            socket.display()
        );
    }
}
