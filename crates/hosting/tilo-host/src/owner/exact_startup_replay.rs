//! Read-only replay of one exact operation already retained by the durable owner.
use super::*;
use collab_types::{DocumentMutation, SignedOp};
use qos_action_broker::peer_actions::PolicyTime;
use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use ui::{collaboration::RouteSpec, invocation_scope::DocumentAuthorization};

impl SessionOwner {
    pub(crate) fn replay_exact_startup_document(
        &self,
        route: &RouteSpec,
        certificate: &tilo_user_identity::DeviceCert,
        mutation: &DocumentMutation,
        authorization: &DocumentAuthorization,
        time: &PolicyTime,
    ) -> Result<(SignedOp, OwnerCommitReceipt), String> {
        let sample = || {
            time.now_ns()
                .map(|ns| SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ns))
                .map_err(|error| error.to_string())
        };
        let now = sample()?;
        if !self.permits_publication()
            || route.cluster_id.to_string() != self.peer.room()
            || certificate != &self.identity.native_cert
            || certificate != self.peer.authority()
            || !self.peer.certificates().contains(certificate)
        {
            return Err("exact startup replay owner, route or certificate unavailable".into());
        }
        let aggregate_hash = self
            .store
            .acknowledged_digest()
            .map_err(|error| error.to_string())?;
        authorization.check_binding(route, mutation)?;
        // No invocation effect is acknowledged by this read-only startup door.
        // An empty visitor alone is NOT canonicality: exact retained bytes and
        // the accepted governor receipt below provide that separate evidence.
        authorization.validate_owner_contributors(|_, _, _| {
            Err("startup replay cannot acknowledge invocation contributors".into())
        })?;
        if !matches!(mutation, DocumentMutation::Snapshot(_)) {
            return Err("startup replay requires an exact retained snapshot".into());
        }
        let record = route.record.as_deref().unwrap_or("");
        self.peer
            .check_document_read(certificate.device_public_key, &route.app_id, record, now)?;
        let Some((durable, true)) = self
            .peer
            .durable_coverage_with_current(&route.app_id, record)?
        else {
            return Err("startup replay lacks complete current durable governor coverage".into());
        };
        if durable.generation > self.aggregate.generation {
            return Err("startup receipt exceeds acknowledged generation".into());
        }
        let payload = mutation.encode().map_err(|error| error.to_string())?;
        let envelope = self
            .outgoing()
            .iter()
            .find_map(|publication| {
                let message: NeighbourMsg = postcard::from_bytes(publication.bytes).ok()?;
                if !qos_peer_state::outbox::fresh(&message, now) {
                    return None;
                }
                let NeighbourMsg::AppDocument {
                    app_id,
                    cluster_id,
                    record_id,
                    envelope,
                    ..
                } = message
                else {
                    return None;
                };
                (app_id == route.app_id
                    && cluster_id == self.peer.room()
                    && record_id == record
                    && envelope.encryption.is_none()
                    && envelope.op_class == collab_types::OpClass::Splice
                    && envelope.artifact
                        == collab_types::document_artifact_id(
                            &route.app_id,
                            self.peer.room(),
                            record,
                        )
                    && envelope.attestation_id
                        == collab_types::document_attestation_id(
                            &route.app_id,
                            self.peer.room(),
                            record,
                        )
                    && envelope.author.as_bytes() == &certificate.device_public_key
                    && envelope.author.as_bytes() == &durable.coverage.author
                    && envelope.seq == durable.coverage.sequence
                    && envelope.payload == payload
                    && *blake3::hash(&envelope.payload).as_bytes() == durable.coverage.payload_hash)
                    .then_some(*envelope)
            })
            .ok_or("startup body is not the exact retained acknowledged operation")?;
        // This read-only lookup always checks current governed write policy,
        // signature and route. Its optional V5 index result is not the witness:
        // legacy owners have the exact retained governor operation/receipt above.
        // It does not parse/apply an arbitrary document candidate.
        let final_now = sample()?;
        self.peer
            .lookup_exact_local_document(&route.app_id, record, &envelope, final_now)
            .map_err(|error| error.to_string())?;
        self.peer.check_document_read(
            certificate.device_public_key,
            &route.app_id,
            record,
            final_now,
        )?;
        let earliest = final_now
            .checked_sub(std::time::Duration::from_secs(300))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let latest = final_now
            .checked_add(std::time::Duration::from_secs(300))
            .ok_or("startup replay clock overflow")?;
        if envelope.timestamp < earliest || envelope.timestamp > latest {
            return Err("startup replay original operation expired".into());
        }
        sample()?;
        let request = OwnerCommitRequest {
            room: self.peer.room().into(),
            operation: CommitOperation::Document {
                app: route.app_id.clone(),
                record: record.into(),
                envelope: Box::new(envelope.clone()),
            },
        };
        let receipt = OwnerCommitReceipt {
            binding: request.binding()?,
            generation: self.aggregate.generation,
            aggregate_hash,
        };
        receipt.validate_for(&request)?;
        Ok((envelope, receipt))
    }
}
