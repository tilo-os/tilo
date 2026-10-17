//! The composers: the SDK rich-text editor, embedded where chat draws it.
//!
//! A channel or thread composer edits that surface's shared draft: rich text
//! in the document, the record's `drafts` field under the composer's key,
//! every edit of it a field edit at that path through the chat's dispatcher, so
//! collaborators see each keystroke and a typed word is one undo step. The
//! message editor edits a local copy until it is saved. Enter sends and Shift+Enter breaks the line in both. What chat
//! needs beyond the editor — the plain text, the caret as an offset into it
//! for presence, a peer's caret drawn at such an offset — is read from the
//! editor's blocks here.

use std::collections::HashMap;

use automerge::AutoCommit;
use tilo_actions::ActionError;
use tilo_document::{RichTextAction, RichTextState};
use tilo_rich_text::{
    Draft, FieldOutcome, PersonField, RichTextComposer, RichTextSource, RichTextSurface, SubmitKey,
};
use ui::framework::PersonRecord;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::EditOp;
use ui::widget::{BlockContent, BlockEditor, PresenceKeyFn, Widget};
use ui::{LocalState, PresenceCtx};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::drafts::ChatDrafts;

/// The draft a channel or thread composer edits: the rich text kept under
/// its key in the person's own drafts record ([`ChatDrafts`]), never in the
/// room's chat record.
pub struct DraftField {
    key: String,
    record: Option<PersonRecord<ChatDrafts>>,
    read_only: bool,
    field: Option<PersonField<ChatDrafts>>,
}

impl DraftField {
    fn new() -> Self {
        Self {
            key: String::new(),
            record: None,
            read_only: false,
            field: None,
        }
    }

    fn field(key: &str, record: &PersonRecord<ChatDrafts>) -> PersonField<ChatDrafts> {
        let located = key.to_owned();
        let edited = key.to_owned();
        let started = key.to_owned();
        PersonField::new(
            record.clone(),
            move |doc: &AutoCommit| crate::projection::draft(doc, &located),
            move |edit| ui::tilo_resource::Stamped {
                operation: crate::drafts::ChatDraftsAction::FieldEdit(
                    ChatDrafts::paths()
                        .drafts
                        .key(edited.clone())
                        .edit(EditOp::Rich {
                            rich: serde_json::to_value(edit).unwrap_or_default(),
                        }),
                ),
                now_ms: ui::tilo_resource::origin_now_ms(),
            },
        )
        .with_start(move || {
            ChatDrafts::START_DRAFT.with(crate::drafts::drafts::StartDraft {
                key: started.clone(),
            })
        })
    }

    fn retarget(&mut self) {
        self.field = self
            .record
            .as_ref()
            .filter(|_| !self.key.is_empty())
            .map(|record| Self::field(&self.key, record));
    }

    fn ensure(&mut self) {
        if let Some(field) = &mut self.field {
            // A record that refuses the start leaves the composer read-only.
            drop(field.ensure());
        }
    }

    /// Whether drafts are kept in the person's record (else in memory on
    /// this device, for a window that knows no person).
    fn kept(&self) -> bool {
        self.record.is_some()
    }
}

impl RichTextSource for DraftField {
    fn read_only(&self) -> bool {
        self.read_only || self.field.as_ref().is_none_or(RichTextSource::read_only)
    }

    fn sync(&mut self, doc: &AutoCommit) {
        if let Some(field) = &mut self.field {
            field.sync(doc);
        }
    }

    fn revision(&self) -> Option<u64> {
        self.field.as_ref().and_then(RichTextSource::revision)
    }

    fn read_from(&mut self, doc: &AutoCommit) -> bool {
        self.field
            .as_mut()
            .is_some_and(|field| field.read_from(doc))
    }

    fn with_state<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        self.field.as_ref().and_then(|field| field.with_state(read))
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
        match &mut self.field {
            Some(field) => field.apply(edits, edit),
            None => Err(ActionError::Refused("the draft is not attached".into())),
        }
    }
}

/// The source behind Chat's one retained composer. Ordinary channels point
/// at their shared document draft; a direct Agent conversation points at a
/// peer-local draft keyed by the exact workspace and Agent. Switching never
/// copies private text into the room document.
pub struct ComposerSource {
    shared: DraftField,
    private: LocalState<HashMap<String, Draft>>,
    private_selection: LocalState<HashMap<String, (usize, usize)>>,
    private_key: Option<String>,
    read_only: bool,
    revision: u64,
    observed: Option<u64>,
}

impl ComposerSource {
    fn new() -> Self {
        Self {
            shared: DraftField::new(),
            private: LocalState::new(HashMap::new()),
            private_selection: LocalState::new(HashMap::new()),
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
}

impl RichTextSource for ComposerSource {
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

/// Private routes distinguish certified people from agent/task editor routes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConversationKey {
    Agent {
        workspace: String,
        route: String,
    },
    Person {
        workspace: String,
        public_key: String,
    },
}

impl ConversationKey {
    pub(crate) fn draft_key(&self) -> String {
        let (kind, workspace, target) = match self {
            Self::Agent { workspace, route } => ("agent", workspace, route.clone()),
            Self::Person {
                workspace,
                public_key,
            } => ("person", workspace, public_key.clone()),
        };
        format!(
            "{kind}:{}:{workspace}:{}:{target}",
            workspace.len(),
            target.len()
        )
    }
}

fn agent_draft_key(workspace: &str, agent: &str) -> String {
    ConversationKey::Agent {
        workspace: workspace.into(),
        route: agent.into(),
    }
    .draft_key()
}

/// The SDK editor as a chat composer.
pub struct Composer<S: RichTextSource + 'static> {
    host: RichTextComposer<S>,
    /// What the mention colours were last built from.
    painted: Option<u64>,
    pending_selection: Option<(usize, usize)>,
}

/// A channel or thread composer over its shared draft.
pub type DraftComposer = Composer<ComposerSource>;
/// The editor for a sent message, local until saved.
pub type MessageEditor = Composer<Draft>;

fn editor() -> BlockEditor {
    tilo_rich_text::compact_editor()
}

impl DraftComposer {
    fn remember_private_selection(&mut self) {
        let Some(key) = self.host.widget().source().private_key.clone() else {
            return;
        };
        let selection = self.selection();
        self.host
            .widget_mut()
            .source_mut()
            .private_selection
            .update(|selections| _ = selections.insert(key, selection));
    }

    /// A composer over the draft named by its key ([`Self::set_key`]).
    pub fn shared() -> Self {
        Self {
            host: RichTextComposer::new(
                RichTextSurface::over(ComposerSource::new())
                    .with_editor(editor())
                    .with_submit(SubmitKey::Enter),
            ),
            painted: None,
            pending_selection: None,
        }
    }

    /// Keep drafts in the person's own `record` from now on.
    pub fn attach_drafts(&mut self, record: PersonRecord<ChatDrafts>) {
        let source = self.host.widget_mut().source_mut();
        source.shared.record = Some(record);
        source.shared.retarget();
    }

    /// Edit the draft of `key` (a channel, or a thread): the person's own,
    /// in their drafts record, or kept on this device when the window knows
    /// no person.
    pub fn set_key(&mut self, key: &str) {
        if !self.host.widget().source().shared.kept() {
            self.set_private_draft(key.to_owned());
            return;
        }
        self.remember_private_selection();
        let source = self.host.widget_mut().source_mut();
        source.use_shared();
        if source.shared.key != key {
            key.clone_into(&mut source.shared.key);
            source.shared.retarget();
        }
    }

    /// Images pasted into the composer since the last call: shared as
    /// attachments, never kept in the draft's text.
    pub fn take_pasted_images(&mut self) -> Vec<ui::clipboard::ClipboardImage> {
        self.host.widget_mut().editor_mut().take_pasted_images()
    }

    /// Make the draft shown exist before its first keystroke.
    pub fn ensure_draft(&mut self, doc: &AutoCommit) {
        let source = self.host.widget_mut().source_mut();
        if source.private_key.is_none() {
            source.shared.ensure();
        }
        self.sync(doc);
    }

    /// Empty the draft shown, once what it held was sent: an edit of the
    /// draft like any other.
    pub fn clear(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let empty = RichTextAction::ReplaceBody {
            markdown: String::new(),
        };
        let surface = self.host.widget_mut();
        // A draft that cannot be emptied keeps its text, to send again.
        drop(surface.source_mut().apply(doc, &empty));
        surface.present_now();
    }

    /// What the draft holds, as an action carries paragraphs.
    pub fn clips(&self) -> Vec<tilo_document::ClipBlock> {
        self.blocks()
            .iter()
            .map(tilo_rich_text::clip_of_content)
            .collect()
    }

    /// Whether the participant may only view.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.host.widget_mut().source_mut().read_only = read_only;
    }

    /// Edit the private draft for one authenticated direct-Agent route.
    pub fn set_agent_draft(&mut self, workspace: &str, agent: &str) {
        self.set_private_draft(agent_draft_key(workspace, agent));
    }

    pub fn set_conversation_draft(&mut self, conversation: &ConversationKey) {
        self.set_private_draft(conversation.draft_key());
    }

    /// Edit the draft kept in memory on this device under `key`.
    fn set_private_draft(&mut self, key: String) {
        if self.host.widget().source().private_key.as_ref() == Some(&key) {
            return;
        }
        self.remember_private_selection();
        let selection = self
            .host
            .widget()
            .source()
            .private_selection
            .with(|selections| selections.get(&key).copied())
            .unwrap_or_default();
        self.host.widget_mut().source_mut().use_private(key);
        self.host.widget_mut().source_mut().observe();
        self.host.widget_mut().present_now();
        self.select(selection.0, selection.1);
        self.pending_selection = Some(selection);
    }

    /// Plain text currently held by the attached private draft.
    pub fn private_plain(&self) -> Option<String> {
        self.host.widget().source().private_plain()
    }

    pub(crate) fn private_revision_for(&self, workspace: &str, agent: &str) -> Option<u64> {
        let key = agent_draft_key(workspace, agent);
        self.host.widget().source().private.with(|drafts| {
            drafts
                .get(&key)
                .and_then(tilo_rich_text::RichTextSource::revision)
        })
    }

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
            self.host.widget_mut().source_mut().observe();
            self.host.widget_mut().present_now();
        }
        cleared
    }

    pub(crate) fn set_conversation_text(&mut self, conversation: &ConversationKey, text: &str) {
        self.set_conversation_draft(conversation);
        self.host
            .widget_mut()
            .source_mut()
            .private
            .update(|drafts| {
                let draft = drafts
                    .entry(conversation.draft_key())
                    .or_insert_with(Draft::new);
                draft.reset(&[BlockContent::paragraph("", text)]);
            });
        self.host.widget_mut().source_mut().observe();
        self.host.widget_mut().present_now();
    }

    /// Adopt only into an empty private draft: never overwrite an unsent Chat edit.
    pub(crate) fn adopt_private_editor(
        &mut self,
        conversation: &ConversationKey,
        editor: &ui::private_draft::Editor,
    ) -> Result<(), String> {
        editor.validate()?;
        self.set_conversation_draft(conversation);
        if self.private_plain().is_some_and(|text| !text.is_empty()) {
            return Err("Chat already has an unsent draft for this conversation".into());
        }
        self.set_conversation_text(conversation, &editor.text);
        self.pending_selection = None;
        self.set_focus(true);
        self.select(editor.anchor, editor.caret);
        let block = self
            .host
            .widget()
            .editor()
            .focused_index()
            .ok_or("private draft editor focus unavailable")?;
        if let Some(line) = self
            .host
            .widget_mut()
            .editor_mut()
            .blocks_mut()
            .get_mut(block)
        {
            line.line
                .inner_mut()
                .restore_private_preedit(&editor.preedit, editor.preedit_cursor);
        } else {
            return Err("private draft editor unavailable".into());
        }
        self.remember_private_selection();
        Ok(())
    }

    pub(crate) fn conversation_revision(&self, conversation: &ConversationKey) -> Option<u64> {
        self.host.widget().source().private.with(|drafts| {
            drafts
                .get(&conversation.draft_key())
                .and_then(tilo_rich_text::RichTextSource::revision)
        })
    }

    pub(crate) fn clear_conversation_revision(
        &mut self,
        conversation: &ConversationKey,
        revision: u64,
    ) -> bool {
        if self.conversation_revision(conversation) != Some(revision) {
            return false;
        }
        self.clear_conversation_draft(conversation);
        true
    }

    pub(crate) fn clear_conversation_draft(&mut self, conversation: &ConversationKey) {
        let key = conversation.draft_key();
        self.host
            .widget_mut()
            .source_mut()
            .private
            .update(|drafts| {
                if let Some(draft) = drafts.get_mut(&key) {
                    draft.clear();
                }
            });
        self.host.widget_mut().source_mut().observe();
        self.host.widget_mut().present_now();
    }

    pub(crate) fn share_private_store_from(&mut self, other: &Self) {
        let private = other.host.widget().source().private.clone();
        let private_selection = other.host.widget().source().private_selection.clone();
        let source = self.host.widget_mut().source_mut();
        source.private = private;
        source.private_selection = private_selection;
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
            pending_selection: None,
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

    /// What the edit holds, as an action carries paragraphs.
    pub fn clips(&self) -> Vec<tilo_document::ClipBlock> {
        self.host.widget().source().clip_blocks()
    }
}

impl<S: RichTextSource + 'static> Composer<S> {
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
        if let Some((anchor, caret)) = self.pending_selection.take() {
            self.select(anchor, caret);
        }
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
        self.host.widget_mut().editor_mut().select_document_range(
            ui::widget::DocumentPosition { block, byte: from },
            ui::widget::DocumentPosition {
                block: same,
                byte: to,
            },
        );
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
mod private_handoff_tests {
    use super::*;

    #[test]
    fn private_adoption_retains_reverse_selection_and_display_only_preedit() {
        let key = ConversationKey::Person {
            workspace: "workspace".into(),
            public_key: "a".repeat(64),
        };
        let mut composer = DraftComposer::shared();
        let editor = ui::private_draft::Editor {
            text: "private text".into(),
            anchor: 8,
            caret: 2,
            preedit: "候補".into(),
            preedit_cursor: Some((0, 3)),
        };
        composer.adopt_private_editor(&key, &editor).unwrap();
        assert_eq!(composer.private_plain().as_deref(), Some("private text"));
        assert_eq!(composer.selection(), (8, 2));
        let block = composer.host.widget().editor().focused_index().unwrap();
        let captured = composer.host.widget().editor().blocks()[block]
            .line
            .inner()
            .private_draft_editor();
        assert_eq!(captured.preedit, "候補");
        assert_eq!(captured.preedit_cursor, Some((0, 3)));
        assert!(
            !composer.value().contains("候補"),
            "preedit is not committed draft content"
        );
        let newer = ui::private_draft::Editor {
            text: "replacement".into(),
            ..editor
        };
        assert!(composer.adopt_private_editor(&key, &newer).is_err());
        assert_eq!(composer.private_plain().as_deref(), Some("private text"));
        assert_eq!(composer.selection(), (8, 2));
    }
}
