//! A desk nobody touches asks for nothing: every part of it that draws
//! must settle, so the loop waits for the person rather than painting the
//! same desk again and again.

use super::*;

/// A desk with nothing happening on it waits for something to happen:
/// every part of it that says it needs a frame must settle, or the loop
/// draws the same desk again and again.
#[test]
fn a_settled_desk_asks_for_no_frame() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 300);
    let wait = session.compositor().next_wait();
    assert!(
        wait >= Duration::from_millis(100),
        "an idle desk waits for the person, not for a frame: {wait:?}"
    );
}
