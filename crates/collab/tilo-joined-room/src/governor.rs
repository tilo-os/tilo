//! The rooms this device governs, as a [`GovernorStore`]: a room lives in
//! the native owner's record, and a change to it is durable and reaches
//! its members once the owner published it
//! ([`NativeWorkspaceOwner::publish_room`]). The exchange itself is the
//! one every governor answers ([`Governor`]); issuing and cancelling
//! invitations is this device's own.

use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_governor::{Governor, GovernorStore, RoomTxn};
use tilo_peer_state::StatePeer;
use tilo_peer_state::join::invitations::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
};
use tilo_peer_state::join::{JoinTransportRequest, JoinTransportResponse, TransportHint};
use tilo_peer_state::join_ledger::{Governed, JoinError, JoinLedger, Signer};

/// The randomness one governor answer draws: an invitation's nonce, or a
/// challenge's.
#[derive(Clone, Copy, Debug)]
pub struct Nonces {
    pub invitation: [u8; 16],
    pub challenge: [u8; 32],
}

impl Nonces {
    /// Fresh nonces from the system's randomness.
    ///
    /// # Errors
    /// The system has no randomness to give.
    pub fn fresh() -> Result<Self, String> {
        let mut invitation = [0; 16];
        let mut challenge = [0; 32];
        getrandom::fill(&mut invitation).map_err(|error| error.to_string())?;
        getrandom::fill(&mut challenge).map_err(|error| error.to_string())?;
        Ok(Self {
            invitation,
            challenge,
        })
    }
}

fn now_ns(now: SystemTime) -> Result<u64, JoinError> {
    crate::enrollment::ns(now).map_err(JoinError::new)
}

/// The signed governance a member admits to join: the room's governance
/// alone, as one message (`crate::enrollment` admits nothing else from a
/// bootstrap). Its records follow through the room's sync.
impl tilo_peer_state::join_ledger::GovernanceMessages for NativeCommittedView {
    fn governance_messages(&self) -> Vec<Vec<u8>> {
        postcard::to_stdvec(&self.peer.room_message())
            .map(|bytes| vec![bytes])
            .unwrap_or_default()
    }
}

/// `view`, a room governed here, as its join ledger reads it.
fn governed(view: &NativeCommittedView) -> Governed<'_> {
    Governed {
        peer: &view.peer,
        workspace_id: tilo_peer_state::join::room_workspace_id(&view.record.room.to_string())
            .unwrap_or_default(),
        generation: view.record.generation,
        governance_messages: view,
    }
}

impl NativeWorkspaceOwner {
    /// A copy of `room` as committed, when this device governs it with
    /// `key`.
    fn governed_candidate(
        &self,
        room: RoomId,
        key: &SigningKey,
    ) -> Result<NativeCommittedView, JoinError> {
        let candidate = self
            .with_current(room, |view| Ok(view.clone()))
            .map_err(JoinError::new)?;
        if !candidate.governed_here()
            || candidate.peer.authority().device_public_key != key.verifying_key().to_bytes()
        {
            return Err(JoinError::new("this device does not govern the room"));
        }
        Ok(candidate)
    }

    /// Publish `candidate`, whose join ledger is now `ledger`, before
    /// anyone hears of the change.
    fn publish_ledger(
        &self,
        room: RoomId,
        mut candidate: NativeCommittedView,
        ledger: JoinLedger,
    ) -> Result<(), JoinError> {
        if candidate.record.join_ledger == ledger {
            return Ok(());
        }
        candidate.record.join_ledger = ledger;
        self.publish_room(room, candidate, None)
            .map_err(JoinError::new)
    }

    /// Issue an invitation to `room`, which this device governs with
    /// `key`, reached through `transport`: this device's own signed hint.
    ///
    /// # Errors
    /// The room is not governed here, the request is refused, or the
    /// invitation cannot be published.
    pub fn issue_invitation(
        &self,
        room: RoomId,
        request: IssueInvitationRequest,
        transport: TransportHint,
        key: &SigningKey,
        nonces: Nonces,
        now: SystemTime,
    ) -> Result<IssuedInvitation, JoinError> {
        let now = now_ns(now)?;
        let _turn = self.0.mutation.lock();
        let candidate = self.governed_candidate(room, key)?;
        let mut ledger = candidate.record.join_ledger.clone();
        let administrator = candidate.peer.authority().clone();
        let issued = ledger.issue(
            &governed(&candidate),
            &administrator,
            request,
            vec![transport],
            Signer {
                key,
                nonce: nonces.invitation,
            },
            now,
        )?;
        self.publish_ledger(room, candidate, ledger)?;
        Ok(issued)
    }

    /// Cancel an unredeemed invitation to `room`.
    ///
    /// # Errors
    /// The room is not governed here, the cancellation is refused, or it
    /// cannot be published.
    pub fn cancel_invitation(
        &self,
        room: RoomId,
        request: &CancelInvitationRequest,
        key: &SigningKey,
    ) -> Result<(), JoinError> {
        let _turn = self.0.mutation.lock();
        let candidate = self.governed_candidate(room, key)?;
        let mut ledger = candidate.record.join_ledger.clone();
        if ledger.cancel(request)? {
            self.publish_ledger(room, candidate, ledger)?;
        }
        Ok(())
    }

    /// The invitations to `room` still redeemable at `now`.
    pub fn pending_invitations(
        &self,
        room: RoomId,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<PendingInvitation>, JoinError> {
        let now = now_ns(now)?;
        Ok(self
            .governed_candidate(room, key)?
            .record
            .join_ledger
            .pending(now))
    }

    /// The invitations to `room` still redeemable at `now`, each with its
    /// exact signed bearer: what the room's list shows and what a resend
    /// hands back unchanged.
    ///
    /// # Errors
    /// The room is not governed here.
    pub fn pending_issued_invitations(
        &self,
        room: RoomId,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<(PendingInvitation, Vec<u8>)>, JoinError> {
        let now = now_ns(now)?;
        Ok(self
            .governed_candidate(room, key)?
            .record
            .join_ledger
            .pending_issued(now))
    }

    /// The signed bearer this room issued under `invite_id`, while it is
    /// still redeemable.
    ///
    /// # Errors
    /// The room is not governed here or the clock precedes the epoch.
    pub fn issued_invitation(
        &self,
        room: RoomId,
        invite_id: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Option<Vec<u8>>, JoinError> {
        let now = now_ns(now)?;
        Ok(self
            .governed_candidate(room, key)?
            .record
            .join_ledger
            .issued_invitation(invite_id, now))
    }

    /// Answer one workspace-join request from the device at
    /// `remote_endpoint`, its authenticated transport key, for a room this
    /// device governs with `key`: the exchange every governor answers.
    pub fn serve_join(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
        key: &SigningKey,
        nonces: Nonces,
        now: SystemTime,
    ) -> JoinTransportResponse {
        let now = match now_ns(now) {
            Ok(now) => now,
            Err(error) => {
                return JoinTransportResponse::Error {
                    code: error.code(),
                    message: error.to_string(),
                };
            }
        };
        let governor = Governor(OwnedRooms {
            owner: self,
            key,
            nonce: nonces.challenge,
        });
        // Every step here is decided and published before its future is
        // first polled: the owner's rooms never wait.
        let answer = std::pin::pin!(governor.answer(remote_endpoint, request, now));
        match answer.poll(&mut std::task::Context::from_waker(std::task::Waker::noop())) {
            std::task::Poll::Ready(answer) => answer,
            std::task::Poll::Pending => JoinTransportResponse::Error {
                code: 503,
                message: "the join answer was not ready".into(),
            },
        }
    }
}

/// The rooms `owner` governs with `key`, answering one request drawn
/// with `nonce`.
struct OwnedRooms<'a> {
    owner: &'a NativeWorkspaceOwner,
    key: &'a SigningKey,
    nonce: [u8; 32],
}

/// A room governed here while a governor decides, under the owner's
/// mutation turn: its committed copy, and whether its ledger changed since
/// it was last published.
struct OwnedRoom<'a> {
    owner: &'a NativeWorkspaceOwner,
    room: RoomId,
    name: String,
    candidate: NativeCommittedView,
    staged: bool,
}

impl GovernorStore for OwnedRooms<'_> {
    fn key(&self) -> &SigningKey {
        self.key
    }

    fn nonce(&self) -> Result<[u8; 32], JoinError> {
        Ok(self.nonce)
    }

    fn with_room<T: Send>(
        &self,
        room: &str,
        decide: impl FnOnce(&mut dyn RoomTxn) -> Result<T, JoinError> + Send,
    ) -> impl std::future::Future<Output = Result<T, JoinError>> + Send {
        std::future::ready(self.decide_in(room, decide))
    }
}

impl OwnedRooms<'_> {
    fn decide_in<T>(
        &self,
        name: &str,
        decide: impl FnOnce(&mut dyn RoomTxn) -> Result<T, JoinError>,
    ) -> Result<T, JoinError> {
        let room =
            RoomId::parse(name).ok_or_else(|| JoinError::new("the request names no room"))?;
        let _turn = self.owner.0.mutation.lock();
        let mut held = OwnedRoom {
            owner: self.owner,
            room,
            name: name.to_owned(),
            candidate: self.owner.governed_candidate(room, self.key)?,
            staged: false,
        };
        let answer = decide(&mut held);
        if held.staged {
            self.owner
                .publish_room(room, held.candidate, None)
                .map_err(JoinError::new)?;
        }
        answer
    }
}

impl RoomTxn for OwnedRoom<'_> {
    fn room(&self) -> &str {
        &self.name
    }

    fn governed(&self) -> Governed<'_> {
        governed(&self.candidate)
    }

    fn ledger(&self) -> &JoinLedger {
        &self.candidate.record.join_ledger
    }

    fn stage_ledger(&mut self, ledger: JoinLedger) {
        if self.candidate.record.join_ledger != ledger {
            self.candidate.record.join_ledger = ledger;
            self.staged = true;
        }
    }

    fn commit_governance(
        &mut self,
        peer: StatePeer,
        _now: u64,
        ledger: &mut dyn FnMut(&Governed<'_>) -> Result<JoinLedger, JoinError>,
    ) -> Result<(), JoinError> {
        let mut next = self.candidate.clone();
        next.record.peer = peer.durable().map_err(JoinError::new)?;
        next.peer = peer;
        next.record.generation = next
            .record
            .generation
            .checked_add(1)
            .ok_or_else(|| JoinError::new("native owner generation exhausted"))?;
        next.record.join_ledger = ledger(&governed(&next))?;
        self.owner
            .publish_room(self.room, next.clone(), None)
            .map_err(JoinError::new)?;
        self.candidate = next;
        self.staged = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
