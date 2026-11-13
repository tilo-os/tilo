//! A window with nothing to show beside its list yet ([`NavShell::content_shown`]).

use super::*;

const W: u32 = 1280;
const H: u32 = 800;

/// Until something is shown, the list lists for the whole window: no content column stands
/// beside it, and its rows run to the window's trailing edge.
#[test]
fn the_list_lists_for_the_window_until_something_is_shown() {
    let shell = mail_shell().content_shown(false);
    let mut h = harness(shell, W, H, Theme::dark());
    settle(&mut h, W, H);
    assert_eq!(geometry(&h).overview, OverviewPlacement::List);
    assert_eq!(
        geometry(&h).content_column,
        Rect::ZERO,
        "no content column stands beside the list"
    );
    let list = child_rect(&h, "overview").expect("the list shows");
    assert!(
        child_rect(&h, "content").is_none(),
        "the content is laid out nowhere"
    );
    assert!(
        (list.x1 - f64::from(W)).abs() < 1.0,
        "the list runs to the window's edge: {list:?}"
    );

    h.root_mut::<NavShell>().set_content_shown(true);
    settle(&mut h, W, H);
    let (list, content) = (
        child_rect(&h, "overview").expect("the list"),
        child_rect(&h, "content").expect("the content"),
    );
    assert!(list.x1 <= content.x0 + 1.0, "side by side");
    assert!((content.x1 - f64::from(W)).abs() < 1.0, "{content:?}");

    h.root_mut::<NavShell>().set_content_shown(false);
    settle(&mut h, W, H);
    assert!(
        child_rect(&h, "content").is_none(),
        "the room goes back to the list"
    );
}

/// A phone-shaped window lands on the list: it is the screen the window opens on, with nothing
/// to go back to, and the places stand a toggle away behind it.
#[test]
fn a_phone_shows_its_list_with_the_places_a_toggle_away() {
    const NARROW: u32 = 390;
    let shell = mail_shell().content_shown(false);
    let mut h = harness(shell, NARROW, H, Theme::dark());
    settle(&mut h, NARROW, H);
    let g = geometry(&h);
    assert_eq!(g.stack, [NavPane::Sidebar, NavPane::Overview]);
    let list = child_rect(&h, "overview").expect("the list is the screen");
    assert!((list.width() - f64::from(NARROW)).abs() < 1.0, "{list:?}");
    assert!(child_rect(&h, "content").is_none());
    assert!(
        child_rect(&h, "sidebar").is_none(),
        "the sidebar waits behind"
    );
    assert_eq!(g.back, Rect::ZERO, "the list is where the window opened");
    assert!(
        g.toggle.width() > 0.0,
        "it holds the way to the places, which come back to it"
    );

    h.root_mut::<NavShell>().show(NavPane::Overview);
    settle(&mut h, NARROW, H);
    assert_eq!(
        h.root::<NavShell>().front_pane(),
        NavPane::Overview,
        "the list stays in front rather than a content screen that is not there"
    );
}
