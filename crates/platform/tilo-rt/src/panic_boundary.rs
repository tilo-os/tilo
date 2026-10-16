//! Which panics a process survives.
//!
//! Release builds unwind, but a Tilo process still ends on the first panic
//! on any thread: [`abort_outside_boundaries`] installs a panic hook that
//! aborts once every earlier hook (a crash log) has run. This crate installs
//! it as soon as a process starts a runtime or a thread through it, so code
//! keeps the abort-on-panic contract it is written against: state shared
//! with a thread that unwound halfway is not trusted anywhere else, and a
//! task's panic is never swallowed by its runtime.
//!
//! The one exception is a thread running inside [`isolate`]. There the hook
//! returns, the panic unwinds to the boundary, and the helper's owner decides
//! what happens next (typically: release what the helper held and start it
//! again). The compositor isolates its helpers this way, since its death ends
//! the person's session. The boundary is opt-in per call, never per thread
//! name.
//!
//! Debug and test builds keep unwinding everywhere, as they always have:
//! a test that expects a panic must not abort its whole binary. Under
//! `panic = "abort"` [`isolate`] only runs its body: nothing unwinds.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Once;

/// Abort the process on any panic outside [`isolate`], after the hooks
/// installed before this one (which it calls first) have reported it.
/// Idempotent; a hook installed later should chain to this one.
pub fn abort_outside_boundaries() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let earlier = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            earlier(info);
            if !is_isolated() {
                std::process::abort();
            }
        }));
    });
}

/// [`abort_outside_boundaries`] in an optimised build that unwinds; nothing
/// in a debug or test build, which unwinds everywhere.
pub(crate) fn abort_outside_boundaries_in_release() {
    if cfg!(all(panic = "unwind", not(debug_assertions))) {
        abort_outside_boundaries();
    }
}

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Run `body` as a panic boundary: a panic inside it (on this thread)
/// unwinds to here and is returned as `Err` with its payload, and
/// [`is_isolated`] answers `true` meanwhile so the process's panic hook lets
/// it unwind.
///
/// `body` must leave nothing half-updated that outlives it, or whoever
/// reads that state afterwards must tolerate it (poisoned locks read as
/// unavailable, a dropped channel as closed).
///
/// # Errors
///
/// The panic payload when `body` panicked.
pub fn isolate<R>(body: impl FnOnce() -> R) -> std::thread::Result<R> {
    struct Entered;
    impl Drop for Entered {
        fn drop(&mut self) {
            DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
    DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    let _entered = Entered;
    catch_unwind(AssertUnwindSafe(body))
}

/// Whether the calling thread is inside [`isolate`]: a panic hook that
/// aborts the process for any other panic returns for this one.
#[must_use]
pub fn is_isolated() -> bool {
    DEPTH.with(|depth| depth.get() > 0)
}

/// The panic message in `payload`, for a log line.
#[must_use]
pub fn message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-text panic payload>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_inside_the_boundary_returns_and_the_marker_follows_the_scope() {
        assert!(!is_isolated());
        let inner = isolate(|| {
            assert!(is_isolated());
            let nested = isolate(|| panic!("helper failed"));
            let payload = nested.expect_err("the nested panic is caught");
            assert_eq!(message(payload.as_ref()), "helper failed");
            assert!(is_isolated(), "the outer boundary still holds");
            7
        });
        assert_eq!(inner.expect("the outer body returned"), 7);
        assert!(!is_isolated());

        let formatted = isolate(|| panic!("{} failed", "broker"));
        assert_eq!(
            message(formatted.expect_err("caught").as_ref()),
            "broker failed"
        );
        assert!(!is_isolated(), "an unwound boundary is left");
    }

    const FIXTURE: &str = "TILO_RT_PANIC_BOUNDARY_FIXTURE";

    /// The scenario this test binary runs when it is the fixture child.
    #[test]
    fn panic_boundary_fixture() {
        let Ok(scenario) = std::env::var(FIXTURE) else {
            return;
        };
        abort_outside_boundaries();
        if scenario == "isolated" {
            let helper =
                crate::spawn_thread("helper", || isolate(|| panic!("helper failed")).is_err())
                    .expect("helper thread");
            assert!(helper.join().expect("the helper thread returned"));
        } else {
            let _ = crate::spawn_thread("worker", || panic!("worker failed"))
                .expect("worker thread")
                .join();
        }
        println!("process survived");
    }

    #[test]
    fn only_a_panic_inside_a_boundary_leaves_the_process_running() {
        let run = |scenario: &str| {
            std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "panic_boundary::tests::panic_boundary_fixture",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(FIXTURE, scenario)
                .output()
                .expect("fixture child")
        };
        let isolated = run("isolated");
        assert!(isolated.status.success(), "{isolated:?}");
        assert!(String::from_utf8_lossy(&isolated.stdout).contains("process survived"));

        let outside = run("outside");
        assert!(!outside.status.success(), "{outside:?}");
        assert!(!String::from_utf8_lossy(&outside.stdout).contains("process survived"));
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            assert_eq!(outside.status.signal(), Some(libc_sigabrt()), "{outside:?}");
        }
    }

    #[cfg(unix)]
    const fn libc_sigabrt() -> i32 {
        6
    }
}
