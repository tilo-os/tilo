//! Reading pages stored in the first format, before note bodies were rich
//! text: a page's `blocks` List of `{id, kind, text, nesting, code_group,
//! notes_order}` maps, formatted by native `notes.inline.*` text marks.
//! [`NotesDoc::upgrade`](crate::doc::NotesDoc::upgrade) is the only reader.

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use ui::widget::marks::{Mark, MarkSpan};
use ui::widget::{BlockContent, BlockKind};

use crate::model::PageId;

const MARK_PREFIX: &str = "notes.inline.";

/// A mark on the first format's text, as it was stored.
struct NativeMark {
    name: String,
    value: ScalarValue,
    start: usize,
    end: usize,
}

/// The byte offset of character `character` in `text`.
fn byte_of(text: &str, character: usize) -> Option<usize> {
    if character == text.chars().count() {
        return Some(text.len());
    }
    text.char_indices().nth(character).map(|(byte, _)| byte)
}

fn page_value(mark: &NativeMark, prefix: &str, name: &str) -> Option<PageId> {
    let page = match &mark.value {
        ScalarValue::Str(value) => value.to_string(),
        _ => name.strip_prefix(prefix)?.to_owned(),
    };
    page.parse().ok()
}

/// A Notes mark on the first format's text, as the editor's mark.
fn editor_mark(text: &str, mark: &NativeMark) -> Option<MarkSpan> {
    let name = mark.name.strip_prefix(MARK_PREFIX)?;
    let editor = match name {
        "bold" => Mark::Bold,
        "italic" => Mark::Italic,
        "code" => Mark::Code,
        "underline" => Mark::Underline,
        "strikethrough" => Mark::Strikethrough,
        link if link.starts_with("link:") => Mark::Link(page_value(mark, "link:", link)?),
        embed if embed.starts_with("transclusion:") => {
            Mark::Transclusion(page_value(mark, "transclusion:", embed)?)
        }
        "external-link" => match &mark.value {
            ScalarValue::Str(url) => Mark::ExternalLink(url.to_string()),
            _ => return None,
        },
        _ => return None,
    };
    let (start, end) = (byte_of(text, mark.start)?, byte_of(text, mark.end)?);
    (start < end).then(|| MarkSpan::new(start..end, editor))
}

/// Who wrote a run of the first format's text: its `author` mark, over bytes.
fn author_run(text: &str, mark: &NativeMark) -> Option<(std::ops::Range<usize>, String)> {
    if mark.name != tilo_doc::schema::MARK_AUTHOR {
        return None;
    }
    let ScalarValue::Str(author) = &mark.value else {
        return None;
    };
    let (start, end) = (byte_of(text, mark.start)?, byte_of(text, mark.end)?);
    (start < end && !author.is_empty()).then(|| (start..end, author.to_string()))
}

fn string(doc: &AutoCommit, block: &ObjId, key: &str) -> Option<String> {
    match doc.get(block, key) {
        Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

/// The page's blocks in the order they were shown. A kind this version does
/// not know keeps its text as a paragraph, and a block without a text body
/// comes across empty, so nothing written in the first format is lost.
pub(crate) fn blocks(doc: &AutoCommit, list: &ObjId) -> Vec<BlockContent> {
    crate::block_order::objects(doc, list)
        .into_iter()
        .map(|block| {
            let text_obj = match doc.get(&block, "text") {
                Ok(Some((Value::Object(ObjType::Text), text))) => Some(text),
                _ => None,
            };
            let text = text_obj
                .as_ref()
                .and_then(|text| doc.text(text).ok())
                .unwrap_or_default();
            let native: Vec<NativeMark> = text_obj
                .as_ref()
                .and_then(|text| doc.marks(text).ok())
                .unwrap_or_default()
                .into_iter()
                .map(|mark| NativeMark {
                    name: mark.name.to_string(),
                    value: mark.value.clone(),
                    start: mark.start,
                    end: mark.end,
                })
                .collect();
            let mut marks: Vec<MarkSpan> = native
                .iter()
                .filter_map(|mark| editor_mark(&text, mark))
                .collect();
            let authors = native
                .iter()
                .filter_map(|mark| author_run(&text, mark))
                .collect();
            marks.sort_by_key(|span| (span.range.start, span.range.end));
            let kind = string(doc, &block, "kind")
                .and_then(|kind| BlockKind::from_stored(&kind))
                .unwrap_or(BlockKind::Paragraph);
            let nesting = doc
                .get(&block, "nesting")
                .ok()
                .flatten()
                .and_then(|(value, _)| value.as_u64())
                .map_or(0, |depth| u8::try_from(depth.min(5)).unwrap_or(0));
            BlockContent {
                kind,
                marks,
                nesting,
                code_group: string(doc, &block, "code_group"),
                authors,
                ..BlockContent::paragraph("", text)
            }
        })
        .collect()
}
