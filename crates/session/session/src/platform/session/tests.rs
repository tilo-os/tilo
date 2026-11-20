//! What the session's own hosts tell the watchdog.

use std::time::Duration;

use tilo_liveness::watchdog::{Policy, Watchdog};

use super::{LoopTurns as _, Turns};

/// The compositor's hook marks the loop's turns as the watchdog counts them, so a loop that
/// waits for events is never judged.
#[test]
fn the_compositor_hook_marks_the_loops_turns() {
    let watchdog = Watchdog::watch_this_thread(
        Policy {
            period: Duration::from_millis(50),
            hung_after: Duration::from_secs(60 * 60),
            report_within: Duration::from_secs(1),
        },
        |_| panic!("a loop that marks its turns is never judged hung"),
    )
    .expect("the watchdog thread starts");
    let turns = Turns(watchdog);
    for _ in 0..3 {
        turns.parking();
        std::thread::sleep(Duration::from_millis(20));
        turns.unparked();
    }
}
