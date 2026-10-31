//! Taking back a change to text: exactly the characters it inserted go, and
//! exactly the ones it deleted come back, wherever they are now, so
//! somebody else typing in the same text keeps their words.
//!
//! The revert's patches name characters by identity
//! ([`write_selected_patches`](crate::state::write_selected_patches) replays
//! them through cursors), so the text around them may have changed in any
//! way. Only one thing makes a text's part of a change somebody else's too:
//! they rewrote it since. Then their text is the newer value, and the revert
//! leaves it standing, as it leaves a field somebody set since. Somebody
//! rewrote the change's text when, in a change made after it (with it in
//! its history; a concurrent change never saw it):
//!
//! - they deleted a character it inserted;
//! - they typed onto what it inserted, inside it or right beside it on the
//!   same line (`Trip` → `Trip to Rome`): a title being set again, or a
//!   sentence being rewritten;
//! - they typed, in the same way, right where it deleted (a title cleared
//!   and typed again).
//!
//! What they typed anywhere else, or on a line of its own beside the
//! change's text, is their own text, and taking the change back leaves it.

use std::collections::{HashMap, HashSet};

use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, Patch, PatchAction, ReadDoc};

/// A character's identity: its cursor, encoded.
type Key = Vec<u8>;

/// What changes after a run did in one text object, by whom (actor hex).
#[derive(Debug, Default)]
pub(super) struct LaterText {
    inserted: HashMap<Key, String>,
    deleted: HashMap<Key, String>,
}

fn is_text(doc: &AutoCommit, obj: &ObjId) -> bool {
    matches!(doc.object_type(obj), Ok(ObjType::Text))
}

/// How many index units `text` spans in `doc`'s text encoding.
fn units(doc: &AutoCommit, text: &str) -> usize {
    match doc.text_encoding() {
        automerge::TextEncoding::Utf8CodeUnit => text.len(),
        automerge::TextEncoding::Utf16CodeUnit => text.encode_utf16().count(),
        automerge::TextEncoding::UnicodeCodePoint | automerge::TextEncoding::GraphemeCluster => {
            text.chars().count()
        }
    }
}

fn key(doc: &AutoCommit, obj: &ObjId, unit: usize, at: Option<&[ChangeHash]>) -> Option<Key> {
    doc.get_cursor(obj, unit, at)
        .ok()
        .map(|cursor| cursor.to_bytes())
}

/// The characters each text patch of one diff (`lo` to `hi`) inserted, as
/// of `hi`, and deleted, as of `lo`. A diff's patches into one sequence are
/// in document order, each indexed as the ones before it left the text: an
/// insertion's units are where they are at `hi`, and a deletion's are where
/// they were at `lo` once the earlier patches' net growth is taken off.
fn each_splice(
    doc: &AutoCommit,
    patches: &[Patch],
    lo: &[ChangeHash],
    hi: &[ChangeHash],
    mut each: impl FnMut(&ObjId, Option<Key>, Option<Key>),
) {
    let mut grown: HashMap<ObjId, (usize, usize)> = HashMap::new();
    for patch in patches {
        if !is_text(doc, &patch.obj) {
            continue;
        }
        let (inserted, deleted) = grown.entry(patch.obj.clone()).or_default();
        match &patch.action {
            PatchAction::SpliceText { index, value, .. } => {
                let width = units(doc, &value.make_string());
                for unit in *index..index.saturating_add(width) {
                    each(&patch.obj, key(doc, &patch.obj, unit, Some(hi)), None);
                }
                *inserted = inserted.saturating_add(width);
            }
            PatchAction::DeleteSeq { index, length } => {
                let from = index.saturating_add(*deleted).saturating_sub(*inserted);
                for unit in from..from.saturating_add(*length) {
                    each(&patch.obj, None, key(doc, &patch.obj, unit, Some(lo)));
                }
                *deleted = deleted.saturating_add(*length);
            }
            _ => {}
        }
    }
}

/// Note what one later change (`lo` to `hi`, its `patches`) by `actor` did
/// in each text object.
pub(super) fn record_later(
    doc: &AutoCommit,
    patches: &[Patch],
    (lo, hi): (&[ChangeHash], &[ChangeHash]),
    actor: &str,
    out: &mut HashMap<ObjId, LaterText>,
) {
    each_splice(doc, patches, lo, hi, |obj, inserted, deleted| {
        let later = out.entry(obj.clone()).or_default();
        if let Some(key) = inserted {
            later
                .inserted
                .entry(key)
                .or_insert_with(|| actor.to_owned());
        }
        if let Some(key) = deleted {
            later.deleted.entry(key).or_insert_with(|| actor.to_owned());
        }
    });
}

/// What the run did in one text object: the characters it inserted, and
/// where it deleted, by the nearest character before the place that it did
/// not insert itself (`None`: the start).
#[derive(Default)]
struct Own {
    inserted: HashSet<Key>,
    deleted_after: HashSet<Option<Key>>,
}

/// The text objects whose part of the run (`forward`, the diff from
/// `before` to `after`) somebody else rewrote since, each with who did.
pub(super) fn rewritten(
    doc: &AutoCommit,
    forward: &[Patch],
    (before, after): (&[ChangeHash], &[ChangeHash]),
    later: &HashMap<ObjId, LaterText>,
) -> HashMap<ObjId, String> {
    let mut own: HashMap<ObjId, Own> = HashMap::new();
    // Where the run deleted, as of `after`: a diff's later patches into a
    // text are all further on, so a deletion's index is its place then.
    let deletions: Vec<(ObjId, usize)> = forward
        .iter()
        .filter(|patch| later.contains_key(&patch.obj) && is_text(doc, &patch.obj))
        .filter_map(|patch| match &patch.action {
            PatchAction::DeleteSeq { index, .. } => Some((patch.obj.clone(), *index)),
            _ => None,
        })
        .collect();
    each_splice(doc, forward, before, after, |obj, inserted, _| {
        if let Some(key) = inserted
            && later.contains_key(obj)
        {
            own.entry(obj.clone()).or_default().inserted.insert(key);
        }
    });
    for (obj, index) in deletions {
        let entry = own.entry(obj.clone()).or_default();
        let mut before_it = index;
        let anchor = loop {
            let Some(unit) = before_it.checked_sub(1) else {
                break None;
            };
            match key(doc, &obj, unit, Some(after)) {
                Some(key) if entry.inserted.contains(&key) => before_it = unit,
                other => break other,
            }
        };
        entry.deleted_after.insert(anchor);
    }
    own.into_iter()
        .filter_map(|(obj, own)| {
            let later = later.get(&obj)?;
            rewriter(doc, &obj, &own, later).map(|actor| (obj, actor))
        })
        .collect()
}

/// Who rewrote the run's part (`own`) of the text `obj` since, if anybody.
fn rewriter(doc: &AutoCommit, obj: &ObjId, own: &Own, later: &LaterText) -> Option<String> {
    if let Some(actor) = own.inserted.iter().find_map(|key| later.deleted.get(key)) {
        return Some(actor.clone());
    }
    // Where each character somebody else typed since is now, when it is
    // still there.
    let mut typed: Vec<(usize, &String)> = later
        .inserted
        .iter()
        .filter_map(|(bytes, actor)| {
            let cursor = automerge::Cursor::try_from(bytes.as_slice()).ok()?;
            let at = doc.get_cursor_position(obj, &cursor, None).ok()?;
            (key(doc, obj, at, None).as_ref() == Some(bytes)).then_some((at, actor))
        })
        .collect();
    if typed.is_empty() {
        return None;
    }
    typed.sort_unstable();
    typed.dedup_by_key(|(at, _)| *at);
    // The live text a unit at a time: each unit holds the character it is
    // part of.
    let live: Vec<char> = doc
        .text(obj)
        .ok()?
        .chars()
        .flat_map(|c| std::iter::repeat_n(c, units(doc, c.encode_utf8(&mut [0; 4]))))
        .collect();
    // Two neighbouring units are on one line unless either is a line break.
    let one_line = |left: Option<usize>, right: Option<usize>| {
        let breaks = |unit: Option<usize>| {
            unit.and_then(|unit| live.get(unit))
                .is_some_and(|c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
        };
        !breaks(left) && !breaks(right)
    };
    let mine = |unit: usize| key(doc, obj, unit, None).is_some_and(|k| own.inserted.contains(&k));
    // Each run of units somebody else typed: its first and past-its-last.
    let mut runs: Vec<(usize, usize, &String)> = Vec::new();
    for (at, actor) in typed {
        match runs.last_mut() {
            Some((_, end, _)) if *end >= at => *end = at.saturating_add(1).max(*end),
            _ => runs.push((at, at.saturating_add(1), actor)),
        }
    }
    runs.into_iter().find_map(|(start, end, actor)| {
        let left = start.checked_sub(1);
        let right = (end < live.len()).then_some(end);
        let last = end.checked_sub(1);
        let onto = (left.is_some_and(mine) && one_line(left, Some(start)))
            || (right.is_some_and(mine) && one_line(last, right));
        let left_key = match left {
            Some(unit) => key(doc, obj, unit, None).map(Some),
            None => Some(None),
        };
        let where_deleted = left_key.is_some_and(|left_key| {
            own.deleted_after.contains(&left_key) && one_line(left, Some(start))
        });
        (onto || where_deleted).then(|| actor.clone())
    })
}
