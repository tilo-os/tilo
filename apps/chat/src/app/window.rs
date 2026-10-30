//! The chat window as the view mounts it: the [`ChatApp`] it shows, shared
//! with the view's effect handler and what the view publishes beside the
//! record, so an effect answers from, and acts on, the window it is about.
//!
//! The window lays out, draws and handles input as the chat does; it adds
//! the scenario a capture stages, done on the next sync.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use ui::Widget;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppModel, Collaborative, Continuable, FloatingSurfaceProvider, ModelPath};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::ChatApp;

/// The [`ChatApp`] the window shows, as the view and the window share it.
pub(crate) type SharedChat = Rc<RefCell<ChatApp>>;

/// The chat window: the [`ChatApp`] it shares with the view.
pub struct ChatWindow {
    chat: SharedChat,
    /// A capture's scenario, staged on the next sync.
    #[cfg(feature = "samples")]
    staged: Rc<RefCell<Option<Staged>>>,
}

/// The scenario a capture stages (`None` for a plain capture).
#[cfg(feature = "samples")]
struct Staged(Option<String>);

impl ChatWindow {
    pub(crate) fn new(chat: SharedChat) -> Self {
        Self {
            chat,
            #[cfg(feature = "samples")]
            staged: Rc::default(),
        }
    }

    /// The chat this window shows.
    pub fn state(&self) -> RefMut<'_, ChatApp> {
        self.chat.borrow_mut()
    }

    /// What stages a capture's scenario on the window's next sync.
    #[cfg(feature = "samples")]
    pub(crate) fn stager(&self) -> impl FnMut(Option<&str>) + 'static {
        let staged = Rc::clone(&self.staged);
        move |scenario| *staged.borrow_mut() = Some(Staged(scenario.map(str::to_owned)))
    }
}

impl Widget for ChatWindow {
    ui::provides!(
        dyn ui::widget::Collaborative,
        dyn ui::widget::Continuable,
        dyn ui::widget::FloatingSurfaceProvider,
        dyn ui::collaboration::participants::Participants,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::application_view::ApplicationViewState,
        dyn ui::view::capability::ViewState,
        dyn ui::semantic_document_read::SemanticReaders
    );

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        self.state().settle(edits, path);
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.state().set_motion_reduced(reduced);
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.chat.borrow().primary_vertical_scroll()
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.state().apply_primary_vertical_scroll(action)
    }

    fn is_focusable(&self) -> bool {
        self.chat.borrow().is_focusable()
    }

    fn is_focused(&self) -> bool {
        self.chat.borrow().is_focused()
    }

    fn set_focus(&mut self, focused: bool) {
        self.state().set_focus(focused);
    }

    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        self.chat.borrow().ime_content()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.chat.borrow().ime_cursor_rect()
    }

    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        #[cfg(feature = "samples")]
        if let Some(Staged(scenario)) = self.staged.borrow_mut().take() {
            let mut chat = self.state();
            chat.stage_parts(scenario.as_deref());
            chat.stage_scenario(scenario.as_deref().unwrap_or_default());
        }
        self.state().sync(doc, path);
    }

    fn composition_follows_allocation(&self) -> bool {
        self.chat.borrow().composition_follows_allocation()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.state().build_layout(taffy, theme, node_map);
        // The window stands in for the chat at the chat's node, as a
        // decorator does for what it wraps: what reaches the node reaches
        // the chat through the window.
        let chat = self.chat.as_ptr().cast::<()>().cast_const();
        if node_map
            .get(&node)
            .is_some_and(|registered| std::ptr::eq(registered.cast::<()>().cast_const(), chat))
        {
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        }
        node
    }

    fn stands_for(&self, address: *const ()) -> bool {
        std::ptr::from_ref(self).cast::<()>() == address
            || self.chat.as_ptr().cast::<()>().cast_const() == address
    }

    // The window stands for the chat's node: the walk visits the chat.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        visitor(&*self.chat.borrow());
        true
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.state().apply_layout(taffy, node, origin);
    }

    fn is_layout_dirty(&self) -> bool {
        self.chat.borrow().is_layout_dirty()
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.state().draw(scene, tf, tr, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.state().draw_overlay(scene, tf, tr, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.state().handle_event(event, ctx);
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.chat.borrow().tooltip_target_at(point)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.chat.borrow().cursor_shape(point)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.chat.borrow().accessibility()
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.chat.borrow().synthetic_accessibility_rect(path)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        self.state().focus_synthetic_accessibility(path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.state()
            .perform_semantic_action(target, action, state, path)
    }

    // Named as the chat it shows.
    fn name(&self) -> &'static str {
        "ChatApp"
    }

    fn get_rect(&self) -> Rect {
        self.chat.borrow().get_rect()
    }

    fn traps_focus(&self) -> bool {
        self.chat.borrow().traps_focus()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.chat.borrow().modal_capture()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.state().take_structure_dirty()
    }

    fn update(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        self.state().update(doc, path)
    }

    fn needs_animation_frame(&self) -> bool {
        self.chat.borrow().needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.chat.borrow().next_wake_deadline()
    }

    fn needs_idle_sync(&self) -> bool {
        #[cfg(feature = "samples")]
        if self.staged.borrow().is_some() {
            return true;
        }
        self.chat.borrow().needs_idle_sync()
    }

    fn wants_pointer_capture(&self) -> bool {
        self.chat.borrow().wants_pointer_capture()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.chat.borrow().has_pending_visual_work()
    }

    fn needs_redraw(&self) -> bool {
        self.chat.borrow().needs_redraw()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.chat.borrow().collect_damage(regions);
    }

    fn damage_covers_own_box(&self) -> bool {
        self.chat.borrow().damage_covers_own_box()
    }

    fn release_scene_cache(&mut self) {
        self.state().release_scene_cache();
    }

    fn mark_drawn(&mut self) {
        self.state().mark_drawn();
    }
}

impl ui::semantic_document_read::SemanticReaders for ChatWindow {
    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        self.chat.borrow().semantic_readers()
    }
}

impl Continuable for ChatWindow {
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        path: &ObjId,
    ) -> Option<ui::collaboration::ContinuationMetadata> {
        self.chat.borrow().continuation_metadata(state, path)
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        self.state()
            .restore_continuation_metadata(metadata, state, path)
    }
}

impl Collaborative for ChatWindow {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        self.state().take_collab_requests()
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        self.state().handle_collab_event(event);
    }
}

impl ui::presence::capability::PublishesPresence for ChatWindow {
    fn presence_payload(&self) -> Option<(ui::presence::PresenceKey, ui::PresencePayload)> {
        self.chat.borrow().presence_payload()
    }
}

impl ui::collaboration::participants::Participants for ChatWindow {
    fn join_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool {
        self.state().join_participant(participant)
    }

    fn leave_participant(&mut self, participant: &tilo_actions::ParticipantId) {
        self.state().leave_participant(participant);
    }

    fn set_participant_label(&mut self, participant: &tilo_actions::ParticipantId, label: &str) {
        self.state().set_participant_label(participant, label);
    }
}

impl ui::view::capability::ViewState for ChatWindow {
    fn view_state(&self) -> Option<ui::view::ViewContribution> {
        self.chat.borrow().view_state()
    }

    fn restore_view_state(&mut self, view: &ui::view::ViewContribution) {
        self.state().restore_view_state(view);
    }
}

impl ui::application_view::ApplicationViewState for ChatWindow {
    fn capture_application_view(
        &self,
    ) -> Result<Option<ui::application_view::ApplicationLocalView>, ui::view::ThawError> {
        self.chat.borrow().capture_application_view()
    }

    fn restore_application_view(
        &mut self,
        local: &ui::application_view::ApplicationLocalView,
        doc: &AutoCommit,
    ) -> Result<ui::application_view::ApplicationFocusRestore, ui::view::ThawError> {
        self.state().restore_application_view(local, doc)
    }

    fn finish_application_view_restore(&mut self) -> Result<(), ui::view::ThawError> {
        self.state().finish_application_view_restore()
    }
}

impl FloatingSurfaceProvider for ChatWindow {
    fn floating_surface_accessibility(&self, id: &str) -> Option<ui::a11y::AccessNode> {
        self.chat.borrow().floating_surface_accessibility(id)
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        self.state()
            .perform_floating_surface_accessibility_action(id, target, action, state, path)
    }

    fn floating_surface_ime(&self, id: &str) -> Option<(ui::ime::InputContent, Rect)> {
        self.chat.borrow().floating_surface_ime(id)
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<ui::floating_surface::FloatingSurfaceRequest> {
        self.state().take_floating_surface_request()
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.state()
            .draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
    }

    fn draw_floating_surface(
        &mut self,
        id: &str,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> bool {
        self.state()
            .draw_floating_surface(id, scene, transform, text_renderer, theme, presence)
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<ui::floating_surface::FloatingSurfacePresentation> {
        self.chat.borrow().floating_surface_presentations()
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        self.chat.borrow().active_floating_surface_ids()
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        self.state().set_floating_surface_bounds(bounds);
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        self.state().on_floating_surface_host_message(message);
    }

    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
        self.state().on_floating_surface_focus_changed(id, focused);
    }

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        self.state().on_floating_surface_ime_source_changed(id);
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        self.state().on_floating_surface_event_source(id);
    }
}
