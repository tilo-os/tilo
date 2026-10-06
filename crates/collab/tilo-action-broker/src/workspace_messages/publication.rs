//! Closed native host publication door, never a wire authority DTO.
use super::upload::{RetainedUpload, UploadCharge};
use std::sync::{Arc, Mutex};

type HostAttestations = Arc<NativeMessageHostAttestations>;

/// Inert exact original host evidence. No constructor or wire form exists;
/// every cloned memo Arc retains its actual processing byte charge.
pub struct NativeMessageHostAttestations {
    proofs: Vec<tilo_joined_room::AgentMessageAdmissionAttestation>,
    _charge: Arc<UploadCharge>,
}

impl NativeMessageHostAttestations {
    pub fn proofs(&self) -> &[tilo_joined_room::AgentMessageAdmissionAttestation] {
        &self.proofs
    }
}

/// Only the private original-invocation service constructs this request.
/// Uploaded data is descriptive until the owner validates the actual signed
/// operation and committed row in its serialized mutation turn.
pub struct NativeMessagePublication {
    pub(super) request: tilo_peer_state::owner_commit::OwnerCommitRequest,
    pub(super) admissions: Vec<tilo_joined_room::AgentMessageAdmissionData>,
    pub(super) publisher: tilo_joined_room::AgentMessagePublisherLease,
    pub(super) _uploads: Vec<Arc<RetainedUpload>>,
    pub(super) processing: Arc<UploadCharge>,
    pub(super) host_attestations: Mutex<Option<HostAttestations>>,
}

impl NativeMessagePublication {
    pub fn request(&self) -> &tilo_peer_state::owner_commit::OwnerCommitRequest {
        &self.request
    }
    pub fn admissions(&self) -> &[tilo_joined_room::AgentMessageAdmissionData] {
        &self.admissions
    }
    pub fn publisher(&self) -> &tilo_joined_room::AgentMessagePublisherLease {
        &self.publisher
    }

    /// Server-crate seam on a closed, nonserializable request. This is inert
    /// correlation, never a free signing endpoint or proof of effect. Only the
    /// installed host initializes it; its charge follows this same request Arc.
    ///
    /// # Errors
    /// Refuses an unavailable/poisoned memo rather than creating new evidence.
    pub fn original_host_attestations(&self) -> Result<Option<HostAttestations>, String> {
        self.host_attestations
            .lock()
            .map(|proofs| proofs.clone())
            .map_err(|_| "native Message host memo unavailable".into())
    }

    /// Initialize once from the exact immutable admission/request inputs.
    /// Later certificate/key changes must not overwrite historical evidence.
    ///
    /// # Errors
    /// Refuses changed original inputs, replacement proof, or an unavailable memo.
    pub fn retain_original_host_attestations(
        &self,
        proofs: Vec<tilo_joined_room::AgentMessageAdmissionAttestation>,
    ) -> Result<HostAttestations, String> {
        if proofs.len() != self.admissions.len()
            || proofs
                .iter()
                .zip(&self.admissions)
                .any(|(proof, data)| &proof.data != data)
        {
            return Err("native Message host memo changes original admission".into());
        }
        let mut memo = self
            .host_attestations
            .lock()
            .map_err(|_| "native Message host memo unavailable")?;
        if let Some(original) = memo.as_ref() {
            if original.proofs() != proofs.as_slice() {
                return Err("native Message host memo cannot be replaced".into());
            }
            return Ok(original.clone());
        }
        let original = Arc::new(NativeMessageHostAttestations {
            proofs,
            _charge: self.processing.clone(),
        });
        *memo = Some(original.clone());
        drop(memo);
        Ok(original)
    }
}

impl crate::collab_broker_capnp::CapnpBrokerState {
    pub(in crate::collab_broker_capnp) async fn commit_native_message_publication(
        &self,
        request: NativeMessagePublication,
    ) -> Result<(), capnp::Error> {
        struct Waiting<'a> {
            failure: &'a std::sync::Mutex<Option<String>>,
            complete: bool,
        }
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                if !self.complete
                    && let Ok(mut failure) = self.failure.lock()
                {
                    failure.get_or_insert_with(|| {
                        "native Message commit interrupted; outcome unknown".into()
                    });
                }
            }
        }
        self.owner_health().map_err(capnp::Error::failed)?;
        let expected = request.request().clone();
        let service = self
            .services
            .workspace_messages
            .as_ref()
            .ok_or_else(|| capnp::Error::failed("native Message owner unavailable".into()))?;
        let mut waiting = Waiting {
            failure: &self.owner_commit_failure,
            complete: false,
        };
        // The generic service error has no typed proof of non-effect. An
        // interrupted call, error, or mismatched receipt is unknown settlement;
        // preserve the original source/reference and fence further admission.
        let receipt = service
            .commit_message(Arc::new(request))
            .await
            .map_err(capnp::Error::failed)?;
        receipt
            .validate_for(&expected)
            .map_err(capnp::Error::failed)?;
        waiting.complete = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab_broker_capnp::workspace_messages::upload::UploadBudget;

    #[test]
    fn retained_host_memo_keeps_processing_charge_after_request_releases_it() {
        let budget = Arc::new(UploadBudget::default());
        let charge = Arc::new(
            budget
                .reserve(
                    runtime_collab::document_contribution::MAX_UPLOAD_BYTES,
                    false,
                )
                .unwrap(),
        );
        // Inert lifetime-only data: this does not manufacture a publication,
        // signature, original invocation, or owner admission.
        let memo = Arc::new(NativeMessageHostAttestations {
            proofs: Vec::new(),
            _charge: charge.clone(),
        });
        let retained = memo.clone();
        drop(charge);
        drop(memo);
        assert!(budget.reserve(1, false).is_err());
        drop(retained);
        assert!(
            budget
                .reserve(
                    runtime_collab::document_contribution::MAX_UPLOAD_BYTES,
                    false,
                )
                .is_ok()
        );
    }
}
