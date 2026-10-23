//! How long a new Browser window takes to show its chrome: the first window of
//! a profile, which starts the engine host, and a new tab beside it, which
//! browses through that host. Both run the production app host and the real
//! Chromium engine client against a stand-in host that is still starting (it
//! never reports ready), so the numbers are the window's own work and any
//! wait on the engine. Budgets apply to optimised builds only; a debug build
//! prints the numbers.
//!
//! `cargo test --profile thin-app -p browser --test perf_budget -- --nocapture`
#![allow(
    clippy::cast_possible_truncation,
    reason = "microsecond timings of a window opening fit in u64"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    clippy::panic,
    reason = "a benchmark reports on stderr and fails loudly"
)]

use std::path::Path;
use std::time::Instant;

use ui::framework::testing::TestApp;

/// An engine host that starts and reads its commands but never reports
/// ready, as a host still initializing on a loaded machine does.
#[cfg(unix)]
fn starting_host(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let host = dir.join("host");
    std::fs::write(
        &host,
        format!(
            "#!/bin/sh\n[ \"$1\" = --warm ] && exit 0\n\
             echo spawned >> '{}'\nexec 3>&1\nexec cat >/dev/null\n",
            dir.join("spawns").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o755)).unwrap();
    // macOS assesses a newly written executable on its first launch, which
    // on a busy machine has taken longer than the engine waits for a host:
    // launch it once here, so what is measured is the window, not the
    // assessment.
    assert!(
        std::process::Command::new(&host)
            .arg("--warm")
            .status()
            .unwrap()
            .success()
    );
    host
}

/// Open a Browser window and draw its first frame; the time it took.
fn open_window() -> (TestApp<browser::BrowserShell>, u64) {
    let started = Instant::now();
    let mut app = TestApp::<browser::BrowserShell>::new().expect("Browser opens");
    app.harness().relayout();
    let elapsed = started.elapsed().as_micros() as u64;
    assert!(
        !app.harness().targets().is_empty(),
        "the window drew its chrome"
    );
    (app, elapsed)
}

fn budget(label: &str, value: u64, limit: u64) {
    eprintln!("perf {label}: {value}us (budget {limit}us in optimised builds)");
    if !cfg!(debug_assertions) {
        assert!(value <= limit, "{label}: {value}us exceeds {limit}us");
    }
}

#[test]
#[cfg(unix)]
fn a_new_window_and_a_new_tab_open_while_the_engine_starts() {
    let dir = tempfile::tempdir().unwrap();
    let host = starting_host(dir.path());
    let runtime = dir.path().join("run");
    std::fs::create_dir_all(&runtime).unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_CHROMIUM_HOST", &host)
        .set("TILO_BROWSER_APP_ROOT", dir.path().join("browser"))
        .set("HOME", dir.path())
        .set("XDG_RUNTIME_DIR", &runtime)
        .set("XDG_DATA_HOME", dir.path().join("data"))
        .set("XDG_CONFIG_HOME", dir.path().join("config"))
        .set("XDG_CACHE_HOME", dir.path().join("cache"))
        .set("XDG_STATE_HOME", dir.path().join("state"));

    // The profile's first window starts the engine host. It does so without
    // waiting on it, so the stand-in may not have run its first line yet.
    let (_first, first) = open_window();
    let spawns = || {
        std::fs::read_to_string(dir.path().join("spawns"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let started = Instant::now();
    while spawns() == 0 && started.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(spawns(), 1, "the first window starts the engine host");
    // Its time is mostly the process's one-time setup (the font database,
    // the first text renderer: 65-75 ms on the ARM builder) and swings with
    // the machine's load, so it is held only to what tells waiting on the
    // host apart: this host never becomes ready, and a window that waited
    // for it would take the host's 20 s readiness limit.
    budget(
        "browser first window (host still starting)",
        first,
        5_000_000,
    );

    // A new tab beside it is another Browser window on the same profile: it
    // browses through the first window's host, whether or not that is ready.
    let mut tabs = Vec::new();
    let mut open = Vec::new();
    for _ in 0..3 {
        let (app, elapsed) = open_window();
        open.push(app);
        tabs.push(elapsed);
    }
    tabs.sort_unstable();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("spawns"))
            .unwrap_or_default()
            .lines()
            .count(),
        1,
        "new tabs share the first window's host"
    );
    // The windows stayed open until here, so each joined a live host.
    drop(open);
    budget("browser new tab (median of 3)", tabs[1], 150_000);
}
