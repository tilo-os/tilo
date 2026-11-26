//! Rich text being written before it is sent: a reply, a message, a comment.
//!
//! A [`Draft`] keeps its own small document, so nothing reaches the shared
//! one until the person sends it; then the owning resource's action carries
//! what was written ([`Draft::clip_blocks`]) in one step, and the draft is
//! cleared. Its text reads back as plain text for search and export.

use tilo_document::{ClipBlock, RichText, RichTextAction, RichTextState};
use ui::automerge::{AutoCommit, ROOT};
use ui::rich_edit::BlockContent;
use ui::tilo_actions::{Action, ActionError};
use ui::tilo_resource::Doc;

use crate::convert::clip_block;
use crate::source::{FieldState, RichTextSource};

/// Rich text written locally, for sending as a whole.
pub struct Draft {
    doc: AutoCommit,
    field: Option<FieldState>,
    revision: u64,
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
        edit.apply(&mut self.doc)?;
        self.doc.commit();
        Ok(())
    }
}
