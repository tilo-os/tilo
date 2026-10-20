//! Joining a room this device governs: the invitations it signs and the join
//! exchange it answers, the same exchange a workspace host answers for the
//! rooms it governs ([`tilo_delivery_protocol::workspace_join`]).
use crate::NativeWorkspaceOwner;
use ed25519_dalek::SigningKey;
use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc, time::SystemTime};
use tilo_delivery_protocol::workspace_join::{
    IrohWorkspaceHostHintV1, WorkspaceHostMode, WorkspaceJoinService,
};
use tilo_desktop_model::RoomId;
use tilo_peer_state::join::{
    self, JoinBootstrap, JoinChallenge, JoinChallengeResponse, JoinInspection,
    JoinTransportRequest, JoinTransportResponse, MemberRole, TransportHint, VerifiedJoin,
    VerifiedRedemption,
};
use tilo_user_identity::{DeviceCert, RevocationList};

/// Longest a join challenge stays open.
const CHALLENGE_NS: u64 = 60_000_000_000;
/// Most join exchanges in flight at once; the oldest goes first.
const MAX_OPEN_CHALLENGES: usize = 256;

/// What an invitation into a room this device governs grants and to whom.
pub struct GovernedInvitation {
    pub role: MemberRole,
    /// The only person who may redeem it, when it names one.
    pub recipient_user_public_key: Option<[u8; 32]>,
    pub invitee_name: Option<String>,
    pub expires_at_ns: u64,
    /// Where this device serves the room ([`NativeWorkspaceOwner::governed_hint`]).
    pub transport: Vec<TransportHint>,
}

impl NativeWorkspaceOwner {
    /// This device's signed statement that it serves `room`, which it
    /// governs, at `endpoint_addr`, reaching it through `packet_relay_url`
    /// when no direct path is found.
    ///
    /// # Errors
    /// The room is not governed here, or `key` is not its governor's.
    pub fn governed_hint(
        &self,
        room: RoomId,
        key: &SigningKey,
        endpoint_addr: iroh::EndpointAddr,
        packet_relay_url: String,
    ) -> Result<TransportHint, String> {
        let governor = self.governing(room, key)?;
        IrohWorkspaceHostHintV1 {
            version: 1,
            mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
            room: room.to_string(),
            workspace_id: workspace_id(room)?,
            governing_authority: governor.clone(),
            endpoint_addr,
            host_user_public_key: governor.user_public_key,
            host_device_certificate: governor,
            packet_relay_url,
        }
        .transport_hint(key)
        .map_err(|error| error.to_string())
    }

    /// Sign an invitation into `room`, which this device governs.
    ///
    /// # Errors
    /// The room is not governed here, `key` is not its governor's, or the
    /// invitation cannot be signed.
    pub fn issue_invitation(
        &self,
        room: RoomId,
        key: &SigningKey,
        invitation: GovernedInvitation,
    ) -> Result<Vec<u8>, String> {
        let governor = self.governing(room, key)?;
        join::issue_invitation(
            join::InviteGrant {
                workspace_id: workspace_id(room)?,
                room: room.to_string(),
                authority: governor.clone(),
                issuer: governor,
                delegation: None,
                role: invitation.role,
                nonce: random()?,
                recipient_user_public_key: invitation.recipient_user_public_key,
                invitee_name: invitation.invitee_name,
                expires_at_ns: invitation.expires_at_ns,
                transport: invitation.transport,
            },
            key,
        )
    }

    /// The governor of `room` when this device governs it and `key` is its.
    fn governing(&self, room: RoomId, key: &SigningKey) -> Result<DeviceCert, String> {
        self.with_current(room, |view| {
            if !view.governed_here()
                || view.governor().device_public_key != key.verifying_key().to_bytes()
            {
                return Err("this device does not govern the room".into());
            }
            Ok(view.governor().clone())
        })
    }

    /// Admit the member `redemption` names into `room` and answer with the
    /// room as the member starts from: its roster and its signed checkpoint
    /// at the next generation.
    fn admit_redemption(
        &self,
        room: RoomId,
        redemption: &VerifiedRedemption,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<JoinBootstrap, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if !candidate.governed_here()
            || candidate.governor().device_public_key != key.verifying_key().to_bytes()
        {
            return Err("this device does not govern the room".into());
        }
        candidate
            .peer
            .governance_mut()
            .admit_invited_member(redemption, key, now)?;
        let checkpoint = candidate
            .peer
            .admitted_room()
            .map(tilo_peer_state::durability::entries)
            .transpose()?
            .unwrap_or_default()
            .values()
            .map(|receipt| receipt.generation)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("room checkpoint generation exhausted")?;
        let governance_messages = candidate
            .peer
            .durable_bootstrap_messages(key, now, checkpoint)?
            .iter()
            .map(|message| postcard::to_stdvec(message).map_err(|error| error.to_string()))
            .collect::<Result<_, _>>()?;
        let bootstrap = JoinBootstrap {
            version: 1,
            invite_id: redemption.join().invite_id(),
            device_cert: redemption.certificate().clone(),
            role: redemption.join().grant().role,
            room: room.to_string(),
            workspace_id: redemption.join().grant().workspace_id,
            authority: candidate.governor().clone(),
            certified_roster: candidate.peer.certificates(),
            revocations: candidate.peer.revocations(),
            records: candidate.peer.rows(),
            governance_messages,
            generation: checkpoint,
        };
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_room(room, candidate, None)?;
        Ok(bootstrap)
    }
}

/// The join exchange for the rooms `owner` governs, answered as the device
/// whose key it holds: the challenge binds the authenticated endpoint, and
/// completion admits the member and returns the room's checkpoint.
#[derive(Clone)]
pub struct GovernedJoins(Arc<Joins>);

struct Joins {
    owner: NativeWorkspaceOwner,
    key: SigningKey,
    challenges: Mutex<Vec<(JoinChallenge, DeviceCert)>>,
    /// Answers already given, so a member that lost one gets it again.
    completions: Mutex<BTreeMap<[u8; 32], JoinBootstrap>>,
}

impl std::fmt::Debug for GovernedJoins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GovernedJoins")
    }
}

impl GovernedJoins {
    #[must_use]
    pub fn new(owner: NativeWorkspaceOwner, key: SigningKey) -> Self {
        Self(Arc::new(Joins {
            owner,
            key,
            challenges: Mutex::new(Vec::new()),
            completions: Mutex::new(BTreeMap::new()),
        }))
    }

    /// Answer one join request from the authenticated endpoint `remote`.
    #[must_use]
    pub fn answer(&self, remote: [u8; 32], request: JoinTransportRequest) -> JoinTransportResponse {
        let now = SystemTime::now();
        let result = match request {
            JoinTransportRequest::Inspect { signed_invitation } => {
                self.inspect(&signed_invitation, now).map(|join| {
                    JoinTransportResponse::Inspection(JoinInspection {
                        invite_id: join.invite_id(),
                        grant: join.grant().clone(),
                    })
                })
            }
            JoinTransportRequest::Challenge(request) => self
                .challenge(remote, request, now)
                .map(JoinTransportResponse::Challenge),
            JoinTransportRequest::Complete(request) => self
                .complete(remote, &request, now)
                .map(JoinTransportResponse::Complete),
            _ => Err("this device answers joins only".into()),
        };
        result.unwrap_or_else(|message| JoinTransportResponse::Error { code: 403, message })
    }

    fn revocations(&self, room: RoomId) -> Result<RevocationList, String> {
        self.0.owner.with_current(room, |view| {
            let mut list = RevocationList::new();
            for entry in view.peer().revocations() {
                list.insert(entry).map_err(|error| error.to_string())?;
            }
            Ok(list)
        })
    }

    /// The invitation, when it is this device's own into a room it governs.
    fn inspect(&self, signed_invitation: &[u8], now: SystemTime) -> Result<VerifiedJoin, String> {
        let at = crate::enrollment::ns(now)?;
        let unverified = join::decode_and_verify_invitation(
            signed_invitation,
            None,
            &RevocationList::new(),
            at,
        )?;
        let room = RoomId::parse(&unverified.grant().room).ok_or("invalid invitation room")?;
        let governor = self.0.owner.governing(room, &self.0.key)?;
        let verified = join::decode_and_verify_invitation(
            signed_invitation,
            None,
            &self.revocations(room)?,
            at,
        )?;
        let grant = verified.grant();
        if grant.authority != governor || grant.issuer != governor || grant.delegation.is_some() {
            return Err("the invitation is not this device's".into());
        }
        Ok(verified)
    }

    fn challenge(
        &self,
        remote: [u8; 32],
        request: join::JoinChallengeRequest,
        now: SystemTime,
    ) -> Result<JoinChallengeResponse, String> {
        if remote != request.device_cert.device_public_key {
            return Err("join certificate does not match authenticated transport endpoint".into());
        }
        if let Some((challenge, _)) = self.0.challenges.lock().iter().find(|(challenge, cert)| {
            challenge.request_id == request.request_id && *cert == request.device_cert
        }) {
            return Ok(JoinChallengeResponse {
                challenge: challenge.clone(),
            });
        }
        let verified = self.inspect(&request.signed_invitation, now)?;
        if verified
            .grant()
            .recipient_user_public_key
            .is_some_and(|recipient| recipient != request.device_cert.user_public_key)
        {
            return Err("invitation recipient does not match joining identity".into());
        }
        let at = crate::enrollment::ns(now)?;
        let challenge = JoinChallenge::new(
            &verified,
            &request.device_cert,
            request.request_id,
            random()?,
            verified
                .grant()
                .expires_at_ns
                .min(at.saturating_add(CHALLENGE_NS)),
            at,
        )?;
        {
            let mut challenges = self.0.challenges.lock();
            challenges.retain(|(open, _)| open.expires_at_ns > at);
            if challenges.len() >= MAX_OPEN_CHALLENGES {
                challenges.remove(0);
            }
            challenges.push((challenge.clone(), request.device_cert));
        }
        Ok(JoinChallengeResponse { challenge })
    }

    fn complete(
        &self,
        remote: [u8; 32],
        request: &join::JoinCompleteRequest,
        now: SystemTime,
    ) -> Result<JoinBootstrap, String> {
        let request_id = request.proof.challenge.request_id;
        if let Some(bootstrap) = self.0.completions.lock().get(&request_id) {
            return Ok(bootstrap.clone());
        }
        let (challenge, cert) = self
            .0
            .challenges
            .lock()
            .iter()
            .find(|(challenge, _)| *challenge == request.proof.challenge)
            .cloned()
            .ok_or("join challenge was not issued by this device")?;
        if cert.device_public_key != remote {
            return Err("join proof does not match authenticated transport endpoint".into());
        }
        let room = RoomId::parse(&challenge.room).ok_or("invalid challenge room")?;
        let redemption = join::verify_join_proof(
            &request.proof,
            &challenge,
            &cert,
            &self.revocations(room)?,
            crate::enrollment::ns(now)?,
        )?;
        let bootstrap = self
            .0
            .owner
            .admit_redemption(room, &redemption, &self.0.key, now)?;
        self.0
            .challenges
            .lock()
            .retain(|(open, _)| open.request_id != request_id);
        self.0
            .completions
            .lock()
            .insert(request_id, bootstrap.clone());
        Ok(bootstrap)
    }
}

impl WorkspaceJoinService for GovernedJoins {
    async fn handle(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        self.answer(remote_endpoint, request)
    }
}

fn workspace_id(room: RoomId) -> Result<u64, String> {
    tilo_relay_protocol::room_workspace_id(&room.to_string()).map_err(|error| error.to_string())
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

#[cfg(test)]
#[path = "governed_joins_tests.rs"]
mod tests;
