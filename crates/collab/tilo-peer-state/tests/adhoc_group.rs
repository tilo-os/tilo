//! A workspace five desktops hold with no server in it: three of one person,
//! who made the workspace, and two of a coworker, who joined from an
//! invitation. Every device keeps a writable copy and serves it on its own
//! link, so what any one writes reaches all five, a lease-held job is done on
//! one device at a time and moves when its device goes, and a device that leaves
//! takes nothing of the rest with it. A client keeps its link with one holder
//! at a time and moves to another when its holder is gone.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::{addr, device, localhost_endpoint};

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use iroh::Endpoint;
use iroh::protocol::Router;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync};
use tilo_peer_state::join::invitations::IssueInvitationRequest;
use tilo_peer_state::join::{self, Role};
use tilo_peer_state::join_answer::JoinAnswer;
use tilo_peer_state::owner::enroll::admit_as_member;
use tilo_peer_state::owner::testing;
use tilo_peer_state::owner::{NativeWorkspaceOwner, OwnerJoinWorkspaces, open_file};
use tilo_peer_state::{CLOCK_SKEW_NS, unranked};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

const WORKSPACE: &str = "a0a0a0a0-a0a0-4a0a-8a0a-a0a0a0a0a0a0";
const APP: &str = "test.app";
const DOCUMENT: &str = "tab:1";

/// The job whose side effects only one device at a time may do.
const JOB: &str = "bot:ticker";
const JOB_TTL: Duration = Duration::from_secs(5);

fn workspace() -> tilo_desktop_model::WorkspaceId {
    tilo_desktop_model::WorkspaceId::parse(WORKSPACE).unwrap()
}

/// One desktop: its key, its certificate, the owner that keeps its copy of the
/// workspace, the endpoint it serves and dials on, and the links it holds.
struct Desktop {
    cert: DeviceCert,
    key: SigningKey,
    owner: NativeWorkspaceOwner,
    endpoint: Endpoint,
    router: Option<Router>,
    link: Option<DirectSync>,
    dials: Vec<tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>>>,
    /// How many times this device did the leased job: the side effect the
    /// lease makes happen once.
    done: std::sync::atomic::AtomicU32,
}

impl Desktop {
    /// The desktop whose owner is kept in `name`'s file, with an endpoint bound
    /// to its own key.
    async fn bound(directory: &Path, name: &str, key: SigningKey, cert: DeviceCert) -> Self {
        let endpoint = localhost_endpoint(&key).await;
        Self {
            cert,
            key,
            owner: open_file(&directory.join(format!("{name}.json"))).expect("this device's owner"),
            endpoint,
            router: None,
            link: None,
            dials: Vec::new(),
            done: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn id(&self) -> [u8; 32] {
        self.cert.device_public_key
    }

    /// Hold the workspace here and serve it on this device's own link.
    fn serve(&mut self) {
        let link = DirectSync::over(
            self.owner
                .replica(workspace())
                .expect("this device holds it"),
            self.key.clone(),
        )
        .owning(self.owner.clone(), workspace())
        .expect("this device's copy is served");
        self.router = Some(
            Router::builder(self.endpoint.clone())
                .accept(DIRECT_ALPN, link.clone())
                .spawn(),
        );
        self.link = Some(link);
    }

    /// Dial `other`, keeping the link for as long as both devices are up.
    fn dials(&mut self, other: &Desktop) {
        let link = self.link.clone().expect("a served copy dials");
        let endpoint = self.endpoint.clone();
        let other = addr(&other.endpoint);
        self.dials.push(tokio::spawn(
            async move { link.connect(&endpoint, other).await },
        ));
    }

    fn link(&self) -> &DirectSync {
        self.link.as_ref().expect("this device serves its copy")
    }

    /// What this device's copy of the document holds under `field`.
    fn value(&self, field: &str) -> Option<String> {
        let doc = self.link().read(|peer| peer.document(APP, DOCUMENT))?;
        let doc = AutoCommit::load(&doc).unwrap();
        doc.get(ROOT, field)
            .unwrap()
            .map(|(value, _)| value.into_string().unwrap_or_default())
    }

    /// Write `value` under `field` as this device, and publish it on every link.
    fn write(&self, field: &str, value: &str) {
        let mut edit = match self.link().read(|peer| peer.document(APP, DOCUMENT)) {
            Some(bytes) => AutoCommit::load(&bytes).unwrap(),
            None => AutoCommit::new(),
        };
        edit.set_actor(automerge::ActorId::from(
            self.key.verifying_key().to_bytes().to_vec(),
        ));
        let heads = edit.get_heads();
        edit.put(ROOT, field, value).unwrap();
        edit.commit_edit("fixture", field);
        let change = edit.save_after(&heads);
        self.link()
            .publish(|peer| {
                peer.publication_mut().publish_incremental(
                    APP,
                    DOCUMENT,
                    change,
                    &self.key,
                    SystemTime::now(),
                )
            })
            .unwrap();
    }

    /// Take the job's lease here at `now`, answering whether this device holds
    /// it and so does the job. The claim is workspace state, published to the
    /// other holders the way this device's own governance is.
    fn takes_job(&self, now: SystemTime) -> bool {
        let before = self
            .link()
            .read(|peer| peer.workspace_heads().unwrap_or_default());
        let claimed = self
            .link()
            .replica()
            .write(|peer| {
                peer.workspace_mut()
                    .take_lease(&self.cert, JOB, JOB_TTL, unranked(), now)
            })
            .unwrap();
        self.publish_workspace_since(&before);
        if claimed.is_some() {
            self.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        claimed.is_some()
    }

    /// Who holds the job's lease, as this device's copy reads it at `now`.
    fn job_holder(&self, now: SystemTime) -> Option<[u8; 32]> {
        self.link()
            .read(|peer| peer.workspace().lease(JOB, now).holder)
            .map(|claim| claim.device)
    }

    fn publish_workspace_since(&self, before: &[automerge::ChangeHash]) {
        let changes = self
            .link()
            .read(|peer| peer.workspace_changes_since(before));
        if changes.is_empty() {
            return;
        }
        self.link()
            .send(neighbour_wire::NeighbourMsg::SyncResp {
                changes,
                workspace: WORKSPACE.to_owned(),
            })
            .unwrap();
    }

    /// Stop serving and dialing: what a device going away looks like to the
    /// rest of the group.
    async fn leaves(mut self) {
        for dial in self.dials.drain(..) {
            dial.abort();
            let _ = dial.await;
        }
        if let Some(router) = self.router.take() {
            router.shutdown().await.unwrap();
        }
        self.endpoint.close().await;
    }
}

/// Wait until `done` holds of every desktop in `desktops`.
async fn until(desktops: &[&Desktop], step: &str, done: impl Fn(&Desktop) -> bool + Sync) {
    let mut waits: Vec<_> = desktops
        .iter()
        .enumerate()
        .map(|(position, desktop)| (position, *desktop, desktop.link().changes()))
        .collect();
    let timeout = tokio::time::timeout(Duration::from_secs(60), async {
        while !waits.is_empty() {
            for index in (0..waits.len()).rev() {
                let (_, desktop, changes) = waits.get_mut(index).expect("a desktop still waits");
                if done(desktop) {
                    waits.swap_remove(index);
                } else {
                    changes.changed().await.expect("the device stays up");
                }
            }
        }
    })
    .await;
    if timeout.is_err() {
        let left: Vec<_> = waits
            .iter()
            .filter(|(_, desktop, _)| !done(desktop))
            .map(|(position, _, _)| *position)
            .collect();
        panic!("every device converges: {step} (waiting on {left:?})");
    }
}

/// The invitation the workspace's own device issues, as the text a person
/// hands their coworker.
async fn invitation(with: &Desktop, request_id: [u8; 32]) -> String {
    let issued = with
        .owner
        .issue_invitation(
            workspace(),
            &with.cert,
            &IssueInvitationRequest {
                request_id,
                role: Role::Editor,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                inviter_name: None,
            },
            false,
            &[testing::own_host_hint_here(
                workspace(),
                &with.cert,
                &with.key,
            )],
            &with.key,
            SystemTime::now(),
        )
        .await
        .expect("the workspace's device issues an invitation");
    join::encode_invitation_text(&issued.signed_invitation)
}

/// `joiner` redeems `invitation` through `with`'s own join answers: what a
/// device does with a link a person hands it, and nothing else.
async fn joins(joiner: &Desktop, invitation: String, with: &Desktop) {
    let service: Arc<dyn tilo_peer_state::join_transport::LocalJoinService> = Arc::new(JoinAnswer(
        OwnerJoinWorkspaces::new(with.owner.clone(), with.key.clone()),
    ));
    let enrollment = tilo_peer_state::owner::enroll::prepare_and_complete(
        tilo_peer_state::join_transport::WorkspaceJoinClient::local(
            service,
            joiner.key.verifying_key().to_bytes(),
        ),
        DeviceIdentity::kept(joiner.cert.clone(), joiner.key.clone()),
        join::WorkspaceId::parse(WORKSPACE).expect("a workspace topic"),
        None,
        invitation,
        joiner.owner.clone(),
    )
    .await
    .expect("the workspace's device admits this one");
    joiner
        .owner
        .install(&enrollment, SystemTime::now())
        .expect("the join installs here");
}

/// `joiner` enters as one of its person's further devices, through the
/// workspace `with` holds: no invitation, and no server to ask.
async fn enters_as_member(joiner: &Desktop, with: &Desktop) {
    let service: Arc<dyn tilo_peer_state::join_transport::LocalJoinService> = Arc::new(JoinAnswer(
        OwnerJoinWorkspaces::new(with.owner.clone(), with.key.clone()),
    ));
    admit_as_member(
        tilo_peer_state::join_transport::WorkspaceJoinClient::local(
            service,
            joiner.key.verifying_key().to_bytes(),
        ),
        DeviceIdentity::kept(joiner.cert.clone(), joiner.key.clone()),
        joiner.owner.clone(),
        workspace(),
        testing::own_host_hint_here(workspace(), &with.cert, &with.key),
        SystemTime::now(),
    )
    .await
    .expect("the workspace admits this person's further device");
}

/// Five desktops holding one workspace, with the wiring each link needs: the
/// workspace made on `a1`, and every desktop serving and reachable.
async fn held_by_five(directory: &Path) -> [Desktop; 5] {
    let root_a = SigningKey::from_bytes(&[0x51; 32]);
    let root_b = SigningKey::from_bytes(&[0x61; 32]);
    let (a1_key, a1_cert) = device(&root_a, 0x52);
    let (a2_key, a2_cert) = device(&root_a, 0x53);
    let (a3_key, a3_cert) = device(&root_a, 0x54);
    let (b1_key, b1_cert) = device(&root_b, 0x62);
    let (b2_key, b2_cert) = device(&root_b, 0x63);
    let mut a1 = Desktop::bound(directory, "a1", a1_key, a1_cert.clone()).await;
    let mut a2 = Desktop::bound(directory, "a2", a2_key, a2_cert).await;
    let mut a3 = Desktop::bound(directory, "a3", a3_key, a3_cert).await;
    let mut b1 = Desktop::bound(directory, "b1", b1_key, b1_cert).await;
    let mut b2 = Desktop::bound(directory, "b2", b2_key, b2_cert).await;

    // A's first desktop makes the workspace, naming its own address as where
    // the workspace is reached, and starts the document its apps write to. No
    // server and no admin workspace is made for any of this.
    a1.owner
        .create_workspace(
            workspace(),
            "Shared",
            &a1.cert,
            &a1.key,
            Some(&testing::own_host_hint_here(workspace(), &a1.cert, &a1.key)),
            SystemTime::now(),
        )
        .await
        .expect("A's desktop makes the workspace");
    testing::start_resource(
        &a1.owner,
        workspace(),
        APP,
        DOCUMENT,
        &a1.key,
        SystemTime::now(),
    );
    enters_as_member(&a2, &a1).await;
    enters_as_member(&a3, &a1).await;

    // B joins from the invitation with nothing else to do: no desktop of
    // theirs to configure and no second step.
    joins(&b1, invitation(&a1, [1; 32]).await, &a1).await;
    // B's second desktop is B's own device, so the workspace admits it as one:
    // no second invitation. B's first desktop is a key holder, and any key
    // holder lets a member's further device in.
    let before = b1
        .owner
        .replica(workspace())
        .expect("B's first desktop holds the workspace")
        .read(|peer| peer.workspace_heads().unwrap_or_default());
    enters_as_member(&b2, &b1).await;

    a1.serve();
    a2.serve();
    a3.serve();
    b1.serve();
    b2.serve();
    // The holders reach each other: each dials the holders above it, one link
    // per pair. A's first desktop reaches B's first before the rest: B's first
    // desktop hands what it admitted to the holders it links with, as it hands
    // every mark it makes in the workspace, and A's first desktop carries it
    // on to the others in the governance its own links start with.
    a1.dials(&b1);
    tokio::time::timeout(
        Duration::from_secs(30),
        a1.link().caught_up().wait_for(|up| *up),
    )
    .await
    .expect("A's first desktop reaches B's first")
    .unwrap();
    b1.publish_workspace_since(&before);
    until(&[&a1], "the holders know B's second desktop", |desktop| {
        desktop
            .link()
            .read(|peer| peer.permissions(b2.id(), SystemTime::now()).is_ok())
    })
    .await;
    assert_eq!(
        b2.link()
            .read(|peer| peer.governance().owner_user_public_key()),
        a1.cert.user_public_key,
        "B's second desktop holds the workspace's owner, not the device it joined through"
    );
    a1.dials(&a2);
    a1.dials(&a3);
    a2.dials(&a3);
    a2.dials(&b1);
    a3.dials(&b1);
    // B's second desktop keeps its link with one holder at a time, as a
    // member's device does: it reaches a holder that knows it.
    b2.dials(&a1);

    [a1, a2, a3, b1, b2]
}

#[tokio::test(flavor = "multi_thread")]
async fn five_desktops_hold_one_workspace_with_no_server_in_it() {
    let directory = tempfile::tempdir().unwrap();
    let [a1, a2, a3, b1, mut b2] = Box::pin(held_by_five(directory.path())).await;
    let all: [&Desktop; 5] = [&a1, &a2, &a3, &b1, &b2];

    // Everybody holds the workspace and the document it starts with.
    for desktop in &all {
        assert!(
            desktop.owner.contains_workspace(workspace()),
            "every desktop of the group holds the workspace"
        );
    }
    until(&all, "every desktop holds the document", |desktop| {
        desktop
            .link()
            .read(|peer| peer.document(APP, DOCUMENT))
            .is_some()
    })
    .await;

    // A write on any one of the five reaches the other four: each writes under
    // its own field, and every copy ends with all five.
    for (name, desktop) in [
        ("a1", &a1),
        ("a2", &a2),
        ("a3", &a3),
        ("b1", &b1),
        ("b2", &b2),
    ] {
        desktop.write(name, name);
    }
    until(&all, "every write reaches all five", |desktop| {
        ["a1", "a2", "a3", "b1", "b2"]
            .into_iter()
            .all(|name| desktop.value(name).as_deref() == Some(name))
    })
    .await;

    // The job is leased: one device does it, and the one that does not hold
    // the lease does nothing. The claim is workspace state, so every holder
    // reads the same holder.
    let now = SystemTime::now();
    assert!(a2.takes_job(now), "the first claimant takes the free lease");
    let a2_id = a2.id();
    until(&all, "every holder reads the claimant", |desktop| {
        desktop.job_holder(now) == Some(a2_id)
    })
    .await;
    assert!(
        !b1.takes_job(now) && !b2.takes_job(now),
        "a device that finds the lease held does nothing"
    );
    assert_eq!(
        a2.done.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the job was done once, on the lease's holder"
    );

    // The holder's device goes away without a goodbye: its deadline passes,
    // and one of the others takes the work on from there.
    a2.leaves().await;
    let later = now + JOB_TTL + Duration::from_nanos(CLOCK_SKEW_NS) + Duration::from_secs(1);
    let remaining: [&Desktop; 4] = [&a1, &a3, &b1, &b2];
    assert!(
        a3.takes_job(later),
        "the first device to see the deadline passed takes the work"
    );
    let new_holder = a3.id();
    until(
        &remaining,
        "every holder reads the new lease holder",
        |desktop| desktop.job_holder(later) == Some(new_holder),
    )
    .await;
    assert!(
        !b1.takes_job(later) && !b2.takes_job(later),
        "once one device took the work over, the others do nothing"
    );
    // The job was done once on the device that held the lease, once on the
    // device that took it over, and never anywhere else.
    assert_eq!(
        (
            a3.done.load(std::sync::atomic::Ordering::Relaxed),
            a1.done.load(std::sync::atomic::Ordering::Relaxed),
            b1.done.load(std::sync::atomic::Ordering::Relaxed),
            b2.done.load(std::sync::atomic::Ordering::Relaxed),
        ),
        (1, 0, 0, 0),
        "the job is done once per term and nowhere else"
    );

    // A device leaving takes nothing of the rest with it: with the founder
    // gone, what one holder writes still reaches the others.
    a1.leaves().await;
    a3.write("after-a1", "written with the founder gone");
    let linked: [&Desktop; 2] = [&a3, &b1];
    until(&linked, "the rest keep carrying writes", |desktop| {
        desktop.value("after-a1").as_deref() == Some("written with the founder gone")
    })
    .await;

    // A client whose holder is gone moves to another holder and catches up on
    // what was written while it held no link there.
    b2.dials(&a3);
    until(
        &[&b2],
        "the client catches up from its new holder",
        |desktop| desktop.value("after-a1").as_deref() == Some("written with the founder gone"),
    )
    .await;
    b2.write("from-b2", "through its new holder");
    let moved: [&Desktop; 3] = [&a3, &b1, &b2];
    until(
        &moved,
        "the client writes through its new holder",
        |desktop| desktop.value("from-b2").as_deref() == Some("through its new holder"),
    )
    .await;
    assert_eq!(
        b2.value("after-a1").as_deref(),
        Some("written with the founder gone"),
        "the client caught up on what it missed"
    );

    for desktop in [a3, b1, b2] {
        desktop.leaves().await;
    }
}
