//! A desk nobody touches asks for nothing: every part of it that draws must settle, so the loop
//! waits for the person rather than painting the same desk again and again.

use super::*;

/// A desk with nothing happening on it waits for something to happen: every part of it that says it
/// needs a frame must settle, or the loop draws the same desk again and again.
#[test]
fn a_settled_desk_asks_for_no_frame() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 300);
    // What the desk asks for is the property; the frames above are only how far it was stepped. A
    // pass it still owes (the last of what it drew, a change it carried) comes due a moment after
    // them, so wait for the desk to say it rests before reading its answer.
    let rests = |ask: Option<Duration>| ask.is_none_or(|ask| ask >= Duration::from_secs(1));
    let _ = crate::common::wait_maybe(&mut session, |session| {
        (rests(session.compositor().next_wait()) && rests(wait_for(session))).then_some(())
    });
    let wait = session.compositor().next_wait();
    // What the session itself needs a pass for is minutes apart, not a poll per quarter second: a
    // clock, a presence beat, a look for a model.
    let left = session
        .next_deadline()
        .map(|due| due.saturating_duration_since(Instant::now()));
    assert!(
        rests(wait) && rests(left),
        "an idle desk waits for the person or a minute of its own, not for a frame: a frame in \
         {wait:?}, a pass in {left:?} after {:?} of stepping it",
        tilo_test_env::hang_bound()
    );
}

/// A desk nobody touches is passed when something of the session's own comes due, not four times a
/// second: nothing of it asks to be looked at.
#[test]
fn an_idle_desk_wakes_for_nothing() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 60);
    let window = Duration::from_secs(2);
    let started = Instant::now();
    let mut turns = 0_u32;
    while started.elapsed() < window {
        let left = window.checked_sub(started.elapsed()).unwrap_or_default();
        let wait = wait_for(&mut session).map(|wait| wait.min(left));
        session.step(wait).expect("step");
        turns += 1;
    }
    let per_second = f64::from(turns) / started.elapsed().as_secs_f64();
    eprintln!("an idle desk's turns: {per_second:.2}/s");
    assert!(
        turns <= 2,
        "an idle desk is passed {turns} times in {window:?}, not once"
    );
}

/// A gesture's events arrive far faster than a display shows frames: the display they are shown on
/// takes them in at its blank, so the desk's turn is taken once per refresh, not once per event.
#[test]
fn a_held_frame_takes_the_turns_of_a_gesture() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 30);
    let drawn = session.compositor().frames_presented();
    let window = Duration::from_millis(120);
    let started = Instant::now();
    let mut turns = 0_u32;
    while started.elapsed() < window {
        session
            .compositor()
            .input(InputEvent::PointerMotion { x: 320.0, y: 240.0 })
            .expect("input");
        session.step(Duration::from_millis(1)).expect("step");
        turns += 1;
    }
    // One turn per refresh of the session's 60 Hz output: nine in this window. A turn drawn at once
    // for every event is twenty-one, the desk laid out for each of them.
    assert!(
        turns <= 14,
        "a held frame takes the gesture's turns with it: {turns} in {window:?}"
    );
    assert!(
        session.compositor().frames_presented() > drawn,
        "the frames the desk drew are shown"
    );
}
