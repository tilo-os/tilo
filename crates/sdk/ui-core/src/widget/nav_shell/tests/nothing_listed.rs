//! The window's empty state ([`NavShell::nothing_listed`]).

use super::*;

const W: u32 = 1280;
const H: u32 = 800;

/// While there is nothing to list, the empty state stands where the
/// overview and the content stand, the sidebar beside it; once cleared the
/// list and the content come back, split as before, and the app's own
/// widgets are the ones it gave.
#[test]
fn the_empty_state_owns_the_content_area_until_something_is_listed() {
    let shell = mail_shell().nothing_listed(Probe::boxed("empty").0);
    let mut h = harness(shell, W, H, Theme::dark());
    settle(&mut h, W, H);
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Docked, "the sidebar stays");
    assert_eq!(g.overview, OverviewPlacement::None, "no list column");
    let empty = child_rect(&h, "empty").expect("the empty state is shown");
    assert!(child_rect(&h, "overview").is_none() && child_rect(&h, "content").is_none());
    assert!(
        empty.x0 <= g.sidebar_column.x1 + 1.0 && empty.x1 >= f64::from(W) - 1.0,
        "it spans everything beside the sidebar: {empty:?}"
    );
    assert!(
        h.root_mut::<NavShell>()
            .content_mut::<Probe>()
            .is_some_and(|probe| probe.id == "content"),
        "the app's content is held, not dropped"
    );

    h.root_mut::<NavShell>().set_nothing_listed(None);
    settle(&mut h, W, H);
    let g = geometry(&h);
    assert_eq!(g.overview, OverviewPlacement::List, "the split comes back");
    assert!(child_rect(&h, "empty").is_none());
    let (overview, content) = (
        child_rect(&h, "overview").expect("the list is back"),
        child_rect(&h, "content").expect("the content is back"),
    );
    assert!(overview.x1 <= content.x0 + 1.0, "side by side");
}

/// A swap of the content while the empty state shows lands behind it, and
/// is what comes back.
#[test]
fn a_swap_behind_the_empty_state_is_what_comes_back() {
    let shell = mail_shell().nothing_listed(Probe::boxed("empty").0);
    let mut h = harness(shell, W, H, Theme::dark());
    settle(&mut h, W, H);
    h.root_mut::<NavShell>()
        .set_content(Probe::boxed("reading").0);
    settle(&mut h, W, H);
    assert!(child_rect(&h, "empty").is_some() && child_rect(&h, "reading").is_none());
    h.root_mut::<NavShell>().set_nothing_listed(None);
    settle(&mut h, W, H);
    assert!(
        child_rect(&h, "reading").is_some(),
        "the swapped content shows"
    );
    assert!(child_rect(&h, "empty").is_none());
}

/// In a phone-wide window of a list and its content, the empty state is
/// the one screen, with no list to go back to.
#[test]
fn a_narrow_window_shows_the_empty_state_alone() {
    const NARROW: u32 = 420;
    let shell = NavShell::new()
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .nothing_listed(Probe::boxed("empty").0);
    let mut h = harness(shell, NARROW, H, Theme::dark());
    settle(&mut h, NARROW, H);
    let empty = child_rect(&h, "empty").expect("the empty state is shown");
    assert!(
        empty.width() >= f64::from(NARROW) - 1.0,
        "full width: {empty:?}"
    );
    assert!(!geometry(&h).stack.contains(&NavPane::Overview));
}

/// A phone-wide window with a sidebar whose toggle is in the window's bar,
/// as Passwords has: the empty state is the window's one screen, right
/// under the bar, with no Back and no header row above it, since there is
/// nothing to drill out of. The bar's toggle still reaches the sidebar.
#[test]
fn a_narrow_window_draws_no_back_over_the_empty_state() {
    const NARROW: u32 = 420;
    let mut shell = NavShell::new()
        .with_header_in_window_bar()
        .sidebar(Probe::boxed("sidebar").0)
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .nothing_listed(Probe::boxed("empty").0);
    shell.set_sidebar_toggle_in_window_bar(true);
    let mut h = harness(shell, NARROW, H, Theme::dark());
    settle(&mut h, NARROW, H);
    let g = geometry(&h);
    assert_eq!(g.sidebar, SidebarPlacement::Screen);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Content,
        "it opens on the empty state, not the sidebar"
    );
    assert_eq!(g.back, Rect::ZERO, "no Back chip");
    assert_eq!(g.back_column, None, "Back goes nowhere");
    assert_eq!(g.header_height, 0.0, "no header row");
    assert!(g.headers.iter().all(|header| *header == Rect::ZERO));
    let back = h
        .root::<NavShell>()
        .part(Part::Back)
        .map_or(Rect::ZERO, Widget::get_rect);
    assert!(back.width() <= 0.0, "no Back part shown: {back:?}");
    let empty = child_rect(&h, "empty").expect("the empty state is shown");
    assert!(
        empty.y0.abs() < 1e-9 && empty.width() >= f64::from(NARROW) - 1.0,
        "it starts right under the bar, full width: {empty:?}"
    );
    assert!(!h.root::<NavShell>().can_go_back());
    assert!(!h.root_mut::<NavShell>().back(), "Back goes nowhere");

    h.root_mut::<NavShell>().toggle_sidebar();
    settle(&mut h, NARROW, H);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Sidebar,
        "the bar's toggle reaches the sidebar"
    );
}
