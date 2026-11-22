//! A compaction this device's replica refuses, decided once more with the keys the device holds.
use crate::TransportOrigin;
use crate::owner::{NativeCommittedView, NativeWorkspaceOwner};
use collab_types::{OpKind, SignedOp};
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::WorkspaceId;
use web_time::SystemTime;

/// Whether `message` is a compaction of a document's sealed history, or a piece of one.
pub(crate) fn carries_compaction(message: &NeighbourMsg) -> bool {
    matches!(message, NeighbourMsg::AppDocument { envelope, .. } if envelope.kind == OpKind::Compaction)
}

impl NativeWorkspaceOwner {
    /// Decide `message`, a compaction this device's replica refused with `refusal`, once more
    /// with the keys this device holds ([`Self::keep_sealed_histories`]): a replica refuses a
    /// compaction that covers writes it does not hold, since without keys it cannot tell one
    /// that folded them from one that left them out. A device that was away, or was ended
    /// before it had kept what it was sent, holds only part of what its workspace compacted
    /// meanwhile, and those writes are nowhere but in the compaction from then on: refusing it
    /// for good, the device never completed the document again and took in nothing written to
    /// it afterwards. With the keys the compaction is opened and taken when it holds every
    /// change this device holds of what it replaces
    /// ([`crate::PeerPublicationMut::take_compaction_behind`]).
    ///
    /// A compaction it cannot open because it is sealed under an epoch whose key this device does
    /// not hold is no refusal: it is kept in the workspace's record and decided by
    /// [`Self::retry_kept_compactions`] once the key is here, so a restart between loses nothing.
    /// One its author may not write, or one that opens and still lacks a change this device
    /// holds, is refused. `None` when this device holds no keys or the refusal stands.
    pub(crate) fn receive_behind(
        &self,
        candidate: &NativeCommittedView,
        message: &NeighbourMsg,
        refusal: &crate::AdmissionError,
        now: SystemTime,
    ) -> Option<Result<crate::AdmissionReport, crate::AdmissionError>> {
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return None;
        };
        let workspace = candidate.workspace_id();
        let route = (app_id.as_str(), document_id.as_str());
        // What its author may write, and what it is, needs no key: a compaction none of that
        // holds up is refused, never kept.
        if candidate
            .live()
            .publication()
            .check_sealed_compaction(route, envelope, now)
            .is_err()
        {
            self.0.forget_compaction(workspace, message);
            return None;
        }
        let keys = self.0.history_keys.lock().clone()?;
        let mut open = |write: &SignedOp| keys.open(workspace, write);
        let taken = candidate.live().publication_mut().take_compaction_behind(
            route,
            (**envelope).clone(),
            &mut open,
            now,
        );
        match taken {
            Ok(Some(compaction)) => {
                self.0.forget_compaction(workspace, message);
                tracing::info!(
                    %workspace,
                    app = route.0,
                    document = route.1,
                    %refusal,
                    "this device held only part of what a compaction covers; the compaction holds all of it and was taken"
                );
                Some(
                    crate::admission::DocumentAdmission::from_op(
                        cluster_id,
                        route.0,
                        route.1,
                        &compaction,
                    )
                    .map(|admission| {
                        crate::AdmissionReport::new(
                            crate::InputDisposition::Accepted(admission),
                            crate::ReceiveOutcome {
                                changed: true,
                                metadata_changed: true,
                                ..Default::default()
                            },
                        )
                    })
                    .map_err(|error| crate::AdmissionError::from(error.to_string())),
                )
            }
            Ok(None) => Some(Ok(crate::AdmissionReport::new(
                crate::InputDisposition::MetadataOnly,
                crate::ReceiveOutcome::default(),
            ))),
            Err(error) => {
                // A write it folds is sealed under an epoch whose key this device lacks: the
                // compaction is a frame it cannot decide yet, kept in the record for the key.
                if missing_key(&mut open, route, envelope, candidate) {
                    if self.0.keep_compaction(workspace, message) {
                        tracing::info!(
                            %workspace,
                            app = route.0,
                            document = route.1,
                            %refusal,
                            %error,
                            "a compaction this device cannot open yet is kept until its epoch's key is here"
                        );
                    }
                    return Some(Ok(crate::AdmissionReport::new(
                        crate::InputDisposition::RetryLater(
                            crate::MissingPrerequisite::PersistedMls,
                        ),
                        crate::ReceiveOutcome {
                            metadata_changed: true,
                            ..Default::default()
                        },
                    )));
                }
                self.0.forget_compaction(workspace, message);
                tracing::warn!(
                    %workspace,
                    app = route.0,
                    document = route.1,
                    %refusal,
                    %error,
                    "a compaction this device is behind stays refused"
                );
                None
            }
        }
    }
}

impl NativeWorkspaceOwner {
    /// Take the compactions this device kept for an epoch's key it lacked
    /// ([`Self::receive_behind`]) again, through the same admission a replay of each takes: one
    /// the keys now open is taken and leaves the record, one still waiting for its key stays, and
    /// one that opens yet cannot be taken is refused for good. Called when the device's channel
    /// keys arrive.
    pub fn retry_kept_compactions(&self, workspace: WorkspaceId) {
        let kept = self.0.kept(workspace);
        if kept.is_empty() {
            return;
        }
        let mut inputs = Vec::new();
        for kept in kept {
            let Ok(message) = postcard::from_bytes::<NeighbourMsg>(&kept.message) else {
                continue;
            };
            let NeighbourMsg::AppDocument { envelope, .. } = &message else {
                continue;
            };
            let author = *envelope.author.as_bytes();
            inputs.push((TransportOrigin::RetainedServerDelta(author), message));
        }
        if inputs.is_empty() {
            return;
        }
        if let Err(error) = self.receive_batch(workspace, inputs, tilo_clock::wall()) {
            tracing::warn!(
                %workspace,
                %error,
                "this device could not decide the compactions it kept for keys it did not hold"
            );
        }
    }
}

/// Whether opening `compaction`, or a write it covers that this device holds, needs a content key
/// this device lacks: the compaction is then a frame it cannot open yet, not one that leaves a
/// change out.
fn missing_key(
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    (app, document): (&str, &str),
    compaction: &SignedOp,
    candidate: &NativeCommittedView,
) -> bool {
    if open(compaction).is_err() {
        return true;
    }
    let Ok(wrapper) = collab_types::SealedCompaction::parse(compaction) else {
        return false;
    };
    let live = candidate.live();
    let mut holds = |write: &SignedOp| {
        write.kind != OpKind::Compaction && wrapper.covers(write) && open(write).is_err()
    };
    live.publication()
        .sealed_history(app, document)
        .iter()
        .any(&mut holds)
}
