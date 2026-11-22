//! Certified agents: a member the workspace's record of its agents names,
//! joining under an agent certificate their person's device signed. The broker
//! admits the agent's join by the certificate's chain and the record, and asks
//! again on every revalidation.

use super::*;
use tilo_peer_state::join::agent_cert::AgentCert;

const HOUR_NS: u64 = 3600 * 1_000_000_000;
/// The session the agent works in.
const AGENT_SESSION: &str = "so.tilo.session/agent-session/whats-left";
const AGENT: &str = "claude-code";

fn now_ns() -> u64 {
    tilo_clock::try_unix_now_ns().unwrap()
}

/// What a person's device makes for an agent session: a key and a
/// certificate of it, signed by `device`.
struct Minted {
    agent_key: SigningKey,
    delegation: AgentCert,
    device: tilo_user_identity::DeviceCert,
}

fn mint(
    device_key: &SigningKey,
    device: &tilo_user_identity::DeviceCert,
    issued: u64,
    expires: u64,
) -> Minted {
    let mut seed = [7_u8; 32];
    seed[0] = u8::try_from(issued % 251).unwrap();
    let agent_key = SigningKey::from_bytes(&seed);
    let delegation = AgentCert::sign(
        device_key,
        device.user_public_key,
        agent_key.verifying_key().to_bytes(),
        (AGENT, AGENT_SESSION),
        workspace_id(),
        issued,
        expires,
    )
    .unwrap();
    Minted {
        agent_key,
        delegation,
        device: device.clone(),
    }
}

impl Minted {
    fn presentation(&self) -> Vec<u8> {
        let workspace = workspace_id().to_string();
        runtime_collab::AgentJoinIdentity {
            workspace_id: workspace.clone(),
            device_certificate: serde_json::to_vec(&self.device).unwrap(),
            certificate: self.delegation.encode(),
            proof: self
                .agent_key
                .sign(&tilo_identity_client::collaboration_join_digest(&workspace).unwrap())
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap()
    }
}

impl Fixture {
    /// The workspace lists the agent, and its session goes.
    fn ask(&self, _: &Minted) {
        self.list_agent();
        self.native.sessions.lock().unwrap().push((
            AGENT_SESSION.into(),
            crate::apps::services::GoingSession::default(),
        ));
    }

    fn end_session(&self) {
        self.native.sessions.lock().unwrap().clear();
    }

    /// The agent joins as the launcher started does: its presentation
    /// verified, its session asked about, as `join_channel` does.
    fn agent_join(&self, minted: &Minted) -> Result<AdmittedJoinIdentity, String> {
        let route = CrdtRoute::record("so.tilo.agent", workspace_id().to_string(), "main");
        let admitted = verify_join_identity(
            &route,
            &minted.presentation(),
            self.state.membership.as_ref(),
            self.state.trusted_devices.as_deref().map(Vec::as_slice),
            self.state.certificate_authority.get().map(Arc::as_ref),
        )?;
        let cert = admitted
            .agent_cert
            .as_deref()
            .ok_or("no agent certificate")?;
        self.state.agent_session_goes(&route.cluster_id, cert)?;
        Ok(admitted)
    }

    fn list_agent(&self) {
        let mut listed = self.native.listed.lock().unwrap();
        if listed.is_empty() {
            listed.push(crate::apps::services::ListedAgent {
                id: AGENT.into(),
                name: "Claude Code".into(),
                acts_for: String::new(),
            });
        }
    }

    /// The agent's admission `admitted`, checked again as every later call of its connection is.
    fn revalidated(&self, admitted: &AdmittedJoinIdentity) -> Result<AdmittedJoinIdentity, String> {
        self.state
            .revalidate_admission(
                &CrdtRoute::record("so.tilo.agent", workspace_id().to_string(), "main"),
                admitted,
            )
            .map_err(|error| error.to_string())
    }

    /// Apply a change to the workspace's governance as its governor writes it.
    fn govern(
        &self,
        change: impl FnOnce(
            &mut tilo_peer_state::StatePeer,
            &SigningKey,
            std::time::SystemTime,
        ) -> Result<bool, String>,
    ) {
        let now = std::time::SystemTime::now();
        let messages = self
            .native
            .owner
            .with_current(workspace_id(), |view| {
                let before = view.peer().clone();
                let mut next = before.clone();
                change(&mut next, &self.native.key, now)?;
                next.publication_mut().durable_mutation_messages(
                    &before,
                    &self.native.key,
                    now,
                    view.generation().checked_add(1).unwrap(),
                )
            })
            .unwrap();
        for message in messages {
            self.native
                .owner
                .receive(
                    workspace_id(),
                    tilo_peer_state::TransportOrigin::RetainedServerDelta(
                        self.native.key.verifying_key().to_bytes(),
                    ),
                    message,
                    now,
                )
                .unwrap();
        }
        *self.native.cached_projection.lock().unwrap() = None;
    }

    /// The person's second device, certified by their root and admitted to
    /// the workspace's device list.
    pub(super) fn second_device(&self) -> (SigningKey, tilo_user_identity::DeviceCert) {
        let key = SigningKey::from_bytes(&[93; 32]);
        let cert = tilo_peer_state::owner::testing::certificate(
            &SigningKey::from_bytes(&[91; 32]),
            &key,
            "Phone",
        );
        let admitted = cert.clone();
        self.govern(move |peer, governor, now| {
            peer.governance_mut()
                .admit_member_device(&admitted, governor, now)
        });
        (key, cert)
    }
}

/// A certified agent the workspace lists is a member: it works in its session, as itself, for its
/// person.
#[tokio::test(flavor = "current_thread")]
async fn a_certified_agent_is_a_member_and_works_in_its_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let now = now_ns();
            let minted = mint(&f.native.key, &f.native.certificate, now - 1, now + HOUR_NS);
            f.ask(&minted);
            let admitted = f.agent_join(&minted).unwrap();
            let current = f.revalidated(&admitted).unwrap();
            assert_eq!(current.agent_id.as_deref(), Some(AGENT));
            assert!(CapRole::Editor.includes(current.role));
            assert_eq!(current.broker_actor().session(), Some(AGENT_SESSION));
            let route = CrdtRoute::record("so.tilo.agent", workspace_id().to_string(), "main");
            let roster = f.state.native_membership_roster(&route, &admitted).unwrap();
            let me = roster.iter().find(|row| row.is_self).unwrap();
            assert_eq!(me.id, collab_types::MemberId::agent(AGENT));
            assert_eq!(me.name, "Claude Code");
            drop(f);
        })
        .await;
}

/// An agent the workspace's record does not list is no member, whatever its
/// certificate says.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_the_workspace_does_not_list_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let now = now_ns();
            let minted = mint(&f.native.key, &f.native.certificate, now - 1, now + HOUR_NS);
            f.ask(&minted);
            f.native.listed.lock().unwrap().clear();
            let refused = f.agent_join(&minted).unwrap_err();
            assert!(refused.contains("lists no agent"), "{refused}");
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_expired_agent_certificate_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let now = now_ns();
            let minted = mint(
                &f.native.key,
                &f.native.certificate,
                now - 2 * HOUR_NS,
                now - HOUR_NS,
            );
            f.ask(&minted);
            let refused = f.agent_join(&minted).unwrap_err();
            assert!(refused.contains("expired"), "{refused}");
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_certificate_signed_by_an_uncertified_device_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let now = now_ns();
            // Certified by the person's root, but never in the workspace's device
            // list.
            let stranger_key = SigningKey::from_bytes(&[94; 32]);
            let stranger = tilo_peer_state::owner::testing::certificate(
                &SigningKey::from_bytes(&[91; 32]),
                &stranger_key,
                "Stranger",
            );
            let minted = mint(&stranger_key, &stranger, now - 1, now + HOUR_NS);
            f.ask(&minted);
            assert!(
                f.agent_join(&minted).is_err(),
                "an unlisted device certifies nothing"
            );
            // A listed device's certificate, with another key's signature.
            let mut forged = mint(&stranger_key, &f.native.certificate, now - 1, now + HOUR_NS);
            forged.delegation.device_public_key = f.native.certificate.device_public_key;
            let refused = f.agent_join(&forged).unwrap_err();
            assert!(refused.contains("agent certificate refused"), "{refused}");
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_ended_session_ends_its_agent() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let now = now_ns();
            let minted = mint(&f.native.key, &f.native.certificate, now - 1, now + HOUR_NS);
            f.ask(&minted);
            let admitted = f.agent_join(&minted).unwrap();
            f.revalidated(&admitted).unwrap();
            f.end_session();
            let refused = f.revalidated(&admitted).unwrap_err();
            assert!(refused.contains("has ended"), "{refused}");
            let rejoined = f.agent_join(&minted).unwrap_err();
            assert!(rejoined.contains("has ended"), "{rejoined}");
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revoking_the_device_cuts_its_agents() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let now = now_ns();
            let (phone_key, phone) = f.second_device();
            let minted = mint(&phone_key, &phone, now - 1, now + HOUR_NS);
            f.ask(&minted);
            let admitted = f.agent_join(&minted).unwrap();
            f.revalidated(&admitted).unwrap();
            let mut revocation = tilo_user_identity::RevocationEntry {
                user_public_key: phone.user_public_key,
                revoked_device_public_key: phone.device_public_key,
                revoked_at_ns: now,
                reason: tilo_user_identity::RevocationReason::Stolen,
                signature: [0; 64],
            };
            tilo_user_identity::sign_revocation_entry(
                &mut revocation,
                &SigningKey::from_bytes(&[91; 32]),
            )
            .unwrap();
            f.govern(move |peer, governor, now| {
                peer.governance_mut()
                    .publish_governed_revocations(&[revocation], governor, now)
            });
            assert!(
                f.revalidated(&admitted).is_err(),
                "a revoked device's agent does nothing more"
            );
            assert!(f.agent_join(&minted).is_err());
            drop(f);
        })
        .await;
}
