//! Certified agents: a member the workspace's record of its agents names,
//! joining under an agent certificate their person's device signed. The broker
//! admits the agent's join by the certificate's chain and the record, and asks
//! again on every revalidation.

use super::*;
use tilo_peer_state::join::agent_cert::AgentCert;

const HOUR_NS: u64 = 3600 * 1_000_000_000;
/// The session the agent works in.
const TASK_ISSUE: &str = "tasks.tracker/issues/whats-left";
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
        (AGENT, TASK_ISSUE),
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
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &workspace,
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .unwrap()
    }
}

impl Fixture {
    /// The workspace lists the agent, kept by the person, and its session's
    /// run goes, carrying its certificate.
    fn ask(&self, minted: &Minted) {
        self.list_agent(minted);
        self.native.runs.lock().unwrap().push(TrackerRun {
            issue: TASK_ISSUE.into(),
            title: "What's left for the release?".into(),
            instruction: "What's left for the release?".into(),
            agent: minted.delegation.agent_id.clone(),
            epoch: 1,
            starter: hex::encode(minted.device.user_public_key),
            active: true,
            finished_by_agent: false,
            paused_through: None,
            budget: None,
            subject: None,
            directions: Vec::new(),
            turn: None,
            question: None,
            delegation: Some(Ok(minted.delegation.encode())),
            origin: None,
        });
        self.state.refresh_native_task_delivery();
    }

    fn end_session(&self) {
        for run in self.native.runs.lock().unwrap().iter_mut() {
            if run.issue == TASK_ISSUE {
                run.active = false;
            }
        }
        self.state.refresh_native_task_delivery();
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
        self.state.agent_listed(&route.cluster_id, cert)?;
        self.state.session_still_goes(&route.cluster_id, cert)?;
        Ok(admitted)
    }

    fn list_agent(&self, minted: &Minted) {
        let mut listed = self.native.listed.lock().unwrap();
        if listed.is_empty() {
            listed.push(crate::services::ListedAgent {
                id: AGENT.into(),
                name: "Claude Code".into(),
                keeper: hex::encode(minted.device.user_public_key),
            });
        }
    }

    fn agent_channel(&self, peer: u64, minted: &Minted) -> runtime_collab::ChannelHandle {
        let admitted = self.agent_join(minted).unwrap();
        let route = CrdtRoute::record("so.tilo.agent", workspace_id().to_string(), "main");
        wire_channel(super::JoinedChannel {
            as_app: None,
            committed_read: Rc::new(std::cell::RefCell::new(committed_document::Slot::default())),
            state: self.state.clone(),
            directories: self.directories.clone(),
            channel_id: route.channel_key(),
            peer_id: peer,
            connection_id: peer,
            verified_identity: admitted,
            record_kind: None,
            route_access: RouteAccess::Own,
            lease: None,
            _hold: None,
        })
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
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(
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

async fn report_progress(
    channel: &runtime_collab::ChannelHandle,
    run: &runtime_collab::TaskRunEnvelope,
) -> Result<(), runtime_portal::NotAvailable> {
    report(
        channel,
        run,
        runtime_collab::TaskRunUpdate::Progress {
            message: "Reading the release tasks".into(),
            presentation: None,
        },
    )
    .await
}

/// A certified agent the workspace lists is a member: its session's run
/// reaches it and it reports on it, as itself, for its person.
#[tokio::test(flavor = "current_thread")]
async fn a_certified_agent_is_a_member_and_works_in_its_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let now = now_ns();
            let minted = mint(&f.native.key, &f.native.certificate, now - 1, now + HOUR_NS);
            f.ask(&minted);
            let agent = f.agent_channel(300, &minted);
            let run = delivered(&agent).await;
            assert_eq!(run.task_id, TASK_ISSUE);
            report_progress(&agent, &run).await.unwrap();
            let admitted = f.agent_join(&minted).unwrap();
            let current = f
                .state
                .revalidate_admission(
                    &CrdtRoute::record("so.tilo.agent", workspace_id().to_string(), "main"),
                    &admitted,
                )
                .unwrap();
            assert_eq!(current.agent_id.as_deref(), Some(AGENT));
            assert!(current.role <= CapRole::Editor);
            assert_eq!(current.broker_actor().issue(), Some(TASK_ISSUE));
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
            // The run's check refuses it too, whoever reads it.
            let mut run = f.native.runs.lock().unwrap()[0].clone();
            run.delegation = Some(Ok(forged.delegation.encode()));
            run.agent.clone_from(&forged.delegation.agent_id);
            assert!(
                f.native
                    .check_agent(&workspace_id().to_string(), &run)
                    .is_err()
            );
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_finished_or_cancelled_session_ends_its_agent() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let now = now_ns();
            let minted = mint(&f.native.key, &f.native.certificate, now - 1, now + HOUR_NS);
            f.ask(&minted);
            let agent = f.agent_channel(300, &minted);
            let run = delivered(&agent).await;
            report_progress(&agent, &run).await.unwrap();
            f.end_session();
            let refused = report_progress(&agent, &run).await.unwrap_err().to_string();
            assert!(refused.contains("has ended"), "{refused}");
            let rejoined = f.agent_join(&minted).unwrap_err();
            assert!(rejoined.contains("has ended"), "{rejoined}");
            drop(f);
        })
        .await;
}

/// An agent's own finish ends its session, and the agent still hears how its
/// last steps ended and tells its host; it never joins again, and a run its
/// person stopped cuts it off as before.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_hears_its_own_finish() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let now = now_ns();
            let minted = mint(&f.native.key, &f.native.certificate, now - 1, now + HOUR_NS);
            f.ask(&minted);
            let agent = f.agent_channel(300, &minted);
            let run = delivered(&agent).await;
            for run in f.native.runs.lock().unwrap().iter_mut() {
                run.active = false;
                run.finished_by_agent = true;
            }
            f.state.refresh_native_task_delivery();
            report_progress(&agent, &run)
                .await
                .expect("its own finish is still heard");
            let rejoined = f.agent_join(&minted).unwrap_err();
            assert!(rejoined.contains("has ended"), "{rejoined}");
            for run in f.native.runs.lock().unwrap().iter_mut() {
                run.finished_by_agent = false;
            }
            f.state.refresh_native_task_delivery();
            let refused = report_progress(&agent, &run).await.unwrap_err().to_string();
            assert!(refused.contains("has ended"), "{refused}");
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
            let agent = f.agent_channel(300, &minted);
            let run = delivered(&agent).await;
            report_progress(&agent, &run).await.unwrap();
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
                report_progress(&agent, &run).await.is_err(),
                "a revoked device's agent reports nothing more"
            );
            assert!(f.agent_join(&minted).is_err());
            let current = f.native.runs.lock().unwrap()[0].clone();
            let refused = f
                .native
                .check_agent(&workspace_id().to_string(), &current)
                .unwrap_err();
            assert!(refused.contains("revoked"), "{refused}");
            drop(f);
        })
        .await;
}

/// A member agent's run reaches it under its membership, with no
/// certificate on the run.
#[tokio::test(flavor = "current_thread")]
async fn a_collaborator_still_works() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            f.approve();
            let agent = f.channel(100, true);
            let run = delivered(&agent).await;
            report_progress(&agent, &run).await.unwrap();
            drop(f);
        })
        .await;
}
