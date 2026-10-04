//! Block commands copy the canonical source, including marks the view does not paint.
use std::ops::Range;
use std::rc::Rc;

use tilo_doc::snapshot::BlockView;
use tilo_document::action::RegionKind;
use tilo_document::{ClipAttr, ClipBlock, RichTextAction};

/// Keep unsupported nested structures and floating-point attributes intact by
/// refusing them; the existing clipboard vocabulary cannot represent them.
pub(crate) fn duplicate(sources: &[Rc<BlockView>], range: Range<usize>) -> Option<RichTextAction> {
    let selected = sources.get(range)?;
    let anchor = selected.last()?;
    if selected.len() >= 10_000
        || selected.iter().any(|view| {
            !view.children.is_empty()
                || view
                    .attrs
                    .values()
                    .any(|value| matches!(value, tilo_doc::AttrVal::Float(_)))
        })
    {
        return None;
    }
    // An empty first fragment leaves the anchor unchanged, then inserts each
    // complete copied paragraph after it. The entire copy is one action.
    let mut blocks = vec![ClipBlock {
        kind: "paragraph".into(),
        text: String::new(),
        marks: Vec::new(),
        level: None,
        ordered: None,
        align: None,
        attrs: std::collections::BTreeMap::default(),
    }];
    // Mint once while authoring the command, then carry the identities in
    // its payload. Existing PasteFragment replay semantics remain unchanged.
    let nonce = ui::automerge::ActorId::random().to_hex_string();
    let mut groups = std::collections::BTreeMap::<String, String>::new();
    let mut legacy: Option<(Option<String>, String)> = None;
    for view in selected {
        let mut clip = crate::clip_block(view);
        if clip.attrs.get("code") == Some(&ClipAttr::Bool(true))
            && !clip.attrs.contains_key("code_group")
        {
            let lang = match clip.attrs.get("lang") {
                Some(ClipAttr::Str(lang)) => Some(lang.clone()),
                _ => None,
            };
            if legacy
                .as_ref()
                .is_none_or(|(previous, _)| previous != &lang)
            {
                legacy = Some((lang, view.id.clone()));
            }
            if let Some((_, group)) = &legacy {
                clip.attrs
                    .insert("code_group".into(), ClipAttr::Str(group.clone()));
            }
        } else {
            legacy = None;
        }
        if let Some(ClipAttr::Str(group)) = clip.attrs.get("code_group") {
            let fresh = if let Some(fresh) = groups.get(group) {
                fresh.clone()
            } else {
                let fresh = format!("copy-{nonce}-{}", groups.len());
                groups.insert(group.clone(), fresh.clone());
                fresh
            };
            clip.attrs.insert("code_group".into(), ClipAttr::Str(fresh));
        }
        blocks.push(clip);
    }
    let at = anchor.text.chars().count();
    Some(RichTextAction::PasteFragment {
        region: RegionKind::Body,
        start_block: anchor.id.clone(),
        start: at,
        end_block: anchor.id.clone(),
        end: at,
        blocks,
        adopt_first_style: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_doc::document::{MarkSpan, MarkValue};

    fn source() -> Rc<BlockView> {
        Rc::new(BlockView {
            id: "source".into(),
            kind: "paragraph".into(),
            text: "é😀".into(),
            marks: vec![MarkSpan {
                name: "color".into(),
                value: MarkValue::Str("#ff0000".into()),
                start: 0,
                end: 2,
            }],
            attrs: [
                ("code".into(), tilo_doc::AttrVal::Bool(true)),
                (
                    "code_group".into(),
                    tilo_doc::AttrVal::Str("original-fence".into()),
                ),
            ]
            .into_iter()
            .collect(),
            children: Vec::new(),
        })
    }

    #[test]
    fn command_carries_unpainted_marks_and_fresh_fence_ids_in_its_replayable_payload() {
        let source = source();
        let RichTextAction::PasteFragment { blocks: first, .. } =
            duplicate(&[Rc::clone(&source)], 0..1).unwrap()
        else {
            panic!("expected a paste action");
        };
        let RichTextAction::PasteFragment { blocks: second, .. } =
            duplicate(&[Rc::clone(&source)], 0..1).unwrap()
        else {
            panic!("expected a paste action");
        };
        assert_eq!(first[1].marks, source.marks);
        assert_eq!(first[1].text, source.text);
        assert_ne!(
            first[1].attrs["code_group"],
            ClipAttr::Str("original-fence".into())
        );
        assert_ne!(first[1].attrs["code_group"], second[1].attrs["code_group"]);
    }

    #[test]
    fn clipboard_limits_refuse_unsupported_metadata_instead_of_dropping_it() {
        let mut source = (*source()).clone();
        source
            .attrs
            .insert("line_spacing".into(), tilo_doc::AttrVal::Float(1.5));
        assert!(duplicate(&[Rc::new(source.clone())], 0..1).is_none());
        source.attrs.remove("line_spacing");
        source.children.push(source.clone());
        assert!(duplicate(&[Rc::new(source)], 0..1).is_none());
    }
}
