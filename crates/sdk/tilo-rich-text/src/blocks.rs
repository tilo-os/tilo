//! Rich text as the editor's blocks: what an app that stored text in a
//! form of its own reads and writes when it moves to rich text.

use qdoc_rich_text::RichTextState;
use qdoc_rich_text::action::{AttrVal, MarkVal};
use ui::automerge::{ObjId, ScalarValue};
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{Doc, ModelError};
use ui::widget::BlockContent;

use crate::convert::{KindSpec, char_of_byte, content_of, document_mark};
use crate::source::FieldState;

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

fn attr_scalar(value: AttrVal) -> ScalarValue {
    match value {
        AttrVal::Bool(value) => ScalarValue::Boolean(value),
        AttrVal::Int(value) => ScalarValue::Int(value),
        AttrVal::Float(value) => ScalarValue::F64(value),
        AttrVal::Str(value) => ScalarValue::Str(value.into()),
    }
}

fn mark_scalar(value: MarkVal) -> ScalarValue {
    match value {
        MarkVal::Bool(value) => ScalarValue::Boolean(value),
        MarkVal::Int(value) => ScalarValue::Int(value),
        MarkVal::Str(value) => ScalarValue::Str(value.into()),
    }
}

/// The body under `root` as the editor shows it, a block per paragraph:
/// for an export or a projection written in the editor's terms.
///
/// # Errors
/// Returns [`ModelError`] when `root` holds no rich text.
pub fn blocks_under(doc: &Doc, root: &ObjId) -> Result<Vec<BlockContent>, ModelError> {
    let containers =
        qdoc::model::open(doc, root).map_err(|error| ModelError::invalid(error.to_string()))?;
    Ok(
        qdoc::model::materialize_region(doc, &containers, qdoc::model::Region::Body)
            .iter()
            .map(content_of)
            .collect(),
    )
}

/// Write `blocks` into the rich text under `root`, which holds no text yet
/// (as [`initialize_under`](qdoc_rich_text::RichText::initialize_under)
/// leaves it): how an app brings text it stored in a form of its own into
/// rich text, when migrating a document or seeding one. Kinds, nesting,
/// code fences and inline marks are stored as the editor stores them when a
/// person types them; block ids are the document's own.
///
/// Like an import, the blocks are written as they are: a mark the editor's
/// own edits would not author (a link to a plain `http:` address written
/// before) is kept, never dropped. For use inside the owning resource's
/// action or a migration on load.
///
/// # Errors
/// Refuses rich text that already holds text.
pub fn fill_under(doc: &mut Doc, root: &ObjId, blocks: &[BlockContent]) -> Result<(), ActionError> {
    let field = FieldState::read_only(doc, root.clone(), 0)
        .map_err(|error| refused(format!("no rich text to fill: {error}")))?;
    let presented = shown(&field.state);
    let [empty] = presented.as_slice() else {
        return Err(refused("the rich text already holds paragraphs"));
    };
    if !empty.text.is_empty() {
        return Err(refused("the rich text already holds text"));
    }
    if blocks.is_empty() {
        return Ok(());
    }
    let containers = qdoc::model::open(doc, root)
        .map_err(|error| refused(format!("no rich text to fill: {error}")))?;
    // Ids unique to this field and this content at this version of the
    // document: the same on a replica replaying the write, and clear of every
    // id elsewhere in the document.
    let mut seed = root.to_string().into_bytes();
    for block in blocks {
        seed.extend_from_slice(block.text.as_bytes());
        seed.push(0);
    }
    let cx = ui::tilo_resource::ActionCx::for_apply(doc, &seed, 0);
    let mut ids = qdoc::model::IdGen::for_action(cx);
    let mut dm = qdoc::model::DocMut {
        doc,
        c: &containers,
        ids: &mut ids,
    };
    let mut previous: Option<String> = None;
    for block in blocks {
        let spec = KindSpec::of(block);
        let id = dm.insert_block(previous.as_deref(), spec.kind);
        if let Some(level) = spec.level {
            dm.set_attr(&id, "level", level);
        }
        if let Some(ordered) = spec.ordered {
            dm.set_attr(&id, "ordered", ordered);
        }
        for (key, value) in spec.attrs {
            if let Some(value) = value {
                dm.set_attr(&id, key, attr_scalar(value));
            }
        }
        dm.splice_text(&id, 0, 0, &block.text);
        for span in &block.marks {
            let (name, value) = document_mark(&span.mark);
            let start = char_of_byte(&block.text, span.range.start);
            let end = char_of_byte(&block.text, span.range.end);
            if start < end {
                dm.add_mark(&id, start, end, name, mark_scalar(value));
            }
        }
        for (range, author) in &block.authors {
            let start = char_of_byte(&block.text, range.start);
            let end = char_of_byte(&block.text, range.end);
            if start < end && !author.is_empty() {
                dm.add_mark(&id, start, end, qdoc::schema::MARK_AUTHOR, author.as_str());
            }
        }
        previous = Some(id);
    }
    // The paragraph the new blocks were written after the start of.
    dm.remove_block(&empty.id);
    Ok(())
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
            block(BlockKind::Table { visual: true }, "| a | b |\n| - | - |"),
            block(BlockKind::Table { visual: false }, "| c |"),
            block(BlockKind::Paragraph, "soft\nbreak"),
            {
                // A link the editor would not author now, kept as written.
                let mut old = block(BlockKind::Paragraph, "old site");
                old.marks = vec![MarkSpan::new(
                    0..3,
                    Mark::ExternalLink("http://example.test".into()),
                )];
                old
            },
            {
                // Who wrote what comes across with the text.
                let mut authored = block(BlockKind::Paragraph, "éλ and more");
                authored.authors = vec![(0..4, "ada".into()), (4..13, "grace".into())];
                authored
            },
        ];
        fill_under(&mut doc, &root, &blocks).unwrap();
        let shown = blocks_under(&doc, &root).unwrap();
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
            "Agenda\nplain bold\nitem\ndone\nfn main() {}\n\nquoted\n| a | b |\n| - | - |\n| c |\nsoft\nbreak\nold site\néλ and more"
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
        assert_eq!(blocks_under(&doc, &root).unwrap().len(), 1);
        let missing = doc.put_object(ROOT, "other", ObjType::Map).unwrap();
        assert!(fill_under(&mut doc, &missing, &[]).is_err());
        assert!(blocks_under(&doc, &missing).is_err());
    }
}
