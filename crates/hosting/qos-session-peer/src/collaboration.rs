//! Shared-workspace invitation service backed by the serialized native owner.
//!
//! The cryptographic and governed-document contract lives in
//! `qos_peer_state::join`; this module only owns native-host request
//! idempotency and the `SessionOwner` durability barrier.

mod media;
mod resources;

use crate::SessionOwner;
use qos_peer_state::GovernedAccess;
use qos_peer_state::join::{
    InviteGrant, JoinBootstrap, JoinChallenge, JoinChallengeRequest, JoinChallengeResponse,
    JoinCompleteRequest, MemberRole, MembershipChallenge, MembershipChallengeRequest,
    MembershipChallengeResponse, MembershipCompleteRequest, TransportHint, VerifiedJoin,
    decode_and_verify_invitation, issue_invitation, verify_join_proof, verify_membership_proof,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{fmt, time::Duration};
use tilo_user_identity::RevocationList;
use web_time::SystemTime;

pub(crate) struct CollaborationCommand {
    pub(crate) remote_endpoint: [u8; 32],
    pub(crate) request: qos_peer_state::join::JoinTransportRequest,
    pub(crate) reply: tokio::sync::oneshot::Sender<qos_peer_state::join::JoinTransportResponse>,
}

/// Cloneable owner-neutral bridge from the Iroh handler into the serialized owner loop.
#[derive(Clone, Debug)]
pub struct CollaborationHandle {
    sender: tokio::sync::mpsc::Sender<CollaborationCommand>,
}

pub(crate) struct CollaborationAdminCommand {
    pub(crate) request: qos_action_broker::collaboration_admin::CollaborationAdminRequest,
    pub(crate) reply: tokio::sync::oneshot::Sender<
        Result<serde_json::Value, qos_action_broker::collaboration_admin::CollaborationAdminError>,
    >,
}

#[derive(Clone, Debug)]
pub(crate) struct CollaborationAdminHandle {
    room: String,
    media_governance: bool,
    sender: tokio::sync::mpsc::Sender<CollaborationAdminCommand>,
}

impl CollaborationAdminHandle {
    pub(crate) fn bounded(
        room: String,
        capacity: usize,
        media_governance: bool,
    ) -> (Self, tokio::sync::mpsc::Receiver<CollaborationAdminCommand>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
        (
            Self {
                room,
                sender,
                media_governance,
            },
            receiver,
        )
    }
}

impl qos_action_broker::collaboration_admin::CollaborationAdmin for CollaborationAdminHandle {
    fn room(&self) -> &str {
        &self.room
    }

    fn supports_media_governance(&self) -> bool {
        self.media_governance
    }

    fn execute(
        &self,
        request: qos_action_broker::collaboration_admin::CollaborationAdminRequest,
    ) -> qos_action_broker::collaboration_admin::CollaborationAdminFuture {
        let sender = self.sender.clone();
        Box::pin(async move {
            let (reply, response) = tokio::sync::oneshot::channel();
            sender
                .send(CollaborationAdminCommand { request, reply })
                .await
                .map_err(|_| {
                    qos_action_broker::collaboration_admin::CollaborationAdminError::Uncertain(
                        "workspace collaboration authority is unavailable".into(),
                    )
                })?;
            response.await.map_err(|_| {
                qos_action_broker::collaboration_admin::CollaborationAdminError::Uncertain(
                    "workspace collaboration authority stopped before durable completion".into(),
                )
            })?
        })
    }
}

impl CollaborationHandle {
    pub(crate) fn bounded(
        capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<CollaborationCommand>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
        (Self { sender }, receiver)
    }
}

impl tilo_relay::workspace_join::WorkspaceJoinService for CollaborationHandle {
    async fn handle(
        &self,
        remote_endpoint: [u8; 32],
        request: qos_peer_state::join::JoinTransportRequest,
    ) -> qos_peer_state::join::JoinTransportResponse {
        let (reply, response) = tokio::sync::oneshot::channel();
        if self
            .sender
            .send(CollaborationCommand {
                remote_endpoint,
                request,
                reply,
            })
            .await
            .is_err()
        {
            return qos_peer_state::join::JoinTransportResponse::Error {
                code: 503,
                message: "workspace authority is unavailable".into(),
            };
        }
        response
            .await
            .unwrap_or(qos_peer_state::join::JoinTransportResponse::Error {
                code: 503,
                message: "workspace authority stopped before completing the request".into(),
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueInvitationRequest {
    pub request_id: [u8; 32],
    pub role: MemberRole,
    pub recipient_user_public_key: Option<[u8; 32]>,
    pub invitee_name: Option<String>,
    pub expires_at_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedInvitation {
    pub invite_id: [u8; 32],
    pub signed_invitation: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionIssuedInvitation {
    pub(crate) issued: IssuedInvitation,
    pub(crate) expires_at_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelInvitationRequest {
    pub request_id: [u8; 32],
    pub invite_id: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvitation {
    pub invite_id: [u8; 32],
    pub role: MemberRole,
    pub recipient_user_public_key: Option<[u8; 32]>,
    pub invitee_name: Option<String>,
    pub expires_at_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMemberRoleRequest {
    pub request_id: [u8; 32],
    pub user_public_key: [u8; 32],
    pub role: MemberRole,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveMemberRequest {
    pub request_id: [u8; 32],
    pub user_public_key: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinCompletion {
    pub bootstrap: JoinBootstrap,
    pub access: GovernedAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceCompletion {
    pub generation: u64,
    pub access: GovernedAccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationError(String);

impl CollaborationError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CollaborationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for CollaborationError {}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct CollaborationStateV4 {
    pub(crate) invitations: Vec<InvitationRecord>,
    pub(crate) join_challenges: Vec<JoinChallengeRecord>,
    pub(crate) join_completions: Vec<JoinCompletionRecord>,
    pub(crate) membership_challenges: Vec<MembershipChallengeRecord>,
    pub(crate) administration: Vec<AdministrationRecordV4>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AdministrationRecordV4 {
    pub(crate) request_id: [u8; 32],
    pub(crate) request_digest: [u8; 32],
    pub(crate) generation: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct CollaborationState {
    pub(crate) invitations: Vec<InvitationRecord>,
    pub(crate) join_challenges: Vec<JoinChallengeRecord>,
    pub(crate) join_completions: Vec<JoinCompletionRecord>,
    pub(crate) membership_challenges: Vec<MembershipChallengeRecord>,
    pub(crate) administration: Vec<AdministrationRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct InvitationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
    issued: IssuedInvitation,
    grant: InviteGrant,
    cancellation: Option<CancellationRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CancellationRecord {
    request_id: [u8; 32],
    request_digest: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct JoinChallengeRecord {
    pub(crate) remote_endpoint: [u8; 32],
    pub(crate) request_digest: [u8; 32],
    pub(crate) device_cert: tilo_user_identity::DeviceCert,
    pub(crate) challenge: JoinChallenge,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct JoinCompletionRecord {
    pub(crate) remote_endpoint: [u8; 32],
    pub(crate) request_id: [u8; 32],
    pub(crate) proof_digest: [u8; 32],
    pub(crate) bootstrap: JoinBootstrap,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct MembershipChallengeRecord {
    pub(crate) remote_endpoint: [u8; 32],
    pub(crate) request_digest: [u8; 32],
    pub(crate) device_cert: tilo_user_identity::DeviceCert,
    pub(crate) challenge: MembershipChallenge,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AdministrationRecord {
    pub(crate) request_id: [u8; 32],
    pub(crate) request_digest: [u8; 32],
    pub(crate) generation: u64,
    pub(crate) media_result: Option<qos_peer_state::room::media::MediaCallResultV1>,
}

#[derive(Clone, Debug)]
pub struct CollaborationService {
    transport: Vec<TransportHint>,
}

impl CollaborationService {
    #[must_use]
    pub fn new(transport: Vec<TransportHint>) -> Self {
        Self { transport }
    }

    pub fn issue(
        &self,
        owner: &mut SessionOwner,
        administrator: [u8; 32],
        request: IssueInvitationRequest,
        now_ns: u64,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let request_digest = request_digest(&request)?;
        self.issue_with_digest(owner, administrator, request, request_digest, now_ns)
    }

    pub(crate) fn issue_from_action(
        &self,
        owner: &mut SessionOwner,
        administrator: [u8; 32],
        request_id: [u8; 32],
        role: MemberRole,
        invitee_name: Option<String>,
        expires_in_seconds: u64,
        now_ns: u64,
    ) -> Result<ActionIssuedInvitation, CollaborationError> {
        #[derive(Serialize)]
        struct RelativeIssueIntent<'a> {
            role: MemberRole,
            invitee_name: &'a Option<String>,
            expires_in_seconds: u64,
        }
        if expires_in_seconds == 0 || expires_in_seconds > 30 * 24 * 60 * 60 {
            return Err(CollaborationError::new("invitation lifetime is invalid"));
        }
        let digest = request_digest(&RelativeIssueIntent {
            role,
            invitee_name: &invitee_name,
            expires_in_seconds,
        })?;
        let request = IssueInvitationRequest {
            request_id,
            role,
            recipient_user_public_key: None,
            invitee_name,
            expires_at_ns: now_ns.saturating_add(expires_in_seconds.saturating_mul(1_000_000_000)),
        };
        let issued = self.issue_with_digest(owner, administrator, request, digest, now_ns)?;
        let expires_at_ns = owner
            .collaboration_state()
            .invitations
            .iter()
            .find(|record| record.request_id == request_id)
            .map(|record| record.grant.expires_at_ns)
            .ok_or_else(|| CollaborationError::new("committed invitation record is missing"))?;
        Ok(ActionIssuedInvitation {
            issued,
            expires_at_ns,
        })
    }

    fn issue_with_digest(
        &self,
        owner: &mut SessionOwner,
        administrator: [u8; 32],
        request: IssueInvitationRequest,
        request_digest: [u8; 32],
        now_ns: u64,
    ) -> Result<IssuedInvitation, CollaborationError> {
        authorize_administrator(
            owner,
            administrator,
            now_ns,
            request.request_id,
            "invitation.issue",
        )?;
        if let Some(record) = owner
            .collaboration_state()
            .invitations
            .iter()
            .find(|record| record.request_id == request.request_id)
        {
            if record.request_digest != request_digest {
                return Err(CollaborationError::new(
                    "invitation request ID already names a different request",
                ));
            }
            return Ok(record.issued.clone());
        }
        validate_issue_request(&request, now_ns)?;

        let mut nonce = [0; 16];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let (candidate, delegation) = owner
            .prepare_workspace_host_delegation(system_time(now_ns))
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        let grant = InviteGrant {
            workspace_id: owner.workspace_id(),
            room: owner.peer().room().to_owned(),
            authority: owner.peer().authority().clone(),
            issuer: owner.host_certificate().clone(),
            delegation: Some(delegation),
            role: request.role,
            nonce,
            recipient_user_public_key: request.recipient_user_public_key,
            invitee_name: request.invitee_name,
            expires_at_ns: request.expires_at_ns,
            transport: self.transport.clone(),
        };
        let signed_invitation = owner
            .sign_invitation(grant.clone())
            .map_err(CollaborationError::new)?;
        let issued = IssuedInvitation {
            invite_id: *blake3::hash(&signed_invitation).as_bytes(),
            signed_invitation,
        };
        let mut state = owner.collaboration_state().clone();
        state.invitations.push(InvitationRecord {
            request_id: request.request_id,
            request_digest,
            issued: issued.clone(),
            grant,
            cancellation: None,
        });
        owner
            .commit_collaboration_candidate(candidate, state)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        tracing::info!(
            room = %owner.peer().room(),
            invite_id = %hex::encode(issued.invite_id),
            request_id = %hex::encode(request.request_id),
            role = ?request.role,
            outcome = "issued",
            "workspace invitation issued"
        );
        Ok(issued)
    }

    pub fn cancel(
        &self,
        owner: &mut SessionOwner,
        administrator: [u8; 32],
        request: CancelInvitationRequest,
    ) -> Result<(), CollaborationError> {
        authorize_administrator(
            owner,
            administrator,
            current_time_ns(),
            request.request_id,
            "invitation.cancel",
        )?;
        let digest = request_digest(&request)?;
        let mut state = owner.collaboration_state().clone();
        if let Some(prior) = state
            .invitations
            .iter()
            .filter_map(|record| record.cancellation.as_ref())
            .find(|cancelled| cancelled.request_id == request.request_id)
        {
            if prior.request_digest != digest {
                return Err(CollaborationError::new(
                    "invitation cancellation request ID already names a different request",
                ));
            }
            return Ok(());
        }
        let record = state
            .invitations
            .iter_mut()
            .find(|record| record.issued.invite_id == request.invite_id)
            .ok_or_else(|| CollaborationError::new("invitation does not exist"))?;
        if let Some(cancelled) = &record.cancellation {
            if cancelled.request_id != request.request_id {
                return Err(CollaborationError::new(
                    "invitation was already cancelled by a different request",
                ));
            }
            return Ok(());
        }
        record.cancellation = Some(CancellationRecord {
            request_id: request.request_id,
            request_digest: digest,
        });
        owner
            .commit_collaboration_state(state)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        tracing::info!(
            room = %owner.peer().room(),
            invite_id = %hex::encode(request.invite_id),
            request_id = %hex::encode(request.request_id),
            outcome = "revoked",
            "workspace invitation cancelled"
        );
        Ok(())
    }

    #[must_use]
    pub fn pending(
        &self,
        owner: &SessionOwner,
        administrator: [u8; 32],
        now_ns: u64,
    ) -> Result<Vec<PendingInvitation>, CollaborationError> {
        authorize_administrator(owner, administrator, now_ns, [0; 32], "invitation.pending")?;
        let mut invitations = owner
            .collaboration_state()
            .invitations
            .iter()
            .filter(|record| record.cancellation.is_none() && record.grant.expires_at_ns > now_ns)
            .map(|record| PendingInvitation {
                invite_id: record.issued.invite_id,
                role: record.grant.role,
                recipient_user_public_key: record.grant.recipient_user_public_key,
                invitee_name: record.grant.invitee_name.clone(),
                expires_at_ns: record.grant.expires_at_ns,
            })
            .collect::<Vec<_>>();
        invitations.sort_by_key(|invitation| invitation.invite_id);
        Ok(invitations)
    }

    pub fn invitation_inventory(
        &self,
        owner: &SessionOwner,
        administrator: [u8; 32],
        now_ns: u64,
    ) -> Result<serde_json::Value, CollaborationError> {
        authorize_administrator(owner, administrator, now_ns, [0; 32], "invitation.list")?;
        let invitations = owner
            .collaboration_state()
            .invitations
            .iter()
            .map(|record| {
                let redeemed = owner
                    .collaboration_state()
                    .join_completions
                    .iter()
                    .any(|completion| completion.bootstrap.invite_id == record.issued.invite_id);
                let status = if redeemed {
                    "redeemed"
                } else if record.cancellation.is_some() || record.grant.expires_at_ns <= now_ns {
                    "revoked"
                } else {
                    "pending"
                };
                serde_json::json!({
                    "id": hex::encode(record.issued.invite_id),
                    "role": role_name(record.grant.role),
                    "expires_at_ms": record.grant.expires_at_ns / 1_000_000,
                    "status": status,
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({
            "generation": owner.committed_generation().to_string(),
            "invitations": invitations,
        }))
    }

    pub fn inspect(
        &self,
        owner: &SessionOwner,
        signed_invitation: &[u8],
        now_ns: u64,
    ) -> Result<VerifiedJoin, CollaborationError> {
        require_active_schema(owner)?;
        let invite_id = *blake3::hash(signed_invitation).as_bytes();
        let record = owner
            .collaboration_state()
            .invitations
            .iter()
            .find(|record| record.issued.invite_id == invite_id)
            .ok_or_else(|| CollaborationError::new("invitation was not issued by this host"))?;
        if record.issued.signed_invitation != signed_invitation {
            return Err(CollaborationError::new(
                "invitation bytes do not match issuance",
            ));
        }
        if record.cancellation.is_some() {
            return Err(CollaborationError::new("invitation was cancelled"));
        }
        decode_and_verify_invitation(signed_invitation, None, &revocations(owner)?, now_ns)
            .map_err(CollaborationError::new)
    }

    pub fn challenge(
        &self,
        owner: &mut SessionOwner,
        remote_endpoint: [u8; 32],
        request: JoinChallengeRequest,
        now_ns: u64,
    ) -> Result<JoinChallengeResponse, CollaborationError> {
        require_active_schema(owner)?;
        if remote_endpoint != request.device_cert.device_public_key {
            return Err(CollaborationError::new(
                "join certificate does not match authenticated transport endpoint",
            ));
        }
        let request_digest = request_digest(&(remote_endpoint, &request))?;
        if let Some(prior) = owner
            .collaboration_state()
            .join_challenges
            .iter()
            .find(|prior| {
                prior.challenge.request_id == request.request_id
                    && prior.challenge.expires_at_ns > now_ns
            })
        {
            if prior.request_digest != request_digest {
                return Err(CollaborationError::new(
                    "join request ID already names a different challenge",
                ));
            }
            return Ok(JoinChallengeResponse {
                challenge: prior.challenge.clone(),
            });
        }
        let verified = self.inspect(owner, &request.signed_invitation, now_ns)?;
        if verified
            .grant()
            .recipient_user_public_key
            .is_some_and(|recipient| recipient != request.device_cert.user_public_key)
        {
            return Err(CollaborationError::new(
                "invitation recipient does not match joining identity",
            ));
        }
        let mut nonce = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let expires_at_ns = verified
            .grant()
            .expires_at_ns
            .min(now_ns.saturating_add(60_000_000_000));
        let challenge = JoinChallenge::new(
            &verified,
            &request.device_cert,
            request.request_id,
            nonce,
            expires_at_ns,
            now_ns,
        )
        .map_err(CollaborationError::new)?;
        let mut state = owner.collaboration_state().clone();
        state
            .join_challenges
            .retain(|record| record.challenge.expires_at_ns > now_ns);
        if state.join_challenges.len() >= 4096 {
            return Err(CollaborationError::new(
                "pending join challenge limit reached",
            ));
        }
        state.join_challenges.push(JoinChallengeRecord {
            remote_endpoint,
            request_digest,
            device_cert: request.device_cert,
            challenge: challenge.clone(),
        });
        owner
            .commit_collaboration_state(state)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        Ok(JoinChallengeResponse { challenge })
    }

    pub fn complete(
        &self,
        owner: &mut SessionOwner,
        remote_endpoint: [u8; 32],
        request: JoinCompleteRequest,
        now_ns: u64,
    ) -> Result<JoinCompletion, CollaborationError> {
        require_active_schema(owner)?;
        let request_id = request.proof.challenge.request_id;
        let proof_digest = request_digest(&request.proof)?;
        if let Some(prior) = owner
            .collaboration_state()
            .join_completions
            .iter()
            .find(|prior| prior.request_id == request_id)
        {
            if prior.remote_endpoint != remote_endpoint || prior.proof_digest != proof_digest {
                return Err(CollaborationError::new(
                    "join request ID already completed with different proof",
                ));
            }
            owner
                .peer()
                .permissions(
                    prior.bootstrap.device_cert.device_public_key,
                    system_time(now_ns),
                )
                .map_err(CollaborationError::new)?;
            return Ok(JoinCompletion {
                bootstrap: prior.bootstrap.clone(),
                access: owner
                    .peer()
                    .governed_access(system_time(now_ns))
                    .map_err(CollaborationError::new)?,
            });
        }
        let challenge = owner
            .collaboration_state()
            .join_challenges
            .iter()
            .find(|prior| prior.challenge == request.proof.challenge)
            .cloned()
            .ok_or_else(|| CollaborationError::new("join challenge was not issued by this host"))?;
        if challenge.remote_endpoint != remote_endpoint
            || challenge.device_cert.device_public_key != remote_endpoint
        {
            return Err(CollaborationError::new(
                "join proof does not match authenticated transport endpoint",
            ));
        }
        let redemption = verify_join_proof(
            &request.proof,
            &challenge.challenge,
            &challenge.device_cert,
            &revocations(owner)?,
            now_ns,
        )
        .map_err(CollaborationError::new)?;
        let issued = owner
            .collaboration_state()
            .invitations
            .iter()
            .find(|record| record.issued.invite_id == redemption.join().invite_id())
            .ok_or_else(|| CollaborationError::new("invitation was not issued by this host"))?;
        if issued.cancellation.is_some() {
            return Err(CollaborationError::new("invitation was cancelled"));
        }
        let candidate = match owner.prepare_invited_member(&redemption, system_time(now_ns)) {
            Ok(candidate) => candidate,
            Err(error) => {
                tracing::warn!(
                    room = %owner.peer().room(),
                    invite_id = %hex::encode(redemption.join().invite_id()),
                    request_id = %hex::encode(request_id),
                    person = %hex::encode(redemption.certificate().user_public_key),
                    device = %hex::encode(redemption.certificate().device_public_key),
                    outcome = "replay_rejected",
                    reason = %error,
                    "workspace invitation redemption rejected"
                );
                return Err(CollaborationError::new(error.to_string()));
            }
        };
        let access = candidate
            .governed_access(system_time(now_ns))
            .map_err(CollaborationError::new)?;
        let bootstrap = owner
            .commit_join_redemption(
                candidate,
                owner.collaboration_state().clone(),
                remote_endpoint,
                request_id,
                proof_digest,
                redemption.join().invite_id(),
                redemption.certificate().clone(),
                redemption.join().grant().role,
            )
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        tracing::info!(
            room = %bootstrap.room,
            invite_id = %hex::encode(bootstrap.invite_id),
            request_id = %hex::encode(request_id),
            person = %hex::encode(bootstrap.device_cert.user_public_key),
            device = %hex::encode(bootstrap.device_cert.device_public_key),
            role = ?bootstrap.role,
            outcome = "redeemed",
            "workspace invitation redemption committed"
        );
        Ok(JoinCompletion { bootstrap, access })
    }

    pub fn membership_challenge(
        &self,
        owner: &mut SessionOwner,
        remote_endpoint: [u8; 32],
        request: MembershipChallengeRequest,
        now_ns: u64,
    ) -> Result<MembershipChallengeResponse, CollaborationError> {
        require_active_schema(owner)?;
        if remote_endpoint != request.device_cert.device_public_key
            || request.room != owner.peer().room()
            || request.workspace_id != owner.workspace_id()
        {
            return Err(CollaborationError::new(
                "membership request does not match authenticated workspace endpoint",
            ));
        }
        let request_digest = request_digest(&(remote_endpoint, &request))?;
        if let Some(prior) = owner
            .collaboration_state()
            .membership_challenges
            .iter()
            .find(|prior| {
                prior.challenge.request_id == request.request_id
                    && prior.challenge.expires_at_ns > now_ns
            })
        {
            if prior.request_digest != request_digest {
                return Err(CollaborationError::new(
                    "membership request ID already names a different challenge",
                ));
            }
            return Ok(MembershipChallengeResponse {
                challenge: prior.challenge.clone(),
            });
        }
        let mut nonce = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let challenge = owner
            .peer()
            .membership_challenge(
                owner.workspace_id(),
                &request.device_cert,
                request.request_id,
                nonce,
                now_ns.saturating_add(60_000_000_000),
                system_time(now_ns),
            )
            .map_err(CollaborationError::new)?;
        let mut state = owner.collaboration_state().clone();
        state
            .membership_challenges
            .retain(|record| record.challenge.expires_at_ns > now_ns);
        if state.membership_challenges.len() >= 4096 {
            return Err(CollaborationError::new(
                "pending membership challenge limit reached",
            ));
        }
        state.membership_challenges.push(MembershipChallengeRecord {
            remote_endpoint,
            request_digest,
            device_cert: request.device_cert,
            challenge: challenge.clone(),
        });
        owner
            .commit_collaboration_state(state)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        Ok(MembershipChallengeResponse { challenge })
    }

    pub fn membership_complete(
        &self,
        owner: &SessionOwner,
        remote_endpoint: [u8; 32],
        request: MembershipCompleteRequest,
        now_ns: u64,
    ) -> Result<JoinCompletion, CollaborationError> {
        require_active_schema(owner)?;
        let issued = owner
            .collaboration_state()
            .membership_challenges
            .iter()
            .find(|prior| prior.challenge == request.proof.challenge)
            .ok_or_else(|| {
                CollaborationError::new("membership challenge was not issued by this host")
            })?;
        if issued.remote_endpoint != remote_endpoint
            || issued.device_cert.device_public_key != remote_endpoint
        {
            return Err(CollaborationError::new(
                "membership proof does not match authenticated transport endpoint",
            ));
        }
        let membership = verify_membership_proof(
            &request.proof,
            &issued.challenge,
            &issued.device_cert,
            &revocations(owner)?,
            now_ns,
        )
        .map_err(CollaborationError::new)?;
        let role = owner
            .peer()
            .verify_member_refresh(&membership, system_time(now_ns))
            .map_err(CollaborationError::new)?;
        let invite_id = owner
            .collaboration_state()
            .join_completions
            .iter()
            .find(|completion| {
                completion.bootstrap.device_cert.device_public_key == remote_endpoint
            })
            .map_or([0; 32], |completion| completion.bootstrap.invite_id);
        Ok(JoinCompletion {
            bootstrap: owner.current_join_bootstrap(invite_id, issued.device_cert.clone(), role),
            access: owner
                .peer()
                .governed_access(system_time(now_ns))
                .map_err(CollaborationError::new)?,
        })
    }

    pub fn set_member_role(
        &self,
        owner: &mut SessionOwner,
        administrator: [u8; 32],
        request: SetMemberRoleRequest,
        now_ns: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        authorize_administrator(
            owner,
            administrator,
            now_ns,
            request.request_id,
            "member.role.set",
        )?;
        self.commit_administration(
            owner,
            request.request_id,
            request_digest(&request)?,
            owner
                .prepare_member_role(request.user_public_key, request.role, system_time(now_ns))
                .map_err(|error| CollaborationError::new(error.to_string()))?,
            now_ns,
        )
    }

    pub fn remove_member(
        &self,
        owner: &mut SessionOwner,
        administrator: [u8; 32],
        request: RemoveMemberRequest,
        now_ns: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        authorize_administrator(
            owner,
            administrator,
            now_ns,
            request.request_id,
            "member.remove",
        )?;
        let completion = self.commit_administration(
            owner,
            request.request_id,
            request_digest(&request)?,
            owner
                .prepare_member_removal(request.user_public_key, system_time(now_ns))
                .map_err(|error| CollaborationError::new(error.to_string()))?,
            now_ns,
        )?;
        tracing::info!(
            room = %owner.peer().room(),
            request_id = %hex::encode(request.request_id),
            person = %hex::encode(request.user_public_key),
            outcome = "removed",
            "workspace member removal committed"
        );
        Ok(completion)
    }

    fn commit_administration(
        &self,
        owner: &mut SessionOwner,
        request_id: [u8; 32],
        digest: [u8; 32],
        candidate: qos_peer_state::StatePeer,
        now_ns: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        if let Some(prior) = owner
            .collaboration_state()
            .administration
            .iter()
            .find(|prior| prior.request_id == request_id)
        {
            if prior.request_digest != digest {
                return Err(CollaborationError::new(
                    "administration request ID already names a different operation",
                ));
            }
            return Ok(GovernanceCompletion {
                generation: prior.generation,
                access: owner
                    .peer()
                    .governed_access(system_time(now_ns))
                    .map_err(CollaborationError::new)?,
            });
        }
        let generation = owner
            .committed_generation()
            .checked_add(1)
            .ok_or_else(|| CollaborationError::new("session generation exhausted"))?;
        let mut state = owner.collaboration_state().clone();
        state.administration.push(AdministrationRecord {
            media_result: None,
            request_id,
            request_digest: digest,
            generation,
        });
        owner
            .commit_collaboration_candidate(candidate, state)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        Ok(GovernanceCompletion {
            generation,
            access: owner
                .peer()
                .governed_access(system_time(now_ns))
                .map_err(CollaborationError::new)?,
        })
    }
}

fn validate_issue_request(
    request: &IssueInvitationRequest,
    now_ns: u64,
) -> Result<(), CollaborationError> {
    if request.request_id == [0; 32] {
        return Err(CollaborationError::new(
            "invitation request ID cannot be zero",
        ));
    }
    if request.expires_at_ns <= now_ns {
        return Err(CollaborationError::new(
            "invitation expiry must be in the future",
        ));
    }
    if let Some(name) = request.invitee_name.as_deref()
        && (name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control))
    {
        return Err(CollaborationError::new("invitee name is invalid"));
    }
    Ok(())
}

fn role_name(role: MemberRole) -> &'static str {
    match role {
        MemberRole::Viewer => "viewer",
        MemberRole::Commenter => "commenter",
        MemberRole::Editor => "editor",
    }
}

// The V5 reader checkpoint is not an operational authority, including old retry receipts.
// Do not use permits_publication here: V4 prerequisite recovery must retain its existing behavior.
fn require_active_schema(owner: &SessionOwner) -> Result<(), CollaborationError> {
    match owner.schema_read_only_reason() {
        Some(reason) => Err(CollaborationError::new(reason)),
        None => Ok(()),
    }
}

fn authorize_administrator(
    owner: &SessionOwner,
    device_public_key: [u8; 32],
    now_ns: u64,
    request_id: [u8; 32],
    operation: &'static str,
) -> Result<(), CollaborationError> {
    let result = (|| {
        require_active_schema(owner)?;
        let certificate = owner
            .peer()
            .certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == device_public_key)
            .ok_or_else(|| {
                CollaborationError::new("collaboration administrator is not a member")
            })?;
        if certificate.is_expired_at(now_ns)
            || owner
                .peer()
                .revocations()
                .iter()
                .any(|entry| entry.revoked_device_public_key == device_public_key)
            || !(certificate == *owner.peer().authority()
                || (invitation_operation(operation)
                    && owner_enrolled_device(owner, &certificate, now_ns)))
        {
            return Err(CollaborationError::new(
                "collaboration administration requires live Owner authority",
            ));
        }
        Ok(())
    })();
    if let Err(reason) = &result {
        tracing::warn!(
            room = %owner.peer().room(),
            request_id = %hex::encode(request_id),
            device = %hex::encode(device_public_key),
            operation,
            outcome = "denied",
            %reason,
            "workspace collaboration administration denied"
        );
    }
    result
}

/// Invitation administration only. Member role changes and removal remain
/// with the room authority itself.
fn invitation_operation(operation: &str) -> bool {
    matches!(
        operation,
        "invitation.issue" | "invitation.cancel" | "invitation.pending" | "invitation.list"
    )
}

/// A device the local owner enrolled on this host: its exact certificate is a
/// committed local-owner enrollment issuance, it is certified under the room
/// authority's own user root, and current governance resolves it to the Owner
/// role. Invited members (other roots) and devices merely sharing the root
/// without this host's enrollment grant are refused.
fn owner_enrolled_device(
    owner: &SessionOwner,
    certificate: &tilo_user_identity::DeviceCert,
    now_ns: u64,
) -> bool {
    let authority = owner.peer().authority();
    certificate.user_public_key == authority.user_public_key
        && certificate.device_public_key != authority.device_public_key
        && owner.issuances().iter().any(|issuance| {
            issuance.device_public_key == certificate.device_public_key
                && issuance.certificate == *certificate
        })
        && tilo_user_identity::verify_device_cert_at(certificate, now_ns).is_ok()
        && owner
            .peer()
            .permissions(certificate.device_public_key, system_time(now_ns))
            .is_ok_and(|(role, _, _)| role == collab_types::Role::Owner)
}

fn current_time_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        })
}

fn request_digest(value: &impl Serialize) -> Result<[u8; 32], CollaborationError> {
    postcard::to_stdvec(value)
        .map(|bytes| *blake3::hash(&bytes).as_bytes())
        .map_err(|error| CollaborationError::new(error.to_string()))
}

fn system_time(now_ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_nanos(now_ns)
}

fn revocations(owner: &SessionOwner) -> Result<RevocationList, CollaborationError> {
    let mut revocations = RevocationList::new();
    for entry in owner.peer().revocations() {
        revocations
            .insert(entry)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
    }
    Ok(revocations)
}

pub(crate) fn sign(grant: InviteGrant, key: &ed25519_dalek::SigningKey) -> Result<Vec<u8>, String> {
    issue_invitation(grant, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    fn guest(now_ns: u64) -> (SigningKey, DeviceCert) {
        let root = SigningKey::from_bytes(&[91; 32]);
        let device = SigningKey::from_bytes(&[92; 32]);
        let mut certificate = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Challenge pressure guest".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now_ns.saturating_sub(1),
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut certificate, &root).unwrap();
        (device, certificate)
    }

    fn enroll_owner_device(owner: &mut SessionOwner, seed: u8, now_ns: u64) -> DeviceCert {
        use qos_peer_state::enrollment;
        let key = SigningKey::from_bytes(&[seed; 32]);
        let mut authority = crate::enrollment::EnrollmentAuthority::default();
        let challenge = authority
            .challenge(
                owner,
                enrollment::ChallengeRequest {
                    device_public_key: hex::encode(key.verifying_key().to_bytes()),
                    request_id: hex::encode([seed; 32]),
                },
                now_ns,
            )
            .unwrap();
        let proof =
            enrollment::sign(&enrollment::decode(&challenge.challenge).unwrap(), &key).unwrap();
        authority.complete(owner, proof, now_ns).unwrap()
    }

    fn invitation(request: u8, now_ns: u64) -> IssueInvitationRequest {
        IssueInvitationRequest {
            request_id: [request; 32],
            role: MemberRole::Editor,
            recipient_user_public_key: None,
            invitee_name: Some("Second person".into()),
            expires_at_ns: now_ns + 180_000_000_000,
        }
    }

    #[test]
    fn owner_enrolled_browser_device_administers_invitations_only() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let now_ns = current_time_ns();
        let browser = enroll_owner_device(&mut owner, 71, now_ns);
        assert_eq!(
            browser.user_public_key,
            owner.peer().authority().user_public_key
        );
        assert!(!browser.capabilities.can_admit_devices);
        let service = CollaborationService::new(vec![]);
        let issued = service
            .issue(
                &mut owner,
                browser.device_public_key,
                invitation(72, now_ns),
                now_ns,
            )
            .unwrap();
        // The issued grant is still the host's own delegated authority.
        let pending = service
            .pending(&owner, browser.device_public_key, now_ns)
            .unwrap();
        assert!(!pending.is_empty());
        service
            .cancel(
                &mut owner,
                browser.device_public_key,
                CancelInvitationRequest {
                    request_id: [73; 32],
                    invite_id: issued.invite_id,
                },
            )
            .unwrap();
        // Governance administration beyond invitations stays with the authority.
        let error = service
            .set_member_role(
                &mut owner,
                browser.device_public_key,
                SetMemberRoleRequest {
                    request_id: [74; 32],
                    user_public_key: [9; 32],
                    role: MemberRole::Viewer,
                },
                now_ns,
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("live Owner authority"),
            "{error}"
        );
    }

    #[test]
    fn invited_member_and_unenrolled_owner_root_device_cannot_issue_invitations() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let administrator = owner.peer().authority().device_public_key;
        let now_ns = current_time_ns();
        let service = CollaborationService::new(vec![]);
        // An invited person (distinct root) holding the Editor role.
        let (guest_key, guest) = guest(now_ns);
        let issued = service
            .issue(&mut owner, administrator, invitation(75, now_ns), now_ns)
            .unwrap();
        let challenge = service
            .challenge(
                &mut owner,
                guest.device_public_key,
                JoinChallengeRequest {
                    signed_invitation: issued.signed_invitation,
                    device_cert: guest.clone(),
                    request_id: [76; 32],
                },
                now_ns,
            )
            .unwrap()
            .challenge;
        service
            .complete(
                &mut owner,
                guest.device_public_key,
                JoinCompleteRequest {
                    proof: qos_peer_state::join::sign_join_proof(challenge, &guest_key).unwrap(),
                },
                now_ns + 1,
            )
            .unwrap();
        assert!(owner.peer().certificates().contains(&guest));
        let error = service
            .issue(
                &mut owner,
                guest.device_public_key,
                invitation(77, now_ns),
                now_ns,
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("live Owner authority"),
            "{error}"
        );
        assert!(
            service
                .pending(&owner, guest.device_public_key, now_ns)
                .is_err()
        );
        // A same-root device certificate without this host's enrollment grant.
        let root = owner.identity.root.clone();
        let stray = SigningKey::from_bytes(&[78; 32]);
        let mut unenrolled = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: stray.verifying_key().to_bytes(),
            device_label: "Unenrolled owner-root device".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now_ns.saturating_sub(1),
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut unenrolled, &root).unwrap();
        assert!(!owner_enrolled_device(&owner, &unenrolled, now_ns));
        assert!(
            service
                .issue(
                    &mut owner,
                    unenrolled.device_public_key,
                    invitation(79, now_ns),
                    now_ns
                )
                .is_err()
        );
        // An expired owner-enrolled device is refused as well.
        let browser = enroll_owner_device(&mut owner, 80, now_ns);
        assert!(
            service
                .issue(
                    &mut owner,
                    browser.device_public_key,
                    invitation(81, now_ns),
                    now_ns
                )
                .is_ok()
        );
        let mut expired = browser.clone();
        expired.expires_at_ns = Some(now_ns.saturating_sub(1));
        assert!(!owner_enrolled_device(&owner, &expired, now_ns));
    }

    #[test]
    fn relative_issue_retry_reuses_absolute_expiry_and_bearer() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let administrator = owner.peer().authority().device_public_key;
        let service = CollaborationService::new(vec![]);
        let now_ns = current_time_ns();
        let first = service
            .issue_from_action(
                &mut owner,
                administrator,
                [81; 32],
                MemberRole::Editor,
                Some("Relative retry".into()),
                300,
                now_ns,
            )
            .unwrap();
        let retry = service
            .issue_from_action(
                &mut owner,
                administrator,
                [81; 32],
                MemberRole::Editor,
                Some("Relative retry".into()),
                300,
                now_ns + 120_000_000_000,
            )
            .unwrap();
        assert_eq!(retry, first);
        assert_eq!(first.expires_at_ns, now_ns + 300_000_000_000);
        assert!(
            service
                .issue_from_action(
                    &mut owner,
                    administrator,
                    [81; 32],
                    MemberRole::Viewer,
                    Some("Relative retry".into()),
                    300,
                    now_ns + 120_000_000_000,
                )
                .unwrap_err()
                .to_string()
                .contains("different request")
        );
    }

    #[test]
    fn expired_challenge_pressure_is_pruned_without_receipt_changes() {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
        let administrator = owner.peer().authority().device_public_key;
        let authority = owner.peer().authority().clone();
        let now_ns = current_time_ns();
        let (guest_key, guest) = guest(now_ns);
        let service = CollaborationService::new(vec![]);
        let issued = service
            .issue(
                &mut owner,
                administrator,
                IssueInvitationRequest {
                    request_id: [82; 32],
                    role: MemberRole::Viewer,
                    recipient_user_public_key: Some(guest.user_public_key),
                    invitee_name: None,
                    expires_at_ns: now_ns + 180_000_000_000,
                },
                now_ns,
            )
            .unwrap();
        service
            .challenge(
                &mut owner,
                guest.device_public_key,
                JoinChallengeRequest {
                    signed_invitation: issued.signed_invitation.clone(),
                    device_cert: guest.clone(),
                    request_id: [83; 32],
                },
                now_ns,
            )
            .unwrap();
        let room = owner.peer().room().to_owned();
        let workspace_id = owner.workspace_id();
        service
            .membership_challenge(
                &mut owner,
                authority.device_public_key,
                MembershipChallengeRequest {
                    room: room.clone(),
                    workspace_id,
                    device_cert: authority.clone(),
                    request_id: [84; 32],
                },
                now_ns,
            )
            .unwrap();
        let mut state = owner.collaboration_state().clone();
        state.join_challenges = vec![state.join_challenges[0].clone(); 4096];
        state.membership_challenges = vec![state.membership_challenges[0].clone(); 4096];
        owner.commit_collaboration_state(state).unwrap();
        drop(owner);

        let mut owner = SessionOwner::open(&temp.path().join("owner")).unwrap();
        let later = now_ns + 61_000_000_000;
        service
            .challenge(
                &mut owner,
                guest.device_public_key,
                JoinChallengeRequest {
                    signed_invitation: issued.signed_invitation,
                    device_cert: guest.clone(),
                    request_id: [85; 32],
                },
                later,
            )
            .unwrap();
        service
            .membership_challenge(
                &mut owner,
                authority.device_public_key,
                MembershipChallengeRequest {
                    room,
                    workspace_id,
                    device_cert: authority,
                    request_id: [86; 32],
                },
                later,
            )
            .unwrap();
        assert_eq!(owner.collaboration_state().join_challenges.len(), 1);
        assert_eq!(owner.collaboration_state().membership_challenges.len(), 1);
        assert!(owner.collaboration_state().join_completions.is_empty());
        drop(guest_key);
    }
}

impl From<CollaborationStateV4> for CollaborationState {
    fn from(old: CollaborationStateV4) -> Self {
        Self {
            invitations: old.invitations,
            join_challenges: old.join_challenges,
            join_completions: old.join_completions,
            membership_challenges: old.membership_challenges,
            administration: old
                .administration
                .into_iter()
                .map(|old| AdministrationRecord {
                    request_id: old.request_id,
                    request_digest: old.request_digest,
                    generation: old.generation,
                    media_result: None,
                })
                .collect(),
        }
    }
}
/// Borrowed legacy serialization preserves literal V4 without cloning retained reply bytes.
pub(crate) struct CollaborationV4View<'a>(pub(crate) &'a CollaborationState);
impl Serialize for CollaborationV4View<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeSeq, SerializeStruct};
        struct Administration<'a>(&'a [AdministrationRecord]);
        impl Serialize for Administration<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
                for r in self.0 {
                    if r.media_result.is_some() {
                        return Err(serde::ser::Error::custom(
                            "V5 media result cannot be downgraded",
                        ));
                    }
                    seq.serialize_element(&AdministrationRecordV4 {
                        request_id: r.request_id,
                        request_digest: r.request_digest,
                        generation: r.generation,
                    })?;
                }
                seq.end()
            }
        }
        let mut value = serializer.serialize_struct("CollaborationStateV4", 5)?;
        value.serialize_field("invitations", &self.0.invitations)?;
        value.serialize_field("join_challenges", &self.0.join_challenges)?;
        value.serialize_field("join_completions", &self.0.join_completions)?;
        value.serialize_field("membership_challenges", &self.0.membership_challenges)?;
        value.serialize_field("administration", &Administration(&self.0.administration))?;
        value.end()
    }
}
