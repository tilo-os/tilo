//! Where a version's name lives: in the document, with the changes it names.
//!
//! A name that lived in the widget would vanish on close, and a name that lived
//! beside the document would never reach a collaborator. So a name is written
//! into the same CRDT as the content it describes — one record at the document
//! root per named range, keyed by the range it covers.
//!
//! ```text
//! ROOT
//!   "qos:group:<hex of the range's first change>"   <- the key IS the range
//!      label : "Intro rewrite"
//!      end   : "<hex of the range's last change>"   (absent while open)
//!      by    : "<actor hex of whoever wrote the name>"
//!      at    : <unix millis>
//! ```
//!
//! # Why the key is the range's first change
//!
//! Two people naming **the same** run of changes at the same time is the case a
//! shared history has to survive. Because the key is derived from the range
//! rather than minted per writer, both writes land on the same map key, and
//! automerge's own map-conflict rule picks one winner — deterministically, and
//! identically on every peer. The losing name is not destroyed: it is still in
//! the change graph and still reachable through `get_conflicts`.
//!
//! # Why the records sit at the root rather than under one container map
//!
//! A container would have to be created by whoever names first, and two peers
//! that both name first create two containers under one key. Automerge resolves
//! *that* conflict too — by discarding one container and every record inside it.
//! Two people naming two different ranges at the same time would then silently
//! lose one name. Flat, prefixed root keys have no container to race over, so
//! only genuinely-colliding ranges ever conflict.
//!
//! Overlapping-but-different ranges are a different question, and it is
//! answered at read time by
//! [`group_entries`](crate::widget::history_group::group_entries) rather than
//! here — this module only stores what people wrote.
//!
//! # These writes are changes too
//!
//! Naming a version commits into the document, so it appears in the change
//! graph like anything else. It is deliberately **not** given a row of its own
//! in the history surfaces: the name is already on screen as the group's title,
//! and a "1 field" row directly under the band it named is noise. See
//! [`is_group_marker`], which is what the producer filters on.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, PatchAction, Prop, ReadDoc, ScalarValue, Value};

use crate::history_source::authors::AuthorDirectory;
use crate::widget::history::HistoryEntryId;
use crate::widget::history_group::{from_hex16, hex16, NamedRange};

/// Prefix of the root keys holding this document's named ranges.
///
/// Namespaced so it cannot collide with an app's own schema: `qdoc` uses
/// `content` / `comments` / `props`, and the shell's room document uses
/// `workspaces` / `active`.
pub const GROUPS_KEY: &str = "qos:group:";

/// Longest name accepted. A version name is a label, not a paragraph; past this
/// it stops fitting anywhere it is rendered and starts being a way to make
/// another user's timeline unreadable.
pub const MAX_LABEL_LEN: usize = 120;

/// Read every named range out of `doc`.
///
/// Records that do not parse (a corrupted key, a missing label) are skipped
/// rather than guessed at — a band in the wrong place is worse than no band.
pub fn read_named_ranges(doc: &AutoCommit, authors: &AuthorDirectory) -> Vec<NamedRange> {
    let mut out = Vec::new();
    for key in doc.keys(automerge::ROOT) {
        let Some(hex) = key.strip_prefix(GROUPS_KEY) else {
            continue;
        };
        let Some(start) = from_hex16(hex) else {
            continue;
        };
        let Ok(Some((Value::Object(ObjType::Map), rec))) = doc.get(automerge::ROOT, &key) else {
            continue;
        };
        let Some(label) = str_at(doc, &rec, "label") else {
            continue;
        };
        let end = str_at(doc, &rec, "end").and_then(|s| from_hex16(&s));
        let by = str_at(doc, &rec, "by");
        let created = int_at(doc, &rec, "at")
            .filter(|ms| *ms > 0)
            .map(|ms| UNIX_EPOCH + Duration::from_millis(ms as u64))
            .unwrap_or(UNIX_EPOCH);
        out.push(NamedRange {
            id: key.clone(),
            label,
            start,
            end,
            named_by: by.map(|a| authors.resolve(&a)),
            created,
        });
    }
    // Stable order so two peers with the same document build the same forest.
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Name the run of changes from `start` to `end`, or open an unbounded run when
/// `end` is `None`.
///
/// Returns the record id (which is also the document key). The transaction is
/// left open — the host commits, so the naming lands stamped and attributed
/// like any other edit.
pub fn name_range(
    doc: &mut AutoCommit,
    start: &HistoryEntryId,
    end: Option<&HistoryEntryId>,
    label: &str,
) -> String {
    let key = format!("{GROUPS_KEY}{}", hex16(start));
    let label: String = label.trim().chars().take(MAX_LABEL_LEN).collect();
    let by = doc.get_actor().to_hex_string();
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    // Replacing the whole record rather than patching fields keeps a rename and
    // a re-scope atomic: a peer never observes the new label against the old
    // range.
    let Ok(rec) = doc.put_object(automerge::ROOT, &key, ObjType::Map) else {
        return key;
    };
    let _ = doc.put(&rec, "label", label);
    if let Some(e) = end {
        let _ = doc.put(&rec, "end", hex16(e));
    }
    let _ = doc.put(&rec, "by", by);
    let _ = doc.put(&rec, "at", at);
    key
}

/// Close an open range at `end`. `false` when no such record exists.
pub fn close_range(doc: &mut AutoCommit, record_id: &str, end: &HistoryEntryId) -> bool {
    let Ok(Some((Value::Object(ObjType::Map), rec))) = doc.get(automerge::ROOT, record_id) else {
        return false;
    };
    doc.put(&rec, "end", hex16(end)).is_ok()
}

/// Remove a name. The changes it covered stay; only the band goes.
pub fn clear_name(doc: &mut AutoCommit, record_id: &str) -> bool {
    if !record_id.starts_with(GROUPS_KEY) {
        return false;
    }
    if doc.get(automerge::ROOT, record_id).ok().flatten().is_none() {
        return false;
    }
    doc.delete(automerge::ROOT, record_id).is_ok()
}

/// True when every patch in `patches` lands inside the named-range map — i.e.
/// the change did nothing but name a version.
///
/// The producer uses this to keep naming out of the row list. An empty patch
/// set is **not** a marker: a change that did nothing visible is still a change,
/// and hiding it would be a different (and dishonest) behaviour.
pub fn is_group_marker(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|p| match p.path.first() {
            Some((_, Prop::Map(k))) => k.starts_with(GROUPS_KEY),
            Some(_) => false,
            // A patch at the root: the creation (or removal) of a record itself.
            None => match &p.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key, .. } => {
                    key.starts_with(GROUPS_KEY)
                }
                _ => false,
            },
        })
}

fn str_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(t) => Some(t.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn int_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Int(n) => Some(*n),
            ScalarValue::Uint(n) => Some(*n as i64),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(b: u8) -> HistoryEntryId {
        HistoryEntryId([b; 16])
    }

    #[test]
    fn a_name_round_trips_through_the_document() {
        let mut doc = AutoCommit::new();
        doc.set_actor(automerge::ActorId::from("alice".as_bytes()));
        let rec = name_range(&mut doc, &id(1), Some(&id(9)), "  Intro rewrite  ");
        doc.commit();

        let dir = AuthorDirectory::new();
        let ranges = read_named_ranges(&doc, &dir);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].id, rec);
        assert_eq!(ranges[0].label, "Intro rewrite", "the label is trimmed");
        assert_eq!(ranges[0].start, id(1));
        assert_eq!(ranges[0].end, Some(id(9)));
        assert!(ranges[0].named_by.is_some(), "the writer is recorded");
    }

    #[test]
    fn an_open_range_has_no_end_until_it_is_closed() {
        let mut doc = AutoCommit::new();
        let rec = name_range(&mut doc, &id(1), None, "Working");
        doc.commit();
        let ranges = read_named_ranges(&doc, &AuthorDirectory::new());
        assert_eq!(ranges[0].end, None, "an open range is open");

        assert!(close_range(&mut doc, &rec, &id(4)));
        doc.commit();
        let ranges = read_named_ranges(&doc, &AuthorDirectory::new());
        assert_eq!(ranges[0].end, Some(id(4)));
    }

    #[test]
    fn clearing_a_name_removes_the_band_and_nothing_else() {
        let mut doc = AutoCommit::new();
        let a = name_range(&mut doc, &id(1), Some(&id(2)), "One");
        let b = name_range(&mut doc, &id(3), Some(&id(4)), "Two");
        doc.commit();
        assert!(clear_name(&mut doc, &a));
        assert!(!clear_name(&mut doc, "not-a-record"));
        doc.commit();
        let ranges = read_named_ranges(&doc, &AuthorDirectory::new());
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].id, b);
    }

    #[test]
    fn two_peers_naming_the_same_range_converge_on_one_name() {
        // The key is the range, not the writer — so this is a map conflict,
        // which automerge resolves the same way on both sides. Whichever name
        // wins, both peers must see the *same* one, and exactly one band.
        let mut alice = AutoCommit::new();
        alice.set_actor(automerge::ActorId::from("alice".as_bytes()));
        alice.put(automerge::ROOT, "seed", "x").unwrap();
        alice.commit();
        let mut bob = alice.fork();
        bob.set_actor(automerge::ActorId::from("bob".as_bytes()));

        name_range(&mut alice, &id(1), Some(&id(5)), "Intro rewrite");
        alice.commit();
        name_range(&mut bob, &id(1), Some(&id(5)), "First draft");
        bob.commit();

        let mut alice_merged = alice.fork();
        alice_merged.merge(&mut bob.clone()).unwrap();
        let mut bob_merged = bob.fork();
        bob_merged.merge(&mut alice.clone()).unwrap();

        let a = read_named_ranges(&alice_merged, &AuthorDirectory::new());
        let b = read_named_ranges(&bob_merged, &AuthorDirectory::new());
        assert_eq!(a.len(), 1, "one range, one band");
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].label, b[0].label, "both peers read the same winner");
        assert!(
            a[0].label == "Intro rewrite" || a[0].label == "First draft",
            "the winner is one of the two names actually written: {:?}",
            a[0].label
        );
    }

    #[test]
    fn two_peers_naming_different_ranges_both_survive() {
        let mut alice = AutoCommit::new();
        alice.set_actor(automerge::ActorId::from("alice".as_bytes()));
        alice.put(automerge::ROOT, "seed", "x").unwrap();
        alice.commit();
        let mut bob = alice.fork();
        bob.set_actor(automerge::ActorId::from("bob".as_bytes()));

        name_range(&mut alice, &id(1), Some(&id(3)), "Intro");
        alice.commit();
        name_range(&mut bob, &id(4), Some(&id(6)), "Body");
        bob.commit();
        alice.merge(&mut bob).unwrap();

        let ranges = read_named_ranges(&alice, &AuthorDirectory::new());
        let labels: Vec<&str> = ranges.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels.len(), 2, "got {labels:?}");
        assert!(labels.contains(&"Intro") && labels.contains(&"Body"));
    }

    #[test]
    fn naming_a_version_is_recognisable_as_a_marker_change() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "content", "hello").unwrap();
        doc.commit();
        let before = doc.get_heads();
        name_range(&mut doc, &id(1), Some(&id(2)), "A version");
        doc.commit();
        let after = doc.get_heads();
        let patches = doc.diff(&before, &after);
        assert!(!patches.is_empty());
        assert!(is_group_marker(&patches), "got {patches:?}");

        // A real edit is not a marker, even one that also happens to be small.
        let before = doc.get_heads();
        doc.put(automerge::ROOT, "content", "hello there").unwrap();
        doc.commit();
        let after = doc.get_heads();
        let patches = doc.diff(&before, &after);
        assert!(!is_group_marker(&patches));
    }

    #[test]
    fn a_very_long_name_is_capped_rather_than_stored_whole() {
        let mut doc = AutoCommit::new();
        name_range(&mut doc, &id(1), None, &"x".repeat(1000));
        doc.commit();
        let ranges = read_named_ranges(&doc, &AuthorDirectory::new());
        assert_eq!(ranges[0].label.chars().count(), MAX_LABEL_LEN);
    }

    #[test]
    fn a_record_under_a_key_that_is_not_a_change_id_is_skipped() {
        let mut doc = AutoCommit::new();
        let rec = doc
            .put_object(automerge::ROOT, format!("{GROUPS_KEY}junk"), ObjType::Map)
            .unwrap();
        doc.put(&rec, "label", "nowhere").unwrap();
        doc.commit();
        assert!(read_named_ranges(&doc, &AuthorDirectory::new()).is_empty());
    }
}
