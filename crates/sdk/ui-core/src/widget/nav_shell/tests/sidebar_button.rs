//! The window bar's sidebar button flips the sidebar on every click,
//! whatever opened it, where it docks, floats as a drawer or is a screen of
//! its own.

use super::*;
use crate::headless::SidebarShown;

/// Click the sidebar button the shell's header row holds three times in a window `width` wide
/// that the desktop's bar hosts; each click flips the sidebar, and the shell and its frame agree.
/// Returns where it stood before the first click.
fn three_clicks(shell: NavShell, width: u32, placement: SidebarPlacement) -> bool {
    let mut h = harness(shell, width, 800, Theme::dark());
    h.present_in_window_bar();
    settle(&mut h, width, 800);
    assert_eq!(geometry(&h).sidebar, placement, "{width} px wide");
    let first = h.root::<NavShell>().is_sidebar_open();
    let mut expected = first;
    for click in 0..3 {
        let toggle = geometry(&h).toggle;
        assert!(
            toggle.width() > 0.0,
            "{width} px wide, click {click}: the row holds the sidebar button"
        );
        h.click_at(toggle.center().x, toggle.center().y);
        settle(&mut h, width, 800);
        expected = !expected;
        let g = geometry(&h);
        assert_eq!(
            h.root::<NavShell>().is_sidebar_open(),
            expected,
            "{width} px wide: click {click} flips the sidebar"
        );
        assert_eq!(
            g.sidebar_column.width() > 0.5,
            expected,
            "{width} px wide: the frame shows what the shell holds after click {click}"
        );
    }
    first
}

#[test]
fn the_bars_sidebar_button_flips_a_docked_sidebar() {
    assert!(three_clicks(mail_shell(), 1280, SidebarPlacement::Docked));
}

/// A narrow window shows one screen at a time: the list the window opened on is a place, with
/// no way back, and holds the bar's button that brings the places over it; the sidebar's own
/// screen, in front then, holds the button that puts it away for the screen it left.
#[test]
fn a_sidebar_screen_holds_the_bar_button_and_the_place_before_it_its_way_to_the_places() {
    let mut h = harness(
        NavShell::new()
            .content_kind(ContentKind::List)
            .sidebar(Probe::boxed("sidebar").0)
            .content(Probe::boxed("content").0),
        400,
        800,
        Theme::dark(),
    );
    h.present_in_window_bar();
    settle(&mut h, 400, 800);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Screen);
    let toolbar = h.root::<NavShell>().window_toolbar();
    assert_eq!(
        toolbar.sidebar,
        Some(false),
        "the place the window opened on holds the way to the places: {toolbar:?}"
    );
    assert!(
        toolbar.leading.is_empty(),
        "and no Back, having nowhere to go back to: {toolbar:?}"
    );
    assert!(
        !h.root_mut::<NavShell>().back(),
        "Back leads nowhere from a place"
    );
    let toggle = geometry(&h).toggle;
    assert!(
        toggle.width() > 0.0,
        "the place's row holds the sidebar button"
    );
    h.click_at(toggle.center().x, toggle.center().y);
    settle(&mut h, 400, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    let toolbar = h.root::<NavShell>().window_toolbar();
    assert_eq!(
        toolbar.sidebar,
        Some(true),
        "the sidebar's own screen holds its button: {toolbar:?}"
    );
    assert!(
        toolbar.leading.is_empty(),
        "and is the first screen, with nowhere to go back to: {toolbar:?}"
    );
    assert!(
        h.root_mut::<NavShell>()
            .window_toolbar_event(&crate::window_toolbar::ToolbarEvent::ToggleSidebar)
    );
    settle(&mut h, 400, 800);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Content,
        "pressing it puts the sidebar away for the screen in front"
    );
}

/// With an overview, putting the sidebar's screen away goes back to the screen that was in
/// front.
#[test]
fn hiding_a_sidebar_screen_returns_to_the_screen_in_front() {
    let mut h = harness(mail_shell(), 560, 800, Theme::dark());
    h.present_in_window_bar();
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    h.root_mut::<NavShell>().show(NavPane::Sidebar);
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    assert!(
        h.root_mut::<NavShell>()
            .window_toolbar_event(&crate::window_toolbar::ToolbarEvent::ToggleSidebar)
    );
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
}

/// A document's sidebar floats over it as a drawer in a narrow window.
#[test]
fn the_bars_sidebar_button_flips_a_sidebar_drawer() {
    let shell = NavShell::new()
        .content_kind(ContentKind::Document)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0);
    assert!(!three_clicks(shell, 400, SidebarPlacement::Drawer));
}

#[test]
fn agreement_needs_the_bar_the_shell_and_the_frame() {
    let state = |bar, shell, frame| SidebarShown { bar, shell, frame };
    assert_eq!(state(true, true, true).agreed(), Some(true));
    assert_eq!(state(false, false, false).agreed(), Some(false));
    assert_eq!(state(true, false, false).agreed(), None);
    assert_eq!(state(false, false, true).agreed(), None);
}
