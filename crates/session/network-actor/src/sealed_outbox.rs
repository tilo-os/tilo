//! This device's sealed writes to a workspace whose records are sealed channels,
//! held until a host of the workspace keeps them.
//!
//! The workspace's governor cannot open a sealed write and never admits it, so
//! the owner's outgoing custody, which the governor's admission retires,
//! does not carry it. A host retains it for the record's channel members
//! instead, and says when it did: the write goes to the host as a commit
//! ([`RelayCoeditSession::commit`]), the host answers once it is durable,
//! and only that answer lets the write go here. Every session of the workspace
//! that starts sends what is still held, so a write sent into a session that
//! ended before its host kept it, one that died unnoticed included, reaches
//! the workspace's next host. A write's commit id is the hash of its bytes: a
//! host keeps a write it is sent twice once, and says so both times.
//!
//! Each write is held on disk as well, in a log of the workspace's own
//! ([`super::held_log`]): a write the device shut down with is held again
//! when the workspace's route is set up ([`SealedOutbox::restore`]), and sent
//! with the rest. Nothing bounds what a workspace holds: a write is never let go
//! before a host keeps it. While the disk cannot take a write, it is held
//! in memory, the workspace says this device cannot keep its writes, and the log
//! takes it as soon as it can ([`RecordKeeper`]).
//!
//! [`RelayCoeditSession::commit`]: tilo_relay_session::RelayCoeditSession::commit

use std::collections::{HashSet, VecDeque};

use neighbour_wire::Retention;
use tilo_desktop_model::WorkspaceId;
use tilo_relay_session::CoeditPayload;

use super::held_log::{HeldLog, HeldWrite};

struct Held {
    op: [u8; 16],
    channel: [u8; 32],
    retention: Retention,
    bytes: Vec<u8>,
    /// A host answered that it did not keep it: it stays held, and is not
    /// on its way to a host until a session sends it again.
    refused: bool,
}

impl Held {
    fn payload(&self) -> CoeditPayload {
        let (channel, body) = (self.channel, self.bytes.clone());
        match self.retention {
            Retention::Checkpoint => CoeditPayload::Checkpoint { channel, body },
            Retention::Delta => CoeditPayload::RetainedDelta { channel, body },
        }
    }
}

/// One workspace's sealed writes no host has said it keeps yet, oldest first.
#[derive(Default)]
pub(super) struct SealedOutbox {
    held: VecDeque<Held>,
}

impl SealedOutbox {
    /// Hold `bytes`, a sealed write retained on `channel` as `retention`,
    /// until a host keeps it, and answer its commit: what to send a session
    /// up now, and the write as the workspace's log holds it when it was not
    /// held yet. A write already held is not held twice.
    pub(super) fn hold(
        &mut self,
        channel: [u8; 32],
        retention: Retention,
        bytes: Vec<u8>,
    ) -> ([u8; 16], CoeditPayload, Option<HeldWrite>) {
        let op = commit_id(&bytes);
        if let Some(held) = self.held.iter().find(|held| held.op == op) {
            return (op, held.payload(), None);
        }
        let record = HeldWrite {
            op,
            channel,
            retention,
            bytes: bytes.clone(),
        };
        let payload = self.push(Held {
            op,
            channel,
            retention,
            bytes,
            refused: false,
        });
        (op, payload, Some(record))
    }

    /// Hold again `writes`, which the workspace's log held when the device
    /// started: the next session of the workspace sends them.
    pub(super) fn restore(&mut self, writes: Vec<HeldWrite>) {
        for write in writes {
            if self.held.iter().any(|held| held.op == write.op) {
                continue;
            }
            self.push(Held {
                op: write.op,
                channel: write.channel,
                retention: write.retention,
                bytes: write.bytes,
                refused: false,
            });
        }
    }

    fn push(&mut self, held: Held) -> CoeditPayload {
        let payload = held.payload();
        self.held.push_back(held);
        payload
    }

    /// A host answered that it did not keep commit `op`: it stays held.
    /// Whether it was held.
    pub(super) fn refused(&mut self, op: [u8; 16]) -> bool {
        self.held
            .iter_mut()
            .find(|held| held.op == op)
            .is_some_and(|held| {
                held.refused = true;
                true
            })
    }

    /// Every write is on its way to a host again: a session that started
    /// sent them all.
    pub(super) fn sent_again(&mut self) {
        for held in &mut self.held {
            held.refused = false;
        }
    }

    /// How many held writes are not yet synced: all of them while no
    /// session of the workspace carries them (`carried` false), else those its
    /// host did not keep. A write sent on a session up now is on its way.
    pub(super) fn unsynced(&self, carried: bool) -> usize {
        if carried {
            self.held.iter().filter(|held| held.refused).count()
        } else {
            self.held.len()
        }
    }

    /// A host keeps commit `op`: it is let go. Whether it was held.
    pub(super) fn kept(&mut self, op: [u8; 16]) -> bool {
        let Some(index) = self.held.iter().position(|held| held.op == op) else {
            return false;
        };
        self.held.remove(index);
        true
    }

    /// Every write still held, as the commit a session that starts sends,
    /// oldest first.
    pub(super) fn commits(&self) -> impl Iterator<Item = ([u8; 16], CoeditPayload)> + '_ {
        self.held.iter().map(|held| (held.op, held.payload()))
    }

    /// Whether every write was kept by a host.
    pub(super) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

/// Keeps held sealed writes in their workspaces' logs ([`HeldLog`]), and lets
/// kept ones out, on a thread of its own and in the order they happened:
/// each entry is made durable, which the network's turn does not wait for.
/// Kept writes of one workspace that come together are one entry.
///
/// An entry the disk does not take (it is full, say) waits, with every
/// later one, and the workspace says this device cannot keep its writes now
/// ([`crate::workspace_sync::cannot_keep`]); the writes stay held in
/// memory meanwhile. The log takes them, in order, at the next entry or the
/// next heartbeat's [`Self::retry`].
pub(super) struct RecordKeeper {
    tx: Option<std::sync::mpsc::Sender<Keep>>,
}

enum Keep {
    Hold(u64, WorkspaceId, HeldWrite),
    /// Hosts kept these writes; with nothing held any more (`true`), the
    /// workspace's log goes.
    Kept(u64, WorkspaceId, Vec<[u8; 16]>, bool),
    Retry,
}

impl Keep {
    fn workspace(&self) -> Option<(u64, WorkspaceId)> {
        match self {
            Self::Hold(ws, workspace, _) | Self::Kept(ws, workspace, _, _) => {
                Some((*ws, *workspace))
            }
            Self::Retry => None,
        }
    }
}

impl RecordKeeper {
    /// A keeper of the logs under `log`; with none, held writes are kept in
    /// memory alone.
    pub(super) fn new(log: Option<HeldLog>) -> Self {
        let Some(log) = log else {
            return Self { tx: None };
        };
        let (tx, rx) = std::sync::mpsc::channel::<Keep>();
        let started = std::thread::Builder::new()
            .name("sealed-records".into())
            .spawn(move || {
                let mut waiting = Waiting::default();
                while let Ok(first) = rx.recv() {
                    waiting.add(first);
                    for next in rx.try_iter() {
                        waiting.add(next);
                    }
                    waiting.keep(&log);
                }
            });
        if let Err(error) = &started {
            tracing::warn!(%error, "held sealed writes are kept in memory only");
        }
        Self {
            tx: started.is_ok().then_some(tx),
        }
    }

    /// Hold `write` in the log of `workspace`, on topic `ws`.
    pub(super) fn hold(&self, ws: u64, workspace: WorkspaceId, write: HeldWrite) {
        self.send(Keep::Hold(ws, workspace, write));
    }

    /// A host keeps `op`: let it out of `workspace`'s log; `empty` when nothing
    /// is held for the workspace any more.
    pub(super) fn kept(&self, ws: u64, workspace: WorkspaceId, op: [u8; 16], empty: bool) {
        self.send(Keep::Kept(ws, workspace, vec![op], empty));
    }

    /// Try again what the disk did not take.
    pub(super) fn retry(&self) {
        self.send(Keep::Retry);
    }

    fn send(&self, keep: Keep) {
        if let Some(tx) = &self.tx
            && tx.send(keep).is_err()
        {
            tracing::warn!("held sealed writes are kept in memory only");
        }
    }
}

/// Entries not durable yet, in order, and the workspaces whose writes the disk
/// does not take now.
#[derive(Default)]
struct Waiting {
    entries: VecDeque<Keep>,
    stalled: HashSet<u64>,
    /// Workspaces whose log was repaired since the keeper started.
    repaired: HashSet<WorkspaceId>,
}

impl Waiting {
    fn add(&mut self, keep: Keep) {
        match keep {
            Keep::Retry => {}
            Keep::Kept(ws, workspace, ops, empty) => {
                // Joined with the kept writes of the same workspace just before.
                if let Some(Keep::Kept(last_ws, last_workspace, last_ops, last_empty)) =
                    self.entries.back_mut()
                    && (*last_ws, *last_workspace) == (ws, workspace)
                {
                    last_ops.extend(ops);
                    *last_empty = empty;
                    return;
                }
                self.entries
                    .push_back(Keep::Kept(ws, workspace, ops, empty));
            }
            keep @ Keep::Hold(..) => self.entries.push_back(keep),
        }
    }

    /// Make the waiting entries durable in order, up to the first the disk
    /// does not take. Whether a workspace's word on keeping its writes changed.
    fn keep(&mut self, log: &HeldLog) -> bool {
        let before = self.stalled.clone();
        self.stalled.clear();
        while let Some(next) = self.entries.front() {
            let Some((_, workspace)) = next.workspace() else {
                self.entries.pop_front();
                continue;
            };
            let repaired = if self.repaired.contains(&workspace) {
                Ok(())
            } else {
                log.repair(workspace)
            };
            let kept = repaired.and_then(|()| match next {
                Keep::Hold(_, _, write) => log.hold(workspace, write),
                Keep::Kept(_, _, ops, empty) => log.kept(workspace, ops.clone(), *empty),
                Keep::Retry => Ok(()),
            });
            if let Err(error) = kept {
                tracing::warn!(%workspace, %error, "this device cannot keep its writes to the workspace on disk now");
                // Every workspace still waiting cannot keep its writes.
                self.stalled.extend(
                    self.entries
                        .iter()
                        .filter_map(|keep| keep.workspace().map(|(ws, _)| ws)),
                );
                break;
            }
            self.repaired.insert(workspace);
            self.entries.pop_front();
        }
        let mut changed = false;
        for ws in before.symmetric_difference(&self.stalled) {
            changed |= crate::workspace_sync::cannot_keep(*ws, self.stalled.contains(ws));
        }
        changed
    }
}

/// The commit id of a sealed write: its bytes' hash, the same on every send.
fn commit_id(bytes: &[u8]) -> [u8; 16] {
    let mut op = [0; 16];
    op.copy_from_slice(&blake3::hash(bytes).as_bytes()[..16]);
    op
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(outbox: &SealedOutbox) -> Vec<[u8; 16]> {
        outbox.commits().map(|(op, _)| op).collect()
    }

    #[test]
    fn a_sealed_write_is_held_until_a_host_keeps_it() {
        let mut outbox = SealedOutbox::default();
        let (op, payload, record) = outbox.hold([1; 32], Retention::Delta, b"edit".to_vec());
        assert!(
            record.is_some(),
            "held for the first time, it goes into the record"
        );
        assert_eq!(
            payload,
            CoeditPayload::RetainedDelta {
                channel: [1; 32],
                body: b"edit".to_vec()
            }
        );
        assert_eq!(sent(&outbox), [op], "sent, but not kept yet");
        assert!(
            !outbox.kept([9; 16]),
            "another write's answer lets nothing go"
        );
        assert_eq!(sent(&outbox), [op]);
        assert!(outbox.kept(op));
        assert!(outbox.is_empty(), "kept by a host, it is let go");
        assert!(!outbox.kept(op), "a second answer finds nothing");
    }

    #[test]
    fn a_session_that_starts_sends_every_write_still_held_in_order() {
        let mut outbox = SealedOutbox::default();
        let (first, _, _) = outbox.hold([1; 32], Retention::Delta, b"first".to_vec());
        let (second, _, _) = outbox.hold([1; 32], Retention::Checkpoint, b"second".to_vec());
        let (third, _, _) = outbox.hold([2; 32], Retention::Delta, b"third".to_vec());
        // The session they went out on ends; the host kept only the second.
        assert!(outbox.kept(second));
        // The next session sends what is still held, as the same commits.
        let resent: Vec<_> = outbox.commits().collect();
        assert_eq!(
            resent,
            [
                (
                    first,
                    CoeditPayload::RetainedDelta {
                        channel: [1; 32],
                        body: b"first".to_vec()
                    }
                ),
                (
                    third,
                    CoeditPayload::RetainedDelta {
                        channel: [2; 32],
                        body: b"third".to_vec()
                    }
                ),
            ]
        );
        // And again on the session after, until a host keeps them.
        assert_eq!(sent(&outbox), [first, third]);
        assert!(outbox.kept(first) && outbox.kept(third));
        assert!(outbox.is_empty());
    }

    #[test]
    fn a_write_held_twice_is_one_commit() {
        let mut outbox = SealedOutbox::default();
        let (op, _, _) = outbox.hold([1; 32], Retention::Delta, b"edit".to_vec());
        let (again, _, record) = outbox.hold([1; 32], Retention::Delta, b"edit".to_vec());
        assert_eq!(op, again, "the same bytes are the same commit");
        assert!(record.is_none(), "and goes into the log once");
        assert_eq!(sent(&outbox), [op]);
        assert!(outbox.kept(op));
        assert!(outbox.is_empty());
    }

    #[test]
    fn writes_the_log_held_at_start_are_held_again_and_count_as_not_yet_synced() {
        let mut outbox = SealedOutbox::default();
        let (op, _, record) = outbox.hold([1; 32], Retention::Delta, b"edit".to_vec());
        let record = record.unwrap();
        // The device starts again: its log still holds the write.
        let mut restarted = SealedOutbox::default();
        restarted.restore(vec![record.clone(), record]);
        assert_eq!(
            sent(&restarted),
            [op],
            "held once, sent on the next session"
        );
        assert_eq!(restarted.unsynced(false), 1, "no session carries it");
        assert_eq!(restarted.unsynced(true), 0, "on its way on a session");
        assert!(restarted.refused(op));
        assert_eq!(restarted.unsynced(true), 1, "the host did not keep it");
        restarted.sent_again();
        assert_eq!(
            restarted.unsynced(true),
            0,
            "sent again on the next session"
        );
        assert!(restarted.kept(op));
        assert_eq!(restarted.unsynced(false), 0, "kept, it is synced");
    }

    #[test]
    fn a_workspace_holds_every_write_no_host_kept_however_many() {
        let mut outbox = SealedOutbox::default();
        let ops: Vec<_> = (0_u32..5000)
            .map(|index| {
                outbox
                    .hold([1; 32], Retention::Delta, index.to_le_bytes().repeat(4096))
                    .0
            })
            .collect();
        assert_eq!(sent(&outbox), ops, "none let go, oldest first");
    }

    #[test]
    fn a_write_the_disk_does_not_take_waits_and_the_workspace_says_so_until_it_does() {
        const WS: u64 = 0x5eed_0b0b;
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("held");
        // Where the logs go is taken: the disk takes nothing.
        std::fs::write(&logs, b"in the way").unwrap();
        let log = HeldLog::new(logs.clone());
        let workspace = WorkspaceId::new();
        let write = |index: u8| HeldWrite {
            op: [index; 16],
            channel: [2; 32],
            retention: Retention::Delta,
            bytes: vec![index; 32],
        };
        let mut waiting = Waiting::default();
        waiting.add(Keep::Hold(WS, workspace, write(1)));
        waiting.add(Keep::Hold(WS, workspace, write(2)));
        assert!(waiting.keep(&log), "the workspace's word changed");
        assert!(crate::workspace_sync::cannot_keep_writes(WS));
        assert_eq!(waiting.entries.len(), 2, "both wait, in order");
        // The disk takes writes again: the next try keeps both.
        std::fs::remove_file(&logs).unwrap();
        waiting.add(Keep::Retry);
        assert!(waiting.keep(&log));
        assert!(!crate::workspace_sync::cannot_keep_writes(WS));
        assert!(waiting.entries.is_empty());
        assert_eq!(log.read(workspace).unwrap(), [write(1), write(2)]);
    }
}
