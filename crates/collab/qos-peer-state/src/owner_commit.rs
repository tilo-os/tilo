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
            CommitOperation::DesktopCommand { invocation, caller, command } => {
                if invocation.is_nil() || command.source.room != self.room
                    || command.source.heads.is_empty() || command.source.heads.len() > 128
                    || caller.capabilities.can_act_as_host
                {
                    return Err("invalid desktop owner command binding".into());
                }
                (CommitTarget::DesktopCommand { invocation: *invocation },
                 postcard::to_allocvec(&(invocation, caller, command)).map_err(|e| e.to_string())?)
            }
            CommitOperation::Document {
                app,
                record,
                envelope,
            } => {
                collab_types::validate_document_payload_route(app, &self.room, record, envelope)
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
