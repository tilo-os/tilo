//! Image replacement shares the canonical paste action and its undo boundary.
use tilo_document::action::RegionKind;
use tilo_document::{ClipBlock, RichTextAction, RichTextState};
use ui::widget::block_editor::ImagePasteTarget;

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
        if block.text.is_empty() && block.kind == "paragraph" && block.children.is_empty() {
            return Some(RichTextAction::SpliceText {
                region: RegionKind::Body,
                block: id.clone(),
                at: 0,
                del: 0,
                ins: text,
            });
        }
        return Some(RichTextAction::InsertParagraph {
            after: id.clone(),
            text,
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
    if *end_byte < last.text.len() {
        fragment.push(slice(
            state.block(&last.id)?,
            end..last.text.chars().count(),
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_document::{
        RichText,
        action::{MarkVal, Span},
    };
    use ui::automerge::{AutoCommit, ROOT};

    fn state(doc: &AutoCommit) -> RichTextState {
        crate::source::FieldState::read_only(doc, ROOT, 0)
            .unwrap()
            .state
    }

    fn fixture() -> AutoCommit {
        let mut doc = AutoCommit::new();
        RichText::initialize_under(&mut doc, &ROOT).unwrap();
        RichTextAction::ReplaceBody {
            markdown: "Ä😀\n\n日本\n\nAfter".into(),
        }
        .apply_under(&mut doc, &ROOT)
        .unwrap();
        doc
    }

    fn selection(state: &RichTextState) -> ImagePasteTarget {
        ImagePasteTarget::Replace {
            blocks: state.blocks(0..2).map(|block| content_of(block)).collect(),
            start_byte: "Ä".len(),
            end_byte: "日".len(),
        }
    }

    #[test]
    fn partial_paste_keeps_canonical_links_unknown_marks_and_authors() {
        let mut doc = fixture();
        let before = state(&doc);
        let spans = before
            .blocks(0..2)
            .map(|block| Span {
                block: block.id.clone(),
                start: 0,
                end: 2,
            })
            .collect::<Vec<_>>();
        for (name, value) in [
            ("link", MarkVal::Str("https://example.com".into())),
            ("custom_annotation", MarkVal::Str("keep".into())),
            ("author", MarkVal::Str("peer".into())),
        ] {
            RichTextAction::SetMark {
                region: RegionKind::Body,
                spans: spans.clone(),
                name: name.into(),
                value: Some(value),
                also_clear: vec![],
            }
            .apply_under(&mut doc, &ROOT)
            .unwrap();
        }
        let before = state(&doc);
        let edit = plan(
            &before,
            &selection(&before),
            "![Image](asset:synthetic)".into(),
        )
        .unwrap();
        edit.apply_under(&mut doc, &ROOT).unwrap();
        let after = state(&doc);
        let blocks = after.blocks(0..4).collect::<Vec<_>>();
        assert_eq!(
            blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(),
            ["Ä", "![Image](asset:synthetic)", "本", "After"]
        );
        for block in [blocks[0], blocks[2]] {
            for name in ["link", "custom_annotation", "author"] {
                assert!(
                    block
                        .marks
                        .iter()
                        .any(|mark| mark.name == name && mark.start == 0 && mark.end == 1),
                    "missing {name}: {:?}",
                    block.marks
                );
            }
        }
        assert_eq!(blocks[3], before.blocks(2..3).next().unwrap());
    }

    #[test]
    fn changed_or_interleaved_targets_refuse_without_mutation() {
        for interleave in [false, true] {
            let mut doc = fixture();
            let before = state(&doc);
            let target = selection(&before);
            let first = before.blocks(0..1).next().unwrap().id.clone();
            let edit = if interleave {
                RichTextAction::InsertParagraph {
                    after: first,
                    text: "Peer".into(),
                }
            } else {
                RichTextAction::SpliceText {
                    region: RegionKind::Body,
                    block: first,
                    at: 0,
                    del: 0,
                    ins: "Peer".into(),
                }
            };
            edit.apply_under(&mut doc, &ROOT).unwrap();
            let heads = doc.get_heads();
            assert!(plan(&state(&doc), &target, "![Image](asset:synthetic)".into()).is_none());
            assert_eq!(doc.get_heads(), heads);
        }
    }

    #[test]
    fn incompatible_source_attributes_refuse_instead_of_downgrading() {
        for (key, value) in [
            ("code", ui::automerge::ScalarValue::Boolean(true)),
            (
                "table_source",
                ui::automerge::ScalarValue::Str("| a |".into()),
            ),
            ("custom_float", ui::automerge::ScalarValue::F64(1.25)),
        ] {
            let mut doc = fixture();
            let first = state(&doc).blocks(0..1).next().unwrap().id.clone();
            // Imported canonical content can carry attributes outside the
            // editor action allowlist; refusing must preserve those too.
            let captured_after = ImagePasteTarget::After(first.clone());
            RichTextAction::SpliceText {
                region: RegionKind::Body,
                block: first.clone(),
                at: 0,
                del: 2,
                ins: String::new(),
            }
            .apply_under(&mut doc, &ROOT)
            .unwrap();
            let c = tilo_doc::model::open(&doc, &ROOT).unwrap();
            let mut ids = tilo_doc::model::IdGen::new(doc.get_actor().to_hex_string());
            tilo_doc::model::DocMut {
                doc: &mut doc,
                c: &c,
                ids: &mut ids,
            }
            .set_attr(&first, key, value);
            let before = state(&doc);
            let target = ImagePasteTarget::Replace {
                blocks: before.blocks(0..2).map(|block| content_of(block)).collect(),
                start_byte: 0,
                end_byte: "日".len(),
            };
            assert!(
                plan(&before, &captured_after, "![Image](asset:synthetic)".into()).is_none(),
                "empty {key}"
            );
            assert!(
                plan(&before, &target, "![Image](asset:synthetic)".into()).is_none(),
                "{key}"
            );
        }
    }
}
