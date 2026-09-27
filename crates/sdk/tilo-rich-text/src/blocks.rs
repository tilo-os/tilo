//! Rich text as the editor's blocks: what an app that stored text in a
//! form of its own reads and writes when it moves to rich text.

use qdoc_rich_text::{RichTextAction, RichTextState};
use ui::automerge::ObjId;
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{Doc, ModelError};
use ui::widget::BlockContent;

use crate::convert::content_of;
use crate::edit::{self, Host};
use crate::source::FieldState;

/// Edits applied straight under the field, read back incrementally.
struct Under {
    field: FieldState,
}

impl Host for Under {
    fn apply(&mut self, doc: &mut Doc, action: &RichTextAction) -> Result<(), ActionError> {
        action.apply_under(doc, &self.field.root)
    }

    fn read<T>(&mut self, doc: &mut Doc, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        if self.field.refresh(doc).is_err() {
            self.field = FieldState::read(doc, self.field.root.clone(), 0).ok()?;
        }
        Some(read(&self.field.state))
    }
}

fn refused(message: impl Into<String>) -> ActionError {
    ActionError::Refused(message.into())
}

fn shown(state: &RichTextState) -> Vec<BlockContent> {
    state
        .body()
        .all()
        .filter_map(|id| state.block(id))
        .map(|view| content_of(view))
        .collect()
}

/// The body under `root` as the editor shows it, a block per paragraph:
/// for an export or a projection written in the editor's terms.
///
/// # Errors
/// Returns [`ModelError`] when `root` holds no rich text.
pub fn blocks_under(doc: &mut Doc, root: &ObjId) -> Result<Vec<BlockContent>, ModelError> {
    FieldState::read(doc, root.clone(), 0).map(|field| shown(&field.state))
}

/// Write `blocks` into the rich text under `root`, which holds no text yet
/// (as [`initialize_under`](qdoc_rich_text::RichText::initialize_under)
/// leaves it): how an app brings text it stored in a form of its own into
/// rich text, when migrating a document or seeding one. Kinds, nesting,
/// code fences and inline marks are stored as the editor stores them when a
/// person types them; block ids are the document's own.
///
/// The writes are the document's own edits applied directly, for use
/// inside the owning resource's action or a migration on load.
///
/// # Errors
/// Refuses rich text that already holds text, and returns the first edit
/// refused; the edits before it stay written.
pub fn fill_under(doc: &mut Doc, root: &ObjId, blocks: &[BlockContent]) -> Result<(), ActionError> {
    let field = FieldState::read(doc, root.clone(), 0)
        .map_err(|error| refused(format!("no rich text to fill: {error}")))?;
    let presented = shown(&field.state);
    let [first] = presented.as_slice() else {
        return Err(refused("the rich text already holds paragraphs"));
    };
    if !first.text.is_empty() {
        return Err(refused("the rich text already holds text"));
    }
    if blocks.is_empty() {
        return Ok(());
    }
    // The first block takes the empty paragraph's place; the rest are new.
    let edited: Vec<BlockContent> = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| BlockContent {
            id: if index == 0 {
                first.id.clone()
            } else {
                format!("\u{0}fill-{index}")
            },
            ..block.clone()
        })
        .collect();
    let mut host = Under { field };
    edit::commit(&mut host, doc, &presented, &edited).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qdoc_rich_text::RichText;
    use ui::automerge::transaction::Transactable;
    use ui::automerge::{AutoCommit, ObjType, ROOT};
    use ui::widget::BlockKind;
    use ui::widget::marks::{Mark, MarkSpan};

    fn field(doc: &mut AutoCommit) -> ObjId {
        let root = doc.put_object(ROOT, "notes", ObjType::Map).unwrap();
        RichText::initialize_under(doc, &root).unwrap();
        root
    }

    fn block(kind: BlockKind, text: &str) -> BlockContent {
        BlockContent {
            kind,
            ..BlockContent::paragraph("", text)
        }
    }

    #[test]
    fn blocks_are_written_as_the_editor_would_store_them() {
        let mut doc = AutoCommit::new();
        let root = field(&mut doc);
        let mut bold = block(BlockKind::Paragraph, "plain bold");
        bold.marks = vec![MarkSpan::new(6..10, Mark::Bold)];
        bold.nesting = 2;
        let mut code = block(
            BlockKind::CodeBlock {
                lang: Some("rust".into()),
            },
            "fn main() {}",
        );
        code.code_group = Some("fence".into());
        let blocks = [
            block(BlockKind::Heading(1), "Agenda"),
            bold,
            block(BlockKind::Bullet { indent: 1 }, "item"),
            block(
                BlockKind::Checklist {
                    indent: 0,
                    done: true,
                },
                "done",
            ),
            code,
            block(BlockKind::Divider, ""),
            block(BlockKind::Quote, "quoted"),
        ];
        fill_under(&mut doc, &root, &blocks).unwrap();
        let shown = blocks_under(&mut doc, &root).unwrap();
        let strip = |blocks: &[BlockContent]| -> Vec<BlockContent> {
            blocks
                .iter()
                .map(|block| BlockContent {
                    id: String::new(),
                    ..block.clone()
                })
                .collect()
        };
        assert_eq!(strip(&shown), strip(&blocks));
        assert_eq!(
            RichText::plain_text_under(&doc, &root).unwrap(),
            "Agenda\nplain bold\nitem\ndone\nfn main() {}\n\nquoted"
        );
    }

    #[test]
    fn text_already_there_is_never_overwritten() {
        let mut doc = AutoCommit::new();
        let root = field(&mut doc);
        fill_under(&mut doc, &root, &[block(BlockKind::Paragraph, "first")]).unwrap();
        assert!(fill_under(&mut doc, &root, &[block(BlockKind::Paragraph, "second")]).is_err());
        assert_eq!(RichText::plain_text_under(&doc, &root).unwrap(), "first");
    }

    #[test]
    fn nothing_to_fill_leaves_the_empty_paragraph() {
        let mut doc = AutoCommit::new();
        let root = field(&mut doc);
        fill_under(&mut doc, &root, &[]).unwrap();
        assert_eq!(blocks_under(&mut doc, &root).unwrap().len(), 1);
        let missing = doc.put_object(ROOT, "other", ObjType::Map).unwrap();
        assert!(fill_under(&mut doc, &missing, &[]).is_err());
        assert!(blocks_under(&mut doc, &missing).is_err());
    }
}
