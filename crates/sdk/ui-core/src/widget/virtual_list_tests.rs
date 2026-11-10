use super::*;
use crate::time::Duration;
use automerge::AutoCommit;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::testing::Dispatch;
use crate::widget::context_menu::MenuItem;
use taffy::AvailableSpace;

pub(super) struct HugeList {
    pub(super) count: usize,
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

    fn request_range(&mut self, range: Range<usize>, _open: Option<&u64>) {
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
    /// Whether the row says its actions stand at its trailing edge, as a row whose controls
    /// only it places them does.
    trailing_actions: bool,
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
            trailing_actions: false,
        }
    }

    /// The row says where its two controls stand, at its trailing edge, in its own box.
    pub(super) fn with_trailing_actions(mut self) -> Self {
        self.trailing_actions = true;
        self
    }
}

impl crate::widget::RowActionRects for ProbeRow {
    fn row_action_rects(&self) -> Vec<Rect> {
        if !self.trailing_actions {
            return Vec::new();
        }
        let side = self.rect.height().min(20.0) - 4.0;
        let cy = self.rect.center().y;
        (0..2_u32)
            .map(|nth| {
                let right = self.rect.x1 - 2.0 - f64::from(1 - nth) * (side + 2.0);
                Rect::new(right - side, cy - side / 2.0, right, cy + side / 2.0)
            })
            .collect()
    }
}

impl Widget for ProbeRow {
    crate::provides!(dyn crate::widget::RowActionRects);

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
        // A list whose rows open when chosen reports the opening alone: the
        // activation selects the row on its way there, which is no choice of
        // its own to report.
        let opened = Rc::new(RefCell::new(Vec::new()));
        let chosen = Rc::new(RefCell::new(Vec::new()));
        let (opens, chooses) = (Rc::clone(&opened), Rc::clone(&chosen));
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
        .on_select(move |key, _| chooses.borrow_mut().push(key))
        .on_activate(move |key| opens.borrow_mut().push(key));
        let _ = lay_out(&mut list, 300.0);
        assert!(Widget::perform_semantic_action(
            &mut list,
            "key:20",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert!(list.selection().is_selected(&20));
        assert_eq!(&*opened.borrow(), &[20], "the row opens once");
        assert!(
            chosen.borrow().is_empty(),
            "and choosing it is not reported again"
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
            Rect::new(0.0, -10.0, 300.0 - gutter, 14.0),
            "the list's own row box, clear of the bar's gutter",
        );
        // The row is the control its widget states, not a row around one: a reader is handed
        // one node where the list shows one row.
        assert_eq!(
            first.name.as_deref(),
            Some("1"),
            "the row reads as the control it holds",
        );
        assert!(
            first
                .children
                .iter()
                .all(|node| node.role != crate::a11y::Role::Button),
            "the row's own widget is not handed over a second time"
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
        // The row is the button its widget states: one node where the list shows one row.
        assert!(
            semantics.children[0]
                .children
                .iter()
                .all(|node| node.role != crate::a11y::Role::Button),
            "the row's own widget is not handed over a second time"
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
        assert!(!list.scroll.momentum.is_active());

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
        assert!(!list.scroll.momentum.is_active());
        assert!(!list.needs_animation_frame());
    }
}

#[test]
fn a_placement_that_placed_no_row_leaves_the_pointer_the_rows_that_mount() {
    // A scroll that outruns the rows a list mounted asks for a placement of
    // its neighbours: that placement finds no row near the box and places
    // none, so it must not narrow the pointer to nothing. The rows the frame
    // mounts stand under the pointer before they are placed, and a press
    // there is theirs.
    let keys = Rc::new(RefCell::new((0_u64..80).collect::<Vec<_>>()));
    let presses = Rc::new(Cell::new(0));
    let captured = Rc::clone(&presses);
    let mut list = VirtualList::new(
        SharedOrder {
            keys: Rc::clone(&keys),
        },
        20.0,
        move |_, index, _| {
            Box::new(ProbeRow::new(
                format!("row {index}"),
                20.0,
                true,
                Rc::clone(&captured),
            ))
        },
    )
    .with_viewport_height(60.0);
    let (taffy, node) = lay_out(&mut list, 200.0);
    assert!(!list.mounted.is_empty(), "the rows at the top show");
    assert!(list.mounted.len() < 80, "a list mounts only what it shows");

    // The view runs past every mounted row, and the frame places the list
    // again from the layout it has: no row is near the box yet.
    list.set_scroll_offset(1200.0);
    let tree = crate::widget::layout::TreeId::of(&taffy);
    crate::widget::layout::placing_again_in(tree, || {
        list.apply_layout(&taffy, node, Point::ZERO);
    });

    // What that frame mounts is what the pointer now stands on, placed by
    // the next frame: the press is the row's before that placement.
    let mut mounted = TaffyTree::new();
    let mut node_map = HashMap::new();
    let grown = list.build_layout(&mut mounted, &Theme::dark(), &mut node_map);
    assert!(!list.mounted.is_empty(), "the rows at the view show");
    dispatch(&mut list, &cursor(10.0), &mounted, grown);
    dispatch(&mut list, &left(ElementState::Pressed), &mounted, grown);
    assert_eq!(
        presses.get(),
        1,
        "the row the pointer stands on takes its press"
    );
}

#[test]
fn a_press_opens_the_row_of_a_list_that_opens_on_press() {
    let opened = Rc::new(std::cell::RefCell::new(Vec::new()));
    let door = Rc::clone(&opened);
    let model = VecListModel::new((0_u64..3).collect(), |key| *key).unwrap();
    let mut list = VirtualList::new(model, 20.0, |_, _, key| {
        Box::new(ProbeRow::new(
            key.to_string(),
            20.0,
            false,
            Rc::new(Cell::new(0)),
        ))
    })
    .with_viewport_height(60.0)
    .with_semantic_key(ToString::to_string)
    .open_on_press()
    .on_activate_view("probe-list", move |key| door.borrow_mut().push(key));
    let (taffy, node) = lay_out(&mut list, 200.0);

    dispatch(&mut list, &cursor(10.0), &taffy, node);
    dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
    dispatch(&mut list, &left(ElementState::Released), &taffy, node);

    assert_eq!(*opened.borrow(), vec![0], "one press opens the row");
    assert!(
        list.selection().is_selected(&0),
        "the row it opened is the choice"
    );
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

#[test]
fn ctrl_arrows_move_the_focused_row_through_the_reorder_commit() {
    fn send(
        list: &mut VirtualList<VecListModel<u32, u32>>,
        key: NamedKey,
        modifiers: winit::keyboard::ModifiersState,
        (taffy, node): (&TaffyTree<()>, NodeId),
    ) -> bool {
        Dispatch::new()
            .modifiers(modifiers)
            .layout(taffy, node)
            .send(list, &crate::synthetic_input::named_key(key))
            .handled
    }
    let ctrl = winit::keyboard::ModifiersState::CONTROL;
    let alt = winit::keyboard::ModifiersState::ALT;
    let plain = winit::keyboard::ModifiersState::empty();

    let mut list = plain_rows(3).reorderable();
    let (taffy, node) = lay_out(&mut list, 300.0);
    let laid_out = (&taffy, node);
    list.set_focus(true);
    assert!(list.reveal(&0));
    assert!(send(&mut list, NamedKey::ArrowDown, ctrl, laid_out));
    assert_eq!(list.model().items(), &[1, 0, 2], "Ctrl+Down moves it down");
    assert_eq!(list.focused_key(), Some(&0), "the moved row keeps focus");
    assert!(send(&mut list, NamedKey::ArrowUp, alt, laid_out));
    assert_eq!(list.model().items(), &[0, 1, 2], "Alt+Up, as a tree's rows");
    assert!(
        send(&mut list, NamedKey::ArrowUp, ctrl, laid_out),
        "the first row has nowhere up to go, and the list still takes its chord"
    );
    assert_eq!(list.model().items(), &[0, 1, 2]);
    assert_eq!(
        list.focused_key(),
        Some(&0),
        "and a refused move is no navigation"
    );
    assert!(send(&mut list, NamedKey::ArrowDown, plain, laid_out));
    assert_eq!(list.focused_key(), Some(&1), "a plain arrow still walks");
    assert_eq!(list.model().items(), &[0, 1, 2]);

    // A list that does not reorder walks under Ctrl as it always did.
    let mut fixed = plain_rows(3);
    let (taffy, node) = lay_out(&mut fixed, 300.0);
    fixed.set_focus(true);
    assert!(fixed.reveal(&0));
    assert!(send(&mut fixed, NamedKey::ArrowDown, ctrl, (&taffy, node)));
    assert_eq!(fixed.focused_key(), Some(&1));
    assert_eq!(fixed.model().items(), &[0, 1, 2]);
}

/// Enter on a list whose rows open nothing chooses the row, as a click does, and says so
/// once: the app's published view action reaches the keyboard too.
#[test]
fn enter_chooses_a_row_that_opens_nothing() {
    let chosen = Rc::new(RefCell::new(Vec::new()));
    let choosing = Rc::clone(&chosen);
    let mut list = plain_rows(3).on_select(move |key, selected| {
        if selected {
            choosing.borrow_mut().push(key);
        }
    });
    let (taffy, node) = lay_out(&mut list, 300.0);
    list.set_focus(true);
    assert!(list.reveal(&1));
    assert!(
        Dispatch::new()
            .layout(&taffy, node)
            .send(
                &mut list,
                &crate::synthetic_input::named_key(NamedKey::Enter)
            )
            .handled
    );
    assert!(list.selection().is_selected(&1));
    assert_eq!(&*chosen.borrow(), &[1], "chosen once");
}

/// Shift and an arrow grow a choice of rows from where it began, and each step tells the
/// listener what is chosen, in the order the rows stand; Escape puts the choice away and
/// says so.
#[test]
fn shift_arrows_extend_a_choice_and_escape_puts_it_away() {
    let reported = Rc::new(RefCell::new(Vec::<Vec<u32>>::new()));
    let reports = Rc::clone(&reported);
    let mut list = plain_rows(6)
        .multi_select(true)
        .on_selected(move |keys| reports.borrow_mut().push(keys.to_vec()));
    let (taffy, node) = lay_out(&mut list, 300.0);
    list.set_focus(true);
    // As a click on the second row leaves the list: the keyboard stands there and the
    // choice begins there.
    assert!(list.reveal(&1));
    list.selection_mut().select_only(1);
    let shift = winit::keyboard::ModifiersState::SHIFT;
    for _ in 0..2 {
        assert!(
            Dispatch::new()
                .modifiers(shift)
                .layout(&taffy, node)
                .send(
                    &mut list,
                    &crate::synthetic_input::named_key(NamedKey::ArrowDown)
                )
                .handled,
            "the list takes the extended walk"
        );
    }
    assert_eq!(
        &*reported.borrow(),
        &[vec![1, 2], vec![1, 2, 3]],
        "each step tells the listener the whole choice, in row order"
    );
    assert!(
        list.selection().is_selected(&3),
        "the row the keyboard walked to is chosen"
    );
    assert!(
        Dispatch::new()
            .layout(&taffy, node)
            .send(
                &mut list,
                &crate::synthetic_input::named_key(NamedKey::Escape)
            )
            .handled,
        "Escape belongs to a list that holds a choice"
    );
    assert_eq!(
        reported.borrow_mut().pop(),
        Some(Vec::new()),
        "the choice is put away, and the listener hears of it"
    );
    assert!(
        list.selection().is_empty(6),
        "an empty choice, not one merely hidden"
    );
}

/// A click that changes a choice by its modifier changes the choice alone: it does not tell
/// the row's listener the row was chosen, so a list whose choosing opens a page does not open
/// one for every row a person adds to a choice. A plain click still opens the row it lands on.
#[test]
fn a_modified_click_changes_the_choice_without_opening_the_row() {
    let opened = Rc::new(RefCell::new(Vec::<u32>::new()));
    let reported = Rc::new(RefCell::new(Vec::<Vec<u32>>::new()));
    let (opens, reports) = (Rc::clone(&opened), Rc::clone(&reported));
    let mut list = plain_rows(4)
        .multi_select(true)
        .on_select(move |key, _| opens.borrow_mut().push(key))
        .on_selected(move |keys| reports.borrow_mut().push(keys.to_vec()));
    let ctrl = winit::keyboard::ModifiersState::CONTROL;
    click_row(&mut list, 1, ctrl);
    assert_eq!(
        list.selection().stored_keys().copied().collect::<Vec<_>>(),
        vec![1],
        "the modified click chooses the row it lands on"
    );
    click_row(&mut list, 3, ctrl);
    let mut chosen: Vec<u32> = list.selection().stored_keys().copied().collect();
    chosen.sort_unstable();
    assert_eq!(chosen, vec![1, 3], "and the next one adds to the choice");
    assert!(
        opened.borrow().is_empty(),
        "a click that chooses as part of a choice opens nothing: {:?}",
        opened.borrow()
    );
    assert_eq!(
        &*reported.borrow(),
        &[vec![1], vec![1, 3]],
        "a choice of one row is kept, so the next click adds to it"
    );
    click_row(&mut list, 2, winit::keyboard::ModifiersState::empty());
    assert_eq!(&*opened.borrow(), &[2], "a plain click opens its row");
    assert_eq!(
        list.selection().stored_keys().copied().collect::<Vec<_>>(),
        vec![2],
        "and leaves a choice of that row alone"
    );
}

/// How a person gathers a few rows: click one, add the next with Control held, then walk
/// with Shift and the arrow. Every step reports the choice; the walk extends the rows they
/// gathered instead of replacing them, so the choice the window's bar stands for grows.
#[test]
fn shift_and_the_arrow_extend_the_choice_a_modified_click_gathered() {
    let reported = Rc::new(RefCell::new(Vec::<Vec<u32>>::new()));
    let reports = Rc::clone(&reported);
    let mut list = plain_rows(6)
        .selection_follows_focus()
        .multi_select(true)
        .on_selected(move |keys| reports.borrow_mut().push(keys.to_vec()));
    click_row(&mut list, 0, winit::keyboard::ModifiersState::empty());
    click_row(&mut list, 1, winit::keyboard::ModifiersState::CONTROL);
    list.set_focus(true);
    let (taffy, node) = lay_out(&mut list, 300.0);
    let shift = winit::keyboard::ModifiersState::SHIFT;
    for _ in 0..2 {
        assert!(
            Dispatch::new()
                .modifiers(shift)
                .layout(&taffy, node)
                .send(
                    &mut list,
                    &crate::synthetic_input::named_key(NamedKey::ArrowDown)
                )
                .handled,
            "the list takes the extended walk"
        );
    }
    assert_eq!(
        &*reported.borrow(),
        &[vec![0], vec![0, 1], vec![0, 1, 2], vec![0, 1, 2, 3]],
        "the walk grows the choice the modified click added to, one row per step"
    );
}

/// Click `key`'s row with `modifiers` held, as a person's pointer does.
fn click_row(
    list: &mut VirtualList<VecListModel<u32, u32>>,
    key: u32,
    modifiers: winit::keyboard::ModifiersState,
) {
    let (taffy, node) = lay_out(list, 300.0);
    let at = list.row_rect(&key).expect("the row is placed").center();
    let dispatch = |list: &mut VirtualList<VecListModel<u32, u32>>, event: &WindowEvent| {
        Dispatch::new()
            .modifiers(modifiers)
            .layout(&taffy, node)
            .send(list, event)
    };
    dispatch(
        list,
        &WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: (at.x, at.y).into(),
        },
    );
    dispatch(list, &crate::synthetic_input::mouse_press());
    dispatch(list, &crate::synthetic_input::mouse_release());
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

/// A list that follows its end stands a short list's rows down at its foot; the box the list
/// draws for a row — the lift a selection takes, the band over a group, the seam a drop lands
/// on — stands down there with it. A lift at the row's top instead paints a filled block where
/// the row would have been if the list were full, with nothing in it.
#[test]
fn a_list_following_its_end_lifts_the_row_where_it_stands() {
    let mut list = VirtualList::from_items(
        (0..3_u32).collect(),
        40.0,
        |item| *item,
        |_, _, _| Box::new(crate::widget::Spacer::fixed(40.0)) as Element,
    )
    .unwrap()
    .with_viewport_height(200.0)
    .following_end();
    let _ = lay_out(&mut list, 300.0);
    assert!(
        list.end_slack() > 40.0,
        "a short list's rows stand at the foot: {}",
        list.end_slack()
    );
    for row in &list.mounted {
        let stands = row.widget.get_rect();
        let key = list
            .model
            .key_at(row.index)
            .expect("a mounted row has a key");
        let lift = list.row_rect(&key).expect("a row in view has a box");
        assert!(
            (lift.y0 - stands.y0).abs() < 0.5 && (lift.y1 - stands.y1).abs() < 0.5,
            "row {} stands at {stands:?} and its lift is drawn at {lift:?}",
            row.index
        );
    }
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
        assert!(list.scroll.to.opacity(at(2000)) < 0.05);
    });
    assert!(list.scroll.to.opacity(at(2400)) >= 1.0);
}

/// What a person's right click does: the window notes the gesture, the row takes the press and
/// its menu opens with the typing, and the release leaves it open.
fn right_click_row<M>(list: &mut VirtualList<M>, row: usize)
where
    M: ListModel + 'static,
{
    use winit::event::{ElementState, MouseButton};
    let (taffy, node) = lay_out(list, 300.0);
    let at = list.rect.y0
        + list.viewport.item_view_top(row).unwrap()
        + list.viewport.item_height(row).unwrap() / 2.0;
    let press = crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right);
    crate::widget::context_menu::note_input(&press);
    dispatch(list, &cursor(at), &taffy, node);
    dispatch(list, &press, &taffy, node);
    dispatch(
        list,
        &crate::synthetic_input::mouse_input(ElementState::Released, MouseButton::Right),
        &taffy,
        node,
    );
}

/// A row's menu carries what the app says the row is about: the row that sends it opens the
/// send-to box on that subject, in the menu's place. A list that names nothing for its rows
/// opens the same menu with no subject.
#[test]
fn a_rows_menu_carries_what_the_app_names_for_the_row() {
    use crate::shared_selection::{SharedSelection, set_send_host_available};
    set_send_host_available(true);
    fn open_item(_: &VecListModel<u32, u32>, _: Option<&u32>) -> Vec<MenuItem> {
        vec![MenuItem::action("Open", || {})]
    }
    fn send_to_of(
        list: &mut VirtualList<VecListModel<u32, u32>>,
        typed: &str,
    ) -> Vec<SharedSelection> {
        let menu = list.menus.owned_mut().expect("the list owns its menu");
        if !typed.is_empty() {
            menu.context_menu_mut().set_query(typed);
        }
        let send_at = menu.context_menu().items().len() - 1;
        menu.context_menu_mut().click_item(send_at);
        let menu = menu.context_menu();
        assert!(!menu.is_open(), "the box takes the menu's place");
        assert!(menu.send_to_alone_open(), "the box stands alone");
        menu.send_to().subjects().to_vec()
    }
    let mut list = plain_rows(10)
        .with_context_menu(open_item, |_, _| {})
        .with_row_about(|key: &u32| Some(SharedSelection::of_text(&format!("issue {key}"))));
    right_click_row(&mut list, 3);
    let subjects = send_to_of(&mut list, "a note");
    assert_eq!(subjects.len(), 1, "the menu is about the row the app named");
    assert_eq!(
        subjects[0].anchor,
        SharedSelection::of_text("issue 3").anchor
    );
    // With the menu gone, the box is what the list hands the keyboard and the
    // pointer to: what the person types next lands in its message.
    assert!(
        list.send_to_alone_open(),
        "the box stands where the menu did"
    );
    assert!(
        list.modal_capture().is_some(),
        "the box hears the pointer while it stands"
    );
    let (taffy, node) = lay_out(&mut list, 300.0);
    dispatch(
        &mut list,
        &crate::synthetic_input::char_key("!"),
        &taffy,
        node,
    );
    let text = list
        .menus
        .owned()
        .expect("the list owns its menu")
        .context_menu()
        .send_to()
        .text()
        .to_owned();
    assert_eq!(text, "a note!", "the box takes the typing");

    let mut plain = plain_rows(3).with_context_menu(open_item, |_, _| {});
    right_click_row(&mut plain, 0);
    assert!(
        send_to_of(&mut plain, "").is_empty(),
        "a row the app does not name opens a menu with no subject"
    );
    set_send_host_available(false);
}

/// A second click activates and a right click opens the rows menu.
#[test]
fn a_second_click_activates_and_a_right_click_opens_the_rows_menu() {
    let activated = Rc::new(RefCell::new(Vec::new()));
    let menus = Rc::new(RefCell::new(Vec::new()));
    let (a, m) = (Rc::clone(&activated), Rc::clone(&menus));
    let mut list = plain_rows(10)
        .on_activate(move |key| a.borrow_mut().push(key))
        .with_context_menu(
            move |_: &VecListModel<u32, u32>, key| {
                m.borrow_mut().push(key.copied());
                vec![MenuItem::action("Open", || {})]
            },
            |_, _| {},
        );
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
    assert_eq!(menus.as_slice(), [Some(3)]);
    assert!(
        list.selection().is_selected(&3),
        "the row is selected first"
    );
    assert!(list.context_menu_is_open());
    assert!(
        list.modal_capture()
            .is_some_and(|menu| menu.contains(Point::new(20.0, row_three))),
        "the menu stands where the press was"
    );
}

#[test]
fn a_right_click_opened_rows_menu_offers_to_send_after_the_release() {
    use crate::shared_selection::{SharedSelection, set_send_host_available};

    set_send_host_available(true);
    let mut list = plain_rows(10)
        .with_context_menu(
            |_: &VecListModel<u32, u32>, _| vec![MenuItem::action("Open issue", || {})],
            |_, _| {},
        )
        .with_row_about(|key: &u32| Some(SharedSelection::of_text(&format!("issue {key}"))));
    right_click_row(&mut list, 3);
    assert!(list.context_menu_is_open(), "the menu stays open");
    let menu = list.menus.owned_mut().expect("the list owns its menu");
    assert!(
        !menu.context_menu().send_to().is_open(),
        "the menu stands alone until the row that sends is chosen"
    );
    let send_at = menu.context_menu().items().len() - 1;
    menu.context_menu_mut().click_item(send_at);
    let menu = menu.context_menu();
    assert!(!menu.is_open(), "the box takes the menu's place");
    assert!(
        menu.send_to_alone_open(),
        "the box is what takes the typing"
    );
    assert_eq!(menu.send_to().subjects().len(), 1);
    assert_eq!(
        menu.send_to().subjects()[0].anchor,
        SharedSelection::of_text("issue 3").anchor
    );
    set_send_host_available(false);
}

#[test]
fn the_menu_key_and_shift_f10_open_the_focused_rows_menu() {
    let menus = Rc::new(RefCell::new(Vec::new()));
    let m = Rc::clone(&menus);
    let mut list = plain_rows(10).with_context_menu(
        move |_: &VecListModel<u32, u32>, key| {
            m.borrow_mut().push(key.copied());
            vec![MenuItem::action("Open", || {})]
        },
        |_, _| {},
    );
    let (taffy, node) = lay_out(&mut list, 300.0);
    list.set_focus(true);
    assert!(list.reveal(&2));
    let send = |list: &mut VirtualList<VecListModel<u32, u32>>,
                event: &WindowEvent,
                modifiers: winit::keyboard::ModifiersState| {
        Dispatch::new()
            .modifiers(modifiers)
            .layout(&taffy, node)
            .send(list, event)
            .handled
    };
    let menu = crate::synthetic_input::named_key(NamedKey::ContextMenu);
    assert!(send(
        &mut list,
        &menu,
        winit::keyboard::ModifiersState::empty()
    ));
    assert_eq!(menus.borrow().len(), 1);
    assert_eq!(menus.borrow()[0], Some(2), "the row the keyboard is on");
    list.close_context_menu();
    let f10 = crate::synthetic_input::named_key(NamedKey::F10);
    assert!(send(
        &mut list,
        &f10,
        winit::keyboard::ModifiersState::SHIFT
    ));
    assert_eq!(menus.borrow().len(), 2, "Shift+F10, as a tree's rows");
    assert_eq!(menus.borrow()[1], Some(2));
    list.close_context_menu();
    assert!(
        !send(&mut list, &f10, winit::keyboard::ModifiersState::empty()),
        "F10 alone is no menu"
    );
    assert_eq!(menus.borrow().len(), 2);

    // A row with no menu attached offers none, and the key stays the window's.
    let mut plain = plain_rows(3);
    let (taffy, node) = lay_out(&mut plain, 300.0);
    plain.set_focus(true);
    assert!(plain.reveal(&0));
    assert!(
        !Dispatch::new()
            .layout(&taffy, node)
            .send(&mut plain, &menu)
            .handled
    );
}

/// A list set up to report its row menus hands its host the row's key, its box and the point
/// the menu opens at, and leaves the selection to the host's menu.
#[test]
fn a_reported_row_menu_hands_the_host_the_row_and_where_it_opens() {
    let reports: Rc<RefCell<Vec<(u32, Rect, Point)>>> = Rc::new(RefCell::new(Vec::new()));
    let r = Rc::clone(&reports);
    let mut list = plain_rows(10).with_row_menu_reporter(move |key, row, at| {
        r.borrow_mut().push((key, row, at));
    });
    let (taffy, node) = lay_out(&mut list, 300.0);
    let middle = |list: &VirtualList<VecListModel<u32, u32>>, index: usize| {
        list.rect.y0
            + list.viewport.item_view_top(index).unwrap()
            + list.viewport.item_height(index).unwrap() / 2.0
    };
    let row_three = middle(&list, 3);
    dispatch(&mut list, &cursor(row_three), &taffy, node);
    assert!(dispatch(
        &mut list,
        &crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right),
        &taffy,
        node,
    ));
    {
        let reports = reports.borrow();
        assert_eq!(reports.len(), 1, "one report for the press");
        let (key, row, at) = reports[0];
        assert_eq!(key, 3, "the row under the pointer");
        assert_eq!(Some(row), list.row_rect(&3), "its box, to anchor the menu");
        assert_eq!(at, Point::new(20.0, row_three), "where the press was");
        assert!(
            !list.selection().is_selected(&3),
            "the host's menu decides what it acts on"
        );
        assert!(!list.context_menu_is_open(), "no menu of the list's own");
    }

    // The Menu key on the focused row reports it too, at the middle of its row.
    list.set_focus(true);
    assert!(list.reveal(&5));
    let menu = crate::synthetic_input::named_key(NamedKey::ContextMenu);
    assert!(
        Dispatch::new()
            .layout(&taffy, node)
            .send(&mut list, &menu)
            .handled
    );
    let reports = reports.borrow();
    assert_eq!(reports.len(), 2);
    let (key, row, at) = reports[1];
    assert_eq!(key, 5, "the row the keyboard is on");
    assert_eq!(Some(row), list.row_rect(&5));
    assert_eq!(at, row.center(), "at the middle of the row");
}

/// A list offers its rows one menu: setting the menu the list builds replaces the host's
/// report, and the other way round.
#[test]
fn a_list_has_one_row_menu() {
    let reports = Rc::new(Cell::new(0));
    let reported = Rc::clone(&reports);
    let handled = Rc::new(RefCell::new(Vec::new()));
    let chosen = Rc::clone(&handled);
    let mut list = plain_rows(4)
        .with_row_menu_reporter(move |_, _, _| reported.set(reported.get() + 1))
        .with_context_menu(
            move |_, key| {
                chosen.borrow_mut().push(key.copied());
                vec![MenuItem::action("Open", || {})]
            },
            |_, _| {},
        );
    let (taffy, node) = lay_out(&mut list, 300.0);
    let row_one = list.row_rect(&1).expect("row one").center().y;
    dispatch(&mut list, &cursor(row_one), &taffy, node);
    assert!(dispatch(
        &mut list,
        &crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right),
        &taffy,
        node,
    ));
    assert_eq!(reports.get(), 0, "the host no longer hears of it");
    assert_eq!(*handled.borrow(), [Some(1)], "the list builds the menu");
    assert!(list.context_menu_is_open());
}

/// The list asks the mounted row where its actions stand, so a row the crate does not build
/// still says where a script presses one.
#[test]
fn row_action_rect_asks_the_mounted_row() {
    let mut list = VirtualList::new(
        VecListModel::new((0..3_u32).collect(), |i| *i).unwrap(),
        40.0,
        |_, _, key| {
            let row = ProbeRow::new(key.to_string(), 40.0, false, Rc::new(Cell::new(0)));
            Box::new(if *key == 1 {
                row.with_trailing_actions()
            } else {
                row
            }) as Element
        },
    )
    .with_viewport_height(200.0);
    let _ = lay_out(&mut list, 300.0);
    let row = list.row_rect(&1).expect("row one is placed");
    let second = list.row_action_rect(&1, 1).expect("its second action");
    assert!(
        row.contains(second.center()),
        "in the row's own box: {row:?} vs {second:?}"
    );
    let first = list.row_action_rect(&1, 0).expect("its first action");
    assert!(first.x1 <= second.x0, "in the order the row shows them");
    assert_eq!(list.row_action_rect(&1, 2), None, "a third is no action");
    assert_eq!(
        list.row_action_rect(&0, 0),
        None,
        "a row without actions says none"
    );
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

/// A delegate whose row a view wraps, as `std.list` gives every row the swipe its item offers:
/// the list still hands the row its selection, in place.
#[test]
fn a_selection_delegate_wrapped_in_its_swipe_takes_its_selection() {
    let model = VecListModel::new((0..3_u32).collect(), |i| *i).unwrap();
    let mut list = VirtualList::with_selection_delegate(model, 40.0, |_, _, key, _| {
        let row = crate::widget::ListRow::new(format!("r{key}"), format!("Row {key}"));
        Box::new(crate::widget::SwipeRow::new(Box::new(row), None, None)) as Element
    })
    .with_viewport_height(400.0);
    let _ = lay_out(&mut list, 300.0);
    list.selection_mut().select_only(1);
    list.push_row_selection();
    let selected: Vec<bool> = list
        .mounted
        .iter_mut()
        .map(|row| {
            row.widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<crate::widget::SwipeRow>())
                .expect("the row's swipe")
                .content()
                .as_any()
                .and_then(|any| any.downcast_ref::<crate::widget::ListRow>())
                .expect("the row under its swipe")
                .is_selected()
        })
        .collect();
    assert_eq!(selected, [false, true, false], "only the chosen row is");
}

/// A finger's tap on a row wrapped in its swipe is a click on that row all the same: the swipe
/// holds the press, the finger lifts where it came down, and the list takes the row.
#[test]
fn a_tap_on_a_row_wrapped_in_its_swipe_is_its_click() {
    let mut list = VirtualList::from_items(
        vec![0_u32, 1, 2],
        40.0,
        |item| *item,
        |item, _, _| {
            let row = Box::new(crate::widget::Text::new(&format!("row {item}"))) as Element;
            Box::new(crate::widget::SwipeRow::new(row, None, None)) as Element
        },
    )
    .unwrap()
    .with_viewport_height(200.0);
    let (taffy, node) = lay_out(&mut list, 300.0);
    let at = list.row_rect(&1).expect("the row is placed").center();
    let send = |list: &mut VirtualList<VecListModel<u32, u32>>, event: &WindowEvent| {
        Dispatch::new()
            .layout(&taffy, node)
            .send(list, event)
            .handled
    };
    let touch = |phase, at: Point| {
        WindowEvent::Touch(winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(at.x, at.y),
            force: None,
            id: 7,
        })
    };
    // The finger comes down on the row and lifts where it came down: the platform turns that
    // into the pointer's press and release at the same place.
    send(&mut list, &touch(winit::event::TouchPhase::Started, at));
    send(&mut list, &crate::synthetic_input::cursor_moved(at.x, at.y));
    send(&mut list, &crate::synthetic_input::mouse_press());
    send(&mut list, &crate::synthetic_input::mouse_release());
    send(&mut list, &touch(winit::event::TouchPhase::Ended, at));
    assert!(
        list.selection().is_selected(&1),
        "the tapped row is the list's choice, not the swipe's"
    );
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

/// A list among a page's other content stands as tall as its rows: every row shows, and what
/// follows it stands after its last row, never over it.
#[test]
fn a_list_fitting_its_rows_is_as_tall_as_they_are() {
    fn find<'a>(
        node: &'a crate::headless::InspectionNode,
        widget: &str,
    ) -> Option<&'a crate::headless::InspectionNode> {
        if node.widget == widget {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, widget))
    }
    let list = VirtualList::new(HugeList { count: 5 }, 24.0, |_, _, key| {
        Box::new(crate::widget::Button::new(&key.to_string(), "Step"))
    })
    .fitting_rows();
    let page = crate::widget::Column::new()
        .push(Box::new(list))
        .push(Box::new(crate::widget::Text::new("Notes after the steps")));
    let mut h = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        page,
        (300, 800),
        crate::theme::Theme::light(),
    );
    // The frames a live window draws while the rows settle their heights.
    for _ in 0..crate::widget::SETTLE_FRAMES {
        h.relayout();
    }
    let inspection = h.inspection();
    let list = find(&inspection.root, "VirtualList").expect("the list");
    let row_height = list.children.first().expect("a row").rect[3];
    assert_eq!(list.children.len(), 5, "every row shows");
    assert!(
        (list.rect[3] - 5.0 * row_height).abs() < 1.0,
        "the list is as tall as its five rows: {:?}",
        list.rect
    );
    let after = find(&inspection.root, "Text").expect("the text after it");
    assert!(
        after.rect[1] >= list.rect[1] + list.rect[3] - 0.5,
        "what follows stands after the last row: list {:?}, text {:?}",
        list.rect,
        after.rect
    );
}

/// A list as tall as its rows settles on the height they come to even when the layout reads it
/// a fraction of a pixel higher: the box it is given is that reading, not a height of its own to
/// ask for again, or the window would lay itself out for ever.
#[test]
fn a_list_as_tall_as_its_rows_asks_for_the_height_its_rows_come_to_once() {
    // The rows come to 580.35; the layout gave a box of 581 and the viewport already stands at
    // the rows' height: nothing to ask for.
    assert!(!fit_needs_layout(581.0, 580.35, 580.35));
    // Rows that measured taller than the box the list stands in: the list grows to them.
    assert!(fit_needs_layout(506.0, 580.35, 506.0));
    // Rows that shrank under a box the viewport still stands at: the list shrinks to them.
    assert!(fit_needs_layout(680.0, 673.0, 680.0));
}

/// A transcript shows the way back it owes its reader: to the first unread row while that is
/// above the view, and to what is being said now while it has nothing unread but is not at its
/// end. Taking it moves the reader, and where it moved them to changes what it offers next.
#[test]
fn a_transcript_offers_the_way_to_what_is_unread_or_to_what_is_now() {
    use crate::widget::conversation::Jump;
    let mut list = VirtualList::from_items(
        (0..300_u32).collect(),
        40.0,
        |item| *item,
        |_, _, _| Box::new(crate::widget::Spacer::fixed(40.0)) as Element,
    )
    .unwrap()
    .with_viewport_height(200.0)
    .following_end()
    .with_id("transcript")
    .unread_from(Some("250".to_owned()));
    list.set_key_presenting(|id: &str| id.parse::<u32>().ok());
    let (taffy, node) = lay_out(&mut list, 300.0);
    // The unread row is below the view (the list opens at its end), so the pill offers what is
    // now only when there is nothing unread: here it offers the way back to the row.
    assert_eq!(list.jump(), Some(Jump::Unread));
    assert!(list.pill_rect().is_some(), "the pill has a rectangle");
    // Taking it brings the row into view, and it stops offering that way.
    let _ = dispatch(
        &mut list,
        &crate::synthetic_input::named_key(NamedKey::Home),
        &taffy,
        node,
    );
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
    list.jump_to_unread();
    let _ = lay_out(&mut list, 300.0);
    assert!(
        list.visible_range().contains(&250),
        "the unread row shows: {:?}",
        list.visible_range()
    );
    assert_eq!(list.jump(), None, "nothing more to go to");

    // A transcript with nothing unread, read into its history, offers what is now.
    let mut reading = VirtualList::from_items(
        (0..300_u32).collect(),
        40.0,
        |item| *item,
        |_, _, _| Box::new(crate::widget::Spacer::fixed(40.0)) as Element,
    )
    .unwrap()
    .with_viewport_height(200.0)
    .following_end();
    let _ = lay_out(&mut reading, 300.0);
    assert_eq!(reading.jump(), None, "at its end with nothing unread");
    reading.set_scroll_offset(reading.scroll_offset() - 400.0);
    let _ = lay_out(&mut reading, 300.0);
    assert_eq!(reading.jump(), Some(Jump::Present));
    reading.jump_to_present();
    let _ = lay_out(&mut reading, 300.0);
    assert!(reading.is_at_end(), "it went back to the newest row");
    assert_eq!(reading.jump(), None);
}

/// A transcript's page column: rows of a page, each as tall as its words, and a long enough
/// column to stand at its end rather than its start.
fn page_column() -> VirtualList<VecListModel<u32, u32>> {
    use crate::tokens::Rhythm;
    use crate::widget::Text;
    use crate::widget::arrange::{Page, Slot, Stack};
    VirtualList::from_items(
        (0..40_u32).collect(),
        64.0,
        |item| *item,
        |item, _, _| {
            let said = Stack::stack()
                .gap(Rhythm::Tight)
                .push(Box::new(Text::new(&format!("message {item}"))))
                .push(Box::new(Text::new(
                    "A longer line of what somebody said, wrapping over a couple of lines in a \
                     narrow window so the rows differ in height",
                )));
            let row = Page::full().row_after(Rhythm::Section);
            Box::new(row.push_slot(Slot::new(Box::new(said) as Element))) as Element
        },
    )
    .unwrap()
    .filling()
    .following_end()
}

/// Every box of the window, outermost first.
fn boxes(widget: &dyn Widget, out: &mut Vec<(String, vello::kurbo::Rect)>) {
    out.push((widget.name().to_owned(), widget.get_rect()));
    for child in widget.children() {
        boxes(child, out);
    }
}

/// The edges of the window's boxes that stand between `scale`'s device pixels, said one by one.
fn off_device_pixels(widget: &dyn Widget, scale: f64) -> Vec<String> {
    let mut laid = Vec::new();
    boxes(widget, &mut laid);
    assert!(laid.len() > 10, "the window has boxes: {laid:?}");
    laid.iter()
        .flat_map(|(name, rect)| {
            [rect.x0, rect.y0, rect.x1, rect.y1]
                .into_iter()
                .filter(|edge| (edge * scale - (edge * scale).round()).abs() > 1e-3)
                .map(move |edge| format!("{name} {rect:?}: {edge} at {scale}x"))
        })
        .collect()
}

/// Every edge of every box in the window stands on a device pixel at `scale`, whatever the
/// window's history: the scale a person's display runs at when the window is opened at it,
/// and the scale it changes to under a window already laid out.
#[test]
fn a_window_laid_out_at_a_scale_stands_on_its_device_pixels() {
    for scale in [1.25_f64, 1.5, 2.0] {
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            page_column(),
            (390, 844),
            Theme::light(),
        );
        harness.set_input_scale(scale).expect("a display scale");
        harness.relayout();
        let off = off_device_pixels(harness.root_widget(), scale);
        assert!(
            off.is_empty(),
            "laid out at {scale}x, {} edges off: {}",
            off.len(),
            off.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
        );

        // A display the window moves to: the rows were measured on the grid it came from.
        let to = if scale == 1.5 { 2.0 } else { 1.5 };
        harness.set_input_scale(to).expect("a display scale");
        // Scrolled where a gesture, a glide to a match or a restore left it: a scroll offset
        // is a length in logical pixels, and not every one of them is a device pixel.
        {
            let mut list = harness.root_mut::<VirtualList<VecListModel<u32, u32>>>();
            let offset = list.scroll_offset();
            list.set_scroll_offset(offset - 17.0);
        }
        harness.relayout();
        let off = off_device_pixels(harness.root_widget(), to);
        assert!(
            off.is_empty(),
            "moved from {scale}x to {to}x, {} edges off: {}",
            off.len(),
            off.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
        );
    }
}

/// The exported node `id`, as the reader is handed it.
pub(super) fn exported(
    nodes: &[(accesskit::NodeId, accesskit::Node)],
    id: accesskit::NodeId,
) -> accesskit::Node {
    nodes
        .iter()
        .find(|(each, _)| *each == id)
        .map(|(_, node)| node.clone())
        .unwrap_or_else(|| panic!("no node {id:?} in the export"))
}

/// Every node handed to a reader under `id`, depth first, with its id.
pub(super) fn inside(
    nodes: &[(accesskit::NodeId, accesskit::Node)],
    id: accesskit::NodeId,
) -> Vec<(accesskit::NodeId, accesskit::Node)> {
    let mut out = Vec::new();
    let mut frontier = vec![id];
    while let Some(next) = frontier.pop() {
        for child in exported(nodes, next).children() {
            out.push((*child, exported(nodes, *child)));
            frontier.push(*child);
        }
    }
    out
}

/// The one node of `nodes` with `role` named `label`, and its id.
pub(super) fn named(
    nodes: &[(accesskit::NodeId, accesskit::Node)],
    role: accesskit::Role,
    label: &str,
) -> (accesskit::NodeId, accesskit::Node) {
    nodes
        .iter()
        .find(|(_, node)| node.role() == role && node.label() == Some(label))
        .cloned()
        .unwrap_or_else(|| panic!("no {role:?} named {label:?}"))
}

/// Nothing of `covered` stands outside `within`.
pub(super) fn within(within: accesskit::Rect, covered: accesskit::Rect) -> bool {
    covered.x0 >= within.x0
        && covered.y0 >= within.y0
        && covered.x1 <= within.x1
        && covered.y1 <= within.y1
}

/// A row states its words, and the controls inside it are worked where they stand: a reader
/// reaches a closed row's box and ticks it off without the row being opened.
#[test]
fn a_closed_rows_own_controls_are_where_the_row_is() {
    use crate::a11y::Action;
    use crate::headless::FrameHarness;
    use crate::widget::{Checkbox, Column, Text};
    use accesskit::Role as AkRole;

    let done = crate::LocalState::new(false);
    let model = VecListModel::new(vec![7_u64], |key| *key).unwrap();
    let state = done.clone();
    let list = VirtualList::new(model, 40.0, move |_, _, _| {
        Box::new(
            Column::new()
                .push(Box::new(Text::new("Buy oat milk")))
                .push(Box::new(
                    Checkbox::from_state(&state).with_accessible_name("Done"),
                )),
        ) as Element
    })
    .with_viewport_height(200.0);
    let mut harness = FrameHarness::for_root(crate::State::new(), list, (300, 400), Theme::dark());

    let nodes = harness.a11y_nodes();
    let (row_id, row) = named(&nodes, AkRole::ListItem, "Buy oat milk");
    let row_bounds = row.bounds().expect("the row stands where it is drawn");
    let controls = inside(&nodes, row_id);
    let (checkbox, _) = named(&controls, AkRole::CheckBox, "Done");
    let bounds = exported(&nodes, checkbox)
        .bounds()
        .expect("the box stands where it is drawn");
    assert!(
        within(row_bounds, bounds),
        "the box stands inside its row: {bounds:?} not in {row_bounds:?}"
    );

    harness
        .perform_a11y_action(checkbox, &Action::Activate)
        .expect("the box is operated");
    assert!(done.with(|checked| *checked), "the tick reaches the box");
}

/// A row that states a box of its own — as every list row does — says it once: the row and the
/// box it carries are the only nodes handed over, and the box is worked where the row says.
#[test]
fn a_closed_rows_checkbox_is_one_control_under_one_row() {
    use crate::a11y::Action;
    use crate::headless::FrameHarness;
    use crate::widget::ListRow;
    use accesskit::Role as AkRole;

    let toggled = Rc::new(Cell::new(None::<bool>));
    let heard = Rc::clone(&toggled);
    let model = VecListModel::new(vec![7_u64], |key| *key).unwrap();
    let list = VirtualList::new(model, 36.0, move |_, _, key| {
        Box::new(
            ListRow::new(key.to_string(), "Buy oat milk").checkbox(false, {
                let heard = Rc::clone(&heard);
                move |on| heard.set(Some(on))
            }),
        ) as Element
    })
    .with_viewport_height(200.0);
    let mut harness = FrameHarness::for_root(crate::State::new(), list, (300, 400), Theme::dark());

    let nodes = harness.a11y_nodes();
    let (row_id, row) = named(&nodes, AkRole::ListItem, "Buy oat milk");
    let controls = inside(&nodes, row_id);
    assert!(
        controls
            .iter()
            .all(|(_, node)| !matches!(node.role(), AkRole::ListItem | AkRole::TreeItem)),
        "the row is one node, not a row inside a row"
    );
    let (checkbox, box_node) = controls
        .iter()
        .find(|(_, node)| node.role() == AkRole::CheckBox)
        .cloned()
        .expect("the row hands over its box");
    assert_eq!(box_node.toggled(), Some(accesskit::Toggled::False));
    assert!(row.bounds().is_some());

    harness
        .perform_a11y_action(checkbox, &Action::Activate)
        .expect("the box is operated");
    assert_eq!(toggled.get(), Some(true), "the tick reaches the row's box");
}
