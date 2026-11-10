#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

use super::tests_support::{alt, grave, key, part_with, reading_with_windows, tab};
use tilo_compositor::SurfaceHost as _;
use tilo_session_core::surface::KeyClaim;

#[test]
fn alt_tab_asks_for_the_switcher_from_rest() {
    let (mut part, link) = part_with(reading_with_windows());
    assert_ne!(
        part.claim_key(&tab(true, true), false, false),
        KeyClaim::Pass
    );
    part.key(&tab(true, true));
    assert_eq!(
        link.asked.borrow().front().map(|ask| ask.action.clone()),
        Some("walk-switcher".to_owned()),
        "Alt+Tab asks the desk for its switcher"
    );
    // Holding Alt and pressing Tab again walks the switcher on.
    link.asked.borrow_mut().clear();
    part.key(&tab(true, false));
    assert_eq!(
        link.asked.borrow().front().map(|ask| ask.action.clone()),
        Some("walk-switcher".to_owned())
    );
    assert_eq!(
        link.asked.borrow().front().map(|ask| ask.arguments.clone()),
        Some(std::collections::BTreeMap::from([
            ("steps".to_owned(), "1".to_owned()),
            ("scope".to_owned(), "workspace".to_owned()),
        ]))
    );
}

#[test]
fn alt_grave_asks_for_the_walk_over_this_apps_windows() {
    let (mut part, link) = part_with(reading_with_windows());
    part.key(&grave(false));
    assert_eq!(
        link.asked.borrow().front().map(|ask| ask.arguments.clone()),
        Some(std::collections::BTreeMap::from([
            ("steps".to_owned(), "1".to_owned()),
            ("scope".to_owned(), "app".to_owned()),
        ])),
        "Alt and the grave key walk the app in front's windows"
    );
    // Shift walks the other way, and stays in the app.
    link.asked.borrow_mut().clear();
    part.key(&grave(true));
    assert_eq!(
        link.asked.borrow().front().map(|ask| ask.arguments.clone()),
        Some(std::collections::BTreeMap::from([
            ("steps".to_owned(), "-1".to_owned()),
            ("scope".to_owned(), "app".to_owned()),
        ]))
    );
}

#[test]
fn letting_alt_go_commits_the_switcher() {
    let mut reading = reading_with_windows();
    reading.here = "switcher".to_owned();
    reading.switcher_revealed = true;
    let (mut part, link) = part_with(reading);
    link.asked.borrow_mut().clear();
    assert_ne!(part.claim_key(&alt(false), false, false), KeyClaim::Pass);
    part.key(&alt(false));
    assert_eq!(
        link.asked.borrow().front().map(|ask| ask.action.clone()),
        Some("commit-switcher".to_owned()),
        "letting Alt go goes to the window the walk reached"
    );
}

#[test]
fn escape_closes_the_surface_that_is_up() {
    let mut reading = super::tests_support::reading_with_windows();
    reading.here = "overview".to_owned();
    let (mut part, link) = part_with(reading);
    assert_ne!(part.claim_key(&escape(), false, false), KeyClaim::Pass);
    part.key(&escape());
    assert_eq!(
        link.asked.borrow().front().map(|ask| ask.action.clone()),
        Some("close-surface".to_owned())
    );
    assert_eq!(
        link.asked.borrow().front().map(|ask| ask.arguments.clone()),
        Some(std::collections::BTreeMap::from([(
            "surface".to_owned(),
            "overview".to_owned()
        )]))
    );
}

#[test]
fn a_surface_up_makes_the_part_cover_the_outputs() {
    let mut reading = super::tests_support::reading_with_windows();
    reading.here = "launcher".to_owned();
    let (part, _link) = part_with(reading);
    assert_eq!(
        part.widgets.placement().size,
        (1280, 800),
        "the launcher has the desk's room"
    );
    assert!(
        part.accepts_pointer(640.0, 400.0),
        "the launcher takes presses"
    );
}

fn escape() -> tilo_session_core::surface::HostKey {
    key(0xff1b, true)
}
