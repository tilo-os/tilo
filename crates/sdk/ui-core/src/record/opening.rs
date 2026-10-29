//! A record read off the frame: a window that can show before its record is decoded opens with
//! the record on its way, and takes it in when it is.
//!
//! Decoding a large record takes long (hundreds of milliseconds at ten thousand items, seconds
//! at a hundred thousand), and a window that waits for it before its first frame shows nothing
//! all that time. A window that can show something before it (what a tilolang app keeps in its
//! index) opens with the record [opening](RecordStore::begin_opening): its owner and its bytes
//! are had at once, and only the decoding runs on a thread of its own. Nothing reads or writes
//! the record meanwhile: it is not [open](RecordStore::try_open_record) until it is
//! [taken in](RecordOpening::take_in), and every open of it takes it in first, waiting for the
//! decoding when it must. A write is therefore never lost or reordered: it waits for the
//! record, then lands on it.

use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};

use automerge::AutoCommit;
use tilo_lock::MutexRecover as _;

use super::{Parked, RecordCheckout, RecordId, RecordOpenError, RecordSet, RecordStore};

type Decoded = Result<app_state::DecodedBytes, app_state::StoreError>;

/// A record whose bytes are being decoded on a thread of their own.
pub(super) struct Opening {
    checkout: RecordCheckout,
    owner: Rc<app_state::DocOwner>,
    decoded: mpsc::Receiver<Decoded>,
    /// The windows to wake when the decoding is done.
    wakes: Arc<Mutex<Vec<crate::WakeHandle>>>,
}

impl RecordSet {
    fn begin_opening(&mut self, want: &RecordCheckout) -> Result<bool, RecordOpenError> {
        // Only the room's own document of a store this set owns: a portable cache is already
        // in memory, and a native room's record comes from its broker read.
        if !want.id.is_package_primary()
            || self.portable.is_some()
            || self.native_initial.is_some()
            || self.open.contains_key(&want.id)
            || self.pending.contains_key(&want.id)
            || self.opening.contains_key(&want.id)
            || !want.is_valid_local_storage_request()
        {
            return Ok(false);
        }
        let (backend, key) = self.canonical_key(want)?;
        if backend.declined_waiting_here() {
            return Ok(false);
        }
        let owner = app_state::DocOwner::acquire_shared(backend, key)?;
        let Some(stored) = owner.stored_bytes(true)? else {
            return Ok(false);
        };
        let (sent, decoded) = mpsc::sync_channel(1);
        let wakes: Arc<Mutex<Vec<crate::WakeHandle>>> = Arc::default();
        let waking = Arc::clone(&wakes);
        crate::background::Background::current()
            .spawn("record-decode", move || {
                let _ = sent.send(stored.decode());
                for wake in waking.lock_recover().drain(..) {
                    wake.wake();
                }
            })
            .map_err(|error| {
                RecordOpenError::Initialization(format!("the record cannot be read: {error}"))
            })?;
        self.opening.insert(
            want.id.clone(),
            Opening {
                checkout: want.clone(),
                owner: Rc::new(owner),
                decoded,
                wakes,
            },
        );
        Ok(true)
    }

    /// The route of the record `want` names while it opens off the frame: the room's own
    /// document, whose route is this set's package and room alone. Its identity is checked when
    /// it is taken in, and nothing reads or writes it before, so naming its route admits no
    /// document. `None` when it is not opening.
    pub(super) fn opening_route(
        &self,
        want: &RecordCheckout,
    ) -> Option<Result<crate::collaboration::RouteSpec, RecordOpenError>> {
        let opening = self.opening.get(&want.id)?;
        Some(if opening.checkout.kind == want.kind {
            self.wire_route(&want.id)
        } else {
            Err(RecordOpenError::IdentityMismatch)
        })
    }

    /// Take in the record `id` was opening, once it is decoded (waiting for it with `wait`):
    /// it is open from then on, as [`Self::try_open_record`] opens it. Whether it is.
    pub(super) fn take_in_opening(
        &mut self,
        id: &RecordId,
        wait: bool,
    ) -> Result<bool, RecordOpenError> {
        let Some(opening) = self.opening.get(id) else {
            return Ok(true);
        };
        let decoded = if wait {
            opening.decoded.recv().ok()
        } else {
            match opening.decoded.try_recv() {
                Ok(decoded) => Some(decoded),
                Err(mpsc::TryRecvError::Empty) => return Ok(false),
                Err(mpsc::TryRecvError::Disconnected) => None,
            }
        };
        let Some(opening) = self.opening.remove(id) else {
            return Ok(false);
        };
        let decoded = decoded.ok_or_else(|| {
            RecordOpenError::Initialization("the record's decoding ended without it".into())
        })??;
        let loaded = opening.owner.decoded(decoded);
        super::validate_identity(loaded.document(), &opening.checkout, &self.home)?;
        let mut doc = opening.owner.accept(loaded)?;
        let autosave = app_state::DocWriter::try_start(&opening.owner)?;
        let history = crate::state::UndoLog::new(&mut doc);
        self.open.insert(
            id.clone(),
            Parked {
                doc: Some(doc),
                kind: opening.checkout.kind.clone(),
                autosave: Some(autosave),
                history,
                action_state: crate::bound_dispatch::RecordActionState::default(),
            },
        );
        Ok(true)
    }
}

impl RecordStore {
    /// Begin opening the record `want` off the frame, when it is the room's own document and
    /// this device holds it: its bytes are read now and decoded on a thread of their own. Until
    /// it is [taken in](RecordOpening::take_in) it is not open, and the window that opened it
    /// shows what it can without it. Whether it began; a record that is not here, or is opened
    /// another way, opens as before.
    ///
    /// # Errors
    /// The record's owner or bytes cannot be had.
    pub fn begin_opening(&self, want: &RecordCheckout) -> Result<bool, RecordOpenError> {
        self.0
            .try_borrow_mut()
            .map_err(|_| RecordOpenError::Busy)?
            .begin_opening(want)
    }

    /// The room these records are kept in: what a window keys what it keeps on this device for
    /// the room by, before any record of it is read.
    #[must_use]
    pub fn home(&self) -> String {
        self.0.borrow().home.clone()
    }

    /// Whether the record `id` is being opened off the frame and is not taken in yet.
    #[must_use]
    pub fn is_opening(&self, id: &RecordId) -> bool {
        self.0
            .try_borrow()
            .is_ok_and(|set| set.opening.contains_key(id))
    }

    /// The record `want` as it opens off the frame.
    #[must_use]
    pub fn opening(&self, want: &RecordCheckout) -> RecordOpening {
        RecordOpening {
            records: self.clone(),
            checkout: want.clone(),
        }
    }
}

/// A record opening off the frame ([`RecordStore::begin_opening`]), for the window that shows
/// before it.
#[derive(Clone)]
pub struct RecordOpening {
    records: RecordStore,
    checkout: RecordCheckout,
}

impl std::fmt::Debug for RecordOpening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordOpening")
            .field("record", &self.checkout.id)
            .finish_non_exhaustive()
    }
}

impl RecordOpening {
    /// The record this is the opening of.
    #[must_use]
    pub fn checkout(&self) -> &RecordCheckout {
        &self.checkout
    }

    /// Have `wake` called when the record is decoded (at once when it already is).
    pub fn wake_when_decoded(&self, wake: crate::WakeHandle) {
        let Ok(set) = self.records.0.try_borrow() else {
            return;
        };
        match set.opening.get(&self.checkout.id) {
            Some(opening) => opening.wakes.lock_recover().push(wake),
            None => wake.wake(),
        }
    }

    /// Take the record in when it is decoded, waiting for it with `wait`. Whether it is open.
    ///
    /// # Errors
    /// The record does not decode, or is not this room's.
    pub fn take_in(&self, wait: bool) -> Result<bool, RecordOpenError> {
        self.records
            .0
            .try_borrow_mut()
            .map_err(|_| RecordOpenError::Busy)?
            .take_in_opening(&self.checkout.id, wait)
    }

    /// The record's document as it is now, waiting for it to be taken in: what a window that
    /// showed before it reads once it needs the record (to write, or to read what it does not
    /// keep). `None` when it cannot be had.
    #[must_use]
    pub fn document(&self) -> Option<AutoCommit> {
        match self.take_in(true) {
            Ok(true) => {}
            Ok(false) => return None,
            Err(error) => {
                tracing::warn!(%error, "the record could not be taken in");
                return None;
            }
        }
        self.records
            .read_checked(&self.checkout.id, AutoCommit::clone)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ROOT, ReadDoc as _};
    use durable_store::{Durability, DurableStore};

    use super::super::{RecordCheckout, RecordKind, RecordOpenError, RecordOwners, RecordStore};

    const PACKAGE: &str = "so.tilo.opening-test";
    const ROOM: &str = "3f2b1c00-0000-4000-8000-0000000000e1";

    fn primary() -> RecordCheckout {
        RecordCheckout::package_primary(RecordKind::new("opening.test"))
    }

    /// The room's own document of a profile at `root`, holding `body` as an earlier session left
    /// it; the store of the room's records over it.
    fn kept(root: &std::path::Path, body: &str) -> RecordStore {
        kept_document(root, body).0
    }

    /// [`kept`], with the document as it was kept.
    fn kept_document(root: &std::path::Path, body: &str) -> (RecordStore, AutoCommit) {
        let store: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(root.to_path_buf()));
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "body", body).unwrap();
        let landing =
            super::super::transfer::install_room_document(&store, PACKAGE, ROOM, &mut doc).unwrap();
        store.flush(None).unwrap();
        assert!(matches!(landing.durability(&*store), Durability::Durable));
        drop(store);
        let records = RecordOwners::new(root.to_path_buf())
            .open(PACKAGE, ROOM)
            .unwrap();
        (records, doc)
    }

    fn body(doc: &AutoCommit) -> String {
        doc.get(ROOT, "body")
            .unwrap()
            .map(|(value, _)| value.as_str().unwrap_or_default().to_owned())
            .unwrap_or_default()
    }

    #[test]
    fn a_record_opening_off_the_frame_is_read_by_nothing_until_it_is_taken_in() {
        let temp = tempfile::tempdir().unwrap();
        let records = kept(temp.path(), "kept");
        let want = primary();
        assert!(records.begin_opening(&want).unwrap());
        assert!(records.is_opening(&want.id));
        assert!(
            matches!(
                records.read_checked(&want.id, |_| ()),
                Err(RecordOpenError::MissingRecord)
            ),
            "nothing reads the record before it is taken in"
        );
        let opening = records.opening(&want);
        let doc = opening.document().expect("the record, once taken in");
        assert_eq!(body(&doc), "kept");
        assert!(!records.is_opening(&want.id));
        // Taken in, it is the open record every write lands on.
        records
            .with(&want.id, |doc| {
                doc.put(ROOT, "body", "written").unwrap();
            })
            .unwrap();
        assert_eq!(records.read_checked(&want.id, body).unwrap(), "written");
    }

    #[test]
    fn opening_a_record_that_is_still_opening_waits_for_it() {
        let temp = tempfile::tempdir().unwrap();
        let records = kept(temp.path(), "kept");
        let want = primary();
        assert!(records.begin_opening(&want).unwrap());
        records.try_open_record(&want).unwrap();
        assert!(!records.is_opening(&want.id));
        assert_eq!(records.read_checked(&want.id, body).unwrap(), "kept");
        // Opened, it is not opened off the frame again.
        assert!(!records.begin_opening(&want).unwrap());
    }

    /// The workspace's join can answer before the window takes the record in: the route's
    /// committed history takes the record in and lands on it.
    #[test]
    fn committed_history_arriving_while_the_record_opens_lands_on_it() {
        let temp = tempfile::tempdir().unwrap();
        let (records, kept) = kept_document(temp.path(), "kept");
        let want = primary();
        assert!(records.begin_opening(&want).unwrap());
        let mut committed = kept.with_actor(automerge::ActorId::random());
        committed.put(ROOT, "from_room", "another device").unwrap();
        records
            .merge_remote_document(
                &want,
                collab_types::DocumentMutation::Snapshot(committed.save()),
            )
            .unwrap();
        assert!(!records.is_opening(&want.id));
        let from_room = records
            .read_checked(&want.id, |doc| {
                doc.get(ROOT, "from_room")
                    .unwrap()
                    .and_then(|(value, _)| value.into_string().ok())
            })
            .unwrap();
        assert_eq!(from_room.as_deref(), Some("another device"));
        assert_eq!(records.read_checked(&want.id, body).unwrap(), "kept");
    }

    #[test]
    fn a_record_this_device_does_not_hold_opens_as_before() {
        let temp = tempfile::tempdir().unwrap();
        let records = RecordOwners::new(temp.path().to_path_buf())
            .open(PACKAGE, ROOM)
            .unwrap();
        assert!(!records.begin_opening(&primary()).unwrap());
        assert!(matches!(
            records.try_open_record(&primary()),
            Err(RecordOpenError::MissingRecord)
        ));
    }
}
