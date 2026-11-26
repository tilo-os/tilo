//! The person's region formats reach the session's own surfaces when they change them in
//! Settings: the first day of their week, their date and clock styles, which their shell's
//! calendar, their shell's views and the tilolang hosts the session runs write by
//! (`crate::formats`). A session that started with one region reads the next one on a pass of its
//! own loop, without a restart.

#![cfg(target_os = "linux")]

use std::time::Duration;

use tilo_session::{Options, Session};

/// A session with its shell on one display, and nothing published into it. What follows the
/// person's region is the session's own pass (`Session::follow_formats`): it reads their settings
/// when they changed and installs the formats its surfaces write by, whatever the session
/// publishes. The hosts it would tell besides are its launcher's (`Options::launch`), and no
/// session here has one, so the apps a development session installs and seeds are no part of
/// what is followed: none is built, installed or seeded for this.
fn session() -> Session {
    crate::common::headless(Options {
        person: crate::common::PERSON.to_owned(),
        collab: false,
        ..Options::default()
    })
}

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
    // The region is the person's settings, kept in a home of this test's own: what it chooses
    // here is never another test's region, nor this machine's.
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let mut session = session();
    let before = tilo_ui_base::format::region::current();
    assert_eq!(
        before.first_day,
        tilo_ui_base::format::region::FirstDayOfWeek::Monday,
        "a home that chose no region starts its week on Monday, so Sunday below is a change"
    );

    // The person starts their week on Sunday and reads a 12-hour clock: the session's own
    // formatters — the week their shell's calendar starts on, the clocks its views write — follow
    // on a later pass, though the session started with Monday.
    choose("Sunday", "H12");
    when(&mut session, || {
        tilo_ui_base::format::region::current().first_day == tilo_ui_base::format::region::FirstDayOfWeek::Sunday
    });
    assert_eq!(
        tilo_ui_base::clock::hour_minute(14, 5),
        "2:05 PM",
        "their views write the clock the person reads"
    );
    assert_eq!(
        tilo_ui_base::format::first_weekday(),
        jiff::civil::Weekday::Sunday,
        "the week starts on the day the person chose"
    );

    // And back: their region is read again, not kept from the day the session started.
    choose("Monday", "H24");
    when(&mut session, || {
        tilo_ui_base::format::region::current().first_day == tilo_ui_base::format::region::FirstDayOfWeek::Monday
    });
    assert_eq!(tilo_ui_base::format::first_weekday(), jiff::civil::Weekday::Monday);
    assert_eq!(tilo_ui_base::clock::hour_minute(14, 5), "14:05");
    tilo_ui_base::format::region::install(before);
}
