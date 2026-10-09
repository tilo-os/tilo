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
    delegation: Option<WorkspaceDelegation>,
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
            delegation: Some(delegation),
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

    /// Empty interactive room governed by the fixture session's actual
    /// root-certified device custody. Unlike `new`, this installs no app
    /// records, messages, agents or separately pinned delivery host.
    ///
    /// # Errors
    /// The certificate does not verify, the key does not match, or the
    /// canonical room seed/checkpoint cannot be admitted.
    pub fn empty_interactive(
        governor: DeviceCert,
        governor_key: SigningKey,
        room: &str,
    ) -> Result<Self, String> {
        tilo_user_identity::verify_device_cert(&governor).map_err(|error| error.to_string())?;
        if governor.device_public_key != governor_key.verifying_key().to_bytes()
            || !governor.capabilities.can_own_workspaces
        {
            return Err("interactive fixture custody cannot govern this room".into());
        }
        let now = SystemTime::now();
        let workspace_id =
            tilo_relay_protocol::room_workspace_id(room).map_err(|error| error.to_string())?;
        let mut peer = StatePeer::new(
            room,
            governor.clone(),
            vec![governor.clone()],
            tilo_user_identity::RevocationList::new(),
            now,
        )?;
        peer.install_seed(
            SeedState {
                room_changes: seed_room(room, &governor, std::slice::from_ref(&governor))?,
                documents: Vec::new(),
            },
            &governor_key,
            now,
        )?;
        peer.select_admission_v5()
            .map_err(|error| error.to_string())?;
        let mut fixture = Self {
            peer,
            host: governor,
            host_key: governor_key.clone(),
            governor_key,
            delegation: None,
            workspace_id,
            generation: 0,
            outgoing: Vec::new(),
            invitations: Vec::new(),
            challenges: Vec::new(),
            completions: BTreeMap::new(),
        };
        fixture.refresh_checkpoint()?;
        Ok(fixture)
    }

    /// Use a separately certified delivery host without making the governor
    /// a sealed-channel steward. Admission uses the ordinary signed delegation.
    ///
    /// # Errors
    /// Rejects a mismatched host key or invalid workspace delegation.
    pub fn with_delivery_host(
        mut self,
        host: DeviceCert,
        host_key: SigningKey,
    ) -> Result<Self, String> {
        if host.device_public_key != host_key.verifying_key().to_bytes() {
            return Err("delivery host key does not match its certificate".into());
        }
        let delegation = join::sign_workspace_delegation(
            self.room(),
            self.workspace_id,
            self.governor(),
            host.clone(),
            WorkspaceDelegationPermissions {
                issue_invitations: true,
                admit_invited_members: true,
            },
            random(),
            None,
            &self.governor_key,
        )?;
        self.peer.admit_certificate_with_workspace_delegation(
            host.clone(),
            delegation.clone(),
            &self.governor_key,
            SystemTime::now(),
        )?;
        self.host = host;
        self.host_key = host_key;
        self.delegation = Some(delegation);
        self.refresh_checkpoint()?;
        Ok(self)
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
            mode: if self.delegation.is_some() {
                WorkspaceHostMode::CertifiedHost
            } else {
                WorkspaceHostMode::NativeInteractiveWorkspaceOwner
            },
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
                delegation: self.delegation.clone(),
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

    /// The ids of the grants naming the agent `agent` in this governor's
    /// admitted room policy.
    ///
    /// # Errors
    /// The governor has no admitted room policy.
    pub fn agent_grants(&self, agent: &str) -> Result<Vec<String>, String> {
        let room = self
            .peer
            .admitted_room()
            .ok_or("governor room policy is absent")?;
        let (_, policy) = tilo_peer_state::room::extract_governed_fields(room);
        let agent = tilo_action_policy::Subject::agent(agent);
        Ok(policy
            .grants
            .into_iter()
            .filter(|grant| grant.subject == agent)
            .map(|grant| grant.id.as_str().to_owned())
            .collect())
    }

    /// Re-sign the room's full checkpoint at the next generation.
    ///
    /// # Errors
    /// The checkpoint cannot be signed.
    pub fn refresh_checkpoint(&mut self) -> Result<(), String> {
        let now = SystemTime::now();
        let mut candidate = self.peer.clone();
        candidate.reconcile_media_governance(&self.governor_key, now)?;
        let admitted_generation = candidate
            .admitted_room()
            .map(tilo_peer_state::durability::entries)
            .transpose()?
            .unwrap_or_default()
            .values()
            .map(|receipt| receipt.generation)
            .max()
            .unwrap_or(0);
        let generation = self
            .generation
            .max(admitted_generation)
            .checked_add(1)
            .ok_or("fixture checkpoint generation exhausted")?;
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
        let id = tracing::enabled!(tracing::Level::DEBUG)
            .then(|| postcard::to_stdvec(&message).ok())
            .flatten()
            .map(|bytes| blake3::hash(&bytes).to_hex().to_string());
        let outcome = self.peer.receive_from(origin, message, SystemTime::now())?;
        tracing::debug!(?id, ?outcome, "native fixture governor admission");
        Ok(())
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

#[cfg(test)]
mod empty_interactive_tests {
    use super::{TestGovernor, certificate};
    use ed25519_dalek::SigningKey;
    use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};

    #[test]
    fn checkpoint_refresh_follows_admitted_native_receipts() {
        let mut governor = TestGovernor::new();
        let key = governor.governor_key().clone();
        let publisher = governor.governor().device_public_key;
        let messages = governor
            .peer()
            .clone()
            .durable_bootstrap_messages(&key, std::time::SystemTime::now(), 20)
            .unwrap();
        for message in messages {
            governor
                .receive(
                    tilo_peer_state::TransportOrigin::RetainedNativePublication { publisher },
                    message,
                )
                .unwrap();
        }
        governor.refresh_checkpoint().unwrap();
        let receipts =
            tilo_peer_state::durability::entries(governor.peer().admitted_room().unwrap()).unwrap();
        assert!(
            !receipts.is_empty(),
            "the signed cut must carry actual document receipts"
        );
        assert!(receipts.values().all(|receipt| receipt.generation > 20));
    }

    #[test]
    fn empty_interactive_preserves_certified_custody_without_seeding_apps_or_host_bits() {
        let signing_identity = SigningKey::from_bytes(&[91; 32]);
        let key = SigningKey::from_bytes(&[92; 32]);
        let cert = certificate(&signing_identity, &key, "Fixture session");
        let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string();
        let governor = TestGovernor::empty_interactive(cert.clone(), key, &room).unwrap();
        assert_eq!(governor.governor(), &cert);
        assert!(!governor.governor().capabilities.can_act_as_host);
        assert!(governor.peer().rows().is_empty());
        let endpoint = iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(
                &SigningKey::from_bytes(&[93; 32]).verifying_key().to_bytes(),
            )
            .unwrap(),
        );
        let hint = governor.hint(endpoint);
        let (payload, _) = IrohWorkspaceHostHintV1::authenticate_transport_hint(&hint).unwrap();
        assert_eq!(
            payload.mode,
            WorkspaceHostMode::NativeInteractiveWorkspaceOwner
        );
        assert_eq!(payload.governing_authority, cert);
        assert!(
            TestGovernor::empty_interactive(cert, SigningKey::from_bytes(&[94; 32]), &room)
                .is_err()
        );
    }

    #[test]
    fn empty_interactive_governor_redeems_its_actual_session_custody_without_app_seed() {
        use tilo_peer_state::join::{
            self, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest,
            JoinTransportResponse, MemberRole,
        };
        let key = SigningKey::from_bytes(&[92; 32]);
        let cert = certificate(&SigningKey::from_bytes(&[91; 32]), &key, "Fixture session");
        let mut governor =
            TestGovernor::empty_interactive(cert.clone(), key.clone(), super::super::ROOM).unwrap();
        let endpoint = iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(
                &SigningKey::from_bytes(&[93; 32]).verifying_key().to_bytes(),
            )
            .unwrap(),
        );
        let expiry = super::ns(std::time::SystemTime::now())
            .checked_add(60_000_000_000)
            .unwrap();
        let invitation = governor.issue(
            vec![governor.hint(endpoint)],
            MemberRole::Editor,
            Some(cert.user_public_key),
            Some("Fixture session".into()),
            expiry,
        );
        let response = governor.handle(
            cert.device_public_key,
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: invitation,
                device_cert: cert.clone(),
                request_id: [95; 32],
            }),
        );
        let JoinTransportResponse::Challenge(response) = response else {
            panic!("actual session challenge refused: {response:?}");
        };
        let proof = join::sign_join_proof(response.challenge, &key).unwrap();
        let response = governor.handle(
            cert.device_public_key,
            JoinTransportRequest::Complete(JoinCompleteRequest { proof }),
        );
        let JoinTransportResponse::Complete(bootstrap) = response else {
            panic!("actual session completion refused: {response:?}");
        };
        assert_eq!(bootstrap.authority, cert);
        assert_eq!(bootstrap.device_cert, cert);
        assert!(bootstrap.records.is_empty());
        assert!(!bootstrap.authority.capabilities.can_act_as_host);
    }
}
