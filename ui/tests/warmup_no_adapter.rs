//! The startup warmup must not take the process down on a machine with
//! no usable wgpu adapter.
//!
//! `ui::warmup::spawn_warmup` runs on the compositor's real startup path
//! (`shell/src/main.rs`). Its Vello stage builds an
//! [`OffscreenRenderer`](ui::offscreen::OffscreenRenderer), which needs
//! an adapter. On a GPU-less box — headless CI, a VM with no llvmpipe —
//! there isn't one, and the documented behaviour is "degrade silently,
//! the real backend reports its own adapter error later".
//!
//! # Why this needs a child process
//!
//! That guarantee used to be implemented with `std::panic::catch_unwind`
//! around a panicking constructor. **`catch_unwind` catches nothing under
//! `panic = "abort"`**, which is what the workspace release profile sets
//! (`Cargo.toml`, `[profile.release]`): the panic runtime aborts at the
//! panic site and no unwinding — hence no catching — ever starts. So the
//! soft-fail worked in dev builds and was inert in exactly the
//! configuration that ships.
//!
//! Cargo ignores `panic` on `[profile.test]`, so a test binary is always
//! built `panic = "unwind"` and cannot host the failing configuration
//! directly. The child process below reproduces it instead: it installs a
//! panic hook that calls `std::process::abort()`. Under `panic = "abort"`
//! the hook still runs and the process then aborts without unwinding, so
//! "hook aborts" and "abort strategy" are indistinguishable to anything
//! downstream of the panic — including `catch_unwind`, which returns in
//! neither case.
//!
//! The child also proves it is really testing the no-adapter case rather
//! than passing because the machine happens to have a GPU: it asserts
//! `OffscreenRenderer::try_new` returns `Err` before it goes anywhere
//! near the warmup, and exits with a distinct code if it does not, so the
//! parent fails loudly instead of passing vacuously.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Set in the child so the same test function runs the child body.
const CHILD_MARKER: &str = "QOS_WARMUP_NO_ADAPTER_CHILD";

/// `wgpu::Backends::from_comma_list` skips tokens it doesn't recognise,
/// so an unknown name yields an empty backend set: the instance has no
/// backends, enumerates no adapters, and adapter acquisition fails on
/// every platform. Preferred over naming a real-but-absent backend
/// (`metal` on Linux), which would stop simulating anything on a Mac.
const NO_BACKENDS: &str = "qos-no-such-backend";

/// Child exit code for "the environment still has an adapter" — the
/// simulation failed, so the run proves nothing.
const EXIT_SIMULATION_FAILED: i32 = 91;
/// Child exit code for "the warmup thread unwound out of a panic".
const EXIT_WARMUP_PANICKED: i32 = 92;
/// Child exit code for "the warmup ran and degraded silently", i.e. the pass.
///
/// Not 0: libtest also exits 0 when its filter matches nothing, so a child that
/// ran no test at all would otherwise be indistinguishable from a pass.
const EXIT_WARMUP_OK: i32 = 90;

/// The name libtest knows this test by.
///
/// Every `tests/*.rs` is a module of `tests/it.rs`, so the test is
/// `warmup_no_adapter::<fn>`. `module_path!()` yields `it::warmup_no_adapter` —
/// one segment too many, because libtest omits the binary's own crate.
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
        // Warmup is on by default, but the parent's environment might
        // have turned it off; the child needs it on to test anything.
        .env("QOS_STARTUP_WARM", "on")
        .env_remove("QOS_STARTUP_METRICS_PATH")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn child test process");

    // Bounded wait: the child builds a throw-away FontSystem, which is
    // slow on a cold fontconfig cache but nowhere near this long.
    let deadline = Instant::now() + Duration::from_secs(180);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("warmup child did not finish within 180s");
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
        // No exit code means a signal. Under `panic = "abort"` semantics
        // that is SIGABRT: the process died at the panic site, which is
        // exactly what a compositor must not do at startup.
        None => panic!(
            "warmup child was killed by a signal ({status}) rather than exiting — \
             the startup warmup aborted the process on a machine with no wgpu adapter"
        ),
    }
}

/// Runs in the re-executed child. Diverges: always exits the process.
fn child_body() -> ! {
    // Step 1 — prove the simulation works. This must happen before the
    // aborting hook is installed so a surprise here surfaces as a clean
    // exit code rather than a signal.
    match ui::offscreen::OffscreenRenderer::try_new(4, 4) {
        Err(e) => eprintln!("child: no adapter, as intended: {e}"),
        Ok(_) => {
            eprintln!("child: an adapter was still available — cannot simulate a GPU-less machine");
            std::process::exit(EXIT_SIMULATION_FAILED);
        }
    }

    // Step 2 — make this process behave like a release build. See the
    // module docs: with `panic = "abort"` the hook runs and the process
    // aborts without unwinding, so `catch_unwind` never returns.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("child: panicked ({info}); aborting the way panic=abort would");
        std::process::abort();
    }));

    // Step 3 — the real production call. `false` means "do the Vello
    // stage too", which is the stage that needs an adapter.
    let handle = ui::warmup::spawn_warmup(false).expect("warmup should be enabled in the child");
    match handle.join() {
        Ok(()) => {
            eprintln!("child: warmup finished without an adapter");
            std::process::exit(EXIT_WARMUP_OK)
        }
        Err(_) => std::process::exit(EXIT_WARMUP_PANICKED),
    }
}
