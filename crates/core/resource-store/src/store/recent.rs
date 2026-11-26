//! The documents of resources' content in the order of the newest version each holds: what a
//! page of the newest versions ([`super::Store::newest`]) reads as far as the page goes, rather
//! than every document the store holds.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use automerge::Automerge;
use collab_types::history::Touches;

use super::content_resource;

/// A document's place: its newest version's place in the order of
/// [`collab_types::history::Version::order`].
type At = (u64, u64);

/// The content documents in memory, by their newest version, the most recent first. A document
/// written since it was placed is placed again when the order is next read, from the changes it
/// took since ([`Touches::read`]): a write costs its own document, and a read of the order costs
/// the documents written since the last.
pub(super) struct Recent {
    /// The documents written since their place was read, or let go from memory.
    written: BTreeSet<String>,
    /// Every document in memory is to be placed: a store that never read the order, or a
    /// staged copy of one, whose documents came with it unplaced.
    unplaced: bool,
    at: HashMap<String, At>,
    by_newest: BTreeSet<(Reverse<At>, String)>,
}

impl Default for Recent {
    fn default() -> Self {
        Self {
            written: BTreeSet::new(),
            unplaced: true,
            at: HashMap::new(),
            by_newest: BTreeSet::new(),
        }
    }
}

impl Recent {
    /// The document `doc` was written, read in or let go: it is placed again when the order is
    /// next read.
    pub(super) fn written(&mut self, doc: &str) {
        if !self.unplaced && content_resource(doc).is_some() {
            self.written.insert(doc.to_owned());
        }
    }

    /// Places every document written since the order was last read, reading the changes each
    /// took into `touches`, and answers the content documents in memory, the most recent first,
    /// each with its newest version's place.
    pub(super) fn read<'a>(
        &'a mut self,
        docs: &HashMap<String, Arc<Automerge>>,
        touches: &mut HashMap<String, Touches>,
    ) -> impl Iterator<Item = (At, &'a str)> + use<'a> {
        let written: Vec<String> = if std::mem::take(&mut self.unplaced) {
            self.written.clear();
            docs.keys()
                .filter(|doc| content_resource(doc).is_some())
                .cloned()
                .collect()
        } else {
            std::mem::take(&mut self.written).into_iter().collect()
        };
        for doc_id in written {
            if let Some(was) = self.at.remove(&doc_id) {
                self.by_newest.remove(&(Reverse(was), doc_id.clone()));
            }
            let Some(doc) = docs.get(&doc_id) else {
                continue;
            };
            let read = touches.entry(doc_id.clone()).or_default();
            read.read(doc, &doc_id);
            if let Some((at, _, _)) = read.by_newest().next() {
                self.at.insert(doc_id.clone(), at);
                self.by_newest.insert((Reverse(at), doc_id));
            }
        }
        self.by_newest
            .iter()
            .map(|(Reverse(at), doc)| (*at, doc.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;

    fn written(doc: &mut Automerge) {
        let mut txn = doc.transaction();
        txn.put(automerge::ROOT, "x", 1).unwrap();
        txn.commit();
    }

    /// Reading the order reads only the documents written since it was last read: a page of
    /// the newest versions costs what changed, not every document in memory.
    #[test]
    fn the_order_reads_only_what_was_written_since_it_was_read() {
        {
            let mut docs: HashMap<String, Arc<Automerge>> = HashMap::new();
            let mut touches = HashMap::new();
            let mut recent = Recent::default();
            let content = "*/note/a~0".to_owned();
            assert!(content_resource(&content).is_some(), "a content document");
            let mut doc = Automerge::new();
            written(&mut doc);
            docs.insert(content.clone(), Arc::new(doc));
            docs.insert("*".to_owned(), Arc::new(Automerge::new()));
            // What is written in a content document is a version of the resource it holds: the
            // document stands in the order, and a document of resources does not.
            assert_eq!(recent.read(&docs, &mut touches).count(), 1);
            assert!(
                touches.contains_key(&content),
                "the content document was read"
            );
            assert!(
                !touches.contains_key("*"),
                "a document of resources is not in the order"
            );
            touches.clear();
            assert_eq!(
                recent.read(&docs, &mut touches).count(),
                1,
                "it keeps its place"
            );
            assert!(touches.is_empty(), "nothing written since: nothing read");
            recent.written(&content);
            let _ = recent.read(&docs, &mut touches).count();
            assert!(touches.contains_key(&content), "written since: read again");
        }
    }
}
