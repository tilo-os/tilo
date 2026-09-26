//! A widget's set of joined records.
//!
//! A bound component holds one record; some surfaces show many at once — a
//! conversation of messages each owned by its author, a mailbox, a folder of
//! annotations. [`JoinedRecords`] is how one widget holds such a set: the host
//! joins each record's collaboration route, merges remote changes into it,
//! publishes the widget's own edits, and brings the records the widget creates
//! into the room's index so other members can find them.
//!
//! Two kinds of member, decided by who may write:
//!
//! - **Own** — a record this device may write: any record that is not a
//!   comment record, and a comment record whose author is this device
//!   ([`RecordId::comment_author`]). Kept in the widget's [`RecordStore`], so
//!   it persists and has history; its changes are published.
//! - **Followed** — somebody else's comment record. Only its author may write
//!   it, so this device keeps an in-memory replica filled by the author's
//!   changes and never publishes to it: a local write would be refused by
//!   every receiver, and an identity stamp of our own would be one.
//!
//! The set is bounded ([`MAX_JOINED_RECORDS`]): every member is a route, so a
//! surface joins the window of records it shows, not a whole room.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use automerge::AutoCommit;

use super::{RecordCheckout, RecordId, RecordOpenError, RecordStore};

/// Upper bound on one widget's joined records — each one is a route.
pub const MAX_JOINED_RECORDS: usize = 256;

/// The set of records one widget shows and edits; see the module docs.
pub struct JoinedRecords {
    store: RecordStore,
    lifetime: Rc<crate::history_mode::owner::LegacyHistoryLifetime>,
    own: Vec<RecordCheckout>,
    followed: BTreeMap<RecordId, (RecordCheckout, Rc<RefCell<AutoCommit>>)>,
    author: Cell<Option<[u8; 32]>>,
    /// Records this widget created, not yet in the room's index.
    references: RefCell<Vec<RecordCheckout>>,
}

impl std::fmt::Debug for JoinedRecords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinedRecords")
            .field("own", &self.own)
            .field("followed", &self.followed.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Drop for JoinedRecords {
    fn drop(&mut self) {
        // Dropping the set ends custody: the host retires its routes.
        self.lifetime.active.set(false);
    }
}

impl JoinedRecords {
    /// A set over `store`, which holds this widget's own records.
    pub fn new(store: RecordStore) -> Self {
        Self {
            store,
            lifetime: crate::history_mode::owner::LegacyHistoryLifetime::new(),
            own: Vec::new(),
            followed: BTreeMap::new(),
            author: Cell::new(None),
            references: RefCell::new(Vec::new()),
        }
    }

    pub fn store(&self) -> &RecordStore {
        &self.store
    }

    /// The device this app signs with, once collaboration has connected —
    /// the author to name in [`RecordId::for_comment`]. Set by the host.
    pub fn comment_author(&self) -> Option<[u8; 32]> {
        self.author.get()
    }

    /// Set by the host from the collaboration connection. A test stands in
    /// for the host here; setting another device's key gains nothing, as
    /// every receiver checks a comment record's changes against its author.
    pub fn set_comment_author(&self, author: Option<[u8; 32]>) {
        self.author.set(author);
    }

    /// A new comment record of this device's on `parent`, named `local`.
    /// `None` before collaboration has told the app who it is.
    pub fn comment_record(&self, local: &str, parent: &RecordId) -> Option<RecordId> {
        RecordId::for_comment(self.comment_author()?, local, parent)
    }

    /// Whether this device may write `id`.
    pub fn may_write(&self, id: &RecordId) -> bool {
        match id.comment_author() {
            Some(author) => self.comment_author() == Some(author),
            None => true,
        }
    }

    /// Create one of this device's records, initialise it, join it and bring
    /// it into the room's index. Refuses a record this device may not write
    /// and one that already exists.
    ///
    /// # Errors
    ///
    /// The store's refusal, or [`RecordOpenError::Busy`] for a record this
    /// device does not own or a full set.
    pub fn create(
        &mut self,
        checkout: &RecordCheckout,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), RecordOpenError> {
        if !self.may_write(&checkout.id) || self.len() >= MAX_JOINED_RECORDS {
            return Err(RecordOpenError::Busy);
        }
        self.store
            .reserve_record(checkout)?
            .initialize(initialize)?;
        if !self.own.contains(checkout) {
            self.own.push(checkout.clone());
        }
        self.references.borrow_mut().push(checkout.clone());
        Ok(())
    }

    /// Join exactly `wanted` (bounded to [`MAX_JOINED_RECORDS`], in order):
    /// own records open from the store, others are followed. Records no
    /// longer wanted leave the set. Returns the own records that could not be
    /// opened, and why.
    pub fn join(&mut self, wanted: &[RecordCheckout]) -> Vec<(RecordCheckout, RecordOpenError)> {
        let wanted: Vec<&RecordCheckout> = wanted.iter().take(MAX_JOINED_RECORDS).collect();
        let mut refused = Vec::new();
        let mut own = Vec::new();
        let mut followed = BTreeMap::new();
        for checkout in wanted {
            if self.may_write(&checkout.id) {
                if own.contains(checkout) {
                    continue;
                }
                match self.store.try_open_record(checkout) {
                    Ok(_) => own.push(checkout.clone()),
                    Err(error) => refused.push((checkout.clone(), error)),
                }
            } else {
                let replica = self.followed.remove(&checkout.id).map_or_else(
                    || Rc::new(RefCell::new(AutoCommit::new())),
                    |(_, replica)| replica,
                );
                followed.insert(checkout.id.clone(), (checkout.clone(), replica));
            }
        }
        self.own = own;
        self.followed = followed;
        refused
    }

    pub fn len(&self) -> usize {
        self.own.len().saturating_add(self.followed.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, id: &RecordId) -> bool {
        self.followed.contains_key(id) || self.own.iter().any(|c| c.id == *id)
    }

    /// Every joined record, own first.
    pub fn joined(&self) -> Vec<RecordCheckout> {
        self.own
            .iter()
            .cloned()
            .chain(self.followed.values().map(|(checkout, _)| checkout.clone()))
            .collect()
    }

    /// Read a joined record. A followed record reads empty until its
    /// author's changes arrive.
    pub fn read<R>(&self, id: &RecordId, f: impl FnOnce(&AutoCommit) -> R) -> Option<R> {
        if let Some((_, replica)) = self.followed.get(id) {
            return replica.try_borrow().ok().map(|doc| f(&doc));
        }
        self.own
            .iter()
            .any(|c| c.id == *id)
            .then(|| self.store.read(id, f))
            .flatten()
    }

    /// Change one of this device's joined records. `None` for a followed
    /// record, which is its author's alone to change.
    pub fn with<R>(&self, id: &RecordId, f: impl FnOnce(&mut AutoCommit) -> R) -> Option<R> {
        if !self.own.iter().any(|c| c.id == *id) {
            return None;
        }
        self.store.with(id, |doc| {
            let result = f(doc);
            doc.commit();
            result
        })
    }

    /// The own records, as route owners for the host.
    pub(crate) fn owners(&self) -> Vec<crate::history_mode::HistoryOwner> {
        self.own
            .iter()
            .map(|checkout| {
                crate::history_mode::HistoryOwner::legacy(
                    self.store.clone(),
                    checkout.clone(),
                    &self.lifetime,
                )
            })
            .collect()
    }

    /// The followed replicas and the routes they arrive on.
    pub(crate) fn followers(
        &self,
    ) -> Vec<(crate::collaboration::RouteSpec, Rc<RefCell<AutoCommit>>)> {
        let Some((_, package, room)) = self.store.canonical_namespace().or_else(|| {
            self.own
                .first()
                .and_then(|c| self.store.history_namespace(&c.id).ok())
                .map(|(storage, package, room)| (storage.unwrap_or_default(), package, room))
        }) else {
            return Vec::new();
        };
        let Ok(room) = uuid::Uuid::parse_str(&room) else {
            return Vec::new();
        };
        if package.is_empty() {
            return Vec::new();
        }
        self.followed
            .values()
            .map(|(checkout, replica)| {
                (
                    crate::collaboration::RouteSpec::record(
                        package.clone(),
                        room,
                        checkout.id.as_str(),
                    ),
                    Rc::clone(replica),
                )
            })
            .collect()
    }

    /// Records created since the host last brought them into the room.
    pub(crate) fn take_references(&self) -> Vec<RecordCheckout> {
        std::mem::take(&mut *self.references.borrow_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{RecordKind, RecordOwners};
    use automerge::{ROOT, ReadDoc, transaction::Transactable};

    const ROOM: &str = "00000000-0000-0000-0000-000000000042";

    fn store(temp: &tempfile::TempDir) -> (RecordOwners, RecordStore) {
        let mut owners = RecordOwners::new(temp.path().to_path_buf());
        let store = owners.open("so.tilo.joined-test", ROOM).unwrap();
        (owners, store)
    }

    fn parent() -> RecordId {
        RecordId::parse("uuid:4f1c2b0e-8a57-4c1d-9f55-2d1b3c4e5f60").unwrap()
    }

    fn comment(author: [u8; 32], local: &str) -> RecordCheckout {
        RecordCheckout::new(
            RecordId::for_comment(author, local, &parent()).unwrap(),
            RecordKind::new("test.comment"),
        )
    }

    #[test]
    fn own_records_are_created_referenced_and_written_while_others_are_followed() {
        let temp = tempfile::tempdir().unwrap();
        let (_owners, store) = store(&temp);
        let mut joined = JoinedRecords::new(store);
        let (me, them) = ([7; 32], [9; 32]);
        assert!(
            joined.comment_record("a", &parent()).is_none(),
            "no author yet"
        );
        joined.set_comment_author(Some(me));
        let mine = comment(me, "a");
        let theirs = comment(them, "b");
        assert_eq!(joined.comment_record("a", &parent()), Some(mine.id.clone()));

        assert!(matches!(
            joined.create(&theirs, |_| Ok(())),
            Err(RecordOpenError::Busy)
        ));
        joined
            .create(&mine, |doc| {
                doc.put(ROOT, "text", "hello").map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(joined.take_references(), vec![mine.clone()]);
        assert!(joined.take_references().is_empty(), "drained once");

        assert!(joined.join(&[mine.clone(), theirs.clone()]).is_empty());
        assert_eq!(joined.len(), 2);
        assert_eq!(
            joined.read(&mine.id, |doc| doc.get(ROOT, "text").unwrap().is_some()),
            Some(true)
        );
        assert!(
            joined
                .with(&mine.id, |doc| doc.put(ROOT, "text", "edited"))
                .is_some()
        );
        assert!(
            joined.with(&theirs.id, |_| ()).is_none(),
            "a followed record is its author's to change"
        );
        assert_eq!(
            joined.read(&theirs.id, |doc| doc.keys(ROOT).count()),
            Some(0),
            "a followed record reads empty until its author's changes arrive"
        );
        assert_eq!(joined.owners().len(), 1);
        let followers = joined.followers();
        assert_eq!(followers.len(), 1);
        assert_eq!(followers[0].0.record.as_deref(), Some(theirs.id.as_str()));

        joined.join(std::slice::from_ref(&theirs));
        assert!(
            !joined.contains(&mine.id),
            "a record no longer wanted leaves"
        );
        assert!(joined.contains(&theirs.id));
    }

    #[test]
    fn a_set_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let (_owners, store) = store(&temp);
        let mut joined = JoinedRecords::new(store);
        let many: Vec<_> = (0..MAX_JOINED_RECORDS + 10)
            .map(|i| comment([3; 32], &format!("c{i}")))
            .collect();
        joined.join(&many);
        assert_eq!(joined.len(), MAX_JOINED_RECORDS);
    }
}
