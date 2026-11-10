//! A change the workspace refuses is never kept: the store holds no trace of
//! it, so no reader meets it and a writer that numbers its next change the
//! refused one's sequence again writes on cleanly.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;
use std::time::SystemTime;

use automerge::{ReadDoc as _, transaction::Transactable as _};
use collab_types::{CommitNamed as _, SignedOp};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use tokio::sync::Mutex as AsyncMutex;

use super::*;
use crate::workspaces::fixtures::{
    admit_view_reader, governor_with, project_standings, test_host_taking_part,
};
use crate::workspaces::sealed_records::fixtures::SealingMember;

const APP: &str = "so.tilo.notes";
const RECORD: &str = "welcome";

/// A workspace whose governor holds a record its members write sealed, on a
/// real store, and a member device that writes it.
struct Lounge {
    _dir: tempfile::TempDir,
    relay: crate::workspaces::fixtures::TestHost,
    workspaces: crate::workspaces::Workspaces,
    workspace: String,
    workspace_id: u64,
    governor_key: SigningKey,
    governor: DeviceCert,
    member_key: SigningKey,
    /// The member's device, certified by the governor's person but not yet
    /// admitted to the workspace's own governance.
    member: DeviceCert,
    writer: SealingMember,
}

async fn lounge() -> Lounge {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(tilo_user_identity::cloud_enrollment::host_capabilities());
    let relay = test_host_taking_part(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    // A device of the governor's own person, as a second device of theirs is:
    // it writes the record, and the workspace has not admitted it yet.
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    workspaces
        .author_room_record(&created.workspace, &cert, (APP, RECORD), |doc| {
            doc.put(automerge::ROOT, "title", "Welcome")
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    let writer = SealingMember::join(
        &workspaces,
        &created.workspace,
        (APP, RECORD),
        SigningKey::from_bytes(&member_key.to_bytes()),
    )
    .await;
    Lounge {
        _dir: dir,
        relay,
        workspaces,
        workspace: created.workspace,
        workspace_id: created.workspace_id,
        governor_key: key,
        governor: cert,
        member_key,
        member,
        writer,
    }
}

impl Lounge {
    /// The host's runtime for the workspace, with the member subscribed to it
    /// as its session would have been: what its change is published through.
    async fn publishing(&self) -> (crate::workspace::SharedWorkspace, Doors) {
        let runtime = self
            .relay
            .host()
            .runtime(self.workspace_id)
            .await
            .expect("the host holds the workspace");
        let (outbound, delivered) = mpsc::channel(32);
        let (disconnect, disconnected) = mpsc::channel(1);
        {
            let mut held = runtime.lock().await;
            held.admit_device(self.member.device_public_key)
                .expect("the workspace admits the member's device");
            held.add_subscriber(Subscriber {
                device_public_key: self.member.device_public_key,
                tx: outbound.clone(),
                disconnect: disconnect.clone(),
            });
        }
        let mut doors = Doors::over(&runtime, self.workspace_id, outbound, disconnect);
        doors.delivered = delivered;
        doors.disconnected = disconnected;
        (runtime, doors)
    }

    /// The runtime admits the member's device and holds its session, as it
    /// does when the workspace's governance reaches the relay.
    async fn publishing_again(&self, runtime: &crate::workspace::SharedWorkspace, doors: &Doors) {
        let mut held = runtime.lock().await;
        held.admit_device(self.member.device_public_key)
            .expect("the workspace admits the member's device");
        held.add_subscriber(Subscriber {
            device_public_key: self.member.device_public_key,
            tx: doors.outbound.clone(),
            disconnect: doors.disconnect.clone(),
        });
    }

    /// `message`, the member's write of the record, as its session commits it.
    fn commit(&self, message: &NeighbourMsg, op: [u8; 16]) -> Envelope {
        let (channel, _) = neighbour_wire::retention(message, false).expect("a retained write");
        Envelope::Commit {
            workspace_id: self.workspace_id,
            op,
            author_device_public_key: self.member.device_public_key,
            authored_at_ns: unix_now_ns(),
            delta: crate::coedit::CoeditPayload::RetainedDelta {
                channel,
                body: postcard::to_stdvec(message).unwrap(),
            }
            .encode()
            .unwrap(),
        }
    }

    /// The member's `seq`-numbered change setting the record's title to
    /// `title` written in the clear, which a record the workspace seals does
    /// not take.
    fn in_the_clear(&self, seq: u64, title: &str) -> NeighbourMsg {
        let mut document = self.writer.document();
        document.set_actor(automerge::ActorId::from(
            self.member.device_public_key.to_vec(),
        ));
        let heads = document.get_heads();
        document
            .put(automerge::ROOT, "title", title)
            .expect("the fixture's write");
        document.commit_edit("fixture", "title");
        let artifact = collab_types::document_artifact_id(APP, &self.workspace, RECORD);
        let author =
            collab_types::Ed25519PublicKey::from_bytes(self.member_key.verifying_key().to_bytes());
        let mut envelope = SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(document.save_after(&heads))
                .encode()
                .unwrap(),
            artifact,
            author,
            collab_types::document_attestation_id(APP, &self.workspace, RECORD),
            collab_types::OpKind::Change,
            SystemTime::now(),
            seq,
        );
        collab_types::sign_op(&mut envelope, &self.member_key).unwrap();
        NeighbourMsg::AppDocument {
            peer: tilo_peer_state::peer_handle(author.as_bytes()),
            app_id: APP.into(),
            cluster_id: self.workspace.clone(),
            record_id: RECORD.into(),
            envelope: Box::new(envelope),
        }
    }

    /// What the workspace's copy reads as the record's title now.
    async fn readable(&self) -> Option<String> {
        let bytes = self
            .workspaces
            .record_document(&self.workspace, APP, RECORD)
            .await
            .unwrap()?;
        automerge::AutoCommit::load(&bytes)
            .ok()?
            .get(automerge::ROOT, "title")
            .ok()
            .flatten()
            .and_then(|(value, _)| value.into_string().ok())
    }

    /// Every write of the record the member made that the workspace's store
    /// keeps, as the store settled it.
    async fn kept(&self) -> Vec<NeighbourMsg> {
        let channel = neighbour_wire::app_document_channel(APP, RECORD);
        self.relay
            .workspace_records()
            .retained_settled(self.workspace_id)
            .await
            .unwrap()
            .into_iter()
            .filter(|kept| kept.author == self.member.device_public_key)
            .filter_map(|kept| postcard::from_bytes::<NeighbourMsg>(&kept.body).ok())
            .filter(|message| {
                neighbour_wire::retention(message, false).is_some_and(|(held, _)| held == channel)
            })
            .collect()
    }
}

/// The doors a member's session is opened with, every one a test does not
/// vary left closed.
struct Doors {
    registry: WorkspaceRegistry,
    metrics: Arc<crate::metrics::Metrics>,
    trust: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    revocations: Arc<std::sync::RwLock<RevocationList>>,
    agents: AgentRegistry,
    resumes: std::sync::Mutex<HashMap<u64, ResumeLog>>,
    agent: std::sync::OnceLock<String>,
    lag: Arc<Lag>,
    outbound: mpsc::Sender<Envelope>,
    disconnect: mpsc::Sender<()>,
    /// The member's side of the two queues its session is opened with, held
    /// open for the session's life: a closed one is a session that left.
    delivered: mpsc::Receiver<Envelope>,
    disconnected: mpsc::Receiver<()>,
}

impl Doors {
    fn over(
        runtime: &crate::workspace::SharedWorkspace,
        workspace_id: u64,
        outbound: mpsc::Sender<Envelope>,
        disconnect: mpsc::Sender<()>,
    ) -> Self {
        Self {
            registry: Arc::new(AsyncMutex::new(HashMap::from([(
                workspace_id,
                runtime.clone(),
            )]))),
            metrics: Arc::new(crate::metrics::Metrics::default()),
            trust: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
            agents: AgentRegistry::default(),
            resumes: std::sync::Mutex::new(HashMap::new()),
            agent: std::sync::OnceLock::new(),
            lag: Arc::new(Lag::default()),
            outbound,
            disconnect,
            delivered: mpsc::channel(1).1,
            disconnected: mpsc::channel(1).1,
        }
    }

    fn ctx<'a>(&'a self, own_user: &'a [u8; 32], own_cert: &'a DeviceCert) -> SessionCtx<'a> {
        SessionCtx {
            workspaces: &self.registry,
            autobind: None,
            outbound: &self.outbound,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: own_user,
            own_device_cert: own_cert,
            own_device_public_key: &own_cert.device_public_key,
            app_serving: None,
            metrics: &self.metrics,
            trust_policy: &self.trust,
            disconnect: &self.disconnect,
            stop: None,
            presence: None,
            revocations: &self.revocations,
            carried_calls: None,
            admin: None,
            fetch: None,
            agents: &self.agents,
            positions: None,
            compactor: None,
            member_limits: None,
            resumes: &self.resumes,
            agent: &self.agent,
            lag: &self.lag,
            execution: Box::leak(Box::new(tilo_actions::remote::ExecutionLease::mint())),
        }
    }
}

/// The peer `cert`'s device connects as, admitted by the relay's own person.
fn peer_of(cert: &DeviceCert, user: [u8; 32]) -> AuthenticatedPeer {
    crate::auth::accept_hello(
        &crate::auth::Hello::new(cert.clone(), [2; 32]),
        &user,
        &crate::policy::TrustPolicy::AllowMatchingUser,
        &RevocationList::new(),
        unix_now_ns(),
    )
    .expect("the relay admits its own person's device")
}

/// A change the workspace refuses leaves nothing in the record's stored
/// history, so the writer's next change, numbering the refused one's sequence
/// again, is kept and the record reads on past where it would have been.
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the host a test binds has to outlive every step it drives, and its shutdown takes it by value"
)]
async fn a_refused_change_is_never_kept_and_the_same_sequence_still_applies() {
    let lounge = lounge().await;
    let (runtime, doors) = lounge.publishing().await;
    let user = lounge.governor.user_public_key;
    let peer = peer_of(&lounge.member, user);
    let session = doors.ctx(&user, &lounge.governor);

    // The workspace's own governance has not taken the device in yet, so the
    // change it writes is one the workspace refuses.
    let answer = handle_commit(
        &peer,
        &session,
        &[lounge.workspace_id],
        lounge.commit(&lounge.in_the_clear(1, "in the clear"), [1; 16]),
    )
    .await
    .unwrap();
    let Some(Envelope::CommitRefused { reason, .. }) = answer else {
        panic!("a change the workspace refuses is not kept, got {answer:?}");
    };
    assert!(
        reason.contains("not granted") || reason.contains("membership"),
        "the workspace's own reason for refusing it, got {reason}"
    );
    assert!(
        lounge.kept().await.is_empty(),
        "the refused change left nothing of the record's stored history"
    );
    // The workspace's own copy of the record did not take it in either.
    assert_eq!(
        lounge.readable().await.as_deref(),
        Some("Welcome"),
        "the refused change is in no copy of the record"
    );

    // The workspace admits the device, and the same writer writes the same
    // sequence again: a change it takes in, kept like any member's.
    admit_view_reader(
        &lounge.workspaces,
        &lounge.workspace,
        &lounge.governor_key,
        &lounge.member,
    )
    .await;
    // The device's admission reaches the relay's own runtime, and the
    // standings its changes are decided against, as a settled decision
    // carries them there.
    lounge.publishing_again(&runtime, &doors).await;
    project_standings(&lounge.workspaces, &lounge.workspace).await;
    let ((), sealed) = lounge.writer.seal(&lounge.workspace, |document| {
        document
            .put(automerge::ROOT, "title", "sealed")
            .expect("the fixture's write");
        document.commit_edit("fixture", "title");
    });
    let NeighbourMsg::AppDocument { envelope, .. } = &sealed else {
        panic!("a sealed write is a document write")
    };
    assert_eq!(envelope.seq, 1, "the refused change's sequence, again");
    let answer = handle_commit(
        &peer,
        &session,
        &[lounge.workspace_id],
        lounge.commit(&sealed, [2; 16]),
    )
    .await
    .unwrap();
    assert!(
        matches!(answer, Some(Envelope::Committed { .. })),
        "the change after the refusal is kept, got {answer:?}"
    );
    let kept = lounge.kept().await;
    assert_eq!(kept.len(), 1, "one write of the record is kept: {kept:?}");
    let NeighbourMsg::AppDocument { envelope, .. } = &kept[0] else {
        panic!("the kept write is a document write")
    };
    assert_eq!(envelope.seq, 1, "the sequence after the refused change");
    // The workspace reads the kept change on, as it reads any member's.
    tilo_test_env::within("the workspace reads the kept change", async {
        while lounge.readable().await.as_deref() != Some("sealed") {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    drop(lounge.workspaces);
    lounge.relay.shutdown().await;
}
