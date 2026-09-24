//! Production workspace-join adapter for the machine-bound Cloud Store.

use crate::{Store, audit, live_projection_from_bootstrap, now_time, revocations};
use anyhow::{Result, anyhow, bail};
use qos_peer_state::join::{
    JoinBootstrap, JoinChallenge, JoinChallengeRequest, JoinChallengeResponse, JoinCompleteRequest,
    JoinInspection, JoinTransportRequest, JoinTransportResponse, MembershipChallenge,
    MembershipChallengeRequest, MembershipChallengeResponse, MembershipCompleteRequest,
    VerifiedJoin, decode_and_verify_invitation, verify_join_proof, verify_membership_proof,
};
use qos_relay::workspace_join::WorkspaceJoinService;
use qos_user_identity::DeviceCert;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod owner_bootstrap;
pub use owner_bootstrap::MembershipRefresh;

const JOIN_CHALLENGE_LIFETIME_NS: u64 = 300_000_000_000;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredJoinChallenge {
    request_digest: [u8; 32],
    device_cert: DeviceCert,
    challenge: JoinChallenge,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredJoinResponse {
    proof_digest: [u8; 32],
    bootstrap: JoinBootstrap,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredMembershipChallenge {
    request_digest: [u8; 32],
    device_cert: DeviceCert,
    challenge: MembershipChallenge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InvitationStatus {
    Pending,
    Redeemed,
    Revoked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StoredInvitation {
    pub signed_invitation: Vec<u8>,
    pub role: qos_peer_state::join::MemberRole,
    pub invitee_name: Option<String>,
    pub expires_at_ns: u64,
    pub status: InvitationStatus,
}

impl Store {
    fn pending_invitation(&self, signed_invitation: &[u8], now: u64) -> Result<&StoredInvitation> {
        self.ensure_reconciled()?;
        let invitation = self
            .state
            .invitations
            .get(&hex::encode(blake3::hash(signed_invitation).as_bytes()))
            .ok_or_else(|| anyhow!("invitation was not issued by this hosted authority"))?;
        if invitation.signed_invitation != signed_invitation {
            bail!("invitation bytes do not match hosted issuance")
        }
        if invitation.expires_at_ns <= now {
            bail!("invitation expired")
        }
        match invitation.status {
            InvitationStatus::Pending => Ok(invitation),
            InvitationStatus::Redeemed => bail!("invitation was already redeemed and consumed"),
            InvitationStatus::Revoked => bail!("invitation was revoked"),
        }
    }

    fn audit_join_denial(
        &self,
        error: &anyhow::Error,
        request_id: [u8; 32],
        signed_invitation: &[u8],
        caller_device: [u8; 32],
        caller_root: [u8; 32],
    ) {
        let message = error.to_string();
        let result = if message.contains("consumed") {
            "denied_invitation_consumed"
        } else if message.contains("revoked") {
            "denied_invitation_revoked"
        } else if message.contains("expired") {
            "denied_invitation_expired"
        } else if message.contains("not issued") {
            "denied_invitation_unknown"
        } else {
            "denied_invitation_invalid"
        };
        audit(
            "collaboration_join_denied",
            serde_json::json!({
                "room": self.state.room,
                "request_id": hex::encode(request_id),
                "invite_id": hex::encode(blake3::hash(signed_invitation).as_bytes()),
                "caller_device": hex::encode(caller_device),
                "caller_root": hex::encode(caller_root),
                "result": result,
            }),
        );
    }

    pub(crate) fn verify_cached_membership(
        &self,
        remote_endpoint: [u8; 32],
        bootstrap: &JoinBootstrap,
        now: u64,
    ) -> Result<()> {
        self.ensure_reconciled()?;
        if bootstrap.device_cert.device_public_key != remote_endpoint {
            bail!("cached join response endpoint mismatch")
        }
        let peer = self.peer_at(now)?;
        if !peer
            .certificates()
            .iter()
            .any(|certificate| certificate == &bootstrap.device_cert)
        {
            bail!("cached join certificate is absent from governed state")
        }
        let member = peer
            .governed_members(now_time(now))
            .map_err(anyhow::Error::msg)?
            .into_iter()
            .find(|member| member.user_public_key == bootstrap.device_cert.user_public_key)
            .ok_or_else(|| anyhow!("cached join member is absent from governed state"))?;
        if member.removed
            || member.role != bootstrap.role
            || !member.active_device_keys.contains(&remote_endpoint)
        {
            bail!("cached join membership is no longer current")
        }
        Ok(())
    }

    fn verified_join(&self, signed_invitation: &[u8], now: u64) -> Result<VerifiedJoin> {
        self.ensure_reconciled()?;
        self.pending_invitation(signed_invitation, now)?;
        let peer = self.peer_at(now)?;
        let verified = decode_and_verify_invitation(
            signed_invitation,
            None,
            &revocations(&peer.revocations())?,
            now,
        )
        .map_err(anyhow::Error::msg)?;
        let grant = verified.grant();
        if grant.room != self.state.room
            || grant.workspace_id != qos_relay::room_workspace_id(&self.state.room)?
            || grant.authority != *peer.authority()
        {
            bail!("invitation is not bound to this hosted workspace authority")
        }
        Ok(verified)
    }

    pub fn join_inspect(&self, signed_invitation: &[u8], now: u64) -> Result<JoinInspection> {
        self.ensure_reconciled()?;
        let verified = self.verified_join(signed_invitation, now)?;
        Ok(JoinInspection {
            invite_id: verified.invite_id(),
            grant: verified.grant().clone(),
        })
    }

    pub fn join_challenge(
        &mut self,
        remote_endpoint: [u8; 32],
        request: JoinChallengeRequest,
        now: u64,
    ) -> Result<JoinChallengeResponse> {
        self.ensure_reconciled()?;
        if request.request_id == [0; 32] || remote_endpoint != request.device_cert.device_public_key
        {
            bail!("join challenge endpoint or request identity mismatch")
        }
        let request_digest = *blake3::hash(&postcard::to_stdvec(&request)?).as_bytes();
        let key = hex::encode(request.request_id);
        let stored = self.state.join_challenges.get(&key).cloned();
        if let Some(stored) = &stored {
            if stored.request_digest != request_digest || stored.device_cert != request.device_cert
            {
                bail!("join request ID substitution")
            }
        }
        let verified = match self.verified_join(&request.signed_invitation, now) {
            Ok(verified) => verified,
            Err(error) => {
                self.audit_join_denial(
                    &error,
                    request.request_id,
                    &request.signed_invitation,
                    remote_endpoint,
                    request.device_cert.user_public_key,
                );
                return Err(error);
            }
        };
        if let Some(stored) = stored {
            return Ok(JoinChallengeResponse {
                challenge: stored.challenge,
            });
        }
        let expires_at_ns = now
            .checked_add(JOIN_CHALLENGE_LIFETIME_NS)
            .ok_or_else(|| anyhow!("join challenge clock overflow"))?
            .min(verified.grant().expires_at_ns);
        let mut nonce = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let challenge = JoinChallenge::new(
            &verified,
            &request.device_cert,
            request.request_id,
            nonce,
            expires_at_ns,
            now,
        )
        .map_err(anyhow::Error::msg)?;
        let mut state = self.state.clone();
        state.join_challenges.insert(
            key,
            StoredJoinChallenge {
                request_digest,
                device_cert: request.device_cert,
                challenge: challenge.clone(),
            },
        );
        self.commit(state)?;
        audit(
            "collaboration_join_challenge_issued",
            serde_json::json!({
                "room": self.state.room,
                "request_id": hex::encode(request.request_id),
                "invite_id": hex::encode(blake3::hash(&challenge.signed_invitation).as_bytes()),
                "caller_device": hex::encode(remote_endpoint),
                "caller_root": hex::encode(challenge.user_public_key),
                "result": "committed",
            }),
        );
        Ok(JoinChallengeResponse { challenge })
    }

    pub fn join_complete(
        &mut self,
        remote_endpoint: [u8; 32],
        request: JoinCompleteRequest,
        now: u64,
    ) -> Result<JoinBootstrap> {
        self.ensure_reconciled()?;
        if remote_endpoint != request.proof.challenge.device_public_key {
            bail!("join proof endpoint mismatch")
        }
        let request_id = request.proof.challenge.request_id;
        let key = hex::encode(request_id);
        let proof_digest = *blake3::hash(&postcard::to_stdvec(&request)?).as_bytes();
        if let Some(stored) = self.state.join_responses.get(&key) {
            if stored.proof_digest != proof_digest {
                bail!("join proof request ID substitution")
            }
            self.verify_cached_membership(remote_endpoint, &stored.bootstrap, now)?;
            return Ok(stored.bootstrap.clone());
        }
        let issued = self
            .state
            .join_challenges
            .get(&key)
            .cloned()
            .ok_or_else(|| anyhow!("join challenge was not issued"))?;
        let (before, mut peer) = self.peer_transition_at(now)?;
        let redemption = verify_join_proof(
            &request.proof,
            &issued.challenge,
            &issued.device_cert,
            &revocations(&peer.revocations())?,
            now,
        )
        .map_err(anyhow::Error::msg)?;
        if let Err(error) = self.pending_invitation(&issued.challenge.signed_invitation, now) {
            self.audit_join_denial(
                &error,
                request_id,
                &issued.challenge.signed_invitation,
                remote_endpoint,
                issued.device_cert.user_public_key,
            );
            return Err(error);
        }
        peer.admit_invited_member(&redemption, &self.key(), now_time(now))
            .map_err(anyhow::Error::msg)?;
        let generation = self.next_generation()?;
        let messages =
            crate::durable_bootstrap_messages(&mut peer, &self.key(), now_time(now), generation)?;
        let live = live_projection_from_bootstrap(&before, &peer, &messages)?;
        let bootstrap = JoinBootstrap {
            version: 1,
            invite_id: redemption.join().invite_id(),
            device_cert: issued.device_cert,
            role: redemption.join().grant().role,
            room: self.state.room.clone(),
            workspace_id: qos_relay::room_workspace_id(&self.state.room)?,
            authority: peer.authority().clone(),
            certified_roster: peer.certificates(),
            revocations: peer.revocations(),
            records: peer.rows(),
            governance_messages: messages
                .iter()
                .map(postcard::to_stdvec)
                .collect::<Result<Vec<_>, _>>()?,
            generation,
        };
        let mut state = self.state.clone();
        state.outbox.extend(live);
        state
            .bound
            .as_mut()
            .ok_or_else(|| anyhow!("workspace not enrolled"))?
            .peer = peer
            .durable()
            .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
        state.join_challenges.remove(&key);
        state.join_responses.insert(
            key,
            StoredJoinResponse {
                proof_digest,
                bootstrap: bootstrap.clone(),
            },
        );
        state
            .invitations
            .get_mut(&hex::encode(bootstrap.invite_id))
            .ok_or_else(|| anyhow!("hosted invitation record disappeared"))?
            .status = InvitationStatus::Redeemed;
        self.commit(state)?;
        audit(
            "collaboration_invitation_redeemed",
            serde_json::json!({
                "room": self.state.room,
                "request_id": hex::encode(request_id),
                "invite_id": hex::encode(bootstrap.invite_id),
                "caller_device": hex::encode(remote_endpoint),
                "caller_root": hex::encode(bootstrap.device_cert.user_public_key),
                "role": bootstrap.role,
                "generation": self.state.generation.to_string(),
                "result": "committed",
            }),
        );
        Ok(bootstrap)
    }

    pub fn membership_challenge(
        &mut self,
        remote_endpoint: [u8; 32],
        request: MembershipChallengeRequest,
        now: u64,
    ) -> Result<MembershipChallengeResponse> {
        self.ensure_reconciled()?;
        if request.request_id == [0; 32]
            || remote_endpoint != request.device_cert.device_public_key
            || request.room != self.state.room
            || request.workspace_id != qos_relay::room_workspace_id(&self.state.room)?
        {
            bail!("membership challenge endpoint or workspace mismatch")
        }
        let request_digest = *blake3::hash(&postcard::to_stdvec(&request)?).as_bytes();
        let key = hex::encode(request.request_id);
        if let Some(stored) = self.state.membership_challenges.get(&key) {
            if stored.request_digest != request_digest || stored.device_cert != request.device_cert
            {
                bail!("membership request ID substitution")
            }
            return Ok(MembershipChallengeResponse {
                challenge: stored.challenge.clone(),
            });
        }
        let peer = self.peer_at(now)?;
        let mut nonce = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let challenge = peer
            .membership_challenge(
                request.workspace_id,
                &request.device_cert,
                request.request_id,
                nonce,
                now.saturating_add(60_000_000_000),
                now_time(now),
            )
            .map_err(anyhow::Error::msg)?;
        let mut state = self.state.clone();
        state.membership_challenges.insert(
            key,
            StoredMembershipChallenge {
                request_digest,
                device_cert: request.device_cert,
                challenge: challenge.clone(),
            },
        );
        self.commit(state)?;
        audit(
            "collaboration_membership_challenge_issued",
            serde_json::json!({
                "room": self.state.room,
                "request_id": hex::encode(request.request_id),
                "caller_device": hex::encode(remote_endpoint),
                "caller_root": hex::encode(challenge.user_public_key),
                "result": "committed",
            }),
        );
        Ok(MembershipChallengeResponse { challenge })
    }

    pub fn membership_complete(
        &mut self,
        remote_endpoint: [u8; 32],
        request: MembershipCompleteRequest,
        now: u64,
    ) -> Result<JoinBootstrap> {
        self.ensure_reconciled()?;
        let request_id = request.proof.challenge.request_id;
        let key = hex::encode(request_id);
        let proof_digest = *blake3::hash(&postcard::to_stdvec(&request)?).as_bytes();
        if let Some(stored) = self.state.membership_responses.get(&key) {
            if stored.proof_digest != proof_digest {
                bail!("membership proof request ID substitution")
            }
            self.verify_cached_membership(remote_endpoint, &stored.bootstrap, now)?;
            return Ok(stored.bootstrap.clone());
        }
        let issued = self
            .state
            .membership_challenges
            .get(&key)
            .cloned()
            .ok_or_else(|| anyhow!("membership challenge was not issued"))?;
        if issued.device_cert.device_public_key != remote_endpoint {
            bail!("membership proof endpoint mismatch")
        }
        let (before, mut peer) = self.peer_transition_at(now)?;
        let membership = verify_membership_proof(
            &request.proof,
            &issued.challenge,
            &issued.device_cert,
            &revocations(&peer.revocations())?,
            now,
        )
        .map_err(anyhow::Error::msg)?;
        let role = peer
            .verify_member_refresh(&membership, now_time(now))
            .map_err(anyhow::Error::msg)?;
        let invite_id = self
            .state
            .join_responses
            .values()
            .find(|response| response.bootstrap.device_cert.device_public_key == remote_endpoint)
            .map_or([0; 32], |response| response.bootstrap.invite_id);
        let generation = self.next_generation()?;
        let messages =
            crate::durable_bootstrap_messages(&mut peer, &self.key(), now_time(now), generation)?;
        let live = live_projection_from_bootstrap(&before, &peer, &messages)?;
        let bootstrap = JoinBootstrap {
            version: 1,
            invite_id,
            device_cert: issued.device_cert,
            role,
            room: self.state.room.clone(),
            workspace_id: qos_relay::room_workspace_id(&self.state.room)?,
            authority: peer.authority().clone(),
            certified_roster: peer.certificates(),
            revocations: peer.revocations(),
            records: peer.rows(),
            governance_messages: messages
                .iter()
                .map(postcard::to_stdvec)
                .collect::<Result<Vec<_>, _>>()?,
            generation,
        };
        let mut state = self.state.clone();
        state.outbox.extend(live);
        state
            .bound
            .as_mut()
            .ok_or_else(|| anyhow!("workspace not enrolled"))?
            .peer = peer
            .durable()
            .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
        state.membership_challenges.remove(&key);
        state.membership_responses.insert(
            key,
            StoredJoinResponse {
                proof_digest,
                bootstrap: bootstrap.clone(),
            },
        );
        self.commit(state)?;
        audit(
            "collaboration_membership_refreshed",
            serde_json::json!({
                "room": self.state.room,
                "request_id": hex::encode(request_id),
                "invite_id": hex::encode(bootstrap.invite_id),
                "caller_device": hex::encode(remote_endpoint),
                "caller_root": hex::encode(bootstrap.device_cert.user_public_key),
                "role": bootstrap.role,
                "generation": self.state.generation.to_string(),
                "result": "committed",
            }),
        );
        Ok(bootstrap)
    }
}

pub(crate) struct AccessUpdate {
    pub completion: tokio::sync::oneshot::Sender<Result<(), String>>,
}

#[derive(Clone, Debug)]
pub struct CloudJoinService {
    root: PathBuf,
    machine: String,
    access_updates: tokio::sync::mpsc::Sender<AccessUpdate>,
}

impl CloudJoinService {
    pub(crate) fn new(
        root: &Path,
        machine: &str,
        access_updates: tokio::sync::mpsc::Sender<AccessUpdate>,
    ) -> Self {
        Self {
            root: root.into(),
            machine: machine.into(),
            access_updates,
        }
    }

    fn unavailable() -> JoinTransportResponse {
        JoinTransportResponse::Error {
            code: 503,
            message: "workspace storage unavailable; reconcile before retry".into(),
        }
    }
    fn error(error: anyhow::Error) -> JoinTransportResponse {
        if crate::is_store_failure(&error) {
            return Self::unavailable();
        }
        JoinTransportResponse::Error {
            code: 400,
            message: error.to_string(),
        }
    }
}

impl WorkspaceJoinService for CloudJoinService {
    async fn handle(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        let now = qos_relay::clock::now_ns();
        match request {
            JoinTransportRequest::Inspect { signed_invitation } => {
                match Store::open(&self.root, &self.machine)
                    .and_then(|store| store.join_inspect(&signed_invitation, now))
                {
                    Ok(inspection) => JoinTransportResponse::Inspection(inspection),
                    Err(error) => Self::error(error),
                }
            }
            JoinTransportRequest::Challenge(request) => {
                match Store::open(&self.root, &self.machine)
                    .and_then(|mut store| store.join_challenge(remote_endpoint, request, now))
                {
                    Ok(challenge) => JoinTransportResponse::Challenge(challenge),
                    Err(error) => Self::error(error),
                }
            }
            JoinTransportRequest::Complete(request) => {
                let result = Store::open(&self.root, &self.machine).and_then(|mut store| {
                    let bootstrap = store.join_complete(remote_endpoint, request, now)?;
                    Ok(bootstrap)
                });
                let bootstrap = match result {
                    Ok(result) => result,
                    Err(error) => return Self::error(error),
                };
                let (completion, completed) = tokio::sync::oneshot::channel();
                if self
                    .access_updates
                    .send(AccessUpdate { completion })
                    .await
                    .is_err()
                {
                    return Self::unavailable();
                }
                match completed.await {
                    Ok(Ok(())) => match Store::open(&self.root, &self.machine).and_then(|store| {
                        store.verify_cached_membership(
                            remote_endpoint,
                            &bootstrap,
                            qos_relay::clock::now_ns(),
                        )
                    }) {
                        Ok(()) => JoinTransportResponse::Complete(bootstrap),
                        Err(error) => Self::error(error),
                    },
                    Ok(Err(_)) => Self::unavailable(),
                    Err(_) => Self::unavailable(),
                }
            }
            JoinTransportRequest::MembershipChallenge(request) => {
                match Store::open(&self.root, &self.machine)
                    .and_then(|mut store| store.membership_challenge(remote_endpoint, request, now))
                {
                    Ok(challenge) => JoinTransportResponse::MembershipChallenge(challenge),
                    Err(error) => Self::error(error),
                }
            }
            JoinTransportRequest::MembershipComplete(request) => {
                let result = Store::open(&self.root, &self.machine).and_then(|mut store| {
                    let refresh = store.membership_refresh(remote_endpoint, request, now)?;
                    Ok(refresh)
                });
                let refresh = match result {
                    Ok(result) => result,
                    Err(error) => return Self::error(error),
                };
                let (completion, completed) = tokio::sync::oneshot::channel();
                if self
                    .access_updates
                    .send(AccessUpdate { completion })
                    .await
                    .is_err()
                {
                    return Self::unavailable();
                }
                match completed.await {
                    Ok(Ok(())) => match Store::open(&self.root, &self.machine).and_then(|store| {
                        store.verify_membership_refresh(
                            remote_endpoint,
                            &refresh,
                            qos_relay::clock::now_ns(),
                        )
                    }) {
                        Ok(()) => refresh.into_response(),
                        Err(error) => Self::error(error),
                    },
                    Ok(Err(_)) => Self::unavailable(),
                    Err(_) => Self::unavailable(),
                }
            }
        }
    }
}
