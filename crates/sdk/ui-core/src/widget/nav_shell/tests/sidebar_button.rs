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
/// screen, in front then, holds the way back to the screen it covered, and the button stands
/// aside for it.
#[test]
fn a_sidebar_screen_holds_the_way_back_and_the_place_before_it_the_places() {
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
        toolbar.sidebar, None,
        "the sidebar's own screen holds the way back instead: {toolbar:?}"
    );
    assert_eq!(
        toolbar.leading.len(),
        1,
        "one leading control, its way back: {toolbar:?}"
    );
    assert!(h.root_mut::<NavShell>().window_toolbar_event(
        &crate::window_toolbar::ToolbarEvent::Action {
            id: WINDOW_BACK.into(),
        }
    ));
    settle(&mut h, 400, 800);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Content,
        "Back returns to the place the sidebar covered"
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

/// A narrow window the desktop's bar hosts, showing a page beside its places: the places' own
/// screen holds the way back to the page, and the page holds the sidebar button in the row that
/// is the window's bar, so the places are never a dead end — as where the app puts the button
/// among its bar's slots. The same shell out of a desktop window drills from its places to the
/// page and back, its places the first screen.
#[test]
fn a_hosted_narrow_windows_places_lead_back_to_the_page() {
    let shell = || {
        NavShell::new()
            .content_kind(ContentKind::Page)
            .sidebar(Probe::boxed("sidebar").0)
            .content(Probe::boxed("content").0)
    };
    let mut h = harness(shell(), 400, 800, Theme::dark());
    h.present_in_window_bar();
    h.root_mut::<NavShell>().show(NavPane::Sidebar);
    settle(&mut h, 400, 800);
    assert!(h.row_holds_bar(), "the row is the window's bar");
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Screen);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);
    let back = geometry(&h).back;
    assert!(back.width() > 0.0, "the places' screen holds the way back");
    h.click_at(back.center().x, back.center().y);
    settle(&mut h, 400, 800);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Content,
        "Back goes forward to the page the places covered"
    );
    let toggle = geometry(&h).toggle;
    assert!(
        toggle.width() > 0.0,
        "the page's row holds the sidebar button"
    );
    assert_eq!(geometry(&h).back, Rect::ZERO, "and no Back to nothing");
    h.click_at(toggle.center().x, toggle.center().y);
    settle(&mut h, 400, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Sidebar);

    let mut own = harness(shell(), 400, 800, Theme::dark());
    own.root_mut::<NavShell>().show(NavPane::Sidebar);
    settle(&mut own, 400, 800);
    assert_eq!(own.root::<NavShell>().front_pane(), NavPane::Sidebar);
    assert_eq!(
        geometry(&own).back,
        Rect::ZERO,
        "out of a desktop window the places are the first screen"
    );
}
