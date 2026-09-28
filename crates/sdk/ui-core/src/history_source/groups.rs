//! Where a version's name lives: in the document, with the changes it names.
//!
//! A name that lived in the widget would vanish on close, and a name that
//! lived beside the document would never reach a collaborator. So a name
//! is written into the same CRDT as the content it describes — one record
//! at the document root per named range, keyed by the range it covers:
//!
//! ```text
//! ROOT
//!   "tilo:group:<hex of the range's first change>"   <- the key IS the range
//!      label : "Intro rewrite"
//!      end   : "<hex of the range's last change>"   (absent while open)
//!      by    : "<actor hex of whoever wrote the name>"
//!      at    : <unix millis>
//! ```
//!
//! The key is the range's first change, not minted per writer: two people
//! naming the same run then collide on one map key, and automerge's own
//! conflict rule resolves it deterministically and identically on every
//! peer (the loser stays reachable via `get_conflicts`). Records sit at the
//! root rather than under one container map for the same reason at a
//! coarser grain: a container created independently by two peers naming
//! *different* ranges at the same time would have one discarded whole,
//! silently losing a name. Flat, prefixed root keys have no container to
//! race over.
//!
//! Naming a version is a change like any other, but is deliberately not
//! given its own row in the history surfaces — see [`is_group_marker`].

use crate::time::{Duration, UNIX_EPOCH};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, PatchAction, Prop, ReadDoc, ScalarValue, Value};

use crate::history_source::authors::AuthorDirectory;
use crate::widget::history::HistoryEntryId;
use crate::widget::history_group::{NamedRange, from_hex16, hex16};

/// Namespaced so it cannot collide with an app's own schema: `qdoc` uses
/// `content` / `comments` / `props`, and the shell's room document uses
/// `workspaces` / `active`.
pub const GROUPS_KEY: &str = "tilo:group:";

/// A version name is a label, not a paragraph; past this it stops fitting
/// anywhere it is rendered and starts being a way to make another user's
/// timeline unreadable.
pub const MAX_LABEL_LEN: usize = 120;

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
        // `filter` above guarantees `ms > 0` by the time the cast below runs,
        // so the conversion never loses a sign; `checked_add` only guards
        // the (unreachable for an out-of-range timestamp) case of overflowing
        // `SystemTime`'s own representation.
        let created = int_at(doc, &rec, "at")
            .filter(|ms| *ms > 0)
            .and_then(|ms| u64::try_from(ms).ok())
            .and_then(|ms| UNIX_EPOCH.checked_add(Duration::from_millis(ms)))
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

/// Names the run of changes from `start` to `end`, or opens an unbounded run
/// when `end` is `None`. Returns the record id (also the document key). The
/// transaction is left open — the host commits, so the naming lands stamped
/// and attributed like any other edit.
pub fn name_range(
    doc: &mut AutoCommit,
    start: &HistoryEntryId,
    end: Option<&HistoryEntryId>,
    label: &str,
) -> String {
    let key = format!("{GROUPS_KEY}{}", hex16(start));
    let label: String = label.trim().chars().take(MAX_LABEL_LEN).collect();
    let by = doc.get_actor().to_hex_string();
    let at = crate::time::wall()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
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

/// Split one named band into two adjacent named bands in the current
/// transaction. The caller supplies both boundary ids because a CRDT history
/// is not inherently linear at concurrent branches; guessing adjacency here
/// would make the operation non-deterministic across peers.
pub fn split_named_range(
    doc: &mut AutoCommit,
    record_id: &str,
    left_end: &HistoryEntryId,
    right_start: &HistoryEntryId,
    left_label: &str,
    right_label: &str,
) -> Option<(String, String)> {
    let original = named_range_record(doc, record_id)?;
    if !clear_name(doc, record_id) {
        return None;
    }
    let left = name_range(doc, &original.0, Some(left_end), left_label);
    let right = name_range(doc, right_start, original.1.as_ref(), right_label);
    Some((left, right))
}

/// Merge named bands into one caller-resolved range. Every old record must
/// exist before anything is changed, so a stale UI cannot partially merge a
/// set that a collaborator has already edited.
pub fn merge_named_ranges(
    doc: &mut AutoCommit,
    record_ids: &[String],
    start: &HistoryEntryId,
    end: Option<&HistoryEntryId>,
    label: &str,
) -> Option<String> {
    if record_ids.len() < 2
        || record_ids
            .iter()
            .any(|record| named_range_record(doc, record).is_none())
    {
        return None;
    }
    for record in record_ids {
        if !clear_name(doc, record) {
            return None;
        }
    }
    Some(name_range(doc, start, end, label))
}

/// Move the boundary between two named bands while preserving both labels.
/// As with split, the caller resolves the two sides from its displayed causal
/// order and sends exact ids.
pub fn move_named_boundary(
    doc: &mut AutoCommit,
    left_record: &str,
    right_record: &str,
    new_left_end: &HistoryEntryId,
    new_right_start: &HistoryEntryId,
) -> Option<(String, String)> {
    let left = named_range_record(doc, left_record)?;
    let right = named_range_record(doc, right_record)?;
    let left_label = str_at_record(doc, left_record, "label")?;
    let right_label = str_at_record(doc, right_record, "label")?;
    if !clear_name(doc, left_record) || !clear_name(doc, right_record) {
        return None;
    }
    let new_left = name_range(doc, &left.0, Some(new_left_end), &left_label);
    let new_right = name_range(doc, new_right_start, right.1.as_ref(), &right_label);
    Some((new_left, new_right))
}

/// One work session beside a seam, as the person saw it: its named record,
/// or `None` while it is still an automatic projection with these bounds and
/// title.
#[derive(Clone, Copy)]
pub struct DisplayedSession<'a> {
    pub record: Option<&'a str>,
    pub start: &'a HistoryEntryId,
    pub end: &'a HistoryEntryId,
    pub label: &'a str,
}

impl DisplayedSession<'_> {
    fn materialize(self, doc: &mut AutoCommit) -> String {
        match self.record {
            Some(record) => record.to_string(),
            None => name_range(doc, self.start, Some(self.end), self.label),
        }
    }
}

/// Correct a seam even when one or both work sessions are still automatic
/// projections. Missing records are materialized with the titles and bounds
/// the person saw, then the bordering action is moved in the same document
/// transaction. The atomic history entries themselves are never rewritten.
pub fn materialize_and_move_boundary(
    doc: &mut AutoCommit,
    left: DisplayedSession<'_>,
    right: DisplayedSession<'_>,
    new_left_end: &HistoryEntryId,
    new_right_start: &HistoryEntryId,
) -> Option<(String, String)> {
    if left
        .record
        .is_some_and(|record| named_range_record(doc, record).is_none())
        || right
            .record
            .is_some_and(|record| named_range_record(doc, record).is_none())
        || matches!((left.record, right.record), (Some(left), Some(right)) if left == right)
    {
        return None;
    }
    let left_record = left.materialize(doc);
    let right_record = right.materialize(doc);
    move_named_boundary(
        doc,
        &left_record,
        &right_record,
        new_left_end,
        new_right_start,
    )
}

fn named_range_record(
    doc: &AutoCommit,
    record_id: &str,
) -> Option<(HistoryEntryId, Option<HistoryEntryId>)> {
    let start = record_id.strip_prefix(GROUPS_KEY).and_then(from_hex16)?;
    let Ok(Some((Value::Object(ObjType::Map), rec))) = doc.get(automerge::ROOT, record_id) else {
        return None;
    };
    let end = str_at(doc, &rec, "end").and_then(|value| from_hex16(&value));
    Some((start, end))
}

fn str_at_record(doc: &AutoCommit, record_id: &str, field: &str) -> Option<String> {
    let Ok(Some((Value::Object(ObjType::Map), rec))) = doc.get(automerge::ROOT, record_id) else {
        return None;
    };
    str_at(doc, &rec, field)
}

/// True when every patch in `patches` lands inside the named-range map — used
/// by the producer to keep naming out of the row list. An empty patch set is
/// **not** a marker: a change that did nothing visible is still a change, and
/// hiding it would be incorrect behaviour.
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
            ScalarValue::Uint(n) => Some(i64::try_from(*n).unwrap_or(i64::MAX)),
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
    fn named_ranges_round_trip_as_bounded_marker_changes() {
        // phase: a_name_round_trips_through_the_document
        {
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
        // phase: an_open_range_has_no_end_until_it_is_closed
        {
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
        // phase: clearing_a_name_removes_the_band_and_nothing_else
        {
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
        // phase: naming_a_version_is_recognisable_as_a_marker_change
        {
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

            // An edit is not a marker, even a small one.
            let before = doc.get_heads();
            doc.put(automerge::ROOT, "content", "hello there").unwrap();
            doc.commit();
            let after = doc.get_heads();
            let patches = doc.diff(&before, &after);
            assert!(!is_group_marker(&patches));
        }
        // phase: a_very_long_name_is_capped_rather_than_stored_whole
        {
            let mut doc = AutoCommit::new();
            name_range(&mut doc, &id(1), None, &"x".repeat(1000));
            doc.commit();
            let ranges = read_named_ranges(&doc, &AuthorDirectory::new());
            assert_eq!(ranges[0].label.chars().count(), MAX_LABEL_LEN);
        }
        // phase: a_record_under_a_key_that_is_not_a_change_id_is_skipped
        {
            let mut doc = AutoCommit::new();
            let rec = doc
                .put_object(automerge::ROOT, format!("{GROUPS_KEY}junk"), ObjType::Map)
                .unwrap();
            doc.put(&rec, "label", "nowhere").unwrap();
            doc.commit();
            assert!(read_named_ranges(&doc, &AuthorDirectory::new()).is_empty());
        }
    }

    #[test]
    fn group_edits_rewrite_bands_atomically_and_refuse_stale_input() {
        // phase: split_merge_and_boundary_move_rewrite_named_bands_atomically
        {
            let mut doc = AutoCommit::new();
            let original = name_range(&mut doc, &id(1), Some(&id(8)), "Whole task");
            doc.commit();

            let (left, right) =
                split_named_range(&mut doc, &original, &id(4), &id(5), "Research", "Write")
                    .expect("existing band splits");
            doc.commit();
            let split = read_named_ranges(&doc, &AuthorDirectory::new());
            assert_eq!(split.len(), 2);
            assert!(split.iter().any(|range| range.label == "Research"));
            assert!(split.iter().any(|range| range.label == "Write"));

            let (left, right) = move_named_boundary(&mut doc, &left, &right, &id(3), &id(4))
                .expect("adjacent bands move");
            doc.commit();
            let moved = read_named_ranges(&doc, &AuthorDirectory::new());
            assert!(moved.iter().any(|range| {
                range.id == left && range.start == id(1) && range.end == Some(id(3))
            }));
            assert!(moved.iter().any(|range| {
                range.id == right && range.start == id(4) && range.end == Some(id(8))
            }));

            let merged = merge_named_ranges(
                &mut doc,
                &[left, right],
                &id(1),
                Some(&id(8)),
                "Finished task",
            )
            .expect("two current bands merge");
            doc.commit();
            let ranges = read_named_ranges(&doc, &AuthorDirectory::new());
            assert_eq!(ranges.len(), 1);
            assert_eq!(ranges[0].id, merged);
            assert_eq!(ranges[0].label, "Finished task");
        }
        // phase: correcting_an_automatic_seam_materializes_both_sessions_then_moves_it
        {
            let mut doc = AutoCommit::new();
            let (left, right) = materialize_and_move_boundary(
                &mut doc,
                DisplayedSession {
                    record: None,
                    start: &id(1),
                    end: &id(2),
                    label: "Write Q3 financial report",
                },
                DisplayedSession {
                    record: None,
                    start: &id(3),
                    end: &id(4),
                    label: "Onboard Sarah",
                },
                &id(1),
                &id(2),
            )
            .expect("automatic sessions become correctable shared ranges");
            doc.commit();

            let ranges = read_named_ranges(&doc, &AuthorDirectory::new());
            assert_eq!(ranges.len(), 2);
            assert!(ranges.iter().any(|range| {
                range.id == left
                    && range.label == "Write Q3 financial report"
                    && range.start == id(1)
                    && range.end == Some(id(1))
            }));
            assert!(ranges.iter().any(|range| {
                range.id == right
                    && range.label == "Onboard Sarah"
                    && range.start == id(2)
                    && range.end == Some(id(4))
            }));
        }
        // phase: stale_group_edits_write_nothing
        {
            let mut doc = AutoCommit::new();
            let before = doc.get_heads();
            assert!(
                split_named_range(&mut doc, "tilo:group:missing", &id(1), &id(2), "A", "B")
                    .is_none()
            );
            assert!(
                merge_named_ranges(
                    &mut doc,
                    &["missing-a".into(), "missing-b".into()],
                    &id(1),
                    Some(&id(2)),
                    "Nope"
                )
                .is_none()
            );
            assert_eq!(doc.get_heads(), before);
        }
    }

    #[test]
    fn peers_naming_ranges_converge() {
        // phase: two_peers_naming_the_same_range_converge_on_one_name
        {
            // Same key = map conflict, resolved identically on both sides.
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
        // phase: two_peers_naming_different_ranges_both_survive
        {
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
    }
}
