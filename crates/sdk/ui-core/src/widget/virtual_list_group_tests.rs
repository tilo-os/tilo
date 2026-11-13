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

#[test]
fn a_grouped_list_heads_each_run_and_keeps_the_done_tail_folded() {
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
}

#[test]
fn a_press_on_a_heading_opens_and_closes_its_group() {
    let mut list = week();
    let (taffy, node) = settle(&mut list);
    let band = band_height(&Theme::dark());
    let done = 4.0 * f64::from(ROW) + 2.0 * band;
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
}

#[test]
fn the_heading_over_the_top_of_the_box_stays_there_while_its_group_scrolls() {
    let mut list = grouped(
        (0..30)
            .map(|index| heading(["Monday", "Tuesday", "Wednesday"][index / 10]))
            .collect(),
    );
    let _ = settle(&mut list);
    let inside = list.viewport.item_top(12).unwrap_or_default() + 5.0;
    list.set_scroll_offset(inside);
    let _ = settle(&mut list);
    let shown = list.bands_in_view();
    assert_eq!(
        shown.first().map(|band| (band.heading.as_str(), band.top)),
        Some(("Tuesday", 0.0))
    );
}

#[test]
fn a_heading_stays_over_the_top_as_a_scroll_moves_it_without_a_layout() {
    let mut list = grouped(
        (0..30)
            .map(|index| heading(["Monday", "Tuesday", "Wednesday"][index / 10]))
            .collect(),
    );
    let _ = settle(&mut list);
    let inside = list.viewport.item_top(12).unwrap_or_default() + 5.0;
    list.set_scroll_offset(inside);
    let shown = list.bands_in_view();
    assert_eq!(
        shown
            .iter()
            .map(|band| (band.heading.as_str(), band.top))
            .collect::<Vec<_>>(),
        vec![("Tuesday", 0.0)],
        "the heading of the group under the top, held there once, over its rows"
    );
}

#[test]
fn a_long_list_asks_the_model_about_the_rows_in_view_only() {
    let groups = (0..100_000)
        .map(|index| heading(&format!("day {}", index / 50)))
        .collect();
    let mut list = grouped(groups);
    let _ = settle(&mut list);
    let asked = list.model.asked.get();
    assert!(
        asked <= 400,
        "a layout asks about the rows it lays out, not the whole list: {asked} of 100000"
    );
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

#[test]
fn a_finger_gets_a_heading_band_it_can_hit() {
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

/// The room the painted heading of the first group leaves between its lowest ink and the row
/// it heads: the gap a person sees, which a finger's taller band must not widen.
fn heading_gap(list: &mut VirtualList<Grouped>, theme: &Theme) -> f64 {
    let top = list.groups.bands.first().map_or(0.0, |band| band.top);
    let row_top = list.row_box(0).expect("a mounted first row").y0;
    let surface = theme.tokens.colors.surfaces.content;
    let wanted = [0, 1, 2].map(|channel| f64::from(surface.components[channel]) * 255.0);
    let rgba = painted(list, theme);
    let mut lowest: Option<f64> = None;
    for y in 0..400_u32 {
        let at = f64::from(y);
        if at < top || at + 1.0 > row_top {
            continue;
        }
        let inked = (0..300_u32).any(|x| {
            let pixel = usize::try_from((y * 300 + x) * 4).unwrap_or(0);
            rgba.get(pixel..pixel + 3).is_some_and(|rgb: &[u8]| {
                rgb.iter()
                    .zip(wanted)
                    .any(|(found, want)| (f64::from(*found) - want).abs() > 30.0)
            })
        });
        if inked {
            lowest = Some(at + 1.0);
        }
    }
    row_top - lowest.unwrap_or(top)
}

#[test]
fn a_heading_keeps_one_gap_to_the_row_it_heads_at_every_width() {
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark();
    let mut list = week();
    list.set_interaction_environment(crate::interaction::InteractionEnvironment::POINTER);
    let _ = settle(&mut list);
    let pointer = heading_gap(&mut list, &theme);
    list.set_interaction_environment(crate::interaction::InteractionEnvironment::TOUCH);
    let _ = settle(&mut list);
    let touch_band = list.groups.height;
    let target = f64::from(crate::interaction::InteractionEnvironment::TOUCH.minimum_target());
    assert!(
        touch_band >= target - 0.5,
        "the band is a target a finger presses: {touch_band} of {target}"
    );
    let touch = heading_gap(&mut list, &theme);
    assert!(
        pointer > 0.0,
        "the heading stands over its row, not on it: {pointer}"
    );
    assert!(
        (pointer - touch).abs() < 1.0,
        "the room a finger needs stands over the heading's line, never between it and its \
         first row: {pointer} on a pointer, {touch} under a finger"
    );
}

fn focused_heading(list: &VirtualList<Grouped>) -> Option<&str> {
    list.focused_group
        .as_ref()
        .map(|(heading, _)| heading.as_str())
}

#[test]
fn the_keyboard_walks_the_headings_and_folds_their_groups() {
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

#[test]
fn a_heading_counts_the_row_on_its_way_that_stands_under_it() {
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
        "each heading counts the rows standing under it"
    );
}

#[test]
fn a_heading_is_a_disclosure_in_the_lists_accessibility_children() {
    let mut list = week();
    let (taffy, node) = settle(&mut list);
    list.set_focus(true);
    assert!(key(&mut list, NamedKey::ArrowDown, &taffy, node));
    assert!(key(&mut list, NamedKey::ArrowUp, &taffy, node));
    assert_eq!(focused_heading(&list), Some("Monday"));

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
    // The list builds each mounted row's node itself, headings standing before
    // them: the walk never visits the row widgets.
    assert!(
        (0..list.mounted.len()).all(|child| Widget::accessibility_represents_child(&list, child)),
        "every mounted row is a node of the list's own"
    );
}

fn access_nodes(list: &mut VirtualList<Grouped>) -> Vec<(accesskit::NodeId, accesskit::Node)> {
    let _ = settle(list);
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map = std::collections::HashMap::new();
    let _ = list.build_layout(&mut taffy, &theme, &mut node_map);
    crate::accesskit_bridge::build_tree_update(&*list, &node_map, None, 1.0).nodes
}

#[test]
fn the_accesskit_tree_hands_a_reader_each_heading_with_its_expanded_state() {
    use accesskit::{Action as AkAction, Role as AkRole};
    let mut list = week();
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
}

#[test]
fn an_agent_reads_and_toggles_a_heading_as_a_control() {
    use crate::a11y::Action;
    let mut list = week();
    let _ = settle(&mut list);
    let name = |heading: &str| format!("group:{heading}");
    fn send(list: &mut VirtualList<Grouped>, target: &str, action: &crate::a11y::Action) -> bool {
        Widget::perform_semantic_action(
            list,
            target,
            action,
            &mut crate::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        )
    }
    assert!(send(&mut list, &name("Done"), &Action::Activate));
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
    assert!(send(&mut list, &name("Monday"), &Action::Collapse));
    let _ = settle(&mut list);
    assert!(
        bands(&list)
            .iter()
            .all(|(heading, _, closed)| heading != "Monday" || *closed),
        "Monday folded: {:?}",
        bands(&list)
    );
    assert!(send(&mut list, &name("Monday"), &Action::Expand));
    let _ = settle(&mut list);
    assert!(
        bands(&list)
            .iter()
            .all(|(heading, _, closed)| heading != "Monday" || !*closed),
        "Monday unfolded: {:?}",
        bands(&list)
    );
    assert!(send(&mut list, &name("Tuesday"), &Action::Focus));
    assert_eq!(focused_heading(&list), Some("Tuesday"));
    // A heading that is not there is not acted on.
    assert!(!send(&mut list, &name("Wednesday"), &Action::Activate));
}

fn painted(list: &mut VirtualList<Grouped>, theme: &Theme) -> Vec<u8> {
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let mut scene = Scene::new();
    list.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        &PresenceCtx::empty(),
    );
    let mut renderer =
        crate::offscreen::OffscreenRenderer::try_new_with_cpu_preprocessing(300, 400)
            .expect("the offscreen renderer");
    renderer.render_rgba(&scene)
}

/// The pixels of `rgba` (300 wide) that wear the focus ring's colour on `row`, at its sides.
fn ring_pixels(rgba: &[u8], theme: &Theme, row: usize) -> usize {
    let ring = theme.border_focus;
    let wanted = [0, 1, 2]
        .map(|channel| crate::num::i32_of_f64(f64::from(ring.components[channel]) * 255.0));
    (0..300)
        .filter(|x| *x < 5 || *x >= 295)
        .filter(|x| {
            let at = (row * 300 + x) * 4;
            rgba.get(at..at + 3).is_some_and(|pixel: &[u8]| {
                pixel
                    .iter()
                    .zip(wanted)
                    .all(|(found, want)| (i32::from(*found) - want).abs() < 70)
            })
        })
        .count()
}

#[test]
fn the_heading_holding_the_keyboard_wears_a_focus_ring() {
    let _settled = crate::motion::testing::settled();
    let theme = Theme::dark();
    let mut list = week();
    let (taffy, node) = settle(&mut list);
    let band = list.groups.height;
    let unfocused = painted(&mut list, &theme);
    let middle = crate::num::usize_of_f64(band / 2.0);
    assert_eq!(
        ring_pixels(&unfocused, &theme, middle),
        0,
        "no heading holds the keyboard: no ring"
    );
    list.set_focus(true);
    assert!(key(&mut list, NamedKey::ArrowDown, &taffy, node));
    assert!(key(&mut list, NamedKey::ArrowUp, &taffy, node));
    assert_eq!(focused_heading(&list), Some("Monday"));
    let focused = painted(&mut list, &theme);
    assert!(
        ring_pixels(&focused, &theme, middle) >= 2,
        "the heading the keyboard is on wears its ring on both its sides"
    );
}

#[test]
fn a_lists_headings_and_rows_share_one_column_and_one_trailing_edge() {
    let mut list = week();
    let _ = settle(&mut list);
    let rect = list.rect;
    let gutter = crate::chrome::disclosure_gutter(&Theme::dark());
    assert!(
        (list.lead - list.bleed.max(gutter)).abs() < 0.5,
        "a list whose headings fold keeps the chevron gutter before its column: {}",
        list.lead
    );
    let header = list.header_rect(0.0);
    let row = list.mounted.first().expect("a mounted row");
    let row = row.widget.get_rect();
    assert!(
        (row.x0 - header.x0).abs() < 0.5,
        "rows stand in their heading's column: {} and {}",
        row.x0,
        header.x0
    );
    assert!(
        (row.x1 - header.x1).abs() < 0.5,
        "the heading's count ends on the rows' trailing edge: {} and {}",
        header.x1,
        row.x1
    );
    assert!(
        (header.x0 - (rect.x0 + list.lead)).abs() < 0.5,
        "the column starts one lead in from the list's edge"
    );
}

#[test]
fn a_list_with_nothing_to_disclose_keeps_no_chevron_room() {
    let mut list = grouped(vec![None, None]);
    let _ = settle(&mut list);
    assert!(
        (list.lead - list.bleed).abs() < 0.5,
        "no heading or parent to fold reserves no gutter: {}",
        list.lead
    );
}

#[test]
fn a_view_titled_like_its_one_group_heads_nothing_over_the_rows() {
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
        (list.band_room(0) - 0.0).abs() < 0.5,
        "the list keeps no heading's room over its first row: {}",
        list.band_room(0)
    );
    assert!(
        (list.band_room(1) - 0.0).abs() < 0.5,
        "the rows after it follow it: {}",
        list.band_room(1)
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
}

#[test]
fn a_heading_counts_only_where_the_number_says_something() {
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

#[test]
fn a_heading_counts_the_rows_left_to_do() {
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
    // The tail heads the rows done with: it counts them as they stand, never what is left of
    // them, which is nothing.
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
}

#[test]
fn a_heading_a_reader_hears_counts_the_rows_left_to_do() {
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

#[test]
fn a_lone_group_heading_starts_the_rows_on_the_content_gutter() {
    let mut headed = grouped(vec![
        heading("Today"),
        heading("Today"),
        heading("Tomorrow"),
    ])
    .in_column();
    let _ = settle(&mut headed);
    let inset = crate::chrome::column_gutter(&Theme::dark());
    let band = band_height(&Theme::dark());
    let headed_band = bands(&headed).first().expect("a heading leads the rows").1;
    assert!(
        (headed_band - inset).abs() < 0.5,
        "the heading stands on the content gutter: {headed_band} of {inset}"
    );
    let headed_top = headed.row_box(0).expect("a mounted first row").y0;
    assert!(
        (headed_top - inset - band).abs() < 0.5,
        "the row stands under the heading's band: {headed_top}"
    );
    let mut lone = grouped(vec![heading("Today"), heading("Today")])
        .in_view("Today", Some(2))
        .in_column();
    let _ = settle(&mut lone);
    let lone_top = lone.row_box(0).expect("a mounted first row").y0;
    assert!(
        (lone_top - inset).abs() < 0.5,
        "with the heading left out the row stands on the content gutter: {lone_top} of {inset}"
    );
}

/// A list of rows that name no groups keeps its rows against its own edge: its block is the
/// list's, with nothing standing over it.
#[test]
fn a_list_of_rows_that_name_no_group_leads_with_them() {
    let mut list = grouped(vec![None, None]);
    let _ = settle(&mut list);
    assert!(
        (list.band_room(0) - 0.0).abs() < 0.5,
        "nothing heads the list: {} over its first row",
        list.band_room(0)
    );
}

#[test]
fn a_list_states_the_room_it_keeps_over_its_first_band() {
    let inset = crate::chrome::column_gutter(&Theme::dark());
    let stated = |list: &VirtualList<Grouped>| {
        let Some(crate::widget::LayoutInfo::List(stated)) = Widget::layout_info(list) else {
            panic!("a list states how it stands its rows");
        };
        stated
    };
    let mut headed = grouped(vec![heading("Today"), heading("Today")]).in_column();
    let _ = settle(&mut headed);
    let under_a_heading = stated(&headed);
    assert!(under_a_heading.heads_rows);
    assert!(
        (f64::from(under_a_heading.lead) - inset).abs() < 0.5,
        "the heading starts on the content gutter: {}",
        under_a_heading.lead
    );
    let mut lone = grouped(vec![heading("Today"), heading("Today")])
        .in_view("Today", Some(2))
        .in_column();
    let _ = settle(&mut lone);
    let under_the_title = stated(&lone);
    assert!(under_the_title.heads_rows);
    assert!(
        (f64::from(under_the_title.lead) - inset).abs() < 0.5,
        "the row starts on the content gutter: {}",
        under_the_title.lead
    );
    let mut plain = grouped(vec![None, None]);
    let _ = settle(&mut plain);
    let plain = stated(&plain);
    assert!(!plain.heads_rows);
    assert!(plain.lead.abs() < 0.5);
    let mut alone = grouped(vec![heading("Today"), heading("Today")]);
    let _ = settle(&mut alone);
    assert!(stated(&alone).lead.abs() < 0.5);
}

#[test]
fn the_room_over_a_lists_first_band_is_no_part_of_it() {
    let mut list = grouped(vec![heading("Today"), heading("Today")])
        .in_view("Today", Some(2))
        .in_column();
    let (taffy, node) = settle(&mut list);
    let inset = crate::chrome::column_gutter(&Theme::dark());
    let at = crate::synthetic_input::cursor_moved(20.0, inset / 2.0);
    let _ = Dispatch::new().layout(&taffy, node).send(&mut list, &at);
    assert_eq!(
        list.key_at_point(vello::kurbo::Point::new(20.0, inset / 2.0)),
        None,
        "the room over the first row is the list's, not the row's"
    );
    assert_eq!(
        list.key_at_point(vello::kurbo::Point::new(20.0, inset + 5.0)),
        Some(0),
        "the row itself takes the press below it"
    );
}

#[test]
fn a_list_of_two_groups_keeps_their_headings_under_a_title() {
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
}

#[test]
fn a_title_counting_other_rows_keeps_the_heading_that_counts_these() {
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
}

#[test]
fn a_list_whose_group_names_another_run_keeps_its_heading() {
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
}

#[test]
fn the_title_a_list_stands_under_takes_its_heading_away_and_gives_it_back() {
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
}

#[test]
fn the_title_counting_what_is_left_takes_the_heading_over_a_finished_row_too() {
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
