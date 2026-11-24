//! An action that wrote more than one record reaches this device as one change.
//!
//! The device that made the action sends one write per record, and each write carries the
//! action's commit inside its signed payload ([`WorkspaceCommit`]). A write in the clear whose
//! commit names other records waits in the prerequisite queue
//! ([`MissingPrerequisite::ActionParts`]) until every record the commit names is here: waiting
//! there as well, or taken in already. The write that completes the action takes the waiting
//! ones in with it, in one admission, so the records change together and one report says so.
//! A write with no commit, or with a commit naming its own record alone, is taken in as before.
//!
//! What waits is bounded as everything in the queue is ([`super::held::MAX_HELD_INPUTS`],
//! [`super::held::MAX_HELD_BYTES`]), and a commit naming more records than the queue can keep is
//! refused. An action whose writes are let go to make room is let go whole, and so is an action
//! whose author wrote one of its records again after it: that later write is taken in, and the
//! action's write to the record never can be. Either way the report's `dropped` names the action.
//!
//! A sealed write carries its commit inside what only the record's readers open, so it is taken
//! in as it comes, and its readers decide it as they open it: until every other record the
//! commit names holds the action's write, the record reads without it and every write after it
//! ([`StatePeer::sealed_action_waits`]). The two records change together either way, sealed
//! or in the clear, and a write in the clear waits for a sealed part as for any other.

use collab_types::{CommitPart, WorkspaceCommit};

use super::{
    AdmissionError, AdmissionReport, DroppedInput, Held, InputDisposition, MissingPrerequisite,
    NeighbourMsg, Route, StatePeer, SystemTime,
};
use crate::admission::DropReason;

/// The commit `message`, a record write in the clear, carries, when it names more than the
/// record it was written to. `None` for any other message, and for a sealed write.
///
/// # Errors
/// The commit does not read, or names a device other than the write's.
pub(super) fn action_of(message: &NeighbourMsg) -> Result<Option<WorkspaceCommit>, AdmissionError> {
    let NeighbourMsg::AppDocument {
        app_id,
        document_id,
        envelope,
        ..
    } = message
    else {
        return Ok(None);
    };
    if envelope.encryption.is_some() {
        return Ok(None);
    }
    let commit = WorkspaceCommit::of_envelope_payload(envelope, &envelope.payload)
        .map_err(|error| AdmissionError::malformed(format!("the write's action: {error}")))?;
    Ok(commit.filter(|commit| commit.parts.len() > 1 || !commit.covers(app_id, document_id)))
}

/// `commit` as a person reads it in a refusal: its id, its workspace and the device that made it.
fn named(commit: &WorkspaceCommit) -> String {
    format!(
        "action {} of workspace {} by device {}",
        hex::encode(commit.id()),
        commit.workspace,
        hex::encode(&commit.author.device[..4]),
    )
}

/// Whether `entry`, a write waiting in the queue, is one of the action `id`'s.
fn waits_for(entry: &([u8; 32], NeighbourMsg, bool), id: &[u8; 32]) -> bool {
    tilo_failure::ignored!(
        "a waiting write whose commit does not read is no part of an action",
        action_of(&entry.1).ok()
    )
    .flatten()
    .is_some_and(|commit| commit.id() == *id)
}

/// Whether `report` says its write was taken in: neither waiting nor already here.
fn taken_in(report: &AdmissionReport) -> bool {
    matches!(
        report.primary,
        InputDisposition::Accepted(_) | InputDisposition::MetadataOnly
    ) && !report.deferred
        && !report.duplicate
}

impl StatePeer {
    /// Admit `message`, `source`'s write to one record of the action `commit` names: taken in
    /// together with the action's writes to its other records once every one of them is here,
    /// waiting in the queue for them until then.
    ///
    /// # Errors
    /// The commit does not fit the write (another workspace, no part for the record it was
    /// written to, changes the write does not carry, more records than can wait), or the write is
    /// refused as any write is.
    pub(super) fn admit_action_part(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        commit: &WorkspaceCommit,
        now: SystemTime,
        retained_replay: bool,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        let part = self.check_action_part(&message, commit)?;
        // Decided as the write alone first: one that waits for anything else, or is here
        // already, is answered as it would be without an action.
        let mut taken = self.clone();
        let mut report = taken.admit_document(
            source,
            message.clone(),
            now,
            false,
            retained_replay,
            strict_report,
        )?;
        if !taken_in(&report) {
            *self = taken;
            return Ok(report);
        }
        let id = commit.id();
        let complete = commit
            .parts
            .iter()
            .all(|other| other == part || self.action_part_here(&id, commit, other));
        let wait = MissingPrerequisite::ActionParts {
            action: id,
            workspace: commit.workspace.to_string(),
        };
        if !complete {
            return self.defer(
                source,
                message,
                Held {
                    retained_replay,
                    prerequisite: wait,
                    now,
                },
            );
        }
        for (author, waiting, replayed) in taken.take_action(&id) {
            let decided =
                taken.admit_document(author, waiting.clone(), now, false, replayed, strict_report);
            match decided {
                Ok(part) if taken_in(&part) => {
                    report.changed |= part.changed;
                    report.metadata_changed |= part.metadata_changed;
                    report
                        .released
                        .extend(part.taken_in(author, waiting, replayed));
                    report.settled.extend(part.settled);
                }
                // One of the action's writes waits for something of its own now: the whole
                // action waits with it, and is taken in once that write is.
                Ok(_) => {
                    return self.defer(
                        source,
                        message,
                        Held {
                            retained_replay,
                            prerequisite: wait,
                            now,
                        },
                    );
                }
                Err(error) => {
                    let record = super::written_route(&waiting)
                        .map(|(route, _)| format!("{}/{}", route.app, route.record))
                        .unwrap_or_default();
                    return Err(AdmissionError::malformed(format!(
                        "{} is not taken in whole: its write to {record} is refused: {error}",
                        named(commit)
                    )));
                }
            }
        }
        *self = taken;
        Ok(report)
    }

    /// The part of `commit` naming the record `message` was written to, once the commit fits the
    /// write.
    fn check_action_part<'a>(
        &self,
        message: &NeighbourMsg,
        commit: &'a WorkspaceCommit,
    ) -> Result<&'a CommitPart, AdmissionError> {
        let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return Err(AdmissionError::malformed(
                "an action's part is a record write",
            ));
        };
        let refuse = |why: String| AdmissionError::malformed(format!("{}: {why}", named(commit)));
        if uuid::Uuid::parse_str(&self.core.workspace).ok() != Some(commit.workspace) {
            return Err(refuse(format!(
                "it was written to workspace {}",
                self.core.workspace
            )));
        }
        let Some(part) = commit.part(app_id, document_id) else {
            return Err(refuse(format!(
                "it names no write to {app_id}/{document_id}, where it arrived"
            )));
        };
        if commit.parts.len() > super::held::MAX_HELD_INPUTS {
            return Err(refuse(format!(
                "it names {} records; at most {} wait for each other",
                commit.parts.len(),
                super::held::MAX_HELD_INPUTS
            )));
        }
        // The commit names the changes the action made in each record: the write to this one
        // carries the ones it names here, or another write could finish the action in its place.
        if let Ok(collab_types::DocumentMutation::Incremental(bytes)) =
            collab_types::DocumentMutation::decode(&envelope.payload)
        {
            let changes = collab_types::parse_changes(&bytes).map_err(AdmissionError::malformed)?;
            if !part
                .heads
                .iter()
                .all(|head| changes.iter().any(|change| change.hash().0 == *head))
            {
                return Err(refuse(format!(
                    "its write to {app_id}/{document_id} carries none of the changes it names there"
                )));
            }
        }
        Ok(part)
    }

    /// Whether the write `part` of the action `id`, `commit`, is here: waiting in the queue, or
    /// taken in already.
    fn action_part_here(&self, id: &[u8; 32], commit: &WorkspaceCommit, part: &CommitPart) -> bool {
        let waiting = self.core.pending.iter().any(|entry| {
            entry.0 == commit.author.device
                && matches!(&entry.1, NeighbourMsg::AppDocument { app_id, document_id, .. }
                    if part.is(app_id, document_id))
                && waits_for(entry, id)
        });
        waiting || self.action_part_taken_in(commit, part)
    }

    /// Whether the write `part` of `commit` is taken in: its record holds every change the part
    /// names, or a sealed write of the action's device claiming them.
    fn action_part_taken_in(&self, commit: &WorkspaceCommit, part: &CommitPart) -> bool {
        self.core
            .documents
            .get(&Route::of(&part.app, &part.document))
            .is_some_and(|document| {
                let device = commit.author.device;
                part.heads
                    .iter()
                    .all(|head| document.doc.has_change(&automerge::ChangeHash(*head)))
                    || document.sealed.iter().any(|write| {
                        write.author.as_bytes() == &device
                            && part
                                .heads
                                .iter()
                                .all(|head| write.claims.iter().any(|claim| claim.change == *head))
                    })
            })
    }

    /// The action `write`, a sealed write to `record` of `app` its reader opened as `plaintext`,
    /// is one write of, while the action's writes to its other records are not all taken in here:
    /// the record reads without it until they are. `None` for a write of no such action.
    ///
    /// # Errors
    /// The commit does not read, or names a device other than the write's.
    pub(crate) fn sealed_action_waits(
        &self,
        (app, record): (&str, &str),
        write: &collab_types::SignedOp,
        plaintext: &[u8],
    ) -> Result<Option<String>, String> {
        let Some(commit) = WorkspaceCommit::of_envelope_payload(write, plaintext)
            .map_err(|error| format!("the write's action: {error}"))?
        else {
            return Ok(None);
        };
        let whole = commit
            .parts
            .iter()
            .all(|part| part.is(app, record) || self.action_part_taken_in(&commit, part));
        Ok((!whole).then(|| named(&commit)))
    }

    /// Take the writes of the action `id` out of the queue, in the order they came.
    fn take_action(&mut self, id: &[u8; 32]) -> Vec<([u8; 32], NeighbourMsg, bool)> {
        let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.core.pending)
            .into_iter()
            .partition(|entry| waits_for(entry, id));
        self.core.pending = kept.into();
        for (_, message, _) in &taken {
            let size = tilo_failure::ignored!(
                "a write the queue measured as it came measures again",
                crate::postcard_sink::encoded_len(message, usize::MAX).ok()
            )
            .unwrap_or(0);
            self.core.pending_bytes = self.core.pending_bytes.saturating_sub(size);
        }
        taken
    }

    /// Let the action `id` go: its writes waiting in the queue are taken out unapplied, each
    /// reported with `reason`.
    pub(super) fn let_go_action(
        &mut self,
        id: &[u8; 32],
        waiting_for: &MissingPrerequisite,
        reason: &DropReason,
    ) -> Vec<DroppedInput> {
        let let_go: Vec<DroppedInput> = self
            .take_action(id)
            .iter()
            .map(|(author, message, _)| {
                super::held::dropped_input(
                    author,
                    message,
                    Some(waiting_for.clone()),
                    reason.clone(),
                )
            })
            .collect();
        for input in &let_go {
            self.remember_missed(input, true);
        }
        let_go
    }

    /// Let go every action of `author` with a write to `route` before `sequence` still waiting:
    /// `author`'s write `sequence` to it was just taken in, so that write never can be, and the
    /// action is never whole.
    pub(super) fn let_go_overtaken_actions(
        &mut self,
        route: &Route,
        author: [u8; 32],
        sequence: u64,
    ) -> Vec<DroppedInput> {
        let overtaken: Vec<WorkspaceCommit> = self
            .core
            .pending
            .iter()
            .filter(|(writer, message, _)| {
                *writer == author
                    && super::written_route(message)
                        .is_some_and(|(written, earlier)| written == *route && earlier < sequence)
            })
            .filter_map(|(_, message, _)| {
                tilo_failure::ignored!(
                    "a waiting write whose commit does not read is no part of an action",
                    action_of(message).ok()
                )
                .flatten()
            })
            .collect();
        let mut let_go = Vec::new();
        for commit in overtaken {
            let id = commit.id();
            let reason = DropReason::Refused(format!(
                "{} is never whole: its author wrote {}/{} again before the rest of it arrived",
                named(&commit),
                route.app,
                route.record
            ));
            let waiting_for = MissingPrerequisite::ActionParts {
                action: id,
                workspace: commit.workspace.to_string(),
            };
            let_go.extend(self.let_go_action(&id, &waiting_for, &reason));
        }
        let_go
    }
}
