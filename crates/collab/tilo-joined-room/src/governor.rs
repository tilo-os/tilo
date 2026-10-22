//! The governor's side of joining, for rooms this device governs: issuing
//! invitations and answering the workspace-join exchange of a device that
//! redeems one. The ledger it keeps between the steps is the one every
//! governor keeps ([`JoinLedger`]), held in the room's record, so a room is
//! joined the same way whether a relay, a cloud host or this device
//! governs it.
//!
//! Every answer that changes the ledger or the room is given only after
//! the owner published it ([`NativeWorkspaceOwner::publish_room`]).

use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_peer_state::join::invitations::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
};
use tilo_peer_state::join::{
    JoinChallengeResponse, JoinInspection, JoinTransportRequest, JoinTransportResponse,
    MembershipChallengeResponse, TransportHint,
};
use tilo_peer_state::join_ledger::{Governed, JoinError, JoinLedger, Redeeming, Signer};

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
/// alone, as one message ([`crate::enrollment`] admits nothing else from a
/// bootstrap). Its records follow through the room's sync.
struct RoomGovernance<'a>(&'a tilo_peer_state::StatePeer);

impl tilo_peer_state::join_ledger::GovernanceMessages for RoomGovernance<'_> {
    fn governance_messages(&self) -> Vec<Vec<u8>> {
        postcard::to_stdvec(&self.0.room_message())
            .map(|bytes| vec![bytes])
            .unwrap_or_default()
    }
}

/// `view`, a room governed here, as its join ledger reads it.
fn governed<'a>(view: &'a NativeCommittedView, messages: &'a RoomGovernance<'a>) -> Governed<'a> {
    Governed {
        peer: &view.peer,
        workspace_id: tilo_peer_state::join::room_workspace_id(&view.record.room.to_string())
            .unwrap_or_default(),
        generation: view.record.generation,
        governance_messages: messages,
    }
}

/// The room a join request is about, read without trusting it: the
/// ledger then verifies it against what that room issued.
fn requested_room(request: &JoinTransportRequest, now: u64) -> Result<RoomId, JoinError> {
    let room = match request {
        JoinTransportRequest::Inspect { signed_invitation } => {
            tilo_peer_state::join_ledger::invitation_room(signed_invitation, now)?
        }
        JoinTransportRequest::Challenge(request) => {
            tilo_peer_state::join_ledger::invitation_room(&request.signed_invitation, now)?
        }
        JoinTransportRequest::Complete(request) => request.proof.challenge.room.clone(),
        JoinTransportRequest::MembershipChallenge(request) => request.room.clone(),
        JoinTransportRequest::MembershipComplete(request) => request.proof.challenge.room.clone(),
    };
    RoomId::parse(&room).ok_or_else(|| JoinError::new("the request names no room"))
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
        let messages = RoomGovernance(&candidate.peer);
        let mut ledger = candidate.record.join_ledger.clone();
        let administrator = candidate.peer.authority().clone();
        let issued = ledger.issue(
            &governed(&candidate, &messages),
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
    ///
    /// # Errors
    /// The room is not governed here.
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

    /// Answer one workspace-join request from the device at
    /// `remote_endpoint`, its authenticated transport key, for a room this
    /// device governs with `key`: the same exchange a relay answers for
    /// the rooms it governs.
    pub fn serve_join(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
        key: &SigningKey,
        nonces: Nonces,
        now: SystemTime,
    ) -> JoinTransportResponse {
        self.answer_join(remote_endpoint, request, key, nonces, now)
            .unwrap_or_else(|error| JoinTransportResponse::Error {
                code: error.code(),
                message: error.to_string(),
            })
    }

    fn answer_join(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
        key: &SigningKey,
        nonces: Nonces,
        now: SystemTime,
    ) -> Result<JoinTransportResponse, JoinError> {
        let at = now;
        let now = now_ns(now)?;
        let room = requested_room(&request, now)?;
        let _turn = self.0.mutation.lock();
        let candidate = self.governed_candidate(room, key)?;
        let messages = RoomGovernance(&candidate.peer);
        let mut ledger = candidate.record.join_ledger.clone();
        Ok(match request {
            JoinTransportRequest::Inspect { signed_invitation } => {
                let join =
                    ledger.inspect(&governed(&candidate, &messages), &signed_invitation, now)?;
                JoinTransportResponse::Inspection(JoinInspection {
                    invite_id: join.invite_id(),
                    grant: join.grant().clone(),
                })
            }
            JoinTransportRequest::Challenge(request) => {
                let challenge = ledger.challenge(
                    &governed(&candidate, &messages),
                    remote_endpoint,
                    request,
                    nonces.challenge,
                    now,
                )?;
                self.publish_ledger(room, candidate, ledger)?;
                JoinTransportResponse::Challenge(JoinChallengeResponse { challenge })
            }
            JoinTransportRequest::Complete(request) => {
                let request_id = request.proof.challenge.request_id;
                let proof_digest = tilo_peer_state::join_ledger::request_digest(&request.proof)?;
                let view = governed(&candidate, &messages);
                if let Some(prior) =
                    ledger.completed(&view, remote_endpoint, request_id, proof_digest, now)?
                {
                    return Ok(JoinTransportResponse::Complete(prior));
                }
                let redemption = ledger.redeem(&view, remote_endpoint, &request.proof, now)?;
                let joins = [Redeeming {
                    remote_endpoint,
                    request_id,
                    proof_digest,
                    redemption: &redemption,
                }];
                let bootstrap = self.admit_join(room, candidate, ledger, &joins, key, at)?;
                JoinTransportResponse::Complete(bootstrap)
            }
            JoinTransportRequest::MembershipChallenge(request) => {
                let challenge = ledger.membership_challenge(
                    &governed(&candidate, &messages),
                    remote_endpoint,
                    request,
                    nonces.challenge,
                    now,
                )?;
                self.publish_ledger(room, candidate, ledger)?;
                JoinTransportResponse::MembershipChallenge(MembershipChallengeResponse {
                    challenge,
                })
            }
            JoinTransportRequest::MembershipComplete(request) => {
                JoinTransportResponse::MembershipComplete(ledger.membership_complete(
                    &governed(&candidate, &messages),
                    remote_endpoint,
                    &request,
                    now,
                )?)
            }
        })
    }

    /// Admit the verified `joins` into `candidate`, the room governed here,
    /// record their receipts in `ledger` and publish both, in that order,
    /// before the joining device hears its bootstrap.
    fn admit_join(
        &self,
        room: RoomId,
        mut candidate: NativeCommittedView,
        mut ledger: JoinLedger,
        joins: &[Redeeming<'_>],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<tilo_peer_state::join::JoinBootstrap, JoinError> {
        let at = now_ns(now)?;
        let refusals = ledger.admit(&mut candidate.peer, joins, key, at);
        if let Some(Some(refused)) = refusals.first() {
            return Err(refused.clone());
        }
        candidate.record.peer = candidate.peer.durable().map_err(JoinError::new)?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or_else(|| JoinError::new("native owner generation exhausted"))?;
        let messages = RoomGovernance(&candidate.peer);
        let delivered = ledger.complete(&governed(&candidate, &messages), joins, &refusals)?;
        let bootstrap = delivered
            .into_iter()
            .next()
            .ok_or_else(|| JoinError::new("join bootstrap was not built"))??;
        candidate.record.join_ledger = ledger;
        self.publish_room(room, candidate, None)
            .map_err(JoinError::new)?;
        Ok(bootstrap)
    }
}

#[cfg(test)]
mod tests;
