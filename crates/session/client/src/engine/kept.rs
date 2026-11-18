//! The sealed documents an engine keeps opened between reads.
//!
//! A sealed document is read by opening its history with the reader's keys and merging it. What
//! a read opened is kept, so the next read opens only what was sealed since: this holds it, for
//! every workspace of the engine together.
//!
//! Nothing here is bounded by a number of documents. A server holds as many documents as its people
//! read, and a bound by count let go of documents in use on a busy one, each then opened whole
//! again by its next read. What bounds it is memory, by memory's own measure: the bytes each
//! document holds opened ([`tilo_peer_state::KeptDocument::held_bytes`]). Past that the document
//! read longest ago goes first, and never the one just read.

use std::collections::BTreeMap;

/// The most bytes of opened history an engine keeps between reads. It protects this device's
/// memory and nothing else: an opened document takes a multiple of the bytes of its history, and
/// an engine that kept every document it ever read would grow with all its people ever read.
/// It holds four of the largest document a read allows, and some two thousand documents with a
/// history of a hundred kilobytes. A document let go is read again by opening its history once.
pub(super) const KEPT_BYTES: usize = 256 * 1024 * 1024;

/// What the kept documents hold, as [`KeptOpened::keep`] says it each time that has doubled or
/// documents went: numbers only, for the log a site's real measure is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Holding {
    /// How many documents are kept opened.
    pub(super) documents: usize,
    /// The bytes they hold together.
    pub(super) bytes: usize,
    /// The bytes they may hold together.
    pub(super) allowed: usize,
    /// How many documents were let go for memory since this engine started.
    pub(super) let_go: u64,
}

/// What is kept opened of each document: `T` under the document's key `K`.
pub(super) struct KeptOpened<K, T> {
    documents: BTreeMap<K, Kept<T>>,
    /// The kept documents by the read that used each last, the one longest ago first.
    by_use: BTreeMap<u64, K>,
    /// How many times a document was kept here: the number of the latest read.
    reads: u64,
    /// The bytes the kept documents hold together.
    bytes: usize,
    /// The bytes they may hold together.
    budget: usize,
    /// How many documents were let go for memory.
    let_go: u64,
    /// The bytes held, and the documents let go, at which [`Self::keep`] next says what is held.
    say_at: (usize, u64),
}

struct Kept<T> {
    opened: T,
    bytes: usize,
    read: u64,
}

impl<K: Ord + Clone, T> Default for KeptOpened<K, T> {
    fn default() -> Self {
        Self::within(KEPT_BYTES)
    }
}

impl<K: Ord + Clone, T> KeptOpened<K, T> {
    /// Nothing kept yet, and at most `budget` bytes to keep.
    pub(super) fn within(budget: usize) -> Self {
        Self {
            documents: BTreeMap::new(),
            by_use: BTreeMap::new(),
            reads: 0,
            bytes: 0,
            budget,
            let_go: 0,
            say_at: (1, 1),
        }
    }

    /// What is kept of `key`, taken out for the read that uses it: [`Self::keep`] puts back what
    /// that read holds of it. A read of the same document meanwhile finds nothing kept.
    pub(super) fn take(&mut self, key: &K) -> Option<T> {
        let kept = self.documents.remove(key)?;
        self.by_use.remove(&kept.read);
        self.bytes = self.bytes.saturating_sub(kept.bytes);
        Some(kept.opened)
    }

    /// Keep `opened`, which holds `bytes`, of `key` as the document read last. When the documents
    /// then hold more than this may, those read longest ago go until they do not, and never
    /// this one: a document in use is not let go for being large, or for where its key sorts.
    /// Nothing is ever refused for it: a document let go is opened again by its next read.
    ///
    /// Answers what is held when that is news: the bytes held have doubled since it last
    /// answered, or as many documents again were let go.
    pub(super) fn keep(&mut self, key: K, opened: T, bytes: usize) -> Option<Holding> {
        drop(self.take(&key));
        self.reads = self.reads.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        self.by_use.insert(self.reads, key.clone());
        self.documents.insert(
            key,
            Kept {
                opened,
                bytes,
                read: self.reads,
            },
        );
        while self.bytes > self.budget && self.documents.len() > 1 {
            let Some((_, longest_ago)) = self.by_use.pop_first() else {
                break;
            };
            if let Some(gone) = self.documents.remove(&longest_ago) {
                self.bytes = self.bytes.saturating_sub(gone.bytes);
                self.let_go = self.let_go.saturating_add(1);
            }
        }
        let (at_bytes, at_let_go) = self.say_at;
        let grew = self.bytes >= at_bytes;
        let went = self.let_go >= at_let_go;
        if !grew && !went {
            return None;
        }
        self.say_at = (
            if grew {
                self.bytes.saturating_mul(2)
            } else {
                at_bytes
            },
            if went {
                self.let_go.saturating_mul(2)
            } else {
                at_let_go
            },
        );
        Some(Holding {
            documents: self.documents.len(),
            bytes: self.bytes,
            allowed: self.budget,
            let_go: self.let_go,
        })
    }

    /// Let go of every document whose key does not stay.
    pub(super) fn retain(&mut self, mut stays: impl FnMut(&K) -> bool) {
        let gone: Vec<K> = self
            .documents
            .keys()
            .filter(|key| !stays(key))
            .cloned()
            .collect();
        for key in gone {
            drop(self.take(&key));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Holding, KeptOpened};

    /// The document whose key sorts first is read between every other document a busy server
    /// reads: it stays kept after ten times the 64 documents an engine once kept, where the first
    /// key in sorted order was the one let go.
    #[test]
    fn a_document_in_use_stays_kept_whatever_number_of_others_is_read_beside_it() {
        let mut kept = KeptOpened::<u32, &str>::within(1024 * 1024);
        kept.keep(0, "first", 100);
        for other in 1..=640 {
            kept.keep(other, "other", 100);
            let first = kept.take(&0);
            assert_eq!(
                first,
                Some("first"),
                "the document in use was let go once {other} others were read"
            );
            kept.keep(0, "first", 100);
        }
        // And every one of the others is kept beside it: together they hold less than allowed.
        for other in 1..=640 {
            assert_eq!(kept.take(&other), Some("other"), "document {other} is kept");
        }
    }

    #[test]
    fn past_its_memory_the_document_read_longest_ago_goes_and_never_the_one_just_read() {
        let mut kept = KeptOpened::<&str, u8>::within(100);
        kept.keep("a", 1, 40);
        kept.keep("b", 2, 40);
        // `a` is read again, so `b` is the one read longest ago.
        let a = kept.take(&"a").unwrap();
        kept.keep("a", a, 40);
        kept.keep("c", 3, 40);
        assert_eq!(kept.take(&"b"), None, "the document read longest ago went");
        assert_eq!(
            kept.take(&"a"),
            Some(1),
            "not the first key in sorted order"
        );
        assert_eq!(kept.take(&"c"), Some(3));

        // A document larger than everything allowed is kept while it is the one just read.
        let mut kept = KeptOpened::<&str, u8>::within(100);
        kept.keep("small", 1, 40);
        kept.keep("large", 2, 500);
        assert_eq!(kept.take(&"small"), None);
        assert_eq!(kept.take(&"large"), Some(2));
    }

    #[test]
    fn what_is_taken_out_or_let_go_no_longer_counts_against_what_is_kept() {
        let mut kept = KeptOpened::<&str, u8>::within(100);
        kept.keep("a", 1, 60);
        kept.keep("b", 2, 60);
        assert_eq!(kept.take(&"a"), None, "both together are more than allowed");
        // `b` is taken out for a read: what follows has all that is allowed.
        assert_eq!(kept.take(&"b"), Some(2));
        kept.keep("c", 3, 60);
        kept.keep("d", 4, 30);
        kept.retain(|key| *key != "c");
        kept.keep("e", 5, 60);
        assert_eq!(kept.take(&"c"), None);
        assert_eq!(
            kept.take(&"d"),
            Some(4),
            "the bytes `c` held were given back"
        );
        assert_eq!(kept.take(&"e"), Some(5));
        // The read that took `b` out ends: whatever was kept meanwhile, `b` is kept again.
        kept.keep("f", 6, 90);
        kept.keep("b", 2, 60);
        assert_eq!(
            kept.take(&"b"),
            Some(2),
            "a read under way's document never goes"
        );
    }

    /// What a site's real measure is read from: said when the bytes held have doubled and when
    /// documents went, in numbers alone.
    #[test]
    fn it_says_what_it_holds_when_that_has_doubled_and_when_some_went() {
        let mut kept = KeptOpened::<u32, u8>::within(1000);
        let holding = |documents, bytes, let_go| {
            Some(Holding {
                documents,
                bytes,
                allowed: 1000,
                let_go,
            })
        };
        assert_eq!(kept.keep(1, 0, 100), holding(1, 100, 0));
        assert_eq!(
            kept.keep(2, 0, 50),
            None,
            "150 bytes: not twice the 100 said"
        );
        assert_eq!(kept.keep(3, 0, 50), holding(3, 200, 0));
        // More than allowed: the document read longest ago goes, and that is said.
        assert_eq!(kept.keep(4, 0, 900), holding(3, 1000, 1));
        assert_eq!(
            kept.keep(5, 0, 50),
            holding(3, 1000, 2),
            "as many again went"
        );
        assert_eq!(kept.keep(6, 0, 50), None, "three went: said again at four");
        assert_eq!(kept.take(&4), Some(0), "what went was read longest ago");
    }
}
