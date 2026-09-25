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
#[cfg(test)]
pub(crate) use owner_bootstrap::forget_reused_governance;
pub(crate) use owner_bootstrap::prune_legacy_receipts;

const JOIN_CHALLENGE_LIFETIME_NS: u64 = 300_000_000_000;
const MEMBERSHIP_CHALLENGE_LIFETIME_NS: u64 = 60_000_000_000;
/// Outstanding (unexpired, uncompleted) membership challenges per device and
/// in total. Issuance sweeps expired ones and evicts the oldest beyond these.
const MEMBERSHIP_CHALLENGES_PER_DEVICE: usize = 4;
const MEMBERSHIP_CHALLENGES_TOTAL: usize = 256;

/// Drops expired membership challenges, then keeps at most the newest
/// `MEMBERSHIP_CHALLENGES_PER_DEVICE - 1` for `device` and
/// `MEMBERSHIP_CHALLENGES_TOTAL - 1` overall, leaving room for one new one.
fn sweep_membership_challenges(
    challenges: &mut std::collections::BTreeMap<String, StoredMembershipChallenge>,
    device: [u8; 32],
    now: u64,
) {
    challenges.retain(|_, issued| issued.challenge.expires_at_ns > now);
    let evict = |challenges: &mut std::collections::BTreeMap<String, StoredMembershipChallenge>,
                 select: &dyn Fn(&StoredMembershipChallenge) -> bool,
                 keep: usize| {
        let mut selected = challenges
            .iter()
            .filter(|(_, issued)| select(issued))
            .map(|(key, issued)| (issued.challenge.expires_at_ns, key.clone()))
            .collect::<Vec<_>>();
        if selected.len() <= keep {
            return;
        }
        selected.sort();
        for (_, key) in selected.into_iter().rev().skip(keep) {
            challenges.remove(&key);
        }
    };
    evict(
        challenges,
        &|issued| issued.device_cert.device_public_key == device,
        MEMBERSHIP_CHALLENGES_PER_DEVICE - 1,
    );
    evict(challenges, &|_| true, MEMBERSHIP_CHALLENGES_TOTAL - 1);
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredJoinChallenge {
    request_digest: [u8; 32],
    device_cert: DeviceCert,
    challenge: JoinChallenge,
}

/// A completed join or member refresh. It retains only a compact receipt
/// (lane T): each whole bootstrap carried the room governance document, so a
/// few retained replies dominated the Store and every commit re-saved them.
/// A lost-reply retry is answered from current governed state instead (see
/// `replay_member_bootstrap`), byte-identical while nothing changed.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredJoinResponse {
    proof_digest: [u8; 32],
    /// Legacy only: the whole reply retained by older Stores. Never written
    /// again; converted to a receipt by the next Store commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bootstrap: Option<JoinBootstrap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receipt: Option<MemberReceipt>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemberReceipt {
    pub(crate) device: [u8; 32],
    pub(crate) invite_id: [u8; 32],
    pub(crate) role: qos_peer_state::join::MemberRole,
    pub(crate) generation: u64,
    pub(crate) completed_at_ns: u64,
}

/// A lost-reply retry of the same member request is answered only this long
/// after its completion (the browser retries within seconds and starts a new
/// request ID for every refresh).
const MEMBER_REPLAY_WINDOW_NS: u64 = 600_000_000_000;

impl StoredJoinResponse {
    fn completed(proof_digest: [u8; 32], receipt: MemberReceipt) -> Self {
        Self {
            proof_digest,
            bootstrap: None,
            receipt: Some(receipt),
        }
    }
    /// The receipt of this entry; a legacy entry is described by its reply
    /// with no replay window left.
    pub(crate) fn receipt(&self) -> Option<MemberReceipt> {
        self.receipt.clone().or_else(|| {
            self.bootstrap.as_ref().map(|bootstrap| MemberReceipt {
                device: bootstrap.device_cert.device_public_key,
                invite_id: bootstrap.invite_id,
                role: bootstrap.role,
                generation: bootstrap.generation,
                completed_at_ns: 0,
            })
        })
    }
    fn device(&self) -> Option<[u8; 32]> {
        self.receipt().map(|receipt| receipt.device)
    }
}

/// Converts legacy whole-reply entries to compact receipts. Runs on every
/// Store commit, so an existing large Store shrinks on its next write.
pub(crate) fn compact_member_responses(
    responses: &mut std::collections::BTreeMap<String, StoredJoinResponse>,
) {
    for response in responses.values_mut() {
        if response.bootstrap.is_some() {
            response.receipt = response.receipt();
            response.bootstrap = None;
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredMembershipChallenge {
    request_digest: [u8; 32],
    device_cert: DeviceCert,
    challenge: MembershipChallenge,
    /// The one proof a no-commit completion accepted for this challenge
    /// (process-local only; see `issued`).
    #[serde(skip)]
    completed_proof: Option<[u8; 32]>,
}

/// Outstanding membership challenges, held in process memory only (lane M).
///
/// Before, every issuance was a Store commit, and an idle owner + guest
/// re-challenging every few seconds grew state.json and the journal without
/// any membership change. A challenge is short-lived (60 s) proof-of-fresh-
/// possession material; it needs no durability:
/// - Freshness is unchanged: the host still draws a fresh random nonce per
///   request ID, the proof is still verified against the host-held challenge
///   (nonce, device certificate, request ID, expiry, current revocations) and
///   current governed membership is rechecked on every completion.
/// - One use: a completion that commits removes the challenge; a completion
///   answered without a commit binds it to that exact proof digest, so the
///   same request ID can only ever complete with that byte-identical proof
///   (a lost-reply retry by the same authenticated endpoint) until expiry.
/// - Restart: this map dies with the process. A proof for a challenge issued
///   before a host restart finds no challenge and is refused ("membership
///   challenge was not issued"); the client must request a new challenge.
///   Durable lost-reply receipts (member and owner) are unchanged and still
///   bind the exact proof the Store verified before it committed.
/// - Legacy persisted challenges are never read and are dropped by the next
///   Store commit.
mod issued {
    use super::StoredMembershipChallenge;
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
        sync::Mutex,
    };
    type Challenges = BTreeMap<String, StoredMembershipChallenge>;
    static ISSUED: Mutex<BTreeMap<PathBuf, Challenges>> = Mutex::new(BTreeMap::new());
    pub(super) fn with<T>(root: &Path, f: impl FnOnce(&mut Challenges) -> T) -> T {
        let mut all = ISSUED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let challenges = all.entry(root.to_owned()).or_default();
        let result = f(challenges);
        if challenges.is_empty() {
            all.remove(root);
        }
        result
    }
    pub(super) fn get(root: &Path, key: &str) -> Option<StoredMembershipChallenge> {
        with(root, |challenges| challenges.get(key).cloned())
    }
    pub(super) fn remove(root: &Path, key: &str) {
        with(root, |challenges| challenges.remove(key));
    }
    /// Binds a challenge to the one proof a no-commit completion accepted.
    pub(super) fn complete(root: &Path, key: &str, proof_digest: [u8; 32]) {
        with(root, |challenges| {
            if let Some(issued) = challenges.get_mut(key) {
                issued.completed_proof = Some(proof_digest);
            }
        });
    }
    /// Refuses a second, different proof for an already completed challenge.
    pub(super) fn check_proof(
        issued: &StoredMembershipChallenge,
        proof_digest: [u8; 32],
    ) -> anyhow::Result<()> {
        if issued
            .completed_proof
            .is_some_and(|completed| completed != proof_digest)
        {
            anyhow::bail!("membership proof request ID substitution")
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn snapshot(root: &Path) -> Challenges {
        with(root, |challenges| challenges.clone())
    }
    #[cfg(test)]
    pub(crate) fn forget(root: &Path) {
        with(root, Challenges::clear);
    }
}
#[cfg(test)]
pub(crate) use issued::{forget as forget_issued_challenges, snapshot as issued_challenges};

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

    /// Answers a lost-reply retry of a completed join or member refresh from
    /// current governed state. The retained receipt binds the exact proof
    /// this Store verified (checked by the caller) to the device, invitation
    /// and role it completed with; current membership is always rechecked.
    /// While the governed state is the one the reply was built from, the
    /// retained signed governance is reused (byte-identical reply, no commit);
    /// otherwise an equivalent bootstrap at a new generation is committed.
    fn replay_member_bootstrap(
        &mut self,
        remote_endpoint: [u8; 32],
        stored: &StoredJoinResponse,
        now: u64,
    ) -> Result<JoinBootstrap> {
        if let Some(bootstrap) = &stored.bootstrap {
            // Legacy retained reply (compacted by the next commit).
            self.verify_cached_membership(remote_endpoint, bootstrap, now)?;
            return Ok(bootstrap.clone());
        }
        let receipt = stored
            .receipt()
            .ok_or_else(|| anyhow!("member receipt is absent"))?;
        if receipt.device != remote_endpoint {
            bail!("cached join response endpoint mismatch")
        }
        if receipt.completed_at_ns > now
            || now - receipt.completed_at_ns >= MEMBER_REPLAY_WINDOW_NS
            || receipt.generation > self.state.generation
        {
            bail!("member receipt expired; request a new membership challenge")
        }
        let (before, mut peer) = self.peer_transition_at(now)?;
        let device_cert = peer
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == remote_endpoint)
            .ok_or_else(|| anyhow!("cached join certificate is absent from governed state"))?;
        let reused = if crate::live_room_publication(&before, &peer)?.is_none() {
            owner_bootstrap::reused_governance(&self.root, &self.state)?
        } else {
            None
        };
        let (generation, governance_messages, next) = match reused {
            Some((generation, messages)) => (generation, messages, None),
            None => {
                let generation = self.next_generation()?;
                let messages = crate::durable_bootstrap_messages(
                    &mut peer,
                    &self.key(),
                    now_time(now),
                    generation,
                )?;
                let live = live_projection_from_bootstrap(&before, &peer, &messages)?;
                let messages = messages
                    .iter()
                    .map(postcard::to_stdvec)
                    .collect::<Result<Vec<_>, _>>()?;
                let mut state = self.state.clone();
                state.outbox.extend(live);
                state
                    .bound
                    .as_mut()
                    .ok_or_else(|| anyhow!("workspace not enrolled"))?
                    .peer = peer
                    .durable()
                    .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
                (generation, messages, Some(state))
            }
        };
        let bootstrap = JoinBootstrap {
            version: 1,
            invite_id: receipt.invite_id,
            device_cert,
            role: receipt.role,
            room: self.state.room.clone(),
            workspace_id: qos_relay::room_workspace_id(&self.state.room)?,
            authority: peer.authority().clone(),
            certified_roster: peer.certificates(),
            revocations: peer.revocations(),
            records: peer.rows(),
            governance_messages,
            generation,
        };
        let result = if next.is_some() {
            "replay_committed"
        } else {
            "replayed"
        };
        if let Some(state) = next {
            // Current membership (same role, active device) before any commit.
            self.verify_cached_membership(remote_endpoint, &bootstrap, now)?;
            owner_bootstrap::remember_governance(
                &self.root,
                &state,
                generation,
                &bootstrap.governance_messages,
            )?;
            self.commit(state)?;
        }
        self.verify_cached_membership(remote_endpoint, &bootstrap, now)?;
        audit(
            "collaboration_member_reply_replayed",
            serde_json::json!({
                "room": self.state.room,
                "caller_device": hex::encode(remote_endpoint),
                "caller_root": hex::encode(bootstrap.device_cert.user_public_key),
                "generation": generation.to_string(),
                "result": result,
            }),
        );
        Ok(bootstrap)
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
        if let Some(stored) = self.state.join_responses.get(&key).cloned() {
            if stored.proof_digest != proof_digest {
                bail!("join proof request ID substitution")
            }
            return self.replay_member_bootstrap(remote_endpoint, &stored, now);
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
            StoredJoinResponse::completed(
                proof_digest,
                MemberReceipt {
                    device: remote_endpoint,
                    invite_id: bootstrap.invite_id,
                    role: bootstrap.role,
                    generation,
                    completed_at_ns: now,
                },
            ),
        );
        state
            .invitations
            .get_mut(&hex::encode(bootstrap.invite_id))
            .ok_or_else(|| anyhow!("hosted invitation record disappeared"))?
            .status = InvitationStatus::Redeemed;
        owner_bootstrap::remember_governance(
            &self.root,
            &state,
            generation,
            &bootstrap.governance_messages,
        )?;
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
        if let Some(stored) =
            issued::get(&self.root, &key).filter(|stored| stored.challenge.expires_at_ns > now)
        {
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
                now.saturating_add(MEMBERSHIP_CHALLENGE_LIFETIME_NS),
                now_time(now),
            )
            .map_err(anyhow::Error::msg)?;
        // Process-local only: issuing a challenge changes no governed or
        // durable state, so it makes no Store commit (see `issued`).
        issued::with(&self.root, |challenges| {
            sweep_membership_challenges(challenges, request.device_cert.device_public_key, now);
            challenges.insert(
                key,
                StoredMembershipChallenge {
                    request_digest,
                    device_cert: request.device_cert,
                    challenge: challenge.clone(),
                    completed_proof: None,
                },
            );
        });
        audit(
            "collaboration_membership_challenge_issued",
            serde_json::json!({
                "room": self.state.room,
                "request_id": hex::encode(request.request_id),
                "caller_device": hex::encode(remote_endpoint),
                "caller_root": hex::encode(challenge.user_public_key),
                "result": "issued",
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
        if let Some(stored) = self.state.membership_responses.get(&key).cloned() {
            if stored.proof_digest != proof_digest {
                bail!("membership proof request ID substitution")
            }
            return self.replay_member_bootstrap(remote_endpoint, &stored, now);
        }
        let issued = issued::get(&self.root, &key)
            .ok_or_else(|| anyhow!("membership challenge was not issued"))?;
        if issued.device_cert.device_public_key != remote_endpoint {
            bail!("membership proof endpoint mismatch")
        }
        issued::check_proof(&issued, proof_digest)?;
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
            .filter_map(StoredJoinResponse::receipt)
            .find(|receipt| receipt.device == remote_endpoint)
            .map_or([0; 32], |receipt| receipt.invite_id);
        // Nothing governed changed since the last bootstrap was built from
        // this exact state: reuse its signed governance without a commit
        // (lane M). The proof above was fully verified against the host-held
        // one-use challenge and current membership; the challenge is now bound
        // to this exact proof.
        if crate::live_room_publication(&before, &peer)?.is_none() {
            if let Some((generation, governance_messages)) =
                owner_bootstrap::reused_governance(&self.root, &self.state)?
            {
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
                    governance_messages,
                    generation,
                };
                self.verify_cached_membership(remote_endpoint, &bootstrap, now)?;
                issued::complete(&self.root, &key, proof_digest);
                audit(
                    "collaboration_membership_refreshed",
                    serde_json::json!({
                        "room": self.state.room,
                        "request_id": hex::encode(request_id),
                        "invite_id": hex::encode(bootstrap.invite_id),
                        "caller_device": hex::encode(remote_endpoint),
                        "caller_root": hex::encode(bootstrap.device_cert.user_public_key),
                        "role": bootstrap.role,
                        "generation": generation.to_string(),
                        "result": "reused",
                    }),
                );
                return Ok(bootstrap);
            }
        }
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
        // Keep only this device's latest refresh receipt: an older request
        // ID is no longer answerable (its challenge is consumed).
        state
            .membership_responses
            .retain(|_, response| response.device() != Some(remote_endpoint));
        state.membership_responses.insert(
            key.clone(),
            StoredJoinResponse::completed(
                proof_digest,
                MemberReceipt {
                    device: remote_endpoint,
                    invite_id,
                    role,
                    generation,
                    completed_at_ns: now,
                },
            ),
        );
        owner_bootstrap::remember_governance(
            &self.root,
            &state,
            generation,
            &bootstrap.governance_messages,
        )?;
        self.commit(state)?;
        // Consumed: a lost-reply retry is now answered from the durable receipt.
        issued::remove(&self.root, &key);
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
