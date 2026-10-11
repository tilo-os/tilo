//! Workspace history over the strip: the shared Activity pane (Catch up,
//! Explore, Replay and Tasks) reading a host's retained history, drawn
//! modally over the shown strip as the native desktop draws it.
//!
//! The pane only reads: it works on an [`ActivitySnapshot`] the host built
//! from its admitted room document, and on detached copies of the records
//! the workspace's windows show. Nothing it does writes either.
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop_model::{DesktopTree, TabId, WorkspaceId};
use ui::kurbo::{Affine, Point, Rect};
use ui::taffy::{NodeId, TaffyTree};
use ui::vello::Scene;
use ui::widget::Widget;
use ui::winit::event::{ElementState, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};

use crate::activity::{
    ActivityHost, ActivitySnapshot, ActivityView as PaneView, DetachedRecordHistory,
    RoomActivityPane,
};
use crate::navigation::ActivityView;

/// Where a surface's workspace history comes from. Without one, Catch up,
/// Explore and Replay stay unavailable.
pub trait HistorySource {
    /// `workspace`'s admitted history: the desktop together with the room
    /// document whose changes are its history. `None` when the host retains
    /// none for it.
    fn workspace_history(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceId,
    ) -> Option<ActivitySnapshot>;

    /// Detached copies of the records `workspace`'s windows show, each with
    /// its whole change history.
    fn record_histories(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceId,
    ) -> Vec<DetachedRecordHistory>;

    /// Some retained desktop history exceeded the host's projection budget.
    fn history_is_partial(&self) -> bool {
        false
    }

    /// What the pane may ask of the host: identity, the device's private
    /// seen frontiers, application names.
    fn host(&self) -> Rc<dyn ActivityHost>;

    /// Name the authors the host knows before the pane reads changes.
    fn register_authors(&self, _authors: &mut ui::AuthorDirectory) {}

    /// The window showing `target`, focused when the person opens it from
    /// the history.
    fn resource_tab(
        &self,
        _tree: &DesktopTree,
        _target: &ui::HistoryResourceTarget,
    ) -> Option<TabId> {
        None
    }

    /// Changes whenever a record the history reads changed, so an open pane
    /// reads again.
    fn revision(&self) -> u64 {
        0
    }
}

pub(super) fn pane_view(view: ActivityView) -> PaneView {
    match view {
        ActivityView::CatchUp => PaneView::CatchUp,
        ActivityView::Explore => PaneView::Explore,
        ActivityView::Replay => PaneView::Replay,
        ActivityView::Tasks => PaneView::Tasks,
    }
}

pub(super) fn navigation_view(view: PaneView) -> ActivityView {
    match view {
        PaneView::CatchUp => ActivityView::CatchUp,
        PaneView::Explore => ActivityView::Explore,
        PaneView::Replay => ActivityView::Replay,
        PaneView::Tasks => ActivityView::Tasks,
    }
}

/// What the person did in the pane that the surface acts on.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Exit {
    pub dismissed: bool,
    pub open: Option<ui::HistoryResourceTarget>,
}

/// The open pane and the history it reads.
pub(super) struct HistoryOverlay {
    pane: RoomActivityPane,
    snapshot: ActivitySnapshot,
    pub workspace: WorkspaceId,
    /// The view the presentation last named.
    pub shown: ActivityView,
    /// The tree and source revisions the pane last read.
    read: (u64, u64),
    exit: Exit,
    cursor: Option<Point>,
    focused: bool,
    dirty: bool,
    rect: Rect,
}

impl HistoryOverlay {
    pub fn open(
        source: &mut dyn HistorySource,
        tree: &DesktopTree,
        revision: u64,
        workspace: WorkspaceId,
        view: ActivityView,
    ) -> Option<Self> {
        let snapshot = source.workspace_history(tree, workspace)?;
        let mut pane = RoomActivityPane::new();
        pane.set_host(source.host());
        pane.load_frontiers();
        pane.begin_view(workspace.raw());
        pane.set_view(pane_view(view));
        let mut overlay = Self {
            pane,
            snapshot,
            workspace,
            shown: view,
            read: (revision, source.revision()),
            exit: Exit::default(),
            cursor: None,
            focused: true,
            dirty: true,
            rect: Rect::ZERO,
        };
        overlay.read_records(source, tree);
        Some(overlay)
    }

    fn read_records(&mut self, source: &mut dyn HistorySource, tree: &DesktopTree) {
        let records = source.record_histories(tree, self.workspace);
        source.register_authors(self.pane.authors_mut());
        self.pane.refresh(&mut self.snapshot);
        self.pane
            .refresh_detached_records(&mut self.snapshot, records);
        if source.history_is_partial() {
            self.pane.mark_desktop_history_capped();
        }
        self.dirty = true;
    }

    /// Read the history again when the desktop or a record changed since.
    pub fn follow(&mut self, source: &mut dyn HistorySource, tree: &DesktopTree, revision: u64) {
        let now = (revision, source.revision());
        if now == self.read {
            return;
        }
        self.read = now;
        if let Some(snapshot) = source.workspace_history(tree, self.workspace) {
            self.snapshot = snapshot;
        }
        self.read_records(source, tree);
    }

    pub fn view(&self) -> ActivityView {
        navigation_view(self.pane.view())
    }

    pub fn set_view(&mut self, view: ActivityView) {
        if self.view() != view {
            self.pane.set_view(pane_view(view));
            self.pane.layout(self.rect);
            self.dirty = true;
        }
    }

    pub fn playing(&self) -> bool {
        self.pane.replay_is_playing()
    }

    pub fn partial(&self) -> bool {
        !self.pane.feed_coverage().is_complete()
    }

    pub fn take_exit(&mut self) -> Exit {
        std::mem::take(&mut self.exit)
    }

    /// The person left: remember what this device has now seen.
    pub fn finish(&mut self) {
        self.pane.finish_view(self.workspace.raw());
    }

    #[cfg(test)]
    pub fn pane(&self) -> &RoomActivityPane {
        &self.pane
    }

    #[cfg(test)]
    pub fn pane_mut(&mut self) -> &mut RoomActivityPane {
        &mut self.pane
    }

    pub fn route(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: ui::winit::event::Modifiers,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Some(Point::new(position.x / scale, position.y / scale));
        }
        let escape = matches!(
            event,
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key == Key::Named(NamedKey::Escape)
        );
        if escape && !self.pane.wants_keyboard() {
            self.exit.dismissed = true;
        } else {
            let outcome =
                self.pane
                    .handle_event(event, scale, &modifiers, self.cursor, &mut self.snapshot);
            if outcome.open_target.is_some() {
                self.exit.open = outcome.open_target;
            }
        }
        self.dirty = true;
    }
}

impl Widget for HistoryOverlay {
    fn name(&self) -> &'static str {
        "WorkspaceActivity"
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    fn modal_capture(&self) -> Option<Rect> {
        Some(self.rect)
    }
    fn traps_focus(&self) -> bool {
        true
    }
    fn wants_tab_input(&self) -> bool {
        true
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(self.pane.accessibility_snapshot())
    }
    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.pane.accessibility_rect_for_path(path)
    }
    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let changed = self.pane.focus_accessibility_path(path);
        self.dirty |= changed;
        changed
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn needs_animation_frame(&self) -> bool {
        self.pane.replay_is_playing() || self.pane.needs_animation_frame(ui::time::now())
    }
    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _theme: &ui::Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = tree
            .new_leaf(overlay_style())
            .unwrap_or_else(|_| NodeId::from(0u64));
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(tree, node, origin);
        self.pane.layout(self.rect);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.pane.observe_replay_presence(presence);
        self.pane.draw(scene, transform, text, theme, presence);
        self.dirty = false;
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.route(event, ctx.scale_factor, *ctx.modifiers);
        *ctx.handled = true;
    }
}

/// A leaf over the whole surface, out of the strip's flow.
pub(super) fn overlay_style() -> ui::taffy::Style {
    ui::taffy::Style {
        position: ui::taffy::Position::Absolute,
        inset: ui::taffy::Rect {
            left: ui::taffy::LengthPercentageAuto::length(0.0),
            top: ui::taffy::LengthPercentageAuto::length(0.0),
            right: ui::taffy::LengthPercentageAuto::auto(),
            bottom: ui::taffy::LengthPercentageAuto::auto(),
        },
        size: ui::taffy::Size {
            width: ui::taffy::Dimension::percent(1.0),
            height: ui::taffy::Dimension::percent(1.0),
        },
        ..Default::default()
    }
}
