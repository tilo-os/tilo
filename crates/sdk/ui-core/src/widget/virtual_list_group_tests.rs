use super::*;
use std::cell::Cell;
use std::rc::Rc;

use crate::testing::Dispatch;
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
}

fn heading(name: &str) -> Option<RowGroup> {
    Some(RowGroup::Heading(name.to_owned()))
}

/// Two tasks due Monday, two due Tuesday, and two done with.
fn week() -> VirtualList<Grouped> {
    let groups = vec![
        heading("Monday"),
        heading("Monday"),
        heading("Tuesday"),
        heading("Tuesday"),
        Some(RowGroup::Tail),
        Some(RowGroup::Tail),
    ];
    VirtualList::new(Grouped::new(groups), 40.0, |_, index, _| {
        Box::new(Text::new(&format!("row {index}"))) as Element
    })
    .with_viewport_height(400.0)
}

fn lay_out(list: &mut VirtualList<Grouped>) -> (TaffyTree<()>, NodeId) {
    let doc = AutoCommit::new();
    list.sync(&doc, &automerge::ROOT);
    let mut taffy = TaffyTree::new();
    let mut node_map = HashMap::new();
    let theme = Theme::dark();
    let node = list.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout(
            node,
            Size {
                width: AvailableSpace::Definite(300.0),
                height: AvailableSpace::Definite(px(list.viewport.viewport())),
            },
        )
        .unwrap();
    list.apply_layout(&taffy, node, Point::ZERO);
    (taffy, node)
}

fn press(list: &mut VirtualList<Grouped>, y: f64, taffy: &TaffyTree<()>, node: NodeId) -> bool {
    let mut at = crate::synthetic_input::cursor_moved(20.0, y);
    let handled = Dispatch::new().layout(taffy, node).send(list, &at).handled;
    at = crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Left);
    handled | Dispatch::new().layout(taffy, node).send(list, &at).handled
}

fn mounted_indexes(list: &VirtualList<Grouped>) -> Vec<usize> {
    list.mounted.iter().map(|row| row.index).collect()
}

#[test]
fn a_grouped_list_heads_each_run_and_keeps_the_done_tail_folded() {
    let mut list = week();
    let (_taffy, _node) = lay_out(&mut list);
    let band = band_height(&Theme::dark());
    assert!(band > 0.0);
    assert_eq!(
        list.groups
            .bands
            .iter()
            .map(|band| (band.heading.as_str(), band.top, band.closed))
            .collect::<Vec<_>>(),
        vec![("Monday", 0.0, false), ("Tuesday", 80.0 + band, false)]
    );
    // The tail's heading stands after the last day, and its rows take no room.
    let tail = band + 80.0 + band + 80.0;
    assert!(
        (list.viewport.item_top(5).unwrap_or_default() - (tail + band)).abs() < 1.5,
        "the done tail stands after the days: {:?}",
        list.viewport.item_top(5)
    );
    let mounted = mounted_indexes(&list);
    assert!(
        !mounted.contains(&4) && !mounted.contains(&5),
        "no done row is built while folded: {mounted:?}"
    );
}

#[test]
fn a_press_on_a_heading_opens_and_closes_its_group() {
    let mut list = week();
    let (taffy, node) = lay_out(&mut list);
    let band = band_height(&Theme::dark());
    let tail = band + 80.0 + band + 80.0;
    assert!(press(&mut list, tail + band / 2.0, &taffy, node));
    let (_taffy, _node) = lay_out(&mut list);
    assert!(mounted_indexes(&list).contains(&4), "the tail opened");
    assert!(
        list.groups.bands.iter().any(|band| band.closed),
        "its heading reads closed"
    );
    // A day's group closes the same way, and its rows let go.
    let (taffy, node) = lay_out(&mut list);
    assert!(press(&mut list, band / 2.0, &taffy, node));
    let (_taffy, _node) = lay_out(&mut list);
    assert!(
        list.groups
            .bands
            .iter()
            .all(|band| band.heading != "Monday" || band.closed),
        "Monday closed"
    );
    assert!(!mounted_indexes(&list).contains(&0), "its rows let go");
}

#[test]
fn the_heading_over_the_top_of_the_box_stays_there_while_its_group_scrolls() {
    let mut list = week();
    let (_taffy, _node) = lay_out(&mut list);
    // Scroll into the middle of Tuesday's run: its heading is above the box.
    let band = band_height(&Theme::dark());
    let tuesday = band + 80.0 + band;
    list.set_scroll_offset(tuesday + 20.0);
    let (_taffy, _node) = lay_out(&mut list);
    let bands = list.bands_in_view();
    assert_eq!(
        bands.first().map(|band| band.heading.as_str()),
        Some("Tuesday")
    );
    assert_eq!(bands.first().map(|band| band.top), Some(0.0));
}

#[test]
fn a_long_list_asks_the_model_about_the_rows_in_view_only() {
    let groups = (0..100_000)
        .map(|index| heading(&format!("day {}", index / 50)))
        .collect();
    let mut list = VirtualList::new(Grouped::new(groups), 40.0, |_, index, _| {
        Box::new(Text::new(&format!("row {index}"))) as Element
    })
    .with_viewport_height(400.0);
    let (_taffy, _node) = lay_out(&mut list);
    let asked = list.model.asked.get();
    assert!(
        asked <= 60,
        "a layout asks about the rows it lays out, not the whole list: {asked} of 100000"
    );
}
