//! A record whose stored sealed history its checks refuse at restore is set aside, never read or
//! merged, and the rest of the workspace opens around it.
//!
//! The refused history stays in the durable state exactly as it was stored, so a later
//! [`StatePeer::durable`] keeps it and what was wrong with it can still be inspected; every
//! restore checks it again. The record stands nowhere in what this replica asks its devices to
//! replay ([`StatePeer::replay_floors`]), as a record it lacks does, so a device keeping the
//! record sends its whole history again. Each write of it is decided by the full admission, as
//! any write is: a refused one stays refused. Once the history taken in reaches the record's
//! watermark again, the record leaves quarantine and its new history is what is stored.
//!
//! Which records are set aside, and why, is part of how the workspace stands for its person
//! ([`crate::workspace_view::WorkspaceReading::quarantined`]).
use super::backend::{SealedLog, StoredDocument};
use super::sealed_history::Refused;
use super::{Route, StatePeer};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A record whose stored sealed history this replica refused when it restored, and why: its
/// history is not read until a device of the workspace sends it again whole.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quarantined {
    /// The app whose record it is.
    pub app: String,
    /// The record.
    pub record: String,
    /// What the stored history failed: the check's own words.
    pub reason: String,
}

/// A record's stored sealed history set aside, as it was stored, and why.
pub(super) struct Quarantine {
    pub(super) reason: String,
    pub(super) history: SealedLog,
}

impl StatePeer {
    /// Every record whose stored sealed history this replica refused when it restored, with why,
    /// in record order. Their histories are not read; each is asked of the workspace's devices
    /// again, and leaves this list once one sends it whole.
    #[must_use]
    pub fn quarantined(&self) -> Vec<Quarantined> {
        self.core
            .documents
            .iter()
            .filter_map(|(route, document)| {
                document.quarantine.as_ref().map(|quarantine| Quarantined {
                    app: route.app.clone(),
                    record: route.record.clone(),
                    reason: quarantine.reason.clone(),
                })
            })
            .collect()
    }

    /// Whether `record` of `app` is set aside ([`Self::quarantined`]).
    pub(super) fn quarantined_route(&self, app: &str, record: &str) -> bool {
        self.core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .is_some_and(|document| document.quarantine.is_some())
    }

    /// Set `route`'s stored sealed history, which its checks refused, aside. A history naming a
    /// record the state does not hold, or a second one for a record, is no record's to set aside:
    /// the state itself is refused.
    pub(super) fn quarantine(&mut self, route: &Route, refused: Refused) -> Result<(), String> {
        let Refused { reason, history } = refused;
        let Some(document) = self.core.documents.get_mut(route) else {
            return Err(reason);
        };
        if document.quarantine.is_some() {
            return Err("two sealed histories for one record".into());
        }
        set(route, document, reason, history.into());
        Ok(())
    }

    /// Set the sealed history `route` holds aside for `reason`, unless it is set aside already.
    pub(super) fn set_aside(&mut self, route: &Route, reason: &str) {
        let Some(document) = self.core.documents.get_mut(route) else {
            return;
        };
        if document.quarantine.is_some() {
            return;
        }
        let mut history = document.kept_beside();
        history.extend(document.sealed.iter().cloned());
        document.sealed = SealedLog::default();
        document.resealed = Arc::default();
        document.checkpoint_writes = Arc::default();
        set(route, document, reason.into(), history.into());
    }
}

/// Set `history`, `route`'s stored sealed history, aside in `document` for `reason`.
fn set(route: &Route, document: &mut StoredDocument, reason: String, history: SealedLog) {
    tracing::warn!(
        app = %route.app,
        record = %route.record,
        %reason,
        "a record's stored history is refused and set aside; it is asked for again"
    );
    document.quarantine = Some(Arc::new(Quarantine { reason, history }));
}

/// Take `document` out of quarantine once the sealed history it took in since reaches its
/// watermark again: for every writer, the writes it holds, those its leading compaction folds
/// and those its content carries in the clear reach the writer's floor.
pub(super) fn leave_once_whole(document: &mut StoredDocument) {
    if document.quarantine.is_none() {
        return;
    }
    let mut reached: BTreeMap<[u8; 32], u64> = BTreeMap::new();
    let mut reach = |author: [u8; 32], seq: u64| {
        let at = reached.entry(author).or_default();
        *at = (*at).max(seq);
    };
    for (author, seq) in &document.covered {
        let key = tilo_failure::ignored!(
            "a covered writer that is no key carries no writer's floor",
            hex::decode(author)
                .ok()
                .and_then(|key| <[u8; 32]>::try_from(key).ok())
        );
        if let Some(author) = key {
            reach(author, *seq);
        }
    }
    if let Some(wrapper) = document
        .sealed
        .first()
        .filter(|first| super::checkpoints::is_checkpoint(first))
        .and_then(|first| {
            tilo_failure::ignored!(
                "a first write that does not parse as a checkpoint folds nothing",
                collab_types::SealedCompaction::parse(first).ok()
            )
        })
    {
        for (author, seq) in &wrapper.covers {
            reach(*author.as_bytes(), *seq);
        }
    }
    for op in document
        .sealed
        .iter()
        .chain(document.resealed.iter())
        .chain(document.checkpoint_writes.iter())
    {
        reach(*op.author.as_bytes(), op.seq);
    }
    let whole = !document.sealed.is_empty()
        && document
            .channel
            .last_seq_per_author
            .iter()
            .all(|(author, floor)| reached.get(author.as_bytes()).is_some_and(|at| at >= floor));
    if whole {
        document.quarantine = None;
    }
}
