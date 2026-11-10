use super::*;
use std::cell::Cell;
use std::rc::Rc;

use crate::testing::Dispatch;
use crate::theme::Theme;
use crate::widget::section::band_height;

/// A list whose rows name their group, and which counts how often it is asked.
struct Grouped {
    groups: Vec<Option<RowGroup>>,
    asked: Rc<Cell<usize>>,
}

impl Grouped {
    fn new(groups: Vec<Option<RowGroup>>) -> Self {
        Self {
            groups,
            asked: Rc::new(Cell::new(0)),
        }
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

    fn discloses(&self) -> bool {
        self.groups.iter().any(Option::is_some)
    }
}

fn heading(name: &str) -> Option<RowGroup> {
    Some(RowGroup::heading(name.to_owned()))
}

const ROW: f32 = 40.0;

fn grouped(groups: Vec<Option<RowGroup>>) -> VirtualList<Grouped> {
    VirtualList::new(Grouped::new(groups), ROW, |_, index, _| {
        Box::new(super::tests::ProbeRow::new(
            format!("row {index}"),
            ROW,
            false,
            Rc::new(Cell::new(0)),
        )) as Element
    })
    .with_viewport_height(400.0)
}

/// Two tasks due Monday, two due Tuesday, and two done with.
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

/// Lay the list out until the rows' measured heights are placed: the first pass measures them,
/// the second stands their bands where the heights put them.
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

/// Point at `y` and press there, then let go: a band folds on the release.
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
    // The folded rows take no room: the list ends under the tail's heading.
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
    // A day's group closes the same way, and its rows let go.
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
    // Scroll into Tuesday's run, past its heading.
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
    // Scroll past Tuesday's heading, the rows still those the last layout mounted.
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

/// Press `key` on the focused list.
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
    // The band is still the heading's to press wherever it stands.
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

/// The heading holding the keyboard, by name.
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

    // Down from nothing lands on the first row, as a list without headings does; up from
    // there reaches the heading over it.
    assert!(key(&mut list, NamedKey::ArrowDown, &taffy, node));
    assert_eq!(list.focused_key(), Some(&0));
    assert_eq!(focused_heading(&list), None);
    assert!(key(&mut list, NamedKey::ArrowUp, &taffy, node));
    assert_eq!(focused_heading(&list), Some("Monday"));
    assert_eq!(list.focused_key(), None, "a heading is not one of the rows");

    // Down again steps into the group the heading heads, and up returns to it.
    assert!(key(&mut list, NamedKey::ArrowDown, &taffy, node));
    assert_eq!(list.focused_key(), Some(&0));
    assert_eq!(focused_heading(&list), None);
    assert!(key(&mut list, NamedKey::ArrowUp, &taffy, node));
    assert_eq!(focused_heading(&list), Some("Monday"));

    // Enter folds the group under the heading: its rows go, and the next stop is the next
    // heading's.
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

    // Space unfolds it where the keyboard already is.
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
    // End reaches the last stop: the folded tail's own heading.
    assert!(key(&mut list, NamedKey::End, &taffy, node));
    assert_eq!(focused_heading(&list), Some("Done"));
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
    // Read as disclosures: the name, how many rows the group holds, and whether they show.
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

/// The AccessKit nodes the person's screen reader is handed for the list.
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
    // Focus and activate: the tail opens, and the keyboard moves to it.
    assert!(send(&mut list, &name("Done"), &Action::Activate));
    assert_eq!(focused_heading(&list), Some("Done"));
    let _ = settle(&mut list);
    assert!(
        mounted_indexes(&list).contains(&4),
        "the tail's rows show: {:?}",
        mounted_indexes(&list)
    );
    // Read back through the tree the agent reads.
    let tree = Widget::accessibility(&list).expect("list semantics");
    let done = tree
        .children
        .iter()
        .find(|child| child.name.as_deref() == Some("Done"))
        .expect("the tail's heading");
    assert_eq!(done.state.expanded, Some(true));
    // Fold and unfold a day from outside the keyboard.
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
    // Focusing a heading moves the keyboard to it, and it scrolls into view.
    assert!(send(&mut list, &name("Tuesday"), &Action::Focus));
    assert_eq!(focused_heading(&list), Some("Tuesday"));
    // A heading that is not there is not acted on.
    assert!(!send(&mut list, &name("Wednesday"), &Action::Activate));
}

/// The list painted as a window paints it, rasterised to RGBA rows `300` wide.
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
