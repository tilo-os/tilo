//! The word processor's window: the [`Editor`] it shows, shared with the
//! view's effect handler and the rows it publishes, and the document it
//! follows. The editor draws everything itself, with no child widgets, so
//! the window answers the widget contract with it.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use ui::collaboration::participants::Participants;
use ui::framework::Live;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::undo::LocalUndo;
use ui::widget::{AppModel, Findable, ModelPath, TaffyTreeExt, Widget};
use ui::{PresenceCtx, PresenceKey};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use super::Editor;
use crate::framework::Document;

/// The word processor's window: the editor it shows and the document it
/// follows.
pub struct WordProcessorWidget {
    editor: Rc<RefCell<Editor>>,
    /// The document as the framework keeps it, shown as it changes; `None`
    /// for a window outside the framework.
    live: Option<Live<Document>>,
    /// The `(revision, loads)` of the document last shown.
    presented: Option<(u64, u64)>,
}

impl Default for WordProcessorWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl WordProcessorWidget {
    pub fn new() -> Self {
        Self::over(Editor::new())
    }

    /// A window showing `editor`.
    pub(crate) fn over(editor: Editor) -> Self {
        Self {
            editor: Rc::new(RefCell::new(editor)),
            live: None,
            presented: None,
        }
    }

    /// This window following the document `live` holds, shown at once.
    #[must_use]
    pub(crate) fn following(mut self, live: Live<Document>) -> Self {
        self.live = Some(live);
        self.follow_document();
        self
    }

    /// The editor the window shows, shared with the view's effect handler
    /// and publications.
    pub fn state(&self) -> RefMut<'_, Editor> {
        self.editor.borrow_mut()
    }

    /// The editor of a window nothing else shares it with: one a test
    /// mounted by itself.
    #[cfg(test)]
    pub(crate) fn editor_mut(&mut self) -> &mut Editor {
        Rc::get_mut(&mut self.editor)
            .expect("only a window outside a view lends its editor")
            .get_mut()
    }

    /// The editor, for the view's effect handler and publications to hold.
    pub(crate) fn editor(&self) -> Rc<RefCell<Editor>> {
        Rc::clone(&self.editor)
    }

    /// Show the document as it is now, when it changed since.
    fn follow_document(&mut self) {
        if let Some(state) = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented))
        {
            self.state().present(&state);
        }
    }
}

/// The page is this window's content, so the window's zoom gestures are the
/// page's: Ctrl+Plus, Ctrl+Minus and Ctrl+0, Ctrl+scroll and a pinch scale
/// the sheet — the words — and the window bar says the level. Without it the
/// window would grow its own chrome while the page a person is trying to
/// read stayed the size it was.
impl ui::content_zoom::ContentZoom for WordProcessorWidget {
    fn zoom_content(&mut self, step: ui::content_zoom::ZoomStep) -> Option<f32> {
        Some(self.state().zoom_page(step))
    }
}

impl Widget for WordProcessorWidget {
    ui::provides!(
        dyn ui::collaboration::participants::Participants,
        dyn ui::undo::LocalUndo,
        dyn ui::resources::PublishesResources,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::semantic_document_read::SemanticReaders,
        dyn ui::widget::Findable,
        dyn ui::content_zoom::ContentZoom
    );

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        Some(self.editor.borrow().primary_vertical_scroll())
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.state().apply_primary_vertical_scroll(action)
    }

    fn native_input_snapshot(
        &self,
        state: &AppModel,
        path: &ModelPath,
    ) -> Option<serde_json::Value> {
        Some(self.editor.borrow().native_input_snapshot(state, path))
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        self.follow_document();
        self.state().sync(state, root);
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        self.state().settle(edits, path);
    }

    /// One self-drawn leaf filling its tile.
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.state()
            .place(ui::widget::layout_rect(taffy, node, origin));
    }

    fn get_rect(&self) -> Rect {
        self.editor.borrow().get_rect()
    }

    fn needs_animation_frame(&self) -> bool {
        self.editor.borrow().needs_animation_frame()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.editor.borrow().modal_capture()
    }

    fn traps_focus(&self) -> bool {
        self.editor.borrow().traps_focus()
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
        self.state().handle_event(event, ctx);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.state()
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn name(&self) -> &'static str {
        "WordProcessor"
    }

    // A test, or a host that embeds the editor, reaches the window by its
    // type.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(self.editor.borrow().accessibility())
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

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.state()
            .perform_access_action(action, key_path, state, path)
    }

    fn test_id(&self) -> Option<&str> {
        Some("word_processor")
    }

    fn claims_raw_touch(&self, point: Point) -> bool {
        self.editor.borrow().claims_raw_touch(point)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.editor.borrow().wants_pointer_capture()
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.state().set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.editor.borrow().is_focused()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.editor.borrow().ime_cursor_rect()
    }

    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        self.editor.borrow().ime_content()
    }

    fn take_action_palette_request(&mut self) -> Option<tilo_actions::ActionAddress> {
        self.state().take_action_palette_request()
    }

    fn needs_redraw(&self) -> bool {
        self.editor.borrow().needs_redraw()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.editor.borrow().next_wake_deadline()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.editor.borrow().collect_damage(regions);
    }

    fn damage_covers_own_box(&self) -> bool {
        self.editor.borrow().damage_covers_own_box()
    }

    fn release_scene_cache(&mut self) {
        self.state().release_scene_cache();
    }

    fn mark_drawn(&mut self) {
        self.state().mark_drawn();
    }

    fn is_layout_dirty(&self) -> bool {
        self.editor.borrow().is_layout_dirty()
    }
}

impl ui::presence::capability::PublishesPresence for WordProcessorWidget {
    fn presence_payload(&self) -> Option<(PresenceKey, ui::PresencePayload)> {
        self.editor.borrow().presence_payload()
    }

    fn shared_selection(&self) -> Option<ui::shared_selection::SharedSelection> {
        self.editor.borrow().shared_selection()
    }
}

impl ui::resources::PublishesResources for WordProcessorWidget {
    // The document is the framework's publication; the editor adds only
    // what an edit answers its caller with.
    fn document_action_result(
        &self,
        doc: &AutoCommit,
        _address: &tilo_actions::ActionAddress,
        changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        Some(crate::semantic_read::changes::summary(doc, changes).into())
    }
}

impl ui::semantic_document_read::SemanticReaders for WordProcessorWidget {
    fn semantic_readers(&self) -> &'static [ui::semantic_document_read::RegisteredSemanticReader] {
        &crate::semantic_read::action::READERS
    }
}

impl LocalUndo for WordProcessorWidget {
    fn handle_local_undo_request(&mut self, request: ui::undo::UndoRequest) -> bool {
        self.state().handle_local_undo_request(request)
    }
}

impl Participants for WordProcessorWidget {
    fn join_participant(&mut self, participant: &tilo_actions::ParticipantId) -> bool {
        self.state().join_participant(participant)
    }
    fn leave_participant(&mut self, participant: &tilo_actions::ParticipantId) {
        self.state().leave_participant(participant);
    }
    fn set_participant_label(&mut self, participant: &tilo_actions::ParticipantId, label: &str) {
        self.state().set_participant_label(participant, label);
    }
    fn participant_presence(&self, doc: &AutoCommit) -> Vec<ui::ParticipantPresence> {
        self.editor.borrow().participant_presence(doc)
    }
}

impl Findable for WordProcessorWidget {
    fn find(
        &mut self,
        query: &str,
        options: ui::widget::FindOptions,
    ) -> Vec<ui::widget::FindMatch> {
        self.state().find(query, options)
    }
    fn reveal(&mut self, index: usize) {
        self.state().reveal(index);
    }
    fn mark_current(&mut self, index: usize) {
        self.state().mark_current(index);
    }
    fn set_find_cover(&mut self, cover: Option<Rect>) {
        self.state().set_find_cover(cover);
    }
    fn clear_find(&mut self) {
        self.state().clear_find();
    }
    fn supports_replace(&self) -> bool {
        self.editor.borrow().supports_replace()
    }
    fn replace(&mut self, index: usize, with: &str) -> bool {
        self.state().replace(index, with)
    }
    fn replace_all(&mut self, with: &str) -> usize {
        self.state().replace_all(with)
    }
    fn replaces_later(&self) -> bool {
        self.editor.borrow().replaces_later()
    }
    fn supports_hex(&self) -> bool {
        self.editor.borrow().supports_hex()
    }
    fn supports_regex(&self) -> bool {
        self.editor.borrow().supports_regex()
    }
    fn step(&mut self, forward: bool) -> bool {
        self.state().step(forward)
    }
    fn content_revision(&self) -> u64 {
        self.editor.borrow().content_revision()
    }
    fn take_engine_results(&mut self) -> Option<(usize, Option<usize>)> {
        self.state().take_engine_results()
    }
}

/// Lets the shell host the word processor in a tab directly, the same way
/// it hosts the terminal and the file explorer, instead of standing a
/// placeholder card in its place.
impl ui::widget::AppWidget for WordProcessorWidget {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The toolkit's app-kind vocabulary has no separate "documents"
        // variant; a word processor is an editor of text, which is what
        // `Editor` names and what its tab glyph depicts.
        ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &'static str {
        "Document"
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        // Nerd Font document glyph, matching the convention the other
        // hosted apps use for their tab pill.
        ui::widget::IconDescriptor::Glyph("󰈬")
    }

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        Some(ui::widget::AppKind::Editor)
    }
}
