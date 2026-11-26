//! Image replacement shares the canonical paste action and its undo boundary.
use tilo_document::action::RegionKind;
use tilo_document::{ClipBlock, RichTextAction, RichTextState};
use ui::rich_edit::block_editor::ImagePasteTarget;

use crate::convert::{char_of_byte, content_of};

fn paragraph(text: String) -> ClipBlock {
    ClipBlock {
        kind: "paragraph".into(),
        text,
        marks: Vec::new(),
        level: None,
        ordered: None,
        align: None,
        attrs: std::collections::BTreeMap::new(),
    }
}

/// A picture pasted into a body with nothing in it yet: the picture, then
/// the empty line the caret goes on.
pub(crate) fn into_empty_body(text: String) -> RichTextAction {
    RichTextAction::ReplaceClips {
        blocks: vec![paragraph(text), paragraph(String::new())],
    }
}

pub(crate) fn plan(
    state: &RichTextState,
    target: &ImagePasteTarget,
    text: String,
) -> Option<RichTextAction> {
    let ImagePasteTarget::Replace {
        blocks,
        start_byte,
        end_byte,
    } = target
    else {
        let ImagePasteTarget::After(id) = target else {
            return None;
        };
        let block = state.block(id)?;
        if !representable(block) {
            return None;
        }
        // The picture goes after the caret's line (or fills it when it is an
        // empty paragraph) and is followed by a fresh empty line, where the
        // caret goes on: what is typed next lands under the picture, and the
        // next paste goes after it, never onto it.
        // The caret's line is carried over whole, as a replaced selection's
        // edges are, so its marks and authors stay.
        let end = block.text.chars().count();
        let mut fragment = Vec::with_capacity(3);
        if !(block.text.is_empty() && block.kind == "paragraph") {
            fragment.push(slice(block, 0..end));
        }
        fragment.push(paragraph(text));
        fragment.push(paragraph(String::new()));
        return Some(RichTextAction::PasteFragment {
            region: RegionKind::Body,
            start_block: id.clone(),
            start: 0,
            end_block: id.clone(),
            end,
            blocks: fragment,
            adopt_first_style: true,
        });
    };
    let first = blocks.first()?;
    let last = blocks.last()?;
    let from = state.body().position(&first.id)?;
    // A peer may have interleaved content without changing either endpoint.
    for (offset, expected) in blocks.iter().enumerate() {
        let id = state.body().id_at(from.checked_add(offset)?)?;
        let current = state.block(id)?;
        if id != expected.id || !representable(current) || content_of(current) != *expected {
            return None;
        }
    }
    // Validate each byte endpoint independently before converting to chars.
    first.text.get(..*start_byte)?;
    last.text.get(*end_byte..)?;
    if first.id == last.id && start_byte > end_byte {
        return None;
    }
    let mut fragment = Vec::new();
    let start = char_of_byte(&first.text, *start_byte);
    let end = char_of_byte(&last.text, *end_byte);
    if *start_byte > 0 {
        fragment.push(slice(state.block(&first.id)?, 0..start));
    }
    fragment.push(paragraph(text));
    // The caret continues on the line after the picture: the rest of the
    // replaced selection's last line, or a fresh empty line.
    if *end_byte < last.text.len() {
        fragment.push(slice(
            state.block(&last.id)?,
            end..last.text.chars().count(),
        ));
    } else {
        fragment.push(paragraph(String::new()));
    }
    Some(RichTextAction::PasteFragment {
        region: RegionKind::Body,
        start_block: first.id.clone(),
        start: 0,
        end_block: last.id.clone(),
        end: last.text.chars().count(),
        blocks: fragment,
        adopt_first_style: *start_byte == 0,
    })
}

// PasteFragment cannot carry child trees or floating-point attributes. Refuse
// rather than silently downgrade them; code/table flags survive style adoption.
fn representable(view: &tilo_doc::snapshot::BlockView) -> bool {
    view.children.is_empty()
        && matches!(
            view.kind.as_str(),
            "paragraph" | "heading" | "quote" | "blockquote" | "list_item"
        )
        && !view
            .attrs
            .values()
            .any(|value| matches!(value, tilo_doc::AttrVal::Float(_)))
        && ![
            "code",
            "lang",
            "code_group",
            "table",
            "table_source",
            "row",
            "col",
        ]
        .iter()
        .any(|key| view.attrs.contains_key(*key))
}

fn slice(view: &tilo_doc::snapshot::BlockView, range: std::ops::Range<usize>) -> ClipBlock {
    let mut clip = crate::clip_block(view);
    clip.text = view
        .text
        .chars()
        .skip(range.start)
        .take(range.end.saturating_sub(range.start))
        .collect();
    clip.marks.retain_mut(|mark| {
        let start = mark.start.max(range.start);
        let end = mark.end.min(range.end);
        mark.start = start.saturating_sub(range.start);
        mark.end = end.saturating_sub(range.start);
        start < end
    });
    clip
}
