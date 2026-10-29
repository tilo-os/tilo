//! `tilo-shell --help` and unknown flags answer with usage and exit, before
//! the compositor starts anything: no spawner, no identity check, no session.

use std::process::{Command, Output};

/// Run the production binary in a throwaway runtime, so a regression that
/// starts a session anyway finds nothing of this process's to touch.
fn shell(args: &[&str]) -> Output {
    let runtime = tempfile::tempdir().expect("temporary runtime");
    Command::new(tilo_test_env::package_binary!("shell"))
        .args(args)
        .env("HOME", runtime.path())
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("XDG_CONFIG_HOME", runtime.path().join("config"))
        .env("XDG_DATA_HOME", runtime.path().join("data"))
        .env("XDG_STATE_HOME", runtime.path().join("state"))
        .env("XDG_CACHE_HOME", runtime.path().join("cache"))
        .env("TILO_IDENTITY_ROOT", runtime.path().join("identity"))
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .output()
        .expect("run shell")
}

#[test]
fn help_prints_usage_and_exits_before_starting_anything() {
    for flag in ["--help", "-h"] {
        let out = shell(&[flag]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{flag}: {stderr}");
        assert!(stdout.starts_with("usage: tilo-shell"), "{flag}: {stdout}");
        assert!(stderr.is_empty(), "{flag} started something: {stderr}");
    }
}

#[test]
fn an_unknown_flag_prints_usage_and_exits_2_before_starting_anything() {
    let out = shell(&["--backend", "headless", "--no-such-flag"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.starts_with("error: unknown option --no-such-flag\n\nusage: tilo-shell"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty());
}
