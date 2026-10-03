//! The composers: the SDK rich-text editor, embedded where chat draws it.
//!
//! A channel or thread composer edits that surface's shared draft (rich text
//! in the document, written directly so collaborators see each keystroke and
//! nothing lands in history); the message editor edits a local copy until it
//! is saved. Enter sends and Shift+Enter breaks the line in both. What chat
//! needs beyond the editor — the plain text, the caret as an offset into it
//! for presence, a peer's caret drawn at such an offset — is read from the
//! editor's blocks here.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::AutoCommit;
use tilo_rich_text::{
    DirectField, Draft, FieldOutcome, RichTextSource, RichTextSurface, SubmitKey,
};
use ui::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{BlockContent, BlockEditor, Embedded, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::doc::ChatDoc;

/// The SDK editor as a chat composer.
pub struct Composer<S: RichTextSource + 'static> {
    host: Embedded<RichTextSurface<S>>,
    /// What the mention colours were last built from.
    painted: Option<u64>,
    /// The draft a shared composer edits.
    key: Option<Rc<RefCell<String>>>,
}

/// A channel or thread composer over its shared draft.
pub type DraftComposer = Composer<DirectField>;
/// The editor for a sent message, local until saved.
pub type MessageEditor = Composer<Draft>;

fn editor() -> BlockEditor {
    tilo_rich_text::compact_editor()
}

impl DraftComposer {
    /// A composer over the draft named by its key ([`Self::set_key`]).
    pub fn shared() -> Self {
        let key = Rc::new(RefCell::new(String::new()));
        let located = Rc::clone(&key);
        let source = DirectField::new(move |doc| ChatDoc::rich_draft(doc, &located.borrow()));
        Self {
            host: Embedded::new(
                RichTextSurface::over(source)
                    .with_editor(editor())
                    .with_submit(SubmitKey::Enter),
            ),
            key: Some(key),
            painted: None,
        }
    }

    /// Edit the draft stored under `key` (a channel, or a thread).
    pub fn set_key(&mut self, key: &str) {
        if let Some(cell) = &self.key
            && *cell.borrow() != key
        {
            key.clone_into(&mut cell.borrow_mut());
        }
    }

    /// The draft this composer edits.
    pub fn key(&self) -> String {
        self.key
            .as_ref()
            .map(|cell| cell.borrow().clone())
            .unwrap_or_default()
    }

    /// Whether the participant may only view.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.host.widget_mut().source_mut().set_read_only(read_only);
    }
}

impl MessageEditor {
    /// An editor for one sent message, empty until [`Self::load`].
    pub fn local() -> Self {
        Self {
            host: Embedded::new(
                RichTextSurface::over(Draft::new())
                    .with_editor(editor())
                    .with_submit(SubmitKey::Enter),
            ),
            key: None,
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
        doc: &mut AutoCommit,
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
    pub fn sync(&mut self, doc: &mut AutoCommit) {
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
        doc: &mut AutoCommit,
        path: &automerge::ObjId,
    ) -> bool {
        self.host.sync(doc, path);
        let done = Widget::perform_action(self.host.widget_mut(), action, doc, path);
        self.host.sync(doc, path);
        done
    }

    /// Put `text` at the caret, as typed.
    pub fn insert_text(&mut self, doc: &mut AutoCommit, text: &str) {
        self.host.sync(doc, &automerge::ROOT);
        self.caret_somewhere();
        self.host.widget_mut().insert_text(doc, text);
        self.host.sync(doc, &automerge::ROOT);
    }

    /// Replace the `@query` before the caret (`query_len` bytes) with a
    /// mention of `id` shown as `label`.
    pub fn complete_mention(
        &mut self,
        doc: &mut AutoCommit,
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
