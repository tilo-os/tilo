//! The window bar's sidebar button flips the sidebar on every click,
//! whatever opened it, where it docks, floats as a drawer or is a screen of
//! its own.

use super::*;
use crate::headless::SidebarShown;

/// Click the bar's sidebar button three times in a window `width` wide and
/// check that each click flipped the sidebar, on the bar, in the shell and
/// on screen alike. Returns where it stood before the first click.
fn three_clicks(shell: NavShell, width: u32, placement: SidebarPlacement) -> bool {
    let mut h = harness(shell, width, 800, Theme::dark());
    h.present_in_window_bar();
    settle(&mut h, width, 800);
    assert_eq!(geometry(&h).sidebar, placement, "{width} px wide");
    let states = h.press_window_sidebar_button(3).expect("the bar's button");
    let shown: Vec<_> = states.iter().map(|state| state.agreed()).collect();
    let first = shown[0].unwrap_or_else(|| panic!("{width} px: {:?}", states[0]));
    assert_eq!(
        shown,
        [Some(first), Some(!first), Some(first), Some(!first)],
        "{width} px wide ({placement:?}): {states:?}"
    );
    first
}

#[test]
fn the_bars_sidebar_button_flips_a_docked_sidebar() {
    assert!(three_clicks(mail_shell(), 1280, SidebarPlacement::Docked));
}

/// A narrow window shows one screen at a time: the button brings the
/// sidebar's screen to the front, and puts it away again for the screen
/// the person left.
#[test]
fn the_bars_sidebar_button_flips_a_sidebar_screen() {
    let shell = NavShell::new()
        .content_kind(ContentKind::List)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0);
    assert!(!three_clicks(shell, 560, SidebarPlacement::Screen));
}

/// With an overview, putting the sidebar's screen away goes back to the
/// screen that was in front.
#[test]
fn hiding_a_sidebar_screen_returns_to_the_screen_in_front() {
    let mut h = harness(mail_shell(), 560, 800, Theme::dark());
    h.present_in_window_bar();
    settle(&mut h, 560, 800);
    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, 560, 800);
    let states = h.press_window_sidebar_button(2).expect("the bar's button");
    assert_eq!(
        states.iter().map(|s| s.agreed()).collect::<Vec<_>>(),
        [Some(false), Some(true), Some(false)],
        "{states:?}"
    );
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Content);
}

/// A document's sidebar floats over it as a drawer in a narrow window.
#[test]
fn the_bars_sidebar_button_flips_a_sidebar_drawer() {
    let shell = NavShell::new()
        .content_kind(ContentKind::Document)
        .sidebar(Probe::boxed("sidebar").0)
        .content(Probe::boxed("content").0);
    assert!(!three_clicks(shell, 560, SidebarPlacement::Drawer));
}

#[test]
fn agreement_needs_the_bar_the_shell_and_the_frame() {
    let state = |bar, shell, frame| SidebarShown { bar, shell, frame };
    assert_eq!(state(true, true, true).agreed(), Some(true));
    assert_eq!(state(false, false, false).agreed(), Some(false));
    assert_eq!(state(true, false, false).agreed(), None);
    assert_eq!(state(false, false, true).agreed(), None);
}
