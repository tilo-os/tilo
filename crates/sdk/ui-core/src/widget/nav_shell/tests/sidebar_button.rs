//! The window bar's sidebar button flips the sidebar on every click,
//! whatever opened it, where it docks, floats as a drawer or is a screen of
//! its own.

use super::*;
use crate::headless::SidebarShown;

/// Click the bar's sidebar button three times in a window `width` wide;
/// each click flips the sidebar ([`FrameHarness::check_window_sidebar_button`]).
/// Returns where it stood before the first click.
fn three_clicks(shell: NavShell, width: u32, placement: SidebarPlacement) -> bool {
    let mut h = harness(shell, width, 800, Theme::dark());
    h.present_in_window_bar();
    settle(&mut h, width, 800);
    assert_eq!(geometry(&h).sidebar, placement, "{width} px wide");
    h.check_window_sidebar_button()
        .unwrap_or_else(|error| panic!("{width} px wide ({placement:?}): {error}"))
}

#[test]
fn the_bars_sidebar_button_flips_a_docked_sidebar() {
    assert!(three_clicks(mail_shell(), 1280, SidebarPlacement::Docked));
}

/// A narrow window shows one screen at a time: the screen the person left has its way back
/// alone, and the sidebar's own screen, behind it, holds the bar's button that puts it away
/// for the screen in front.
#[test]
fn a_sidebar_screen_holds_the_bar_button_and_the_screen_before_it_its_way_back() {
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
    assert!(
        toolbar.sidebar.is_none(),
        "the screen the person left keeps Back alone: {toolbar:?}"
    );
    assert!(
        h.window_sidebar_shown().is_err(),
        "and the bar holds no sidebar button there"
    );
    assert!(h.root_mut::<NavShell>().back());
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
    h.click_window_sidebar_button()
        .expect("the sidebar screen's button");
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
    h.click_window_sidebar_button()
        .expect("the sidebar screen's button");
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
