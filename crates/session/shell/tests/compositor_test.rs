//! The native compositor's process lifecycle: a clean exit leaves no
//! runtime endpoint behind, so the next run can bind the canonical name.

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// `native::run` ends with `process::exit`, so endpoint guards cannot be left
/// to ordinary Rust destruction. Run the production binary twice against
/// one throwaway runtime and require the first run's endpoints to be gone
/// before the second can reclaim the canonical `wayland-1` name.
#[test]
#[ignore = "spawns two real native compositor processes"]
fn clean_exit_removes_runtime_endpoints_and_allows_canonical_rebind() {
    tilo_test_env::require_isolated_desktop();
    let runtime = tempfile::tempdir().expect("temporary runtime");
    let state = runtime.path().join("state.json");
    let identity_root = runtime.path().join("identity-root");
    let spawn = || {
        Command::new(tilo_test_env::package_binary!("shell"))
            .args(["--mode", "native", "--backend", "headless"])
            .env("TILO_RUN_SECONDS", "2")
            // Not this process's `HOME`: another test may own it for its own
            // run and delete the tree it points at (see `private_home`).
            .env(
                "HOME",
                crate::support::desktop::private_home(runtime.path()),
            )
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("XDG_CONFIG_HOME", runtime.path().join("config"))
            .env("XDG_DATA_HOME", runtime.path().join("data"))
            .env("XDG_STATE_HOME", runtime.path().join("state"))
            .env("XDG_CACHE_HOME", runtime.path().join("cache"))
            .env("TILO_STATE_PATH", &state)
            .env("TILO_IDENTITY_ROOT", &identity_root)
            .env("TILO_WORKSPACE_ROOTS", "")
            .env("TILO_HEADLESS_SIZE", "640x480")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn native compositor")
    };
    let wait_for_socket = |child: &mut std::process::Child| {
        let socket = runtime.path().join("wayland-1");
        let deadline = tilo_test_env::hang_deadline();
        while !socket.exists() && child.try_wait().expect("poll compositor").is_none() {
            assert!(
                Instant::now() < deadline,
                "compositor did not bind wayland-1"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            socket.exists(),
            "compositor exited before binding wayland-1"
        );
    };
    let assert_clean = || {
        let tilo_session = runtime.path().join("tilo-session");
        assert!(
            !tilo_session.join("wayland-display").exists(),
            "wayland-display marker survived clean compositor exit"
        );
        for root in [runtime.path(), tilo_session.as_path()] {
            if !root.exists() {
                continue;
            }
            for entry in std::fs::read_dir(root).expect("read runtime endpoints") {
                let name = entry
                    .expect("runtime endpoint entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned();
                assert!(
                    !name.starts_with("wayland-")
                        && !name.starts_with("tilo-osk-")
                        && !name.starts_with("tilo-im-")
                        && !name.starts_with("tilo-privileged-")
                        && !std::path::Path::new(&name)
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("lock")),
                    "stale compositor endpoint remained: {}/{}",
                    root.display(),
                    name
                );
            }
        }
        for service in ["shell-ui.sock", "identity.sock"] {
            assert!(
                !runtime.path().join("tilo/services").join(service).exists(),
                "stale Tilo service endpoint remained: {service}"
            );
        }
    };

    let mut first = spawn();
    wait_for_socket(&mut first);
    assert!(first.wait().expect("wait first compositor").success());
    assert_clean();

    let mut second = spawn();
    wait_for_socket(&mut second);
    assert!(second.wait().expect("wait second compositor").success());
    assert_clean();
}
