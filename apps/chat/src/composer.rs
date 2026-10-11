//! The composers: the SDK rich-text editor, embedded where chat draws it.
//!
//! A channel or thread composer edits that surface's shared draft: rich text
//! in the document, a child resource of the chat record whose every edit is
//! a [`ChatAction::EditDraft`] through the chat's dispatcher, so
//! collaborators see each keystroke and a typed word is one undo step. The
//! message editor edits a local copy until it is saved. Enter sends and Shift+Enter breaks the line in both. What chat
//! needs beyond the editor — the plain text, the caret as an offset into it
//! for presence, a peer's caret drawn at such an offset — is read from the
//! editor's blocks here.

use std::collections::HashMap;

use automerge::AutoCommit;
use tilo_actions::ActionError;
use tilo_document::{RichText, RichTextAction, RichTextState};
use tilo_rich_text::{
    Draft, FieldOutcome, RichTextComposer, RichTextField, RichTextSource, RichTextSurface,
    SubmitKey,
};
use ui::framework::Dispatcher;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{BlockContent, BlockEditor, PresenceKeyFn, Widget};
use ui::{LocalState, PresenceCtx};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::actions::ChatAction;
use crate::doc::ChatDoc;
use crate::resource::Chat;

/// The published id of the draft stored under `key`.
pub fn draft_id(key: &str) -> String {
    format!("chat.draft:{key}")
}

/// The draft a channel or thread composer edits: the rich-text child of the
/// chat record stored under its key, edited through the chat's dispatcher.
pub struct DraftField {
    key: String,
    dispatch: Option<Dispatcher<Chat>>,
    read_only: bool,
    field: RichTextField,
}

impl DraftField {
    fn new() -> Self {
        Self {
            key: String::new(),
            dispatch: None,
            read_only: false,
            field: Self::field("", None),
        }
    }

    fn field(key: &str, dispatch: Option<&Dispatcher<Chat>>) -> RichTextField {
        let located = key.to_owned();
        let locate = move |doc: &AutoCommit| ChatDoc::rich_draft(doc, &located);
        match dispatch {
            Some(dispatch) => {
                let key = key.to_owned();
                let child = dispatch.child::<RichText>(&draft_id(&key), move |edit| {
                    ChatAction::EditDraft {
                        key: key.clone(),
                        edit,
                    }
                });
                RichTextField::new(child, locate)
            }
            None => RichTextField::view_only(locate),
        }
    }

    fn retarget(&mut self) {
        self.field = Self::field(&self.key, self.dispatch.as_ref());
    }
}

impl RichTextSource for DraftField {
    fn read_only(&self) -> bool {
        self.read_only || self.field.read_only()
    }

    fn sync(&mut self, doc: &AutoCommit) {
        self.field.sync(doc);
    }

    fn revision(&self) -> Option<u64> {
        self.field.revision()
    }

    fn read_from(&mut self, doc: &AutoCommit) -> bool {
        self.field.read_from(doc)
    }

    fn with_state<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        self.field.with_state(read)
    }

    fn apply(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        edit: &RichTextAction,
    ) -> Result<(), ActionError> {
        if self.read_only {
            return Err(ActionError::Refused(
                "this conversation is open to view only".into(),
            ));
        }
        self.field.apply(edits, edit)
    }

    // Chat keys collaborators' carets by draft itself (`composer_presence`).
    fn presence_keys(&self) -> Option<PresenceKeyFn> {
        None
    }
}

/// The one composer source. Ordinary channels use their shared published
/// draft; a direct-agent conversation uses a peer-local [`Draft`] keyed by
/// workspace and agent. Switching sources never copies plain text into the
/// shared chat document.
pub struct ComposerSource {
    shared: DraftField,
    private: LocalState<HashMap<String, Draft>>,
    private_editor_states: LocalState<HashMap<String, PrivateEditorState>>,
    private_key: Option<String>,
    read_only: bool,
    revision: u64,
    observed: Option<u64>,
}

#[derive(Clone)]
struct PrivateEditorState {
    focused_block: Option<usize>,
    focused_line: Option<String>,
    selection: Option<[usize; 4]>,
}

impl ComposerSource {
    fn new() -> Self {
        Self {
            shared: DraftField::new(),
            private: LocalState::new(HashMap::new()),
            private_editor_states: LocalState::new(HashMap::new()),
            private_key: None,
            read_only: false,
            revision: 0,
            observed: None,
        }
    }

    fn inner_revision(&self) -> Option<u64> {
        match &self.private_key {
            Some(key) => self.private.with(|drafts| {
                drafts
                    .get(key)
                    .and_then(tilo_rich_text::RichTextSource::revision)
            }),
            None => self.shared.revision(),
        }
    }

    fn observe(&mut self) {
        let current = self.inner_revision();
        if self.observed != current {
            self.observed = current;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn use_private(&mut self, key: String) {
        if self.private_key.as_ref() == Some(&key) {
            return;
        }
        self.private.update(|drafts| {
            drafts.entry(key.clone()).or_insert_with(Draft::new);
        });
        self.private_key = Some(key);
        self.observed = None;
        self.observe();
    }

    fn use_shared(&mut self) {
        if self.private_key.take().is_some() {
            self.observed = None;
            self.observe();
        }
    }

    fn private_plain(&self) -> Option<String> {
        let key = self.private_key.as_ref()?;
        self.private
            .with(|drafts| drafts.get(key).map(Draft::plain_text))
    }

    fn clear_private(&mut self) -> bool {
        let Some(key) = self.private_key.as_ref() else {
            return false;
        };
        let mut cleared = false;
        self.private.update(|drafts| {
            if let Some(draft) = drafts.get_mut(key) {
                draft.clear();
                cleared = true;
            }
        });
        if cleared {
            self.observed = self.inner_revision();
            self.revision = self.revision.wrapping_add(1);
        }
        cleared
    }
}

impl RichTextSource for ComposerSource {
    fn owns_local_history(&self) -> bool {
        self.private_key.is_some()
    }
    fn begin_local_edit(&mut self, metadata: Option<String>) {
        if let Some(key) = &self.private_key {
            self.private.update(|drafts| {
                if let Some(draft) = drafts.get_mut(key) {
                    draft.begin_local_edit(metadata);
                }
            });
        }
    }

    fn finish_local_edit(&mut self) {
        if let Some(key) = &self.private_key {
            self.private.update(|drafts| {
                if let Some(draft) = drafts.get_mut(key) {
                    draft.finish_local_edit();
                }
            });
        }
    }

    fn local_undo(
        &mut self,
        request: ui::undo::UndoRequest,
        metadata: Option<String>,
    ) -> Option<Option<String>> {
        let key = self.private_key.as_ref()?;
        let mut result = None;
        self.private.update(|drafts| {
            if let Some(draft) = drafts.get_mut(key) {
                result = draft.local_undo(request, metadata);
            }
        });
        self.observe();
        result
    }
    fn read_only(&self) -> bool {
        self.read_only || (self.private_key.is_none() && self.shared.read_only())
    }

    fn sync(&mut self, doc: &AutoCommit) {
        if self.private_key.is_none() {
            self.shared.sync(doc);
        }
        self.observe();
    }

    fn revision(&self) -> Option<u64> {
        self.inner_revision().map(|_| self.revision)
    }

    fn read_from(&mut self, doc: &AutoCommit) -> bool {
        let read = match &self.private_key {
            Some(key) => self.private.with(|drafts| {
                drafts
                    .get(key)
                    .is_some_and(|draft| draft.with_state(|_| ()).is_some())
            }),
            None => self.shared.read_from(doc),
        };
        self.observe();
        read
    }

    fn with_state<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        match &self.private_key {
            Some(key) => self
                .private
                .with(|drafts| drafts.get(key).and_then(|draft| draft.with_state(read))),
            None => self.shared.with_state(read),
        }
    }

    fn apply(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        edit: &RichTextAction,
    ) -> Result<(), ActionError> {
        if self.read_only {
            return Err(ActionError::Refused(
                "this conversation is open to view only".into(),
            ));
        }
        let result = match &self.private_key {
            Some(key) => {
                let mut result = Err(ActionError::Refused(
                    "the private draft is not attached".into(),
                ));
                self.private.update(|drafts| {
                    if let Some(draft) = drafts.get_mut(key) {
                        result = draft.apply(edits, edit);
                        draft.sync(edits);
                    }
                });
                result
            }
            None => self.shared.apply(edits, edit),
        };
        self.observe();
        result
    }

    fn presence_keys(&self) -> Option<PresenceKeyFn> {
        self.private_key
            .is_none()
            .then(|| self.shared.presence_keys())
            .flatten()
    }
}

fn agent_draft_key(workspace: &str, agent: &str) -> String {
    format!(
        "agent:{}:{workspace}:{}:{agent}",
        workspace.len(),
        agent.len()
    )
}

/// The SDK editor as a chat composer.
pub struct Composer<S: RichTextSource + 'static> {
    host: RichTextComposer<S>,
    /// What the mention colours were last built from.
    painted: Option<u64>,
}

/// A channel or thread composer over its shared draft.
pub type DraftComposer = Composer<ComposerSource>;
/// The editor for a sent message, local until saved.
pub type MessageEditor = Composer<Draft>;

fn editor() -> BlockEditor {
    tilo_rich_text::compact_editor()
}

impl DraftComposer {
    /// A composer over the draft named by its key ([`Self::set_key`]).
    pub fn shared() -> Self {
        Self {
            host: RichTextComposer::new(
                RichTextSurface::over(ComposerSource::new())
                    .with_editor(editor())
                    .with_submit(SubmitKey::Enter),
            ),
            painted: None,
        }
    }

    /// Edit through `dispatcher` from now on.
    pub fn attach(&mut self, dispatcher: Dispatcher<Chat>) {
        let source = self.host.widget_mut().source_mut();
        source.shared.dispatch = Some(dispatcher);
        source.shared.retarget();
    }

    /// Edit the draft stored under `key` (a channel, or a thread).
    pub fn set_key(&mut self, key: &str) {
        self.remember_private_editor_state();
        let source = self.host.widget_mut().source_mut();
        source.use_shared();
        if source.shared.key != key {
            key.clone_into(&mut source.shared.key);
            source.shared.retarget();
        }
    }

    /// The draft this composer edits.
    pub fn key(&self) -> String {
        let source = self.host.widget().source();
        source
            .private_key
            .clone()
            .unwrap_or_else(|| source.shared.key.clone())
    }

    /// Whether the participant may only view.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.host.widget_mut().source_mut().read_only = read_only;
    }

    /// Edit the private draft belonging to one direct-agent conversation.
    /// The key is a local routing key only; it is never written to the room.
    pub fn set_agent_draft(&mut self, workspace: &str, agent: &str) {
        let key = agent_draft_key(workspace, agent);
        let current = self.host.widget().source().private_key.clone();
        if current.as_deref() == Some(key.as_str()) {
            return;
        }
        let keep_focused = Widget::is_focused(self.host.widget());
        let state = self.private_editor_state();
        if let Some(current) = current {
            self.host
                .widget()
                .source()
                .private_editor_states
                .update(|states| {
                    states.insert(current, state);
                });
        }
        let saved = self
            .host
            .widget()
            .source()
            .private_editor_states
            .with(|states| states.get(&key).cloned());
        self.host.widget_mut().source_mut().use_private(key);
        self.host.widget_mut().present_now();
        self.restore_private_editor_state(saved.as_ref(), keep_focused);
    }

    fn private_editor_state(&self) -> PrivateEditorState {
        let surface = self.host.widget();
        let editor = surface.editor();
        PrivateEditorState {
            focused_block: editor.focused_index(),
            focused_line: editor
                .focused_index()
                .and_then(|index| editor.blocks().get(index))
                .and_then(|block| ui::undo::get_state(block.line.inner())),
            selection: editor.document_selection().map(|selection| {
                [
                    selection.anchor.block,
                    selection.anchor.byte,
                    selection.cursor.block,
                    selection.cursor.byte,
                ]
            }),
        }
    }

    fn remember_private_editor_state(&self) {
        let Some(key) = self.host.widget().source().private_key.clone() else {
            return;
        };
        let state = self.private_editor_state();
        self.host
            .widget()
            .source()
            .private_editor_states
            .update(|states| {
                states.insert(key, state);
            });
    }

    fn restore_private_editor_state(
        &mut self,
        saved: Option<&PrivateEditorState>,
        keep_focused: bool,
    ) {
        let surface = self.host.widget_mut();
        let editor = surface.editor_mut();
        editor.clear_document_selection();
        if let Some(saved) = saved {
            if let Some(index) = saved.focused_block {
                editor.focus_block(index, ui::widget::CursorPlacement::Start);
                if let Some(line) = &saved.focused_line
                    && let Some(block) = editor.blocks_mut().get_mut(index)
                {
                    ui::undo::set_state(block.line.inner_mut(), line);
                }
            }
            if let Some([anchor_block, anchor_byte, cursor_block, cursor_byte]) = saved.selection {
                editor.select_document_range(
                    ui::widget::DocumentPosition {
                        block: anchor_block,
                        byte: anchor_byte,
                    },
                    ui::widget::DocumentPosition {
                        block: cursor_block,
                        byte: cursor_byte,
                    },
                );
            }
            Widget::set_focus(surface, keep_focused);
        } else {
            editor.focus_block(usize::MAX, ui::widget::CursorPlacement::End);
            Widget::set_focus(surface, keep_focused);
        }
    }

    /// Put sample text in the attached peer-local draft for a named capture.
    /// Live input still reaches this store only through the shared editor.
    pub(crate) fn stage_private_text(&mut self, text: &str) {
        let blocks = (!text.is_empty()).then(|| vec![BlockContent::paragraph("", text)]);
        let source = self.host.widget_mut().source_mut();
        let Some(key) = source.private_key.clone() else {
            return;
        };
        source.private.update(|drafts| {
            let draft = drafts.entry(key).or_insert_with(Draft::new);
            draft.reset(blocks.as_deref().unwrap_or_default());
        });
        source.observed = None;
        source.observe();
        self.host.widget_mut().present_now();
    }

    /// Plain text currently held by a direct-agent draft, when attached.
    pub fn private_plain(&self) -> Option<String> {
        self.host.widget().source().private_plain()
    }

    #[cfg(test)]
    pub(crate) fn private_plain_for(&self, workspace: &str, agent: &str) -> Option<String> {
        let key = agent_draft_key(workspace, agent);
        self.host
            .widget()
            .source()
            .private
            .with(|drafts| drafts.get(&key).map(Draft::plain_text))
    }

    pub(crate) fn private_revision_for(&self, workspace: &str, agent: &str) -> Option<u64> {
        let key = agent_draft_key(workspace, agent);
        self.host.widget().source().private.with(|drafts| {
            drafts
                .get(&key)
                .and_then(tilo_rich_text::RichTextSource::revision)
        })
    }

    /// Clear only the currently attached direct-agent draft.
    pub fn clear_private(&mut self) -> bool {
        let cleared = self.host.widget_mut().source_mut().clear_private();
        if cleared {
            self.host.widget_mut().present_now();
        }
        cleared
    }

    /// Clear an exact private draft only when it is still the editor version
    /// whose owner-backed action succeeded. Text equality is insufficient:
    /// edit-away-and-back and formatting-only changes are both newer work.
    pub(crate) fn clear_private_for_revision(
        &mut self,
        workspace: &str,
        agent: &str,
        expected: u64,
    ) -> bool {
        let key = agent_draft_key(workspace, agent);
        let mut cleared = false;
        self.host
            .widget_mut()
            .source_mut()
            .private
            .update(|drafts| {
                let Some(draft) = drafts.get_mut(&key) else {
                    return;
                };
                if tilo_rich_text::RichTextSource::revision(draft) == Some(expected) {
                    draft.clear();
                    cleared = true;
                }
            });
        if cleared {
            self.host.widget_mut().present_now();
        }
        cleared
    }

    /// Share the peer-local draft backing store without copying editor text.
    /// The retained editor itself moves between attached and floated views;
    /// the inactive view only needs the same keyed store for later routes.
    pub(crate) fn share_private_store_from(&mut self, other: &Self) {
        let source = self.host.widget_mut().source_mut();
        source.private = other.host.widget().source().private.clone();
        source.private_editor_states = other.host.widget().source().private_editor_states.clone();
    }
}

impl MessageEditor {
    /// An editor for one sent message, empty until [`Self::load`].
    pub fn local() -> Self {
        Self {
            host: RichTextComposer::new(
                RichTextSurface::over(Draft::new())
                    .with_editor(editor())
                    .with_submit(SubmitKey::Enter),
            ),
            painted: None,
        }
    }

    /// Start editing `blocks`, the caret at the end.
    pub fn load(&mut self, blocks: &[BlockContent]) {
        let surface = self.host.widget_mut();
        surface.source_mut().reset(blocks);
        surface.present_now();
        surface
            .editor_mut()
            .focus_block(usize::MAX, ui::widget::CursorPlacement::End);
    }

    /// Put `text` in as one paragraph (a staged edit, a test).
    pub fn set_text(&mut self, text: &str) {
        let blocks = if text.is_empty() {
            Vec::new()
        } else {
            vec![BlockContent::paragraph("", text)]
        };
        self.load(&blocks);
    }
}

impl<S: RichTextSource + 'static> Composer<S> {
    /// What the editor holds, as an action carries paragraphs.
    pub fn clips(&self) -> Vec<tilo_document::ClipBlock> {
        self.blocks()
            .iter()
            .map(tilo_rich_text::clip_of_content)
            .collect()
    }

    /// The paragraphs the editor shows.
    pub fn blocks(&self) -> Vec<BlockContent> {
        self.host
            .widget()
            .editor()
            .blocks()
            .iter()
            .map(ui::widget::Block::content)
            .collect()
    }

    /// The plain text written, a line per paragraph.
    pub fn value(&self) -> String {
        crate::body::plain_text(&self.blocks())
    }

    pub fn set_placeholder(&mut self, hint: &str) {
        if self.host.widget().editor().empty_hint() != hint {
            self.host.widget_mut().editor_mut().set_empty_hint(hint);
        }
    }

    pub fn set_local_author(&mut self, author: Option<String>) {
        if self.host.widget().editor().local_author() != author.as_deref() {
            self.host.widget_mut().editor_mut().set_local_author(author);
        }
    }

    /// Paint mentions in each participant's colours, built from what
    /// `fingerprint` names; unchanged colours are not applied again.
    pub fn set_mention_paint(
        &mut self,
        (fingerprint, paint): (u64, ui::widget::marks::MentionPaint),
    ) {
        if self.painted != Some(fingerprint) {
            self.host
                .widget_mut()
                .editor_mut()
                .set_mention_paint(Some(paint));
            self.painted = Some(fingerprint);
        }
    }

    pub fn set_focus(&mut self, focused: bool) {
        if Widget::is_focused(self.host.widget()) != focused {
            Widget::set_focus(self.host.widget_mut(), focused);
        }
        let editor = self.host.widget_mut().editor_mut();
        if focused && editor.focused_index().is_none() {
            editor.focus_block(usize::MAX, ui::widget::CursorPlacement::End);
        }
    }

    pub fn is_focused(&self) -> bool {
        Widget::is_focused(self.host.widget())
    }

    pub(crate) fn handle_local_undo_request(&mut self, request: ui::undo::UndoRequest) -> bool {
        ui::undo::LocalUndo::handle_local_undo_request(self.host.widget_mut(), request)
    }

    pub(crate) fn handle_owned_local_undo_request(
        &mut self,
        request: ui::undo::UndoRequest,
    ) -> bool {
        self.host
            .widget_mut()
            .handle_owned_local_undo_request(request)
    }

    #[cfg(test)]
    pub(crate) fn is_composing(&self) -> bool {
        self.host.widget().editor().is_composing()
    }

    /// Measure the canonical paragraphs before reserving the composer band.
    pub fn measure(
        &mut self,
        width: f64,
        max_height: f32,
        theme: &Theme,
        text: &mut TextRenderer,
    ) -> f64 {
        self.host.measure(width, max_height, theme, text)
    }

    pub fn measured_height(&self) -> f64 {
        self.host.measured_height()
    }

    /// Lay the editor out in `rect`; returns its height.
    pub fn layout(&mut self, rect: Rect, theme: &Theme, text: &mut TextRenderer) -> f64 {
        self.host.layout(rect, theme, text)
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.host.draw(scene, transform, text, theme, presence);
    }

    /// The editor's floating parts (its menus, a link's popover), in the
    /// window's top layer.
    pub fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.host
            .draw_overlay(scene, transform, text, theme, presence);
    }

    /// Where the editor was last laid out.
    pub fn rect(&self) -> Rect {
        self.host.rect()
    }

    /// Give the editor one event; returns whether it took it.
    pub fn handle(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        // Show the draft as it is now first, so the edit is made against it.
        self.host.sync(doc, &automerge::ROOT);
        let mut handled = false;
        self.host.handle_event(
            event,
            scale,
            &modifiers,
            doc,
            &automerge::ROOT,
            &mut handled,
        );
        self.host.sync(doc, &automerge::ROOT);
        handled
    }

    /// Whether the person sent (Enter) or let go (Escape) since the last call.
    pub fn take_outcome(&mut self) -> Option<FieldOutcome> {
        self.host.widget_mut().take_outcome()
    }

    /// Bring the editor up to date with the document.
    pub fn sync(&mut self, doc: &AutoCommit) {
        self.host.sync(doc, &automerge::ROOT);
    }

    pub fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(self.host.widget())
    }

    pub fn mark_drawn(&mut self) {
        Widget::mark_drawn(self.host.widget_mut());
    }

    /// An accessibility action (set the value, move the selection).
    pub fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        doc: &mut ui::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> bool {
        self.host.sync(doc, path);
        let done = Widget::perform_action(self.host.widget_mut(), action, &[], doc, path);
        self.host.sync(doc, path);
        done
    }

    /// Put `text` at the caret, as typed.
    pub fn insert_text(&mut self, doc: &mut ui::widget::Edits<'_>, text: &str) {
        self.host.sync(doc, &automerge::ROOT);
        self.caret_somewhere();
        self.host.widget_mut().insert_text(doc, text);
        self.host.sync(doc, &automerge::ROOT);
    }

    /// Replace the `@query` before the caret (`query_len` bytes) with a
    /// mention of `id` shown as `label`.
    pub fn complete_mention(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        query_len: usize,
        id: &str,
        label: &str,
    ) {
        self.host.sync(doc, &automerge::ROOT);
        self.caret_somewhere();
        let surface = self.host.widget_mut();
        surface.editor_mut().select_before_caret(query_len);
        surface.insert_mention(doc, id, label);
        self.host.sync(doc, &automerge::ROOT);
    }

    /// Put the caret at the end when no paragraph holds it.
    fn caret_somewhere(&mut self) {
        let editor = self.host.widget_mut().editor_mut();
        if editor.focused_index().is_none() {
            editor.focus_block(usize::MAX, ui::widget::CursorPlacement::End);
        }
    }

    /// The focused paragraph's text before the caret.
    pub fn text_before_caret(&self) -> Option<String> {
        self.host
            .widget()
            .editor()
            .text_before_caret()
            .map(str::to_owned)
    }

    /// Where paragraph `index` starts in [`Self::value`].
    fn block_offset(blocks: &[ui::widget::Block], index: usize) -> usize {
        blocks
            .iter()
            .take(index)
            .map(|block| block.text().len().saturating_add(1))
            .sum()
    }

    /// The caret and the other end of the selection, as byte offsets into
    /// [`Self::value`].
    pub fn selection(&self) -> (usize, usize) {
        let editor = self.host.widget().editor();
        let blocks = editor.blocks();
        if let Some(selection) = editor.document_selection() {
            let at = |position: ui::widget::DocumentPosition| {
                Self::block_offset(blocks, position.block).saturating_add(position.byte)
            };
            return (at(selection.anchor), at(selection.cursor));
        }
        let Some(index) = editor.focused_index() else {
            return (0, 0);
        };
        let Some(block) = blocks.get(index) else {
            return (0, 0);
        };
        let offset = Self::block_offset(blocks, index);
        let caret = offset.saturating_add(block.line.cursor_pos());
        match block.line.selection_range() {
            Some((start, end)) if start != end => {
                let anchor = if block.line.cursor_pos() == start {
                    end
                } else {
                    start
                };
                (offset.saturating_add(anchor), caret)
            }
            _ => (caret, caret),
        }
    }

    /// The caret, as a byte offset into [`Self::value`].
    pub fn caret(&self) -> usize {
        self.selection().1
    }

    /// Select `anchor..caret` (offsets into [`Self::value`]) within one
    /// paragraph, the caret at `caret`.
    pub fn select(&mut self, anchor: usize, caret: usize) {
        let (Some((block, from)), Some((same, to))) = (self.locate(anchor), self.locate(caret))
        else {
            return;
        };
        let editor = self.host.widget_mut().editor_mut();
        editor.focus_block(block, ui::widget::CursorPlacement::Start);
        if let Some(line) = editor
            .blocks_mut()
            .get_mut(block)
            .map(|block| &mut block.line)
            && same == block
        {
            line.inner_mut().set_selection(from, to);
        }
    }

    /// Select everything written in the focused paragraph.
    pub fn select_all(&mut self) {
        let len = self.value().len();
        self.select(0, len);
    }

    /// The paragraph and byte within it of offset `at` into [`Self::value`].
    fn locate(&self, at: usize) -> Option<(usize, usize)> {
        let blocks = self.host.widget().editor().blocks();
        let mut offset = 0usize;
        for (index, block) in blocks.iter().enumerate() {
            let len = block.text().len();
            if at <= offset.saturating_add(len) {
                return Some((index, at.saturating_sub(offset)));
            }
            offset = offset.saturating_add(len).saturating_add(1);
        }
        blocks
            .len()
            .checked_sub(1)
            .map(|last| (last, blocks.get(last).map_or(0, |block| block.text().len())))
    }

    /// Where a caret at offset `at` into [`Self::value`] is drawn.
    pub fn caret_rect(&self, theme: &Theme, at: usize) -> Option<Rect> {
        let (index, byte) = self.locate(at)?;
        let block = self.host.widget().editor().blocks().get(index)?;
        Some(block.line.caret_rect(theme, byte))
    }

    /// Where a selection of offsets `start..end` into [`Self::value`] is
    /// drawn.
    pub fn selection_rects(&self, theme: &Theme, start: usize, end: usize) -> Vec<Rect> {
        let (Some((first, from)), Some((last, to))) = (self.locate(start), self.locate(end)) else {
            return Vec::new();
        };
        let blocks = self.host.widget().editor().blocks();
        (first..=last)
            .filter_map(|index| {
                let block = blocks.get(index)?;
                let begin = if index == first { from } else { 0 };
                let finish = if index == last {
                    to
                } else {
                    block.text().len()
                };
                Some(block.line.mark_rects(theme, begin..finish))
            })
            .flatten()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_agent_drafts_are_peer_local_and_keyed_by_workspace_and_agent() {
        let mut room_doc = AutoCommit::new();
        let before = room_doc.save();
        let mut composer = DraftComposer::shared();

        composer.set_agent_draft("room-a", "scribe");
        composer.insert_text(
            &mut ui::widget::Edits::lend(&mut room_doc),
            "private for Scribe",
        );
        assert_eq!(
            composer.private_plain().as_deref(),
            Some("private for Scribe")
        );
        assert_eq!(
            room_doc.save(),
            before,
            "private drafts never edit the room"
        );

        composer.set_agent_draft("room-a", "tide");
        composer.insert_text(
            &mut ui::widget::Edits::lend(&mut room_doc),
            "private for Tide",
        );
        assert_eq!(
            composer.private_plain().as_deref(),
            Some("private for Tide")
        );

        composer.set_agent_draft("room-a", "scribe");
        assert_eq!(
            composer.private_plain().as_deref(),
            Some("private for Scribe")
        );
        composer.set_agent_draft("room-b", "scribe");
        assert_eq!(composer.private_plain().as_deref(), Some(""));
        assert_eq!(room_doc.save(), before);
    }

    #[test]
    fn clearing_one_private_draft_does_not_clear_another() {
        let mut room_doc = AutoCommit::new();
        let mut composer = DraftComposer::shared();
        composer.set_agent_draft("room-a", "scribe");
        composer.insert_text(&mut ui::widget::Edits::lend(&mut room_doc), "Scribe");
        composer.set_agent_draft("room-a", "tide");
        composer.insert_text(&mut ui::widget::Edits::lend(&mut room_doc), "Tide");
        assert!(composer.clear_private());
        assert_eq!(composer.private_plain().as_deref(), Some(""));
        composer.set_agent_draft("room-a", "scribe");
        assert_eq!(composer.private_plain().as_deref(), Some("Scribe"));
    }
}
