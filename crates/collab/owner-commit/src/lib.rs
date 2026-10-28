//! Host-local durable handoff. This is not a neighbour message or a wire capability.
//! A receipt binds the exact signed mutation/receipt basis, never queue admission.
pub mod desktop_command;
mod policy_basis;
pub use policy_basis::{PrivatePolicyBasis, canonical_owner, governance_revision};

use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};
use tilo_peer_state::{
    document_import::ImportAction, workspace_membership::WorkspaceMembershipCommand,
};

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
    /// Only the broker's redacted `WorkspaceEffectReceipt` encoding belongs here.
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
    /// Retired agent runtime fact. It keeps the variants after it at their
    /// encoded positions, and every binding of it is refused.
    RetiredAgentRuntime,
    /// A proposal to bring a whole document into the room, or the owner's
    /// answer to one. The broker supplies the caller from its authenticated
    /// durable admission, never from arguments.
    DocumentImport {
        invocation: uuid::Uuid,
        caller: tilo_user_identity::DeviceCert,
        action: ImportAction,
    },
    /// Retired room task command: tasks are the Tasks app's issues. It keeps
    /// the variants after it at their encoded positions, and every binding of
    /// it is refused.
    RetiredWorkspaceTask,
    /// Original authenticated person command; governor signing custody is
    /// supplied out of band by the installed owning host, never by arguments.
    WorkspaceMembershipCommand {
        invocation: uuid::Uuid,
        caller: tilo_user_identity::DeviceCert,
        command: WorkspaceMembershipCommand,
    },
    /// Save or remove one shortcut of the room for the authenticated caller,
    /// whose device's proof names them as its saver
    /// ([`tilo_peer_state::room::proven_saver`]); governor signing custody is
    /// supplied out of band, as for membership. The change is its JSON
    /// ([`tilo_automation::ShortcutChange`]): a shortcut carries
    /// self-describing values a binary encoding does not read back.
    ShortcutChange {
        invocation: uuid::Uuid,
        caller: tilo_user_identity::DeviceCert,
        change: Vec<u8>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitTarget {
    Document {
        app: String,
        record: String,
    },
    EffectReceipt {
        invocation: uuid::Uuid,
    },
    DesktopCommand {
        invocation: uuid::Uuid,
    },
    RetiredAgentRuntime,
    DocumentImport {
        invocation: uuid::Uuid,
    },
    /// Retired with [`CommitOperation::RetiredWorkspaceTask`]; keeps the
    /// targets after it at their encoded positions.
    RetiredWorkspaceTask,
    WorkspaceMembershipCommand {
        device: [u8; 32],
        invocation: uuid::Uuid,
    },
    /// Signed native content root publication; not a caller-facing operation.
    NativeCheckpointRoot {
        root: [u8; 32],
    },
    ShortcutChange {
        device: [u8; 32],
        invocation: uuid::Uuid,
    },
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
    /// The commit binding this request must be receipted against.
    ///
    /// # Errors
    ///
    /// Returns an error if the request's state cannot be encoded.
    pub fn binding(&self) -> Result<CommitBinding, String> {
        if uuid::Uuid::parse_str(&self.room)
            .map_err(|e| e.to_string())?
            .to_string()
            != self.room
        {
            return Err("owner commit requires the exact canonical room UUID".into());
        }
        let (target, bytes) = match &self.operation {
            CommitOperation::RetiredAgentRuntime => {
                return Err("agent runtime owner commits are retired".into());
            }
            CommitOperation::RetiredWorkspaceTask => {
                return Err("room task owner commits are retired".into());
            }
            CommitOperation::DesktopCommand {
                invocation,
                caller,
                command,
            } => {
                let placement_independent = command.command.is_placement_independent();
                if invocation.is_nil()
                    || command.source.room != self.room
                    || (command.source.heads.is_empty() && !placement_independent)
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
            CommitOperation::DocumentImport {
                invocation,
                caller,
                action,
            } => import_binding(*invocation, caller, action)?,
            CommitOperation::WorkspaceMembershipCommand {
                invocation,
                caller,
                command,
            } => membership_binding(*invocation, caller, command)?,
            CommitOperation::ShortcutChange {
                invocation,
                caller,
                change,
            } => shortcut_binding(*invocation, caller, change)?,
            CommitOperation::Document {
                app,
                record,
                envelope,
            } => {
                collab_types::validate_whole_document_payload_route(
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
            request_digest: *blake3::Hasher::new_derive_key("Tilo owner commit request v1")
                .update(&encoded)
                .finalize()
                .as_bytes(),
            basis: *blake3::Hasher::new_derive_key("Tilo owner committed basis v1")
                .update(&bytes)
                .finalize()
                .as_bytes(),
        })
    }
}
fn membership_binding(
    invocation: uuid::Uuid,
    caller: &tilo_user_identity::DeviceCert,
    command: &WorkspaceMembershipCommand,
) -> Result<(CommitTarget, Vec<u8>), String> {
    if invocation.is_nil() {
        return Err("invalid workspace membership invocation".into());
    }
    command.validate()?;
    Ok((
        CommitTarget::WorkspaceMembershipCommand {
            device: caller.device_public_key,
            invocation,
        },
        postcard::to_allocvec(&(invocation, caller, command)).map_err(|error| error.to_string())?,
    ))
}
/// The target and committed basis of a shortcut change request.
fn shortcut_binding(
    invocation: uuid::Uuid,
    caller: &tilo_user_identity::DeviceCert,
    change: &[u8],
) -> Result<(CommitTarget, Vec<u8>), String> {
    if invocation.is_nil()
        || serde_json::from_slice::<tilo_automation::ShortcutChange>(change).is_err()
    {
        return Err("invalid shortcut change binding".into());
    }
    Ok((
        CommitTarget::ShortcutChange {
            device: caller.device_public_key,
            invocation,
        },
        postcard::to_allocvec(&(invocation, caller, change)).map_err(|e| e.to_string())?,
    ))
}
/// The target and committed basis of a document import request.
fn import_binding(
    invocation: uuid::Uuid,
    caller: &tilo_user_identity::DeviceCert,
    action: &ImportAction,
) -> Result<(CommitTarget, Vec<u8>), String> {
    if invocation.is_nil() || caller.capabilities.can_act_as_host {
        return Err("invalid document import binding".into());
    }
    Ok((
        CommitTarget::DocumentImport { invocation },
        postcard::to_allocvec(&(invocation, caller, action)).map_err(|e| e.to_string())?,
    ))
}
impl OwnerCommitReceipt {
    /// Check that this receipt answers `request` exactly.
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
    /// Whether this room must cross this installed owner's durable boundary.
    /// Scoped native owners retain this requirement while incomplete or fenced.
    fn requires_room(&self, _room: &str) -> bool {
        true
    }
    /// Whether `room`'s effect receipts cross this owner's durable boundary.
    /// They are the room's governance, recorded by its governor whichever
    /// model its records follow: here, or at the host that governs it.
    fn records_effects(&self, room: &str) -> bool {
        self.requires_room(room)
    }
    fn commit(&self, request: OwnerCommitRequest) -> CommitFuture;
    /// Commit a certified member's own terminal audit at the actual governor.
    /// The authenticated caller is retained by the broker, never decoded from
    /// the receipt. Implementations must recheck current member and signing custody.
    fn commit_member_effect_receipt(
        &self,
        request: OwnerCommitRequest,
        caller: tilo_user_identity::DeviceCert,
    ) -> CommitFuture {
        let _ = (request, caller);
        Box::pin(async { Err("member effect governor unavailable".into()) })
    }
    /// [`Self::commit`] for a document write, with the resource kind of its
    /// record as the writing app named it when joining the record. Only an
    /// owner that creates a missing record on the app's first write reads
    /// the kind; the request and its receipt binding never include it.
    fn commit_with_record_kind(
        &self,
        request: OwnerCommitRequest,
        record_kind: Option<String>,
    ) -> CommitFuture {
        let _ = record_kind;
        self.commit(request)
    }
    /// Whether the owner can currently accept commits.
    fn health(&self) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_commit_postcard_literals_and_request_digest_are_stable() {
        let invocation = uuid::Uuid::from_bytes([7; 16]);
        let encoded = |value: &CommitTarget| postcard::to_allocvec(value).unwrap();
        assert_eq!(
            encoded(&CommitTarget::Document {
                app: "a".into(),
                record: "r".into()
            }),
            b"\x00\x01a\x01r"
        );
        assert_eq!(
            encoded(&CommitTarget::EffectReceipt { invocation }),
            [vec![1, 16], vec![7; 16]].concat()
        );
        assert_eq!(
            encoded(&CommitTarget::DesktopCommand { invocation }),
            [vec![2, 16], vec![7; 16]].concat()
        );
        assert_eq!(encoded(&CommitTarget::RetiredWorkspaceTask), vec![5]);
        assert_eq!(
            encoded(&CommitTarget::WorkspaceMembershipCommand {
                device: [9; 32],
                invocation,
            }),
            [vec![6], vec![9; 32], vec![16], vec![7; 16]].concat()
        );
        let request = OwnerCommitRequest {
            room: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            operation: CommitOperation::EffectReceipt {
                invocation,
                redacted: vec![5, 6],
            },
        };
        let literal = [
            b"\x24aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa\x01\x10".to_vec(),
            vec![7; 16],
            vec![2, 5, 6],
        ]
        .concat();
        assert_eq!(postcard::to_allocvec(&request).unwrap(), literal);
        assert_eq!(
            request.binding().unwrap().request_digest,
            *blake3::Hasher::new_derive_key("Tilo owner commit request v1")
                .update(&literal)
                .finalize()
                .as_bytes()
        );
    }
}
