//! A workspace five desktops hold with no server in it: three of one person,
//! who made the workspace, and two of a coworker, who joined from an
//! invitation. Every desktop opens its own workspace service in process, as
//! each desktop does; the workspace is placed on the service of the desktop it
//! was made on, and every desktop takes part in it as a member over its own
//! direct link. What any one writes reaches all five, a lease-held job is done
//! on one device at a time and moves when its device goes, and a device that
//! leaves takes nothing of the rest with it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::{device, localhost_endpoint};

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use iroh::Endpoint;
use iroh::protocol::Router;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tilo_identity::identity_store::DeviceIdentity;
use tilo_peer_state::direct::DirectSync;
use tilo_peer_state::join::{self, Role, TransportHint};
use tilo_peer_state::join_transport::WorkspaceJoinClient;
use tilo_peer_state::owner::enroll::{admit_as_member, prepare_and_complete};
use tilo_peer_state::owner::session::{
    CannotDial, Connect, Holder, Link, Linked, OwnHost, Reach, Scope,
};
use tilo_peer_state::owner::{NativeWorkspaceOwner, open_file};
use tilo_peer_state::{CLOCK_SKEW_NS, CopyRole, unranked};
use tilo_server::Reaching;
use tilo_server::service::{ServiceConfig, ServiceIdentity, WorkspaceService};
use tilo_server::workspaces::IssueInvitationRequest;
use tilo_server_protocol::WorkSetting;
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

/// One desktop: its key and certificate, its own workspace service on its
/// endpoint (as every desktop opens one, taking no one else's work), the owner
/// that keeps its member's copy of the workspace, and its link to the
/// workspace.
struct Desktop {
    cert: DeviceCert,
    key: SigningKey,
    owner: NativeWorkspaceOwner,
    endpoint: Endpoint,
    service: Arc<WorkspaceService>,
    router: Router,
    link: Option<DirectSync>,
    syncing: Option<tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>>>,
    /// How many times this device did the leased job: the side effect the
    /// lease makes happen once.
    done: AtomicU32,
}

impl Desktop {
    /// The desktop `name`, with its workspace service open on an endpoint
    /// bound to its own key, and its owner kept in `directory`.
    async fn start(directory: &Path, name: &str, key: SigningKey, cert: DeviceCert) -> Self {
        let endpoint = localhost_endpoint(&key).await;
        let mut config = ServiceConfig::new(
            directory.join(name).join("workspace-service"),
            WorkSetting::none(),
        );
        // The group's endpoints reach each other on this machine alone: no
        // other machine names where they are reached.
        config.reaching = Reaching::OnItsOwn;
        // The service writes as the desktop, whose key its endpoint is bound with: one device,
        // one key, as every device's service opens.
        let identity = ServiceIdentity::as_device(&cert, key.clone());
        let service = WorkspaceService::open(config, identity, endpoint.clone(), None)
            .await
            .expect("this desktop's workspace service opens");
        let mut router = Router::builder(endpoint.clone());
        for (alpn, handler) in service.protocols() {
            router = router.accept(alpn, handler);
        }
        Self {
            owner: open_file(&directory.join(format!("{name}.json"))).expect("this device's owner"),
            cert,
            key,
            endpoint,
            service: Arc::new(service),
            router: router.spawn(),
            link: None,
            syncing: None,
            done: AtomicU32::new(0),
        }
    }

    fn id(&self) -> [u8; 32] {
        self.cert.device_public_key
    }

    fn identity(&self) -> DeviceIdentity {
        DeviceIdentity::kept(self.cert.clone(), self.key.clone())
    }

    /// Make the workspace on this desktop: its own service places it and
    /// invites this device, which redeems the invitation in process, as a
    /// desktop founds every workspace it makes.
    async fn founds(&self) {
        let invitation = self
            .service
            .place_own_workspace(WORKSPACE, "Shared", &self.cert)
            .await
            .expect("this desktop's service places the workspace");
        let enrollment = prepare_and_complete(
            WorkspaceJoinClient::local(self.service.join_service(), self.id()),
            self.identity(),
            workspace(),
            None,
            invitation,
            self.owner.clone(),
        )
        .await
        .expect("this desktop's service admits it");
        self.owner
            .install(&enrollment, SystemTime::now())
            .expect("the founding installs here");
    }

    /// Whether this desktop founds the workspace's key group.
    fn founds_group(&self) -> bool {
        self.owner
            .with_current(workspace(), |view| Ok(view.founded_here()))
            .expect("this desktop holds the workspace")
    }

    /// Where the workspace is held: the signed address of each of its holders.
    async fn holders(&self) -> Vec<TransportHint> {
        self.service
            .workspaces()
            .hosts(WORKSPACE)
            .await
            .expect("this desktop's service holds the workspace")
    }

    /// Where this desktop's own service holds the workspace.
    async fn own_address(&self) -> TransportHint {
        self.holders()
            .await
            .into_iter()
            .find(|hint| hint.host.is_of_device(&self.id()))
            .expect("this desktop holds the workspace")
    }

    /// The invitation this desktop's service issues for the workspace, as the
    /// text a person hands their coworker.
    async fn invitation(&self, request_id: [u8; 32]) -> String {
        let issued = self
            .service
            .workspaces()
            .issue(
                WORKSPACE,
                &self.cert,
                IssueInvitationRequest {
                    request_id,
                    role: Role::Editor,
                    recipient_user_public_key: None,
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                    inviter_name: None,
                },
            )
            .await
            .expect("the workspace's holder issues an invitation");
        join::encode_invitation_text(&issued.signed_invitation)
    }

    /// Redeem `invitation` with the workspace's holder over the network: what
    /// a device does with a link a person hands it, and nothing else.
    async fn joins(&self, invitation: String) {
        let enrollment = prepare_and_complete(
            WorkspaceJoinClient::new(self.endpoint.clone()),
            self.identity(),
            workspace(),
            None,
            invitation,
            self.owner.clone(),
        )
        .await
        .expect("the workspace's holder admits this device");
        self.owner
            .install(&enrollment, SystemTime::now())
            .expect("the join installs here");
    }

    /// Enter as one of its person's further devices at the holder `at`: no
    /// invitation, and no server to ask.
    async fn enters_as_member(&self, at: TransportHint) {
        admit_as_member(
            WorkspaceJoinClient::new(self.endpoint.clone()),
            self.identity(),
            self.owner.clone(),
            workspace(),
            at,
            SystemTime::now(),
        )
        .await
        .expect("the workspace admits this person's further device");
    }

    /// Link this device's copy of the workspace to the holder `via`: in
    /// process when it is this desktop's own service, over the holder's
    /// endpoint otherwise, as a desktop's session reaches every holder.
    async fn links_to(&mut self, via: &TransportHint) {
        let connect = Connect::new(self.owner.clone(), workspace()).expect("enrolled here");
        let reach = Reach {
            endpoint: self.endpoint.clone(),
            own_host: Some(Arc::clone(&self.service) as Arc<dyn OwnHost>),
        };
        let linked = reach
            .session(&connect, via, Arc::new(self.key.clone()))
            .await
            .expect("this device reaches the holder");
        self.link = Some(linked.link.clone());
        self.syncing = Some(tokio::spawn(linked.run()));
    }

    /// Link to the holder this device's enrollment names.
    async fn takes_part(&mut self) {
        let connect = Connect::new(self.owner.clone(), workspace()).expect("enrolled here");
        let enrolled = connect
            .holders(&self.id())
            .expect("the workspace names its holders")
            .into_iter()
            .next()
            .expect("the enrollment names a holder");
        self.links_to(&enrolled).await;
    }

    fn link(&self) -> &DirectSync {
        self.link
            .as_ref()
            .expect("this device takes part in the workspace")
    }

    /// The resource `resource` as this device has it.
    fn loaded(&self, resource: &str) -> Option<AutoCommit> {
        let doc = self.link().read(|peer| peer.document(APP, resource))?;
        Some(AutoCommit::load(&doc).unwrap())
    }

    /// What this device's copy of the document holds under `field`.
    fn value(&self, field: &str) -> Option<String> {
        let doc = self.loaded(DOCUMENT)?;
        doc.get(ROOT, field)
            .unwrap()
            .map(|(value, _)| value.into_string().unwrap_or_default())
    }

    /// Every change of the resource `resource` this device has, each as its
    /// bytes: the resource as it stands, byte for byte.
    fn changes_of(&self, resource: &str) -> Option<std::collections::BTreeSet<Vec<u8>>> {
        let mut doc = self.loaded(resource)?;
        Some(
            doc.get_changes(&[])
                .into_iter()
                .map(|change| change.raw_bytes().to_vec())
                .collect(),
        )
    }

    /// Start the document the group writes to, as this device.
    fn starts_document(&self) {
        self.starts(DOCUMENT);
    }

    /// Start the resource `resource`, as this device.
    fn starts(&self, resource: &str) {
        self.link()
            .publish(|peer| {
                peer.publication_mut().publish_document(
                    APP,
                    resource,
                    AutoCommit::new().save(),
                    &self.key,
                    SystemTime::now(),
                )
            })
            .expect("this device starts the document");
    }

    /// Write `value` under `field` as this device, and publish it on its link.
    fn write(&self, field: &str, value: &str) {
        self.writes(DOCUMENT, field, value);
    }

    /// Write `value` under `field` of the resource `resource` as this device,
    /// and send it on its link.
    fn writes(&self, resource: &str, field: &str, value: &str) {
        let mut edit = self.loaded(resource).unwrap_or_default();
        edit.set_actor(automerge::ActorId::from(
            self.key.verifying_key().to_bytes().to_vec(),
        ));
        let heads = edit.get_heads();
        edit.put(ROOT, field, value).unwrap();
        edit.commit_edit("fixture", field, &self.cert.user_public_key);
        let change = edit.save_after(&heads);
        self.link()
            .publish(|peer| {
                peer.publication_mut().publish_incremental(
                    APP,
                    resource,
                    change,
                    &self.key,
                    SystemTime::now(),
                )
            })
            .unwrap();
    }

    /// Take the job's lease here at `now`, answering whether this device holds
    /// it and so does the job. The claim is workspace state this device signs,
    /// published on its link as any of its changes to the workspace.
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
        let changes = self
            .link()
            .read(|peer| peer.workspace_changes_since(&before));
        if !changes.is_empty() {
            self.link()
                .send(neighbour_wire::NeighbourMsg::SyncResp {
                    changes,
                    workspace: WORKSPACE.to_owned(),
                })
                .unwrap();
        }
        if claimed.is_some() {
            self.done.fetch_add(1, Ordering::Relaxed);
        }
        claimed.is_some()
    }

    /// Who holds the job's lease, as this device's copy reads it at `now`.
    fn job_holder(&self, now: SystemTime) -> Option<[u8; 32]> {
        self.link()
            .read(|peer| peer.workspace().lease(JOB, now).holder)
            .map(|claim| claim.device)
    }

    /// End its link and stop its service and endpoint: what a device going
    /// away looks like to the rest of the group.
    async fn leaves(mut self) {
        self.drops_link().await;
        self.router.shutdown().await.unwrap();
        if let Ok(service) = Arc::try_unwrap(self.service) {
            service.shutdown().await;
        }
        self.endpoint.close().await;
    }

    async fn drops_link(&mut self) {
        if let Some(syncing) = self.syncing.take() {
            syncing.abort();
            let _ = syncing.await;
        }
        self.link = None;
    }
}

/// Wait until `done` holds of every desktop in `desktops`.
async fn until(desktops: &[&Desktop], step: &str, done: impl Fn(&Desktop) -> bool + Sync) {
    let mut waits: Vec<_> = desktops
        .iter()
        .enumerate()
        .map(|(position, desktop)| (position, *desktop, desktop.link().changes()))
        .collect();
    let timeout = tokio::time::timeout(Duration::from_secs(30), async {
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

/// Five desktops of two people in one workspace, made on `a1`, each linked to
/// the service it is held on and holding the document the group writes to.
async fn held_by_five(directory: &Path) -> [Desktop; 5] {
    let root_a = SigningKey::from_bytes(&[0x51; 32]);
    let root_b = SigningKey::from_bytes(&[0x61; 32]);
    let (a1_key, a1_cert) = device(&root_a, 0x52);
    let (a2_key, a2_cert) = device(&root_a, 0x53);
    let (a3_key, a3_cert) = device(&root_a, 0x54);
    let (b1_key, b1_cert) = device(&root_b, 0x62);
    let (b2_key, b2_cert) = device(&root_b, 0x63);
    let mut a1 = Desktop::start(directory, "a1", a1_key, a1_cert).await;
    let mut a2 = Desktop::start(directory, "a2", a2_key, a2_cert).await;
    let mut a3 = Desktop::start(directory, "a3", a3_key, a3_cert).await;
    let mut b1 = Desktop::start(directory, "b1", b1_key, b1_cert).await;
    let mut b2 = Desktop::start(directory, "b2", b2_key, b2_cert).await;

    // A's first desktop makes the workspace on its own service and starts the
    // document its apps write to. No server and no admin workspace is made for
    // any of this.
    a1.founds().await;
    a1.takes_part().await;
    a1.starts_document();
    let at_a1 = a1
        .holders()
        .await
        .into_iter()
        .next()
        .expect("the workspace is held where it was made");
    // A's other desktops are A's own devices: the workspace admits them as
    // such, with no invitation.
    a2.enters_as_member(at_a1.clone()).await;
    a3.enters_as_member(at_a1.clone()).await;
    // B joins from the invitation with nothing else to do, and B's second
    // desktop enters as B's further device.
    b1.joins(a1.invitation([1; 32]).await).await;
    b2.enters_as_member(at_a1).await;
    for desktop in [&mut a2, &mut a3, &mut b1, &mut b2] {
        desktop.takes_part().await;
    }
    let desktops = [a1, a2, a3, b1, b2];
    until(
        &desktops.each_ref(),
        "every desktop holds the document",
        |desktop| {
            desktop
                .link()
                .read(|peer| peer.document(APP, DOCUMENT))
                .is_some()
        },
    )
    .await;
    desktops
}

#[tokio::test(flavor = "multi_thread")]
async fn five_desktops_hold_one_workspace_with_no_server_in_it() {
    let directory = tempfile::tempdir().unwrap();
    let [a1, a2, a3, b1, b2] = Box::pin(held_by_five(directory.path())).await;
    let all: [&Desktop; 5] = [&a1, &a2, &a3, &b1, &b2];
    for desktop in &all {
        assert!(
            desktop.owner.contains_workspace(workspace()),
            "every desktop of the group holds the workspace"
        );
    }

    // A write on any of the desktops that joined reaches the other four: each
    // writes under its own field, and every copy ends with all of them.
    let writers = [("a2", &a2), ("a3", &a3), ("b1", &b1), ("b2", &b2)];
    for (name, desktop) in writers {
        desktop.write(name, name);
    }
    until(&all, "every write reaches all five", |desktop| {
        writers
            .iter()
            .all(|(name, _)| desktop.value(name).as_deref() == Some(*name))
    })
    .await;

    // The job is leased: one device does it, and a device that does not hold
    // the lease does nothing. The claim is workspace state, so every desktop
    // reads the same holder.
    let now = SystemTime::now();
    assert!(a2.takes_job(now), "the first claimant takes the free lease");
    let a2_id = a2.id();
    until(&all, "every desktop reads the claimant", |desktop| {
        desktop.job_holder(now) == Some(a2_id)
    })
    .await;
    assert!(
        !b1.takes_job(now) && !b2.takes_job(now),
        "a device that finds the lease held does nothing"
    );
    assert_eq!(
        a2.done.load(Ordering::Relaxed),
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
        "every desktop reads the new lease holder",
        |desktop| desktop.job_holder(later) == Some(new_holder),
    )
    .await;
    assert!(
        !b1.takes_job(later) && !b2.takes_job(later),
        "once one device took the work over, the others do nothing"
    );
    assert_eq!(
        (
            a3.done.load(Ordering::Relaxed),
            a1.done.load(Ordering::Relaxed),
            b1.done.load(Ordering::Relaxed),
            b2.done.load(Ordering::Relaxed),
        ),
        (1, 0, 0, 0),
        "the job is done once per term and nowhere else"
    );

    // A device leaving takes nothing of the rest with it: what the others
    // write still reaches each of them.
    b2.write("after-a2", "written with a2 gone");
    until(&remaining, "the rest keep carrying writes", |desktop| {
        desktop.value("after-a2").as_deref() == Some("written with a2 gone")
    })
    .await;

    for desktop in [a1, a3, b1, b2] {
        desktop.leaves().await;
    }
}

/// The desktop a workspace was made on founds its key group, though its own
/// service created the workspace: nobody else would, and the desktop could
/// seal nothing. A further desktop of the same person does not.
#[tokio::test(flavor = "multi_thread")]
async fn the_desktop_a_workspace_was_made_on_founds_its_key_group() {
    let directory = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[0x71; 32]);
    let (a1_key, a1_cert) = device(&root, 0x72);
    let (a2_key, a2_cert) = device(&root, 0x73);
    let desktops = [
        Desktop::start(directory.path(), "a1", a1_key, a1_cert).await,
        Desktop::start(directory.path(), "a2", a2_key, a2_cert).await,
    ];
    let [a1, a2] = &desktops;
    a1.founds().await;
    a2.enters_as_member(a1.own_address().await).await;
    assert!(
        a1.founds_group(),
        "the desktop it was made on founds the group"
    );
    assert!(!a2.founds_group(), "a further desktop joins the group");
    for desktop in desktops {
        desktop.leaves().await;
    }
}

/// The desktop the workspace was made on writes to it once the others linked,
/// and its write reaches them, as every other desktop's does.
#[tokio::test(flavor = "multi_thread")]
async fn the_desktop_a_workspace_was_made_on_writes_to_it_after_the_others_linked() {
    let directory = tempfile::tempdir().unwrap();
    let desktops = Box::pin(held_by_five(directory.path())).await;
    let [a1, ..] = &desktops;
    a1.write("a1", "from the desktop the workspace was made on");
    until(
        &desktops.each_ref(),
        "the write reaches all five",
        |desktop| {
            desktop.value("a1").as_deref() == Some("from the desktop the workspace was made on")
        },
    )
    .await;
    for desktop in desktops {
        desktop.leaves().await;
    }
}

/// The desktop the workspace was made on leaves, and the rest keep it: the
/// workspace is its person's, so that person's other desktops hold it too,
/// each on its own service, and every desktop whose link was to the one that
/// left links to a holder still up. A desktop of the coworker, who does not
/// govern the workspace, holds none of it, and still has every write.
#[tokio::test(flavor = "multi_thread")]
async fn the_rest_keep_the_workspace_when_the_desktop_it_was_made_on_leaves() {
    let directory = tempfile::tempdir().unwrap();
    let [a1, mut a2, mut a3, mut b1, mut b2] = Box::pin(held_by_five(directory.path())).await;

    // The workspace's person has their other desktops hold it too.
    for holder in [&a2, &a3] {
        a1.service
            .workspaces()
            .attach_server(
                WORKSPACE,
                holder
                    .service
                    .workspaces()
                    .device_cert()
                    .device_public_key,
                holder.service.node_addr(),
                CopyRole::KeyHolder,
            )
            .await
            .expect("another desktop of the workspace's person holds it too");
    }
    // The coworker does not govern the workspace: their desktop holds none of
    // it.
    assert!(
        a1.service
            .workspaces()
            .attach_server(
                WORKSPACE,
                b1.service
                    .workspaces()
                    .device_cert()
                    .device_public_key,
                b1.service.node_addr(),
                CopyRole::KeyHolder,
            )
            .await
            .is_err(),
        "a desktop of someone who does not govern the workspace holds none of it"
    );
    a2.write("before", "written while every desktop was up");
    let all: [&Desktop; 5] = [&a1, &a2, &a3, &b1, &b2];
    until(&all, "every desktop has the first write", |desktop| {
        desktop.value("before").as_deref() == Some("written while every desktop was up")
    })
    .await;

    // The desktop the workspace was made on goes away. Each of the others
    // links to a holder still up: A's desktops to their own service, B's to
    // one of A's.
    a1.leaves().await;
    let at_a2 = a2.own_address().await;
    let at_a3 = a3.own_address().await;
    for (desktop, at) in [
        (&mut a2, &at_a2),
        (&mut a3, &at_a3),
        (&mut b1, &at_a2),
        (&mut b2, &at_a3),
    ] {
        desktop.drops_link().await;
        desktop.links_to(at).await;
    }

    a3.write("after-a1", "written with the founder gone");
    b2.write("from-b2", "through another holder");
    let rest: [&Desktop; 4] = [&a2, &a3, &b1, &b2];
    until(&rest, "the rest keep carrying writes", |desktop| {
        desktop.value("after-a1").as_deref() == Some("written with the founder gone")
            && desktop.value("from-b2").as_deref() == Some("through another holder")
    })
    .await;

    for desktop in [a2, a3, b1, b2] {
        desktop.leaves().await;
    }
}

/// The desktop a workspace was made on hands it to another desktop of its
/// person and goes away at once, with no other copy of the workspace up. The
/// desktop it was handed to has every resource as it stood: a desktop that
/// enters the workspace after reads each of them, change for change, from it
/// alone, and still does once that desktop starts again from its files.
#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_handed_the_workspace_keeps_all_of_it_when_the_one_that_handed_it_leaves_at_once()
{
    const RESOURCES: [&str; 3] = [DOCUMENT, "tab:2", "tab:3"];
    let directory = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[0x81; 32]);
    let (a1_key, a1_cert) = device(&root, 0x82);
    let (a2_key, a2_cert) = device(&root, 0x83);
    let mut a1 = Desktop::start(directory.path(), "a1", a1_key, a1_cert).await;
    let a2 = Desktop::start(directory.path(), "a2", a2_key.clone(), a2_cert.clone()).await;
    a1.founds().await;
    a1.takes_part().await;
    for (at, resource) in RESOURCES.iter().enumerate() {
        a1.starts(resource);
        a1.writes(resource, "title", &format!("resource {at}"));
        a1.writes(resource, "body", &format!("what resource {at} says"));
    }
    let made: Vec<_> = RESOURCES
        .iter()
        .map(|resource| a1.changes_of(resource).expect("the resource is written"))
        .collect();
    let has_all = |desktop: &Desktop| {
        RESOURCES
            .iter()
            .zip(&made)
            .all(|(resource, made)| desktop.changes_of(resource).as_ref() == Some(made))
    };

    // Another desktop of the person reads every resource from the desktop it
    // was made on, whose service has them all. It goes away then.
    let (seen_key, seen_cert) = device(&root, 0x84);
    let mut seen = Desktop::start(directory.path(), "seen", seen_key, seen_cert).await;
    seen.enters_as_member(a1.own_address().await).await;
    seen.takes_part().await;
    until(
        &[&seen],
        "the desktop it was made on serves all of it",
        has_all,
    )
    .await;
    seen.leaves().await;

    // The desktop it was made on hands it to the person's other desktop and
    // goes away straight after: no copy of the workspace but the one handed
    // over is up.
    a1.service
        .workspaces()
        .attach_server(
            WORKSPACE,
            a2.service
                .workspaces()
                .device_cert()
                .device_public_key,
            a2.service.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .expect("the person's other desktop takes the workspace");
    a1.leaves().await;

    // What a page reads from the desktop handed the workspace, its service's
    // own copy, is all of it too, before it ever starts again.
    let served = a2.service.workspaces();
    let mut writes = served.document_writes();
    tilo_test_env::within(
        "the copy of the desktop handed the workspace holds all of it",
        async {
            loop {
                let mut all = true;
                for (resource, made) in RESOURCES.iter().zip(&made) {
                    let changes = served
                        .resource_document(WORKSPACE, APP, resource)
                        .await
                        .ok()
                        .flatten()
                        .map(|doc| {
                            AutoCommit::load(&doc)
                                .unwrap()
                                .get_changes(&[])
                                .into_iter()
                                .map(|change| change.raw_bytes().to_vec())
                                .collect::<std::collections::BTreeSet<_>>()
                        });
                    all &= changes.as_ref() == Some(made);
                }
                if all {
                    return;
                }
                // A write missed while this read is in the copy: read again.
                assert!(
                    writes.recv().await != Err(tokio::sync::broadcast::error::RecvError::Closed),
                    "the service stays up"
                );
            }
        },
    )
    .await;

    let (later_key, later_cert) = device(&root, 0x85);
    let mut later = Desktop::start(directory.path(), "later", later_key, later_cert).await;
    later.enters_as_member(a2.own_address().await).await;
    later.takes_part().await;
    until(
        &[&later],
        "the desktop handed the workspace serves all of it",
        has_all,
    )
    .await;
    later.leaves().await;

    // From its files alone, after a restart, it serves all of it still.
    a2.leaves().await;
    let a2 = Desktop::start(directory.path(), "a2", a2_key, a2_cert).await;
    let (after_key, after_cert) = device(&root, 0x86);
    let mut after = Desktop::start(directory.path(), "after", after_key, after_cert).await;
    after.enters_as_member(a2.own_address().await).await;
    after.takes_part().await;
    until(&[&after], "the restarted desktop serves all of it", has_all).await;
    after.leaves().await;
    a2.leaves().await;
}

/// A holder that hears why its hold dials no holder of the workspace, and
/// nothing else.
struct Causes {
    reach: Reach,
    heard: Arc<(std::sync::Mutex<Vec<CannotDial>>, tokio::sync::Notify)>,
}

impl Holder for Causes {
    fn reach(&self) -> Reach {
        self.reach.clone()
    }

    fn dial_failed(
        &self,
        _via: TransportHint,
        reason: tilo_server_protocol::refusal::Refusal,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        panic!("the hold dialled a holder: {reason}");
    }

    fn cannot_dial(
        &self,
        cause: CannotDial,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        self.heard.0.lock().unwrap().push(cause);
        self.heard.1.notify_one();
        Box::pin(std::future::ready(()))
    }

    fn run(
        &self,
        _linked: Linked,
        _scope: Arc<Scope>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        panic!("the hold linked to a holder");
    }
}

/// Why the hold of `desktop`'s workspace, as `identity`, with its own
/// service running or not (`own_host`), dials none of its holders: the first
/// cause it reports, and the one it reports again once retried.
async fn why_no_dial(
    desktop: &Desktop,
    identity: Option<DeviceIdentity>,
    own_host: bool,
) -> (CannotDial, CannotDial) {
    let heard = Arc::new((
        std::sync::Mutex::new(Vec::new()),
        tokio::sync::Notify::new(),
    ));
    let hearing = Arc::clone(&heard);
    let holder = Causes {
        reach: Reach {
            endpoint: desktop.endpoint.clone(),
            own_host: own_host.then(|| Arc::clone(&desktop.service) as Arc<dyn OwnHost>),
        },
        heard,
    };
    let connect = Connect::new(desktop.owner.clone(), workspace()).expect("enrolled here");
    let link = Arc::new(Link::default());
    let holding = tokio::spawn({
        let link = Arc::clone(&link);
        async move {
            tilo_peer_state::owner::session::hold(connect, identity, &holder, &link).await;
        }
    });
    let said = |index: usize| {
        let hearing = Arc::clone(&hearing);
        async move {
            loop {
                let notified = hearing.1.notified();
                if let Some(cause) = hearing.0.lock().unwrap().get(index) {
                    return *cause;
                }
                notified.await;
            }
        }
    };
    let first = tokio::time::timeout(Duration::from_secs(30), said(0))
        .await
        .expect("the hold says why it dials nothing");
    link.retry();
    let again = tokio::time::timeout(Duration::from_secs(30), said(1))
        .await
        .expect("a retried hold says why again");
    holding.abort();
    (first, again)
}

async fn founded(directory: &Path) -> Desktop {
    let root = SigningKey::from_bytes(&[0x71; 32]);
    let (key, cert) = device(&root, 0x72);
    let desktop = Desktop::start(directory, "founder", key, cert).await;
    desktop.founds().await;
    desktop
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_with_no_identity_says_so_each_time_it_is_retried() {
    let directory = tempfile::tempdir().unwrap();
    let desktop = founded(directory.path()).await;
    let heard = why_no_dial(&desktop, None, true).await;
    drop(desktop);
    assert_eq!(heard, (CannotDial::NoIdentity, CannotDial::NoIdentity));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_on_this_device_whose_service_is_not_open_says_so() {
    let directory = tempfile::tempdir().unwrap();
    let desktop = founded(directory.path()).await;
    let heard = why_no_dial(&desktop, Some(desktop.identity()), false).await;
    drop(desktop);
    assert_eq!(
        heard,
        (CannotDial::OwnServiceStopped, CannotDial::OwnServiceStopped)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_the_workspace_does_not_take_and_cannot_renew_says_so() {
    let directory = tempfile::tempdir().unwrap();
    let desktop = founded(directory.path()).await;
    // Another device of another person holds this copy: its certificate is
    // not the one the workspace enrolled, and nothing renews it.
    let (key, cert) = device(&SigningKey::from_bytes(&[0x73; 32]), 0x74);
    let stranger = DeviceIdentity::kept(cert, key);
    let heard = why_no_dial(&desktop, Some(stranger), true).await;
    drop(desktop);
    assert_eq!(heard, (CannotDial::NotRenewed, CannotDial::NotRenewed));
}
