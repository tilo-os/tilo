//! The startup warmup must not take the process down on a machine with no
//! usable wgpu adapter — `tilo_ui::warmup::spawn_warmup`'s Vello stage builds
//! an [`OffscreenRenderer`](tilo_ui::offscreen::OffscreenRenderer), and on a
//! GPU-less box that needs to degrade silently rather than crash.
//!
//! # Why this needs a child process
//!
//! `catch_unwind` catches nothing under `panic = "abort"`, which is what
//! the release profile sets — so a soft-fail built on it is inert in
//! exactly the configuration that ships. Cargo ignores `panic` on
//! `[profile.test]`, so a test binary is always `panic = "unwind"` and
//! can't host that configuration directly. The child reproduces it with a
//! panic hook that calls `std::process::abort()` — indistinguishable from
//! a real abort-strategy panic to anything downstream, `catch_unwind`
//! included.
//!
//! The child also asserts `OffscreenRenderer::try_new` actually returns
//! `Err` before going near the warmup, and exits with a distinct code if
//! it doesn't — so a machine that happens to have a GPU fails the test
//! loudly instead of passing vacuously.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Set in the child so the same test function runs the child body.
const CHILD_MARKER: &str = "TILO_WARMUP_NO_ADAPTER_CHILD";

/// `wgpu::Backends::from_comma_list` skips tokens it doesn't recognise, so
/// an unknown name yields an empty backend set on every platform. Preferred
/// over naming a real-but-absent backend (`metal` on Linux), which would
/// stop simulating anything on a Mac.
const NO_BACKENDS: &str = "tilo-no-such-backend";

/// Child exit code for "the environment still has an adapter" — the
/// simulation failed, so the run proves nothing.
const EXIT_SIMULATION_FAILED: i32 = 91;
/// Child exit code for "the warmup thread unwound out of a panic".
const EXIT_WARMUP_PANICKED: i32 = 92;
/// Child exit code for "the warmup ran and degraded silently", i.e. the
/// pass. Not 0: libtest also exits 0 when its filter matches nothing.
const EXIT_WARMUP_OK: i32 = 90;

/// The name libtest knows this test by. Every `tests/*.rs` is a module of
/// `tests/it.rs`, so `module_path!()` yields `it::warmup_no_adapter` — one
/// segment too many, since libtest omits the binary's own crate.
fn libtest_name(func: &str) -> String {
    let m = module_path!();
    let m = m.split_once("::").map_or(m, |(_, rest)| rest);
    format!("{m}::{func}")
}

#[test]
fn warmup_degrades_silently_when_no_wgpu_adapter_exists() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        child_body();
    }

    let exe = std::env::current_exe().expect("current_exe");
    let mut child = Command::new(exe)
        .args([
            "--exact",
            &libtest_name("warmup_degrades_silently_when_no_wgpu_adapter_exists"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_MARKER, "1")
        .env("WGPU_BACKEND", NO_BACKENDS)
        // CI pins ordinary renderer tests to llvmpipe. This child is
        // intentionally constructing an empty backend set, so inheriting an
        // adapter requirement would make wgpu reject the contradictory
        // configuration before `try_new` can exercise its no-adapter path.
        .env_remove("WGPU_ADAPTER_NAME")
        // Warmup is on by default, but the parent's environment might
        // have turned it off; the child needs it on to test anything.
        .env("TILO_STARTUP_WARM", "on")
        .env_remove("TILO_STARTUP_METRICS_PATH")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn child test process");

    // Bounded wait: the child builds a throw-away FontSystem, which is
    // slow on a cold fontconfig cache but nowhere near this long.
    let deadline = tilo_test_env::hang_deadline();
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("warmup child did not finish");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    match status.code() {
        Some(EXIT_WARMUP_OK) => {}
        Some(0) => panic!(
            "the child ran no test — libtest's filter matched nothing, so this \
             run proves nothing. The test's module path has probably changed."
        ),
        Some(EXIT_SIMULATION_FAILED) => panic!(
            "child still found a wgpu adapter with WGPU_BACKEND={NO_BACKENDS}; \
             the no-adapter case was never exercised, so this run proves nothing"
        ),
        Some(EXIT_WARMUP_PANICKED) => panic!(
            "the warmup thread panicked instead of degrading silently when \
             no wgpu adapter was available"
        ),
        Some(other) => panic!("warmup child exited with unexpected code {other}"),
        // No exit code means a signal — under `panic = "abort"` that is
        // SIGABRT, exactly what a compositor must not do at startup.
        None => panic!(
            "warmup child was killed by a signal ({status}) rather than exiting — \
             the startup warmup aborted the process on a machine with no wgpu adapter"
        ),
    }
}

/// Runs in the re-executed child. Diverges: always exits the process.
#[allow(
    clippy::exit,
    reason = "this re-executed child process communicates its outcome to the parent purely via \
              exit code; the function returns `!`, so returning normally is not an option"
)]
fn child_body() -> ! {
    // Prove the simulation works, before the aborting hook is installed,
    // so a surprise here surfaces as a clean exit code rather than a signal.
    if let Err(e) = tilo_ui::offscreen::OffscreenRenderer::try_new(4, 4) {
        eprintln!("child: no adapter, as intended: {e}");
    } else {
        eprintln!("child: an adapter was still available — cannot simulate a GPU-less machine");
        std::process::exit(EXIT_SIMULATION_FAILED);
    }

    // Make this process behave like a release build — see the module docs.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("child: panicked ({info}); aborting the way panic=abort would");
        std::process::abort();
    }));

    // `false` means "do the Vello stage too", the stage that needs an adapter.
    let handle =
        tilo_ui::warmup::spawn_warmup(false).expect("warmup should be enabled in the child");
    match handle.join() {
        Ok(()) => {
            eprintln!("child: warmup finished without an adapter");
            std::process::exit(EXIT_WARMUP_OK)
        }
        Err(_) => std::process::exit(EXIT_WARMUP_PANICKED),
    }
}
