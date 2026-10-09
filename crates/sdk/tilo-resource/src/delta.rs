//! What changed in a document since a resource was last read.
//!
//! Re-reading a whole resource on every change is fine for a settings record
//! and ruinous for a twenty-megabyte manuscript: one keystroke would cost a
//! read of the whole document. A [`Delta`] names the parts of the document a
//! set of changes touched, so a resource re-reads only those
//! ([`Resource::refresh`](crate::Resource::refresh)). A [`Tracker`] produces
//! deltas for one live document from the operations of the changes that
//! arrived, at a cost proportional to the change rather than to the
//! document.

use std::collections::{BTreeSet, HashMap};

use automerge::{ChangeHash, ObjId, Patch, PatchAction, Prop, ReadDoc};

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

/// Where an object sits: the path from the root to it, as `(parent, prop)`
/// pairs, root first — what a state already knowing an object answers
/// without asking the document (see [`Resource::locate`](crate::Resource::locate)).
pub type ObjPath = Vec<(ObjId, Prop)>;

/// Above this many new changes, or operations in them, a delta is
/// [`Delta::everything`]: reading the change would cost about as much as
/// reading the resource.
const MAX_CHANGES: usize = 4_096;
const MAX_OPS: u64 = 200_000;

fn exid(doc: &Doc, obj: &automerge::legacy::ObjectId) -> Option<ObjId> {
    match obj {
        automerge::legacy::ObjectId::Root => Some(automerge::ROOT),
        automerge::legacy::ObjectId::Id(id) => doc.import(&id.to_string()).ok().map(|(id, _)| id),
    }
}

impl Delta {
    /// The delta `changes` make to `doc` (which already holds them), read from
    /// their operations: each touched object, and the path to it. Paths come
    /// from `locate` when it knows the object, from the changes themselves
    /// for objects they create, and otherwise from the document.
    pub fn from_changes(
        doc: &Doc,
        changes: &[automerge::Change],
        locate: &dyn Fn(&ObjId) -> Option<ObjPath>,
    ) -> Self {
        let mut index: HashMap<ObjId, Touched> = HashMap::new();
        let mut created: HashMap<ObjId, (ObjId, Option<String>)> = HashMap::new();
        let mut located: std::collections::HashSet<ObjId> = std::collections::HashSet::new();
        let mut touched = Vec::new();
        for change in changes {
            let actor = change.actor_id().to_hex_string();
            let start = change.start_op().get();
            for (offset, op) in change.decode().operations.iter().enumerate() {
                let Some(obj) = exid(doc, &op.obj) else {
                    continue;
                };
                let key = match &op.key {
                    automerge::legacy::Key::Map(key) => Some(key.to_string()),
                    automerge::legacy::Key::Seq(_) => None,
                };
                let entry = index.entry(obj.clone()).or_default();
                match &key {
                    Some(key) => {
                        entry.written.insert(key.clone());
                    }
                    None => entry.all = true,
                }
                if matches!(op.action, automerge::legacy::OpType::Make(_)) {
                    let counter = start.saturating_add(u64::try_from(offset).unwrap_or(u64::MAX));
                    if let Ok((made, _)) = doc.import(&format!("{counter}@{actor}")) {
                        created.insert(made, (obj.clone(), key));
                    }
                }
                touched.push(obj);
            }
        }
        for obj in touched {
            mark_path(doc, &obj, &created, locate, &mut located, &mut index);
        }
        Self {
            kind: Kind::Indexed(index),
        }
    }
}

/// Mark everything above `obj` as touched below the prop leading to it.
fn mark_path(
    doc: &Doc,
    obj: &ObjId,
    created: &HashMap<ObjId, (ObjId, Option<String>)>,
    locate: &dyn Fn(&ObjId) -> Option<ObjPath>,
    located: &mut std::collections::HashSet<ObjId>,
    index: &mut HashMap<ObjId, Touched>,
) {
    let mut current = obj.clone();
    loop {
        if current == automerge::ROOT || !located.insert(current.clone()) {
            return;
        }
        // An object these changes made hangs off an object they touched.
        if let Some((parent, key)) = created.get(&current) {
            let entry = index.entry(parent.clone()).or_default();
            match key {
                Some(key) => {
                    entry.below.insert(key.clone());
                }
                None => entry.all = true,
            }
            current = parent.clone();
            continue;
        }
        let path: Option<ObjPath> = locate(&current).or_else(|| {
            let parents: Vec<automerge::Parent> = doc.parents(&current).ok()?.collect();
            // An object no longer reachable changes nothing anyone reads.
            if parents.iter().any(|parent| !parent.visible) {
                return None;
            }
            Some(
                parents
                    .into_iter()
                    .rev()
                    .map(|parent| (parent.obj, parent.prop))
                    .collect(),
            )
        });
        for (parent, prop) in path.unwrap_or_default() {
            let entry = index.entry(parent.clone()).or_default();
            match prop {
                Prop::Map(key) => {
                    entry.below.insert(key);
                }
                Prop::Seq(_) => entry.all = true,
            }
            located.insert(parent);
        }
        return;
    }
}

/// Produces the [`Delta`] of one live document between successive reads,
/// from the operations of the changes that arrived in between: its cost
/// follows the change, however large the document.
#[derive(Debug, Default)]
pub struct Tracker {
    heads: Option<Vec<ChangeHash>>,
}

impl Tracker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start tracking from the document as it stands, for a state that was
    /// just read from it.
    pub fn start(&mut self, doc: &mut Doc) {
        self.heads = Some(doc.get_heads());
    }

    /// Track a document from `heads`, the version a state was read at.
    #[must_use]
    pub fn at(heads: Vec<ChangeHash>) -> Self {
        Self { heads: Some(heads) }
    }

    /// The heads the last delta ended at.
    pub fn heads(&self) -> Option<&[ChangeHash]> {
        self.heads.as_deref()
    }

    /// What changed since the previous call (or [`Self::start`]). The first
    /// call of a tracker that was not started is [`Delta::everything`].
    pub fn delta(&mut self, doc: &mut Doc) -> Delta {
        self.delta_located(doc, &|_| None)
    }

    /// [`Self::delta`], with `locate` answering where objects the state
    /// already knows sit, so the document is not asked.
    pub fn delta_located(
        &mut self,
        doc: &mut Doc,
        locate: &dyn Fn(&ObjId) -> Option<ObjPath>,
    ) -> Delta {
        // Closes the open transaction, so its edits are part of the delta.
        doc.get_heads();
        self.committed_delta_located(doc, locate)
    }

    /// [`Self::delta_located`] up to the committed document, read through a
    /// shared reference: edits of a transaction still open come with the
    /// delta after it closes.
    pub fn committed_delta_located(
        &mut self,
        doc: &Doc,
        locate: &dyn Fn(&ObjId) -> Option<ObjPath>,
    ) -> Delta {
        let heads = doc.committed_heads();
        let committed = doc.committed_document();
        let Some(previous) = self.heads.replace(heads.clone()) else {
            return Delta::everything();
        };
        if previous == heads {
            return Delta::nothing();
        }
        // A version this document does not have: it was replaced wholesale.
        if previous
            .iter()
            .any(|hash| committed.get_change_meta_by_hash(hash).is_none())
        {
            return Delta::everything();
        }
        let metas = committed.get_changes_meta(&previous);
        let ops: u64 = metas
            .iter()
            .map(|meta| meta.max_op.saturating_add(1).saturating_sub(meta.start_op))
            .sum();
        if metas.len() > MAX_CHANGES || ops > MAX_OPS {
            return Delta::everything();
        }
        let hashes: Vec<ChangeHash> = metas.iter().map(|meta| meta.hash).collect();
        let mut changes = Vec::with_capacity(hashes.len());
        for hash in &hashes {
            match committed.get_change_by_hash(hash) {
                Some(change) => changes.push(change),
                None => return Delta::everything(),
            }
        }
        Delta::from_changes(doc, &changes, locate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT};

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
    fn a_replaced_document_reads_as_everything_and_new_objects_locate_themselves() {
        let mut doc = Doc::new();
        doc.put(ROOT, "x", 1).unwrap();
        doc.commit();
        let mut tracker = Tracker::new();
        tracker.start(&mut doc);
        let blocks = doc.put_object(ROOT, "blocks", ObjType::Map).unwrap();
        let block = doc.put_object(&blocks, "new", ObjType::Map).unwrap();
        let text = doc.put_object(&block, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "fresh").unwrap();
        doc.commit();
        let delta = tracker.delta_located(&mut doc, &|_| panic!("created objects need no lookup"));
        assert_eq!(delta.key(&ROOT, "blocks"), Touch::Written);
        assert_eq!(delta.under(&blocks).key("new"), Touch::Written);
        assert!(delta.under(&text).is_all());

        let mut other = Doc::new();
        other.put(ROOT, "x", 9).unwrap();
        other.commit();
        let delta = tracker.delta(&mut other);
        assert!(delta.is_everything());
    }

    #[test]
    fn an_edit_located_by_the_state_asks_the_document_nothing() {
        let mut doc = Doc::new();
        let blocks = doc.put_object(ROOT, "blocks", ObjType::Map).unwrap();
        let block = doc.put_object(&blocks, "b", ObjType::Map).unwrap();
        let text = doc.put_object(&block, "text", ObjType::Text).unwrap();
        doc.commit();
        let mut tracker = Tracker::new();
        tracker.start(&mut doc);
        doc.splice_text(&text, 0, 0, "k").unwrap();
        doc.commit();
        let path = vec![
            (ROOT, Prop::Map("blocks".into())),
            (blocks.clone(), Prop::Map("b".into())),
            (block.clone(), Prop::Map("text".into())),
        ];
        let asked = std::cell::Cell::new(0);
        let delta = tracker.delta_located(&mut doc, &|obj| {
            asked.set(asked.get() + 1);
            (*obj == text).then(|| path.clone())
        });
        assert_eq!(asked.get(), 1);
        assert_eq!(delta.under(&blocks).key("b"), Touch::Below);
        assert_eq!(delta.under(&block).key("text"), Touch::Below);
    }
}
