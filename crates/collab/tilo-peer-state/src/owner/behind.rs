//! A compaction this device's replica refuses, decided once more with the keys the device holds.
use crate::owner::{NativeCommittedView, NativeWorkspaceOwner};
use collab_types::{OpKind, SignedOp};
use neighbour_wire::NeighbourMsg;
use web_time::SystemTime;

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
    /// ([`crate::PeerPublicationMut::take_compaction_behind`]). `None` when this device holds no
    /// keys or the compaction is not taken: the refusal stands.
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
        let keys = self.0.history_keys.lock().clone()?;
        let workspace = candidate.workspace_id();
        let route = (app_id.as_str(), document_id.as_str());
        let mut open = |write: &SignedOp| keys.open(workspace, write);
        let taken = candidate.live().publication_mut().take_compaction_behind(
            route,
            (**envelope).clone(),
            &mut open,
            now,
        );
        match taken {
            Ok(Some(compaction)) => {
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

/// Whether `message` is a compaction of a document's sealed history, or a piece of one.
pub(crate) fn carries_compaction(message: &NeighbourMsg) -> bool {
    matches!(message, NeighbourMsg::AppDocument { envelope, .. } if envelope.kind == OpKind::Compaction)
}
