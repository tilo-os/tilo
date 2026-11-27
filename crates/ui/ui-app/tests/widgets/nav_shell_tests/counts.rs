//! A count says something wants attention: zero draws nothing, and a column's
//! header leaves the number to the place beside it that already shows it.

use super::*;
use tilo_ui::widget::{NavItem, NavList, NavSection};

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
        .overview(TestLeaf::fill("overview").boxed().0)
        .content(TestLeaf::fill("content").boxed().0)
        .sidebar_header("Places", Vec::new())
        .overview_header("Inbox", Vec::new())
        .overview_count(usize::try_from(count).expect("a count that fits"))
}

fn header_count(h: &Driver) -> Option<String> {
    header_part(h).and_then(|title| title.value)
}

fn header_part(h: &Driver) -> Option<tilo_ui_base::a11y::AccessNode> {
    h.root::<NavShell>()
        .test_part(Part::Title(Column::Overview))
        .and_then(Widget::accessibility)
}

/// What the sidebar draws beside `place`, as a reader hears it.
fn place_count(h: &Driver, place: &str) -> Option<String> {
    fn find<'a>(
        node: &'a tilo_ui_base::a11y::AccessNode,
        name: &str,
    ) -> Option<&'a tilo_ui_base::a11y::AccessNode> {
        if node.name.as_deref() == Some(name) {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, name))
    }
    let list = h.root::<NavShell>().sidebar_ref::<NavList>()?;
    let node = list.accessibility()?;
    find(&node, place)?.value.clone()
}

fn header_width(h: &Driver) -> f64 {
    h.root::<NavShell>()
        .test_part(Part::Title(Column::Overview))
        .map(Widget::get_rect)
        .map(|rect| rect.width())
        .expect("the overview's title")
}

tilo_test_env::scenario! {
    fn a_place_s_count_shows_once_between_the_row_and_the_header;

    // A place's count is shown once: the sidebar beside the content shows it, so the header leaves
    // the number to the place.
    phase a_place_and_the_header_beside_it_show_its_count_once;
    {
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




    // The header says the count wherever no place beside it does: a narrow window whose list is
    // the screen, a sidebar narrowed to its rail, and a number the place does not show.
    phase the_header_says_the_count_no_place_beside_it_shows;
    {
        let mut h = harness(places_shell(6).content_kind(ContentKind::List), 400, 800, Theme::dark());
        settle(&mut h, 400, 800);
        assert_eq!(geometry(&h).sidebar, SidebarPlacement::Screen);
        assert_eq!(header_count(&h).as_deref(), Some("6"), "a narrow window");

        let rail = NavList::new(vec![NavSection::untitled(
            "places",
            vec![NavItem::new("inbox", "Inbox").count(6)],
        )])
        .with_id("places")
        .selected("inbox")
        .rail();
        let shell = NavShell::new()
            .sidebar(Box::new(rail))
            .overview(TestLeaf::fill("overview").boxed().0)
            .content(TestLeaf::fill("content").boxed().0)
            .overview_header("Inbox", Vec::new())
            .overview_count(6);
        let mut h = harness(shell, 1280, 800, Theme::dark());
        h.root_mut::<NavShell>().stage_sidebar_width(64.0);
        settle(&mut h, 1280, 800);
        assert!(
            h.root::<NavShell>()
                .sidebar_ref::<NavList>()
                .is_some_and(NavList::is_rail)
        );
        assert_eq!(header_count(&h).as_deref(), Some("6"), "beside a rail");

        let mut h = harness(places_shell(6), 1280, 800, Theme::dark());
        settle(&mut h, 1280, 800);
        h.root_mut::<NavShell>().set_overview_count(Some(7));
        settle(&mut h, 1280, 800);
        assert_eq!(place_count(&h, "Inbox").as_deref(), Some("6"));
        assert_eq!(header_count(&h).as_deref(), Some("7"), "a number the place does not show");
    }

    // Zero is never drawn: the header shows no badge and keeps the width one would
    // have taken, and the place's row says nothing.
    phase zero_draws_no_count_and_keeps_no_area_for_one;
    {
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
            "no area is kept for a count that is not drawn: {} against {plain}",
            header_width(&h)
        );
    }
}
