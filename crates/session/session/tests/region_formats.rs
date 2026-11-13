//! The person's region formats reach the session's own surfaces when they change them in
//! Settings: the first day of their week, their date and clock styles, which their desk's
//! calendar, their shell's views and the tilolang hosts the session runs write by
//! (`crate::formats`). A session that started with one region reads the next one on a pass of its
//! own loop, without a restart.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use crate::common::{private_home, seeded_session};

/// Step the loop once.
fn step(session: &mut tilo_session::Session) {
    session.step(Duration::from_millis(1)).expect("step");
}

/// Step the loop until `done` holds.
fn when(session: &mut tilo_session::Session, done: impl Fn() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while std::time::Instant::now() < deadline {
        step(session);
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the session never read the person's region");
}

/// The region the person chose in Region & Formats, kept as the pane keeps it.
fn choose(first_day: &str, time: &str) {
    settings_store::update_section(
        "locale",
        serde_json::json!({"first_day_of_week": first_day, "time_format": time})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    )
    .expect("the region is kept");
}

#[test]
fn a_region_the_person_changes_reaches_the_sessions_own_formats() {
    let root = private_home();
    let mut session = seeded_session(root.path());
    let before = ui::format::region::current();

    // The person starts their week on Sunday and reads a 12-hour clock: the session's own
    // formatters — the week their desk's calendar starts on, the clocks its views write — follow
    // on a later pass, though the session started with Monday.
    choose("Sunday", "H12");
    when(&mut session, || {
        ui::format::region::current().first_day == ui::format::region::FirstDayOfWeek::Sunday
    });
    assert_eq!(
        ui::clock::hour_minute(14, 5),
        "2:05 PM",
        "their views write the clock the person reads"
    );
    assert_eq!(
        ui::format::first_weekday(),
        chrono::Weekday::Sun,
        "the week starts on the day the person chose"
    );

    // And back: their region is read again, not kept from the day the session started.
    choose("Monday", "H24");
    when(&mut session, || {
        ui::format::region::current().first_day == ui::format::region::FirstDayOfWeek::Monday
    });
    assert_eq!(ui::format::first_weekday(), chrono::Weekday::Mon);
    assert_eq!(ui::clock::hour_minute(14, 5), "14:05");
    ui::format::region::install(before);
}
