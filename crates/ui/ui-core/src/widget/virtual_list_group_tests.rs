use super::*;
use std::cell::Cell;
use std::rc::Rc;

use crate::testing::Dispatch;
use crate::theme::Theme;
use crate::widget::section::band_height;

struct Grouped {
    groups: Vec<Option<RowGroup>>,
    finished: Vec<usize>,
    asked: Rc<Cell<usize>>,
}

impl Grouped {
    fn new(groups: Vec<Option<RowGroup>>) -> Self {
        Self {
            groups,
            finished: Vec::new(),
            asked: Rc::new(Cell::new(0)),
        }
    }

    fn finished(mut self, rows: &[usize]) -> Self {
        self.finished = rows.to_vec();
        self
    }
}

impl ListModel for Grouped {
    type Key = u64;

    fn len(&self) -> usize {
        self.groups.len()
    }

    fn key_at(&self, index: usize) -> Option<u64> {
        (index < self.groups.len()).then_some(index as u64)
    }

    fn index_of(&self, key: &u64) -> Option<usize> {
        usize::try_from(*key)
            .ok()
            .filter(|index| *index < self.groups.len())
    }

    fn group_at(&self, index: usize) -> Option<RowGroup> {
        self.asked.set(self.asked.get() + 1);
        self.groups.get(index).cloned().flatten()
    }

    fn done(&self, index: usize) -> bool {
        self.finished.contains(&index)
    }

    fn discloses(&self) -> bool {
        self.groups.iter().any(Option::is_some)
    }
}

fn heading(name: &str) -> Option<RowGroup> {
    Some(RowGroup::heading(name.to_owned()))
}

const ROW: f32 = 40.0;

fn grouped(groups: Vec<Option<RowGroup>>) -> VirtualList<Grouped> {
    listed(Grouped::new(groups))
}

fn grouped_finished(groups: Vec<Option<RowGroup>>, finished: &[usize]) -> VirtualList<Grouped> {
    listed(Grouped::new(groups).finished(finished))
}

fn listed(model: Grouped) -> VirtualList<Grouped> {
    VirtualList::new(model, ROW, |_, index, _| {
        Box::new(super::tests::ProbeRow::new(
            format!("row {index}"),
            ROW,
            false,
            Rc::new(Cell::new(0)),
        )) as Element
    })
    .with_viewport_height(400.0)
}

fn week() -> VirtualList<Grouped> {
    grouped(vec![
        heading("Monday"),
        heading("Monday"),
        heading("Tuesday"),
        heading("Tuesday"),
        Some(RowGroup::Tail),
        Some(RowGroup::Tail),
    ])
    .with_tail_title("Done")
}

fn settle(list: &mut VirtualList<Grouped>) -> (TaffyTree<()>, NodeId) {
    let _ = super::tests::lay_out(list, 300.0);
    super::tests::lay_out(list, 300.0)
}

fn bands(list: &VirtualList<Grouped>) -> Vec<(String, f64, bool)> {
    list.groups
        .bands
        .iter()
        .map(|band| (band.heading.clone(), band.top, band.closed))
        .collect()
}

fn mounted_indexes(list: &VirtualList<Grouped>) -> Vec<usize> {
    list.mounted.iter().map(|row| row.index).collect()
}

fn press(list: &mut VirtualList<Grouped>, y: f64, taffy: &TaffyTree<()>, node: NodeId) -> bool {
    let at = crate::synthetic_input::cursor_moved(20.0, y);
    let hovered = Dispatch::new().layout(taffy, node).send(list, &at).handled;
    let mut handled = hovered;
    for state in [ElementState::Pressed, ElementState::Released] {
        let event = crate::synthetic_input::mouse_input(state, MouseButton::Left);
        handled |= Dispatch::new()
            .layout(taffy, node)
            .send(list, &event)
            .handled;
    }
    handled
}

tilo_test_env::scenario! {
    fn group_headings_head_fold_and_stay_over_their_rows;

    phase a_grouped_list_heads_each_run_and_opens_and_closes_them;
    {
        // Each run of rows under one heading is a band, and the done tail is folded.
        let mut list = week();
        let _ = settle(&mut list);
        let band = band_height(&Theme::dark());
        let row = f64::from(ROW);
        assert_eq!(
            bands(&list),
            vec![
                ("Monday".to_owned(), 0.0, false),
                ("Tuesday".to_owned(), 2.0 * row + band, false),
                ("Done".to_owned(), 4.0 * row + 2.0 * band, true),
            ]
        );
        let done = 4.0 * row + 2.0 * band;
        assert!((list.viewport.content_extent() - (done + band)).abs() < 0.5);
        let mounted = mounted_indexes(&list);
        assert!(
            !mounted.contains(&4) && !mounted.contains(&5),
            "no done row is built while folded: {mounted:?}"
        );

        // A press on the tail's heading opens it; a press on Monday's folds it.
        let (taffy, node) = settle(&mut list);
        assert!(press(&mut list, done + band / 2.0, &taffy, node));
        let _ = settle(&mut list);
        assert!(
            mounted_indexes(&list).contains(&4),
            "the tail opened: {:?}",
            mounted_indexes(&list)
        );
        let (taffy, node) = settle(&mut list);
        assert!(press(&mut list, band / 2.0, &taffy, node));
        let _ = settle(&mut list);
        assert!(
            bands(&list)
                .iter()
                .all(|(heading, _, closed)| heading != "Monday" || *closed),
            "Monday closed: {:?}",
            bands(&list)
        );
        assert!(!mounted_indexes(&list).contains(&0), "its rows let go");

        // Under a finger the band is tall enough to press, and pressing it folds.
        let mut list = week();
        list.set_interaction_environment(crate::interaction::InteractionEnvironment::TOUCH);
        let (taffy, node) = settle(&mut list);
        let target = f64::from(crate::interaction::InteractionEnvironment::TOUCH.minimum_target());
        assert!(
            list.groups.height >= target - 0.5,
            "the band is a target a finger presses: {} of {target}",
            list.groups.height
        );
        let middle = list.groups.height / 2.0;
        assert!(press(&mut list, middle, &taffy, node));
        let _ = settle(&mut list);
        assert!(
            bands(&list)
                .iter()
                .all(|(heading, _, closed)| heading != "Monday" || *closed),
            "Monday folded by the press: {:?}",
            bands(&list)
        );
    }

    phase the_heading_over_the_top_stays_there_while_its_group_scrolls;
    {
        let mut list = grouped(
            (0..30)
                .map(|index| heading(["Monday", "Tuesday", "Wednesday"][index / 10]))
                .collect(),
        );
        let _ = settle(&mut list);
        let inside = list.viewport.item_top(12).unwrap_or_default() + 5.0;
        list.set_scroll_offset(inside);
        // A scroll that moves without a layout holds the heading over its rows.
        let shown = list.bands_in_view();
        assert_eq!(
            shown
                .iter()
                .map(|band| (band.heading.as_str(), band.top))
                .collect::<Vec<_>>(),
            vec![("Tuesday", 0.0)],
            "the heading of the group under the top, held there once, over its rows"
        );
        // And it is still there once the layout catches up.
        let _ = settle(&mut list);
        let shown = list.bands_in_view();
        assert_eq!(
            shown.first().map(|band| (band.heading.as_str(), band.top)),
            Some(("Tuesday", 0.0))
        );
    }


    phase the_keyboard_walks_the_headings_and_folds_their_groups;
    {
        let mut list = week();
        let (taffy, node) = settle(&mut list);
        list.set_focus(true);
        assert_eq!(list.focused_key(), None);

        assert!(key(&mut list, NamedKey::ArrowDown, &taffy, node));
        assert_eq!(list.focused_key(), Some(&0));
        assert_eq!(focused_heading(&list), None);
        assert!(key(&mut list, NamedKey::ArrowUp, &taffy, node));
        assert_eq!(focused_heading(&list), Some("Monday"));
        assert_eq!(list.focused_key(), None, "a heading is not one of the rows");

        assert!(key(&mut list, NamedKey::ArrowDown, &taffy, node));
        assert_eq!(list.focused_key(), Some(&0));
        assert_eq!(focused_heading(&list), None);
        assert!(key(&mut list, NamedKey::ArrowUp, &taffy, node));
        assert_eq!(focused_heading(&list), Some("Monday"));

        assert!(key(&mut list, NamedKey::Enter, &taffy, node));
        let (taffy, node) = settle(&mut list);
        assert!(
            bands(&list)
                .iter()
                .all(|(heading, _, closed)| heading != "Monday" || *closed),
            "Monday folded: {:?}",
            bands(&list)
        );
        assert!(
            !mounted_indexes(&list).contains(&0) && !mounted_indexes(&list).contains(&1),
            "the folded rows let go: {:?}",
            mounted_indexes(&list)
        );
        assert!(key(&mut list, NamedKey::ArrowDown, &taffy, node));
        assert_eq!(focused_heading(&list), Some("Tuesday"));

        assert!(key(&mut list, NamedKey::ArrowUp, &taffy, node));
        assert_eq!(focused_heading(&list), Some("Monday"));
        assert!(key(&mut list, NamedKey::Space, &taffy, node));
        let (taffy, node) = settle(&mut list);
        assert!(
            bands(&list)
                .iter()
                .all(|(heading, _, closed)| heading != "Monday" || !*closed),
            "Monday unfolded: {:?}",
            bands(&list)
        );
        assert!(
            mounted_indexes(&list).contains(&0),
            "its rows come back: {:?}",
            mounted_indexes(&list)
        );
        assert!(key(&mut list, NamedKey::End, &taffy, node));
        assert_eq!(focused_heading(&list), Some("Done"));
    }
}

fn key(
    list: &mut VirtualList<Grouped>,
    key: NamedKey,
    taffy: &TaffyTree<()>,
    node: NodeId,
) -> bool {
    Dispatch::new()
        .layout(taffy, node)
        .send(list, &crate::synthetic_input::named_key(key))
        .handled
}

fn focused_heading(list: &VirtualList<Grouped>) -> Option<&str> {
    list.focused_group
        .as_ref()
        .map(|(heading, _)| heading.as_str())
}

tilo_test_env::scenario! {
    fn a_heading_counts_what_stands_under_it;

    phase a_heading_counts_the_rows_under_it_that_are_worth_counting;
    {
        // A heading counts the rows it heads; a group of one counts nothing.
        let mut list = grouped(vec![heading("Today"), None, heading("Tomorrow")]);
        let _ = settle(&mut list);
        let tree = Widget::accessibility(&list).expect("list semantics");
        let headings: Vec<_> = tree
            .children
            .iter()
            .filter(|child| child.role == crate::a11y::Role::Button)
            .map(|child| (child.name.clone().unwrap_or_default(), child.value.clone()))
            .collect();
        assert_eq!(
            headings,
            [
                ("Today".to_owned(), Some("2".to_owned())),
                ("Tomorrow".to_owned(), Some("1".to_owned()))
            ],
            "each heading counts the rows under it"
        );
        let mut one = grouped(vec![heading("Today")]);
        let _ = settle(&mut one);
        let band = one
            .groups
            .bands
            .first()
            .expect("the heading stands")
            .clone();
        assert_eq!(
            one.shown_heading_count(&band),
            None,
            "a group of one counts nothing"
        );
        let mut two = grouped(vec![heading("Today"), heading("Today")]);
        let _ = settle(&mut two);
        let band = two
            .groups
            .bands
            .first()
            .expect("the heading stands")
            .clone();
        assert_eq!(
            two.shown_heading_count(&band),
            Some(2),
            "two rows are a number worth reading"
        );

        // What is left to do is what a heading counts, and the tail counts what is done.
        let mut one = grouped_finished(vec![heading("Today"), heading("Today")], &[1]);
        assert_eq!(
            open_count(&mut one),
            None,
            "one task is left: the number would restate the row under it"
        );
        let mut two = grouped_finished(
            vec![heading("Today"), heading("Today"), heading("Today")],
            &[2],
        );
        assert_eq!(open_count(&mut two), Some(2), "two tasks are left to do");
        let mut week = listed(
            Grouped::new(vec![
                heading("Monday"),
                heading("Monday"),
                Some(RowGroup::Tail),
                Some(RowGroup::Tail),
            ])
            .finished(&[0, 2, 3]),
        )
        .with_tail_title("Done");
        let _ = settle(&mut week);
        let tail = week
            .groups
            .bands
            .iter()
            .find(|band| band.heading == "Done")
            .expect("the tail's heading stands")
            .clone();
        assert_eq!(
            week.shown_heading_count(&tail),
            Some(2),
            "the items done with are counted, both of them"
        );
        let monday = week
            .groups
            .bands
            .iter()
            .find(|band| band.heading == "Monday")
            .expect("Monday's heading stands")
            .clone();
        assert_eq!(
            week.shown_heading_count(&monday),
            None,
            "the one row left to do under Monday says itself"
        );

        // A reader hears the same number, with a finished row not read as one to do.
        let mut list = grouped_finished(
            vec![
                heading("Today"),
                heading("Today"),
                heading("Tomorrow"),
                None,
            ],
            &[0],
        );
        let _ = settle(&mut list);
        let tree = Widget::accessibility(&list).expect("list semantics");
        let headings: Vec<_> = tree
            .children
            .iter()
            .filter(|child| child.role == crate::a11y::Role::Button)
            .map(|child| (child.name.clone().unwrap_or_default(), child.value.clone()))
            .collect();
        assert_eq!(
            headings,
            [
                ("Today".to_owned(), Some("1".to_owned())),
                ("Tomorrow".to_owned(), Some("2".to_owned()))
            ],
            "the finished row is not read as one still to do, the row on its way is"
        );
    }
}

fn send(list: &mut VirtualList<Grouped>, target: &str, action: &crate::a11y::Action) -> bool {
    Widget::perform_semantic_action(
        list,
        target,
        action,
        &mut crate::widget::Edits::lend(&mut automerge::AutoCommit::new()),
        &automerge::ROOT,
    )
}

tilo_test_env::scenario! {
    fn a_heading_is_a_disclosure_a_reader_and_an_agent_can_use;

    phase a_heading_is_a_disclosure_a_reader_and_an_agent_can_use;
    {
        use accesskit::{Action as AkAction, Role as AkRole};
        let mut list = week();
        let (taffy, node) = settle(&mut list);
        list.set_focus(true);
        assert!(key(&mut list, NamedKey::ArrowDown, &taffy, node));
        assert!(key(&mut list, NamedKey::ArrowUp, &taffy, node));
        assert_eq!(focused_heading(&list), Some("Monday"));

        // The list's own nodes: each heading before the rows it holds.
        let tree = Widget::accessibility(&list).expect("list semantics");
        let names = tree
            .children
            .iter()
            .map(|child| child.name.clone().unwrap_or_default())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "Monday", "row 0", "row 1", "Tuesday", "row 2", "row 3", "Done"
            ],
            "each heading stands before the rows it holds"
        );
        let monday = &tree.children[0];
        assert_eq!(monday.role, crate::a11y::Role::Button);
        assert_eq!(monday.value.as_deref(), Some("2"));
        assert_eq!(monday.state.expanded, Some(true));
        assert!(monday.state.focusable);
        assert!(monday.state.focused, "the keyboard is on it");
        assert_eq!(monday.action_target.as_deref(), Some("group:Monday"));
        assert!(monday.bounds.is_some(), "it is where its band is drawn");
        let done = tree.children.last().expect("the folded tail's heading");
        assert_eq!(done.name.as_deref(), Some("Done"));
        assert_eq!(done.value.as_deref(), Some("2"));
        assert_eq!(done.state.expanded, Some(false));
        assert!(!done.state.focused);
        assert!(
            (0..list.mounted.len()).all(|child| Widget::accessibility_represents_child(&list, child)),
            "every mounted row is a node of the list's own"
        );

        // The accesskit tree a reader is handed says the same.
        let nodes = access_nodes(&mut list);
        let heading = |label: &str| {
            nodes
                .iter()
                .find(|(_, node)| node.role() == AkRole::Button && node.label() == Some(label))
                .map_or_else(|| panic!("no heading labelled {label:?}"), |(_, node)| node)
        };
        let monday = heading("Monday");
        assert_eq!(monday.value(), Some("2"), "the rows it holds");
        assert_eq!(monday.is_expanded(), Some(true));
        assert!(monday.supports_action(AkAction::Collapse));
        assert!(monday.supports_action(AkAction::Click), "a press folds it");
        let done = heading("Done");
        assert_eq!(done.value(), Some("2"));
        assert_eq!(done.is_expanded(), Some(false));
        assert!(done.supports_action(AkAction::Expand));

        // An agent reads and toggles a heading as a control.
        let mut list = week();
        let _ = settle(&mut list);
        let name = |heading: &str| format!("group:{heading}");
        assert!(send(&mut list, &name("Done"), &crate::a11y::Action::Activate));
        assert_eq!(focused_heading(&list), Some("Done"));
        let _ = settle(&mut list);
        assert!(
            mounted_indexes(&list).contains(&4),
            "the tail's rows show: {:?}",
            mounted_indexes(&list)
        );
        let tree = Widget::accessibility(&list).expect("list semantics");
        let done = tree
            .children
            .iter()
            .find(|child| child.name.as_deref() == Some("Done"))
            .expect("the tail's heading");
        assert_eq!(done.state.expanded, Some(true));
        assert!(send(&mut list, &name("Monday"), &crate::a11y::Action::Collapse));
        let _ = settle(&mut list);
        assert!(
            bands(&list)
                .iter()
                .all(|(heading, _, closed)| heading != "Monday" || *closed),
            "Monday folded: {:?}",
            bands(&list)
        );
        assert!(send(&mut list, &name("Monday"), &crate::a11y::Action::Expand));
        let _ = settle(&mut list);
        assert!(
            bands(&list)
                .iter()
                .all(|(heading, _, closed)| heading != "Monday" || !*closed),
            "Monday unfolded: {:?}",
            bands(&list)
        );
        assert!(send(&mut list, &name("Tuesday"), &crate::a11y::Action::Focus));
        assert_eq!(focused_heading(&list), Some("Tuesday"));
        // A heading that is not there is not acted on.
        assert!(!send(&mut list, &name("Wednesday"), &crate::a11y::Action::Activate));
    }
}

fn access_nodes(list: &mut VirtualList<Grouped>) -> Vec<(accesskit::NodeId, accesskit::Node)> {
    let _ = settle(list);
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map = std::collections::HashMap::new();
    let _ = list.build_layout(&mut taffy, &theme, &mut node_map);
    crate::accesskit_bridge::build_tree_update(&*list, &node_map, None, 1.0).nodes
}

tilo_test_env::scenario! {
    fn a_title_over_a_list_takes_or_keeps_its_headings;

    phase a_title_over_a_list_takes_or_keeps_its_headings;
    {
        // A view titled like its one group heads nothing over the rows.
        let mut list = grouped(vec![heading("Today"), heading("Today")])
            .in_view("Today", Some(2))
            .in_column();
        let _ = settle(&mut list);
        assert!(
            bands(&list).is_empty(),
            "the title says it: no heading stands over the rows ({:?})",
            bands(&list)
        );
        let row = f64::from(ROW);
        assert_eq!(mounted_indexes(&list), vec![0, 1], "both rows are built");
        let inset = crate::chrome::column_gutter(&Theme::dark());
        assert!(
            (list.band_gap(0) - 0.0).abs() < 0.5,
            "the list keeps no heading's area over its first row: {}",
            list.band_gap(0)
        );
        assert!(
            (list.band_gap(1) - 0.0).abs() < 0.5,
            "the rows after it follow it: {}",
            list.band_gap(1)
        );
        let first = list.row_box(0).expect("a mounted first row");
        assert!(
            (first.height() - row).abs() < 0.5,
            "the first row takes its own height, with no band over it: {}",
            first.height()
        );
        assert!(
            (first.y0 - inset).abs() < 0.5,
            "the row's box stands on the column's content gutter: {} of {inset}",
            first.y0
        );

        // Two runs keep the headings that tell them apart under a title.
        let mut list = grouped(vec![
            heading("Overdue"),
            heading("Overdue"),
            heading("Today"),
            heading("Today"),
        ])
        .in_view("Today", Some(4));
        let _ = settle(&mut list);
        assert_eq!(
            bands(&list)
                .iter()
                .map(|(heading, _, _)| heading.as_str())
                .collect::<Vec<_>>(),
            vec!["Overdue", "Today"],
            "two runs keep the headings that tell them apart"
        );

        // A title counting other rows keeps the heading that counts these.
        let mut list = grouped(vec![heading("Today"), heading("Today")]).in_view("Today", Some(3));
        let _ = settle(&mut list);
        assert_eq!(
            bands(&list)
                .iter()
                .map(|(heading, _, _)| heading.as_str())
                .collect::<Vec<_>>(),
            vec!["Today"],
            "the heading counts what the title does not"
        );

        // A list whose group names another run keeps its heading.
        let mut list =
            grouped(vec![heading("Tomorrow"), heading("Tomorrow")]).in_view("Upcoming", Some(2));
        let _ = settle(&mut list);
        assert_eq!(
            bands(&list)
                .iter()
                .map(|(heading, _, _)| heading.as_str())
                .collect::<Vec<_>>(),
            vec!["Tomorrow"]
        );

        // The title a list stands under takes its heading away and gives it back.
        let mut list = grouped(vec![heading("Today"), heading("Today")]);
        let _ = settle(&mut list);
        assert_eq!(
            bands(&list)
                .iter()
                .map(|(heading, _, _)| heading.as_str())
                .collect::<Vec<_>>(),
            vec!["Today"],
            "no title over the list: its heading stands"
        );
        list.set_view_title(Some("Today".to_owned()), Some(2));
        let _ = settle(&mut list);
        assert!(
            bands(&list).is_empty(),
            "titled as the heading says: the heading goes ({:?})",
            bands(&list)
        );
        list.set_view_title(Some("Upcoming".to_owned()), Some(2));
        let _ = settle(&mut list);
        assert_eq!(
            bands(&list)
                .iter()
                .map(|(heading, _, _)| heading.as_str())
                .collect::<Vec<_>>(),
            vec!["Today"],
            "a title saying something else gives the heading back"
        );

        // The title counting what is left takes the heading over a finished row too.
        let mut list =
            grouped_finished(vec![heading("Today"), heading("Today")], &[1]).in_view("Today", Some(1));
        let _ = settle(&mut list);
        assert!(
            bands(&list).is_empty(),
            "the title says it: no heading stands over the rows ({:?})",
            bands(&list)
        );
        let mut list =
            grouped_finished(vec![heading("Today"), heading("Today")], &[1]).in_view("Today", Some(2));
        let _ = settle(&mut list);
        assert_eq!(
            bands(&list)
                .iter()
                .map(|(heading, _, _)| heading.as_str())
                .collect::<Vec<_>>(),
            vec!["Today"],
            "a title counting every row leaves the heading to count what is left"
        );
        assert_eq!(open_count(&mut list), None, "one row is left to do");
    }
}

fn open_count(list: &mut VirtualList<Grouped>) -> Option<usize> {
    let _ = settle(list);
    let band = list
        .groups
        .bands
        .first()
        .expect("the heading stands")
        .clone();
    list.shown_heading_count(&band)
}

/// An agent's outline of a list reads each row once, as the list states it: the row widgets
/// the list represents in its own nodes are never a second line beside them.
#[test]
fn an_agents_outline_reads_each_row_and_heading_once() {
    let _settled = crate::motion::testing::settled();
    let mut list = week();
    let _ = settle(&mut list);
    let outline = crate::semantic_outline::outline(&list);
    for line in [
        "\"Monday\"",
        "\"Tuesday\"",
        "\"row 0\"",
        "\"row 2\"",
        "\"Done\"",
    ] {
        assert_eq!(
            outline.text.matches(line).count(),
            1,
            "{line} is one line of the outline:\n{}",
            outline.text
        );
    }
}
