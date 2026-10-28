//! The persisted records Activity derives history from, read off the
//! compositor loop.
//!
//! Activity joins the workspace tree with every record its rooms hold. A
//! record that is not open on this desktop is read from its app's store on
//! disk: a `stat` to see whether it moved, then up to 16 MiB and an
//! Automerge load when it did. On the loop thread either holds a frame up
//! for the disk, and a peer's edit to a large record lands in the next
//! Activity refresh. [`ColdRecordReads`] serves Activity what a worker last
//! read, as a [`PersistedRecordHistory`] that never touches the disk: a
//! record not read yet is unavailable for now, and one that moved keeps its
//! last-read document until the worker has read the new one. The records
//! Activity asked for go to the worker after a refresh; it stats each and
//! loads only those whose revision moved, and when one did it wakes the
//! loop, which takes the reads in and has Activity refresh again.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use tilo_desktop::activity::PersistedRecordHistory;
use tilo_desktop_model::RoomId;
use ui::record::{RecordCheckout, RecordId, RecordKind};

use crate::shell::room_records::LocalRecordReader;

/// The least time between two passes over the records on disk. Activity
/// refreshes on many frames; the files are looked at no more often.
const PASS_INTERVAL: Duration = Duration::from_millis(500);

/// How many passes a read document is kept while Activity no longer asks
/// for it: a record that left every room Activity shows is let go.
const UNASKED_PASSES: u64 = 8;

/// One record as the worker last read it.
struct Read {
    revision: u64,
    /// The document; `None` when it was corrupt, too large or not this
    /// record at that revision.
    document: Option<AutoCommit>,
    /// The last pass that asked for it.
    asked: u64,
}

/// What a pass found for one record.
enum Found {
    /// Still at the revision it was last read at.
    Unchanged,
    /// Read now, at `revision`.
    Read {
        revision: u64,
        document: Option<Box<AutoCommit>>,
    },
    /// No longer at a readable location.
    Gone,
}

/// One pass: each record to look at, with the revision it was last read at.
struct Pass {
    reader: LocalRecordReader,
    records: Vec<(RecordId, Option<u64>)>,
    /// Rooms whose apps' room documents to list.
    rooms: Vec<RoomId>,
    wake: std::task::Waker,
}

/// What one pass found: the records it looked at, and each listed room's
/// app room documents.
struct PassFound {
    records: Vec<(RecordId, Found)>,
    rooms: Vec<(RoomId, Vec<RecordCheckout>)>,
}

/// The worker thread and what it found.
struct Worker {
    passes: mpsc::Sender<Pass>,
    found: mpsc::Receiver<PassFound>,
}

/// Persisted records as the worker last read them; see the module docs.
#[derive(Default)]
pub(crate) struct ColdRecordReads {
    read: RefCell<HashMap<RecordId, Read>>,
    /// What Activity asked for since the last pass.
    asked: RefCell<BTreeSet<RecordId>>,
    worker: Option<Worker>,
    /// Whether a pass handed to the worker has not been taken in yet.
    reading: bool,
    passes: u64,
    last_pass: Option<Instant>,
    /// Rooms Activity asked for the room documents of since the last pass.
    asked_rooms: RefCell<BTreeSet<RoomId>>,
    /// Each room's app room documents, as the worker last listed them.
    room_documents: RefCell<BTreeMap<RoomId, Vec<RecordCheckout>>>,
}

impl PersistedRecordHistory for ColdRecordReads {
    /// The document as the worker last read it. Never reads the disk.
    fn load_record(&self, id: &RecordId) -> Option<AutoCommit> {
        self.asked.borrow_mut().insert(id.clone());
        self.read.borrow().get(id)?.document.clone()
    }

    /// The revision the served document was read at. Never reads the disk.
    fn revision(&self, id: &RecordId) -> Option<u64> {
        self.asked.borrow_mut().insert(id.clone());
        self.read.borrow().get(id).map(|read| read.revision)
    }

    /// The room's app room documents as the worker last listed them. Never
    /// reads the disk.
    fn room_documents(&self, room: RoomId) -> Vec<RecordCheckout> {
        self.asked_rooms.borrow_mut().insert(room);
        self.room_documents
            .borrow()
            .get(&room)
            .cloned()
            .unwrap_or_default()
    }
}

impl ColdRecordReads {
    /// Take in what the worker read since the last call. Returns whether
    /// what Activity is served changed, so it must refresh again. Never
    /// waits.
    pub(crate) fn take_reads(&mut self) -> bool {
        if !self.reading {
            return false;
        }
        let Some(worker) = &self.worker else {
            self.reading = false;
            return false;
        };
        let found = match worker.found.try_recv() {
            Ok(found) => found,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.worker = None;
                self.reading = false;
                return false;
            }
        };
        self.reading = false;
        let mut changed = false;
        {
            let mut listed = self.room_documents.borrow_mut();
            for (room, documents) in found.rooms {
                if listed.get(&room) != Some(&documents) {
                    listed.insert(room, documents);
                    changed = true;
                }
            }
        }
        let mut read = self.read.borrow_mut();
        for (id, found) in found.records {
            match found {
                Found::Unchanged => {}
                Found::Read { revision, document } => {
                    let asked = read.get(&id).map_or(self.passes, |read| read.asked);
                    read.insert(
                        id,
                        Read {
                            revision,
                            document: document.map(|document| *document),
                            asked,
                        },
                    );
                    changed = true;
                }
                Found::Gone => changed |= read.remove(&id).is_some(),
            }
        }
        changed
    }

    /// Hand the records Activity asked for since the last pass to the
    /// worker, through `reader`. Does nothing while a pass is still being
    /// read, or within [`PASS_INTERVAL`] of the last one: what was asked
    /// waits for the next. Never waits.
    pub(crate) fn request(&mut self, reader: Option<LocalRecordReader>, now: Instant) {
        if self.reading
            || self
                .last_pass
                .is_some_and(|at| now.saturating_duration_since(at) < PASS_INTERVAL)
        {
            return;
        }
        let Some(reader) = reader else {
            return;
        };
        let asked = std::mem::take(&mut *self.asked.borrow_mut());
        let rooms: Vec<RoomId> = std::mem::take(&mut *self.asked_rooms.borrow_mut())
            .into_iter()
            .collect();
        if asked.is_empty() && rooms.is_empty() {
            return;
        }
        self.passes = self.passes.saturating_add(1);
        self.last_pass = Some(now);
        let pass = self.passes;
        let records = {
            let mut read = self.read.borrow_mut();
            for id in &asked {
                if let Some(read) = read.get_mut(id) {
                    read.asked = pass;
                }
            }
            read.retain(|_, read| pass.saturating_sub(read.asked) <= UNASKED_PASSES);
            asked
                .into_iter()
                .map(|id| {
                    let seen = read.get(&id).map(|read| read.revision);
                    (id, seen)
                })
                .collect()
        };
        // Made on the loop's thread, so the pass wakes the loop it is for.
        let wake = crate::shell::search::invocation_wake::waker();
        let sent = self.worker().is_some_and(|worker| {
            worker
                .passes
                .send(Pass {
                    reader,
                    records,
                    rooms,
                    wake,
                })
                .is_ok()
        });
        if sent {
            self.reading = true;
        } else {
            self.worker = None;
        }
    }

    /// Whether a pass is still being read.
    #[cfg(test)]
    pub(crate) fn reading(&self) -> bool {
        self.reading
    }

    fn worker(&mut self) -> Option<&Worker> {
        if self.worker.is_none() {
            let (passes, received) = mpsc::channel::<Pass>();
            let (report, found) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("tilo-activity-records".into())
                .spawn(move || {
                    for pass in received {
                        // Listed first: a room document is read under the
                        // id the listing registers.
                        let rooms: Vec<(RoomId, Vec<RecordCheckout>)> = pass
                            .rooms
                            .iter()
                            .map(|room| {
                                let documents = pass
                                    .reader
                                    .room_documents(*room)
                                    .into_iter()
                                    .map(|document| {
                                        RecordCheckout::new(
                                            document.id,
                                            RecordKind::new("document"),
                                        )
                                    })
                                    .collect();
                                (*room, documents)
                            })
                            .collect();
                        let records = read_pass(&pass.reader, pass.records);
                        let moved = !rooms.is_empty()
                            || records
                                .iter()
                                .any(|(_, found)| !matches!(found, Found::Unchanged));
                        let found = PassFound { records, rooms };
                        if report.send(found).is_err() {
                            break;
                        }
                        // Unchanged reads need no frame: the loop takes them
                        // in whenever it next runs.
                        if moved {
                            pass.wake.wake();
                        }
                    }
                });
            match spawned {
                Ok(_) => self.worker = Some(Worker { passes, found }),
                Err(error) => {
                    tracing::warn!(%error, "activity record reads did not start; records stay unavailable");
                }
            }
        }
        self.worker.as_ref()
    }
}

/// Look at each record `records` names: a `stat`, and the document itself
/// only when its revision moved since it was last read. Blocks on the disk:
/// never on the compositor loop.
fn read_pass(
    reader: &LocalRecordReader,
    records: Vec<(RecordId, Option<u64>)>,
) -> Vec<(RecordId, Found)> {
    #[cfg(test)]
    held::hold(&records);
    records
        .into_iter()
        .map(|(id, seen)| {
            let found = match reader.revision(&id) {
                None => Found::Gone,
                Some(revision) if seen == Some(revision) => Found::Unchanged,
                Some(revision) => Found::Read {
                    revision,
                    document: reader.load(&id).map(Box::new),
                },
            };
            (id, found)
        })
        .collect()
}

/// Test support: hold the passes that read a record until released, as a
/// disk that does not answer would.
#[cfg(test)]
pub(crate) mod held {
    use std::sync::{Mutex, mpsc};

    use ui::record::RecordId;

    static HELD: Mutex<Vec<(RecordId, mpsc::Receiver<()>)>> = Mutex::new(Vec::new());

    /// Hold the next pass that reads `record` until the returned sender is
    /// used or dropped.
    pub(crate) fn hold_record(record: RecordId) -> mpsc::Sender<()> {
        let (release, held) = mpsc::channel();
        HELD.lock().unwrap().push((record, held));
        release
    }

    pub(super) fn hold(records: &[(RecordId, Option<u64>)]) {
        let mut all = HELD.lock().unwrap();
        let held = all
            .iter()
            .position(|(held, _)| records.iter().any(|(id, _)| id == held))
            .map(|index| all.remove(index).1);
        drop(all);
        // Bounded, so a test that never releases fails instead of hanging.
        if let Some(held) = held {
            let _ = held.recv_timeout(std::time::Duration::from_secs(30));
        }
    }
}
