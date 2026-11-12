//! The wall clock a test holds, so what the desk draws of the date and time
//! is the same at the end of the test as at its start.

use tilo_clock::testing::SharedWall;

/// Hold this thread's wall clock at [`crate::CAPTURE_LOCAL_TIME`], the
/// instant a test environment and a `--shots` capture start at.
///
/// A test that takes a picture of the desk before something happens and
/// another after compares the thing it did, not the minute the bar's clock
/// turned over in between. Hold the clock and the bar shows the same date and
/// time in both pictures.
///
/// Only the thread that calls this follows the clock, so the other tests of
/// the process read their own; hold the whole process with
/// [`crate::in_own_process`] and [`tilo_clock::testing::hold_wall_everywhere`]
/// where a thread of the test's own must read it too.
#[must_use]
pub fn held_wall_clock() -> SharedWall {
    let millis = crate::environment::capture_millis().unwrap_or(0);
    let wall = SharedWall::default();
    wall.follow();
    wall.pin(
        tilo_clock::time::UNIX_EPOCH
            .checked_add(tilo_clock::time::Duration::from_millis(
                millis.unsigned_abs(),
            ))
            .unwrap_or(tilo_clock::time::UNIX_EPOCH),
    );
    wall
}
