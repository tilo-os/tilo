//! A count says something wants attention: zero draws nothing, and a column's
//! header leaves the number to the place beside it that already shows it.

use super::*;
use crate::widget::{NavItem, NavList, NavSection};

/// A shell whose sidebar lists places, `inbox` chosen and holding `count`
/// things, and whose overview header is that same place.
fn places_shell(count: u32) -> NavShell {
    let places = NavList::new(vec![NavSection::untitled(
        "places",
        vec![
            NavItem::new("inbox", "Inbox").count(count),
            NavItem::new("done", "Done"),
        ],
    )])
    .with_id("places")
    .selected("inbox");
    NavShell::new()
        .sidebar(Box::new(places))
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .sidebar_header("Places", Vec::new())
        .overview_header("Inbox", Vec::new())
        .overview_count(usize::try_from(count).expect("a count that fits"))
}

fn header_count(h: &FrameHarness) -> Option<String> {
    header_part(h).and_then(|title| title.value)
}

fn header_part(h: &FrameHarness) -> Option<crate::a11y::AccessNode> {
    h.root::<NavShell>()
        .part(Part::Title(Column::Overview))
        .and_then(Widget::accessibility)
}

/// What the sidebar draws beside `place`, as a reader hears it.
fn place_count(h: &FrameHarness, place: &str) -> Option<String> {
    fn find<'a>(
        node: &'a crate::a11y::AccessNode,
        name: &str,
    ) -> Option<&'a crate::a11y::AccessNode> {
        if node.name.as_deref() == Some(name) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, name))
    }
    let list = h.root::<NavShell>().sidebar_ref::<NavList>()?;
    let node = list.accessibility()?;
    find(&node, place)?.value.clone()
}

fn header_width(h: &FrameHarness) -> f64 {
    h.root::<NavShell>()
        .part(Part::Title(Column::Overview))
        .map(Widget::get_rect)
        .map(|rect| rect.width())
        .expect("the overview's title")
}

/// A place's count is shown once: the sidebar beside the content shows it, so the header leaves
/// the number to the place.
#[test]
fn a_place_and_the_header_beside_it_show_its_count_once() {
    let mut h = harness(places_shell(6), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Docked);
    assert_eq!(place_count(&h, "Inbox").as_deref(), Some("6"));
    assert_eq!(header_count(&h), None, "the sidebar beside it says 6");

    h.root_mut::<NavShell>().close_sidebar();
    settle(&mut h, 1280, 800);
    assert!(!h.root::<NavShell>().is_sidebar_open());
    assert_eq!(
        header_count(&h).as_deref(),
        Some("6"),
        "with the sidebar put away the header is the only place left to say it"
    );

    h.root_mut::<NavShell>().open_sidebar();
    settle(&mut h, 1280, 800);
    assert_eq!(header_count(&h), None);
    assert_eq!(place_count(&h, "Inbox").as_deref(), Some("6"));
}

/// A narrow window shows the list as the screen: no sidebar stands beside the
/// header, so the header keeps the count.
#[test]
fn a_narrow_window_keeps_the_count_in_the_header() {
    let shell = places_shell(6).content_kind(ContentKind::List);
    let mut h = harness(shell, 400, 800, Theme::dark());
    settle(&mut h, 400, 800);
    assert_eq!(geometry(&h).sidebar, SidebarPlacement::Screen);
    assert_eq!(header_count(&h).as_deref(), Some("6"));
}

#[test]
fn a_rail_leaves_the_number_to_the_header() {
    let places = NavList::new(vec![NavSection::untitled(
        "places",
        vec![NavItem::new("inbox", "Inbox").count(6)],
    )])
    .with_id("places")
    .selected("inbox")
    .rail();
    let shell = NavShell::new()
        .sidebar(Box::new(places))
        .overview(Probe::boxed("overview").0)
        .content(Probe::boxed("content").0)
        .overview_header("Inbox", Vec::new())
        .overview_count(6);
    let mut h = harness(shell, 1280, 800, Theme::dark());
    h.root_mut::<NavShell>().stage_sidebar_width(64.0);
    settle(&mut h, 1280, 800);
    assert!(
        h.root::<NavShell>()
            .sidebar_ref::<NavList>()
            .expect("the sidebar is the list")
            .is_rail()
    );
    assert_eq!(header_count(&h).as_deref(), Some("6"));
}

/// The header leaves only the count the place already shows: a number the
/// place does not show is the header's to say.
#[test]
fn a_header_count_the_place_does_not_show_stays() {
    let mut h = harness(places_shell(6), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    h.root_mut::<NavShell>().set_overview_count(Some(7));
    settle(&mut h, 1280, 800);
    assert_eq!(place_count(&h, "Inbox").as_deref(), Some("6"));
    assert_eq!(header_count(&h).as_deref(), Some("7"));
}

/// Zero is never drawn: the header shows no badge and keeps the room one would
/// have taken, and the place's row says nothing.
#[test]
fn zero_draws_no_count_and_keeps_no_room_for_one() {
    let mut h = harness(places_shell(0), 1280, 800, Theme::dark());
    settle(&mut h, 1280, 800);
    assert_eq!(place_count(&h, "Inbox"), None, "nothing wants attention");
    assert_eq!(header_count(&h), None);

    let plain = header_width(&h);
    h.root_mut::<NavShell>().set_overview_count(Some(0));
    settle(&mut h, 1280, 800);
    assert_eq!(header_count(&h), None);
    assert!(
        (header_width(&h) - plain).abs() < 0.5,
        "no room is kept for a count that is not drawn: {} against {plain}",
        header_width(&h)
    );
}
