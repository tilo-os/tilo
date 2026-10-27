use super::*;
use crate::time::Duration;
use automerge::AutoCommit;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::testing::Dispatch;
use taffy::AvailableSpace;

struct HugeList {
    count: usize,
}

impl ListModel for HugeList {
    type Key = u64;

    fn len(&self) -> usize {
        self.count
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        (index < self.count)
            .then(|| u64::try_from(index).ok())
            .flatten()
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        usize::try_from(*key)
            .ok()
            .filter(|index| *index < self.count)
    }
}

struct SharedOrder {
    keys: Rc<RefCell<Vec<u64>>>,
}

impl ListModel for SharedOrder {
    type Key = u64;

    fn len(&self) -> usize {
        self.keys.borrow().len()
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        self.keys.borrow().get(index).copied()
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        self.keys
            .borrow()
            .iter()
            .position(|candidate| candidate == key)
    }
}

struct NotifyingOrder {
    keys: Rc<RefCell<Vec<u64>>>,
    changes: Rc<RefCell<Vec<ListModelChange<u64>>>>,
}

impl ListModel for NotifyingOrder {
    type Key = u64;

    fn len(&self) -> usize {
        self.keys.borrow().len()
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        self.keys.borrow().get(index).copied()
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        self.keys
            .borrow()
            .iter()
            .position(|candidate| candidate == key)
    }

    fn poll_changes(&mut self) -> Vec<ListModelChange<Self::Key>> {
        std::mem::take(&mut *self.changes.borrow_mut())
    }
}

pub(super) struct PagedList {
    pub(super) count: usize,
    pub(super) states: Rc<RefCell<HashMap<usize, u8>>>,
    pub(super) requests: Rc<RefCell<Vec<Range<usize>>>>,
    pub(super) changed: Rc<Cell<bool>>,
    pub(super) pending: Rc<Cell<bool>>,
}

impl ListModel for PagedList {
    type Key = u64;

    fn len(&self) -> usize {
        self.count
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        (index < self.count)
            .then(|| u64::try_from(index).ok())
            .flatten()
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        usize::try_from(*key)
            .ok()
            .filter(|index| *index < self.count)
    }

    fn row_state(&self, index: usize) -> Arrival<(), &str> {
        match self.states.borrow().get(&index).copied() {
            Some(1) => Arrival::Ready(()),
            Some(2) => Arrival::Failed("Fetch failed"),
            _ => Arrival::local(),
        }
    }

    fn request_range(&mut self, range: Range<usize>) {
        self.requests.borrow_mut().push(range);
    }

    fn poll_changed(&mut self) -> bool {
        self.changed.replace(false)
    }

    fn needs_poll(&self) -> bool {
        self.pending.get()
    }
}

pub(super) struct ProbeRow {
    label: String,
    height: f32,
    consumes_press: bool,
    presses: Rc<Cell<usize>>,
    rect: Rect,
}

impl ProbeRow {
    pub(super) fn new(
        label: impl Into<String>,
        height: f32,
        consumes_press: bool,
        presses: Rc<Cell<usize>>,
    ) -> Self {
        Self {
            label: label.into(),
            height,
            consumes_press,
            presses,
            rect: Rect::ZERO,
        }
    }
}

impl Widget for ProbeRow {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.consumes_press
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            )
        {
            self.presses.set(self.presses.get().saturating_add(1));
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "ProbeRow"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name(&self.label))
    }
}

pub(super) fn lay_out<M: ListModel + 'static>(
    list: &mut VirtualList<M>,
    width: f32,
) -> (TaffyTree<()>, NodeId) {
    let doc = AutoCommit::new();
    list.sync(&doc, &automerge::ROOT);
    let mut taffy = TaffyTree::new();
    let mut node_map = HashMap::new();
    let node = list.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
    taffy
        .compute_layout(
            node,
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::Definite(px(list.viewport.viewport())),
            },
        )
        .unwrap();
    list.apply_layout(&taffy, node, Point::ZERO);
    (taffy, node)
}

fn cursor(y: f64) -> WindowEvent {
    crate::synthetic_input::cursor_moved(20.0, y)
}

fn left(state: ElementState) -> WindowEvent {
    crate::synthetic_input::mouse_input(state, MouseButton::Left)
}

fn dispatch<M: ListModel + 'static>(
    list: &mut VirtualList<M>,
    event: &WindowEvent,
    taffy: &TaffyTree<()>,
    node: NodeId,
) -> bool {
    Dispatch::new()
        .layout(taffy, node)
        .send(list, event)
        .handled
}

#[test]
fn list_models_keep_stable_unique_keys_through_mutation_and_restore() {
    {
        let result = VecListModel::new(vec![1, 1], |value| *value);
        assert!(matches!(result, Err(DuplicateListKey)));
    }

    {
        let mut selection = KeySelection::new();
        selection.toggle(1_u64);
        selection.toggle(99_u64);
        let list = VirtualList::new(HugeList { count: 10 }, 24.0, |_, index, _| {
            Box::new(crate::widget::Text::new(&format!("Item {index}")))
        })
        .with_selection(selection);

        assert!(list.selection().is_selected(&1));
        assert!(!list.selection().is_selected(&99));
        assert_eq!(list.selection().len(10), 1);
    }

    {
        let mut model = VecListModel::new(vec!["a", "b", "c"], |value| *value).unwrap();
        assert!(model.move_item(0, 2));
        assert_eq!(model.key_at(2), Some("a"));
        assert_eq!(model.index_of(&"b"), Some(0));
        assert_eq!(model.remove(1), Some("c"));
        assert_eq!(model.index_of(&"a"), Some(1));
        assert!(model.push("d", "d").is_ok());
        assert_eq!(model.index_of(&"d"), Some(2));
        assert_eq!(model.push("d", "duplicate"), Err(DuplicateListKey));
    }

    {
        let list = VirtualList::from_items(
            vec![(7_u64, "seven")],
            24.0,
            |item| item.0,
            |item, _, _| Box::new(ProbeRow::new(item.1, 24.0, false, Rc::new(Cell::new(0)))),
        )
        .unwrap()
        .with_id("contacts-list");
        assert_eq!(list.model().key_at(0), Some(7));
        assert_eq!(Widget::test_id(&list), Some("contacts-list"));
    }

    {
        let states = Rc::new(RefCell::new(HashMap::new()));
        let model = PagedList {
            count: 1,
            states: states.clone(),
            requests: Rc::default(),
            changed: Rc::default(),
            pending: Rc::default(),
        };
        let mut list = VirtualList::new(model, 24.0, |_, _, key| {
            Box::new(crate::widget::Button::new(&key.to_string(), "Row"))
        })
        .with_viewport_height(48.0);
        let _ = lay_out(&mut list, 300.0);
        list.take_structure_dirty();
        let before = list.mounted[0].node;
        assert!(before.is_some());
        let doc = AutoCommit::new();
        list.sync(&doc, &automerge::ROOT);
        assert_eq!(
            list.mounted[0].node, before,
            "sync preserves a retained row's layout node"
        );
        list.take_structure_dirty();
        states.borrow_mut().insert(0, 1);
        list.sync(&doc, &automerge::ROOT);
        assert!(list.mounted[0].node.is_none());
        assert!(
            list.take_structure_dirty(),
            "same key with new state replaces a delegate and invalidates topology"
        );
    }
}

#[test]
fn semantic_rows_share_scrolled_variable_geometry_and_stable_key_actions() {
    {
        let activated = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&activated);
        let model = VecListModel::new(vec![10_u64, 20], |key| *key).unwrap();
        let mut list = VirtualList::new(model, 24.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                24.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(100.0)
        .with_semantic_key(u64::to_string)
        .on_activate(move |key| captured.borrow_mut().push(key));
        let _ = lay_out(&mut list, 300.0);

        let semantics = Widget::accessibility(&list).expect("list semantics");
        assert_eq!(
            semantics.children[1].action_target.as_deref(),
            Some("key:20")
        );
        assert!(Widget::perform_semantic_action(
            &mut list,
            "key:20",
            &crate::a11y::Action::Focus,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(list.focused_key, Some(20));
        assert!(Widget::perform_semantic_action(
            &mut list,
            "key:20",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert!(list.selection().is_selected(&20));
        assert_eq!(&*activated.borrow(), &[20]);
    }

    {
        let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
        let mut list = VirtualList::new(model, 24.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                24.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(48.0);
        list.set_scroll_offset(10.0);
        let _ = lay_out(&mut list, 300.0);

        let semantics = Widget::accessibility(&list).expect("list semantics");
        let first = semantics.children.first().expect("first mounted row");
        // One bleed in from the list's edges, and clear of the bar's
        // gutter while the rows overflow.
        let gutter = f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);
        assert_eq!(
            first.bounds.expect("row bounds").to_rect(),
            Rect::new(8.0, -10.0, 300.0 - gutter - 8.0, 14.0)
        );
        assert_eq!(
            list.mounted[0].widget.get_rect(),
            first.bounds.expect("row bounds").to_rect(),
            "semantic and retained delegate geometry agree",
        );
    }

    {
        let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
        let mut list = VirtualList::new(model, 24.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                24.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(100.0);
        assert!(list.set_row_height(0, 300.0));
        list.focused_key = Some(1);

        assert!(list.on_key(&Key::Named(NamedKey::PageDown), false, false));
        assert_eq!(list.focused_key, Some(1));
        assert_eq!(list.scroll_offset(), 90.0);
        assert!(list.on_key(&Key::Named(NamedKey::PageUp), false, false));
        assert_eq!(list.focused_key, Some(1));
        assert_eq!(list.scroll_offset(), 0.0);
    }

    {
        // A list that fits its viewport: Page Down and Page Up move to
        // its last and first rows rather than doing nothing.
        let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
        let mut list = VirtualList::new(model, 24.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                24.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(100.0);
        list.focused_key = Some(1);

        assert!(list.on_key(&Key::Named(NamedKey::PageDown), false, false));
        assert_eq!(list.focused_key, Some(3));
        assert!(list.on_key(&Key::Named(NamedKey::PageUp), false, false));
        assert_eq!(list.focused_key, Some(1));
        assert_eq!(list.scroll_offset(), 0.0);
    }
}

#[test]
fn hundred_million_row_lists_mount_and_request_only_the_window() {
    {
        let builds = Rc::new(Cell::new(0_usize));
        let captured = Rc::clone(&builds);
        let presses = Rc::new(Cell::new(0));
        let mut list = VirtualList::new(HugeList { count: 100_000_000 }, 20.0, move |_, _, key| {
            captured.set(captured.get().saturating_add(1));
            Box::new(ProbeRow::new(
                format!("row {key}"),
                20.0,
                false,
                Rc::clone(&presses),
            ))
        })
        .with_viewport_height(100.0);
        list.set_scroll_offset(50_000_000.0 * 20.0);
        let _ = lay_out(&mut list, 300.0);

        assert_eq!(list.model().len(), 100_000_000);
        assert!(list.mounted_len() <= 10);
        assert_eq!(builds.get(), list.mounted_len());
        assert!(list.visible_range().len() <= 10);
        let top = list.scrollbar_thumb().expect("overflow has a scrollbar");
        list.set_scroll_offset(list.max_scroll_offset());
        let bottom = list.scrollbar_thumb().expect("scrollbar remains visible");
        assert!(bottom.y0 > top.y0);
        let track = crate::widget::scrollbar::vertical_track(list.rect, list.gutter);
        assert!((bottom.y1 - track.y1).abs() < 0.001);
    }
}

#[test]
fn delegates_follow_stable_keys_and_rebuild_only_what_changed() {
    {
        let keys = Rc::new(RefCell::new(vec![1, 2, 3, 4]));
        let builds = Rc::new(Cell::new(0_usize));
        let captured = Rc::clone(&builds);
        let mut list = VirtualList::new(
            SharedOrder {
                keys: Rc::clone(&keys),
            },
            20.0,
            move |_, _, key| {
                captured.set(captured.get().saturating_add(1));
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            },
        )
        .with_viewport_height(80.0);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(builds.get(), 4);

        keys.borrow_mut().swap(0, 3);
        list.notify_model_changed();
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(builds.get(), 4, "visible keyed widgets are retained");
    }

    {
        let keys = Rc::new(RefCell::new(vec![1, 2, 3, 4]));
        let changes = Rc::new(RefCell::new(Vec::new()));
        let builds = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&builds);
        let mut list = VirtualList::new(
            NotifyingOrder {
                keys,
                changes: Rc::clone(&changes),
            },
            20.0,
            move |_, _, key| {
                captured.borrow_mut().push(*key);
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            },
        )
        .with_viewport_height(80.0);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(*builds.borrow(), vec![1, 2, 3, 4]);

        changes
            .borrow_mut()
            .push(ListModelChange::RowsChanged { keys: vec![2] });
        assert!(Widget::update(
            &mut list,
            &AutoCommit::new(),
            &automerge::ROOT,
        ));
        let _ = lay_out(&mut list, 300.0);

        assert_eq!(*builds.borrow(), vec![1, 2, 3, 4, 2]);
        assert_eq!(list.mounted_len(), 4);
    }

    {
        let builds = Rc::new(Cell::new(0_usize));
        let captured = Rc::clone(&builds);
        let model = VecListModel::new(vec![1_u64, 2, 3, 4], |key| *key).unwrap();
        let mut list = VirtualList::new(model, 20.0, move |_, _, key| {
            captured.set(captured.get().saturating_add(1));
            Box::new(ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(100.0);
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(builds.get(), 4);

        list.update_model_changes(
            &[ListModelChange::RowsInserted { index: 4, count: 1 }],
            |model| model.push(5, 5).unwrap(),
        );
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(builds.get(), 5, "only the inserted key mounts");

        list.update_model_changes(
            &[ListModelChange::RowsMoved {
                from: 4,
                to: 0,
                count: 1,
            }],
            |model| assert!(model.move_item(4, 0)),
        );
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(builds.get(), 5, "moving stable keys retains delegates");
    }

    {
        let keys = Rc::new(RefCell::new((0_u64..100).collect::<Vec<_>>()));
        let mut list = VirtualList::new(
            SharedOrder {
                keys: Rc::clone(&keys),
            },
            20.0,
            |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            },
        )
        .with_viewport_height(40.0);
        list.set_scroll_offset(50.0 * 20.0 + 5.0);
        let before = list.viewport.item_view_top(50);

        keys.borrow_mut().insert(0, 1_000);
        list.notify_model_changed();

        assert_eq!(list.model().index_of(&50), Some(51));
        assert_eq!(list.viewport.item_view_top(51), before);
    }

    {
        let model = VecListModel::new(vec![0_u64, 1, 2], |key| *key).unwrap();
        let mut list = VirtualList::new(model, 20.0, |_, _, key| {
            let height = if *key == 1 { 55.0 } else { 20.0 };
            Box::new(ProbeRow::new(
                key.to_string(),
                height,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(100.0);
        let _ = lay_out(&mut list, 200.0);

        assert_eq!(list.viewport.item_height(0), Some(20.0));
        assert_eq!(list.viewport.item_height(1), Some(55.0));
        assert_eq!(list.viewport.item_top(2), Some(75.0));
    }
}

#[test]
fn pointer_input_separates_delegate_presses_drag_scroll_and_selection() {
    {
        let presses = Rc::new(Cell::new(0));
        let captured = Rc::clone(&presses);
        let model = VecListModel::new(vec![7_u64], |key| *key).unwrap();
        let mut list = VirtualList::new(model, 30.0, move |_, _, key| {
            Box::new(ProbeRow::new(
                format!("action {key}"),
                30.0,
                true,
                Rc::clone(&captured),
            ))
        })
        .with_viewport_height(30.0);
        let (taffy, node) = lay_out(&mut list, 200.0);

        dispatch(&mut list, &cursor(15.0), &taffy, node);
        assert!(dispatch(
            &mut list,
            &left(ElementState::Pressed),
            &taffy,
            node
        ));
        assert_eq!(presses.get(), 1);
        assert!(!list.selection().is_selected(&7));

        let semantics = list.accessibility().unwrap();
        assert_eq!(semantics.role, crate::a11y::Role::List);
        assert_eq!(semantics.children[0].role, crate::a11y::Role::ListItem);
        assert_eq!(semantics.children[0].name.as_deref(), Some("action 7"));
        assert_eq!(
            semantics.children[0].children[0].role,
            crate::a11y::Role::Button
        );
    }

    {
        let model = VecListModel::new((0_u64..100).collect(), |key| *key).unwrap();
        let mut list = VirtualList::new(model, 20.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(100.0);
        let (taffy, node) = lay_out(&mut list, 200.0);

        dispatch(&mut list, &cursor(50.0), &taffy, node);
        dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
        dispatch(&mut list, &cursor(10.0), &taffy, node);
        dispatch(&mut list, &left(ElementState::Released), &taffy, node);
        assert_eq!(list.scroll_offset(), 40.0);
        assert!(list.selection().is_empty(100));

        list.set_scroll_offset(0.0);
        let (taffy, node) = lay_out(&mut list, 200.0);
        dispatch(&mut list, &cursor(10.0), &taffy, node);
        dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
        dispatch(&mut list, &left(ElementState::Released), &taffy, node);
        assert!(list.selection().is_selected(&0));
    }

    {
        let model = VecListModel::new((0_u64..100).collect(), |key| *key).unwrap();
        let mut list = VirtualList::new(model, 20.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(100.0);
        let (taffy, node) = lay_out(&mut list, 200.0);
        list.reduce_motion = true;

        dispatch(&mut list, &cursor(50.0), &taffy, node);
        let wheel = crate::synthetic_input::mouse_wheel(0.0, -1.0);
        assert!(dispatch(&mut list, &wheel, &taffy, node));
        assert_eq!(list.scroll_offset(), 60.0);
        assert!(!list.momentum.is_active());

        let now = crate::time::now();
        let started = now.checked_sub(Duration::from_millis(20)).unwrap_or(now);
        let moved = now.checked_sub(Duration::from_millis(5)).unwrap_or(now);
        list.drag_scroll.begin(50.0, list.scroll_offset(), started);
        assert!(list.drag_scroll.update(10.0, moved).is_some());
        assert!(dispatch(
            &mut list,
            &left(ElementState::Released),
            &taffy,
            node
        ));
        assert!(!list.momentum.is_active());
        assert!(!list.needs_animation_frame());
    }
}

#[test]
fn list_reorder_previews_and_commits_valid_drops_only() {
    {
        let mut list = VirtualList::from_items(
            vec![0_u64, 1, 2],
            20.0,
            |key| *key,
            |key, _, _| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            },
        )
        .unwrap()
        .with_viewport_height(60.0)
        .reorderable();
        let (taffy, node) = lay_out(&mut list, 200.0);

        dispatch(&mut list, &cursor(10.0), &taffy, node);
        dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
        dispatch(&mut list, &cursor(55.0), &taffy, node);
        assert_eq!(
            list.reorder_target(),
            Some(&DropTarget::new(2, DropPlacement::After))
        );
        assert!(list.reorder_preview().is_some());
        dispatch(&mut list, &left(ElementState::Released), &taffy, node);

        assert_eq!(list.model().items(), &[1, 2, 0]);
        assert_eq!(list.model().index_of(&0), Some(2));
        assert_eq!(list.reorder_target(), None);
    }

    {
        let model = VecListModel::new(vec![0_u64, 1], |key| *key).unwrap();
        let mut list = VirtualList::new(model, 20.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(40.0)
        .with_reorder(|_, _, _| false, |_, _| true);
        let (taffy, node) = lay_out(&mut list, 200.0);

        dispatch(&mut list, &cursor(10.0), &taffy, node);
        dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
        dispatch(&mut list, &cursor(35.0), &taffy, node);
        assert!(list.reorder_preview().is_none());
        dispatch(&mut list, &left(ElementState::Released), &taffy, node);
        assert_eq!(list.model().items(), &[0, 1]);
    }
}

fn plain_rows(count: u32) -> VirtualList<VecListModel<u32, u32>> {
    VirtualList::from_items(
        (0..count).collect(),
        40.0,
        |item| *item,
        |item, _, _| Box::new(crate::widget::Text::new(&format!("row {item}"))) as Element,
    )
    .unwrap()
    .with_viewport_height(200.0)
}

#[test]
fn rows_stand_a_bleed_in_and_the_bar_takes_the_shared_gutter() {
    let mut list = plain_rows(100);
    let _ = lay_out(&mut list, 300.0);
    let theme = Theme::dark();
    let bleed = crate::chrome::row_bleed(&theme);
    let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
    let row = list.mounted[0].widget.get_rect();
    assert!((row.x0 - bleed).abs() < 0.5, "a bleed in: {row:?}");
    assert!(
        (row.x1 - (300.0 - gutter - bleed)).abs() < 0.5,
        "clear of the bar: {row:?}"
    );
    let track = list.scrollbar_track().unwrap();
    assert!(track.x0 >= 300.0 - gutter && track.x1 <= 300.0);
    // Without overflow the rows take the list's width less the bleed.
    let mut short = plain_rows(2);
    let _ = lay_out(&mut short, 300.0);
    let row = short.mounted[0].widget.get_rect();
    assert!((row.x1 - (300.0 - bleed)).abs() < 0.5);
    assert!(short.scrollbar_track().is_none());
}

#[test]
fn a_columns_rows_start_on_its_content_edge_and_leave_the_bar_its_gutter() {
    let theme = Theme::dark();
    let edge = crate::chrome::column_gutter(&theme);
    let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
    let lift = crate::chrome::row_bleed(&theme);
    let mut list = plain_rows(100).in_column();
    let _ = lay_out(&mut list, 300.0);
    let row = list.mounted[0].widget.get_rect();
    assert!((row.x0 - edge).abs() < 0.5, "on the content edge: {row:?}");
    assert!(
        row.x1 + lift <= 300.0 - gutter + 0.5,
        "the lift clears the bar: {row:?}"
    );
    assert!((row.x1 - (300.0 - edge.max(gutter + lift))).abs() < 0.5);
    let track = list.scrollbar_track().unwrap();
    assert!(
        track.x1 <= 300.0 && track.x0 >= 300.0 - gutter,
        "the bar stays on the edge"
    );
    let mut short = plain_rows(2).in_column();
    let _ = lay_out(&mut short, 300.0);
    let row = short.mounted[0].widget.get_rect();
    assert!((row.x0 - edge).abs() < 0.5 && (row.x1 - (300.0 - edge)).abs() < 0.5);
}

/// The rect of the first widget under `widget` that holds no other.
fn first_leaf(widget: &dyn Widget) -> Rect {
    widget
        .children()
        .first()
        .map_or_else(|| widget.get_rect(), |child| first_leaf(*child))
}

#[test]
fn flush_page_rows_with_page_edges_stand_where_a_page_puts_its_content() {
    use crate::widget::arrange::{Page, PageRowEdges};
    let text = |n: u32| Box::new(crate::widget::Text::new(&format!("row {n}"))) as Element;
    let mut list = VirtualList::from_items(
        vec![0_u32, 1, 2],
        40.0,
        |item| *item,
        move |item, _, _| Box::new(Page::full().row().push(text(*item))) as Element,
    )
    .unwrap()
    .with_viewport_height(400.0)
    .flush()
    .with_edges(PageRowEdges::new());
    // The first layout learns the rows' width, the second places them by it.
    let _ = lay_out(&mut list, 600.0);
    let _ = lay_out(&mut list, 600.0);
    let row = first_leaf(list.mounted[0].widget.as_ref());

    let mut page = Page::full().push(text(0));
    let mut taffy = TaffyTree::new();
    let mut node_map = HashMap::new();
    let node = page.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
    taffy
        .compute_layout(
            node,
            Size {
                width: AvailableSpace::Definite(600.0),
                height: AvailableSpace::Definite(400.0),
            },
        )
        .unwrap();
    page.apply_layout(&taffy, node, Point::ZERO);
    let alone = first_leaf(&page);
    assert!(
        (row.x0 - alone.x0).abs() < 0.5,
        "one content edge: {row:?} {alone:?}"
    );
    assert!(
        (row.y0 - alone.y0).abs() < 0.5,
        "the page's top spacing: {row:?} {alone:?}"
    );
}

#[test]
fn a_filling_list_takes_its_parents_height_as_its_viewport() {
    let mut list = plain_rows(1000).filling();
    let doc = AutoCommit::new();
    let mut taffy = TaffyTree::new();
    let mut node_map = HashMap::new();
    let mut lay_out_in = |list: &mut VirtualList<_>, taffy: &mut TaffyTree<()>| {
        list.sync(&doc, &automerge::ROOT);
        taffy.clear();
        node_map.clear();
        let node = list.build_layout(taffy, &Theme::dark(), &mut node_map);
        let parent = taffy
            .new_with_children(
                Style {
                    size: Size::from_lengths(300.0_f32, 600.0_f32),
                    ..Default::default()
                },
                &[node],
            )
            .unwrap();
        taffy
            .compute_layout(
                parent,
                Size {
                    width: AvailableSpace::MaxContent,
                    height: AvailableSpace::MaxContent,
                },
            )
            .unwrap();
        list.apply_layout(taffy, node, Point::ZERO);
    };
    lay_out_in(&mut list, &mut taffy);
    assert!((list.get_rect().height() - 600.0).abs() < 0.5);
    lay_out_in(&mut list, &mut taffy);
    let rows = list.visible_range();
    assert!(
        rows.end * 40 >= 600,
        "the window covers the height: {rows:?}"
    );
}

#[test]
fn selection_can_follow_the_keyboard_focus() {
    let selected = Rc::new(RefCell::new(Vec::new()));
    let record = Rc::clone(&selected);
    let mut list = plain_rows(10)
        .selection_follows_focus()
        .on_select(move |key, on| record.borrow_mut().push((key, on)));
    let (taffy, node) = lay_out(&mut list, 300.0);
    list.set_focus(true);
    assert!(list.reveal(&2));
    let down = crate::synthetic_input::named_key(NamedKey::ArrowDown);
    assert!(dispatch(&mut list, &down, &taffy, node));
    assert_eq!(list.focused_key(), Some(&3));
    assert!(
        list.selection().is_selected(&3),
        "the focused row is chosen"
    );
    assert!(!list.selection().is_selected(&2));
    assert_eq!(selected.borrow().last(), Some(&(3, true)));
}

#[test]
fn a_transcript_opens_at_its_end_and_follows_it_until_scrolled_away() {
    let mut list = plain_rows(10_000).following_end();
    // The rows settle their heights over a few layouts; the list stays at its end.
    for _ in 0..4 {
        let _ = lay_out(&mut list, 300.0);
    }
    assert!(list.is_at_end());
    assert!(list.max_scroll_offset() > 0.0);
    assert!((list.max_scroll_offset() - list.scroll_offset()).abs() < 0.01);
    assert!(
        list.visible_range().contains(&9_999),
        "the newest row shows"
    );

    // A row arrives: the list follows it.
    list.update_model_changes(
        &[ListModelChange::RowsInserted {
            index: 10_000,
            count: 1,
        }],
        |model| model.push(10_000, 10_000).unwrap(),
    );
    for _ in 0..2 {
        let _ = lay_out(&mut list, 300.0);
    }
    assert!((list.max_scroll_offset() - list.scroll_offset()).abs() < 0.01);
    assert!(list.visible_range().contains(&10_000));

    // Scrolled back to read: an arrival leaves the rows where they are. (Rows as tall as
    // the estimate, so that rows settling their heights above move nothing either.)
    let mut list = VirtualList::from_items(
        (0..10_000_u32).collect(),
        40.0,
        |item| *item,
        |_, _, _| Box::new(crate::widget::Spacer::fixed(40.0)) as Element,
    )
    .unwrap()
    .with_viewport_height(200.0)
    .following_end();
    let _ = lay_out(&mut list, 300.0);
    list.set_scroll_offset(list.scroll_offset() - 500.0);
    let _ = lay_out(&mut list, 300.0);
    assert!(!list.is_at_end());
    let reading = list.scroll_offset();
    list.update_model_changes(
        &[ListModelChange::RowsInserted {
            index: 10_000,
            count: 1,
        }],
        |model| model.push(10_000, 10_000).unwrap(),
    );
    let _ = lay_out(&mut list, 300.0);
    assert!(
        (list.scroll_offset() - reading).abs() < 0.01,
        "the rows read stay put"
    );
    assert!(!list.is_at_end());

    // Back at the end, it follows again.
    list.set_scroll_offset(list.max_scroll_offset());
    assert!(list.is_at_end());
}

#[test]
fn reveal_scrolls_a_row_into_view_and_focuses_it() {
    let mut list = plain_rows(1000);
    let _ = lay_out(&mut list, 300.0);
    assert!(list.reveal(&700));
    let _ = lay_out(&mut list, 300.0);
    assert_eq!(list.focused_key(), Some(&700));
    assert!(list.visible_range().contains(&700));
    assert!(!list.reveal(&5000), "an unknown key reveals nothing");
}

#[test]
fn the_key_at_a_point_is_the_row_drawn_there() {
    let mut list = plain_rows(10);
    let _ = lay_out(&mut list, 300.0);
    let middle = |index: usize| {
        Point::new(
            list.rect.center().x,
            list.rect.y0
                + list.viewport.item_view_top(index).unwrap()
                + list.viewport.item_height(index).unwrap() / 2.0,
        )
    };
    let (one, four) = (middle(1), middle(4));
    assert_eq!(list.key_at_point(one), Some(1));
    assert_eq!(list.key_at_point(four), Some(4));
    assert_eq!(
        list.key_at_point(Point::new(list.rect.x0 - 1.0, one.y)),
        None,
        "outside the list is no row"
    );
}

#[test]
fn a_reveal_a_click_asked_for_glides_and_the_wheel_takes_over() {
    use crate::time::testing::with_monotonic_time;
    use crate::tokens::MotionSource;
    let start = crate::time::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    let doc = AutoCommit::new();
    // Rows of about 40 px in a view 200 px tall: row 12 ends some 320
    // px down.
    let mut list = plain_rows(1000);
    let (taffy, node) = lay_out(&mut list, 300.0);
    with_monotonic_time(start, || {
        crate::motion::note_source(MotionSource::Pointer);
        assert!(list.reveal(&12));
    });
    assert_eq!(list.scroll_offset(), 0.0, "it starts where it is");
    assert_eq!(list.focused_key(), Some(&12), "the row is focused at once");
    with_monotonic_time(at(40), || {
        list.update(&doc, &automerge::ROOT);
    });
    let drawn = list.scroll_offset();
    assert!(drawn > 0.0 && drawn < 320.0, "on its way: {drawn}");
    assert_eq!(drawn.fract(), 0.0, "in whole pixels, which copy");
    // The wheel takes over from where the rows are drawn.
    with_monotonic_time(at(40), || {
        let wheel = WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::PixelDelta((0.0, 0.0).into()),
            phase: winit::event::TouchPhase::Moved,
        };
        dispatch(&mut list, &wheel, &taffy, node);
    });
    with_monotonic_time(at(400), || {
        list.update(&doc, &automerge::ROOT);
        assert!(
            !list.reveal_in_progress(at(400)),
            "nothing moves after the wheel"
        );
    });
    assert_eq!(list.scroll_offset(), drawn);

    // Undisturbed, it arrives exactly and asks for nothing more.
    with_monotonic_time(at(1000), || {
        crate::motion::note_source(MotionSource::Pointer);
        assert!(list.reveal(&20));
    });
    // The rows laid out so far stand at their measured heights.
    let bottom_of = |list: &VirtualList<VecListModel<u32, u32>>, index: usize| {
        list.viewport.item_top(index).unwrap() + list.viewport.item_height(index).unwrap()
            - list.viewport.viewport()
    };
    let target = bottom_of(&list, 20);
    with_monotonic_time(at(1400), || {
        list.update(&doc, &automerge::ROOT);
        assert!(!list.reveal_in_progress(at(1400)));
    });
    assert_eq!(list.scroll_offset(), target);

    // A row many views away fades in where it lands.
    with_monotonic_time(at(2000), || {
        crate::motion::note_source(MotionSource::Pointer);
        assert!(list.reveal(&900));
        assert_eq!(
            list.scroll_offset(),
            bottom_of(&list, 900),
            "no scrolling through"
        );
        assert!(list.scroll_to.opacity(at(2000)) < 0.05);
    });
    assert!(list.scroll_to.opacity(at(2400)) >= 1.0);
}

#[test]
fn a_second_click_activates_and_a_right_click_opens_the_rows_menu() {
    let activated = Rc::new(RefCell::new(Vec::new()));
    let menus = Rc::new(RefCell::new(Vec::new()));
    let (a, m) = (Rc::clone(&activated), Rc::clone(&menus));
    let mut list = plain_rows(10)
        .on_activate(move |key| a.borrow_mut().push(key))
        .on_context(move |key, _, rect| m.borrow_mut().push((key, rect)));
    let (taffy, node) = lay_out(&mut list, 300.0);
    let middle = |list: &VirtualList<VecListModel<u32, u32>>, index: usize| {
        list.rect.y0
            + list.viewport.item_view_top(index).unwrap()
            + list.viewport.item_height(index).unwrap() / 2.0
    };
    let row_one = middle(&list, 1);
    let row_three = middle(&list, 3);
    let click = |list: &mut VirtualList<VecListModel<u32, u32>>| {
        dispatch(list, &cursor(row_one), &taffy, node);
        dispatch(list, &left(ElementState::Pressed), &taffy, node);
        dispatch(list, &left(ElementState::Released), &taffy, node);
    };
    click(&mut list);
    assert!(activated.borrow().is_empty(), "one click selects");
    assert!(list.selection().is_selected(&1));
    click(&mut list);
    assert_eq!(*activated.borrow(), [1], "the second click activates");
    dispatch(&mut list, &cursor(row_three), &taffy, node);
    assert!(dispatch(
        &mut list,
        &crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right),
        &taffy,
        node,
    ));
    let menus = menus.borrow();
    assert_eq!(menus.len(), 1);
    assert_eq!(menus[0].0, 3);
    assert!(
        list.selection().is_selected(&3),
        "the row is selected first"
    );
    assert!(menus[0].1.contains(Point::new(20.0, row_three)));
}

#[test]
fn a_selection_delegate_receives_its_neighbours_and_list_rows_join() {
    let model = VecListModel::new((0..6_u32).collect(), |i| *i).unwrap();
    let mut list = VirtualList::with_selection_delegate(model, 40.0, |_, _, key, _| {
        Box::new(crate::widget::ListRow::new(
            format!("r{key}"),
            format!("Row {key}"),
        )) as Element
    })
    .multi_select(true)
    .with_viewport_height(400.0);
    let _ = lay_out(&mut list, 300.0);
    list.selection_mut().select_only(1);
    list.selection_mut().toggle(2);
    list.push_row_selection();
    let joins: Vec<crate::chrome::RowJoin> = list
        .mounted
        .iter_mut()
        .take(4)
        .map(|row| {
            row.widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<crate::widget::ListRow>())
                .unwrap()
                .join()
        })
        .collect();
    assert_eq!(joins[1], crate::chrome::RowJoin::new(false, true));
    assert_eq!(joins[2], crate::chrome::RowJoin::new(true, false));
    assert!(list.row_selection(0).below);
    assert!(list.row_selection(3).above);
}

/// A change the person just made moves the rows it displaced from where
/// they stood (`motion::ListChange`) and fades a row they added in; a
/// change nobody just made (a sync) and a shortcut's result land at
/// once.
#[test]
fn rows_the_person_moves_glide_and_a_sync_lands_at_once() {
    use crate::tokens::MotionSource;
    let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
    let mut list = VirtualList::new(model, 20.0, |_, _, key| {
        Box::new(ProbeRow::new(
            key.to_string(),
            20.0,
            false,
            Rc::new(Cell::new(0)),
        ))
    })
    .with_viewport_height(100.0);
    let _ = lay_out(&mut list, 300.0);
    let motion = Theme::dark().motion();

    // Nobody touched anything: a sync adds a row.
    list.update_model_changes(
        &[ListModelChange::RowsInserted { index: 3, count: 1 }],
        |model| model.push(4, 4).unwrap(),
    );
    let _ = lay_out(&mut list, 300.0);
    list.start_row_motion(motion);
    assert!(list.row_motion.is_empty(), "a sync lands at once");

    // A drag moves the last row to the top: the others move down from
    // where they stood.
    crate::motion::note_source(MotionSource::Pointer);
    list.update_model_changes(
        &[ListModelChange::RowsMoved {
            from: 3,
            to: 0,
            count: 1,
        }],
        |model| assert!(model.move_item(3, 0)),
    );
    let _ = lay_out(&mut list, 300.0);
    list.start_row_motion(motion);
    let now = crate::time::now();
    let first = list.row_motion.get(&1).expect("row 1 moved down");
    assert!(
        first.offset(now) < 0.0,
        "drawn where it stood, above its new place"
    );
    assert!(list.needs_animation_frame());
    let later = now + Duration::from_millis(600);
    assert!(!list.row_motion.is_moving(later), "and lands");

    // A key moves it back: it lands at once.
    crate::motion::note_source(MotionSource::Keyboard);
    list.update_model_changes(
        &[ListModelChange::RowsMoved {
            from: 0,
            to: 3,
            count: 1,
        }],
        |model| assert!(model.move_item(0, 3)),
    );
    assert!(!list.row_motion.has_pending(), "a shortcut's result lands");
}
