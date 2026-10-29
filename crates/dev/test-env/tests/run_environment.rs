//! The test environment a lane promises reaches every test, wherever the
//! tests run.
//!
//! nextest's `ci` profile sets it up on the machine that executes the tests
//! (`tools/test-env-setup.sh`, run as the profile's setup scripts in
//! `.config/nextest.toml`), and every test binary then runs inside
//! `tools/with-test-environment.sh`. These tests hold both halves: what the
//! setup promises is usable from inside a test, and the audio endpoint it
//! starts ends with the run that owns it.
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;

use tilo_test_env::require_capability;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The private endpoint's socket, reachable from this test.
fn audio_endpoint() -> Result<PathBuf, String> {
    let server = var("PULSE_SERVER").ok_or("PULSE_SERVER is not set")?;
    let socket = PathBuf::from(
        server
            .strip_prefix("unix:")
            .ok_or_else(|| format!("PULSE_SERVER={server} is not a Unix socket"))?,
    );
    UnixStream::connect(&socket)
        .map_err(|error| format!("cannot connect to {}: {error}", socket.display()))?;
    Ok(socket)
}

#[test]
fn the_promised_audio_endpoint_and_renderer_reach_the_test() {
    tilo_test_env::require_isolated_desktop();
    let socket = require_capability!(Audio, audio_endpoint(), "no private audio endpoint");
    let runtime = var("PULSE_RUNTIME_PATH").expect("the endpoint's runtime directory");
    assert_eq!(socket, Path::new(&runtime).join("native"));
    let driver = require_capability!(Gpu, var("VK_DRIVER_FILES"), "no software Vulkan driver");
    assert!(Path::new(&driver).is_file(), "{driver} is readable");
}

#[test]
fn the_setup_audio_endpoint_ends_with_its_owner() {
    tilo_test_env::require_isolated_desktop();
    let found = Command::new("sh")
        .args(["-c", "command -v pulseaudio >/dev/null"])
        .status()
        .is_ok_and(|status| status.success());
    require_capability!(Audio, found, "pulseaudio is not installed");

    let scratch = tempfile::tempdir().expect("scratch directory");
    let env_file = scratch.path().join("env");
    let mut owner = Command::new("sleep")
        .arg("600")
        .spawn()
        .expect("start the owner");
    let output = Command::new("bash")
        .arg(repo().join("tools/test-env-setup.sh"))
        .arg("audio")
        .env("NEXTEST_ENV", &env_file)
        .env("TILO_TEST_ENV_OWNER", owner.id().to_string())
        .env_remove("TILO_HEADLESS_AUDIO_PID")
        .env_remove("PULSE_SERVER")
        .env_remove("PULSE_RUNTIME_PATH")
        .env_remove("TILO_REQUIRE_AUDIO")
        .output()
        .expect("run the audio setup");
    assert!(
        output.status.success(),
        "audio setup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let provided = fs::read_to_string(&env_file).expect("the provided environment");
    let value = |name: &str| {
        provided
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("{name} provided in {provided}"))
            .to_owned()
    };
    let pid = value("TILO_HEADLESS_AUDIO_PID");
    let runtime = PathBuf::from(value("PULSE_RUNTIME_PATH"));
    assert_eq!(
        value("PULSE_SERVER"),
        format!("unix:{}/native", runtime.display())
    );
    assert_eq!(value("TILO_REQUIRE_AUDIO"), "1");
    let comm = format!("/proc/{pid}/comm");
    assert_eq!(
        fs::read_to_string(&comm).ok().as_deref(),
        Some("pulseaudio\n")
    );
    UnixStream::connect(runtime.join("native")).expect("the endpoint accepts clients");

    // The endpoint outlives the setup step and ends with the run that owns it.
    owner.kill().expect("end the owner");
    owner.wait().expect("reap the owner");
    tilo_test_env::wait_until("the endpoint ends with its owner", || {
        fs::read_to_string(&comm).ok().as_deref() != Some("pulseaudio\n") && !runtime.exists()
    });
}
