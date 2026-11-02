use super::*;
use crate::testing::{Dispatch, dispatch};
use automerge::AutoCommit;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use taffy::TaffyTree;
use taffy::prelude::TaffyMaxContent;

fn sample_tree() -> TreeNode<()> {
    TreeNode::branch(
        1,
        "root",
        (),
        vec![
            TreeNode::leaf(2, "child1", ()),
            TreeNode::leaf(3, "child2", ()),
        ],
    )
}

/// Choosing a node is the tree's published view action: an agent's
/// choice shows the node and reaches the app as a person's does, and
/// one the tree does not have is refused.
#[test]
fn an_agent_chooses_a_node_through_the_published_view_action() {
    let chosen = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&chosen);
    let tree = TreeView::new(sample_tree())
        .on_view("files.tree", move |node| seen.borrow_mut().push(node));
    let mut h = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        tree,
        (300, 300),
        crate::Theme::dark(),
    );
    let resources = h.resources();
    let control = resources
        .iter()
        .find(|resource| resource.id.0 == "tilo.control:files.tree")
        .expect("the tree publishes its choice");
    let choices: Vec<&str> = control.actions[0].params()[0]
        .choices
        .iter()
        .map(AsRef::as_ref)
        .collect();
    assert_eq!(choices, ["1", "2", "3"]);
    let entry = |key: &str| serde_json::Map::from_iter([("entry".into(), serde_json::json!(key))]);
    h.invoke_published("tilo.control:files.tree", "select", &entry("3"))
        .expect("an agent chooses a node");
    assert_eq!(h.root::<TreeView<()>>().selected(), Some(3));
    assert_eq!(chosen.borrow().as_slice(), [3]);
    assert!(
        h.invoke_published("tilo.control:files.tree", "select", &entry("9"))
            .is_err()
    );
}

fn reorder_tree() -> TreeNode<&'static str> {
    TreeNode::branch(
        1,
        "root",
        "root-data",
        vec![
            TreeNode::branch(
                2,
                "source folder",
                "folder-data",
                vec![
                    TreeNode::leaf(3, "first", "first-data"),
                    TreeNode::branch(
                        4,
                        "nested",
                        "nested-data",
                        vec![TreeNode::leaf(5, "nested leaf", "leaf-data")],
                    ),
                ],
            ),
            TreeNode::branch(6, "destination", "destination-data", Vec::new()),
            TreeNode::leaf(7, "last", "last-data"),
        ],
    )
}

#[test]
fn owned_tree_reorder_moves_subtrees_rejects_invalid_targets_and_commits_drops() {
    {
        let mut tree = reorder_tree();
        assert!(tree.reorder(&ReorderDrop {
            source: 4,
            target: DropTarget::new(6, DropPlacement::Inside),
        }));
        assert_eq!(tree.parent_id(4), Some(6));
        assert_eq!(tree.child_id(2, 0), Some(3));
        assert_eq!(tree.child_id(6, 0), Some(4));
        assert_eq!(tree.find(5).map(|node| node.data), Some("leaf-data"));

        assert!(tree.reorder(&ReorderDrop {
            source: 3,
            target: DropTarget::new(7, DropPlacement::After),
        }));
        assert_eq!(tree.parent_id(3), Some(1));
        assert_eq!(
            tree.children.iter().map(|node| node.id).collect::<Vec<_>>(),
            vec![2, 6, 7, 3]
        );
        assert_eq!(tree.find(3).map(|node| node.data), Some("first-data"));
    }

    {
        let mut tree = reorder_tree();
        let original = tree.children.iter().map(|node| node.id).collect::<Vec<_>>();
        for dropped in [
            ReorderDrop {
                source: 1,
                target: DropTarget::new(2, DropPlacement::Before),
            },
            ReorderDrop {
                source: 2,
                target: DropTarget::new(4, DropPlacement::Inside),
            },
            ReorderDrop {
                source: 3,
                target: DropTarget::new(7, DropPlacement::Inside),
            },
            ReorderDrop {
                source: 99,
                target: DropTarget::new(7, DropPlacement::After),
            },
            ReorderDrop {
                source: 3,
                target: DropTarget::new(4, DropPlacement::Before),
            },
            ReorderDrop {
                source: 5,
                target: DropTarget::new(4, DropPlacement::Inside),
            },
        ] {
            assert!(!tree.reorder(&dropped));
        }
        assert_eq!(
            tree.children.iter().map(|node| node.id).collect::<Vec<_>>(),
            original
        );
        assert_eq!(tree.parent_id(5), Some(4));
    }

    {
        let view = TreeView::new(reorder_tree()).reorderable();
        assert!(view.reorder_validator.is_some());
        assert!(view.reorder_commit.is_some());
    }

    {
        let mut view = TreeView::new(sample_tree())
            .with_expanded([1])
            .reorderable();
        view.set_viewport(88.0, f32_of_f64(ROW_HEIGHT));
        let (taffy, node) = lay_out_tree(&mut view, 240.0);

        dispatch_laid_out(&mut view, &cursor_moved(80.0, 33.0), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &cursor_moved(80.0, 65.0), &taffy, node);
        assert_eq!(
            view.reorder_target(),
            Some(&DropTarget::new(3, DropPlacement::After))
        );
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);

        assert_eq!(
            view.model()
                .children
                .iter()
                .map(|node| node.id)
                .collect::<Vec<_>>(),
            vec![3, 2]
        );
        assert_eq!(view.visible_index_of(2), Some(2));
    }
}

struct ExternalNode {
    label: String,
    branch: bool,
    children: Vec<NodeId>,
}

struct ExternalModel {
    root: NodeId,
    nodes: HashMap<NodeId, ExternalNode>,
    child_queries: Cell<usize>,
}

impl TreeModel for ExternalModel {
    fn root_id(&self) -> NodeId {
        self.root
    }

    fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    fn label(&self, id: NodeId) -> Option<&str> {
        self.nodes.get(&id).map(|node| node.label.as_str())
    }

    fn is_branch(&self, id: NodeId) -> bool {
        self.nodes.get(&id).is_some_and(|node| node.branch)
    }

    fn parent_id(&self, id: NodeId) -> Option<NodeId> {
        self.nodes
            .iter()
            .find_map(|(parent, node)| node.children.contains(&id).then_some(*parent))
    }

    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        self.nodes
            .get(&parent)?
            .children
            .iter()
            .position(|id| *id == child)
    }

    fn child_count(&self, parent: NodeId) -> usize {
        self.child_queries
            .set(self.child_queries.get().saturating_add(1));
        self.nodes
            .get(&parent)
            .map_or(0, |node| node.children.len())
    }

    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        self.nodes.get(&parent)?.children.get(index).copied()
    }
}

fn external_model() -> ExternalModel {
    ExternalModel {
        root: 1,
        nodes: HashMap::from([
            (
                1,
                ExternalNode {
                    label: "root".into(),
                    branch: true,
                    children: vec![2, 3, 4],
                },
            ),
            (
                2,
                ExternalNode {
                    label: "folder".into(),
                    branch: true,
                    children: vec![20],
                },
            ),
            (
                20,
                ExternalNode {
                    label: "hidden child".into(),
                    branch: false,
                    children: Vec::new(),
                },
            ),
            (
                3,
                ExternalNode {
                    label: "anchor".into(),
                    branch: false,
                    children: Vec::new(),
                },
            ),
            (
                4,
                ExternalNode {
                    label: "last".into(),
                    branch: false,
                    children: Vec::new(),
                },
            ),
        ]),
        child_queries: Cell::new(0),
    }
}

struct SharedOrderModel {
    children: Rc<RefCell<Vec<NodeId>>>,
}

impl TreeModel for SharedOrderModel {
    fn root_id(&self) -> NodeId {
        1
    }

    fn contains(&self, id: NodeId) -> bool {
        id == 1 || self.children.borrow().contains(&id)
    }

    fn label(&self, id: NodeId) -> Option<&str> {
        self.contains(id).then_some("row")
    }

    fn is_branch(&self, id: NodeId) -> bool {
        id == 1
    }

    fn parent_id(&self, id: NodeId) -> Option<NodeId> {
        (id != 1 && self.contains(id)).then_some(1)
    }

    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        (parent == 1)
            .then(|| self.children.borrow().iter().position(|id| *id == child))
            .flatten()
    }

    fn child_count(&self, parent: NodeId) -> usize {
        if parent == 1 {
            self.children.borrow().len()
        } else {
            0
        }
    }

    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        (parent == 1)
            .then(|| self.children.borrow().get(index).copied())
            .flatten()
    }
}

#[derive(Clone, Copy)]
enum PagedSlotState {
    Loading,
    Error,
    Ready,
}

const PAGED_ID_BASE: NodeId = 20_000_000_000;

struct PagedModel {
    count: usize,
    states: Rc<RefCell<HashMap<usize, PagedSlotState>>>,
    self_loading: bool,
    requests: Vec<TreeLoadRequest>,
    changes: Vec<TreeModelChange>,
}

impl PagedModel {
    fn id(index: usize) -> Option<NodeId> {
        Some(PAGED_ID_BASE.saturating_add(u64::try_from(index).ok()?))
    }

    fn index(id: NodeId) -> Option<usize> {
        usize::try_from(id.checked_sub(PAGED_ID_BASE)?).ok()
    }

    fn state(&self, index: usize) -> Option<PagedSlotState> {
        (index < self.count).then(|| {
            self.states
                .borrow()
                .get(&index)
                .copied()
                .unwrap_or(PagedSlotState::Loading)
        })
    }
}

impl TreeModel for PagedModel {
    fn root_id(&self) -> NodeId {
        1
    }

    fn contains(&self, id: NodeId) -> bool {
        if id == 1 {
            return true;
        }
        Self::index(id)
            .is_some_and(|index| matches!(self.state(index), Some(PagedSlotState::Ready)))
    }

    fn label(&self, id: NodeId) -> Option<&str> {
        if id == 1 {
            Some("root")
        } else {
            self.contains(id).then_some("loaded row")
        }
    }

    fn is_branch(&self, id: NodeId) -> bool {
        id == 1
    }

    fn parent_id(&self, id: NodeId) -> Option<NodeId> {
        (id != 1 && self.contains(id)).then_some(1)
    }

    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        (parent == 1)
            .then(|| Self::index(child))
            .flatten()
            .filter(|index| matches!(self.state(*index), Some(PagedSlotState::Ready)))
    }

    fn child_count(&self, parent: NodeId) -> usize {
        if parent == 1 { self.count } else { 0 }
    }

    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        (parent == 1 && matches!(self.state(index), Some(PagedSlotState::Ready)))
            .then(|| Self::id(index))
            .flatten()
    }

    fn child_state(&self, parent: NodeId, index: usize) -> TreeChildState<'_> {
        if parent != 1 {
            return Arrival::Failed(Some("Unknown parent"));
        }
        match self.state(index) {
            Some(PagedSlotState::Loading) => Arrival::local(),
            Some(PagedSlotState::Error) => Arrival::Failed(Some("Network unavailable")),
            Some(PagedSlotState::Ready) => {
                Self::id(index).map_or(Arrival::Failed(Some("Invalid row id")), Arrival::Ready)
            }
            None => Arrival::Failed(Some("Row out of bounds")),
        }
    }

    fn request_load(&mut self, request: TreeLoadRequest) -> bool {
        if !self.self_loading || request.slot.parent != 1 || request.slot.index >= self.count {
            return false;
        }
        self.requests.push(request);
        self.states
            .borrow_mut()
            .insert(request.slot.index, PagedSlotState::Ready);
        self.changes.push(TreeModelChange::SlotsChanged {
            slots: vec![request.slot],
        });
        true
    }

    fn poll_changes(&mut self) -> Vec<TreeModelChange> {
        std::mem::take(&mut self.changes)
    }
}

const HUGE_CHILDREN: usize = 100_000_000;
const HUGE_BRANCH_POSITION: usize = 50_000_000;
const HUGE_CHILD_BASE: NodeId = 1_000_000_000;
const HUGE_GRANDCHILD_A: NodeId = 9_000_000_001;
const HUGE_GRANDCHILD_B: NodeId = 9_000_000_002;

struct HugeModel {
    child_id_queries: Cell<usize>,
}

impl HugeModel {
    fn direct_id(index: usize) -> Option<NodeId> {
        Some(HUGE_CHILD_BASE.saturating_add(u64::try_from(index).ok()?))
    }

    fn direct_index(id: NodeId) -> Option<usize> {
        let offset = id.checked_sub(HUGE_CHILD_BASE)?;
        usize::try_from(offset)
            .ok()
            .filter(|index| *index < HUGE_CHILDREN)
    }

    fn branch_id() -> NodeId {
        Self::direct_id(HUGE_BRANCH_POSITION).unwrap()
    }
}

impl TreeModel for HugeModel {
    fn root_id(&self) -> NodeId {
        1
    }

    fn contains(&self, id: NodeId) -> bool {
        id == 1
            || Self::direct_index(id).is_some()
            || matches!(id, HUGE_GRANDCHILD_A | HUGE_GRANDCHILD_B)
    }

    fn label(&self, id: NodeId) -> Option<&str> {
        self.contains(id).then_some("row")
    }

    fn is_branch(&self, id: NodeId) -> bool {
        id == 1 || id == Self::branch_id()
    }

    fn parent_id(&self, id: NodeId) -> Option<NodeId> {
        if Self::direct_index(id).is_some() {
            Some(1)
        } else if matches!(id, HUGE_GRANDCHILD_A | HUGE_GRANDCHILD_B) {
            Some(Self::branch_id())
        } else {
            None
        }
    }

    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        if parent == 1 {
            Self::direct_index(child)
        } else if parent == Self::branch_id() {
            match child {
                HUGE_GRANDCHILD_A => Some(0),
                HUGE_GRANDCHILD_B => Some(1),
                _ => None,
            }
        } else {
            None
        }
    }

    fn child_count(&self, parent: NodeId) -> usize {
        if parent == 1 {
            HUGE_CHILDREN
        } else if parent == Self::branch_id() {
            2
        } else {
            0
        }
    }

    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        self.child_id_queries
            .set(self.child_id_queries.get().saturating_add(1));
        if parent == 1 && index < HUGE_CHILDREN {
            Self::direct_id(index)
        } else if parent == Self::branch_id() {
            match index {
                0 => Some(HUGE_GRANDCHILD_A),
                1 => Some(HUGE_GRANDCHILD_B),
                _ => None,
            }
        } else {
            None
        }
    }
}

struct DelegateProbe {
    label: String,
    height: f32,
    consume_press: bool,
    presses: Rc<Cell<usize>>,
    rect: Rect,
    cursor: Option<Point>,
}

impl DelegateProbe {
    fn new(
        label: impl Into<String>,
        height: f32,
        consume_press: bool,
        presses: Rc<Cell<usize>>,
    ) -> Self {
        Self {
            label: label.into(),
            height,
            consume_press,
            presses,
            rect: Rect::ZERO,
            cursor: None,
        }
    }
}

impl Widget for DelegateProbe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(position.x, position.y));
        }
        if self.consume_press
            && self.cursor.is_some_and(|point| self.rect.contains(point))
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
        "DelegateProbe"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name(&self.label))
    }
}

pub(super) fn lay_out_tree<T: 'static, M: TreeModel + 'static>(
    view: &mut TreeView<T, M>,
    width: f32,
) -> (TaffyTree<()>, taffy::NodeId) {
    let doc = AutoCommit::new();
    view.sync(&doc, &automerge::ROOT);
    let mut taffy = TaffyTree::new();
    let mut node_map = HashMap::new();
    let node = view.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
    taffy
        .compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::Definite(f32_of_f64(view.body_height())),
            },
        )
        .unwrap();
    view.apply_layout(&taffy, node, Point::ZERO);
    (taffy, node)
}

#[test]
fn unchanged_layout_preserves_tree_paint_while_theme_and_extent_invalidate() {
    fn layout(view: &mut TreeView<()>, theme: &Theme, width: f32) {
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = view.build_layout(&mut taffy, theme, &mut nodes);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::Definite(240.0),
                },
            )
            .unwrap();
        view.apply_layout(&taffy, node, Point::ZERO);
    }
    fn paint(view: &mut TreeView<()>, theme: &Theme, text: &mut TextRenderer) -> Scene {
        let mut scene = Scene::new();
        view.draw(
            &mut scene,
            Affine::IDENTITY,
            text,
            theme,
            &PresenceCtx::empty(),
        );
        scene
    }
    let dark = Theme::dark().with_reduce_motion(true);
    let light = Theme::light().with_reduce_motion(true);
    let mut view = TreeView::new(sample_tree()).hide_root();
    let mut text = TextRenderer::new();
    view.sync(&AutoCommit::new(), &automerge::ROOT);
    layout(&mut view, &dark, 280.0);
    let initial = paint(&mut view, &dark, &mut text);
    view.mark_drawn();
    layout(&mut view, &dark, 280.0);
    assert!(
        !view.needs_redraw(),
        "an identical layout must not dirty the sidebar"
    );
    let mut damage = Vec::new();
    view.collect_damage(&mut damage);
    assert!(damage.is_empty());
    assert!(initial.encoding_eq(&paint(&mut view, &dark, &mut text)));
    view.mark_drawn();
    layout(&mut view, &light, 280.0);
    assert!(
        view.needs_redraw(),
        "theme changes must invalidate retained ink"
    );
    let themed = paint(&mut view, &light, &mut text);
    assert!(!initial.encoding_eq(&themed));
    view.mark_drawn();
    layout(&mut view, &light, 360.0);
    assert!(
        view.needs_redraw(),
        "changed bounds must invalidate retained ink"
    );
    assert!(!themed.encoding_eq(&paint(&mut view, &light, &mut text)));
}

#[test]
fn a_settled_tree_damages_the_row_that_changed_and_matches_gpu_pixels() {
    fn layout(view: &mut TreeView<()>, theme: &Theme) {
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = view.build_layout(&mut taffy, theme, &mut nodes);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(280.0),
                    height: taffy::AvailableSpace::Definite(240.0),
                },
            )
            .unwrap();
        view.apply_layout(&taffy, node, Point::ZERO);
    }
    fn paint(view: &mut TreeView<()>, theme: &Theme, text: &mut TextRenderer) -> Scene {
        let mut scene = Scene::new();
        view.draw(
            &mut scene,
            Affine::IDENTITY,
            text,
            theme,
            &PresenceCtx::empty(),
        );
        scene
    }
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let theme = theme.with_reduce_motion(true);
        // Six pages, so one row is a sixth of the tree.
        let pages = (2..8)
            .map(|id| TreeNode::leaf(id, format!("page {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), pages)).hide_root();
        let mut text = TextRenderer::new();
        view.sync(&AutoCommit::new(), &automerge::ROOT);
        layout(&mut view, &theme);
        let before = paint(&mut view, &theme, &mut text);
        view.mark_drawn();
        view.set_current(Some(3));
        layout(&mut view, &theme);
        let after = paint(&mut view, &theme, &mut text);
        let mut damage = Vec::new();
        view.collect_damage(&mut damage);
        view.mark_drawn();
        assert!(!damage.is_empty(), "the row changed");
        let area: f64 = damage.iter().map(Rect::area).sum();
        assert!(
            area < view.get_rect().area() / 4.0,
            "one row changed, not the tree: {damage:?} of {:?}",
            view.get_rect()
        );
        let background = theme.tokens.colors.surfaces.content;
        let mut retained = crate::offscreen::OffscreenRenderer::new(280, 240);
        let mut full = crate::offscreen::OffscreenRenderer::new(280, 240);
        retained.render(&before, background);
        let regions: Vec<_> = damage.iter().map(|rect| rect.inflate(1.0, 1.0)).collect();
        retained.render_damage_regions_gpu_wait(&after, background, &regions);
        assert_eq!(
            retained.readback_current(),
            full.render(&after, background),
            "the sparse repaint equals a full render"
        );
    }
}

pub(super) fn cursor_moved(x: f64, y: f64) -> WindowEvent {
    crate::synthetic_input::cursor_moved(x, y)
}

pub(super) fn mouse_press() -> WindowEvent {
    crate::synthetic_input::mouse_press()
}

pub(super) fn mouse_release() -> WindowEvent {
    crate::synthetic_input::mouse_release()
}

pub(super) fn key_press(logical_key: Key) -> WindowEvent {
    crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
        logical_key,
        None,
        ElementState::Pressed,
    ))
}

pub(super) fn dispatch_laid_out<T: 'static, M: TreeModel + 'static>(
    view: &mut TreeView<T, M>,
    event: &WindowEvent,
    taffy: &TaffyTree<()>,
    node: taffy::NodeId,
) -> bool {
    let handled = Dispatch::new()
        .layout(taffy, node)
        .send(view, event)
        .handled;
    // Opening a node is a published action the host answers after the
    // event that reported it.
    crate::testing::answer_controls(view);
    handled
}

#[test]
fn expansion_toggles_from_disclosure_and_keys_and_persists_quietly() {
    // Case `disclosure_hit_zone_is_the_painted_column_at_two_x`.
    // At 2× text the disclosure column is painted at the `sm` line width
    // (30 px), not the 1× 16 px. A press between the two must toggle the
    // branch: the hit zone is the painted column, not the pinned constant.
    {
        let root = TreeNode::branch(1, "crates", (), vec![TreeNode::leaf(2, "main.rs", ())]);
        let mut view = TreeView::new(root);
        let doc = AutoCommit::new();
        view.sync(&doc, &automerge::ROOT);
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let theme = Theme::dark().with_text_scale(2.0);
        let node = view.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(320.0),
                    height: taffy::AvailableSpace::Definite(f32_of_f64(view.body_height())),
                },
            )
            .unwrap();
        view.apply_layout(&taffy, node, Point::ZERO);
        assert!(
            view.disclosure_w > DISCLOSURE_WIDTH,
            "2x resolves a wider disclosure column: {}",
            view.disclosure_w
        );
        assert!(view.viewport.estimated_height() > ROW_HEIGHT);
        assert!(!view.expanded.contains(&1));

        let x = f64::midpoint(DISCLOSURE_WIDTH, view.disclosure_w);
        let y = view.viewport.estimated_height() / 2.0;
        dispatch_laid_out(&mut view, &cursor_moved(x, y), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        assert!(
            view.expanded.contains(&1),
            "a press at x={x} inside the painted disclosure column toggles the branch"
        );
    }

    {
        let mut view = TreeView::new(sample_tree());
        let rows = view.visible_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 1);

        view.toggle_expand(1);
        let rows = view.visible_rows();
        let ids: Vec<NodeId> = rows.iter().map(|(id, _, _)| *id).collect();
        assert!(
            ids.contains(&2),
            "child1 must be visible after expanding root"
        );
        assert!(
            ids.contains(&3),
            "child2 must be visible after expanding root"
        );
        assert_eq!(rows.len(), 3);
    }

    {
        let mut view = TreeView::new(sample_tree());
        view.set_focus(true);
        view.focused = Some(TreeRowKey::Node(1));
        assert!(!view.is_expanded(1));

        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowRight)));

        assert!(
            view.is_expanded(1),
            "ArrowRight must expand the focused branch"
        );
    }

    {
        let mut view = TreeView::new(TreeNode::branch(1, "loading", (), Vec::new()));
        view.toggle_expand(1);
        assert!(view.is_expanded(1));
        let node = Widget::accessibility(&view).unwrap();
        assert_eq!(node.children[0].state.expanded, Some(true));
    }

    {
        let changes = Rc::new(RefCell::new(Vec::new()));
        let sink = changes.clone();
        let mut view = TreeView::new(sample_tree())
            .with_expanded([1])
            .with_accessible_name("Project files")
            .on_expand(move |id, expanded| sink.borrow_mut().push((id, expanded)));

        assert!(view.is_expanded(1));
        assert!(view.set_expanded(1, false));
        assert!(!view.set_expanded(1, false), "idempotent controlled update");
        assert!(!view.set_expanded(2, true), "a leaf cannot expand");
        assert_eq!(*changes.borrow(), vec![(1, false)]);
        assert_eq!(
            Widget::accessibility(&view).unwrap().name.as_deref(),
            Some("Project files")
        );
    }

    {
        let echoed = Rc::new(Cell::new(0));
        let count = Rc::clone(&echoed);
        let mut view = TreeView::new(navigator_tree())
            .hide_root()
            .on_expand(move |_, _| count.set(count.get() + 1));
        view.restore_expanded([2, 5, 99]);
        assert!(view.is_expanded(2));
        assert!(!view.is_expanded(5), "leaves are not expandable");
        assert_eq!(echoed.get(), 0);
        view.set_expanded(2, false);
        assert_eq!(echoed.get(), 1, "a user change still reports");
    }

    {
        let (view, activated, _) = logged(
            TreeView::new(navigator_tree())
                .hide_root()
                .click_activation(TreeClickActivation::SingleClick),
        );
        let mut view = view;
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        // The caret column starts at the list's s2 inset, never on the
        // pane edge.
        let caret = view.disclosure_rect(2).expect("caret");
        assert!(caret.x0 >= view.rect.x0 + f64::from(Theme::dark().tokens.spacing.s2));
        let at = Point::new(caret.x0 + 2.0, row_center(&view, 2).y);
        dispatch_laid_out(&mut view, &cursor_moved(at.x, at.y), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        assert!(view.is_expanded(2));
        assert!(activated.borrow().is_empty());
    }
}

#[test]
fn selection_reports_callbacks_and_multiselect_by_stable_id() {
    {
        let captured: Rc<RefCell<Option<NodeId>>> = Rc::new(RefCell::new(None));
        let captured_clone = captured.clone();
        let mut view = TreeView::new(sample_tree()).on_select(move |id| {
            *captured_clone.borrow_mut() = Some(id);
        });
        view.toggle_expand(1);

        // Taffy's `auto` width on a leaf inside a block container collapses
        // to zero under `MAX_CONTENT`, so wrap it in a fixed-width parent
        // and patch `view.rect` manually rather than trust
        // `apply_layout`'s width.
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let node = view.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let root_taffy = taffy.parent_node(
            Style {
                size: Size::from_lengths(200.0, 80.0),
                ..Default::default()
            },
            &[node],
        );
        taffy
            .compute_layout(root_taffy, taffy::Size::MAX_CONTENT)
            .unwrap();
        view.apply_layout(&taffy, node, Point::ZERO);
        view.rect = Rect::new(
            0.0,
            0.0,
            200.0,
            f64_of_usize(view.visible_rows().len()) * ROW_HEIGHT,
        );
        let target_id = view.visible_rows()[1].0;
        let target_rect = Rect::new(0.0, ROW_HEIGHT, 200.0, ROW_HEIGHT * 2.0);
        assert_eq!(target_id, 2);
        let click_x = target_rect.x0 + 60.0; // past depth-1 indent + glyph
        let click_y = target_rect.y0 + target_rect.height() / 2.0;

        dispatch(&mut view, &cursor_moved(click_x, click_y));
        dispatch(&mut view, &mouse_press());
        assert_eq!(
            *captured.borrow(),
            None,
            "selection waits until the gesture is known to be a click"
        );
        dispatch(&mut view, &mouse_release());

        assert_eq!(
            *captured.borrow(),
            Some(2),
            "on_select must fire with child1 id"
        );
        assert_eq!(view.selected(), Some(2));
    }

    {
        let mut view = TreeView::from_model(external_model())
            .with_expanded([1, 2])
            .multi_select(true);
        view.apply_selection_gesture(20, false, false);
        view.apply_selection_gesture(3, true, false);
        assert!(view.selection().is_selected(&20));
        assert!(view.selection().is_selected(&3));

        view.set_expanded(2, false);
        assert!(
            view.selection().is_selected(&20),
            "hidden nodes stay selected"
        );
        view.update_source(
            &[TreeModelChange::RowsMoved {
                parent: 1,
                from: 1,
                to: 0,
                count: 1,
            }],
            |model| {
                let children = &mut model.nodes.get_mut(&1).unwrap().children;
                let moved = children.remove(1);
                children.insert(0, moved);
            },
        );
        assert!(view.selection().is_selected(&3));
        assert!(view.selection().is_selected(&20));
    }

    {
        let mut view = TreeView::from_model(external_model())
            .with_expanded([1, 2])
            .multi_select(true);
        view.apply_selection_gesture(2, false, false);
        view.apply_selection_gesture(4, false, true);
        assert!(view.selection().is_selected(&2));
        assert!(view.selection().is_selected(&20));
        assert!(view.selection().is_selected(&3));
        assert!(view.selection().is_selected(&4));

        assert!(view.on_key_with_modifiers(&Key::Character("a".into()), true, false));
        assert!(view.selection().is_select_all());
        assert_eq!(view.selection().stored_keys().count(), 0);
        assert!(view.selection().is_selected(&20));

        view.apply_selection_gesture(3, true, false);
        assert!(!view.selection().is_selected(&3));
        assert_eq!(
            view.selection().stored_keys().copied().collect::<Vec<_>>(),
            vec![3]
        );
    }
}

#[test]
fn typing_in_a_focused_list_jumps_to_the_row_it_names() {
    let names = ["Apple", "apricot", "Banana", "berry", "Cherry"];
    let children = names
        .iter()
        .enumerate()
        .map(|(index, name)| TreeNode::leaf(index as u64 + 2, (*name).to_owned(), ()))
        .collect();
    let mut view = TreeView::new(TreeNode::branch(1, "root", (), children))
        .hide_root()
        .selection_follows_focus(true);
    view.set_focus(true);
    view.focused = Some(TreeRowKey::Node(2));

    dispatch(&mut view, &key_press(Key::Character("c".into())));
    assert_eq!(view.focused(), Some(6), "c moves to Cherry");
    assert_eq!(view.selected(), Some(6), "a navigation list selects it too");

    dispatch(&mut view, &key_press(Key::Character("b".into())));
    // Within the pause the letters spell "cb", which names nothing; the
    // list stays where it is rather than jumping somewhere unexpected.
    assert_eq!(view.focused(), Some(6));
}

#[test]
fn keyboard_navigation_moves_focus_and_separates_selection_from_activation() {
    {
        let mut view = TreeView::new(sample_tree());
        view.toggle_expand(1);
        view.set_focus(true);
        view.focused = Some(TreeRowKey::Node(1));

        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowDown)));

        let rows = view.visible_rows();
        let second_id = rows[1].0;
        assert_eq!(
            view.focused(),
            Some(second_id),
            "ArrowDown must move focus from row 0 to row 1",
        );
    }

    {
        let children = (2_u64..=20)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
        view.toggle_expand(1);
        view.set_viewport(44.0, f32_of_f64(ROW_HEIGHT));
        view.set_focus(true);
        view.focused = Some(TreeRowKey::Node(1));

        dispatch(&mut view, &key_press(Key::Named(NamedKey::End)));
        assert_eq!(view.focused(), Some(20));
        assert!(view.scroll_y() > 0.0, "End reveals the final row");

        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowLeft)));
        assert_eq!(view.focused(), Some(1), "Left on a leaf focuses its parent");
        assert_eq!(view.scroll_y(), 0.0, "revealing the root returns to top");
    }

    {
        let (view, activated, selected) = logged(
            TreeView::new(navigator_tree())
                .hide_root()
                .selection_follows_focus(true),
        );
        let mut view = view;
        view.set_focus(true);
        view.focused = Some(TreeRowKey::Node(2));

        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowDown)));
        assert_eq!(view.focused(), Some(5));
        assert_eq!(view.selected(), Some(5), "selection follows keyboard focus");
        assert!(activated.borrow().is_empty(), "browsing opens nothing");

        dispatch(&mut view, &key_press(Key::Character(" ".into())));
        assert!(
            activated.borrow().is_empty(),
            "Space selects without activating"
        );

        dispatch(&mut view, &key_press(Key::Named(NamedKey::Enter)));
        assert_eq!(*activated.borrow(), [5], "Enter activates the focused row");
        assert!(selected.borrow().contains(&5));
    }

    {
        let (mut view, activated, _) = logged(TreeView::new(navigator_tree()).hide_root());
        view.set_focus(true);
        view.focused = Some(TreeRowKey::Node(2));
        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowDown)));
        assert_eq!(view.focused(), Some(5));
        assert_eq!(view.selected(), None);
        assert!(activated.borrow().is_empty());
    }

    {
        let (mut view, activated, _) = logged(TreeView::new(navigator_tree()).hide_root());
        view.focused = Some(TreeRowKey::Node(2));
        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowDown)));
        dispatch(&mut view, &key_press(Key::Named(NamedKey::Enter)));
        assert_eq!(view.focused(), Some(2), "keys reach only a focused tree");
        assert!(activated.borrow().is_empty());
    }
}

#[test]
fn accessibility_reports_tree_role_value_focus_and_row_state() {
    {
        use crate::a11y::Role;
        let view = TreeView::new(sample_tree());
        let node = Widget::accessibility(&view).expect("tree is not presentational");
        assert_eq!(node.role, Role::Tree);
        assert_eq!(node.name, None, "the tree has no intrinsic label");
        assert_eq!(node.value, None, "no value until a row is selected");
        assert!(node.state.focusable);
        assert!(!node.state.disabled);
        assert!(!node.state.focused);
    }

    {
        let view = TreeView::new(sample_tree()).with_id("workspace-tree");
        assert_eq!(Widget::test_id(&view), Some("workspace-tree"));
    }

    {
        let mut view = TreeView::new(sample_tree());
        view.toggle_expand(1);
        view.select(2); // "child1"
        let node = Widget::accessibility(&view).unwrap();
        assert_eq!(
            node.value.as_deref(),
            Some("child1"),
            "value is the selected node's label",
        );
    }

    {
        let mut view = TreeView::new(sample_tree());
        view.set_focus(true);
        let node = Widget::accessibility(&view).unwrap();
        assert!(
            node.state.focused,
            "widget focus surfaces in the access node"
        );
    }

    {
        let mut view = TreeView::new(sample_tree());
        view.toggle_expand(1);
        view.select(2);
        view.focused = Some(TreeRowKey::Node(2));
        view.set_focus(true);
        let node = Widget::accessibility(&view).unwrap();

        assert_eq!(node.children.len(), 3);
        assert_eq!(node.children[0].state.expanded, Some(true));
        assert_eq!(node.children[0].level, Some(1));
        assert_eq!(node.children[1].level, Some(2));
        assert!(node.children[1].state.selected);
        assert!(node.children[1].state.focused);
        assert_eq!(node.children[1].name.as_deref(), Some("child1"));
    }

    {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&seen);
        let mut view = TreeView::new(navigator_tree())
            .hide_root()
            .with_row_decoration(move |_, row, theme| {
                log.borrow_mut().push(row);
                TreeRowDecoration::new()
                    .icon(
                        if row.is_branch {
                            Icon::Folder
                        } else {
                            Icon::File
                        },
                        theme.tokens.colors.text_dim,
                    )
                    .trailing("3")
                    .description(if row.is_branch { "Folder" } else { "File" })
            });
        view.set_current(Some(5));
        view.select(2);
        let _ = lay_out_tree(&mut view, 240.0);
        draw_view(&mut view);
        let rows = seen.borrow().clone();
        let readme = rows
            .iter()
            .find(|row| row.id == 5)
            .expect("README decorated");
        assert!(readme.current && !readme.selected && !readme.is_branch);
        let src = rows.iter().find(|row| row.id == 2).expect("src decorated");
        assert!(src.selected && src.is_branch && !src.expanded && !src.current);

        let semantics = Widget::accessibility(&view).unwrap();
        let described: Vec<_> = semantics
            .children
            .iter()
            .map(|item| item.description.clone())
            .collect();
        assert_eq!(
            described,
            [Some("Folder".to_owned()), Some("File".to_owned())]
        );
        assert_eq!(view.current(), Some(5));
        view.set_current(Some(99));
        assert_eq!(view.current(), None, "an unknown id is not current");
    }
}

#[test]
fn bounded_viewport_windows_paint_and_semantics_around_a_stable_anchor() {
    {
        let children = (2_u64..102)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let view = TreeView::new(TreeNode::branch(1, "root", (), children))
            .with_expanded([1])
            .with_viewport(44.0, 30.0);
        assert_eq!(view.viewport.viewport(), 44.0);
        assert_eq!(view.viewport.estimated_height(), 30.0);
        assert!(view.visible_row_range().len() < view.visible_len());
    }

    {
        let children = (2_u64..=100_001)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
        view.toggle_expand(1);
        view.set_viewport(110.0, f32_of_f64(ROW_HEIGHT));
        let middle = view.max_scroll_y() / 2.0;
        view.set_scroll_y(middle);

        let visible = view.visible_row_range();
        assert!(visible.len() <= 12, "bounded geometry window: {visible:?}");
        let node = Widget::accessibility(&view).unwrap();
        assert!(node.children.len() <= 12, "bounded semantic window");
        let first = node.children.first().expect("visible tree item");
        assert_eq!(first.role, crate::a11y::Role::TreeItem);
        assert_eq!(first.set_size, Some(100_001));
        assert!(first.pos_in_set.expect("absolute position") > 49_000);
    }

    {
        let mut view = TreeView::new(sample_tree());
        view.toggle_expand(1);
        view.set_viewport(f32_of_f64(ROW_HEIGHT * 2.0), f32_of_f64(ROW_HEIGHT));
        view.set_scroll_y(10.0);
        let _ = lay_out_tree(&mut view, 320.0);

        let semantics = Widget::accessibility(&view).expect("tree semantics");
        let first = semantics.children.first().expect("first visible row");
        assert_eq!(
            first.bounds.expect("tree row bounds").to_rect(),
            Rect::new(0.0, -10.0, 320.0, ROW_HEIGHT - 10.0),
        );
    }

    {
        let root = TreeNode::branch(
            1,
            "root",
            (),
            vec![
                TreeNode::branch(
                    2,
                    "folder",
                    (),
                    vec![
                        TreeNode::leaf(3, "a", ()),
                        TreeNode::leaf(4, "b", ()),
                        TreeNode::leaf(5, "c", ()),
                    ],
                ),
                TreeNode::leaf(10, "anchor", ()),
                TreeNode::leaf(11, "after", ()),
            ],
        );
        let mut view = TreeView::new(root);
        view.toggle_expand(1);
        view.set_viewport(f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
        view.set_scroll_y(f32_of_f64(ROW_HEIGHT * 2.0 + 5.0));
        let before = view.viewport.item_view_top(2);

        view.toggle_expand(2);

        let anchor = view.visible_index_of(10).expect("anchor remains visible");
        assert_eq!(view.viewport.item_view_top(anchor), before);
    }
}

#[test]
fn external_model_changes_reconcile_state_and_keep_anchors() {
    {
        let mut view =
            TreeView::new(TreeNode::branch(1, "root", (), Vec::new())).with_expanded([1]);
        view.update_model(|root| {
            root.children.push(TreeNode::leaf(2, "loaded", ()));
        });
        assert_eq!(
            view.visible_rows()
                .iter()
                .map(|(id, _, _)| *id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        view.select(2);
        view.focused = Some(TreeRowKey::Node(2));
        view.update_model(|root| root.children.clear());
        assert_eq!(view.selected(), None, "deleted ids leave selection sane");
        assert_eq!(view.focused(), None, "deleted ids leave focus sane");
        assert!(view.is_expanded(1), "the surviving lazy branch stays open");
    }

    {
        let mut view = TreeView::from_model(external_model());
        assert_eq!(
            view.model().child_queries.get(),
            0,
            "a collapsed root does not query its children"
        );
        view = view.with_expanded([1]);
        assert_eq!(view.model().child_queries.get(), 1);
        assert_eq!(
            view.visible_rows()
                .iter()
                .map(|(id, _, _)| *id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4],
            "the collapsed folder's child is not queried into visible order"
        );

        view.set_viewport(f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
        view.set_scroll_y(f32_of_f64(ROW_HEIGHT * 2.0 + 5.0));
        let before = view
            .viewport
            .item_view_top(view.visible_index_of(3).unwrap());
        view.update_source(
            &[TreeModelChange::RowsInserted {
                parent: 1,
                index: 0,
                count: 1,
            }],
            |model| {
                model.nodes.insert(
                    5,
                    ExternalNode {
                        label: "inserted".into(),
                        branch: false,
                        children: Vec::new(),
                    },
                );
                model.nodes.get_mut(&1).unwrap().children.insert(0, 5);
            },
        );
        let anchor = view.visible_index_of(3).expect("stable id remains");
        assert_eq!(view.viewport.item_view_top(anchor), before);
    }

    {
        let children = Rc::new(RefCell::new(vec![2, 3, 4]));
        let mut view = TreeView::from_model(SharedOrderModel {
            children: Rc::clone(&children),
        })
        .with_expanded([1]);
        view.set_viewport(f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
        view.set_scroll_offset(ROW_HEIGHT * 2.0 + 5.0);
        let before = view
            .viewport
            .item_view_top(view.visible_index_of(3).unwrap());

        children.borrow_mut().insert(0, 5);
        view.notify_model_changes(&[TreeModelChange::RowsInserted {
            parent: 1,
            index: 0,
            count: 1,
        }]);

        let anchor = view.visible_index_of(3).expect("stable id remains");
        assert_eq!(view.viewport.item_view_top(anchor), before);
    }

    {
        let mut view = TreeView::from_model(external_model())
            .with_expanded([1])
            .with_selected(Some(3));
        view.focused = Some(TreeRowKey::Node(3));
        view.set_viewport(66.0, f32_of_f64(ROW_HEIGHT));
        let offset = view.scroll_y();

        view.update_source(&[TreeModelChange::RowsChanged { ids: vec![3] }], |model| {
            model.nodes.get_mut(&3).unwrap().label = "renamed".into();
        });
        assert_eq!(view.scroll_y(), offset, "data changes preserve geometry");
        assert!(
            Widget::accessibility(&view)
                .unwrap()
                .children
                .iter()
                .any(|row| row.name.as_deref() == Some("renamed")),
            "semantics query the fresh model label"
        );

        view.update_source(
            &[TreeModelChange::RowsRemoved {
                parent: 1,
                index: 1,
                count: 1,
            }],
            |model| {
                model.nodes.get_mut(&1).unwrap().children.remove(1);
                model.nodes.remove(&3);
            },
        );
        assert_eq!(view.selected(), None);
        assert_eq!(view.focused(), None);
        assert_eq!(view.visible_index_of(3), None);
    }

    {
        let mut view = TreeView::from_model(external_model()).with_expanded([1]);
        let old_index = view.visible_index_of(3).unwrap();
        assert!(view.set_row_height(old_index, 73.0));

        view.update_source(
            &[TreeModelChange::RowsMoved {
                parent: 1,
                from: 1,
                to: 0,
                count: 1,
            }],
            |model| {
                let children = &mut model.nodes.get_mut(&1).unwrap().children;
                let moved = children.remove(1);
                children.insert(0, moved);
            },
        );

        let new_index = view.visible_index_of(3).unwrap();
        assert_eq!(new_index, 1);
        assert_eq!(view.viewport.item_height(new_index), Some(73.0));
    }
}

#[test]
fn scroll_input_pages_wheels_and_drags_over_sparse_rows() {
    {
        let children = (2_u64..=101)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
        view.toggle_expand(1);
        view.set_viewport(100.0, f32_of_f64(ROW_HEIGHT));
        assert!(view.set_row_height(0, 300.0));
        assert!(view.focus_at(0));

        assert!(view.on_key_with_modifiers(&Key::Named(NamedKey::PageDown), false, false,));
        assert_eq!(view.focused(), Some(1));
        assert_eq!(view.scroll_y(), 90.0);
        assert!(view.on_key_with_modifiers(&Key::Named(NamedKey::PageUp), false, false,));
        assert_eq!(view.scroll_y(), 0.0);

        assert!(view.on_wheel(&MouseScrollDelta::LineDelta(0.0, -1.0), TouchPhase::Moved));
        let immediate = view.scroll_y();
        assert_eq!(immediate, 60.0);
        assert!(view.needs_animation_frame());
        let frame = crate::time::now() + Duration::from_millis(16);
        // The glide moves only the view: it is no change of the model.
        assert!(!crate::time::testing::with_monotonic_time(frame, || {
            Widget::update(&mut view, &AutoCommit::new(), &automerge::ROOT)
        }));
        assert!(view.scroll_y() > immediate, "momentum advances after input");
    }

    {
        let children = (2_u64..=101)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
        view.toggle_expand(1);
        view.set_viewport(100.0, f32_of_f64(ROW_HEIGHT));
        let (taffy, node) = lay_out_tree(&mut view, 320.0);

        dispatch_laid_out(&mut view, &cursor_moved(100.0, 80.0), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        assert!(view.wants_pointer_capture());
        dispatch_laid_out(&mut view, &cursor_moved(100.0, 20.0), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        assert_eq!(view.scroll_y(), 60.0);
        assert_eq!(view.selected(), None, "panning does not select a row");

        view.set_scroll_y(0.0);
        dispatch_laid_out(&mut view, &cursor_moved(100.0, 10.0), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        assert_eq!(
            view.selected(),
            Some(1),
            "release below threshold is a click"
        );
    }

    {
        let children = (2_u64..102)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
        view.set_expanded(1, true);
        view.set_viewport(100.0, 22.0);
        view.reduce_motion = true;

        assert!(view.on_wheel(&MouseScrollDelta::LineDelta(0.0, -1.0), TouchPhase::Moved));
        assert_eq!(view.scroll_y(), 60.0_f32);
        assert!(!view.scroll.momentum.is_active());

        let now = crate::time::now();
        let started = now.checked_sub(Duration::from_millis(20)).unwrap_or(now);
        let moved = now.checked_sub(Duration::from_millis(5)).unwrap_or(now);
        let offset = f64::from(view.scroll_y());
        view.drag_scroll.begin(50.0, offset, started);
        assert!(view.drag_scroll.update(10.0, moved).is_some());
        dispatch(&mut view, &mouse_release());
        assert!(!view.scroll.momentum.is_active());
        assert!(!view.needs_animation_frame());
    }
}

#[test]
fn paged_tree_demands_visible_slots_and_recovers_from_errors() {
    {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&requests);
        let mut view = TreeView::from_model(PagedModel {
            count: 100_000_000,
            states: Rc::new(RefCell::new(HashMap::new())),
            self_loading: false,
            requests: Vec::new(),
            changes: Vec::new(),
        })
        .with_expanded([1])
        .on_load_request(move |request| captured.borrow_mut().push(request));
        view.set_viewport(66.0, f32_of_f64(ROW_HEIGHT));
        view.set_scroll_offset(f64_of_usize(50_000_000) * ROW_HEIGHT);

        let demanded = view.request_visible_rows();
        assert!(demanded > 0);
        assert!(demanded <= 8, "overscan must keep page demand bounded");
        assert_eq!(view.request_visible_rows(), 0, "requests are deduplicated");
        let requests = requests.borrow();
        assert_eq!(requests.len(), demanded);
        assert!(
            requests
                .iter()
                .all(|request| request.reason == TreeLoadReason::Visible)
        );
        assert!(requests.iter().all(|request| {
            request.slot.index.abs_diff(50_000_000) <= 5 && request.slot.parent == 1
        }));
        assert_eq!(view.visible_len(), 100_000_001);
    }

    {
        let mut view = TreeView::from_model(PagedModel {
            count: 4,
            states: Rc::new(RefCell::new(HashMap::new())),
            self_loading: true,
            requests: Vec::new(),
            changes: Vec::new(),
        })
        .with_expanded([1]);
        view.set_viewport(66.0, f32_of_f64(ROW_HEIGHT));
        assert!(matches!(
            view.index.row_at(&view.model, 1).map(|(key, _)| key),
            Some(TreeRowKey::Slot(_))
        ));
        assert!(view.focus_at(1));

        let demanded = view.request_visible_rows();
        assert!(demanded > 0);
        assert_eq!(view.model().requests.len(), demanded);

        let document = AutoCommit::new();
        assert!(Widget::update(&mut view, &document, &automerge::ROOT));

        let first = PagedModel::id(0).expect("stable paged id");
        assert_eq!(
            view.index.row_at(&view.model, 1).map(|(key, _)| key),
            Some(TreeRowKey::Node(first))
        );
        assert_eq!(view.focused(), Some(first));
        assert_eq!(view.visible_row(1), Some((first, "loaded row", 1)));
        assert_eq!(view.request_visible_rows(), 0);
    }

    {
        let states = Rc::new(RefCell::new(HashMap::from([(0, PagedSlotState::Error)])));
        let requests = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&requests);
        let mut view = TreeView::from_model(PagedModel {
            count: 1,
            states: Rc::clone(&states),
            self_loading: false,
            requests: Vec::new(),
            changes: Vec::new(),
        })
        .with_expanded([1])
        .on_load_request(move |request| captured.borrow_mut().push(request));
        view.set_viewport(f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
        view.set_scroll_offset(ROW_HEIGHT + 5.0);
        assert!(view.focus_at(1));
        view.set_focus(true);
        let before = view.viewport.item_view_top(1);

        let semantics = Widget::accessibility(&view).unwrap();
        let error = semantics
            .children
            .iter()
            .find(|row| row.state.invalid)
            .expect("error placeholder is exposed");
        assert!(error.state.focusable);
        assert!(error.state.focused);
        assert!(error.name.as_deref().unwrap().contains("Activate to retry"));

        assert!(view.on_key_with_modifiers(&Key::Named(NamedKey::Enter), false, false));
        assert_eq!(
            requests.borrow().as_slice(),
            &[TreeLoadRequest {
                slot: TreeSlot {
                    parent: 1,
                    index: 0,
                },
                reason: TreeLoadReason::Retry,
            }]
        );

        states.borrow_mut().insert(0, PagedSlotState::Ready);
        view.notify_model_changes(&[TreeModelChange::SlotsChanged {
            slots: vec![TreeSlot {
                parent: 1,
                index: 0,
            }],
        }]);

        let id = PagedModel::id(0).unwrap();
        assert_eq!(view.focused(), Some(id));
        assert_eq!(view.visible_row(1), Some((id, "loaded row", 1)));
        assert_eq!(view.viewport.item_view_top(1), before);
        let loaded = Widget::accessibility(&view)
            .unwrap()
            .children
            .into_iter()
            .find(|row| row.name.as_deref() == Some("loaded row"))
            .expect("loaded row replaces placeholder semantics");
        assert!(!loaded.state.busy);
        assert!(!loaded.state.invalid);
        assert!(loaded.state.focused);
    }

    // A slot on its way is the tree's to paint: no delegate is built
    // for it, its placeholder starts with the ground, and it reads as
    // busy; a failed slot names why.
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let t0 = crate::time::now();
        let states = Rc::new(RefCell::new(HashMap::from([
            (0, PagedSlotState::Loading),
            (1, PagedSlotState::Error),
        ])));
        let builds = Rc::new(Cell::new(0_usize));
        let captured = Rc::clone(&builds);
        let mut view = TreeView::from_model(PagedModel {
            count: 2,
            states: Rc::clone(&states),
            self_loading: false,
            requests: Vec::new(),
            changes: Vec::new(),
        })
        .with_expanded([1])
        .with_row_delegate(move |_, _, _| {
            captured.set(captured.get().saturating_add(1));
            Box::new(DelegateProbe::new(
                "row",
                34.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        });
        view.set_viewport(120.0, f32_of_f64(ROW_HEIGHT));
        let _ = lay_out_tree(&mut view, 360.0);
        view.observe_arrivals(t0);

        assert_eq!(builds.get(), 1, "only the resident root is built");
        let pending = TreeSlot {
            parent: 1,
            index: 0,
        };
        assert_eq!(
            view.arrivals
                .face(&pending, SkeletonShape::Text { lines: 1 }, t0)
                .map(|face| face.step()),
            Some(crate::arriving::Step::Ground)
        );
        assert_eq!(
            Widget::next_wake_deadline(&view),
            Some(t0 + crate::arriving::GRACE),
            "its placeholder's next step is a deadline"
        );
        let semantics = Widget::accessibility(&view).unwrap();
        let loading = semantics
            .children
            .iter()
            .find(|row| row.action_target.as_deref() == Some("slot:1:0"))
            .expect("loading slot semantics");
        assert!(loading.state.busy);
        let error = semantics
            .children
            .iter()
            .find(|row| row.action_target.as_deref() == Some("slot:1:1"))
            .expect("error slot semantics");
        assert_eq!(
            error.name.as_deref(),
            Some("Network unavailable. Activate to retry")
        );
        assert!(error.state.invalid);
    });
}

#[test]
fn hundred_million_child_trees_stay_sparse_end_to_end() {
    {
        let model = HugeModel {
            child_id_queries: Cell::new(0),
        };
        let branch = HugeModel::branch_id();
        let expanded = HashSet::from([1, branch]);
        let index = SparseTreeIndex::build(&model, &expanded, false);

        assert_eq!(index.len(), 100_000_003);
        assert_eq!(index.row_at(&model, 0), Some((TreeRowKey::Node(1), 0)));
        assert_eq!(
            index.row_at(&model, HUGE_BRANCH_POSITION.saturating_add(1)),
            Some((TreeRowKey::Node(branch), 1))
        );
        assert_eq!(
            index.row_at(&model, HUGE_BRANCH_POSITION.saturating_add(2)),
            Some((TreeRowKey::Node(HUGE_GRANDCHILD_A), 2))
        );
        assert_eq!(
            index.row_at(&model, HUGE_BRANCH_POSITION.saturating_add(3)),
            Some((TreeRowKey::Node(HUGE_GRANDCHILD_B), 2))
        );
        let after_branch = HugeModel::direct_id(HUGE_BRANCH_POSITION.saturating_add(1)).unwrap();
        assert_eq!(
            index.row_at(&model, HUGE_BRANCH_POSITION.saturating_add(4)),
            Some((TreeRowKey::Node(after_branch), 1))
        );
        assert_eq!(index.index_of(&model, HUGE_GRANDCHILD_B), Some(50_000_003));
        assert_eq!(index.index_of(&model, after_branch), Some(50_000_004));
        assert!(
            model.child_id_queries.get() < 10,
            "resolution must not scan the enormous sibling set"
        );
    }

    {
        let model = HugeModel {
            child_id_queries: Cell::new(0),
        };
        let mut view = TreeView::from_model(model)
            .with_expanded([1, HugeModel::branch_id()])
            .with_accessible_name("Huge tree");
        view.set_viewport(720.0, f32_of_f64(ROW_HEIGHT));
        view.set_scroll_offset(f64_of_usize(HUGE_BRANCH_POSITION) * ROW_HEIGHT);

        assert_eq!(view.visible_len(), 100_000_003);
        assert_eq!(view.viewport.resident_measurements(), 0);
        assert!(view.visible_row_range().len() <= 40);
        let semantics = Widget::accessibility(&view).unwrap();
        assert_eq!(semantics.name.as_deref(), Some("Huge tree"));
        assert!(semantics.children.len() <= 40);
        assert_eq!(semantics.children[0].set_size, Some(100_000_003));
        assert!(
            view.model().child_id_queries.get() < 100,
            "one frame must query only its sparse window"
        );
    }

    {
        let builds = Rc::new(Cell::new(0_usize));
        let captured = Rc::clone(&builds);
        let mut view = TreeView::from_model(HugeModel {
            child_id_queries: Cell::new(0),
        })
        .with_expanded([1, HugeModel::branch_id()])
        .with_row_delegate(move |_, id, _| {
            captured.set(captured.get().saturating_add(1));
            Box::new(DelegateProbe::new(
                id.to_string(),
                22.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        });
        view.set_viewport(110.0, f32_of_f64(ROW_HEIGHT));
        view.set_scroll_offset(f64_of_usize(HUGE_BRANCH_POSITION) * ROW_HEIGHT);
        let _ = lay_out_tree(&mut view, 320.0);

        assert_eq!(view.visible_len(), 100_000_003);
        assert!(view.mounted_rows.len() <= 10);
        assert_eq!(builds.get(), view.mounted_rows.len());
        assert!(view.model().child_id_queries.get() < 100);
    }
}

#[test]
fn delegated_rows_mount_measure_and_handle_input_before_selection() {
    {
        let presses = Rc::new(Cell::new(0_usize));
        let captured = Rc::clone(&presses);
        let mut view = TreeView::new(sample_tree())
            .with_expanded([1])
            .with_row_delegate(move |model, id, _| {
                Box::new(DelegateProbe::new(
                    model.label(id).unwrap_or("row"),
                    22.0,
                    true,
                    Rc::clone(&captured),
                ))
            });
        view.set_viewport(66.0, f32_of_f64(ROW_HEIGHT));
        let (taffy, node) = lay_out_tree(&mut view, 260.0);

        dispatch_laid_out(&mut view, &cursor_moved(80.0, 11.0), &taffy, node);
        assert!(dispatch_laid_out(&mut view, &mouse_press(), &taffy, node));
        assert_eq!(presses.get(), 1);
        assert_eq!(view.selected(), None);

        let semantics = view.accessibility().unwrap();
        assert_eq!(semantics.children[0].role, crate::a11y::Role::TreeItem);
        assert_eq!(
            semantics.children[0].children[0].role,
            crate::a11y::Role::Button
        );
    }

    {
        let builds = Rc::new(Cell::new(0_usize));
        let captured = Rc::clone(&builds);
        let mut view = TreeView::from_model(external_model())
            .with_expanded([1])
            .with_row_delegate(move |model, id, _| {
                captured.set(captured.get().saturating_add(1));
                Box::new(DelegateProbe::new(
                    model.label(id).unwrap_or("row"),
                    if id == 2 { 55.0 } else { 22.0 },
                    false,
                    Rc::new(Cell::new(0)),
                ))
            });
        view.set_viewport(120.0, f32_of_f64(ROW_HEIGHT));
        let _ = lay_out_tree(&mut view, 260.0);
        assert_eq!(builds.get(), 4);
        assert_eq!(view.viewport.item_height(1), Some(55.0));
        assert_eq!(view.viewport.item_top(2), Some(77.0));

        view.update_source(&[TreeModelChange::RowsChanged { ids: vec![2] }], |model| {
            model.nodes.get_mut(&2).unwrap().label = "renamed".into();
        });
        let _ = lay_out_tree(&mut view, 260.0);
        assert_eq!(builds.get(), 5, "only the invalidated id is rebuilt");
        assert_eq!(
            view.mounted_rows
                .iter()
                .find(|row| row.key == TreeRowKey::Node(2))
                .and_then(|row| row.widget.accessibility())
                .and_then(|node| node.name),
            Some("renamed".into())
        );
    }
}

#[test]
fn tree_reorder_drag_previews_commits_rejects_and_hover_expands() {
    // A pointer `rows` rows down the viewport (2.5 = the middle of the
    // third row), so the cases follow the row height.
    fn row_y(rows: f64) -> f64 {
        rows * ROW_HEIGHT
    }
    // Case `tree_reorder_previews_and_commits_a_valid_sibling_drop`.
    fn tree_reorder_previews_and_commits_a_valid_sibling_drop() {
        let mut view = TreeView::from_model(external_model())
            .with_expanded([1])
            .with_reorder(
                |_, _, _| true,
                |model, dropped| {
                    let Some(parent) = model.parent_id(dropped.source) else {
                        return false;
                    };
                    let Some(from) = model.child_index(parent, dropped.source) else {
                        return false;
                    };
                    let Some(target_parent) = model.parent_id(dropped.target.key) else {
                        return false;
                    };
                    if target_parent != parent {
                        return false;
                    }
                    let Some(target) = model.child_index(parent, dropped.target.key) else {
                        return false;
                    };
                    let boundary = match dropped.target.placement {
                        DropPlacement::Before => target,
                        DropPlacement::After => target.saturating_add(1),
                        DropPlacement::Inside => return false,
                    };
                    let to = if boundary > from {
                        boundary.saturating_sub(1)
                    } else {
                        boundary
                    };
                    let children = &mut model.nodes.get_mut(&parent).unwrap().children;
                    let moved = children.remove(from);
                    children.insert(to, moved);
                    true
                },
            );
        view.set_viewport(f32_of_f64(4.0 * ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
        let (taffy, node) = lay_out_tree(&mut view, 240.0);

        dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(2.5)), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(3.9)), &taffy, node);
        assert_eq!(
            view.reorder_target(),
            Some(&DropTarget::new(4, DropPlacement::After))
        );
        assert!(view.reorder_preview().is_some());
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);

        assert_eq!(view.model().nodes.get(&1).unwrap().children, vec![2, 4, 3]);
        assert_eq!(view.visible_index_of(3), Some(3));
        assert_eq!(view.reorder_target(), None);
    }
    tree_reorder_previews_and_commits_a_valid_sibling_drop();

    {
        let mut view = TreeView::from_model(external_model())
            .with_expanded([1, 2])
            .with_reorder(|_, _, _| true, |_, _| true);
        view.set_viewport(f32_of_f64(5.0 * ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
        let (taffy, node) = lay_out_tree(&mut view, 240.0);

        dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(1.5)), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(2.73)), &taffy, node);
        assert!(view.reorder_preview().is_none());
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        assert_eq!(view.model().nodes.get(&1).unwrap().children, vec![2, 3, 4]);
    }

    {
        let mut view = TreeView::from_model(external_model())
            .with_expanded([1])
            .with_reorder(|_, _, _| true, |_, _| false);
        view.reorder = ReorderController::new()
            .with_drag_threshold(0.0)
            .with_hover_expand_delay(Duration::ZERO);
        view.set_viewport(f32_of_f64(4.0 * ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
        let (taffy, node) = lay_out_tree(&mut view, 240.0);

        dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(2.5)), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(1.5)), &taffy, node);
        assert_eq!(
            view.reorder_target(),
            Some(&DropTarget::new(2, DropPlacement::Inside))
        );
        let state = AutoCommit::new();
        assert!(view.update(&state, &automerge::ROOT));
        assert!(view.is_expanded(2));
        assert_eq!(view.visible_index_of(20), Some(2));
    }
}

#[test]
fn structural_transitions_animate_keyed_rows_unless_motion_is_reduced() {
    {
        let mut view = TreeView::from_model(external_model()).with_expanded([1]);
        view.set_viewport(110.0, f32_of_f64(ROW_HEIGHT));
        let _ = lay_out_tree(&mut view, 240.0);
        view.transitions.clear();
        view.refresh_transition_snapshot();

        assert!(view.set_expanded(2, true));
        let now = crate::time::now();
        let (moving, _) = view.transitions.pose(&TreeRowKey::Node(3), now);
        assert!(moving < 0.0, "the row below drops from where it stood");
        let (_, entering) = view.transitions.pose(&TreeRowKey::Node(20), now);
        assert!(entering < 1.0);
        assert!(view.transitions.is_moving(now));

        view.transitions.clear();
        view.refresh_transition_snapshot();
        assert!(view.set_expanded(2, false));
        let now = crate::time::now();
        assert!(
            view.transitions
                .leaving(now)
                .any(|(key, _, opacity)| *key == TreeRowKey::Node(20) && opacity > 0.0)
        );
        assert!(
            view.left_rows.iter().any(|row| {
                row.key == TreeRowKey::Node(20) && row.payload.label == "hidden child"
            })
        );
    }

    {
        let mut view = TreeView::from_model(external_model()).with_expanded([1]);
        view.set_viewport(110.0, f32_of_f64(ROW_HEIGHT));
        view.reduce_motion = true;
        view.transitions.clear();
        view.refresh_transition_snapshot();

        assert!(view.set_expanded(2, true));
        let now = crate::time::now();
        assert!(!view.transitions.is_moving(now));
        assert_eq!(view.transitions.leaving(now).count(), 0);
        assert_eq!(
            view.transitions.pose(&TreeRowKey::Node(20), now),
            (0.0, 1.0)
        );
    }
}

pub(super) fn navigator_tree() -> TreeNode<()> {
    TreeNode::branch(
        1,
        "project",
        (),
        vec![
            TreeNode::branch(
                2,
                "src",
                (),
                vec![
                    TreeNode::leaf(3, "main.rs", ()),
                    TreeNode::leaf(4, "lib.rs", ()),
                ],
            ),
            TreeNode::leaf(5, "README.md", ()),
        ],
    )
}

type Log = Rc<RefCell<Vec<NodeId>>>;

fn logged(view: TreeView<()>) -> (TreeView<()>, Log, Log) {
    let activated = Log::default();
    let selected = Log::default();
    let a = Rc::clone(&activated);
    let s = Rc::clone(&selected);
    let view = view
        .on_activate_view("tree", move |id| a.borrow_mut().push(id))
        .on_select(move |id| s.borrow_mut().push(id));
    (view, activated, selected)
}

pub(super) fn draw_view<T: 'static, M: TreeModel + 'static>(view: &mut TreeView<T, M>) {
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    view.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
}

/// Centre of the visible row showing `id`.
pub(super) fn row_center<T, M: TreeModel>(view: &TreeView<T, M>, id: NodeId) -> Point {
    let index = view.visible_index_of(id).expect("row is visible");
    let top = view.viewport.item_view_top(index).expect("row is laid out");
    let height = view.viewport.item_height(index).expect("row has a height");
    Point::new(
        view.rect.x0 + view.rect.width() * 0.75,
        view.body_top() + top + height / 2.0,
    )
}

fn click_row(view: &mut TreeView<()>, taffy: &TaffyTree<()>, node: taffy::NodeId, id: NodeId) {
    let at = row_center(view, id);
    dispatch_laid_out(view, &cursor_moved(at.x, at.y), taffy, node);
    dispatch_laid_out(view, &mouse_press(), taffy, node);
    dispatch_laid_out(view, &mouse_release(), taffy, node);
}

#[test]
fn activation_follows_click_mode_and_semantic_activate() {
    {
        let (view, activated, _) = logged(
            TreeView::new(navigator_tree())
                .hide_root()
                .click_activation(TreeClickActivation::SingleClick),
        );
        let mut view = view;
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        click_row(&mut view, &taffy, node, 5);
        assert_eq!(*activated.borrow(), [5]);
        assert_eq!(view.selected(), Some(5));
    }

    {
        let (view, activated, _) = logged(TreeView::new(navigator_tree()).hide_root());
        let mut view = view;
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        click_row(&mut view, &taffy, node, 5);
        assert!(activated.borrow().is_empty(), "one click only selects");
        assert_eq!(view.selected(), Some(5));
        click_row(&mut view, &taffy, node, 5);
        assert_eq!(*activated.borrow(), [5], "the second click activates");
    }

    {
        let (mut view, activated, _) = logged(TreeView::new(navigator_tree()).hide_root());
        let mut doc = AutoCommit::new();
        assert!(Widget::perform_semantic_action(
            &mut view,
            "node:5",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(*activated.borrow(), [5]);
        assert_eq!(view.selected(), Some(5));
    }
}

#[test]
fn layout_places_heading_hidden_root_and_viewport() {
    {
        let mut view = TreeView::new(navigator_tree()).hide_root();
        assert_eq!(
            view.visible_rows()
                .into_iter()
                .map(|(id, _, depth)| (id, depth))
                .collect::<Vec<_>>(),
            [(2, 0), (5, 0)]
        );
        view.set_expanded(2, true);
        assert_eq!(
            view.visible_row(1).map(|(id, _, depth)| (id, depth)),
            Some((3, 1))
        );
        assert_eq!(view.visible_index_of(5), Some(3));
        assert_eq!(view.visible_index_of(1), None, "the root is not a row");

        view.set_focus(true);
        view.focused = Some(TreeRowKey::Node(2));
        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowLeft)));
        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowLeft)));
        assert_eq!(
            view.focused(),
            Some(2),
            "Left never walks onto the hidden root"
        );
    }

    {
        let mut view = TreeView::new(navigator_tree())
            .hide_root()
            .with_heading("Explorer");
        let (taffy, node) = lay_out_tree(&mut view, 240.0);
        assert!(view.heading_h > 0.0);
        assert!(view.get_rect().height() >= view.heading_h + view.body_height() - 0.5);
        let in_heading = Point::new(view.rect.x0 + 40.0, view.rect.y0 + view.heading_h / 2.0);
        assert_eq!(
            view.row_key_at(in_heading),
            None,
            "the heading is not a row"
        );
        assert_eq!(
            view.row_key_at(row_center(&view, 2)),
            Some(TreeRowKey::Node(2))
        );
        dispatch_laid_out(
            &mut view,
            &cursor_moved(in_heading.x, in_heading.y),
            &taffy,
            node,
        );
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        assert_eq!(
            view.selected(),
            None,
            "a press on the heading selects nothing"
        );
        let semantics = Widget::accessibility(&view).unwrap();
        assert_eq!(semantics.name.as_deref(), Some("Explorer"));
    }

    {
        let children = (2_u64..=40)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children))
            .hide_root()
            .fill();
        view.place_at(Rect::new(-120.0, 50.0, 120.0, 250.0));
        assert!(
            (view.viewport.viewport() - 200.0).abs() < 0.01,
            "the viewport is the rect"
        );
        assert!(view.max_scroll_offset() > 0.0, "39 rows overflow 200 px");
        assert_eq!(
            view.row_key_at(Point::new(0.0, 51.0)),
            Some(TreeRowKey::Node(2)),
            "hit-testing follows the external rect"
        );
        assert_eq!(view.row_key_at(Point::new(200.0, 51.0)), None);
        view.place_at(Rect::new(0.0, 0.0, 240.0, 100.0));
        assert!(
            (view.viewport.viewport() - 100.0).abs() < 0.01,
            "and follows it each frame"
        );
    }
}
