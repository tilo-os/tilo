//! How far this device's own writes have gone: a record write is pending until another device
//! that keeps the workspace has taken it in, and refused once the workspace refused it for good.
//!
//! The device's store keeps, per workspace, each record with one of its own writes no such
//! device has answered yet ([`crate::direct::Answered`]), with the newest of those writes' frame:
//! a link that comes up sends it again ([`crate::direct::DirectSync`]), and its answer comes on
//! that link. A write answered as kept is let go, so whatever is not listed has reached another
//! device or is not this device's write. The writes the workspace refused are listed while the
//! device runs, the newest [`REFUSED_KEPT`] of them.
//!
//! A record is one entry however many of its writes wait: a later write of it was built on the
//! earlier ones, so it stands for the record and its changes cover theirs, which is what a view
//! of one of its items asks ([`RecordReach::of_change`]). The log thus holds an entry per record
//! with pending writes, not per write.
//!
//! The list is a log of entries beside the store's segments (`reach/<workspace>`), appended as
//! writes are noted and answered and read back when the workspace is first asked about: a write
//! pending when the device stopped is pending after it starts again, until an answer comes. A
//! reached or refused write is answered in the log, and the log is written again with only what
//! is still pending once it holds much more than that, so an answer never outweighs the list.
//!
//! Every view reads it the same way. The device's workspace service answers an app's ask about
//! one of its records from it, and tells the app's windows that the record moved when it does,
//! so a tilolang view shows it with `reach(r)` and a Rust view reads the same record.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use neighbour_wire::NeighbourMsg;
use tilo_lock::MutexRecover as _;

use crate::direct::Kept;
use crate::join::WorkspaceId;

/// The directory under the store's root that holds each workspace's log.
const REACH: &str = "reach";
/// How many refused writes a workspace's list keeps, the newest.
const REFUSED_KEPT: usize = 256;
/// How many entries a log may hold beyond twice what it still lists before it is written again
/// with only those.
const SLACK: usize = 256;
/// How many records' moves a receiver that has not read them is told of before the oldest go.
const MOVES: usize = 1024;

/// Where one of this device's writes, or a change it carries, stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Reach {
    /// No other device that keeps the workspace has taken it in yet: while they are all
    /// offline, it stays here.
    Pending,
    /// Another device that keeps the workspace took it in; or it is not this device's write,
    /// which reached this device from another.
    Reached,
    /// The workspace refused it for good.
    Refused,
}

/// One of this device's writes of a record, as [`RecordReach`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnWrite {
    /// The hash of the write's frame, as the device that answers it names it.
    pub write: [u8; 32],
    /// Its sequence number, as this device signed it.
    pub seq: u64,
    /// The changes it carries, by hash: what a view of one item of the record asks about.
    pub changes: Vec<[u8; 32]>,
}

/// This device's writes of one record that no other device keeping the workspace has taken in
/// yet, and those the workspace refused, each oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordReach {
    /// Not taken in by another device yet: one entry per record with pending writes, its newest
    /// write standing for the record and its changes naming those of every pending write.
    pub pending: Vec<OwnWrite>,
    /// Refused for good.
    pub refused: Vec<OwnWrite>,
}

impl RecordReach {
    /// Where the record stands as a whole: pending while any write of it is, else refused when
    /// one of it was, else reached.
    #[must_use]
    pub fn state(&self) -> Reach {
        if !self.pending.is_empty() {
            Reach::Pending
        } else if !self.refused.is_empty() {
            Reach::Refused
        } else {
            Reach::Reached
        }
    }

    /// Where the change `change` (its hash) stands: the write that carries it, or reached when
    /// no write listed here carries it.
    #[must_use]
    pub fn of_change(&self, change: &[u8; 32]) -> Reach {
        let carries = |write: &OwnWrite| write.changes.contains(change);
        if self.refused.iter().any(carries) {
            Reach::Refused
        } else if self.pending.iter().any(carries) {
            Reach::Pending
        } else {
            Reach::Reached
        }
    }
}

/// A record whose reach moved: one of this device's writes of it was noted, taken in or refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReachMoved {
    /// The workspace it is of.
    pub workspace: WorkspaceId,
    /// The app whose record it is.
    pub app: String,
    /// The record, as its writes name it.
    pub record: String,
}

/// One write the log names.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Noted {
    write: [u8; 32],
    app: String,
    record: String,
    seq: u64,
    changes: Vec<[u8; 32]>,
    /// The frame, sent again on each link that comes up while it is pending; empty once it is
    /// refused.
    frame: Vec<u8>,
}

impl Noted {
    fn own(&self) -> OwnWrite {
        OwnWrite {
            write: self.write,
            seq: self.seq,
            changes: self.changes.clone(),
        }
    }
}

/// One entry of a workspace's log.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
enum Entry {
    /// This device wrote it, and nobody answered it yet.
    Noted(Noted),
    /// Another device that keeps the workspace took it in.
    Reached([u8; 32]),
    /// The workspace refused it for good.
    Refused([u8; 32]),
}

/// What one workspace's log lists now.
#[derive(Default)]
struct Ledger {
    pending: BTreeMap<[u8; 32], Noted>,
    refused: VecDeque<Noted>,
    /// How many entries the log file holds.
    logged: usize,
}

impl Ledger {
    /// Take `entry` in, as the log holds it.
    fn apply(&mut self, entry: Entry) {
        match entry {
            Entry::Noted(mut noted) => {
                // A later write of a record is built on what an earlier pending write of it
                // carried: the record keeps one entry, whose changes name theirs, so a view of
                // one of its items still reads pending. The later write stands for the record,
                // its frame the one a link sends again; an earlier write only adds the changes
                // it carried.
                let existing = self
                    .pending
                    .iter()
                    .find(|(_, pending)| pending.app == noted.app && pending.record == noted.record)
                    .map(|(write, _)| *write);
                let Some(write) = existing else {
                    self.pending.insert(noted.write, noted);
                    return;
                };
                let Some(mut kept) = self.pending.remove(&write) else {
                    return;
                };
                if noted.seq >= kept.seq {
                    merge_changes(&mut noted.changes, kept.changes);
                    kept = noted;
                } else {
                    merge_changes(&mut kept.changes, noted.changes);
                }
                self.pending.insert(kept.write, kept);
            }
            Entry::Reached(write) => {
                self.pending.remove(&write);
            }
            Entry::Refused(write) => {
                if let Some(mut noted) = self.pending.remove(&write) {
                    noted.frame = Vec::new();
                    self.refused.push_back(noted);
                    while self.refused.len() > REFUSED_KEPT {
                        self.refused.pop_front();
                    }
                }
            }
        }
    }

    /// The entries that list what this lists, and nothing else: only what is still pending,
    /// since an answer already recorded is no part of the list a restart reads back.
    fn entries(&self) -> Vec<Entry> {
        self.pending.values().cloned().map(Entry::Noted).collect()
    }

    /// How many entries list something still pending: one per record with a pending write.
    fn live(&self) -> usize {
        self.pending.len()
    }
}

/// Add to `changes` each change of `more` it does not name already.
fn merge_changes(changes: &mut Vec<[u8; 32]>, more: Vec<[u8; 32]>) {
    for change in more {
        if !changes.contains(&change) {
            changes.push(change);
        }
    }
}

/// Each workspace's list of this device's writes, kept under a store's root.
pub(crate) struct Ledgers {
    ledgers: std::sync::Mutex<BTreeMap<WorkspaceId, Ledger>>,
    moved: tokio::sync::broadcast::Sender<ReachMoved>,
}

impl Default for Ledgers {
    fn default() -> Self {
        Self {
            ledgers: std::sync::Mutex::default(),
            moved: tokio::sync::broadcast::channel(MOVES).0,
        }
    }
}

/// The file `workspace`'s log is kept in, under the store's `root`.
fn log_path(root: &Path, workspace: WorkspaceId) -> PathBuf {
    root.join(REACH).join(workspace.to_string())
}

/// `entry` as the log holds it: its length, then its bytes.
fn framed(entry: &Entry) -> Result<Vec<u8>, postcard::Error> {
    let body = postcard::to_allocvec(entry)?;
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    let mut bytes = Vec::with_capacity(body.len().saturating_add(4));
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(&body);
    Ok(bytes)
}

/// What `workspace`'s log under `root` lists: every whole entry, up to one a crash cut short.
fn load(root: &Path, workspace: WorkspaceId) -> Ledger {
    let mut ledger = Ledger::default();
    let bytes = match std::fs::read(log_path(root, workspace)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return ledger,
        Err(error) => {
            tracing::warn!(%workspace, %error, "this device's list of its unanswered writes could not be read; it starts empty");
            return ledger;
        }
    };
    let mut rest = bytes.as_slice();
    while let Some((len, body)) = rest.split_first_chunk::<4>() {
        let len = usize::try_from(u32::from_le_bytes(*len)).unwrap_or(usize::MAX);
        let Some((entry, after)) = body.split_at_checked(len) else {
            break;
        };
        let Ok(entry) = postcard::from_bytes::<Entry>(entry) else {
            break;
        };
        ledger.apply(entry);
        ledger.logged = ledger.logged.saturating_add(1);
        rest = after;
    }
    ledger
}

impl Ledgers {
    /// `workspace`'s ledger, read from its log under `root` the first time it is asked for.
    fn ledger<'a>(
        ledgers: &'a mut BTreeMap<WorkspaceId, Ledger>,
        root: &Path,
        workspace: WorkspaceId,
    ) -> &'a mut Ledger {
        ledgers
            .entry(workspace)
            .or_insert_with(|| load(root, workspace))
    }

    /// Append `entry` to `workspace`'s log, or write the log again with only what `ledger`
    /// lists once it holds much more than that.
    fn log(root: &Path, workspace: WorkspaceId, ledger: &mut Ledger, entry: &Entry) {
        let path = log_path(root, workspace);
        let written = if ledger.logged > ledger.live().saturating_mul(2).saturating_add(SLACK) {
            let mut bytes = Vec::new();
            let entries = ledger.entries();
            let framed: Result<(), postcard::Error> = entries.iter().try_for_each(|entry| {
                bytes.extend(framed(entry)?);
                Ok(())
            });
            framed
                .map_err(std::io::Error::other)
                .and_then(|()| tilo_files::native::atomic::write_creating_parent(&path, &bytes))
                .map(|()| entries.len())
        } else {
            framed(entry)
                .map_err(std::io::Error::other)
                .and_then(|bytes| {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)?
                        .write_all(&bytes)
                })
                .map(|()| ledger.logged.saturating_add(1))
        };
        match written {
            Ok(logged) => ledger.logged = logged,
            Err(error) => {
                tracing::warn!(%workspace, %error, "this device's list of its unanswered writes was not kept; a restart forgets this entry");
            }
        }
    }

    /// Note `message`, a write this device sends to `workspace`'s other devices, as pending:
    /// nothing but an app's record write is noted, and a write noted already is left as it is.
    /// A write of a record that has one pending is merged into its entry, the newer write
    /// standing for the record.
    pub(crate) fn note(&self, root: &Path, workspace: WorkspaceId, message: &NeighbourMsg) {
        let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return;
        };
        let Ok(frame) = postcard::to_allocvec(message) else {
            return;
        };
        let write = *blake3::hash(&frame).as_bytes();
        // The changes a write carries are named in its claims, in the clear even when its
        // payload is sealed; a write that claims none names them in its payload.
        let mut changes: Vec<[u8; 32]> = envelope.claims.iter().map(|claim| claim.change).collect();
        if changes.is_empty() && envelope.encryption.is_none() {
            changes = crate::state::change_hashes::change_hashes(&envelope.payload);
        }
        let noted = Noted {
            write,
            app: app_id.clone(),
            record: document_id.clone(),
            seq: envelope.seq,
            changes,
            frame,
        };
        {
            let mut ledgers = self.ledgers.lock_recover();
            let ledger = Self::ledger(&mut ledgers, root, workspace);
            if ledger.pending.contains_key(&write) {
                return;
            }
            let entry = Entry::Noted(noted);
            ledger.apply(entry.clone());
            Self::log(root, workspace, ledger, &entry);
        }
        let _ = self.moved.send(ReachMoved {
            workspace,
            app: app_id.clone(),
            record: document_id.clone(),
        });
    }

    /// Take another device's answer to this device's write `write` of `workspace`: kept, it has
    /// reached that device and is let go; refused, it is listed as refused. An answer to a write
    /// not pending here (answered already, not noted, or one a later write of its record stands
    /// for) changes nothing: that record waits for its newer write's answer.
    pub(crate) fn answer(&self, root: &Path, workspace: WorkspaceId, write: [u8; 32], kept: Kept) {
        let moved = {
            let mut ledgers = self.ledgers.lock_recover();
            let ledger = Self::ledger(&mut ledgers, root, workspace);
            let Some(noted) = ledger.pending.get(&write) else {
                return;
            };
            let moved = ReachMoved {
                workspace,
                app: noted.app.clone(),
                record: noted.record.clone(),
            };
            let entry = match kept {
                Kept::Durable => Entry::Reached(write),
                Kept::Refused { .. } => Entry::Refused(write),
            };
            ledger.apply(entry.clone());
            Self::log(root, workspace, ledger, &entry);
            moved
        };
        let _ = self.moved.send(moved);
    }

    /// This device's writes of `app`'s `record` in `workspace` that no other device keeping it
    /// has taken in, and those the workspace refused.
    pub(crate) fn record(
        &self,
        root: &Path,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
    ) -> RecordReach {
        let mut ledgers = self.ledgers.lock_recover();
        let ledger = Self::ledger(&mut ledgers, root, workspace);
        let of_record = |noted: &&Noted| noted.app == app && noted.record == record;
        let mut pending: Vec<OwnWrite> = ledger
            .pending
            .values()
            .filter(of_record)
            .map(Noted::own)
            .collect();
        pending.sort_by_key(|write| write.seq);
        RecordReach {
            pending,
            refused: ledger
                .refused
                .iter()
                .filter(of_record)
                .map(Noted::own)
                .collect(),
        }
    }

    /// The newest write of each record of `workspace` no other device keeping it has taken in,
    /// in the order it wrote them: what each link that comes up sends again, so its answers come.
    /// The store keeps the earlier writes it was built on, which a link takes in as history.
    pub(crate) fn unreached(&self, root: &Path, workspace: WorkspaceId) -> Vec<NeighbourMsg> {
        let mut pending: Vec<(u64, Vec<u8>)> = {
            let mut ledgers = self.ledgers.lock_recover();
            Self::ledger(&mut ledgers, root, workspace)
                .pending
                .values()
                .map(|noted| (noted.seq, noted.frame.clone()))
                .collect()
        };
        pending.sort_by_key(|(seq, _)| *seq);
        pending
            .into_iter()
            .filter_map(|(_, frame)| NeighbourMsg::decode(&frame).ok())
            .collect()
    }

    /// Told each record whose reach moved, in every workspace of the store.
    pub(crate) fn moved(&self) -> tokio::sync::broadcast::Receiver<ReachMoved> {
        self.moved.subscribe()
    }

    /// Forget `workspace`'s list with the workspace.
    pub(crate) fn forget(&self, root: &Path, workspace: WorkspaceId) -> std::io::Result<()> {
        self.ledgers.lock_recover().remove(&workspace);
        match std::fs::remove_file(log_path(root, workspace)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, Ledger, Noted, Reach, RecordReach};

    fn noted_on(record: &str, write: u8, seq: u64) -> Noted {
        Noted {
            write: [write; 32],
            app: "test.app".into(),
            record: record.into(),
            seq,
            changes: vec![[write.wrapping_add(100); 32]],
            frame: vec![write],
        }
    }

    fn noted(write: u8, seq: u64) -> Noted {
        noted_on("tab:1", write, seq)
    }

    #[test]
    fn a_write_is_pending_until_it_is_answered_and_a_refused_one_stays_refused() {
        let mut ledger = Ledger::default();
        ledger.apply(Entry::Noted(noted_on("tab:1", 1, 1)));
        ledger.apply(Entry::Noted(noted_on("tab:2", 2, 2)));
        ledger.apply(Entry::Reached([1; 32]));
        ledger.apply(Entry::Refused([2; 32]));
        assert!(ledger.pending.is_empty());
        assert_eq!(ledger.refused.len(), 1);
        assert!(
            ledger.refused[0].frame.is_empty(),
            "a refused write is not sent again"
        );
        let reach = RecordReach {
            pending: Vec::new(),
            refused: ledger.refused.iter().map(Noted::own).collect(),
        };
        assert_eq!(reach.state(), Reach::Refused);
        assert_eq!(reach.of_change(&[102; 32]), Reach::Refused);
        assert_eq!(reach.of_change(&[101; 32]), Reach::Reached);
    }

    #[test]
    fn a_later_write_of_a_record_stands_for_the_one_entry_it_keeps() {
        let mut ledger = Ledger::default();
        ledger.apply(Entry::Noted(noted(1, 1)));
        ledger.apply(Entry::Noted(noted(2, 2)));
        assert_eq!(
            ledger.live(),
            1,
            "one entry per record, however many of its writes wait"
        );
        let pending: Vec<&Noted> = ledger.pending.values().collect();
        assert_eq!(
            pending[0].write, [2; 32],
            "the later write stands for the record"
        );
        assert_eq!(pending[0].seq, 2);
        assert!(
            pending[0].changes.contains(&[101; 32]),
            "the earlier write's change is still named, so a view of its item reads pending"
        );
        assert!(pending[0].changes.contains(&[102; 32]));
        // The record waits for its later write: an answer to the earlier one is no answer.
        ledger.apply(Entry::Reached([1; 32]));
        assert_eq!(ledger.live(), 1);
        ledger.apply(Entry::Reached([2; 32]));
        assert_eq!(ledger.live(), 0);
        assert!(
            ledger.entries().is_empty(),
            "an answer is no part of the log written again"
        );
    }

    #[test]
    fn a_refused_write_is_listed_while_the_device_runs_but_not_written_again() {
        let mut ledger = Ledger::default();
        ledger.apply(Entry::Noted(noted(1, 1)));
        ledger.apply(Entry::Refused([1; 32]));
        assert_eq!(
            ledger.refused.len(),
            1,
            "a view still reads that the workspace refused it"
        );
        assert_eq!(ledger.live(), 0);
        assert!(
            ledger.entries().is_empty(),
            "an answer already recorded is not written again"
        );
        let mut read_back = Ledger::default();
        for entry in ledger.entries() {
            read_back.apply(entry);
        }
        assert!(read_back.pending.is_empty() && read_back.refused.is_empty());
    }
}
