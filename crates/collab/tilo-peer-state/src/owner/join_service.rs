//! The workspace-join exchange answered by this device's own engine.
//!
//! Every participant's device holds the workspace's admitted governance, its
//! documents and its join ledger, so a join is answered by whichever device
//! the joining one reaches: the same ledger, the same admission and the same
//! bootstrap, published by this device like any other change it makes. No
//! device answers for another, and no device holds a privilege the workspace's
//! governance does not give it.
use super::record::{self, StoredJoinReceipt};
use super::{NativeWorkspaceOwner, WorkspaceId, publication};
use crate::StatePeer;
use crate::join::TransportHint;
use crate::join::invitations::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
};
use crate::join_answer::{JoinWorkspaces, WorkspaceTxn};
use crate::join_ledger::{GovernanceMessages, Governed, JoinError, JoinLedger, Signer};
use crate::owner_commit::{CommitBinding, CommitTarget, OwnerCommitReceipt};
use crate::replica::Replica;
use collab_types::signer::DeviceSigner;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tilo_user_identity::DeviceCert;

/// Most join admissions one workspace keeps receipted.
const MAX_JOIN_RECEIPTS: usize = 4096;

/// `now` as a clock reading from nanoseconds since the Unix epoch.
fn at(now: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(now))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The receipt binding of one join admission: the workspace's join ledger after
/// it, and the governance change it committed.
fn join_binding(
    workspace: WorkspaceId,
    device: [u8; 32],
    ledger: &JoinLedger,
    changes: &[Vec<u8>],
) -> Result<CommitBinding, String> {
    let mut encoded = postcard::to_allocvec(ledger).map_err(|error| error.to_string())?;
    let request_digest = *blake3::Hasher::new_derive_key("Tilo join admission ledger v1")
        .update(&encoded)
        .finalize()
        .as_bytes();
    encoded = postcard::to_allocvec(&changes).map_err(|error| error.to_string())?;
    let basis = *blake3::Hasher::new_derive_key("Tilo join admission change v1")
        .update(&encoded)
        .finalize()
        .as_bytes();
    Ok(CommitBinding {
        workspace: workspace.to_string(),
        target: CommitTarget::JoinAdmission { device },
        request_digest,
        basis,
    })
}

/// One workspace of this device's engine while a join is decided: the state the
/// exchange decides on, and the record it stages beside it, published once so
/// nothing of it is observable before it is durable.
struct OwnerWorkspace<'a> {
    owner: &'a NativeWorkspaceOwner,
    workspace: WorkspaceId,
    /// The workspace's id as the exchange names it.
    named: String,
    workspace_id: u64,
    /// The workspace state this exchange decides on and answers with.
    peer: StatePeer,
    record: record::StoredWorkspace,
    /// This device's live replica, which the answer is installed into.
    replica: Replica,
    staged: bool,
}

impl OwnerWorkspace<'_> {
    /// Make what the exchange staged durable, and send what it committed,
    /// before the answer it belongs to is given.
    fn publish(&mut self) -> Result<(), JoinError> {
        if !self.staged {
            return Ok(());
        }
        self.record.generation = self
            .record
            .generation
            .checked_add(1)
            .ok_or_else(|| JoinError::new("native join generation exhausted"))?;
        *self.replica.lock() = self.peer.clone();
        self.owner
            .publish(self.workspace, &self.replica, self.record.clone(), None)
            .map_err(JoinError::new)?;
        self.staged = false;
        Ok(())
    }
}

impl GovernanceMessages for OwnerWorkspace<'_> {
    fn governance_messages(&self) -> Result<Vec<Vec<u8>>, String> {
        self.peer.governance_messages()
    }
}

impl WorkspaceTxn for OwnerWorkspace<'_> {
    fn workspace(&self) -> &str {
        &self.named
    }

    fn governed(&self) -> Governed<'_> {
        Governed {
            peer: (&self.peer).into(),
            host: &self.record.device,
            workspace_id: self.workspace_id,
            generation: self.record.generation,
            governance_messages: None,
        }
    }

    fn ledger(&self) -> &JoinLedger {
        &self.record.join_ledger
    }

    fn stage_ledger(&mut self, ledger: JoinLedger) {
        if self.record.join_ledger != ledger {
            self.record.join_ledger = ledger;
            self.staged = true;
        }
    }

    fn commit_governance(
        &mut self,
        peer: StatePeer,
        now: u64,
        decide: &mut dyn FnMut(&Governed<'_>) -> Result<JoinLedger, JoinError>,
    ) -> Result<(), JoinError> {
        let before = self.peer.workspace_heads().unwrap_or_default();
        self.peer = peer;
        let generation = self
            .record
            .generation
            .checked_add(1)
            .ok_or_else(|| JoinError::new("native join generation exhausted"))?;
        let answered = {
            let governed = Governed {
                peer: (&self.peer).into(),
                host: &self.record.device,
                workspace_id: self.workspace_id,
                generation,
                governance_messages: None,
            };
            decide(&governed)?
        };
        let changes = self.peer.workspace_changes_since(&before);
        self.record.join_ledger = answered;
        self.record.generation = generation;
        if !changes.is_empty() {
            let binding = join_binding(
                self.workspace,
                self.record.device.device_public_key,
                &self.record.join_ledger,
                &changes,
            )
            .map_err(JoinError::new)?;
            if self.record.join_admissions.len() >= MAX_JOIN_RECEIPTS {
                return Err(JoinError::new("native join custody capacity exhausted"));
            }
            self.record.join_admissions.insert(
                hex::encode(binding.request_digest),
                StoredJoinReceipt {
                    receipt: OwnerCommitReceipt {
                        binding: binding.clone(),
                        generation,
                        aggregate_hash: self.peer.durable_hash().map_err(JoinError::new)?,
                    },
                },
            );
            publication::add_member_changes(
                &self.peer,
                &mut self.record,
                changes,
                binding,
                at(now),
            )
            .map_err(JoinError::new)?;
        }
        *self.replica.lock() = self.peer.clone();
        self.owner
            .publish(self.workspace, &self.replica, self.record.clone(), None)
            .map_err(JoinError::new)?;
        self.staged = false;
        Ok(())
    }
}

/// This device's workspaces, to the exchange that answers their joins
/// ([`crate::join_answer::JoinAnswer`]).
#[derive(Clone)]
pub struct OwnerJoinWorkspaces {
    owner: NativeWorkspaceOwner,
    signer: Arc<dyn DeviceSigner>,
}

impl std::fmt::Debug for OwnerJoinWorkspaces {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnerJoinWorkspaces")
            .field("device", &hex::encode(self.signer.device_public_key()))
            .finish_non_exhaustive()
    }
}

impl OwnerJoinWorkspaces {
    /// Answer joins for `owner`'s workspaces as the device `signer` is the
    /// custody of, the device its commits are made as.
    #[must_use]
    pub fn new(owner: NativeWorkspaceOwner, signer: impl DeviceSigner + 'static) -> Self {
        Self {
            owner,
            signer: Arc::new(signer),
        }
    }
}

impl OwnerJoinWorkspaces {
    /// Run `decide` on `workspace`, under the workspace's lock, and publish what it
    /// staged before answering it.
    fn answer<T>(
        &self,
        workspace: &str,
        decide: impl FnOnce(&mut dyn WorkspaceTxn) -> Result<T, JoinError>,
    ) -> Result<T, JoinError> {
        let workspace = WorkspaceId::parse(workspace)
            .ok_or_else(|| JoinError::new("this device holds no such workspace"))?;
        let _turn = self.owner.0.mutation.lock();
        let view = self.owner.view(workspace).map_err(JoinError::new)?;
        let workspace_id = crate::join::room_workspace_id(&workspace.to_string())
            .map_err(|error| JoinError::new(error.to_string()))?;
        let mut held = OwnerWorkspace {
            owner: &self.owner,
            workspace,
            named: workspace.to_string(),
            workspace_id,
            peer: view.live().clone(),
            record: view.record.clone(),
            replica: view.replica.clone(),
            staged: false,
        };
        let answer = decide(&mut held);
        if held.staged {
            held.publish()?;
        }
        answer
    }
}

impl JoinWorkspaces for OwnerJoinWorkspaces {
    fn signer(&self) -> &dyn DeviceSigner {
        &*self.signer
    }

    fn nonce(&self) -> Result<[u8; 32], JoinError> {
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(|error| JoinError::new(error.to_string()))?;
        Ok(nonce)
    }

    fn with_workspace<T: Send>(
        &self,
        workspace: &str,
        decide: impl FnOnce(&mut dyn WorkspaceTxn) -> Result<T, JoinError> + Send,
    ) -> impl Future<Output = Result<T, JoinError>> + Send {
        std::future::ready(self.answer(workspace, decide))
    }
}

impl NativeWorkspaceOwner {
    /// Issue an invitation into `workspace` as this device, signed by `signer`, its custody,
    /// outside the owner's turn, and
    /// publish it with the workspace's join ledger: the invitation's grant names
    /// this device, which answers its redemption like any other participant.
    ///
    /// # Errors
    ///
    /// `signer` is not the device's, signing failed, it may not invite, the role it hands out is
    /// above its own, the request is invalid or the workspace cannot be
    /// published.
    pub async fn issue_invitation(
        &self,
        workspace: WorkspaceId,
        administrator: &DeviceCert,
        request: &IssueInvitationRequest,
        transport: &[TransportHint],
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<IssuedInvitation, String> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
        crate::owner::write::signed_turn(signer, |key| {
            let _turn = self.0.mutation.lock();
            self.change_record(workspace, |view, record| {
                if key.device_public_key() != record.device.device_public_key {
                    return Err("invitation is not signed by this device".into());
                }
                view.peer().require_member_writer(key, now)?;
                view.peer()
                    .require_invitation_authority(key, request.role, now)?;
                let issued = {
                    let governed = Governed {
                        peer: view.peer().into(),
                        host: &record.device,
                        workspace_id: crate::join::room_workspace_id(&workspace.to_string())
                            .map_err(|error| error.to_string())?,
                        generation: record.generation,
                        governance_messages: None,
                    };
                    record
                        .join_ledger
                        .issue(
                            &governed,
                            administrator,
                            request.clone(),
                            transport.to_vec(),
                            Signer { key, nonce },
                            crate::owner::enrollment::ns(now)?,
                        )
                        .map_err(|error| error.to_string())?
                };
                record.generation = record
                    .generation
                    .checked_add(1)
                    .ok_or("native join generation exhausted")?;
                key.settled()?;
                Ok(issued)
            })
        })
        .await
    }

    /// Cancel the invitation `request` names in `workspace` as this device,
    /// signed with `key`, and publish the workspace's join ledger without it:
    /// the link is dead for every member that takes the change, and no host
    /// keeps a list of cancelled links. Answers whether the ledger changed;
    /// asking again with the same request changes nothing.
    ///
    /// An invitation is cancelled by the person who issued it, from any of
    /// their devices: the ledger keeps the issuer's user root, not the one
    /// device that first made the link.
    ///
    /// # Errors
    ///
    /// `key` is not the device's, it may not write, the invitation is unknown
    /// or was issued by someone else, or the workspace cannot be published.
    pub fn cancel_invitation(
        &self,
        workspace: WorkspaceId,
        request: &CancelInvitationRequest,
        key: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        self.change_record(workspace, |view, record| {
            if key.device_public_key() != record.device.device_public_key {
                return Err("invitation is not cancelled by this device".into());
            }
            view.peer().require_member_writer(key, now)?;
            let issuer = record
                .join_ledger
                .issuer_of(request.invite_id)
                .ok_or("invitation does not exist")?;
            if issuer != record.device.user_public_key {
                return Err("an invitation is cancelled by the person who issued it".into());
            }
            if !record
                .join_ledger
                .cancel(request)
                .map_err(|error| error.to_string())?
            {
                return Ok(false);
            }
            record.generation = record
                .generation
                .checked_add(1)
                .ok_or("native join generation exhausted")?;
            Ok(true)
        })
    }

    /// The invitations into `workspace` still redeemable at `now`, as this
    /// device holds them: what the person who invited others sees, without
    /// asking a host.
    ///
    /// # Errors
    ///
    /// This device holds no such workspace, `now` is not a clock reading, or
    /// the workspace's record cannot be read.
    pub fn pending_invitations(
        &self,
        workspace: WorkspaceId,
        now: SystemTime,
    ) -> Result<Vec<PendingInvitation>, String> {
        let now = crate::owner::enrollment::ns(now)?;
        self.with_current(workspace, |view| Ok(view.record.join_ledger.pending(now)))
    }
}
