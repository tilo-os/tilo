//! An app that paints its own window inside the shell's content column
//! keeps the shell's sidebar through a [`NavFeed`] and puts the shell's own
//! buttons in the window's bar through a [`WindowBarLink`].

use super::*;
use crate::widget::{NavFeed, NavItem, NavList, NavSection};
use crate::window_toolbar::ToolbarEvent;

fn places() -> Vec<NavSection> {
    let item = |id: &str, label: &str, icon: Icon| NavItem::new(id, label).icon(icon);
    vec![NavSection::untitled(
        "places",
        vec![
            item("home", "Home", Icon::Home),
            item("trending", "Trending", Icon::Flame),
        ],
    )]
}

const W: u32 = 1400;
const H: u32 = 800;

fn hosted() -> (FrameHarness, NavFeed, WindowBarLink) {
    let feed = NavFeed::new();
    let shell = NavShell::new()
        .with_id("hosted")
        .with_window_toolbar()
        .content_kind(ContentKind::Grid);
    let link = shell.window_bar_link();
    let shell = shell
        .sidebar(Box::new(
            NavList::new(Vec::new()).with_id("places").fed_by(&feed),
        ))
        .content(Probe::boxed("content").0);
    feed.show(places(), Some("home".into()));
    let mut h = harness(shell, W, H, Theme::dark());
    settle(&mut h, W, H);
    (h, feed, link)
}

/// The app shows its places through the feed; a row the person chooses
/// comes back to the app as that place's id.
#[test]
fn the_feed_shows_the_apps_places_and_hands_back_the_persons_choice() {
    let (mut h, feed, _) = hosted();
    let list = h
        .root::<NavShell>()
        .sidebar_ref::<NavList>()
        .expect("the sidebar is the list");
    assert_eq!(list.selected_id().as_deref(), Some("home"));
    let row = list.row_rect("trending").expect("the app's place is a row");
    h.click_at(row.center().x, row.center().y);
    assert_eq!(feed.take_chosen(), vec!["trending".to_owned()]);
    assert!(feed.take_chosen().is_empty(), "each choice comes back once");
}

/// The shell's own buttons are the app's slots in the window's bar: the
/// sidebar's toggle shows where the sidebar stands, and pressing it in the
/// bar hides and shows the sidebar. What is not the shell's stays the
/// app's.
#[test]
fn the_link_puts_the_shells_toggle_in_the_apps_window_bar() {
    let (mut h, _, link) = hosted();
    assert_eq!(link.slots().sidebar, Some(true), "docked and shown");
    assert!(link.event(&ToolbarEvent::ToggleSidebar));
    settle(&mut h, W, H);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    assert_eq!(link.slots().sidebar, Some(false));
    assert!(!link.is_sidebar_open());
    assert!(!link.event(&ToolbarEvent::Action { id: "app".into() }));
    assert!(link.event(&ToolbarEvent::ToggleSidebar));
    settle(&mut h, W, H);
    assert!(h.root::<NavShell>().is_sidebar_open());
}

/// A narrow window whose toolbar is the window bar's slots opens on its
/// list, as one with a header row does: the list fills the window, and Back
/// from an opened item comes back to it.
#[test]
fn a_narrow_window_with_its_toolbar_in_the_bar_opens_on_its_list() {
    let shell = NavShell::new()
        .sidebar(Probe::boxed("sidebar").0)
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .with_window_toolbar();
    let mut h = harness(shell, 560, 800, Theme::dark());
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    let list = child_rect(&h, "overview").expect("the list is a column");
    assert!(
        list.width() >= 500.0 && list.height() > 0.0,
        "the list fills the window: {list:?}"
    );
    h.root_mut::<NavShell>().show(NavPane::Content);
    settle(&mut h, 560, 800);
    assert!(h.root_mut::<NavShell>().back());
    settle(&mut h, 560, 800);
    assert_eq!(h.root::<NavShell>().front_pane(), NavPane::Overview);
    let list = child_rect(&h, "overview").expect("the list is a column");
    assert!(list.width() >= 500.0, "back on the list: {list:?}");
}
