//! What a change did, as a sentence a model wrote on a member's device.
//!
//! A description is history metadata, kept where the change's name is kept
//! ([`groups`](super::groups)): in the resource's own document, one record per
//! described change at the root, keyed by the change it describes:
//!
//! ```text
//! ROOT
//!   "tilo:describe:<hex of the change>"
//!      text : "Moved the budget table under Costs"
//!      by   : "<the generator: provider and model>"
//! ```
//!
//! So one member's device writes it, it syncs end to end with the content to
//! everyone in the workspace, and no other device asks a model again. Writing
//! one is a change, but neither a row of History nor a step of undo
//! ([`super::is_history_metadata`]). The facts stay the change itself: a
//! description is shown in place of the change's own summary, never instead
//! of its author, time or kind, and never decides grouping or counts.

use std::collections::HashMap;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, PatchAction, Prop, ReadDoc, Value};

use crate::widget::history::HistoryEntryId;
use crate::widget::history_group::{from_hex16, hex16};

/// Namespaced beside the names of runs, apart from any app's own schema.
pub const DESCRIPTIONS_KEY: &str = "tilo:describe:";

/// A description is one short sentence; past this it is a paragraph that
/// no row has room for.
pub const MAX_DESCRIPTION_LEN: usize = 160;

/// Every description the document carries, by the change it describes.
/// A record that does not parse is skipped: no description is better than a
/// wrong one.
pub fn read_descriptions(doc: &AutoCommit) -> HashMap<HistoryEntryId, String> {
    let mut out = HashMap::new();
    for key in doc.keys(automerge::ROOT) {
        let Some(id) = key.strip_prefix(DESCRIPTIONS_KEY).and_then(from_hex16) else {
            continue;
        };
        let Ok(Some((Value::Object(ObjType::Map), record))) = doc.get(automerge::ROOT, &key) else {
            continue;
        };
        let text = match doc.get(&record, "text") {
            Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
            _ => None,
        };
        if let Some(text) = text.filter(|text| is_acceptable(text)) {
            out.insert(id, text);
        }
    }
    out
}

/// Whether `text` may stand as a description: one non-empty line within
/// [`MAX_DESCRIPTION_LEN`].
pub fn is_acceptable(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text.chars().count() <= MAX_DESCRIPTION_LEN
        && !text.chars().any(char::is_control)
}

/// Write the description of the change `id`, unless the document already
/// carries one: the first member to describe a change describes it for
/// everyone. `false` when nothing was written. The transaction is left open
/// for the host to commit.
pub fn write_description(
    doc: &mut AutoCommit,
    id: &HistoryEntryId,
    text: &str,
    generator: &str,
) -> bool {
    let text = text.trim();
    if !is_acceptable(text) {
        return false;
    }
    let key = format!("{DESCRIPTIONS_KEY}{}", hex16(id));
    if doc.get(automerge::ROOT, &key).ok().flatten().is_some() {
        return false;
    }
    let Ok(record) = doc.put_object(automerge::ROOT, &key, ObjType::Map) else {
        return false;
    };
    let generator: String = generator.chars().take(MAX_DESCRIPTION_LEN).collect();
    doc.put(&record, "text", text).is_ok() && doc.put(&record, "by", generator).is_ok()
}

/// The most a change's evidence quotes of what it wrote.
const MAX_EVIDENCE_CHARS: usize = 240;

/// What each of `ids` wrote, quoted for a model to describe: the text it
/// typed and the values it set, cut to a sentence's worth. Changes the
/// document does not hold are left out.
pub fn change_evidence(
    doc: &mut AutoCommit,
    ids: &[HistoryEntryId],
) -> HashMap<HistoryEntryId, String> {
    let mut out = HashMap::new();
    for change in doc.get_changes(&[]) {
        let id = super::entry_id_from_hash(&change.hash().to_string());
        if !ids.contains(&id) {
            continue;
        }
        let patches = doc.diff(change.deps(), &[change.hash()]);
        let mut quoted = String::new();
        for patch in &patches {
            let piece = match &patch.action {
                PatchAction::SpliceText { value, .. } => value.make_string(),
                PatchAction::PutMap {
                    key,
                    value: (Value::Scalar(value), _),
                    ..
                } if !key.starts_with("tilo:") => match value.as_str() {
                    Some(text) => format!("{key}: {text}"),
                    None => format!("{key}: {value}"),
                },
                _ => continue,
            };
            if !quoted.is_empty() {
                quoted.push(' ');
            }
            quoted.push_str(piece.trim());
            if quoted.chars().count() >= MAX_EVIDENCE_CHARS {
                break;
            }
        }
        let quoted: String = quoted.chars().take(MAX_EVIDENCE_CHARS).collect();
        out.insert(id, quoted);
    }
    out
}

/// Whether a change only wrote descriptions.
pub fn is_description_marker(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|patch| match patch.path.first() {
            Some((_, Prop::Map(key))) => key.starts_with(DESCRIPTIONS_KEY),
            Some(_) => false,
            None => match &patch.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    key.starts_with(DESCRIPTIONS_KEY)
                }
                _ => false,
            },
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_description_is_written_once_and_read_back_by_its_change() {
        let mut doc = AutoCommit::new();
        let id = HistoryEntryId([7; 16]);
        assert!(write_description(
            &mut doc,
            &id,
            " Moved the table under Costs ",
            "local:m"
        ));
        assert!(
            !write_description(&mut doc, &id, "Something else", "local:m"),
            "the first description stands"
        );
        doc.commit();
        assert_eq!(
            read_descriptions(&doc).get(&id).map(String::as_str),
            Some("Moved the table under Costs")
        );

        let before = doc.get_heads();
        assert!(write_description(
            &mut doc,
            &HistoryEntryId([8; 16]),
            "Renamed it",
            "m"
        ));
        doc.commit();
        let after = doc.get_heads();
        let patches = doc.diff(&before, &after);
        assert!(is_description_marker(&patches));

        let mut typed = AutoCommit::new();
        let text = typed
            .put_object(automerge::ROOT, "body", ObjType::Text)
            .unwrap();
        typed.commit();
        typed
            .splice_text(&text, 0, 0, "Costs rose in March")
            .unwrap();
        typed.put(automerge::ROOT, "status", "draft").unwrap();
        typed.commit();
        let change = typed.get_heads().pop().unwrap();
        let id = super::super::entry_id_from_hash(&change.to_string());
        let evidence = change_evidence(&mut typed, &[id]);
        let quoted = evidence.get(&id).unwrap();
        assert!(
            quoted.contains("Costs rose in March") && quoted.contains("status: draft"),
            "{quoted}"
        );

        for refused in ["", "two\nlines", &"x".repeat(MAX_DESCRIPTION_LEN + 1)] {
            assert!(!write_description(
                &mut doc,
                &HistoryEntryId([9; 16]),
                refused,
                "m"
            ));
        }
    }
}
