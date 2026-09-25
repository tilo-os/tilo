//! What changed in a document since a resource was last read.
//!
//! Re-reading a whole resource on every change is fine for a settings record
//! and ruinous for a twenty-megabyte manuscript: one keystroke would cost a
//! read of the whole document. A [`Delta`] names the parts of the document a
//! set of changes touched, so a resource re-reads only those
//! ([`Resource::refresh`](crate::Resource::refresh)). A [`Tracker`] produces
//! deltas for one live document from Automerge's patch log, at a cost
//! proportional to the change rather than to the document.

use std::collections::{BTreeSet, HashMap};

use automerge::{ChangeHash, ObjId, Patch, PatchAction, Prop};

use crate::model::Doc;

/// Above this many patches a delta stops indexing them and asks readers to
/// read everything: a change that large (a whole-document import, a long
/// offline session merging in) costs about as much to index as to re-read.
const MAX_INDEXED_PATCHES: usize = 50_000;

/// How the keys of one map object were touched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Touched {
    /// Keys whose slot was written: a value put, replaced or deleted.
    written: BTreeSet<String>,
    /// Keys holding an object something inside of which changed.
    below: BTreeSet<String>,
    /// The object changed as a whole: a list or text edit, or an unknown
    /// change. Readers re-read it entirely.
    all: bool,
}

impl Touched {
    fn everything() -> Self {
        Self {
            all: true,
            ..Self::default()
        }
    }

    /// Nothing in the object changed.
    pub fn is_empty(&self) -> bool {
        !self.all && self.written.is_empty() && self.below.is_empty()
    }

    /// The object changed in a way that is not keyed: re-read it entirely.
    pub fn is_all(&self) -> bool {
        self.all
    }

    /// How `key` was touched.
    pub fn key(&self, key: &str) -> Touch {
        if self.all || self.written.contains(key) {
            Touch::Written
        } else if self.below.contains(key) {
            Touch::Below
        } else {
            Touch::Untouched
        }
    }

    /// Every key touched in any way, in key order. Empty when [`Self::is_all`].
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        let mut keys: BTreeSet<&str> = self.written.iter().map(String::as_str).collect();
        keys.extend(self.below.iter().map(String::as_str));
        keys.into_iter()
    }

    /// Keys whose slot itself was written.
    pub fn written(&self) -> impl Iterator<Item = &str> {
        self.written.iter().map(String::as_str)
    }
}

/// How one key of a map was touched by a [`Delta`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Touch {
    /// Unchanged: keep what was read.
    Untouched,
    /// The slot holds a new value (or none): read it again.
    Written,
    /// The slot still holds the same object, and something inside it changed:
    /// refresh that object.
    Below,
}

#[derive(Clone, Debug, Default)]
enum Kind {
    #[default]
    Nothing,
    Everything,
    Indexed(HashMap<ObjId, Touched>),
}

/// The parts of a document some set of changes touched. See the module docs.
#[derive(Clone, Debug, Default)]
pub struct Delta {
    kind: Kind,
}

impl Delta {
    /// No change at all.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// An unknown change: every reader re-reads everything.
    #[must_use]
    pub fn everything() -> Self {
        Self {
            kind: Kind::Everything,
        }
    }

    /// The delta Automerge's patches describe.
    #[must_use]
    pub fn from_patches(patches: &[Patch]) -> Self {
        if patches.is_empty() {
            return Self::nothing();
        }
        if patches.len() > MAX_INDEXED_PATCHES {
            return Self::everything();
        }
        let mut index: HashMap<ObjId, Touched> = HashMap::new();
        for patch in patches {
            for (parent, prop) in &patch.path {
                let entry = index.entry(parent.clone()).or_default();
                match prop {
                    Prop::Map(key) => {
                        entry.below.insert(key.clone());
                    }
                    Prop::Seq(_) => entry.all = true,
                }
            }
            let entry = index.entry(patch.obj.clone()).or_default();
            match &patch.action {
                PatchAction::PutMap { key, .. }
                | PatchAction::DeleteMap { key }
                | PatchAction::Conflict {
                    prop: Prop::Map(key),
                }
                | PatchAction::Increment {
                    prop: Prop::Map(key),
                    ..
                } => {
                    entry.written.insert(key.clone());
                }
                _ => entry.all = true,
            }
        }
        Self {
            kind: Kind::Indexed(index),
        }
    }

    /// Nothing changed.
    pub fn is_empty(&self) -> bool {
        matches!(self.kind, Kind::Nothing)
    }

    /// The change is unknown: readers must re-read everything.
    pub fn is_everything(&self) -> bool {
        matches!(self.kind, Kind::Everything)
    }

    /// How the object `obj` was touched.
    pub fn under(&self, obj: &ObjId) -> Touched {
        match &self.kind {
            Kind::Nothing => Touched::default(),
            Kind::Everything => Touched::everything(),
            Kind::Indexed(index) => index.get(obj).cloned().unwrap_or_default(),
        }
    }

    /// Whether anything at or below `obj` changed.
    pub fn touches(&self, obj: &ObjId) -> bool {
        match &self.kind {
            Kind::Nothing => false,
            Kind::Everything => true,
            Kind::Indexed(index) => index.get(obj).is_some_and(|touched| !touched.is_empty()),
        }
    }

    /// How `key` of the map `obj` was touched.
    pub fn key(&self, obj: &ObjId, key: &str) -> Touch {
        match &self.kind {
            Kind::Nothing => Touch::Untouched,
            Kind::Everything => Touch::Written,
            Kind::Indexed(index) => index
                .get(obj)
                .map_or(Touch::Untouched, |touched| touched.key(key)),
        }
    }
}

/// Produces the [`Delta`] of one live document between successive reads.
///
/// It owns the document's diff cursor: Automerge then logs each applied
/// operation, and the delta since the last read costs as much as the change,
/// however large the document. Should anything else move or reset that cursor
/// (an empty stamped commit does), the next delta is
/// [`Delta::everything`] — correct, just not incremental.
#[derive(Debug, Default)]
pub struct Tracker {
    heads: Option<Vec<ChangeHash>>,
    /// The document's diff cursor was set at `heads` by this tracker.
    armed: bool,
}

impl Tracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start tracking from the document as it stands, for a state that was
    /// just read from it.
    pub fn start(&mut self, doc: &mut Doc) {
        let heads = doc.get_heads();
        doc.update_diff_cursor();
        self.heads = Some(heads);
        self.armed = true;
    }

    /// Track a document from `heads`, the version a state was read at by
    /// someone who could only read the document. The first delta arms the
    /// cursor; a document that moved in between reads as
    /// [`Delta::everything`].
    #[must_use]
    pub fn at(heads: Vec<ChangeHash>) -> Self {
        Self {
            heads: Some(heads),
            armed: false,
        }
    }

    /// The heads the last delta ended at.
    pub fn heads(&self) -> Option<&[ChangeHash]> {
        self.heads.as_deref()
    }

    /// What changed since the previous call (or [`Self::start`]). The first
    /// call of a tracker that was not started is [`Delta::everything`].
    pub fn delta(&mut self, doc: &mut Doc) -> Delta {
        let heads = doc.get_heads();
        let delta = match self.heads.as_ref() {
            Some(previous) if *previous == heads => {
                if !self.armed {
                    doc.update_diff_cursor();
                    self.armed = true;
                }
                return Delta::nothing();
            }
            Some(previous) if self.armed && doc.diff_cursor() == *previous => {
                Delta::from_patches(&doc.diff(previous, &heads))
            }
            // Never read, or the cursor moved under us.
            _ => Delta::everything(),
        };
        doc.update_diff_cursor();
        self.armed = true;
        self.heads = Some(heads);
        delta
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT, ReadDoc};

    #[test]
    fn a_splice_touches_the_path_to_its_text_and_nothing_else() {
        let mut doc = Doc::new();
        let blocks = doc.put_object(ROOT, "blocks", ObjType::Map).unwrap();
        let mut texts = Vec::new();
        for id in ["a", "b", "c"] {
            let block = doc.put_object(&blocks, id, ObjType::Map).unwrap();
            texts.push(doc.put_object(&block, "text", ObjType::Text).unwrap());
        }
        doc.commit();
        let mut tracker = Tracker::new();
        tracker.start(&mut doc);

        doc.splice_text(&texts[1], 0, 0, "hello").unwrap();
        let delta = tracker.delta(&mut doc);
        assert_eq!(delta.key(&ROOT, "blocks"), Touch::Below);
        let under = delta.under(&blocks);
        assert_eq!(under.keys().collect::<Vec<_>>(), ["b"]);
        assert_eq!(under.key("b"), Touch::Below);
        assert_eq!(under.key("a"), Touch::Untouched);
        assert!(delta.under(&texts[1]).is_all());
        assert!(!delta.touches(&texts[0]));

        assert!(tracker.delta(&mut doc).is_empty());

        doc.delete(&blocks, "c").unwrap();
        let delta = tracker.delta(&mut doc);
        assert_eq!(delta.under(&blocks).key("c"), Touch::Written);
        assert_eq!(doc.text(&texts[1]).unwrap(), "hello");
    }

    #[test]
    fn a_moved_cursor_degrades_to_everything() {
        let mut doc = Doc::new();
        doc.put(ROOT, "x", 1).unwrap();
        doc.commit();
        let mut tracker = Tracker::new();
        tracker.start(&mut doc);
        doc.put(ROOT, "x", 2).unwrap();
        doc.commit();
        doc.reset_diff_cursor();
        assert!(tracker.delta(&mut doc).is_everything());
        doc.put(ROOT, "x", 3).unwrap();
        let delta = tracker.delta(&mut doc);
        assert_eq!(delta.key(&ROOT, "x"), Touch::Written);
    }
}
