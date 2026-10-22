//! The thread a frame loop runs on, which must never wait on the disk.
//!
//! A path lookup can wait seconds for a busy disk; on the compositor's loop
//! that is a frozen desktop. The compositor marks its loop's thread once
//! started ([`enter`]). Code that may run there asks [`on_frame_loop`] to
//! choose a worker for its filesystem work (an app the compositor hosts in
//! its own process lists folders on one), and code that waits on the disk
//! states it with [`assert_may_wait`]: in a debug or test build, reached on
//! the loop's thread, that panics naming its caller, so a blocking call
//! that found its way onto the loop fails the build's tests where it is
//! made rather than freezing a desktop later.

use std::cell::Cell;

thread_local! {
    static FRAME_LOOP: Cell<bool> = const { Cell::new(false) };
}

/// Mark the calling thread as a frame loop's, for the rest of its life.
pub fn enter() {
    FRAME_LOOP.with(|marked| marked.set(true));
}

/// Whether the calling thread is a frame loop's ([`enter`]).
#[must_use]
pub fn on_frame_loop() -> bool {
    FRAME_LOOP.with(Cell::get)
}

/// State that the caller is about to wait on the disk for `what`. In a debug
/// or test build this panics, naming the caller, when it is reached on a
/// frame loop's thread. A release build lets it wait: a person's desktop
/// stutters rather than loses its session.
///
/// # Panics
///
/// In a debug or test build, on a frame loop's thread.
#[track_caller]
#[allow(
    clippy::panic,
    reason = "debug and test builds fail where a blocking call reaches the frame loop"
)]
pub fn assert_may_wait(what: &str) {
    if cfg!(debug_assertions) && on_frame_loop() {
        let at = std::panic::Location::caller();
        panic!("{at}: {what} waits on the disk on the frame loop's thread; move it to a worker");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_on_the_disk_is_refused_only_on_a_frame_loop() {
        // Another thread, never marked, may wait.
        assert_may_wait("a read");
        std::thread::spawn(|| {
            enter();
            assert!(on_frame_loop());
            let refused = std::panic::catch_unwind(|| assert_may_wait("listing a folder"));
            let message = refused.expect_err("allowed on the frame loop");
            let message = message
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default();
            assert!(message.contains("listing a folder"), "{message}");
            assert!(
                message.contains("frame_loop.rs"),
                "names no caller: {message}"
            );
        })
        .join()
        .unwrap();
        assert!(!on_frame_loop());
    }
}
