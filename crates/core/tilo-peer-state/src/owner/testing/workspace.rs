//! One device holding a workspace, as a test's issuing side.
//!
//! It holds one workspace (with a Chat document, when it starts one), issues
//! invitations as itself, and answers the join exchange the way any device of
//! the workspace does: the challenge binds the authenticated endpoint,
//! completion admits the member and returns the workspace's state.
use crate::{
    GovernedAccess, SeedDocument, SeedState, StatePeer, TransportOrigin,
    join::{
        self, JoinBootstrap, JoinChallenge, JoinChallengeResponse, JoinInspection,
        JoinTransportRequest, JoinTransportResponse, Role, TransportHint,
    },
    seed_workspace,
};
use ed25519_dalek::SigningKey;
use neighbour_wire::{NeighbourMsg, Retention};
use std::{collections::BTreeMap, time::SystemTime};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_user_identity::DeviceCert;

use super::{APP, certificate};
use collab_types::WorkspaceId;

/// Longest a join challenge stays open.
const CHALLENGE_NS: u64 = 60_000_000_000;

pub struct TestWorkspace {
    peer: StatePeer,
    cert: DeviceCert,
    key: SigningKey,
    workspace_id: WorkspaceId,
    generation: u64,
    invitations: Vec<Vec<u8>>,
    challenges: Vec<(JoinChallenge, DeviceCert)>,
    completions: BTreeMap<[u8; 32], JoinBootstrap>,
    /// The instant the fixture treats as now, once a test pins it; otherwise it
    /// follows the wall clock.
    clock: Option<SystemTime>,
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).unwrap();
    bytes
}

/// `cert`'s signed address for the workspace `workspace_id`, saying it is
/// reached at `endpoint_addr` and keeps the workspace with its keys: the hint
/// a device named among a workspace's holders signs for itself.
#[must_use]
pub fn holding_hint(
    cert: &DeviceCert,
    key: &SigningKey,
    workspace_id: WorkspaceId,
    endpoint_addr: iroh_base::EndpointAddr,
) -> TransportHint {
    WorkspaceDeviceHintV1 {
        version: 1,
        holds_keys: true,
        runs_work: false,
        label: String::new(),
        cloud: false,
        origin: None,
        workspace_id,
        endpoint_addr,
        user_public_key: cert.user_public_key,
        device_certificate: cert.clone(),
        packet_relay_url: "https://relay.invalid".into(),
    }
    .transport_hint(key)
    .unwrap()
}

fn ns(now: SystemTime) -> u64 {
    crate::owner::enrollment::ns(now).unwrap()
}

impl Default for TestWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

impl TestWorkspace {
    /// A fresh workspace under a new device key, with one Chat record.
    #[must_use]
    pub fn new() -> Self {
        let now = tilo_clock::wall();
        let person = SigningKey::from_bytes(&random());
        let key = SigningKey::from_bytes(&random());
        let device = certificate(&person, &key, "Device");
        let workspace = uuid::Builder::from_random_bytes(random()).into_uuid();
        let workspace_id = WorkspaceId::from(workspace);
        let workspace = workspace.to_string();
        let mut peer = StatePeer::new(
            &workspace,
            device.clone(),
            vec![device.clone()],
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
        collab_types::CommitNamed::commit_edit(
            &mut chat,
            "chat",
            "channels",
            &governor.user_public_key,
        );
        peer.install_seed(
            SeedState {
                workspace_changes: seed_workspace(
                    &workspace,
                    &device,
                    std::slice::from_ref(&device),
                )
                .unwrap(),
                documents: vec![SeedDocument {
                    app_id: APP.into(),
                    document_id: String::new(),
                    kind: "chat".into(),
                    bytes: chat.save(),
                }],
            },
            &key,
            now,
        )
        .unwrap();
        peer.select_exact_admissions().unwrap();
        Self {
            peer,
            cert: device,
            key,
            workspace_id,
            generation: 0,
            invitations: Vec::new(),
            challenges: Vec::new(),
            completions: BTreeMap::new(),
            clock: None,
        }
    }

    /// An empty workspace held by the fixture session's actual root-certified
    /// device. Unlike `new`, this installs no app records, messages or agents.
    pub fn empty_interactive(
        device: DeviceCert,
        key: SigningKey,
        workspace: &str,
    ) -> Result<Self, String> {
        tilo_user_identity::verify_device_cert(&device).map_err(|error| error.to_string())?;
        if device.device_public_key != key.verifying_key().to_bytes()
            || !device.capabilities.can_own_workspaces
        {
            return Err("the fixture's device cannot hold this workspace".into());
        }
        let now = tilo_clock::wall();
        let workspace_id = WorkspaceId::parse(workspace)
            .ok_or_else(|| "the fixture's workspace is not a workspace id".to_string())?;
        let mut peer = StatePeer::new(
            workspace,
            device.clone(),
            vec![device.clone()],
            tilo_user_identity::RevocationList::new(),
            now,
        )?;
        peer.install_seed(
            SeedState {
                workspace_changes: seed_workspace(
                    workspace,
                    &device,
                    std::slice::from_ref(&device),
                )?,
                documents: Vec::new(),
            },
            &key,
            now,
        )?;
        peer.select_exact_admissions()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            peer,
            cert: device,
            key,
            workspace_id,
            generation: 0,
            invitations: Vec::new(),
            challenges: Vec::new(),
            completions: BTreeMap::new(),
            clock: None,
        })
    }

    #[must_use]
    pub fn peer(&self) -> &StatePeer {
        &self.peer
    }
    #[must_use]
    pub fn workspace(&self) -> &str {
        self.peer.workspace_id()
    }
    #[must_use]
    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }
    /// The device this workspace is held on.
    #[must_use]
    pub fn device(&self) -> &DeviceCert {
        &self.cert
    }
    /// Its signing key.
    #[must_use]
    pub fn key(&self) -> &SigningKey {
        &self.key
    }

    /// Answer every later exchange as of `now` instead of the wall clock, so
    /// a test whose other side runs on a fixed instant agrees with the
    /// fixture on what has expired however long the test machine takes.
    pub fn pin_clock(&mut self, now: SystemTime) {
        self.clock = Some(now);
    }

    fn now(&self) -> SystemTime {
        self.clock.unwrap_or_else(SystemTime::now)
    }

    /// This device's signed hint that it serves the workspace at `endpoint_addr`.
    #[must_use]
    pub fn hint(&self, endpoint_addr: iroh_base::EndpointAddr) -> TransportHint {
        holding_hint(&self.cert, &self.key, self.workspace_id, endpoint_addr)
    }

    /// This device's signed invitation for `role`, redeemable until
    /// `expires_at_ns` through `transport`.
    pub fn issue(
        &mut self,
        transport: Vec<TransportHint>,
        role: Role,
        recipient_user_public_key: Option<[u8; 32]>,
        invitee_name: Option<String>,
        expires_at_ns: u64,
    ) -> Vec<u8> {
        let signed = join::issue_invitation(
            join::InviteGrant {
                workspace_id: self.workspace_id,
                issuer: self.cert.clone(),
                role,
                nonce: random(),
                recipient_user_public_key,
                invitee_name,
                expires_at_ns,
                transport,
            },
            &self.key,
        )
        .unwrap();
        self.invitations.push(signed.clone());
        signed
    }

    /// Every message a device starting from nothing needs from this one: the
    /// workspace as it stands and a checkpoint of each record, encoded.
    pub fn outgoing(&self) -> std::vec::IntoIter<(Vec<u8>, Retention)> {
        self.changes()
    }

    /// The same messages, as what a device that has some of the workspace lacks.
    pub fn changes(&self) -> std::vec::IntoIter<(Vec<u8>, Retention)> {
        let now = self.now();
        self.peer
            .clone()
            .publication_mut()
            .bootstrap_messages(&self.key, now)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|message| {
                let (_, retention) = neighbour_wire::retention(&message, false)?;
                Some((postcard::to_stdvec(&message).ok()?, retention))
            })
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn governed_access(&self, now: SystemTime) -> Result<GovernedAccess, String> {
        self.peer.governance().governed_access(now)
    }

    /// The ids of the grants naming the agent `agent` in this fixture's
    /// admitted workspace policy.
    pub fn agent_grants(&self, agent: &str) -> Result<Vec<String>, String> {
        let workspace = self
            .peer
            .admitted_workspace()
            .ok_or("fixture workspace policy is absent")?;
        let (_, policy) = crate::workspace::extract_governed_fields(workspace);
        let agent = tilo_action_policy::Grantee::agent(agent);
        Ok(policy
            .grants
            .into_iter()
            .filter(|grant| grant.grantee == agent)
            .map(|grant| grant.id.as_str().to_owned())
            .collect())
    }

    /// Let `person` exercise the governance `action` of this workspace, as an
    /// administrator may: a key holder a test needs to govern something
    /// without giving it the workspace's owner role.
    pub fn allow_governance(&mut self, person: &str, action: &str) -> Result<(), String> {
        let now = self.now();
        let grant = tilo_action_policy::Grant::new(
            format!("fixture-administrator:{action}"),
            tilo_action_policy::Grantee::person(person.to_owned()),
            tilo_action_policy::roles::governance_action(self.workspace(), action),
            tilo_action_policy::KindSet::OUTSIDE,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(hex::encode(self.peer.owner())),
        );
        let mut candidate = self.peer.clone();
        candidate.governance_mut().put_governed_grants(
            std::slice::from_ref(&grant),
            &self.key,
            now,
        )?;
        self.peer = candidate;
        self.advance();
        Ok(())
    }

    /// Move the fixture on one generation, as a change of its workspace does.
    fn advance(&mut self) {
        self.generation = self.generation.saturating_add(1);
    }

    /// Admit `message` from `origin` into the fixture's peer.
    pub fn receive(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
    ) -> Result<(), String> {
        let id = tracing::enabled!(tracing::Level::DEBUG)
            .then(|| postcard::to_stdvec(&message).ok())
            .flatten()
            .map(|bytes| blake3::hash(&bytes).to_hex().to_string());
        let now = self.now();
        let outcome = self
            .peer
            .receive_from(origin, message, now)
            .map_err(|error| error.to_string())?;
        tracing::debug!(?id, ?outcome, "native fixture admission");
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
            ns(self.now()),
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
        let now = ns(self.now());
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
        let now = self.now();
        let redemption = join::verify_join_proof(
            &request.proof,
            &challenge,
            &cert,
            &self.revocations(),
            ns(now),
        )?;
        let mut candidate = self.peer.clone();
        candidate
            .governance_mut()
            .admit_invited_member(&redemption, &self.key, now)?;
        self.peer = candidate;
        self.advance();
        let bootstrap = JoinBootstrap {
            version: 1,
            invite_id: redemption.join().invite_id(),
            device_cert: cert,
            role: redemption.join().grant().role,
            workspace_id: self.workspace_id,
            host: self.cert.clone(),
            certified_roster: self.peer.certificates(),
            revocations: self.peer.revocations(),
            records: self.peer.rows(),
            // A join is answered with the workspace's committed document alone:
            // record publications reach the member through the workspace's sync.
            governance_messages: vec![
                postcard::to_stdvec(&self.peer.workspace_message())
                    .map_err(|error| error.to_string())?,
            ],
            generation: self.generation,
        };
        self.completions.insert(request_id, bootstrap.clone());
        Ok(bootstrap)
    }
}

#[cfg(test)]
mod empty_interactive_tests {
    use super::{TestWorkspace, certificate};
    use ed25519_dalek::SigningKey;
    use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;

    tilo_test_env::scenario! {
        fn the_fixture_holds_its_workspace_as_one_device;

        phase empty_interactive_holds_the_workspace_it_founded_as_its_own_device;
        {
            let signing_identity = SigningKey::from_bytes(&[91; 32]);
            let key = SigningKey::from_bytes(&[92; 32]);
            let cert = certificate(&signing_identity, &key, "Fixture session");
            let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string();
            let workspace_fixture =
                TestWorkspace::empty_interactive(cert.clone(), key, &workspace).unwrap();
            assert_eq!(workspace_fixture.device(), &cert);
            assert!(workspace_fixture.peer().rows().is_empty());
            let endpoint = iroh_base::EndpointAddr::new(
                iroh_base::EndpointId::from_bytes(
                    &SigningKey::from_bytes(&[93; 32]).verifying_key().to_bytes(),
                )
                .unwrap(),
            );
            let hint = workspace_fixture.hint(endpoint);
            let (payload, _) = WorkspaceDeviceHintV1::authenticate_transport_hint(&hint).unwrap();
            assert!(payload.holds_keys);
            assert_eq!(payload.device_certificate, cert);
            assert!(
                TestWorkspace::empty_interactive(cert, SigningKey::from_bytes(&[94; 32]), &workspace)
                    .is_err()
            );
        }

        phase empty_interactive_answers_a_real_join_without_app_seed;
        {
            use crate::join::{
                self, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest,
                JoinTransportResponse, Role,
            };
            let key = SigningKey::from_bytes(&[92; 32]);
            let cert = certificate(&SigningKey::from_bytes(&[91; 32]), &key, "Fixture session");
            let mut workspace_fixture =
                TestWorkspace::empty_interactive(cert.clone(), key.clone(), super::super::WORKSPACE)
                    .unwrap();
            let endpoint = iroh_base::EndpointAddr::new(
                iroh_base::EndpointId::from_bytes(
                    &SigningKey::from_bytes(&[93; 32]).verifying_key().to_bytes(),
                )
                .unwrap(),
            );
            let expiry = super::ns(tilo_clock::wall())
                .checked_add(60_000_000_000)
                .unwrap();
            let invitation = workspace_fixture.issue(
                vec![workspace_fixture.hint(endpoint)],
                Role::Editor,
                Some(cert.user_public_key),
                Some("Fixture session".into()),
                expiry,
            );
            let response = workspace_fixture.handle(
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
            let response = workspace_fixture.handle(
                cert.device_public_key,
                JoinTransportRequest::Complete(JoinCompleteRequest { proof }),
            );
            let JoinTransportResponse::Complete(bootstrap) = response else {
                panic!("actual session completion refused: {response:?}");
            };
            assert_eq!(bootstrap.host, cert);
            assert_eq!(bootstrap.device_cert, cert);
            assert!(bootstrap.records.is_empty());
        }
    }
}
