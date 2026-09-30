//! A room's governor and its certified host, as a test's issuing side.
//!
//! It governs one room with a Chat document, lets its host issue invitations
//! under a room delegation, and answers the join exchange the way a certified
//! host does: the challenge binds the authenticated endpoint, completion
//! admits the member and returns the room's full checkpoint. Its outgoing
//! publications are that checkpoint, re-signed on every mutation and refresh.
use ed25519_dalek::SigningKey;
use std::{collections::BTreeMap, time::SystemTime};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_peer_state::{
    GovernedAccess, NeighbourMsg, SeedDocument, SeedState, StatePeer, TransportOrigin,
    join::{
        self, JoinBootstrap, JoinChallenge, JoinChallengeResponse, JoinInspection,
        JoinTransportRequest, JoinTransportResponse, MemberRole, TransportHint,
        WorkspaceDelegation, WorkspaceDelegationPermissions,
    },
    seed_room,
    transport::Retention,
};
use tilo_user_identity::DeviceCert;

use super::{APP, certificate};

/// Longest a join challenge stays open.
const CHALLENGE_NS: u64 = 60_000_000_000;

pub struct TestGovernor {
    peer: StatePeer,
    governor_key: SigningKey,
    host: DeviceCert,
    host_key: SigningKey,
    delegation: WorkspaceDelegation,
    workspace_id: u64,
    generation: u64,
    outgoing: Vec<Vec<u8>>,
    invitations: Vec<Vec<u8>>,
    challenges: Vec<(JoinChallenge, DeviceCert)>,
    completions: BTreeMap<[u8; 32], JoinBootstrap>,
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).unwrap();
    bytes
}

fn ns(now: SystemTime) -> u64 {
    crate::enrollment::ns(now).unwrap()
}

impl Default for TestGovernor {
    fn default() -> Self {
        Self::new()
    }
}

impl TestGovernor {
    /// A fresh room, governor and host, each under new keys.
    #[must_use]
    pub fn new() -> Self {
        let now = SystemTime::now();
        let person = SigningKey::from_bytes(&random());
        let governor_key = SigningKey::from_bytes(&random());
        let governor = certificate(&person, &governor_key, "Governor");
        let host_key = SigningKey::from_bytes(&random());
        let mut host = certificate(&person, &host_key, "Certified host");
        host.capabilities.can_act_as_host = true;
        tilo_user_identity::sign_device_cert(&mut host, &person).unwrap();
        let room = uuid::Builder::from_random_bytes(random())
            .into_uuid()
            .to_string();
        let workspace_id = tilo_relay_protocol::room_workspace_id(&room).unwrap();
        let mut peer = StatePeer::new(
            &room,
            governor.clone(),
            vec![governor.clone()],
            tilo_user_identity::RevocationList::new(),
            now,
        )
        .unwrap();
        let mut chat = automerge::AutoCommit::new();
        automerge::transaction::Transactable::put_object(
            &mut chat,
            automerge::ROOT,
            "channels",
            automerge::ObjType::Map,
        )
        .unwrap();
        peer.install_seed(
            SeedState {
                room_changes: seed_room(&room, &governor, std::slice::from_ref(&governor)).unwrap(),
                documents: vec![SeedDocument {
                    app_id: APP.into(),
                    record_id: String::new(),
                    kind: "chat".into(),
                    bytes: chat.save(),
                }],
            },
            &governor_key,
            now,
        )
        .unwrap();
        peer.select_admission_v5().unwrap();
        let delegation = join::sign_workspace_delegation(
            room,
            workspace_id,
            &governor,
            host.clone(),
            WorkspaceDelegationPermissions {
                issue_invitations: true,
                admit_invited_members: true,
            },
            random(),
            None,
            &governor_key,
        )
        .unwrap();
        peer.admit_certificate_with_workspace_delegation(
            host.clone(),
            delegation.clone(),
            &governor_key,
            now,
        )
        .unwrap();
        let mut governor = Self {
            peer,
            governor_key,
            host,
            host_key,
            delegation,
            workspace_id,
            generation: 0,
            outgoing: Vec::new(),
            invitations: Vec::new(),
            challenges: Vec::new(),
            completions: BTreeMap::new(),
        };
        governor.refresh_checkpoint().unwrap();
        governor
    }

    #[must_use]
    pub fn peer(&self) -> &StatePeer {
        &self.peer
    }
    #[must_use]
    pub fn room(&self) -> &str {
        self.peer.room()
    }
    #[must_use]
    pub fn workspace_id(&self) -> u64 {
        self.workspace_id
    }
    #[must_use]
    pub fn governor(&self) -> &DeviceCert {
        self.peer.authority()
    }
    #[must_use]
    pub fn governor_key(&self) -> &SigningKey {
        &self.governor_key
    }
    #[must_use]
    pub fn host(&self) -> &DeviceCert {
        &self.host
    }
    #[must_use]
    pub fn host_key(&self) -> &SigningKey {
        &self.host_key
    }

    /// The host's signed hint that it serves the room at `endpoint_addr`.
    #[must_use]
    pub fn hint(&self, endpoint_addr: iroh_base::EndpointAddr) -> TransportHint {
        IrohWorkspaceHostHintV1 {
            version: 1,
            mode: WorkspaceHostMode::CertifiedHost,
            room: self.room().into(),
            workspace_id: self.workspace_id,
            governing_authority: self.governor().clone(),
            endpoint_addr,
            host_user_public_key: self.host.user_public_key,
            host_device_certificate: self.host.clone(),
            packet_relay_url: "https://relay.invalid".into(),
        }
        .transport_hint(&self.host_key)
        .unwrap()
    }

    /// The host's signed invitation for `role`, redeemable until
    /// `expires_at_ns` through `transport`.
    pub fn issue(
        &mut self,
        transport: Vec<TransportHint>,
        role: MemberRole,
        recipient_user_public_key: Option<[u8; 32]>,
        invitee_name: Option<String>,
        expires_at_ns: u64,
    ) -> Vec<u8> {
        let signed = join::issue_invitation(
            join::InviteGrant {
                workspace_id: self.workspace_id,
                room: self.room().into(),
                authority: self.governor().clone(),
                issuer: self.host.clone(),
                delegation: Some(self.delegation.clone()),
                role,
                nonce: random(),
                recipient_user_public_key,
                invitee_name,
                expires_at_ns,
                transport,
            },
            &self.host_key,
        )
        .unwrap();
        self.invitations.push(signed.clone());
        signed
    }

    /// The room's current publications, oldest first; every one a checkpoint.
    pub fn outgoing(&self) -> impl Iterator<Item = (&[u8], Retention)> {
        self.outgoing
            .iter()
            .map(|bytes| (bytes.as_slice(), Retention::Checkpoint))
    }

    /// # Errors
    /// The room's governance is missing.
    pub fn governed_access(&self, now: SystemTime) -> Result<GovernedAccess, String> {
        self.peer.governed_access(now)
    }

    /// Re-sign the room's full checkpoint at the next generation.
    ///
    /// # Errors
    /// The checkpoint cannot be signed.
    pub fn refresh_checkpoint(&mut self) -> Result<(), String> {
        let now = SystemTime::now();
        let mut candidate = self.peer.clone();
        candidate.reconcile_media_governance(&self.governor_key, now)?;
        let generation = self.generation.saturating_add(1);
        let messages = candidate.durable_bootstrap_messages(&self.governor_key, now, generation)?;
        self.outgoing = messages
            .iter()
            .map(|message| postcard::to_stdvec(message).map_err(|error| error.to_string()))
            .collect::<Result<_, _>>()?;
        self.peer = candidate;
        self.generation = generation;
        Ok(())
    }

    /// Admit `message` from `origin` into the governor's peer.
    ///
    /// # Errors
    /// The peer rejects it.
    pub fn receive(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
    ) -> Result<(), String> {
        self.peer
            .receive_from(origin, message, SystemTime::now())
            .map(|_| ())
    }

    /// Answer one join request from the authenticated endpoint `remote`.
    pub fn handle(
        &mut self,
        remote: [u8; 32],
        request: JoinTransportRequest,
    ) -> JoinTransportResponse {
        let result = match request {
            JoinTransportRequest::Inspect { signed_invitation } => {
                self.inspect(&signed_invitation).map(|join| {
                    JoinTransportResponse::Inspection(JoinInspection {
                        invite_id: join.invite_id(),
                        grant: join.grant().clone(),
                    })
                })
            }
            JoinTransportRequest::Challenge(request) => self
                .challenge(remote, request)
                .map(JoinTransportResponse::Challenge),
            JoinTransportRequest::Complete(request) => self
                .complete(remote, &request)
                .map(JoinTransportResponse::Complete),
            _ => Err("unexpected membership operation".into()),
        };
        result.unwrap_or_else(|message| JoinTransportResponse::Error { code: 403, message })
    }

    fn revocations(&self) -> tilo_user_identity::RevocationList {
        let mut list = tilo_user_identity::RevocationList::new();
        for entry in self.peer.revocations() {
            list.insert(entry).unwrap();
        }
        list
    }

    fn inspect(&self, signed_invitation: &[u8]) -> Result<join::VerifiedJoin, String> {
        if !self
            .invitations
            .iter()
            .any(|issued| issued == signed_invitation)
        {
            return Err("invitation was not issued by this host".into());
        }
        join::decode_and_verify_invitation(
            signed_invitation,
            None,
            &self.revocations(),
            ns(SystemTime::now()),
        )
    }

    fn challenge(
        &mut self,
        remote: [u8; 32],
        request: join::JoinChallengeRequest,
    ) -> Result<JoinChallengeResponse, String> {
        if remote != request.device_cert.device_public_key {
            return Err("join certificate does not match authenticated transport endpoint".into());
        }
        let now = ns(SystemTime::now());
        if let Some((challenge, _)) = self.challenges.iter().find(|(challenge, cert)| {
            challenge.request_id == request.request_id && *cert == request.device_cert
        }) {
            return Ok(JoinChallengeResponse {
                challenge: challenge.clone(),
            });
        }
        let verified = self.inspect(&request.signed_invitation)?;
        if verified
            .grant()
            .recipient_user_public_key
            .is_some_and(|recipient| recipient != request.device_cert.user_public_key)
        {
            return Err("invitation recipient does not match joining identity".into());
        }
        let expires_at_ns = verified
            .grant()
            .expires_at_ns
            .min(now.saturating_add(CHALLENGE_NS));
        let challenge = JoinChallenge::new(
            &verified,
            &request.device_cert,
            request.request_id,
            random(),
            expires_at_ns,
            now,
        )?;
        self.challenges
            .push((challenge.clone(), request.device_cert));
        Ok(JoinChallengeResponse { challenge })
    }

    fn complete(
        &mut self,
        remote: [u8; 32],
        request: &join::JoinCompleteRequest,
    ) -> Result<JoinBootstrap, String> {
        let request_id = request.proof.challenge.request_id;
        if let Some(bootstrap) = self.completions.get(&request_id) {
            return Ok(bootstrap.clone());
        }
        let (challenge, cert) = self
            .challenges
            .iter()
            .find(|(challenge, _)| *challenge == request.proof.challenge)
            .cloned()
            .ok_or("join challenge was not issued by this host")?;
        if cert.device_public_key != remote {
            return Err("join proof does not match authenticated transport endpoint".into());
        }
        let now = SystemTime::now();
        let redemption = join::verify_join_proof(
            &request.proof,
            &challenge,
            &cert,
            &self.revocations(),
            ns(now),
        )?;
        let mut candidate = self.peer.clone();
        candidate.admit_invited_member(&redemption, &self.host_key, now)?;
        let previous = std::mem::replace(&mut self.peer, candidate);
        if let Err(error) = self.refresh_checkpoint() {
            self.peer = previous;
            return Err(error);
        }
        let bootstrap = JoinBootstrap {
            version: 1,
            invite_id: redemption.join().invite_id(),
            device_cert: cert,
            role: redemption.join().grant().role,
            room: self.room().into(),
            workspace_id: self.workspace_id,
            authority: self.governor().clone(),
            certified_roster: self.peer.certificates(),
            revocations: self.peer.revocations(),
            records: self.peer.rows(),
            governance_messages: self.outgoing.clone(),
            generation: self.generation,
        };
        self.completions.insert(request_id, bootstrap.clone());
        Ok(bootstrap)
    }
}
