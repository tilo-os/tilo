//! The window: the [`PlayerState`] the view shares with its effect handler,
//! laid out with its player hosted as the window's one child.

use super::{
    Affine, AppModel, EffectStatus, Element, FloatingSurfaceProvider, FullscreenProvider, HashMap,
    LaunchInput, Live, ModelPath, NodeId, PlayerResource, PlayerState, Point, PresenceCtx, Rc,
    Rect, Ref, RefCell, RefMut, RunMode, Scene, TaffyTree, TextRenderer, Theme, VIDEO_SURFACE,
    VideoEffect, VideoPlayer, ViewCx, Widget, WindowEvent,
};
use crate::actions;
use ui::framework::Publishes;

/// The player's window over the shared [`PlayerState`].
pub struct PlayerApp {
    state: Rc<RefCell<PlayerState>>,
    /// The player as the window's child in the widget tree.
    host: PlayerHost,
    /// The shared library as the document holds it, shown as it changes;
    /// `None` for a window outside the framework.
    document: Option<Live<PlayerResource>>,
    presented: Option<(u64, u64)>,
}

impl PlayerApp {
    /// A player with a fresh engine and no media loaded, for a window built
    /// in `mode`; see [`PlayerState::new`].
    pub fn new(mode: RunMode) -> Self {
        Self::over(PlayerState::new(mode))
    }

    /// The window over `state`.
    pub(crate) fn over(state: PlayerState) -> Self {
        let state = Rc::new(RefCell::new(state));
        Self {
            host: PlayerHost {
                state: Rc::clone(&state),
            },
            state,
            document: None,
            presented: None,
        }
    }

    /// What the window shows, shared with the view's effect handler.
    pub(crate) fn state(&self) -> RefMut<'_, PlayerState> {
        self.state.borrow_mut()
    }

    /// Open what the launch named; see [`PlayerState::open_launch`].
    pub fn open_launch(&mut self, launch: &LaunchInput<'_>) {
        self.state().open_launch(launch);
    }

    /// Read access to the inner video surface, for tests that assert a
    /// decoded frame landed.
    pub fn player(&self) -> Ref<'_, VideoPlayer> {
        Ref::map(self.state.borrow(), |state| &state.player)
    }

    /// Show the shared library the document holds now, when it changed
    /// since.
    fn follow_document(&mut self) {
        let Some(document) = &self.document else {
            return;
        };
        let Some(shown) = document.changed_since(&mut self.presented) else {
            return;
        };
        self.state().present_library(&shown);
    }
}

impl Default for PlayerApp {
    fn default() -> Self {
        Self::new(RunMode::Live)
    }
}

/// The view over `window`: the shared library's verbs bound to the playhead,
/// the window's state attached to the framework and shared with the effect
/// handler, and the live rows of what the player shows.
pub(crate) fn window(cx: &ViewCx<PlayerResource>, mut window: PlayerApp) -> Element {
    window.document = Some(cx.live().clone());
    // The playhead is the player's, not the document's, so the player says
    // what it is on and the view publishes those verbs.
    let playhead = crate::resource::Playhead::default();
    let published = Rc::clone(&playhead);
    cx.bind_declarations(move |_| published.borrow().clone());
    let state = Rc::clone(&window.state);
    {
        let mut state = state.borrow_mut();
        state.share_playhead(&playhead);
        state.attach_framework(cx);
    }
    {
        let (answer, status) = (Rc::clone(&state), Rc::clone(&state));
        cx.effects(move |effect: VideoEffect, cx| answer.borrow_mut().answer_effect(effect, cx))
            .status(move || -> Vec<EffectStatus> { status.borrow().effect_statuses() });
    }
    {
        let state = Rc::clone(&state);
        cx.on_stage(move |scenario| {
            let mut state = state.borrow_mut();
            let launched = state.capture_launched.take();
            state.stage_capture(launched.as_deref(), scenario);
        });
    }
    let (rows, row) = (Rc::clone(&state), state);
    cx.publishing(
        Publishes::new()
            .live_rows(move || {
                actions::live_rows(
                    &rows.borrow().player_rows(),
                    actions::CONTEXTUAL_CHILD_LIMIT,
                )
            })
            .live_row(move |id| actions::live_row(&row.borrow().player_rows(), id)),
        Box::new(window),
    )
}

impl FloatingSurfaceProvider for PlayerApp {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.state().pending_pip.take()
    }
}

impl FullscreenProvider for PlayerApp {
    fn fullscreen_intent(&self) -> Option<bool> {
        FullscreenProvider::fullscreen_intent(&self.state.borrow().player)
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        FullscreenProvider::fullscreen_params(&self.state.borrow().player)
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        FullscreenProvider::on_fullscreen_changed(&mut self.state().player, active);
    }
}

impl ui::widget::controls::ActionControls for PlayerApp {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        ui::widget::semantic_action_bindings(Some(self.state.borrow().accessibility()))
    }
}

impl Widget for PlayerApp {
    fn has_pending_visual_work(&self) -> bool {
        self.state.borrow().has_pending_visual_work()
    }

    ui::provides!(
        dyn ui::widget::FullscreenProvider,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::widget::controls::ActionControls,
    );

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow_document();
        self.state().sync(state, path);
    }

    fn settle(&mut self, _edits: &mut ui::widget::Edits<'_>, _path: &automerge::ObjId) {
        self.state.borrow().settle();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let (root, player) = self.state().build_layout(taffy, theme);
        node_map.insert(player, std::ptr::from_mut::<dyn Widget>(&mut self.host));
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        self.state().adjust_layout_style(taffy, node, text)
    }

    /// The player's place follows the docked library, which follows the
    /// window's width.
    fn composition_follows_allocation(&self) -> bool {
        true
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.state().apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.state()
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.state().handle_event(event, ctx) {
            return;
        }
        // Forward everything else to the player (its controls + shortcuts).
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        if let Some(&player_node) = children.first() {
            self.host
                .handle_event(event, &mut ctx.reborrow(ctx.path, player_node));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.host as &dyn Widget]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.host as &mut (dyn Widget + '_)]
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(self.state.borrow().accessibility())
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.state.borrow().synthetic_accessibility_rect(path)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        self.state().focus_synthetic_accessibility(path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        edits: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.state()
            .perform_semantic_action(target, action, edits, path)
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        let state = self.state.borrow();
        if !state.rect.contains(point) {
            return None;
        }
        state
            .tooltip()
            .filter(|tooltip| !tooltip.is_empty())
            .map(|tooltip| (state.rect, tooltip.to_owned()))
    }

    fn name(&self) -> &'static str {
        "PlayerApp"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.state.borrow().rect
    }

    fn needs_redraw(&self) -> bool {
        self.state.borrow().needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.state().mark_drawn();
    }

    fn is_layout_dirty(&self) -> bool {
        self.state.borrow().is_layout_dirty()
    }

    fn needs_animation_frame(&self) -> bool {
        self.state.borrow().needs_animation_frame()
    }

    fn needs_idle_sync(&self) -> bool {
        self.state.borrow().needs_idle_sync()
    }
}

/// The window's player in the widget tree: every pass the tree runs on the
/// player reaches it in the shared state, where the view's effect handler
/// drives it too.
pub(super) struct PlayerHost {
    state: Rc<RefCell<PlayerState>>,
}

impl PlayerHost {
    fn player(&self) -> Ref<'_, VideoPlayer> {
        Ref::map(self.state.borrow(), |state| &state.player)
    }

    fn player_mut(&self) -> RefMut<'_, VideoPlayer> {
        RefMut::map(self.state.borrow_mut(), |state| &mut state.player)
    }
}

impl FullscreenProvider for PlayerHost {
    fn fullscreen_intent(&self) -> Option<bool> {
        FullscreenProvider::fullscreen_intent(&*self.player())
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        FullscreenProvider::fullscreen_params(&*self.player())
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        FullscreenProvider::on_fullscreen_changed(&mut *self.player_mut(), active);
    }
}

impl Widget for PlayerHost {
    ui::provides!(dyn ui::widget::FullscreenProvider);

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.player_mut().sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self
            .player_mut()
            .build_layout(taffy, theme, &mut HashMap::new());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.player_mut().apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.player_mut()
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.player_mut().handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "VideoPlayer"
    }

    fn test_id(&self) -> Option<&str> {
        Some(VIDEO_SURFACE)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.player().cursor_shape(point)
    }

    fn is_focusable(&self) -> bool {
        self.player().is_focusable()
    }

    fn set_focus(&mut self, focused: bool) {
        self.player_mut().set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.player().is_focused()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.player().wants_pointer_capture()
    }

    fn needs_animation_frame(&self) -> bool {
        self.player().needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.player().needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.player_mut().mark_drawn();
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.player().accessibility()
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.player().synthetic_accessibility_rect(path)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        self.player_mut().focus_synthetic_accessibility(path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        edits: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.player_mut()
            .perform_semantic_action(target, action, edits, path)
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.player().tooltip_target_at(point)
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.player_mut().set_motion_reduced(reduced);
    }

    fn get_rect(&self) -> Rect {
        self.player().get_rect()
    }
}
