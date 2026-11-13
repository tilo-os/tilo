//! Changes built on a change their own writer never sent.
//!
//! A sealed record's history can hold a write whose changes wait for a change no device will ever
//! send: a page that held a record's keys before the record itself drafted a change in that moment,
//! let it go unsigned and unsent, and then wrote its next change on top of it. Every reader then
//! waited for the lost change forever and was refused the whole record as incomplete.
//!
//! Such a write is told from one whose base is merely still on its way by two things the history
//! itself shows, with no clock involved:
//!
//! - one of its changes does not build on its Automerge writer's previous change through anything
//!   the record or its history holds: the writer's own sequence shows the gap (the record may even
//!   hold another change under that sequence, written after the lost one by the same writer);
//! - every write its signer signed before it is held, or folded into a compaction that covers it:
//!   the signer's own signed sequence shows no gap, so no write it signed earlier, the one that
//!   would carry that previous change, is still on its way.
//!
//! A change is written by the device whose engine made it and travels in that device's writes, in
//! the order it signed them; a device's earlier change therefore travels in one of its earlier
//! writes, and those are all here. Every write that builds on such a write, directly or through
//! another, is stuck behind the same hole: only the writer that lost the change ever applied the
//! changes on top of it, so they are that writer's own follow-ups.
//!
//! A reader reads the record without them; only the workspace's store, which every write of the
//! record reaches, folds them away (see the relay's compaction), so a device that is merely behind
//! never drops a write another may still bring.

use std::collections::{BTreeSet, HashMap, HashSet};

use automerge::{AutoCommit, ChangeHash};
use collab_types::{DocumentMutation, OpKind, SealedCompaction};

/// The signer and signed sequence of one write of a sealed history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Signed {
    pub(super) author: [u8; 32],
    pub(super) seq: u64,
}

/// How far each signer's sequence in a record is held without a gap: every write it signed up to
/// its mark is in the record's history, or folded into what covers it (a compaction at the head of
/// the history, or a governed checkpoint's watermarks).
#[derive(Debug, Default)]
pub(super) struct HeldThrough(HashMap<[u8; 32], u64>);

impl HeldThrough {
    /// The marks of `document`'s history.
    pub(super) fn of(document: &super::Document) -> Self {
        let mut floors: HashMap<[u8; 32], u64> = HashMap::new();
        let mut raise = |author: [u8; 32], seq: u64| {
            let floor = floors.entry(author).or_default();
            *floor = (*floor).max(seq);
        };
        for (author, floor) in &document.covered {
            // A watermark that names no key covers no signer's writes: restore and admission
            // refuse such a record before it is read, and here it only holds less.
            if let Some(author) = tilo_failure::ignored!(
                "a watermark naming no key covers nothing a reader waits on",
                hex::decode(author)
                    .ok()
                    .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            ) {
                raise(author, *floor);
            }
        }
        if let Some(head) = document
            .sealed
            .first()
            .filter(|op| op.kind == OpKind::Compaction)
            // A head that does not parse covers nothing here: admission checked it, and holding
            // less only keeps a write waiting.
            .and_then(|op| {
                tilo_failure::ignored!(
                    "an unparsed head only keeps a write waiting, never drops one",
                    SealedCompaction::parse(op).ok()
                )
            })
        {
            for (author, seq) in &head.covers {
                raise(*author.as_bytes(), *seq);
            }
        }
        let mut held: HashMap<[u8; 32], BTreeSet<u64>> = HashMap::new();
        for op in document.sealed.iter() {
            held.entry(*op.author.as_bytes())
                .or_default()
                .insert(op.seq);
        }
        let mut through = floors;
        for (author, seqs) in held {
            let mark = through.entry(author).or_default();
            for seq in seqs.range(mark.saturating_add(1)..) {
                if *seq != mark.saturating_add(1) {
                    break;
                }
                *mark = *seq;
            }
        }
        Self(through)
    }

    /// Whether every write `signed`'s signer signed before it is held.
    fn holds_all_before(&self, signed: Signed) -> bool {
        let before = signed.seq.saturating_sub(1);
        self.0.get(&signed.author).copied().unwrap_or(0) >= before
    }
}

/// Which of `waiting`, the writes of a history that still wait once every write that could merge
/// over `record` did, are stuck behind a change their own writer never sent (see the module), in
/// `waiting`'s order. A write whose signer is unknown, or that is a whole snapshot, never is.
pub(super) fn behind_unsent(
    record: &AutoCommit,
    waiting: &[(Option<Signed>, &DocumentMutation)],
    held: &HeldThrough,
) -> Vec<bool> {
    let changes: Vec<Vec<automerge::Change>> = waiting
        .iter()
        .map(|(_, mutation)| match mutation {
            // A write whose changes do not parse is none the rule decides: it stays waiting, as
            // it would without the rule.
            DocumentMutation::Incremental(bytes) => {
                tilo_failure::ignored!(
                    "an unparsed write stays waiting, as it would without this rule",
                    automerge::Automerge::parse_changes(bytes).unwrap_or_default()
                )
            }
            DocumentMutation::Snapshot(_) => Vec::new(),
        })
        .collect();
    if changes.iter().all(Vec::is_empty) {
        return vec![false; waiting.len()];
    }
    let document = record.committed_document();
    let carried: HashMap<ChangeHash, &automerge::Change> = changes
        .iter()
        .flatten()
        .map(|change| (change.hash(), change))
        .collect();
    // Whether `change` builds on its writer's previous change through what the record and the
    // waiting writes hold. A writer's every change follows its previous one, so where that one is
    // not among its ancestors here, it is behind a change that is missing. The record may hold
    // another change under the same sequence (the writer wrote again from the record as it read
    // without the lost change, as a device that reloads does); that one is not what this change
    // was built on, and is not among its ancestors.
    let follows_a_gap = |change: &automerge::Change| {
        let Some(previous) = change.seq().checked_sub(1).filter(|seq| *seq > 0) else {
            return false;
        };
        let actor = change.actor_id();
        let mut seen: HashSet<ChangeHash> = HashSet::new();
        let mut next: Vec<ChangeHash> = change.deps().to_vec();
        while let Some(hash) = next.pop() {
            if !seen.insert(hash) {
                continue;
            }
            let held = automerge::ReadDoc::get_change_by_hash(document, &hash);
            let Some(ancestor) = held.as_ref().or_else(|| carried.get(&hash).copied()) else {
                continue;
            };
            if ancestor.actor_id() == actor && ancestor.seq() >= previous {
                return false;
            }
            next.extend(ancestor.deps().iter().copied());
        }
        true
    };
    let mut stuck: Vec<bool> = waiting
        .iter()
        .zip(&changes)
        .map(|((signed, _), changes)| {
            signed.is_some_and(|signed| held.holds_all_before(signed))
                && changes.iter().any(follows_a_gap)
        })
        .collect();
    // Everything built on a stuck write is stuck behind the same hole.
    loop {
        let behind: HashSet<ChangeHash> = changes
            .iter()
            .zip(&stuck)
            .filter(|(_, stuck)| **stuck)
            .flat_map(|(changes, _)| changes.iter().map(automerge::Change::hash))
            .collect();
        let mut grew = false;
        for (index, changes) in changes.iter().enumerate() {
            if stuck.get(index).copied().unwrap_or(true) {
                continue;
            }
            if changes
                .iter()
                .any(|change| change.deps().iter().any(|dep| behind.contains(dep)))
            {
                if let Some(entry) = stuck.get_mut(index) {
                    *entry = true;
                }
                grew = true;
            }
        }
        if !grew {
            return stuck;
        }
    }
}
