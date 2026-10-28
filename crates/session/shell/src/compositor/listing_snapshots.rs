//! The listings `actions_enumerate` pages through.
//!
//! A paged listing is read once, on its first page, and its continuations
//! are cut from that reading for as long as its cursor: rows an app
//! publishes or a reply that lands in between show up on the next listing,
//! never as a cursor gone stale mid-listing. A cursor whose listing is no
//! longer held (it was let go of, or the session restarted) is answered from
//! a fresh reading, and is stale only when that reading differs.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use crate::shell::search::action_api::ActionCatalog;
use crate::shell::test_control::ListingRequest;

/// How many listings are held beyond their first page at once. A listing
/// paged to its end lets its reading go; this bounds the ones callers left
/// part-way, the oldest let go first.
const HELD_LISTINGS: usize = 8;

/// What a listing was asked for, beyond which page: a cursor continues
/// only the listing it was cut from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListingKey {
    query: String,
    include_programmatic: bool,
    kinds: Vec<String>,
    files: bool,
}

impl ListingKey {
    pub(crate) fn new(query: &str, include_programmatic: bool, page: &ListingRequest) -> Self {
        Self {
            query: query.to_owned(),
            include_programmatic,
            kinds: page.kinds.clone(),
            files: page.files,
        }
    }
}

/// One listing's reading: its rows, arranged as they are paged, and
/// whether it was complete when read.
#[derive(Clone)]
pub(crate) struct ListingReading {
    pub(crate) catalog: Rc<ActionCatalog>,
    pub(crate) complete: bool,
    pub(crate) fingerprint: u64,
}

struct Held {
    key: ListingKey,
    reading: ListingReading,
}

/// The readings of the listings in flight.
#[derive(Default)]
pub(crate) struct ListingSnapshots {
    held: RefCell<VecDeque<Held>>,
}

impl ListingSnapshots {
    /// The reading the listing `key` identified by `fingerprint` pages
    /// through, if it is held.
    pub(crate) fn find(&self, key: &ListingKey, fingerprint: u64) -> Option<ListingReading> {
        self.held
            .borrow()
            .iter()
            .find(|held| held.key == *key && held.reading.fingerprint == fingerprint)
            .map(|held| held.reading.clone())
    }

    /// Keep `reading` for the rest of its listing's pages.
    pub(crate) fn hold(&self, key: ListingKey, reading: ListingReading) {
        let mut held = self.held.borrow_mut();
        held.retain(|held| !(held.key == key && held.reading.fingerprint == reading.fingerprint));
        if held.len() >= HELD_LISTINGS {
            held.pop_front();
        }
        held.push_back(Held { key, reading });
    }

    /// Let go of a listing whose last page was answered.
    pub(crate) fn release(&self, key: &ListingKey, fingerprint: u64) {
        self.held
            .borrow_mut()
            .retain(|held| !(held.key == *key && held.reading.fingerprint == fingerprint));
    }
}
