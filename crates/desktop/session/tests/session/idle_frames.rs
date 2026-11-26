//! A shell nobody touches asks for nothing: every part of it that draws must settle, so the loop
//! waits for the person rather than painting the same shell again and again.

use super::*;

/// A shell with nothing happening on it waits for something to happen: every part of it that says it
/// needs a frame must settle, or the loop draws the same shell again and again.
#[test]
fn a_settled_shell_asks_for_no_frame() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 300);
    // What the shell asks for is the property; the frames above are only how far it was stepped. A
    // pass it still owes (the last of what it drew, a change it carried) comes due a moment after
    // them, so wait for the shell to say it rests before reading its answer.
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
        "an idle shell waits for the person or a minute of its own, not for a frame: a frame in \
         {wait:?}, a pass in {left:?} after {:?} of stepping it",
        tilo_test_env::hang_bound()
    );
}

/// A shell nobody touches is passed when something of the session's own comes due, not four times a
/// second: nothing of it asks to be looked at.
#[test]
fn an_idle_shell_wakes_for_nothing() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
    eprintln!("an idle shell's turns: {per_second:.2}/s");
    assert!(
        turns <= 2,
        "an idle shell is passed {turns} times in {window:?}, not once"
    );
}

/// A gesture's events arrive far faster than a display shows frames: the display they are shown on
/// takes them in at its blank, so the shell's turn is taken once per refresh, not once per event.
#[test]
fn a_held_frame_takes_the_turns_of_a_gesture() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 30);
    let drawn = session.compositor().frames_presented();
    let shell = session.handle().revision();
    // The gesture's samples are all in the shell's hands before the frame it draws: the shell takes
    // the gesture in one turn, not one per event.
    let events = 120_u32;
    for sample in 0..events {
        session
            .compositor()
            .input(InputEvent::PointerMotion {
                x: 320.0 + f64::from(sample),
                y: 240.0,
            })
            .expect("input");
    }
    assert_eq!(
        session.compositor().pointer_location(),
        (320.0 + f64::from(events - 1), 240.0),
        "the shell took every sample of the gesture as it came"
    );
    session.step(Duration::from_millis(1)).expect("step");
    // One turn for the gesture: the frame the shell draws is the gesture's, however many events it
    // brought, and the shell is laid out for it once.
    let frames = session.compositor().frames_presented() - drawn;
    assert!(
        frames <= 2,
        "a gesture is drawn once, not once per event: {frames} frames for {events} events"
    );
    assert!(frames > 0, "the frame the shell drew is shown");
    assert!(
        session.handle().revision() <= shell + 1,
        "the shell is laid out once for the gesture's events, not once per event: it stood at \
         {shell} and is at {}",
        session.handle().revision()
    );
}

/// A turn the loop hands a settled shell (a timer, a client's message, a display's blank) paints
/// nothing: what it shows has not changed, so the display keeps the frame it has and the machine
/// sleeps.
#[test]
fn a_turn_handed_to_a_settled_shell_paints_nothing() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = session();
    let mut quiet = 0_u32;
    let mut drawn = session.compositor().frames_presented();
    let _ = crate::common::wait_maybe(&mut session, |session| {
        let now = session.compositor().frames_presented();
        quiet = if now == drawn { quiet + 1 } else { 0 };
        drawn = now;
        (quiet >= 6).then_some(())
    });
    session.compositor().record_frames();
    for _ in 0..120 {
        session.step(Duration::ZERO).expect("an offered turn");
    }
    let painted: Vec<String> = (session.compositor().recorded_frames().iter())
        .filter(|sample| sample.painted)
        .map(|sample| {
            let why = tilo_frame_metrics::paint_reason::names(sample.paint_reasons);
            format!("{why} {} {}", sample.paint_clients, sample.paint_animations)
        })
        .collect();
    assert!(
        painted.is_empty(),
        "an idle shell painted {} of 120 turns it was handed, for: {painted:?}",
        painted.len()
    );
}
