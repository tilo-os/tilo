//! Rich text being written before it is sent: a reply, a message, a comment.
//!
//! A [`Draft`] keeps its own small document, so nothing reaches the shared
//! one until the person sends it; then the owning resource's action carries
//! what was written ([`Draft::clip_blocks`]) in one step, and the draft is
//! cleared. Its text reads back as plain text for search and export.

use tilo_document::{ClipBlock, RichText, RichTextAction, RichTextState};
use ui::automerge::{AutoCommit, ROOT};
use ui::tilo_actions::{Action, ActionError};
use ui::tilo_resource::Doc;
use ui::widget::BlockContent;

use crate::convert::clip_block;
use crate::source::{FieldState, RichTextSource};

/// Rich text written locally, for sending as a whole.
pub struct Draft {
    doc: AutoCommit,
    field: Option<FieldState>,
    revision: u64,
    history: ui::UndoLog,
    interaction: Option<(Vec<ui::automerge::ChangeHash>, Option<String>)>,
}

impl Default for Draft {
    fn default() -> Self {
        Self::new()
    }
}

impl Draft {
    /// An empty draft: one empty paragraph.
    #[must_use]
    pub fn new() -> Self {
        let mut draft = Self {
            doc: AutoCommit::new(),
            field: None,
            revision: 0,
            history: ui::UndoLog::default(),
            interaction: None,
        };
        draft.reset(&[]);
        draft
    }

    /// A draft starting from `blocks` (a comment being edited).
    #[must_use]
    pub fn with_blocks(blocks: &[BlockContent]) -> Self {
        let mut draft = Self::new();
        draft.reset(blocks);
        draft
    }

    /// Start over from `blocks`; empty clears the draft.
    pub fn reset(&mut self, blocks: &[BlockContent]) {
        let mut doc = AutoCommit::new();
        let written = RichText::initialize_under(&mut doc, &ROOT).is_ok()
            && (blocks.is_empty() || crate::fill_under(&mut doc, &ROOT, blocks).is_ok());
        if !written {
            tracing::warn!("draft could not be started");
        }
        doc.commit();
        self.revision = self.revision.wrapping_add(1);
        self.field = FieldState::read(&doc, ROOT, self.revision).ok();
        self.doc = doc;
        self.history = ui::UndoLog::new(&mut self.doc);
        self.interaction = None;
    }

    /// Clear what was written.
    pub fn clear(&mut self) {
        self.reset(&[]);
    }

    /// What was written, as the editor shows it.
    #[must_use]
    pub fn blocks(&self) -> Vec<BlockContent> {
        crate::blocks_under(&self.doc, &ROOT).unwrap_or_default()
    }

    /// What was written, as an action carries paragraphs.
    #[must_use]
    pub fn clip_blocks(&self) -> Vec<ClipBlock> {
        tilo_doc::model::open(&self.doc, &ROOT)
            .map(|containers| {
                tilo_doc::model::materialize_region(
                    &self.doc,
                    &containers,
                    tilo_doc::model::Region::Body,
                )
                .iter()
                .map(clip_block)
                .collect()
            })
            .unwrap_or_default()
    }

    /// The plain text written, a line per paragraph.
    #[must_use]
    pub fn plain_text(&self) -> String {
        RichText::plain_text_under(&self.doc, &ROOT).unwrap_or_default()
    }

    /// Whether nothing but whitespace was written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plain_text().trim().is_empty()
    }
}

impl RichTextSource for Draft {
    fn owns_local_history(&self) -> bool {
        true
    }
    fn begin_local_edit(&mut self, metadata: Option<String>) {
        if self.interaction.is_none() {
            self.interaction = Some((self.doc.get_heads(), metadata));
        }
    }

    fn finish_local_edit(&mut self) {
        if let Some((before, metadata)) = self.interaction.take() {
            self.history
                .record_committed_action(&mut self.doc, before, None, metadata);
        }
    }

    fn local_undo(
        &mut self,
        request: ui::undo::UndoRequest,
        metadata: Option<String>,
    ) -> Option<Option<String>> {
        self.finish_local_edit();
        let entry = match request {
            ui::undo::UndoRequest::Undo => {
                self.history.undo_as(&mut self.doc, None, None, metadata)
            }
            ui::undo::UndoRequest::Redo => {
                self.history.redo_as(&mut self.doc, None, None, metadata)
            }
        };
        if entry.is_some() {
            self.revision = self
                .field
                .as_ref()
                .map_or(self.revision, FieldState::revision)
                .wrapping_add(1);
            self.field = FieldState::read(&self.doc, ROOT, self.revision).ok();
        }
        Some(entry.and_then(|entry| entry.widget_metadata))
    }
    fn read_only(&self) -> bool {
        false
    }

    fn sync(&mut self, _doc: &Doc) {
        let Some(field) = self.field.as_mut() else {
            return;
        };
        // The draft's own edits are read once committed.
        self.doc.commit();
        if let Err(error) = field.refresh(&self.doc) {
            tracing::debug!(%error, "draft reread");
            self.revision = self.revision.wrapping_add(1);
            self.field = FieldState::read(&self.doc, ROOT, self.revision).ok();
        }
    }

    fn revision(&self) -> Option<u64> {
        self.field.as_ref().map(FieldState::revision)
    }

    fn with_state<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        self.field.as_ref().map(|field| read(&field.state))
    }

    fn apply(
        &mut self,
        _edits: &mut ui::widget::Edits<'_>,
        edit: &RichTextAction,
    ) -> Result<(), ActionError> {
        let before = self.doc.get_heads();
        edit.apply(&mut self.doc)?;
        self.doc.commit();
        if self.interaction.is_none() {
            self.history
                .record_committed_action(&mut self.doc, before, None, None);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FieldOutcome, RichTextSurface, SubmitKey};
    use ui::kurbo::Rect;
    use ui::widget::marks::{Mark, MarkSpan};
    use ui::widget::{BlockKind, Embedded, Widget};
    use ui::winit::event::{DeviceId, ElementState, KeyEvent, Modifiers, RawKeyEvent, WindowEvent};
    use ui::winit::keyboard::{
        Key, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey,
    };

    fn press(key: Key, text: Option<&str>) -> WindowEvent {
        WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event: KeyEvent::synthetic(
                RawKeyEvent {
                    physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                    state: ElementState::Pressed,
                },
                key.clone(),
                text.map(Into::into),
                KeyLocation::Standard,
                false,
                text.map(Into::into),
                key,
            ),
            is_synthetic: true,
        }
    }

    struct Field {
        host: Embedded<RichTextSurface<Draft>>,
        doc: AutoCommit,
    }

    impl Field {
        fn new(key: SubmitKey) -> Self {
            let mut host = Embedded::new(RichTextSurface::over(Draft::new()).with_submit(key));
            let doc = AutoCommit::new();
            host.sync(&doc, &ROOT);
            let mut text = ui::TextRenderer::new();
            host.layout(
                Rect::new(0.0, 0.0, 400.0, 40.0),
                &ui::Theme::dark(),
                &mut text,
            );
            host.widget_mut().set_focus(true);
            host.widget_mut()
                .editor_mut()
                .focus_block(0, ui::widget::CursorPlacement::Start);
            Self { host, doc }
        }

        fn key(&mut self, event: &WindowEvent, held: ModifiersState) {
            let mut handled = false;
            self.host.handle_event(
                event,
                1.0,
                &Modifiers::from(held),
                &mut ui::widget::Edits::lend(&mut self.doc),
                &ROOT,
                &mut handled,
            );
            self.host.sync(&self.doc, &ROOT);
        }

        fn type_text(&mut self, text: &str) {
            for character in text.chars() {
                let text = character.to_string();
                self.key(
                    &press(Key::Character(text.as_str().into()), Some(&text)),
                    ModifiersState::empty(),
                );
            }
        }
    }

    #[test]
    fn each_arrow_press_moves_a_caret_placed_at_the_end() {
        let mut field = Field::new(SubmitKey::ModEnter);
        let surface = field.host.widget_mut();
        surface
            .source_mut()
            .reset(&[ui::widget::BlockContent::paragraph("", "Hi")]);
        surface.present_now();
        surface
            .editor_mut()
            .focus_block(0, ui::widget::CursorPlacement::End);
        let mut text = ui::TextRenderer::new();
        field.host.layout(
            Rect::new(0.0, 0.0, 400.0, 40.0),
            &ui::Theme::dark(),
            &mut text,
        );
        let mut carets = Vec::new();
        for _ in 0..2 {
            field.key(
                &press(Key::Named(NamedKey::ArrowLeft), None),
                ModifiersState::SHIFT,
            );
            let line = &field.host.widget().editor().blocks()[0].line;
            carets.push((line.cursor_pos(), line.selection_range()));
        }
        assert_eq!(carets, [(1, Some((1, 2))), (0, Some((0, 2)))]);
    }

    #[test]
    fn an_assistive_technology_sets_the_text_as_edits() {
        let mut field = Field::new(SubmitKey::Enter);
        field.type_text("old");
        let surface = field.host.widget_mut();
        assert!(surface.perform_action(
            &ui::a11y::Action::SetValue("first\nsecond".into()),
            &[],
            &mut ui::widget::Edits::lend(&mut field.doc),
            &ROOT,
        ));
        assert_eq!(field.host.widget().source().plain_text(), "first\nsecond");
        assert!(field.host.widget_mut().perform_action(
            &ui::a11y::Action::SetValue("one".into()),
            &[],
            &mut ui::widget::Edits::lend(&mut field.doc),
            &ROOT,
        ));
        assert_eq!(field.host.widget().source().plain_text(), "one");
    }

    #[test]
    fn a_mention_and_an_emoji_go_through_the_field_as_edits() {
        let mut field = Field::new(SubmitKey::Enter);
        field.type_text("hi ");
        field.host.widget_mut().insert_mention(
            &mut ui::widget::Edits::lend(&mut field.doc),
            "u-ada",
            "@Ada",
        );
        field
            .host
            .widget_mut()
            .insert_text(&mut ui::widget::Edits::lend(&mut field.doc), "🎉");
        let draft = field.host.widget().source();
        assert_eq!(draft.plain_text(), "hi @Ada 🎉");
        let blocks = draft.blocks();
        assert_eq!(
            blocks[0].marks,
            [MarkSpan::new(3..7, Mark::Mention("u-ada".into()))]
        );
        let clip = &draft.clip_blocks()[0];
        assert!(
            clip.marks
                .iter()
                .any(|mark| mark.name == "mention" && (mark.start, mark.end) == (3, 7))
        );
    }

    #[test]
    fn an_embedded_field_sends_with_its_key_and_cancels_with_escape() {
        let mut field = Field::new(SubmitKey::ModEnter);
        field.type_text("hi");
        field.key(
            &press(Key::Named(NamedKey::Enter), None),
            ModifiersState::empty(),
        );
        assert_eq!(
            field.host.widget_mut().take_outcome(),
            None,
            "plain Enter writes"
        );
        field.type_text("yo");
        field.key(
            &press(Key::Named(NamedKey::Enter), None),
            ModifiersState::CONTROL,
        );
        assert_eq!(
            field.host.widget_mut().take_outcome(),
            Some(FieldOutcome::Submit)
        );
        assert_eq!(field.host.widget().source().plain_text(), "hi\nyo");
        field.key(
            &press(Key::Named(NamedKey::Escape), None),
            ModifiersState::empty(),
        );
        assert_eq!(
            field.host.widget_mut().take_outcome(),
            Some(FieldOutcome::Cancel)
        );

        let mut chat = Field::new(SubmitKey::Enter);
        chat.type_text("one");
        chat.key(
            &press(Key::Named(NamedKey::Enter), None),
            ModifiersState::SHIFT,
        );
        chat.type_text("two");
        assert_eq!(
            chat.host.widget_mut().take_outcome(),
            None,
            "Shift+Enter breaks the line"
        );
        chat.key(
            &press(Key::Named(NamedKey::Enter), None),
            ModifiersState::empty(),
        );
        assert_eq!(
            chat.host.widget_mut().take_outcome(),
            Some(FieldOutcome::Submit)
        );
        assert_eq!(
            chat.host.widget().source().blocks().len(),
            1,
            "one paragraph"
        );
        assert_eq!(chat.host.widget().source().plain_text(), "one\ntwo");
        chat.host.widget_mut().source_mut().clear();
        chat.host.sync(&chat.doc, &ROOT);
        assert!(chat.host.widget().source().is_empty());
        assert_eq!(chat.host.widget().editor().blocks()[0].text(), "");
    }

    #[test]
    fn a_draft_reads_back_as_blocks_clips_and_plain_text() {
        let mut bold = BlockContent::paragraph("", "plain bold");
        bold.marks = vec![MarkSpan::new(6..10, Mark::Bold)];
        let done = BlockContent {
            kind: BlockKind::Checklist {
                indent: 0,
                done: true,
            },
            ..BlockContent::paragraph("", "shipped")
        };
        let draft = Draft::with_blocks(&[bold, done]);
        assert!(!draft.is_empty());
        assert_eq!(draft.plain_text(), "plain bold\nshipped");
        let clips = draft.clip_blocks();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].marks.len(), 1);
        assert_eq!(clips[0].marks[0].name, "strong");
        assert_eq!(clips[1].kind, "list_item");
        assert_eq!(
            clips[1].attrs.get("checked"),
            Some(&tilo_document::ClipAttr::Bool(true))
        );
        let mut draft = draft;
        draft.clear();
        assert!(draft.is_empty());
        assert_eq!(draft.blocks().len(), 1);
    }
}
