//! Host-local durable handoff. This is not a neighbour message or a wire capability.
//! A receipt binds the exact signed mutation/receipt basis, never queue admission.
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

pub type CommitFuture = Pin<Box<dyn Future<Output = Result<OwnerCommitReceipt, String>> + Send>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OwnerCommitRequest {
    pub room: String,
    pub operation: CommitOperation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CommitOperation {
    Document {
        app: String,
        record: String,
        envelope: Box<collab_types::SignedOp>,
    },
    /// Only the broker's redacted WorkspaceEffectReceipt encoding belongs here.
    /// Private arguments, results and error messages never cross this boundary.
    EffectReceipt {
        invocation: uuid::Uuid,
        redacted: Vec<u8>,
    },
    /// Host-local semantic capability. The broker supplies the certificate and
    /// invocation from its authenticated durable admission, never from arguments.
    DesktopCommand {
        invocation: uuid::Uuid,
        caller: tilo_user_identity::DeviceCert,
        command: crate::desktop_command::BoundCommand,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitTarget {
    Document { app: String, record: String },
    EffectReceipt { invocation: uuid::Uuid },
    DesktopCommand { invocation: uuid::Uuid },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitBinding {
    pub room: String,
    pub target: CommitTarget,
    pub request_digest: [u8; 32],
    /// Exact signed envelope or redacted receipt bytes that the owner admitted.
    pub basis: [u8; 32],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OwnerCommitReceipt {
    pub binding: CommitBinding,
    pub generation: u64,
    pub aggregate_hash: [u8; 32],
}
impl OwnerCommitRequest {
    pub fn binding(&self) -> Result<CommitBinding, String> {
        if uuid::Uuid::parse_str(&self.room)
            .map_err(|e| e.to_string())?
            .to_string()
            != self.room
        {
            return Err("owner commit requires the exact canonical room UUID".into());
        }
        let (target, bytes) = match &self.operation {
            CommitOperation::DesktopCommand {
                invocation,
                caller,
                command,
            } => {
                if invocation.is_nil()
                    || command.source.room != self.room
                    || command.source.heads.is_empty()
                    || command.source.heads.len() > 128
                    || caller.capabilities.can_act_as_host
                {
                    return Err("invalid desktop owner command binding".into());
                }
                (
                    CommitTarget::DesktopCommand {
                        invocation: *invocation,
                    },
                    postcard::to_allocvec(&(invocation, caller, command))
                        .map_err(|e| e.to_string())?,
                )
            }
            CommitOperation::Document {
                app,
                record,
                envelope,
            } => {
                collab_types::validate_document_payload_route_versioned(
                    app, &self.room, record, envelope,
                )
                .map_err(|e| e.to_string())?;
                (
                    CommitTarget::Document {
                        app: app.clone(),
                        record: record.clone(),
                    },
                    postcard::to_allocvec(envelope).map_err(|e| e.to_string())?,
                )
            }
            CommitOperation::EffectReceipt {
                invocation,
                redacted,
            } => {
                if invocation.is_nil() || redacted.is_empty() || redacted.len() > 16 * 1024 {
                    return Err("invalid redacted owner receipt".into());
                }
                (
                    CommitTarget::EffectReceipt {
                        invocation: *invocation,
                    },
                    redacted.clone(),
                )
            }
        };
        let encoded = postcard::to_allocvec(self).map_err(|e| e.to_string())?;
        Ok(CommitBinding {
            room: self.room.clone(),
            target,
            request_digest: *blake3::Hasher::new_derive_key("qOS owner commit request v1")
                .update(&encoded)
                .finalize()
                .as_bytes(),
            basis: *blake3::Hasher::new_derive_key("qOS owner committed basis v1")
                .update(&bytes)
                .finalize()
                .as_bytes(),
        })
    }
}
impl OwnerCommitReceipt {
    pub fn validate_for(&self, request: &OwnerCommitRequest) -> Result<(), String> {
        if self.binding != request.binding()?
            || self.generation == 0
            || self.aggregate_hash == [0; 32]
        {
            return Err(
                "owner receipt does not bind the requested room, route, basis and aggregate".into(),
            );
        }
        Ok(())
    }
}
/// Implemented by the single durable session owner, not by a remote caller.
/// Failure may follow an application effect and therefore means uncertainty.
pub trait OwnerCommitService: Send + Sync {
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture;
    fn health(&self) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{
        DocumentMutation, DocumentWireVersion, Ed25519PublicKey, OpClass, SignedOp,
        document_artifact_id_for_version, document_attestation_id_for_version,
    };
    use std::time::SystemTime;

    #[test]
    fn document_binding_accepts_one_exact_legacy_wire_namespace() {
        let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let app = "dev.qos.notes";
        let record = "tab:1";
        let version = DocumentWireVersion::QosV1;
        let mutation = DocumentMutation::Incremental(vec![1, 2, 3]);
        let mut envelope = SignedOp::new_unsigned(
            mutation.encode_for(version).unwrap(),
            document_artifact_id_for_version(version, app, room, record),
            Ed25519PublicKey::from_bytes([7; 32]),
            document_attestation_id_for_version(version, app, room, record),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            1,
        );
        let request = |envelope: SignedOp| OwnerCommitRequest {
            room: room.into(),
            operation: CommitOperation::Document {
                app: app.into(),
                record: record.into(),
                envelope: Box::new(envelope),
            },
        };
        assert!(request(envelope.clone()).binding().is_ok());

        envelope.payload = mutation.encode_for(DocumentWireVersion::TiloV1).unwrap();
        assert!(
            request(envelope).binding().is_err(),
            "a current payload prefix cannot be spliced onto legacy route ids"
        );
    }
}
