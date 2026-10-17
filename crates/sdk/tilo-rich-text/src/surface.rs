//! The block editor taking part in rich text.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use tilo_doc::snapshot::BlockView;
use tilo_document::{ChangeCursor, RichText, RichTextAction, RichTextState};
use ui::automerge::{AutoCommit, ChangeHash};
use ui::framework::{ResourceSurface, SurfaceCx};
use ui::tilo_actions::ActionError;
use ui::tilo_resource::Doc;
use ui::widget::Edits;
use ui::widget::{Block, BlockContent, BlockEditor, Decorator, EditScope, EventCtx, Widget};
use ui::winit::event::WindowEvent;

use crate::convert::content_of;
use crate::edit::{self, Host};
use crate::source::{Hosted, RichTextField, RichTextSource};

/// The key that sends what an embedded field holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitKey {
    /// Enter sends; Shift+Enter breaks the line (a chat composer).
    Enter,
    /// Ctrl+Enter (Cmd+Enter) sends; Enter starts a paragraph (a comment).
    ModEnter,
}

/// How the person finished with an embedded field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOutcome {
    /// They sent what they wrote.
    Submit,
    /// They let it go (Escape).
    Cancel,
}

/// The toolkit's [`BlockEditor`] showing rich text and editing it through
/// the document's own actions.
///
/// Every keystroke is a [`RichTextAction`] its source applies: in a
/// framework app ([`RichTextViews::rich_text`](crate::RichTextViews::rich_text))
/// the host permission-checks, attributes and replicates it; for rich text
/// inside another resource ([`RichTextSurface::field`]) the owning
/// resource's action carries it. Either way typing a word is one undo step;
/// a collaborator's edit, an undo or a restore is presented without moving
/// the caret of the person typing; a participant who may only view can
/// read, select and copy, and their edits never reach the document; and
/// collaborators' carets are keyed to paragraphs rather than positions on
/// screen. In a framework app, History mode shows past versions in the same
/// editor, read-only.
pub struct RichTextSurface<S = Hosted> {
    editor: BlockEditor,
    source: S,
    /// Set for a field embedded in a form or a thread: the key that sends
    /// what was written, with Escape cancelling.
    submit: Option<SubmitKey>,
    outcome: Option<FieldOutcome>,
    /// What the editor was last shown, and the paragraph each entry was
    /// read from: an entry is converted again only when its paragraph
    /// changed.
    presented: Vec<BlockContent>,
    sources: Vec<Rc<BlockView>>,
    /// Where the document's change history stood when it was presented:
    /// the next presentation converts only the paragraphs changed since.
    cursor: Option<ChangeCursor>,
    revision: Option<u64>,
    keyed: bool,
}

#[cfg(test)]
thread_local! {
    /// Blocks compared with what was presented and paragraphs presented,
    /// for the test that pins a keystroke's work.
    pub(crate) static WORK: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
fn count_work(compared: usize, presented: usize) {
    WORK.with(|work| {
        let (c, p) = work.get();
        work.set((c.saturating_add(compared), p.saturating_add(presented)));
    });
}

#[cfg(not(test))]
fn count_work(_compared: usize, _presented: usize) {}

impl Default for RichTextSurface {
    fn default() -> Self {
        Self::new()
    }
}

fn default_editor() -> BlockEditor {
    let mut editor = BlockEditor::new().with_wrap_lines(true);
    editor.set_find_replace_enabled(true);
    editor
}

impl RichTextSurface {
    /// An editor over a framework app's rich-text resource.
    pub fn new() -> Self {
        Self::over(Hosted::default())
    }
}

impl RichTextSurface<RichTextField> {
    /// An editor over rich text stored in a field of another resource.
    pub fn field(field: RichTextField) -> Self {
        Self::over(field)
    }
}

impl<S: RichTextSource> RichTextSurface<S> {
    /// An editor over rich text from `source`.
    pub fn over(source: S) -> Self {
        Self {
            editor: default_editor(),
            source,
            submit: None,
            outcome: None,
            presented: Vec::new(),
            sources: Vec::new(),
            cursor: None,
            revision: None,
            keyed: false,
        }
    }

    /// Present the document in an editor configured by the caller (its
    /// empty hint, spacing, image store).
    #[must_use]
    pub fn with_editor(mut self, mut editor: BlockEditor) -> Self {
        editor.set_find_replace_enabled(true);
        self.editor = editor;
        self.revision = None;
        self.cursor = None;
        self.keyed = false;
        self
    }

    /// Make this a field embedded in a form or a thread: `key` sends what it
    /// holds and Escape cancels, both reported by [`Self::take_outcome`]
    /// instead of reaching the editor.
    #[must_use]
    pub fn with_submit(mut self, key: SubmitKey) -> Self {
        self.submit = Some(key);
        self
    }

    /// Show what the source holds now, without a document: for a source that
    /// keeps its own ([`Draft`](crate::Draft)) after it was reset.
    pub fn present_now(&mut self) {
        if self.source.revision() != self.revision {
            self.present_state(None);
        }
    }

    /// Put `text` at the caret as the person typing would (an emoji picked
    /// from a menu), through the source's edits.
    pub fn insert_text(&mut self, edits: &mut Edits<'_>, text: &str) {
        if self.editor.insert_text(text) {
            self.commit(edits);
        }
    }

    /// Put a mention of the participant `id`, shown as `label`, at the caret
    /// (replacing a selection, such as the typed query the person picked it
    /// from), through the source's edits.
    pub fn insert_mention(&mut self, edits: &mut Edits<'_>, id: &str, label: &str) {
        if self.editor.insert_mention(id, label) {
            self.commit(edits);
        }
    }

    /// Whether the person sent or cancelled since the last call.
    pub fn take_outcome(&mut self) -> Option<FieldOutcome> {
        self.outcome.take()
    }

    /// Send or cancel as if the person had, for a Send button or a click
    /// outside.
    pub fn finish(&mut self, outcome: FieldOutcome) {
        self.outcome = Some(outcome);
    }

    /// What the editor shows, as the document holds it.
    #[must_use]
    pub fn blocks(&self) -> &[BlockContent] {
        &self.presented
    }

    /// The outcome `event` asks for, in an embedded field.
    fn outcome_of(
        &self,
        event: &WindowEvent,
        modifiers: ui::winit::event::Modifiers,
    ) -> Option<FieldOutcome> {
        use ui::winit::event::{ElementState, KeyEvent};
        use ui::winit::keyboard::{Key, NamedKey};
        let key = self.submit?;
        if !self.editor.is_focused() || self.editor.has_open_popup() {
            return None;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(named),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return None;
        };
        let held = modifiers.state();
        let command = held.control_key() || held.super_key();
        match named {
            NamedKey::Escape => Some(FieldOutcome::Cancel),
            NamedKey::Enter => match key {
                SubmitKey::Enter if !held.shift_key() && !command && !held.alt_key() => {
                    Some(FieldOutcome::Submit)
                }
                SubmitKey::ModEnter if command => Some(FieldOutcome::Submit),
                _ => None,
            },
            _ => None,
        }
    }

    /// The hint an empty document shows.
    #[must_use]
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.editor.set_empty_hint(hint);
        self
    }

    /// Show the document as `doc` holds it now, without writing to it: for a
    /// view shown before its first sync (a page just opened, a past version).
    /// `false` when the source cannot read without writing, or holds no
    /// rich text there.
    pub fn present_from(&mut self, doc: &Doc) -> bool {
        if !self.source.read_from(doc) {
            return false;
        }
        self.present_state(None);
        true
    }

    /// Whether the editor's block `id` is a paragraph of the document, not
    /// the empty line an empty document shows.
    #[must_use]
    pub fn shows_stored(&self, id: &str) -> bool {
        self.presented.iter().any(|block| block.id == id)
    }

    /// The editor, for what the surface does not configure itself.
    pub fn editor(&self) -> &BlockEditor {
        &self.editor
    }

    /// The editor, mutably. What the caller changes through it is compared
    /// with the document whole on the next commit.
    pub fn editor_mut(&mut self) -> &mut BlockEditor {
        self.editor.note_edit_anywhere();
        &mut self.editor
    }

    /// Author a validated image replacement for the host to wrap together with
    /// asset storage. No text or history changes until that one action applies.
    ///
    /// # Errors
    /// Refuses read-only, composing, missing or changed selection targets.
    pub fn image_paste_action(
        &mut self,
        doc: &AutoCommit,
        target: &ui::widget::block_editor::ImagePasteTarget,
        text: String,
    ) -> Result<RichTextAction, ActionError> {
        if self.source.read_only() || self.is_composing() {
            return Err(ActionError::Refused("image paste is not editable".into()));
        }
        let placeholder = self.presented.is_empty()
            && self.editor.blocks().len() == 1
            && self.editor.blocks().first().is_some_and(|block| {
                block.text().is_empty()
                    && matches!(target, ui::widget::block_editor::ImagePasteTarget::After(id) if *id == block.id)
            });
        self.source.sync(doc);
        self.source
            .with_state(|state| {
                if placeholder && state.body().is_empty() {
                    Some(crate::image_paste::into_empty_body(text))
                } else {
                    crate::image_paste::plan(state, target, text)
                }
            })
            .flatten()
            .ok_or_else(|| {
                ActionError::Refused("this selection cannot be replaced with an image".into())
            })
    }

    fn replace_find_match(&mut self, edits: &mut Edits<'_>) -> bool {
        if self.source.read_only() || self.is_composing() {
            return false;
        }
        let Some((target, replacement)) = self.editor.find_replace_request() else {
            return false;
        };
        self.source.sync(edits.doc());
        let action = self
            .source
            .with_state(|state| {
                let current = state.block(&target.block.id)?;
                if content_of(current) != target.block
                    || state.body().position(&current.id).is_none()
                {
                    return None;
                }
                let at = crate::convert::char_of_byte(&current.text, target.start_byte);
                let end = crate::convert::char_of_byte(&current.text, target.end_byte);
                Some(RichTextAction::SpliceText {
                    region: tilo_document::action::RegionKind::Body,
                    block: current.id.clone(),
                    at,
                    del: end.saturating_sub(at),
                    ins: replacement.clone(),
                })
            })
            .flatten();
        let Some(action) = action else {
            return false;
        };
        if let Err(error) = self.source.apply(edits, &action) {
            tracing::debug!(%error, "find replacement refused");
            return false;
        }
        self.source.sync(edits.doc());
        self.present_state(None);
        self.editor.finish_find_replace(&target, replacement.len());
        true
    }

    fn replace_all_find_matches(&mut self, edits: &mut Edits<'_>) -> bool {
        if self.source.read_only() || self.is_composing() {
            return false;
        }
        let Some(batch) = self.editor.find_replace_all_request() else {
            return false;
        };
        self.source.sync(edits.doc());
        let action = self
            .source
            .with_state(|state| {
                if state.body().len() != batch.blocks.len() {
                    return None;
                }
                for (index, captured) in batch.blocks.iter().enumerate() {
                    if state.body().id_at(index)? != captured.id
                        || content_of(state.block(&captured.id)?) != *captured
                    {
                        return None;
                    }
                }
                let mut previous = None;
                let mut byte = 0;
                let mut character = 0usize;
                let spans = batch
                    .matches
                    .iter()
                    .map(|(index, range)| {
                        let block = batch.blocks.get(*index)?;
                        if previous != Some(*index) {
                            previous = Some(*index);
                            byte = 0;
                            character = 0;
                        }
                        // Search ranges are ordered. Count each source character
                        // once even when a long paragraph has many matches.
                        let gap = block.text.get(byte..range.start)?.chars().count();
                        let matched = block.text.get(range.clone())?.chars().count();
                        let start = character.saturating_add(gap);
                        character = start.saturating_add(matched);
                        byte = range.end;
                        Some(tilo_document::action::Span {
                            block: block.id.clone(),
                            start,
                            end: character,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(RichTextAction::ReplaceMatches {
                    region: tilo_document::action::RegionKind::Body,
                    spans,
                    ins: batch.replacement,
                })
            })
            .flatten();
        let Some(action) = action else {
            return false;
        };
        if let Err(error) = self.source.apply(edits, &action) {
            tracing::debug!(%error, "find replace all refused");
            return false;
        }
        self.source.sync(edits.doc());
        self.present_state(None);
        self.editor.finish_find_replace_all();
        true
    }

    fn drain_find_replace(&mut self, edits: &mut Edits<'_>) {
        if self.editor.take_find_replace_all_request() {
            self.replace_all_find_matches(edits);
        }
        if self.editor.take_find_replace_request() {
            self.replace_find_match(edits);
        }
    }

    /// Where the text comes from.
    pub fn source(&self) -> &S {
        &self.source
    }

    /// Where the text comes from, mutably (its permissions).
    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    /// The blocks the person changed since the last presentation: the
    /// presented range `[start, presented_end)` became the editor's
    /// `[start, edited_end)`. `None` when nothing changed. Only the blocks
    /// the editor says the person may have changed are compared, so typing
    /// in a paragraph costs a comparison of that paragraph and its
    /// neighbours; anything else is found by comparing from both ends.
    fn changed(&mut self) -> Option<(usize, usize, usize)> {
        let scope = self.editor.take_edit_scope();
        let blocks = self.editor.blocks();
        let same = |block: &Block, shown: &BlockContent| {
            block.id == shown.id
                && block.kind == shown.kind
                && block.text() == shown.text
                && block.line.marks() == shown.marks.as_slice()
                && block.nesting == shown.nesting
                && block.code_group == shown.code_group
        };
        match scope {
            EditScope::Nothing => return None,
            EditScope::Blocks(range) if blocks.len() == self.presented.len() => {
                let range = range.start.min(blocks.len())..range.end.min(blocks.len());
                count_work(range.len(), 0);
                let presented = &self.presented;
                let differs = |index: &usize| match (blocks.get(*index), presented.get(*index)) {
                    (Some(block), Some(shown)) => !same(block, shown),
                    _ => true,
                };
                let start = range.clone().find(differs)?;
                let end = range
                    .rev()
                    .find(differs)
                    .map_or(start, |last| last.saturating_add(1));
                return Some((start, end, end));
            }
            EditScope::Blocks(_) | EditScope::Anywhere => {}
        }
        count_work(blocks.len(), 0);
        let start = blocks
            .iter()
            .zip(&self.presented)
            .take_while(|(block, shown)| same(block, shown))
            .count();
        if start == blocks.len() && start == self.presented.len() {
            return None;
        }
        let limit = blocks.len().min(self.presented.len()).saturating_sub(start);
        let suffix = blocks
            .iter()
            .rev()
            .zip(self.presented.iter().rev())
            .take(limit)
            .take_while(|(block, shown)| same(block, shown))
            .count();
        Some((
            start,
            self.presented.len().saturating_sub(suffix),
            blocks.len().saturating_sub(suffix),
        ))
    }

    /// Show the document as it is now. `local` names editor blocks that may
    /// show an edit the document did not take (a refused one), to be shown
    /// as the document has them.
    fn present_state(&mut self, local: Option<&Range<usize>>) {
        if !self.keyed
            && let Some(keys) = self.source.presence_keys()
        {
            self.editor.set_presence_key(keys);
            self.keyed = true;
        }
        if self.present_changes(local) {
            return;
        }
        let mut previous = Previous::new(
            std::mem::take(&mut self.sources),
            std::mem::take(&mut self.presented),
        );
        let shown = self.source.with_state(|state: &RichTextState| {
            state
                .body()
                .all()
                .filter_map(|id| state.block(id))
                .map(|view| match previous.take(view) {
                    Some(content) => (content, Rc::clone(view)),
                    None => (content_of(view), Rc::clone(view)),
                })
                .unzip::<_, _, Vec<BlockContent>, Vec<Rc<BlockView>>>()
        });
        let Some((presented, sources)) = shown else {
            return;
        };
        count_work(0, presented.len());
        self.revision = self.source.revision();
        self.cursor = self.source.with_state(RichTextState::change_cursor);
        self.editor.present_blocks(&presented);
        self.presented = presented;
        self.sources = sources;
    }

    /// Present only the paragraphs changed since the last presentation, and
    /// the editor blocks `local` names: `false`, presenting nothing, when
    /// that cannot be told (the body was reordered or read again, the editor
    /// holds other blocks) and the document must be presented whole.
    fn present_changes(&mut self, local: Option<&Range<usize>>) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        if self.editor.blocks().len() != self.presented.len()
            || self.sources.len() != self.presented.len()
        {
            return false;
        }
        let presented = &mut self.presented;
        let sources = &mut self.sources;
        let found = self.source.with_state(|state: &RichTextState| {
            let changed = state.changed_since(cursor)?;
            if state.body().len() != presented.len() {
                return None;
            }
            let mut indices = changed
                .iter()
                .map(|id| state.body().position(id))
                .collect::<Option<Vec<usize>>>()?;
            indices.extend(local.cloned().unwrap_or(0..0));
            indices.sort_unstable();
            indices.dedup();
            for &index in &indices {
                let id = state.body().id_at(index)?;
                let view = state.block(id)?;
                let source = sources.get_mut(index)?;
                if !Rc::ptr_eq(source, view) {
                    *presented.get_mut(index)? = content_of(view);
                    *source = Rc::clone(view);
                }
            }
            Some((indices, state.change_cursor()))
        });
        let Some(Some((indices, cursor))) = found else {
            return false;
        };
        count_work(0, indices.len());
        let changed: Vec<(usize, &BlockContent)> = indices
            .iter()
            .filter_map(|&index| Some((index, self.presented.get(index)?)))
            .collect();
        if self.editor.present_changed(&changed).is_none() {
            return false;
        }
        self.revision = self.source.revision();
        self.cursor = Some(cursor);
        true
    }

    fn is_composing(&self) -> bool {
        self.editor.is_composing()
    }

    fn move_selected_blocks(&mut self, edits: &mut Edits<'_>, up: bool) -> bool {
        if self.source.read_only() || self.is_composing() {
            return false;
        }
        self.commit(edits);
        self.source.sync(edits.doc());
        if self.source.revision() != self.revision {
            self.present_state(None);
        }
        let Some((range, boundary)) = self.editor.selected_block_move(up) else {
            return false;
        };
        let blocks = self.editor.blocks();
        let Some(selected) = blocks.get(range) else {
            return false;
        };
        let ids: Vec<String> = selected.iter().map(|block| block.id.clone()).collect();
        let selection = self.editor.document_selection();
        let reverse = selection.is_some_and(|selection| selection.anchor > selection.cursor);
        let after = boundary
            .checked_sub(1)
            .and_then(|index| blocks.get(index))
            .map(|block| block.id.clone());
        let action = RichTextAction::MoveBlocks {
            blocks: ids.clone(),
            after,
        };
        if let Err(error) = self.source.apply(edits, &action) {
            tracing::debug!(%error, "block movement refused");
            return false;
        }
        self.source.sync(edits.doc());
        self.present_state(None);
        if selection.is_some() {
            self.editor.retain_block_selection(ids, reverse);
        }
        true
    }

    fn move_key(
        &self,
        event: &WindowEvent,
        modifiers: ui::winit::event::Modifiers,
    ) -> Option<bool> {
        use ui::winit::{
            event::ElementState,
            keyboard::{Key, NamedKey},
        };
        let held = modifiers.state();
        if !self.editor.is_focused()
            || self.editor.has_open_popup()
            || self.is_composing()
            || !held.alt_key()
            || held.control_key()
            || held.super_key()
        {
            return None;
        }
        match event {
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                match event.logical_key {
                    Key::Named(NamedKey::ArrowUp) => Some(true),
                    Key::Named(NamedKey::ArrowDown) => Some(false),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn duplicate_blocks(&mut self, edits: &mut Edits<'_>, target: Option<&str>) -> bool {
        if self.source.read_only() || self.is_composing() {
            return false;
        }
        self.commit(edits);
        self.source.sync(edits.doc());
        if self.source.revision() != self.revision {
            self.present_state(None);
        }
        let range = match target {
            Some(id) => self
                .editor
                .blocks()
                .iter()
                .position(|block| block.id == id)
                .and_then(|index| self.editor.block_group_range(index)),
            None => self.editor.selected_block_range(),
        };
        let Some(range) = range else {
            return false;
        };
        let focus = target
            .and_then(|id| self.editor.blocks().iter().position(|block| block.id == id))
            .or_else(|| self.editor.focused_index())
            .filter(|index| range.contains(index))
            .unwrap_or(range.start);
        let copied_focus = range.end.saturating_add(focus.saturating_sub(range.start));
        let Some(action) = crate::block_commands::duplicate(&self.sources, range) else {
            return false;
        };
        if let Err(error) = self.source.apply(edits, &action) {
            tracing::debug!(%error, "block duplication refused");
            return false;
        }
        self.source.sync(edits.doc());
        self.editor.clear_document_selection();
        self.present_state(None);
        self.editor
            .focus_block(copied_focus, ui::widget::CursorPlacement::End);
        true
    }

    fn is_duplicate_key(
        &self,
        event: &WindowEvent,
        modifiers: ui::winit::event::Modifiers,
    ) -> bool {
        use ui::winit::{event::ElementState, keyboard::Key};
        let held = modifiers.state();
        self.editor.is_focused()
            && !self.editor.has_open_popup()
            && !self.is_composing()
            && (held.control_key() || held.super_key())
            && !held.alt_key()
            && !held.shift_key()
            && matches!(event, WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                && matches!(&event.logical_key, Key::Character(key) if key.eq_ignore_ascii_case("d")))
    }

    /// Replace the text with `text`, a paragraph per line, as an edit: the
    /// editor's paragraphs keep their ids where there are as many, so the
    /// document sees text replaced rather than paragraphs recreated.
    fn set_text(&mut self, edits: &mut Edits<'_>, text: &str) {
        let ids: Vec<String> = self
            .editor
            .blocks()
            .iter()
            .map(|block| block.id.clone())
            .collect();
        let mut fresh = 0_usize;
        let lines: Vec<BlockContent> = text
            .split('\n')
            .enumerate()
            .map(|(index, line)| {
                let id = ids.get(index).cloned().unwrap_or_else(|| {
                    loop {
                        fresh = fresh.saturating_add(1);
                        let id = format!("set-value-{fresh}");
                        if !ids.contains(&id) {
                            break id;
                        }
                    }
                });
                BlockContent::paragraph(id, line)
            })
            .collect();
        self.editor.present_blocks(&lines);
        self.editor.note_edit_anywhere();
        self.commit(edits);
    }

    /// Apply what the person did since the last presentation, then show the
    /// document as the edit left it. A participant who may only view is
    /// shown the document again, as if their edit never happened.
    fn commit(&mut self, edits: &mut Edits<'_>) {
        let Some((start, presented_end, edited_end)) = self.changed() else {
            return;
        };
        if !self.source.read_only() && self.source.revision().is_some() {
            // The changed blocks and one on either side: a merge reaches
            // back into the block before, a split names the one after.
            let from = start.saturating_sub(1);
            let presented = self
                .presented
                .get(from..presented_end.saturating_add(1).min(self.presented.len()))
                .unwrap_or(&[]);
            let edited: Vec<BlockContent> = self
                .editor
                .blocks()
                .get(from..edited_end.saturating_add(1).min(self.editor.blocks().len()))
                .unwrap_or(&[])
                .iter()
                .map(Block::content)
                .collect();
            let mut host = Through(&mut self.source, std::marker::PhantomData);
            match edit::commit(&mut host, edits, presented, &edited) {
                Ok(renames) => rename(&mut self.editor, &renames),
                Err(error) => tracing::debug!(%error, "rich-text edit not applied"),
            }
            self.source.sync(edits.doc());
        }
        let local = (presented_end == edited_end).then_some(start..edited_end);
        self.present_state(local.as_ref());
    }
}

/// What the editor was last shown, reused for paragraphs that did not
/// change: looked up by position first (the usual case, nothing moved), by
/// id only when the order changed.
struct Previous {
    entries: Vec<Option<(Rc<BlockView>, BlockContent)>>,
    next: usize,
    by_id: Option<HashMap<String, usize>>,
}

impl Previous {
    fn new(sources: Vec<Rc<BlockView>>, presented: Vec<BlockContent>) -> Self {
        Self {
            entries: sources.into_iter().zip(presented).map(Some).collect(),
            next: 0,
            by_id: None,
        }
    }

    /// The content shown for `view`, when `view` is the paragraph it was
    /// converted from.
    fn take(&mut self, view: &Rc<BlockView>) -> Option<BlockContent> {
        let at = self.next;
        let here = self
            .entries
            .get(at)
            .and_then(Option::as_ref)
            .is_some_and(|(source, _)| source.id == view.id);
        let index = if here {
            at
        } else {
            let entries = &self.entries;
            let by_id = self.by_id.get_or_insert_with(|| {
                entries
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| Some((entry.as_ref()?.0.id.clone(), index)))
                    .collect()
            });
            *by_id.get(view.id.as_str())?
        };
        self.next = index.saturating_add(1);
        let (source, content) = self.entries.get_mut(index)?.take()?;
        Rc::ptr_eq(&source, view).then_some(content)
    }
}

/// Give editor blocks the ids the document minted for them. Two steps, so a
/// chain of renames never collides with an id still in use.
fn rename(editor: &mut BlockEditor, renames: &[(String, String)]) {
    let parked: Vec<(String, &str)> = renames
        .iter()
        .enumerate()
        .filter_map(|(index, (from, to))| {
            let park = format!("\u{0}renaming-{index}");
            editor
                .rename_block(from, &park)
                .then_some((park, to.as_str()))
        })
        .collect();
    for (park, to) in parked {
        editor.rename_block(&park, to);
    }
}

/// A source as the edit host: edits applied through it, results read back.
/// The source, applying to the document a pass lends as `Edits<'e>`.
struct Through<'a, 'e, S>(&'a mut S, std::marker::PhantomData<fn(&'e ())>);

impl<'e, S: RichTextSource> Host for Through<'_, 'e, S> {
    type Doc = Edits<'e>;

    fn apply(&mut self, edits: &mut Edits<'e>, action: &RichTextAction) -> Result<(), ActionError> {
        self.0.apply(edits, action)
    }

    fn read<T>(
        &mut self,
        edits: &mut Edits<'e>,
        read: impl FnOnce(&RichTextState) -> T,
    ) -> Option<T> {
        self.0.sync(edits.doc());
        self.0.with_state(read)
    }
}

impl ResourceSurface<RichText> for RichTextSurface {
    /// The surface reads the live state when it syncs; a new state only
    /// asks for the next sync to present it.
    fn present(&mut self, _state: &Rc<RichTextState>) {
        self.revision = None;
    }

    fn attach(&mut self, cx: SurfaceCx<RichText>) {
        self.source.attach(cx.live(), cx.dispatcher());
        self.revision = None;
        self.cursor = None;
        self.keyed = false;
    }
}

impl<S: RichTextSource + 'static> Decorator for RichTextSurface<S> {
    ui::provides!(dyn ui::undo::CommittedActions; inner = editor);

    type Inner = BlockEditor;

    fn inner(&self) -> &BlockEditor {
        &self.editor
    }

    fn inner_mut(&mut self) -> &mut BlockEditor {
        &mut self.editor
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = self.editor.build_layout(taffy, theme, nodes);
        // Synthetic block actions belong to this canonical document surface.
        ui::widget::decorator::register_node(nodes, node, self);
        node
    }

    /// An edit made outside an input event (a programmatic change of the
    /// editor, a find-and-replace asked for) is written here.
    fn settle(&mut self, edits: &mut Edits<'_>, path: &ui::automerge::ObjId) {
        // What the last keystroke left to do (formatting the markdown it
        // closed, correcting the word it ended) is written now, after the
        // keystroke itself: an undo step of its own.
        self.editor.settle_typing();
        self.commit(edits);
        self.drain_find_replace(edits);
        self.editor.settle(edits, path);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (state, path) = ui::doc::document_ref(state, path);
        self.source.sync(state);
        if self.source.revision() != self.revision {
            self.present_state(None);
        }
        self.editor.sync(state, path);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let Some(outcome) = self.outcome_of(event, *ctx.modifiers) {
            self.outcome = Some(outcome);
            *ctx.handled = true;
            return;
        }
        if self.is_duplicate_key(event, *ctx.modifiers) {
            self.duplicate_blocks(&mut ctx.edits(), None);
            *ctx.handled = true;
            return;
        }
        if let Some(up) = self.move_key(event, *ctx.modifiers) {
            self.move_selected_blocks(&mut ctx.edits(), up);
            *ctx.handled = true;
            return;
        }
        // Input that arrives before the frame settled the last keystroke
        // writes what that keystroke left to do first.
        if self.editor.settle_typing() {
            self.commit(&mut ctx.edits());
        }
        self.editor.handle_event(event, ctx);
        let mut edits = ctx.edits();
        self.commit(&mut edits);
        self.drain_find_replace(&mut edits);
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Decorator::accessibility_shared(self).map(ui::a11y::SharedAccessNode::into_owned)
    }

    /// The editor's tree, each block's row with its Duplicate button, which
    /// the editor keeps with the row while the button is the same.
    fn accessibility_shared(&self) -> Option<ui::a11y::SharedAccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let disabled = self.source.read_only() || self.is_composing();
        let duplicate_label = ui::i18n!("widget-block-duplicate");
        let duplicate = |id: &str, bounds: ui::vello::kurbo::Rect| {
            AccessNode::new(Role::Button)
                .with_name(duplicate_label.clone())
                .with_action_target(format!("notes.block-duplicate:{id}"))
                .with_state(AccessState::new().focusable(!disabled).disabled(disabled))
                .with_bounds(bounds)
        };
        let mut tree =
            self.editor
                .accessibility_shared_with(Some(&ui::widget::block_editor::RowControl {
                    key: u64::from(disabled),
                    build: &duplicate,
                }))?;
        let selection_disabled = disabled || self.editor.selected_block_range().is_none();
        let mut duplicate = AccessNode::new(Role::Button)
            .with_name(ui::i18n!("widget-block-duplicate-selection"))
            .with_action_target("notes.editor:duplicate")
            .with_state(
                AccessState::new()
                    .focusable(!selection_disabled)
                    .disabled(selection_disabled),
            );
        duplicate.bounds = tree.node.bounds;
        tree.children.push(std::sync::Arc::new(duplicate));
        for up in [true, false] {
            let disabled = disabled || self.editor.selected_block_move(up).is_none();
            let mut movement = AccessNode::new(Role::Button)
                .with_name(if up {
                    ui::i18n!("widget-block-move-selection-up")
                } else {
                    ui::i18n!("widget-block-move-selection-down")
                })
                .with_action_target(if up {
                    "notes.editor:move-up"
                } else {
                    "notes.editor:move-down"
                })
                .with_state(AccessState::new().focusable(!disabled).disabled(disabled));
            movement.bounds = tree.node.bounds;
            tree.children.push(std::sync::Arc::new(movement));
        }
        Some(tree)
    }

    /// Only adds children to the editor's node, so its root is the editor's.
    fn accessibility_root(&self) -> Option<ui::a11y::AccessNode> {
        self.editor.accessibility_root()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if target == "notes.find:replace-all" {
            if !matches!(action, ui::a11y::Action::Activate) {
                return false;
            }
            let (state, _) = ui::doc::document(state, path);
            return self.replace_all_find_matches(state);
        }
        if target == "notes.find:replace" {
            if !matches!(action, ui::a11y::Action::Activate) {
                return false;
            }
            let (state, _) = ui::doc::document(state, path);
            return self.replace_find_match(state);
        }
        if matches!(target, "notes.editor:move-up" | "notes.editor:move-down") {
            if !matches!(action, ui::a11y::Action::Activate) {
                return false;
            }
            let (state, _) = ui::doc::document(state, path);
            return self.move_selected_blocks(state, target.ends_with(":move-up"));
        }
        if target == "notes.editor:duplicate" {
            if !matches!(action, ui::a11y::Action::Activate) {
                return false;
            }
            let (state, _) = ui::doc::document(state, path);
            return self.duplicate_blocks(state, None);
        }
        if let Some(id) = target.strip_prefix("notes.block-duplicate:") {
            if !matches!(action, ui::a11y::Action::Activate) {
                return false;
            }
            let (state, _) = ui::doc::document(state, path);
            return self.duplicate_blocks(state, Some(id));
        }
        // What the action edited is written by the settle pass that follows.
        self.editor
            .perform_semantic_action(target, action, state, path)
    }

    // Every edit is an action bounded into undo steps by its source or the
    // host; the editor's own commit requests would close a word mid-typing.

    /// An assistive technology focuses the field or sets its text, as a
    /// person typing would: the new text is written through the source's
    /// edits, a paragraph per line.
    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _key_path: &[u64],
        state: &mut Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        match action {
            ui::a11y::Action::Focus => {
                Widget::set_focus(&mut self.editor, true);
                true
            }
            ui::a11y::Action::SetValue(text) => {
                if self.source.read_only() || self.source.revision().is_none() {
                    return false;
                }
                self.set_text(state, text);
                true
            }
            // What the action edited is written by the settle pass that
            // follows.
            _ => self.editor.perform_action(action, &[], state, path),
        }
    }
}

// Every edit is a dispatcher's, whose undo boundaries the app's root
// reports; the editor itself never brackets a step.
impl<S: RichTextSource + 'static> ui::undo::CommittedActions for RichTextSurface<S> {
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::automerge::ROOT;
    use ui::framework::testing::TestApp;
    use ui::framework::{ResourceApp, ViewCx};
    use ui::tilo_resource::Invoke;

    /// A document edited as a field: the rich text at its root, through a
    /// child dispatcher of the document itself.
    struct FieldApp;

    impl ResourceApp for FieldApp {
        type Resource = RichText;
        const ID: &'static str = "so.tilo.rich-text-field-work";
        const TITLE: &'static str = "Field";

        fn view(cx: &mut ViewCx<RichText>) -> ui::widget::Element {
            let body = cx.child::<RichText>("tilo.rich-text/body", |edit| edit);
            Box::new(RichTextSurface::field(RichTextField::new(body, |_| {
                Some(ROOT)
            })))
        }
    }

    /// Blocks compared and paragraphs presented while typing `typed` into
    /// the first of `paragraphs` paragraphs.
    fn typing_work(paragraphs: usize, typed: &str) -> (usize, usize) {
        ui::i18n::I18nManager::init("en-US");
        let mut app = TestApp::<FieldApp>::new().unwrap();
        let mut markdown = String::new();
        for index in 0..paragraphs {
            use std::fmt::Write as _;
            let _ = write!(markdown, "Paragraph {index} of a long note.\n\n");
        }
        app.invoke(Invoke::<RichText>::apply(&RichTextAction::ReplaceBody { markdown }).unwrap())
            .unwrap();
        let rect = app
            .harness()
            .targets()
            .get("block_editor")
            .copied()
            .unwrap();
        app.harness().click_at(rect.x0 + 40.0, rect.y0 + 10.0);
        app.harness().type_text("warm ");
        WORK.with(|work| work.set((0, 0)));
        app.harness().type_text(typed);
        WORK.with(std::cell::Cell::get)
    }

    #[test]
    fn a_keystroke_compares_and_presents_what_it_touched_not_the_document() {
        let small = typing_work(200, "typed");
        let large = typing_work(2000, "typed");
        assert!(
            small.0 > 0 && small.1 > 0,
            "the typing reached the document: {small:?}"
        );
        assert_eq!(
            small, large,
            "blocks compared and paragraphs presented per keystroke grow with the document"
        );
        assert!(
            small.0 <= 3 * 2 * "typed".len() && small.1 <= 3 * 2 * "typed".len(),
            "a keystroke compares and presents its paragraph and neighbours: {small:?}"
        );
    }
}
