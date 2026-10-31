//! The workspace-join exchange answered by this device's own engine.
//!
//! Every participant's device holds the room's admitted governance, its
//! documents and its join ledger, so a join is answered by whichever device
//! the joining one reaches: the same ledger, the same admission and the same
//! bootstrap, published by this device like any other change it makes. No
//! device answers for another, and no device holds a privilege the room's
//! governance does not give it.
use super::record::StoredJoinReceipt;
use super::{
    NativeCommittedView, NativeWorkspaceOwner, RoomId, StoredPendingEnrollment, publication,
};
use crate::StatePeer;
use crate::join::TransportHint;
use crate::join::invitations::{IssueInvitationRequest, IssuedInvitation};
use crate::join_answer::{JoinRooms, RoomTxn};
use crate::join_ledger::{GovernanceMessages, Governed, JoinError, JoinLedger, Signer};
use crate::owner_commit::{CommitBinding, CommitTarget, OwnerCommitReceipt};
use ed25519_dalek::SigningKey;
use std::future::Future;
use std::time::{Duration, SystemTime};
use tilo_user_identity::DeviceCert;

/// Most join admissions one room keeps receipted.
const MAX_JOIN_RECEIPTS: usize = 4096;

/// `now` as a clock reading from nanoseconds since the Unix epoch.
fn at(now: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(now))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The receipt binding of one join admission: the room's join ledger after
/// it, and the governance change it committed.
fn join_binding(
    room: RoomId,
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
        room: room.to_string(),
        target: CommitTarget::JoinAdmission { device },
        request_digest,
        basis,
    })
}

/// The room's own governance messages, as a joining device admits them.
struct RoomGovernance<'a>(&'a StatePeer);

impl GovernanceMessages for RoomGovernance<'_> {
    fn governance_messages(&self) -> Vec<Vec<u8>> {
        postcard::to_stdvec(&self.0.room_message())
            .map(|bytes| vec![bytes])
            .unwrap_or_default()
    }
}

/// One room of this device's engine while a join is decided: a candidate the
/// answer publishes once, so nothing of it is observable before it is
/// durable, and the ledger staged by the exchange is kept with it.
struct OwnerRoom<'a> {
    owner: &'a NativeWorkspaceOwner,
    room: RoomId,
    /// The room's id as the exchange names it.
    named: String,
    workspace_id: u64,
    candidate: NativeCommittedView,
    staged: bool,
    committed: bool,
}

impl OwnerRoom<'_> {
    /// Make what the exchange staged durable, and send what it committed,
    /// before the answer it belongs to is given.
    fn publish(&mut self) -> Result<(), JoinError> {
        if !self.staged {
            return Ok(());
        }
        let mut next = self.candidate.clone();
        next.record.generation = next
            .generation()
            .checked_add(1)
            .ok_or_else(|| JoinError::new("native join generation exhausted"))?;
        self.owner
            .publish_room(self.room, next, None::<&StoredPendingEnrollment>)
            .map_err(JoinError::new)?;
        self.staged = false;
        Ok(())
    }
}

impl GovernanceMessages for OwnerRoom<'_> {
    fn governance_messages(&self) -> Vec<Vec<u8>> {
        RoomGovernance(self.candidate.peer()).governance_messages()
    }
}

impl RoomTxn for OwnerRoom<'_> {
    fn room(&self) -> &str {
        &self.named
    }

    fn governed(&self) -> Governed<'_> {
        Governed {
            peer: self.candidate.peer(),
            host: self.candidate.device(),
            workspace_id: self.workspace_id,
            generation: self.candidate.generation(),
            governance_messages: self,
        }
    }

    fn ledger(&self) -> &JoinLedger {
        &self.candidate.record.join_ledger
    }

    fn stage_ledger(&mut self, ledger: JoinLedger) {
        if *self.ledger() != ledger {
            self.candidate.record.join_ledger = ledger;
            self.staged = true;
        }
    }

    fn commit_governance(
        &mut self,
        peer: StatePeer,
        now: u64,
        decide: &mut dyn FnMut(&Governed<'_>) -> Result<JoinLedger, JoinError>,
    ) -> Result<(), JoinError> {
        let mut next = self.candidate.clone();
        let before = next.peer.room_heads().unwrap_or_default();
        next.peer = peer;
        let generation = next
            .generation()
            .checked_add(1)
            .ok_or_else(|| JoinError::new("native join generation exhausted"))?;
        let answered = {
            let messages = RoomGovernance(&next.peer);
            let governed = Governed {
                peer: &next.peer,
                host: next.device(),
                workspace_id: self.workspace_id,
                generation,
                governance_messages: &messages,
            };
            decide(&governed)?
        };
        let changes = next.peer.room_changes_since(&before);
        next.record.join_ledger = answered;
        next.record.generation = generation;
        next.record.peer = next.peer.durable().map_err(JoinError::new)?;
        if !changes.is_empty() {
            let binding = join_binding(
                self.room,
                next.device().device_public_key,
                &next.record.join_ledger,
                &changes,
            )
            .map_err(JoinError::new)?;
            if next.record.join_admissions.len() >= MAX_JOIN_RECEIPTS {
                return Err(JoinError::new("native join custody capacity exhausted"));
            }
            next.record.join_admissions.insert(
                hex::encode(binding.request_digest),
                StoredJoinReceipt {
                    receipt: OwnerCommitReceipt {
                        binding: binding.clone(),
                        generation,
                        aggregate_hash: *blake3::hash(&next.record.peer).as_bytes(),
                    },
                },
            );
            publication::add_member_changes(&mut next, changes, binding, at(now))
                .map_err(JoinError::new)?;
        }
        self.owner
            .publish_room(self.room, next.clone(), None::<&StoredPendingEnrollment>)
            .map_err(JoinError::new)?;
        self.candidate = next;
        self.staged = false;
        self.committed = true;
        Ok(())
    }
}

/// This device's rooms, to the exchange that answers their joins
/// ([`crate::join_answer::JoinAnswer`]).
#[derive(Clone)]
pub struct OwnerJoinRooms {
    owner: NativeWorkspaceOwner,
    key: SigningKey,
}

impl std::fmt::Debug for OwnerJoinRooms {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnerJoinRooms")
            .field("device", &hex::encode(self.key.verifying_key().as_bytes()))
            .finish_non_exhaustive()
    }
}

impl OwnerJoinRooms {
    /// Answer joins for `owner`'s rooms as the device whose signing key is
    /// `key`, the device its commits are made as.
    #[must_use]
    pub fn new(owner: NativeWorkspaceOwner, key: SigningKey) -> Self {
        Self { owner, key }
    }
}

impl OwnerJoinRooms {
    /// Run `decide` on `room`, under the room's lock, and publish what it
    /// staged before answering it.
    fn answer<T>(
        &self,
        room: &str,
        decide: impl FnOnce(&mut dyn RoomTxn) -> Result<T, JoinError>,
    ) -> Result<T, JoinError> {
        let room =
            RoomId::parse(room).ok_or_else(|| JoinError::new("this device holds no such room"))?;
        let _turn = self.owner.0.mutation.lock();
        let candidate = self
            .owner
            .with_current(room, |view| Ok(view.clone()))
            .map_err(JoinError::new)?;
        let workspace_id = crate::join::room_workspace_id(&room.to_string())
            .map_err(|error| JoinError::new(error.to_string()))?;
        let mut held = OwnerRoom {
            owner: &self.owner,
            room,
            named: room.to_string(),
            workspace_id,
            candidate,
            staged: false,
            committed: false,
        };
        let answer = decide(&mut held);
        if held.staged && !held.committed {
            held.publish()?;
        }
        answer
    }
}

impl JoinRooms for OwnerJoinRooms {
    fn key(&self) -> &SigningKey {
        &self.key
    }

    fn nonce(&self) -> Result<[u8; 32], JoinError> {
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(|error| JoinError::new(error.to_string()))?;
        Ok(nonce)
    }

    fn with_room<T: Send>(
        &self,
        room: &str,
        decide: impl FnOnce(&mut dyn RoomTxn) -> Result<T, JoinError> + Send,
    ) -> impl Future<Output = Result<T, JoinError>> + Send {
        std::future::ready(self.answer(room, decide))
    }
}

impl NativeWorkspaceOwner {
    /// Issue an invitation into `room` as this device, signed by `key`, and
    /// publish it with the room's join ledger: the invitation's grant names
    /// this device, which answers its redemption like any other participant.
    ///
    /// # Errors
    ///
    /// `key` is not the device's, it may not invite, the request is invalid
    /// or the room cannot be published.
    pub fn issue_invitation(
        &self,
        room: RoomId,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
        transport: Vec<TransportHint>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<IssuedInvitation, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if key.verifying_key().to_bytes() != candidate.device().device_public_key {
            return Err("invitation is not signed by this device".into());
        }
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
        candidate.peer.require_member_writer(key, now)?;
        let mut ledger = candidate.record.join_ledger.clone();
        let issued = {
            let messages = RoomGovernance(&candidate.peer);
            let governed = Governed {
                peer: &candidate.peer,
                host: candidate.device(),
                workspace_id: crate::join::room_workspace_id(&room.to_string())
                    .map_err(|error| error.to_string())?,
                generation: candidate.generation(),
                governance_messages: &messages,
            };
            ledger
                .issue(
                    &governed,
                    administrator,
                    request,
                    transport,
                    Signer { key, nonce },
                    crate::owner::enrollment::ns(now)?,
                )
                .map_err(|error| error.to_string())?
        };
        candidate.record.join_ledger = ledger;
        candidate.record.generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("native join generation exhausted")?;
        self.publish_room(room, candidate, None)?;
        Ok(issued)
    }
}
