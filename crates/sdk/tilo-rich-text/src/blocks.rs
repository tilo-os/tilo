//! Rich text as the editor's blocks: what an app that stored text in a
//! form of its own reads and writes when it moves to rich text.

use tilo_document::RichTextState;
use tilo_document::action::{AttrVal, MarkVal};
use ui::automerge::{ObjId, ScalarValue};
use ui::rich_edit::BlockContent;
use ui::tilo_actions::ActionError;
use ui::tilo_resource::{Doc, ModelError};

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
pub fn blocks_under(doc: &Doc, root: &ObjId) -> Result<Vec<BlockContent>, ModelError> {
    let containers =
        tilo_doc::model::open(doc, root).map_err(|error| ModelError::invalid(error.to_string()))?;
    Ok(
        tilo_doc::model::materialize_region(doc, &containers, tilo_doc::model::Region::Body)
            .iter()
            .map(content_of)
            .collect(),
    )
}

/// Write `blocks` into the rich text under `root`, which holds no text yet
/// (as [`initialize_under`](tilo_document::RichText::initialize_under)
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
    let texts: Vec<&str> = blocks.iter().map(|block| block.text.as_str()).collect();
    let Some((containers, empty, mut ids)) = prepare_fill(doc, root, &texts)? else {
        return Ok(());
    };
    let mut dm = tilo_doc::model::DocMut {
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
                dm.add_mark(
                    &id,
                    start,
                    end,
                    tilo_doc::schema::MARK_AUTHOR,
                    author.as_str(),
                );
            }
        }
        previous = Some(id);
    }
    // The paragraph the new blocks were written after the start of.
    dm.remove_block(&empty);
    Ok(())
}

/// What a fill writes with: the field's containers, the id of the empty
/// paragraph the new blocks replace, and ids unique to this field and this
/// content at this version of the document (the same on a replica replaying
/// the write, clear of every id elsewhere). `None` when there is nothing to
/// write.
fn prepare_fill(
    doc: &Doc,
    root: &ObjId,
    texts: &[&str],
) -> Result<Option<(tilo_doc::model::Containers, String, tilo_doc::model::IdGen)>, ActionError> {
    let field = FieldState::read_only(doc, root.clone(), 0)
        .map_err(|error| refused(format!("no rich text to fill: {error}")))?;
    let presented = shown(&field.state);
    let [empty] = presented.as_slice() else {
        return Err(refused("the rich text already holds paragraphs"));
    };
    if !empty.text.is_empty() {
        return Err(refused("the rich text already holds text"));
    }
    if texts.is_empty() {
        return Ok(None);
    }
    let containers = tilo_doc::model::open(doc, root)
        .map_err(|error| refused(format!("no rich text to fill: {error}")))?;
    let mut seed = root.to_string().into_bytes();
    for text in texts {
        seed.extend_from_slice(text.as_bytes());
        seed.push(0);
    }
    let cx = ui::tilo_resource::ActionCx::for_write(doc, &seed);
    Ok(Some((
        containers,
        empty.id.clone(),
        tilo_doc::model::IdGen::for_action(cx),
    )))
}

/// Write `clips` (paragraphs as an action carries them: kind, text,
/// attributes and marks) into the rich text under `root`, which holds no
/// text yet: how an action stores a comment or a message written in a
/// [`Draft`](crate::Draft).
///
/// # Errors
/// Refuses rich text that already holds text.
pub fn fill_clips_under(
    doc: &mut Doc,
    root: &ObjId,
    clips: &[tilo_document::ClipBlock],
) -> Result<(), ActionError> {
    let texts: Vec<&str> = clips.iter().map(|clip| clip.text.as_str()).collect();
    let Some((containers, empty, mut ids)) = prepare_fill(doc, root, &texts)? else {
        return Ok(());
    };
    let mut dm = tilo_doc::model::DocMut {
        doc,
        c: &containers,
        ids: &mut ids,
    };
    let mut previous: Option<String> = None;
    for clip in clips {
        let id = dm.insert_block(previous.as_deref(), &clip.kind);
        if let Some(level) = clip.level {
            dm.set_attr(&id, "level", level);
        }
        if let Some(ordered) = clip.ordered {
            dm.set_attr(&id, "ordered", ordered);
        }
        if let Some(align) = &clip.align {
            dm.set_attr(&id, "align", align.as_str());
        }
        for (key, value) in &clip.attrs {
            let value = match value {
                tilo_document::ClipAttr::Bool(value) => ScalarValue::Boolean(*value),
                tilo_document::ClipAttr::Int(value) => ScalarValue::Int(*value),
                tilo_document::ClipAttr::Str(value) => ScalarValue::Str(value.as_str().into()),
            };
            dm.set_attr(&id, key, value);
        }
        dm.splice_text(&id, 0, 0, &clip.text);
        for mark in &clip.marks {
            if mark.start < mark.end {
                let value = match &mark.value {
                    tilo_doc::document::MarkValue::Bool(value) => ScalarValue::Boolean(*value),
                    tilo_doc::document::MarkValue::Int(value) => ScalarValue::Int(*value),
                    tilo_doc::document::MarkValue::Str(value) => {
                        ScalarValue::Str(value.as_str().into())
                    }
                };
                dm.add_mark(&id, mark.start, mark.end, &mark.name, value);
            }
        }
        previous = Some(id);
    }
    dm.remove_block(&empty);
    Ok(())
}

/// Put fresh rich text holding `clips` at `key` of the map `parent`,
/// replacing what was there: a record's rich-text field (a comment body, a
/// note) written whole by the record's action.
pub fn put_rich_text(
    doc: &mut Doc,
    parent: &ObjId,
    key: &str,
    clips: &[tilo_document::ClipBlock],
) -> Result<ObjId, ActionError> {
    use ui::automerge::ObjType;
    use ui::automerge::transaction::Transactable;
    let root = doc
        .put_object(parent, key, ObjType::Map)
        .map_err(|error| refused(error.to_string()))?;
    tilo_document::RichText::initialize_under(doc, &root)
        .map_err(|error| refused(error.to_string()))?;
    fill_clips_under(doc, &root, clips)?;
    Ok(root)
}

/// The rich text at `key` of the map `parent`, when there is some.
#[must_use]
pub fn rich_text_at(doc: &Doc, parent: &ObjId, key: &str) -> Option<ObjId> {
    use ui::automerge::{ObjType, ReadDoc, Value};
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::Map), root))) => {
            tilo_doc::model::open(doc, &root).is_ok().then_some(root)
        }
        _ => None,
    }
}

/// Plain text as the paragraphs it reads as, a paragraph per line: how text
/// stored as a plain string comes across to rich text. Empty text is no
/// paragraphs.
#[must_use]
pub fn plain_blocks(text: &str) -> Vec<BlockContent> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n')
        .map(|line| BlockContent::paragraph("", line))
        .collect()
}

/// Plain text as the paragraphs an action carries, a paragraph per line.
#[must_use]
pub fn plain_clips(text: &str) -> Vec<tilo_document::ClipBlock> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n')
        .map(|line| tilo_document::ClipBlock {
            kind: "paragraph".to_owned(),
            text: line.to_owned(),
            marks: Vec::new(),
            level: None,
            ordered: None,
            align: None,
            attrs: std::collections::BTreeMap::new(),
        })
        .collect()
}
