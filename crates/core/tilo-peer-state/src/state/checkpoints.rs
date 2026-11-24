//! A sealed record's checkpoint (its compaction) is a cache of the writes it stands in for, never
//! an authority of its own.
//!
//! A checkpoint names the writes it covers: each author's last one, and a digest of all of them
//! ([`SealedCompaction`]). A device with the record's keys reads a checkpoint only once it has
//! checked that what the checkpoint holds is what those writes merge to, each decided as the write
//! it is ([`StatePeer::check_opened_write`]), with a refused write and every write built on it left
//! out as every reader leaves them out. A checkpoint that holds anything else is read as its writes
//! merge, never as itself. A device with the record's keys that lacks some of the writes reads the
//! checkpoint only once they are here: they stay beside the checkpoint in the history this device
//! keeps ([`super::backend::PeerDocument::checkpoint_writes`]), and the history the workspace's
//! devices hand each other carries them.
//!
//! Each device with the record's keys keeps that it checked a checkpoint in its own row of the
//! workspace ([`CHECKPOINT_CHECKS_KEY`]), signed by its device. The writes a checkpoint stands in
//! for are dropped only once every device with the record's keys has checked it.

use super::document_capture::OpenedRefusal;
use super::facades::{PeerPublication, PeerWorkspaceMut};
use super::sealed_compaction::{
    CheckOpened, LeftOut, RefusedChanges, UnmergedReason, UnmergedWrite, merge_writes,
};
use super::{DocumentMutation, OpKind, Route, SignedOp, StatePeer, SystemTime, now_ns};
use automerge::transaction::Transactable as _;
use automerge::{ObjType, ROOT, ReadDoc as _, ScalarValue, Value};
use collab_types::SealedCompaction;
use collab_types::signer::DeviceSigner;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use web_time::Duration;

/// Key of the checkpoints each device with the record's keys checked, in the workspace's own document:
/// `{<person>/<device> → {<app>:<record> → postcard CheckpointCheck}}`, a row per device that
/// only that device writes, holding the last checkpoint of each record it checked.
pub const CHECKPOINT_CHECKS_KEY: &str = "checkpoint_checks_v1";

/// One device's word that a checkpoint of a record holds exactly what the writes it stands in
/// for merge to, signed by the device that checked it: any device may carry it, none can make
/// it up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointCheck {
    /// The workspace the record is in.
    pub workspace: String,
    /// The app whose record it is.
    pub app: String,
    /// The record.
    pub record: String,
    /// The checkpoint's signature: which checkpoint was checked.
    #[serde(with = "serde_bytes")]
    pub checkpoint: Vec<u8>,
    /// Each author's last write the checkpoint stands in for.
    pub through: Vec<([u8; 32], u64)>,
    /// The device that checked it.
    pub by: [u8; 32],
    /// When, ns since the epoch, on that device's clock.
    pub at_ns: u64,
    /// `by`'s signature over the rest.
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl CheckpointCheck {
    /// The bytes `by`'s signature covers: the check without its signature.
    fn signed_bytes(&self) -> Result<Vec<u8>, String> {
        let mut unsigned = self.clone();
        unsigned.signature = Vec::new();
        postcard::to_allocvec(&unsigned).map_err(|error| error.to_string())
    }

    /// Sign this check as `signer`'s, which answers at once.
    fn sign(&mut self, signer: &dyn DeviceSigner) -> Result<(), String> {
        self.by = signer.device_public_key();
        let signature = collab_types::signer::sign_now(signer, self.signed_bytes()?)
            .map_err(|error| error.to_string())?;
        self.signature = signature.to_vec();
        Ok(())
    }

    /// Whether this check carries `device`'s signature over exactly its contents.
    #[must_use]
    pub fn signed_by(&self, device: &[u8; 32]) -> bool {
        use ed25519_dalek::Verifier as _;
        if &self.by != device {
            return false;
        }
        let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(device) else {
            return false;
        };
        let Ok(bytes): Result<[u8; 64], _> = self.signature.as_slice().try_into() else {
            return false;
        };
        let Ok(signed) = self.signed_bytes() else {
            return false;
        };
        key.verify(&signed, &ed25519_dalek::Signature::from_bytes(&bytes))
            .is_ok()
    }

    /// Whether it says its device checked a checkpoint standing in for every write `checkpoint`
    /// stands in for.
    fn reaches(&self, checkpoint: &SealedCompaction) -> bool {
        checkpoint.covers.iter().all(|(author, seq)| {
            self.through
                .iter()
                .any(|(through, last)| through == author.as_bytes() && last >= seq)
        })
    }
}

/// The key a device keeps its check of `record` of `app` under in its row.
fn entry_key(app: &str, record: &str) -> String {
    format!("{app}:{record}")
}

/// What a reader merges in a record's checkpoint's place, and what of the writes it stands in for
/// the workspace's rules refuse.
#[derive(Clone)]
pub(super) struct CheckpointRead {
    /// The record as the writes the checkpoint stands in for merge, when the checkpoint holds
    /// something else: `None` when it holds exactly that, and is read as itself.
    pub(super) instead: Option<DocumentMutation>,
    /// The writes it stands in for that the workspace's rules refuse, and those built on them.
    pub(super) refused: Vec<UnmergedWrite>,
    /// Their changes: a write after the checkpoint that builds on one is left out with them.
    pub(super) refused_changes: Vec<automerge::ChangeHash>,
}

impl CheckpointRead {
    /// Read the checkpoint as itself: every device with the record's keys checked it, and its
    /// writes are gone.
    const fn itself() -> Self {
        Self {
            instead: None,
            refused: Vec::new(),
            refused_changes: Vec::new(),
        }
    }

    /// Whether the checkpoint holds exactly what its writes merge to.
    pub(super) const fn holds_its_writes(&self) -> bool {
        self.instead.is_none()
    }

    /// The refused writes a reader names: none when the checkpoint holds exactly what its writes
    /// merge to, which folded them away as any compaction folds a refused write, and those it
    /// stands in for otherwise.
    pub(super) fn named(&self) -> &[UnmergedWrite] {
        if self.holds_its_writes() {
            &[]
        } else {
            &self.refused
        }
    }

    /// Leave out, in `refusing`, the changes of the writes it stands in for that the workspace's
    /// rules refuse: a write after it built on one is left out with them.
    pub(super) fn refuse_in(&self, refusing: &mut RefusedChanges) {
        refusing.extend(self.refused_changes.iter().copied());
    }
}

/// Why a reader does not read a record's checkpoint here and now.
pub(super) enum CheckpointRefusal {
    /// Some of the writes it stands in for are not here, and not every device with the record's
    /// keys has checked it: they are fetched and read in its place once here.
    Waits,
    /// This device cannot decide it here and now.
    CannotDecide(String),
}

/// What a reader of a record whose checkpoint lacks the writes it stands in for is told.
pub(crate) const WAITS_FOR_ITS_WRITES: &str =
    "the record's checkpoint is read once the writes it stands in for are here";

impl std::fmt::Display for CheckpointRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Waits => f.write_str(WAITS_FOR_ITS_WRITES),
            Self::CannotDecide(why) => f.write_str(why),
        }
    }
}

impl From<CheckpointRefusal> for OpenedRefusal {
    fn from(refusal: CheckpointRefusal) -> Self {
        Self::CannotDecide(refusal.to_string())
    }
}

/// Whether `op` is the first op of a checkpoint: a compaction, not one of its later segments.
pub(super) fn is_checkpoint(op: &SignedOp) -> bool {
    op.kind == OpKind::Compaction && !collab_types::is_compaction_segment(op)
}

/// Whether `op` is a later segment of the checkpoint `wrapper` describes.
fn segment_of(op: &SignedOp, wrapper: &SealedCompaction) -> bool {
    collab_types::is_compaction_segment(op)
        && wrapper
            .segments
            .contains(&collab_types::segment_hash(&op.payload))
}

/// How `document`'s leading checkpoint is read, given what it opened to (`opened`): as itself
/// once checked against the writes it stands in for, or as those writes merge when it holds
/// anything else. `open` opens those writes, `check` decides each as the write it is, and
/// `settled` says whether every device with the record's keys has checked a checkpoint, which is then read as
/// itself without its writes.
///
/// # Errors
/// [`CheckpointRefusal::Waits`] while writes it stands in for are not here and it is not
/// settled; [`CheckpointRefusal::CannotDecide`] when a write does not open or this device cannot
/// decide one now.
pub(super) fn read_checkpoint(
    document: &super::Document,
    opened: &DocumentMutation,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    check: &mut CheckOpened<'_>,
    settled: &dyn Fn(&SignedOp) -> bool,
) -> Result<CheckpointRead, CheckpointRefusal> {
    let head = document
        .sealed
        .first()
        .filter(|op| is_checkpoint(op))
        .ok_or_else(|| {
            CheckpointRefusal::CannotDecide("a checkpoint is only ever first in its history".into())
        })?;
    if let Some(read) = verdicts::get(&head.signature.0) {
        return Ok(read);
    }
    let wrapper = SealedCompaction::parse(head)
        .map_err(|error| CheckpointRefusal::CannotDecide(error.to_string()))?;
    let cached: Vec<SignedOp> = document.checkpoint_writes.to_vec();
    let Some(start) = complete_below(&cached, head, &wrapper, settled).map(|below| match below {
        Below::Start => None,
        Below::At(index) => Some(index),
    }) else {
        if settled(head) {
            return Ok(CheckpointRead::itself());
        }
        return Err(CheckpointRefusal::Waits);
    };
    let read = merged_writes(document, &cached, &wrapper, start, opened, open, check)?;
    verdicts::put(head.signature.0, &read);
    Ok(read)
}

/// The checkpoint `cached` starts with when it is one: the writes a checkpoint stands in for are
/// dropped only once every device with the record's keys checked the one before them, which then
/// leads what is kept ([`super::backend::PeerDocument::fold_sealed`]).
fn leading_checked(cached: &[SignedOp]) -> Option<usize> {
    cached.first().filter(|op| is_checkpoint(op)).map(|_| 0)
}

/// Where the history a checkpoint stands in for begins: at the record's start, or at a kept
/// checkpoint by its place in `cached`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Below {
    Start,
    At(usize),
}

/// Whether `cached` holds every write `checkpoint` (described by `wrapper`) stands in for, down to
/// a checkpoint every device with the record's keys checked or to the record's start: that
/// checkpoint, by its place in `cached`, [`Below::Start`], and `None` while a write is missing.
///
/// A checkpoint stands in for the history its drafter had: the checkpoint before it, with its
/// segments, and the writes after that one. The digest it names is of exactly those, whatever order
/// they reached this device in. A checkpoint before it that not every device with the record's keys
/// checked is read through its own writes, checked the same way.
fn complete_below(
    cached: &[SignedOp],
    checkpoint: &SignedOp,
    wrapper: &SealedCompaction,
    settled: &dyn Fn(&SignedOp) -> bool,
) -> Option<Below> {
    let heads: Vec<(usize, SealedCompaction)> = cached
        .iter()
        .enumerate()
        .filter(|(_, op)| is_checkpoint(op))
        .filter_map(|(index, op)| {
            tilo_failure::ignored!(
                "a kept op that does not parse as a checkpoint stands in for nothing",
                SealedCompaction::parse(op).ok()
            )
            .map(|parsed| (index, parsed))
        })
        .collect();
    let trusted = leading_checked(cached);
    let (mut op, mut current) = (checkpoint, wrapper.clone());
    loop {
        // The checkpoint this one folded: the latest kept of those it stands in for.
        let below = heads
            .iter()
            .filter(|(index, _)| {
                cached
                    .get(*index)
                    .is_some_and(|kept| kept.signature != op.signature && current.covers(kept))
            })
            .max_by_key(|(index, _)| *index);
        let direct: Vec<&SignedOp> = cached
            .iter()
            .filter(|kept| {
                kept.signature != op.signature
                    && !segment_of(kept, &current)
                    && current.covers(kept)
                    && below.is_none_or(|(index, under)| {
                        cached
                            .get(*index)
                            .is_some_and(|first| first.signature == kept.signature)
                            || segment_of(kept, under)
                            || !under.covers(kept)
                    })
            })
            .collect();
        if collab_types::hash_sealed_prefix(direct).ok()? != current.prefix_hash {
            return None;
        }
        let Some((index, under)) = below else {
            return Some(Below::Start);
        };
        let kept = cached.get(*index)?;
        if trusted == Some(*index) || settled(kept) {
            return Some(Below::At(*index));
        }
        op = kept;
        current = under.clone();
    }
}

/// What the writes `wrapper`'s checkpoint stands in for merge to over the record's cleartext base,
/// from `start` (a checkpoint every device with the record's keys checked, by its place in
/// `cached`, read as itself), each decided by `check`: the checkpoint is read as itself when it
/// holds exactly that, and as that otherwise.
fn merged_writes(
    document: &super::Document,
    cached: &[SignedOp],
    wrapper: &SealedCompaction,
    start: Option<usize>,
    opened: &DocumentMutation,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    check: &mut CheckOpened<'_>,
) -> Result<CheckpointRead, CheckpointRefusal> {
    let cannot = |why: String| CheckpointRefusal::CannotDecide(why);
    let from = start
        .and_then(|index| cached.get(index))
        .map(|first| {
            SealedCompaction::parse(first)
                .map(|parsed| (first, parsed))
                .map_err(|error| cannot(error.to_string()))
        })
        .transpose()?;
    let mut transfers = crate::document_frame::Transfers::default();
    let mut writes = Vec::new();
    let mut refused = RefusedChanges::default();
    let mut named = Vec::new();
    for (index, op) in cached.iter().enumerate() {
        let of_start = from.as_ref().is_some_and(|(first, parsed)| {
            first.signature == op.signature || segment_of(op, parsed)
        });
        if !of_start {
            // A checkpoint not every device with the record's keys checked is read through its own
            // writes, which are kept with it; a write before `start` is in it already.
            if op.kind == OpKind::Compaction
                || !wrapper.covers(op)
                || from.as_ref().is_some_and(|(_, parsed)| parsed.covers(op))
            {
                continue;
            }
        }
        let Some(mutation) = transfers
            .take(*op.author.as_bytes(), &open(op).map_err(cannot)?)
            .map_err(cannot)?
        else {
            continue;
        };
        if of_start {
            writes.push((index, mutation));
            continue;
        }
        match check(op, &mutation) {
            Ok(()) => writes.push((index, mutation)),
            Err(OpenedRefusal::Refused(refusal)) => {
                refused.refuse(&mutation);
                named.push(UnmergedWrite {
                    author: *op.author.as_bytes(),
                    seq: op.seq,
                    reason: UnmergedReason::Refused {
                        refusal: refusal.reason,
                    },
                });
            }
            Err(OpenedRefusal::CannotDecide(why)) => return Err(cannot(why)),
        }
    }
    if transfers.waits_for_changes() {
        return Err(cannot(
            "a segmented write the checkpoint stands in for is incomplete".into(),
        ));
    }
    let (mut merged, left) = merge_writes(
        (*document.doc).clone(),
        writes,
        LeftOut::Named,
        None,
        &mut refused,
    )
    .map_err(cannot)?;
    for (index, reason, _) in left {
        match reason {
            UnmergedReason::AlreadyHeld { .. } => continue,
            // A write whose base is not here yet is no refusal: the checkpoint is read once the
            // base arrives, never in place of what it holds.
            UnmergedReason::MissingDependencies { .. } => return Err(CheckpointRefusal::Waits),
            _ => {}
        }
        if let Some(op) = cached.get(index) {
            named.push(UnmergedWrite {
                author: *op.author.as_bytes(),
                seq: op.seq,
                reason,
            });
        }
    }
    let mut expected = merged.get_heads();
    expected.sort();
    let holds = match opened {
        DocumentMutation::Snapshot(bytes) => tilo_failure::ignored!(
            "a checkpoint that does not load holds nothing its writes merge to",
            tilo_automerge_isolate::load(bytes).ok()
        )
        .is_some_and(|mut claimed| {
            let mut heads = claimed.get_heads();
            heads.sort();
            heads == expected
        }),
        DocumentMutation::Incremental(_) => false,
    };
    Ok(CheckpointRead {
        instead: (!holds).then(|| DocumentMutation::Snapshot(merged.save())),
        refused: named,
        refused_changes: refused.hashes(),
    })
}

/// What reading a checkpoint answered, by its signature, for the readers of this process: the
/// answer is the same every time all its writes are here, and reading every write a checkpoint
/// stands in for again on each read cost what the checkpoint is there to save.
mod verdicts {
    use super::CheckpointRead;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use tilo_lock::MutexRecover as _;

    /// The checkpoints answered at most: past it, the answers start again.
    const KEPT: usize = 512;

    fn answered() -> &'static Mutex<HashMap<[u8; 64], CheckpointRead>> {
        static ANSWERED: OnceLock<Mutex<HashMap<[u8; 64], CheckpointRead>>> = OnceLock::new();
        ANSWERED.get_or_init(Mutex::default)
    }

    pub(super) fn get(signature: &[u8; 64]) -> Option<CheckpointRead> {
        answered().lock_recover().get(signature).cloned()
    }

    pub(super) fn put(signature: [u8; 64], read: &CheckpointRead) {
        let mut answered = answered().lock_recover();
        if answered.len() >= KEPT {
            answered.clear();
        }
        answered.insert(signature, read.clone());
    }
}

impl StatePeer {
    /// The checks of `record` of `app` the workspace keeps, each its device's own and signed by
    /// it: the last checkpoint each device checked.
    fn checkpoint_checks(&self, app: &str, record: &str) -> Vec<CheckpointCheck> {
        let Some(doc) = self.core.workspace_doc.as_deref() else {
            return Vec::new();
        };
        let Ok(Some((Value::Object(ObjType::Map), rows))) = doc.get(ROOT, CHECKPOINT_CHECKS_KEY)
        else {
            return Vec::new();
        };
        let entry = entry_key(app, record);
        let mut checks = Vec::new();
        for row in doc.keys(&rows) {
            let Some((_, device)) = super::device_rows::person_and_device(&row) else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), entries))) = doc.get(&rows, row.as_str())
            else {
                continue;
            };
            let Ok(Some((Value::Scalar(value), _))) = doc.get(&entries, entry.as_str()) else {
                continue;
            };
            let ScalarValue::Bytes(bytes) = value.as_ref() else {
                continue;
            };
            let Ok(check) = postcard::from_bytes::<CheckpointCheck>(bytes) else {
                continue;
            };
            if check.signed_by(&device)
                && check.workspace == self.core.workspace
                && check.app == app
                && check.record == record
            {
                checks.push(check);
            }
        }
        checks
    }

    /// The devices with the keys of `record` of `app`: who checks its checkpoints. They are the
    /// devices the workspace's current membership and key access give the record, judged at the
    /// time its governance records ([`Self::key_access_now`]): a device that was removed,
    /// revoked, or one the record no longer reaches is not among them, so its silence holds
    /// nothing back, and every device holding that governance waits on the same set.
    fn checkpoint_checkers(&self, app: &str, record: &str) -> BTreeSet<[u8; 32]> {
        let now = self.key_access_now();
        tilo_failure::ignored!(
            "a workspace whose devices with the record's keys are not known here has none whose checks settle anything",
            self.document_key_holders(app, record, now).ok()
        )
        .unwrap_or_default()
    }

    /// The instant the devices with a record's keys are counted at: the time of the newest change
    /// the workspace's governance records, in which its membership and key access stand. Every
    /// device holding that governance reads the same instant, so all of them count the same set.
    fn key_access_now(&self) -> SystemTime {
        self.core
            .workspace_doc
            .as_ref()
            .and_then(super::super::cached_document::CachedDocument::newest_change_secs)
            .and_then(|secs| u64::try_from(secs).ok())
            .and_then(|secs| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)))
            .unwrap_or_else(SystemTime::now)
    }

    /// Whether every device with the keys of `record` of `app` checked `checkpoint` itself: the
    /// writes it stands in for may then go, and it leads what this device keeps of them.
    pub(super) fn checkpoint_checked_by_all(
        &self,
        (app, record): (&str, &str),
        checkpoint: &SignedOp,
    ) -> bool {
        let checkers = self.checkpoint_checkers(app, record);
        if checkers.is_empty() {
            return false;
        }
        let checked: BTreeSet<[u8; 32]> = self
            .checkpoint_checks(app, record)
            .into_iter()
            .filter(|check| check.checkpoint == checkpoint.signature.0)
            .map(|check| check.by)
            .collect();
        checkers.is_subset(&checked)
    }

    /// Whether every device with the keys of `record` of `app` checked a checkpoint standing in for
    /// every write `checkpoint` stands in for: those writes may go from a store.
    fn checkpoint_reached_by_all(
        &self,
        (app, record): (&str, &str),
        checkpoint: &SignedOp,
    ) -> bool {
        let Ok(wrapper) = SealedCompaction::parse(checkpoint) else {
            return false;
        };
        let checkers = self.checkpoint_checkers(app, record);
        if checkers.is_empty() {
            return false;
        }
        let reached: BTreeSet<[u8; 32]> = self
            .checkpoint_checks(app, record)
            .into_iter()
            .filter(|check| check.reaches(&wrapper))
            .map(|check| check.by)
            .collect();
        checkers.is_subset(&reached)
    }

    /// Whether a reader that lacks the writes `checkpoint` of `record` of `app` stands in for reads
    /// it as itself: the devices with its keys checked it, and none that checked this record before
    /// holds another. A device with the record's keys that never checked a checkpoint of the record
    /// (a device that joined since) waits for no check of its own; at least one device besides the
    /// checkpoint's author checked it, and, where the record has keys with devices of more than one
    /// person, one of a person other than its author's.
    pub(super) fn checkpoint_settled(
        &self,
        (app, record): (&str, &str),
        checkpoint: &SignedOp,
    ) -> bool {
        let checkers = self.checkpoint_checkers(app, record);
        let checks = self.checkpoint_checks(app, record);
        let checked: BTreeSet<[u8; 32]> = checks
            .iter()
            .filter(|check| check.checkpoint == checkpoint.signature.0)
            .map(|check| check.by)
            .collect();
        let checked_before: BTreeSet<[u8; 32]> = checks.iter().map(|check| check.by).collect();
        if checkers
            .iter()
            .any(|checker| checked_before.contains(checker) && !checked.contains(checker))
        {
            return false;
        }
        let author = *checkpoint.author.as_bytes();
        let person = |device: &[u8; 32]| {
            self.core
                .certificates
                .get(device)
                .map(|cert| cert.user_public_key)
        };
        let checked_by_others: Vec<&[u8; 32]> = checked
            .iter()
            .filter(|device| **device != author && checkers.contains(*device))
            .collect();
        if checked_by_others.is_empty() {
            return false;
        }
        let authors_person = person(&author);
        let others_hold = checkers
            .iter()
            .any(|checker| person(checker).is_some_and(|of| Some(of) != authors_person));
        !others_hold
            || checked_by_others
                .iter()
                .any(|device| person(device).is_some_and(|of| Some(of) != authors_person))
    }
}

impl PeerPublication<'_> {
    /// Check the checkpoint leading the sealed history of `record` of `app` against the writes it
    /// stands in for, opened with `open`: whether it holds exactly what they merge to. `None`
    /// while the history has no checkpoint, writes it stands in for are not here, or this device
    /// cannot decide one now.
    pub fn check_leading_checkpoint(
        self,
        (app, record): (&str, &str),
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    ) -> Option<(SignedOp, bool)> {
        let document = self.peer.core.documents.get(&Route::of(app, record))?;
        let head = document
            .sealed
            .first()
            .filter(|op| is_checkpoint(op))?
            .clone();
        let wrapper = SealedCompaction::parse(&head).ok()?;
        let pieces: Vec<SignedOp> = document
            .sealed
            .iter()
            .take_while(|op| op.signature == head.signature || segment_of(op, &wrapper))
            .cloned()
            .collect();
        let mut transfers = crate::document_frame::Transfers::default();
        let mut opened = None;
        for piece in &pieces {
            opened = transfers
                .take(*piece.author.as_bytes(), &open(piece).ok()?)
                .ok()?;
        }
        let opened = opened?;
        // Its writes are checked here, never taken on the word of other devices with the record's
        // keys.
        let read = read_checkpoint(
            document,
            &opened,
            open,
            &mut |write, mutation| self.peer.check_opened_write((app, record), write, mutation),
            &|_| false,
        )
        .ok()?;
        Some((head, read.holds_its_writes()))
    }

    /// Whether every device with the keys of `record` of `app` checked a checkpoint standing in for
    /// every write the checkpoint `author` signed at `seq` stands in for: a store drops those
    /// writes then. The record's leading checkpoint is that one or a later one standing in for it.
    #[must_use]
    pub fn checkpoint_reached(
        self,
        (app, record): (&str, &str),
        (author, seq): ([u8; 32], u64),
    ) -> bool {
        let Some(document) = self.peer.core.documents.get(&Route::of(app, record)) else {
            return false;
        };
        let Some(head) = document.sealed.first().filter(|op| is_checkpoint(op)) else {
            return false;
        };
        let Ok(wrapper) = SealedCompaction::parse(head) else {
            return false;
        };
        // A checkpoint is numbered as the last write of its author's it folds, 0 when it folds
        // none (a host that only compacts): it leads as itself, or a later checkpoint folds it.
        let leads_as_itself = *head.author.as_bytes() == author && head.seq == seq;
        (leads_as_itself
            || wrapper
                .covered(&collab_types::Ed25519PublicKey::from_bytes(author))
                .is_some_and(|last| last >= seq))
            && self.peer.checkpoint_reached_by_all((app, record), head)
    }

    /// The checkpoint leading the sealed history of `record` of `app`, as this copy holds it.
    #[must_use]
    pub fn leading_checkpoint(self, (app, record): (&str, &str)) -> Option<SignedOp> {
        self.peer
            .core
            .documents
            .get(&Route::of(app, record))?
            .sealed
            .first()
            .filter(|op| is_checkpoint(op))
            .cloned()
    }

    /// Whether the workspace keeps `device`'s own check of a checkpoint of `record` of `app`: the
    /// word a device with the record's keys gave for one.
    #[must_use]
    pub fn checkpoint_checked_by(self, device: &[u8; 32], app: &str, record: &str) -> bool {
        self.peer
            .checkpoint_checks(app, record)
            .iter()
            .any(|check| &check.by == device)
    }

    /// The writes the leading checkpoint of `record` of `app` stands in for that this device
    /// keeps beside it, each as its author signed it.
    #[must_use]
    pub fn checkpoint_writes(self, app: &str, record: &str) -> Vec<SignedOp> {
        self.peer
            .core
            .documents
            .get(&Route::of(app, record))
            .map(|kept| kept.checkpoint_writes.to_vec())
            .unwrap_or_default()
    }
}

impl PeerWorkspaceMut<'_> {
    /// Keep that this device, `caller`, checked `checkpoint`, the checkpoint leading `record` of
    /// `app`, and found it holds exactly what the writes it stands in for merge to: in its own
    /// row of the workspace, signed by `key`, the device's own. Answers whether the workspace
    /// changed: a check already kept is kept once.
    ///
    /// # Errors
    /// `key` is not `caller`'s, `caller` is no current reader of the workspace, `checkpoint` is
    /// no checkpoint, or the check cannot be committed.
    pub fn record_checkpoint_check(
        self,
        caller: &tilo_user_identity::DeviceCert,
        (app, record): (&str, &str),
        checkpoint: &SignedOp,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let wrapper = SealedCompaction::parse(checkpoint).map_err(|error| error.to_string())?;
        let Some(mut candidate) = self.own_row_candidate(caller, key, now)? else {
            return Ok(false);
        };
        let mut check = CheckpointCheck {
            workspace: self.peer.core.workspace.clone(),
            app: app.to_owned(),
            record: record.to_owned(),
            checkpoint: checkpoint.signature.0.to_vec(),
            through: wrapper
                .covers
                .iter()
                .map(|(author, seq)| (*author.as_bytes(), *seq))
                .collect(),
            by: caller.device_public_key,
            at_ns: now_ns(now),
            signature: Vec::new(),
        };
        let entry = entry_key(app, record);
        let row =
            super::device_rows::person_row_key(&caller.user_public_key, &caller.device_public_key);
        let rows = match candidate.get(ROOT, CHECKPOINT_CHECKS_KEY).map_err(err)? {
            Some((Value::Object(ObjType::Map), rows)) => rows,
            // A workspace founded before checks were kept makes their map with the first one.
            _ => candidate
                .put_object(ROOT, CHECKPOINT_CHECKS_KEY, ObjType::Map)
                .map_err(err)?,
        };
        let entries = match candidate.get(&rows, row.as_str()).map_err(err)? {
            Some((Value::Object(ObjType::Map), entries)) => entries,
            _ => candidate
                .put_object(&rows, row.as_str(), ObjType::Map)
                .map_err(err)?,
        };
        if let Ok(Some((Value::Scalar(value), _))) = candidate.get(&entries, entry.as_str())
            && let ScalarValue::Bytes(bytes) = value.as_ref()
            && tilo_failure::ignored!(
                "a kept check that does not parse is not this checkpoint's",
                postcard::from_bytes::<CheckpointCheck>(bytes)
            )
            .is_ok_and(|kept| kept.checkpoint == check.checkpoint)
        {
            return Ok(false);
        }
        check.sign(key)?;
        let bytes = postcard::to_allocvec(&check).map_err(err)?;
        candidate
            .put(&entries, entry.as_str(), ScalarValue::Bytes(bytes))
            .map_err(err)?;
        super::commit_governance(&mut candidate, super::now_secs(now));
        let candidate = crate::cached_document::CachedDocument::written_on(
            self.peer.core.workspace_doc.as_ref(),
            candidate,
        );
        self.peer.core.workspace_doc = Some(candidate);
        Ok(true)
    }
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// Whether `bytes`, a value a device keeps in its row of [`CHECKPOINT_CHECKS_KEY`], is a check
/// of that device's own.
pub(crate) fn kept_by(bytes: &[u8], device: &[u8; 32]) -> bool {
    postcard::from_bytes::<CheckpointCheck>(bytes)
        .is_ok_and(|check| check.by == *device && check.signed_by(device))
}
