//! The characters a change wrote into a document, as the anchor an action names text by.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "a change's own character offsets, bounded by the patch and the text they came from"
)]

/// The key a chunked document keeps its items under, each item's fields in the item's own map.
/// `tilolang-runtime` writes documents whose items are reached another way than a resource's
/// `<collection>/<id>/v`; this crate sits below it, so it names the key itself.
const ITEMS_KEY: &str = "$items";

/// The characters `changes`, one action's patches, wrote into `doc`, as the anchor an invocation
/// names text by: the region and block each text is drawn in, and the range each one changed,
/// counted in characters. A rich text's block is the paragraph the field holding the text names
/// (`body`) and its range what was inserted there; the first region it wrote in is the one it
/// names. Without one, an action that changed one plain text field names that field: the region
/// the field's id, the block the item holding it, and the range the characters it inserted or
/// replaced — the words that are new, not the field around them. The words of an item kept in
/// another item's collection (a comment in an issue's `comments`) are in that collection, under
/// both items (`<issue>/<comment>`). An action that made an item names what it made instead.
/// `None` when it wrote no text.
#[must_use]
pub fn written_by(
    _doc: &impl automerge::ReadDoc,
    changes: &[automerge::Patch],
) -> Option<tilo_actions::InteractionAnchor> {
    use automerge::{ObjType, PatchAction, Prop, Value};
    let mut region: Option<&str> = None;
    let mut ranges: Vec<tilo_actions::InteractionTextRange> = Vec::new();
    let mut fields: Vec<FieldWrite<'_>> = Vec::new();
    let mut made = false;
    for patch in changes {
        let keys: Vec<&str> = patch
            .path
            .iter()
            .map(|(_, prop)| match prop {
                Prop::Map(key) => key.as_str(),
                Prop::Seq(_) => "",
            })
            .collect();
        let rich = keys
            .iter()
            .position(|key| *key == crate::schema::KEY_BLOCKS);
        let (blocks, at, inserted) = match (&patch.action, rich) {
            (PatchAction::SpliceText { index, value, .. }, Some(blocks)) => (blocks, *index, value),
            (PatchAction::SpliceText { index, value, .. }, None) => {
                let start = *index as u64;
                let end = start.saturating_add(value.make_string().chars().count() as u64);
                field_changed(&mut fields, &patch.obj, keys, start, end);
                continue;
            }
            (PatchAction::DeleteSeq { index, length }, None) => {
                let start = *index as u64;
                let end = start.saturating_add(*length as u64);
                field_changed(&mut fields, &patch.obj, keys, start, end);
                continue;
            }
            (
                PatchAction::PutMap {
                    value: (Value::Object(ObjType::Map), _),
                    ..
                },
                _,
            ) => {
                made = true;
                continue;
            }
            _ => continue,
        };
        // A block's text is `…/<field>/blocks/<block>/text`.
        let [.., block, crate::schema::BLOCK_TEXT] = keys.as_slice() else {
            continue;
        };
        let field = blocks.checked_sub(1).map_or("body", |at| keys[at]);
        // One region: the first the action wrote in.
        if block.is_empty() || field.is_empty() || *region.get_or_insert(field) != field {
            continue;
        }
        let start = at as u64;
        let end = start + inserted.make_string().chars().count() as u64;
        if let Some(range) = ranges.iter_mut().find(|range| range.block == *block) {
            range.start = range.start.min(start);
            range.end = range.end.max(end);
        } else if ranges.len() < tilo_actions::MAX_ANCHOR_ITEMS {
            let block = (*block).to_owned();
            ranges.push(tilo_actions::InteractionTextRange { block, start, end });
        }
    }
    if let Some(region) = region {
        return Some(tilo_actions::InteractionAnchor::Text {
            region: region.to_owned(),
            ranges,
        });
    }
    let [field] = fields.as_slice() else {
        return None;
    };
    if made {
        return None;
    }
    let (region, block) = field_place(&field.keys)?;
    Some(tilo_actions::InteractionAnchor::Text {
        region,
        ranges: vec![tilo_actions::InteractionTextRange {
            block,
            start: field.start,
            end: field.end,
        }],
    })
}

/// One plain text field a change wrote into: the text object, the keys down to it, and the
/// characters it inserted or replaced there.
struct FieldWrite<'a> {
    obj: &'a automerge::ObjId,
    keys: Vec<&'a str>,
    start: u64,
    end: u64,
}

/// Note that the text `obj`, at `keys`, gained characters at `start..end` (an insertion or a
/// replacement): the field's changed span grows to hold it.
fn field_changed<'a>(
    fields: &mut Vec<FieldWrite<'a>>,
    obj: &'a automerge::ObjId,
    keys: Vec<&'a str>,
    start: u64,
    end: u64,
) {
    match fields.iter_mut().find(|field| field.obj == obj) {
        Some(field) => {
            field.start = field.start.min(start);
            field.end = field.end.max(end);
        }
        None => fields.push(FieldWrite {
            obj,
            keys,
            start,
            end,
        }),
    }
}

/// The region and block a plain text field at `keys` is drawn in ([`written_by`]): the field
/// and the item holding it, or, for an item inside another item's collection, that collection
/// and the items' path. `None` where a list stands on the way.
fn field_place(keys: &[&str]) -> Option<(String, String)> {
    let (field, above) = keys.split_last()?;
    if keys.iter().any(|key| key.is_empty()) {
        return None;
    }
    // A chunked document keeps its items under `$items`, each item's fields in the item's own
    // map: the item is the key after the collection.
    if let Some(item) = keys
        .iter()
        .rposition(|key| *key == ITEMS_KEY)
        .and_then(|at| keys.get(at + 1))
    {
        return Some(((*field).to_owned(), (*item).to_owned()));
    }
    // Each item on the way is `<collection>/<id>/v`.
    let items: Vec<(&str, &str)> = above
        .windows(3)
        .filter(|keys| keys[2] == tilo_resource::shape::ENTRY_VALUE_KEY)
        .map(|keys| (keys[0], keys[1]))
        .collect();
    match items.as_slice() {
        [] => Some(((*field).to_owned(), String::new())),
        [(_, item)] => Some(((*field).to_owned(), (*item).to_owned())),
        [.., (collection, _)] => Some((
            (*collection).to_owned(),
            items
                .iter()
                .map(|(_, id)| *id)
                .collect::<Vec<_>>()
                .join("/"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{InteractionAnchor, InteractionTextRange};

    /// What `change` wrote into a document holding issue `7d2b` titled "Book the hall", with
    /// one comment `c9f1`.
    fn wrote(change: impl FnOnce(&mut automerge::AutoCommit)) -> Option<InteractionAnchor> {
        use automerge::{ObjType, ROOT, transaction::Transactable as _};
        let mut doc = automerge::AutoCommit::new();
        let issues = doc.put_object(ROOT, "issues", ObjType::Map).unwrap();
        let entry = doc.put_object(&issues, "7d2b", ObjType::Map).unwrap();
        let issue = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        let title = doc.put_object(&issue, "title", ObjType::Text).unwrap();
        doc.splice_text(&title, 0, 0, "Book the hall").unwrap();
        let comments = doc.put_object(&issue, "comments", ObjType::Map).unwrap();
        let entry = doc.put_object(&comments, "c9f1", ObjType::Map).unwrap();
        let comment = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        let text = doc.put_object(&comment, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "Hall 4").unwrap();
        let before = doc.get_heads();
        change(&mut doc);
        let after = doc.get_heads();
        let changes = doc.diff(&before, &after);
        written_by(&doc, &changes)
    }

    fn text_at(region: &str, block: &str, (start, end): (u64, u64)) -> InteractionAnchor {
        InteractionAnchor::Text {
            region: region.into(),
            ranges: vec![InteractionTextRange {
                block: block.into(),
                start,
                end,
            }],
        }
    }

    #[test]
    fn a_change_names_the_characters_it_wrote_where_they_are_drawn() {
        use automerge::{ROOT, ReadDoc as _, transaction::Transactable as _};
        let field = |doc: &automerge::AutoCommit, path: &[&str]| {
            path.iter().fold(ROOT, |obj, key| {
                doc.get(&obj, *key).unwrap().expect("there").1
            })
        };
        let renamed = wrote(|doc| {
            let title = field(doc, &["issues", "7d2b", "v", "title"]);
            doc.splice_text(&title, 5, 4, "").unwrap();
            doc.splice_text(&title, 9, 0, " 4").unwrap();
        });
        assert_eq!(
            renamed,
            Some(text_at("title", "7d2b", (5, 11))),
            "a rename names the characters it replaced and typed, in its item's block"
        );
        let shortened = wrote(|doc| {
            let title = field(doc, &["issues", "7d2b", "v", "title"]);
            doc.splice_text(&title, 4, 9, "").unwrap();
        });
        assert_eq!(
            shortened,
            Some(text_at("title", "7d2b", (4, 13))),
            "a cut names what it took away"
        );
        let said = wrote(|doc| {
            let text = field(
                doc,
                &["issues", "7d2b", "v", "comments", "c9f1", "v", "text"],
            );
            doc.splice_text(&text, 6, 0, " is free").unwrap();
        });
        assert_eq!(
            said,
            Some(text_at("comments", "7d2b/c9f1", (6, 14))),
            "a comment's words are in its issue's thread, where they were typed"
        );
        let both = wrote(|doc| {
            let title = field(doc, &["issues", "7d2b", "v", "title"]);
            doc.splice_text(&title, 0, 0, "Re: ").unwrap();
            let text = field(
                doc,
                &["issues", "7d2b", "v", "comments", "c9f1", "v", "text"],
            );
            doc.splice_text(&text, 0, 0, "Re: ").unwrap();
        });
        assert_eq!(both, None, "two fields are no one place");
        let made = wrote(|doc| {
            let issues = field(doc, &["issues"]);
            let entry = doc
                .put_object(&issues, "e41a", automerge::ObjType::Map)
                .unwrap();
            let issue = doc
                .put_object(&entry, "v", automerge::ObjType::Map)
                .unwrap();
            let title = doc
                .put_object(&issue, "title", automerge::ObjType::Text)
                .unwrap();
            doc.splice_text(&title, 0, 0, "New").unwrap();
        });
        assert_eq!(
            made, None,
            "what made an item names the item, not its title"
        );
    }
}
