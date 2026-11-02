//! One switcher: the tilolang shell's. The desk chrome draws no Alt+Tab panel of its own and
//! keeps no slot for one, so what a person sees when they hold Alt is the shell's view alone.

#![allow(clippy::panic, reason = "a test's assertions")]

#[test]
fn the_desk_chrome_draws_no_switcher_of_its_own() {
    let host = include_str!("../src/desktop_host.rs");
    for gone in [
        "WindowSwitcher",
        "SwitcherOutcome",
        "SwitcherScope",
        "SwitcherPictures",
        "switcher_pictures",
        "chrome-window-switcher",
    ] {
        assert!(
            !host.contains(gone),
            "the desk chrome no longer draws a switcher, so its host names no {gone}"
        );
    }
    assert!(
        !include_str!("../src/lib.rs").contains("switcher_windows"),
        "the desk chrome keeps no windows of its own to draw a switcher from"
    );
    assert!(
        tilo_desktop::overlays::Slot::named("switcher").is_none(),
        "the switcher is the shell's surface, not one of the desk chrome's slots"
    );
}
