//! A member's change reaches the apps of the device it entered through.
//!
//! A device serves the members of every workspace it holds ([`Owning`]), and
//! what its owner takes in from them is its apps' to read. A workspace this
//! device hosts itself holds no link of it, so the change reached its copy and
//! stopped there: the member's edit showed nowhere on the device it entered
//! through. The same holds when this device reaches the workspace's host
//! elsewhere, where its link carries the change on to that host.
tilo_automerge_isolate::test_worker!();

use super::*;
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use std::path::Path;
use std::time::{Duration, SystemTime};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_desktop_model::WorkspaceId as Workspace;
use tilo_peer_state::StatePeer;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync, Owning};
use tilo_peer_state::join::invitations::IssueInvitationRequest;
use tilo_peer_state::join::{Role, TransportHint, encode_invitation_text};
use tilo_peer_state::join_answer::JoinAnswer;
use tilo_peer_state::join_transport::LocalJoinService;
use tilo_peer_state::owner::{NativeWorkspaceOwner, OwnerJoinWorkspaces, enroll};
use tilo_peer_state::store::{Appetite, Store};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

const HERE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const ELSEWHERE: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

/// How long a step of the test may take before it failed.
const PATIENCE: Duration = Duration::from_secs(30);

/// The device key `n` of the person `root`, with `capabilities`.
fn device_with(
    root: &SigningKey,
    n: u8,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{n}"),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).expect("sign the device's certificate");
    (key, cert)
}

/// Where `endpoint` is reached: its identity at the loopback port it bound.
fn addr(endpoint: &Endpoint) -> iroh::EndpointAddr {
    let port = endpoint
        .bound_sockets()
        .first()
        .expect("a loopback socket")
        .port();
    iroh::EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        port,
    )))
}

/// The hint `certificate`'s device answers joins to `workspace` at `endpoint`.
fn hint(
    workspace: Workspace,
    endpoint: &Endpoint,
    certificate: &DeviceCert,
    key: &SigningKey,
) -> TransportHint {
    WorkspaceDeviceHintV1 {
        version: 1,
        holds_keys: true,
        runs_work: false,
        workspace_id: workspace,
        endpoint_addr: addr(endpoint),
        user_public_key: certificate.user_public_key,
        device_certificate: certificate.clone(),
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(key)
    .expect("the host signs the hint its workspaces are reached at")
}

/// This device's owner, kept at `state` as a session keeps it.
fn owner_at(state: &Path) -> NativeWorkspaceOwner {
    tilo_peer_state::owner::open_file(&state.join("workspaces.json")).expect("this device's owner")
}

/// The invitation `administrator` issues into `workspace`, as the text a person
/// hands over.
async fn invitation(
    owner: &NativeWorkspaceOwner,
    workspace: Workspace,
    administrator: &DeviceCert,
    key: &SigningKey,
    transport: TransportHint,
    request_id: [u8; 32],
) -> String {
    let issued = owner
        .issue_invitation(
            workspace,
            administrator,
            &IssueInvitationRequest {
                request_id,
                role: Role::Editor,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                inviter_name: None,
            },
            &[transport],
            key,
            SystemTime::now(),
        )
        .await
        .expect("the workspace's device issues an invitation");
    encode_invitation_text(&issued.signed_invitation)
}

/// `owner`'s device, `cert` and `key`, redeems `invitation` into `workspace`,
/// answering it through `service`: what a device does with a link a person
/// hands it.
async fn joins(
    owner: &NativeWorkspaceOwner,
    cert: &DeviceCert,
    key: &SigningKey,
    workspace: Workspace,
    invitation: String,
    service: std::sync::Arc<dyn LocalJoinService>,
) {
    let identity = HostIdentity::kept(cert.clone(), key.clone());
    let enrollment = enroll::prepare_and_complete(
        tilo_peer_state::join_transport::WorkspaceJoinClient::local(
            service,
            key.verifying_key().to_bytes(),
        ),
        identity,
        workspace,
        None,
        invitation,
        owner.clone(),
    )
    .await
    .expect("the workspace's device admits this one");
    assert_eq!(enrollment.bootstrap().workspace_id, workspace);
    owner
        .install(&enrollment, SystemTime::now())
        .expect("the join installs here");
}

/// Commit `value` to the test record of `workspace` as `owner`'s device,
/// numbered from the durable view as a session's engine does (the record's
/// first write founds it empty).
fn commit_record(
    owner: &NativeWorkspaceOwner,
    workspace: Workspace,
    key: &SigningKey,
    value: &str,
) {
    let now = SystemTime::now();
    let message = owner
        .with_current(workspace, |view| {
            let mut peer = view.peer().clone();
            let publication = peer.publication_mut();
            let Some(bytes) = view.peer().document(APP, RECORD) else {
                return publication.publish_document(
                    APP,
                    RECORD,
                    AutoCommit::new().save(),
                    key,
                    now,
                );
            };
            let mut document = AutoCommit::load(&bytes).expect("a record of this workspace");
            document.set_actor(automerge::ActorId::from(
                key.verifying_key().to_bytes().to_vec(),
            ));
            let heads = document.get_heads();
            document.put(ROOT, "value", value).expect("a change");
            document.commit_edit("fixture", "value");
            publication.publish_incremental(APP, RECORD, document.save_after(&heads), key, now)
        })
        .expect("the write signs");
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a record's write is a document");
    };
    owner
        .commit_document(
            tilo_peer_state::owner_commit::OwnerCommitRequest {
                workspace: workspace.to_string(),
                operation: tilo_peer_state::owner_commit::CommitOperation::Document {
                    app: APP.into(),
                    record: RECORD.into(),
                    envelope,
                },
            },
            now,
        )
        .expect("the owner admits its own write");
}

/// Write `value` into the record of the workspace `peer` is linked for, as
/// `key`'s device.
fn edit(peer: &DirectSync, key: &SigningKey, value: &str) {
    let mut edit = match peer.read(|peer| peer.document(APP, RECORD)) {
        Some(record) => AutoCommit::load(&record).expect("a record of this workspace"),
        None => AutoCommit::new(),
    };
    edit.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = edit.get_heads();
    edit.put(ROOT, "value", value).expect("a change");
    edit.commit_edit("fixture", "value");
    let change = edit.save_after(&heads);
    peer.publish(|peer| {
        peer.publication_mut()
            .publish_incremental(APP, RECORD, change, key, SystemTime::now())
    })
    .expect("the write signs");
}

/// Wait until `done` holds of `sync`'s replica.
async fn until(sync: &DirectSync, step: &str, done: impl Fn(&StatePeer) -> bool + Send + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(PATIENCE, async {
        while !sync.read(&done) {
            changes.changed().await.expect("the replica changed");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{step}"));
}

/// Wait until this device's own replica of `workspace` holds the record `done`
/// accepts.
async fn until_held(
    owner: &NativeWorkspaceOwner,
    workspace: Workspace,
    step: &str,
    done: impl Fn(&StatePeer) -> bool + Send + Sync,
) {
    tokio::time::timeout(PATIENCE, async {
        loop {
            let held = owner
                .with_current(workspace, |view| Ok(done(view.peer())))
                .unwrap_or(false);
            if held {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{step}"));
}

/// The next event `wanted` accepts, within [`PATIENCE`]: `step` fails otherwise.
async fn event(
    events: &mut mpsc::Receiver<NetEvent>,
    step: &str,
    wanted: impl Fn(&NetEvent) -> bool + Send + Sync,
) -> NetEvent {
    tokio::time::timeout(PATIENCE, async {
        loop {
            let event = events
                .recv()
                .await
                .unwrap_or_else(|| panic!("{step}: the actor stopped"));
            if wanted(&event) {
                return event;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{step}"))
}

/// A member's change reaches the apps of the device it entered through, both
/// for a workspace that device hosts itself and for one whose host is another
/// device: the first holds no link of the workspace, which handed its apps
/// nothing.
#[expect(
    clippy::too_many_lines,
    reason = "one device serving a member in two workspaces, hosted here and elsewhere"
)]
#[tokio::test(flavor = "multi_thread")]
async fn a_members_change_reaches_the_apps_of_the_device_it_entered_through() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let hosting = DeviceCapabilities {
        can_act_as_host: true,
        ..DeviceCapabilities::primary()
    };
    let (host_key, host_cert) = device_with(&root, 2, hosting.clone());
    let (far_key, far_cert) = device_with(&root, 3, hosting);
    let (guest_key, guest_cert) = device_with(&root, 4, DeviceCapabilities::member());
    let now = SystemTime::now();
    let (host_endpoint, host_lookup) =
        crate::loopback_endpoint(Some(iroh::SecretKey::from_bytes(&host_key.to_bytes()))).await;
    let (far_endpoint, _) =
        crate::loopback_endpoint(Some(iroh::SecretKey::from_bytes(&far_key.to_bytes()))).await;
    let (guest_endpoint, _) =
        crate::loopback_endpoint(Some(iroh::SecretKey::from_bytes(&guest_key.to_bytes()))).await;
    let here = Workspace::parse(HERE).expect("a workspace id");
    let elsewhere = Workspace::parse(ELSEWHERE).expect("a workspace id");

    // This device founds a workspace, and holds another one its separate host
    // founded: the shapes a device's workspaces take.
    let host_state = tempfile::tempdir().expect("a state directory");
    let host = owner_at(host_state.path());
    let far_state = tempfile::tempdir().expect("a state directory");
    let far = owner_at(far_state.path());
    host.create_workspace(
        here,
        "Here",
        &host_cert,
        &host_key,
        Some(&hint(here, &host_endpoint, &host_cert, &host_key)),
        now,
    )
    .await
    .expect("the workspace is founded here");
    far.create_workspace(
        elsewhere,
        "Elsewhere",
        &far_cert,
        &far_key,
        Some(&hint(elsewhere, &far_endpoint, &far_cert, &far_key)),
        now,
    )
    .await
    .expect("the workspace is founded on the separate host");
    commit_record(&host, here, &host_key, "founded here");
    commit_record(&far, elsewhere, &far_key, "founded elsewhere");

    // The separate host serves its workspaces on the direct lane, as a device
    // holding workspaces does.
    let far_frames = tempfile::tempdir().expect("a frames directory");
    let far_store = std::sync::Arc::new(
        Store::open(far_frames.path(), Appetite::StoreAll).expect("the separate host's frames"),
    );
    let far_serving = Owning::new(far.clone(), far_key.clone(), far_store);
    let far_router = Router::builder(far_endpoint.clone())
        .accept(DIRECT_ALPN, far_serving.clone())
        .spawn();
    let far_service: std::sync::Arc<dyn LocalJoinService> = std::sync::Arc::new(JoinAnswer(
        OwnerJoinWorkspaces::new(far.clone(), far_key.clone()),
    ));
    let host_service: std::sync::Arc<dyn LocalJoinService> = std::sync::Arc::new(JoinAnswer(
        OwnerJoinWorkspaces::new(host.clone(), host_key.clone()),
    ));

    // A member of both workspaces joins them through their hosts, and this
    // device joins the one the separate host holds: the member's enrollment
    // carries this device, as one made after it joined does.
    let guest_state = tempfile::tempdir().expect("a state directory");
    let guest = owner_at(guest_state.path());
    joins(
        &guest,
        &guest_cert,
        &guest_key,
        here,
        invitation(
            &host,
            here,
            &host_cert,
            &host_key,
            hint(here, &host_endpoint, &host_cert, &host_key),
            [7; 32],
        )
        .await,
        std::sync::Arc::clone(&host_service),
    )
    .await;
    joins(
        &host,
        &host_cert,
        &host_key,
        elsewhere,
        invitation(
            &far,
            elsewhere,
            &far_cert,
            &far_key,
            hint(elsewhere, &far_endpoint, &far_cert, &far_key),
            [9; 32],
        )
        .await,
        std::sync::Arc::clone(&far_service),
    )
    .await;
    joins(
        &guest,
        &guest_cert,
        &guest_key,
        elsewhere,
        invitation(
            &far,
            elsewhere,
            &far_cert,
            &far_key,
            hint(elsewhere, &far_endpoint, &far_cert, &far_key),
            [8; 32],
        )
        .await,
        std::sync::Arc::clone(&far_service),
    )
    .await;

    // This device's session: it serves the members of the workspaces it holds,
    // takes part in their links, and its apps read both workspaces.
    let frames = tempfile::tempdir().expect("a frames directory");
    let store = std::sync::Arc::new(
        Store::open(frames.path(), Appetite::StoreAll).expect("this device's frames"),
    );
    let members = Owning::new(host.clone(), host_key.clone(), store);
    let (cmd, actor) = Actor::spawn_serving(
        host_endpoint.clone(),
        host_lookup,
        Some(HostIdentity::kept(host_cert.clone(), host_key.clone())),
        std::sync::Arc::new(Mutex::new(Vec::new())),
        tilo_endpoint_reachability::Reachability::default(),
        vec![(
            DIRECT_ALPN.to_vec(),
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(members.clone()),
        )],
    );
    let (on_event, mut events) = mpsc::channel(256);
    cmd.send(NetCmd::ServeMembers(members))
        .await
        .expect("the actor is running");
    for workspace in [here, elsewhere] {
        cmd.send(NetCmd::JoinWorkspace {
            ws: workspace,
            on_event: on_event.clone(),
        })
        .await
        .expect("the actor is running");
        cmd.send(NetCmd::NativeConnect(
            tilo_peer_state::owner::session::Connect::new(host.clone(), workspace)
                .expect("this device holds the workspace"),
        ))
        .await
        .expect("the actor is running");
    }
    // Its link to the separate host's workspace is up, with what that host
    // retains in hand.
    event(
        &mut events,
        "this device links the workspace of its separate host",
        |event| matches!(event, NetEvent::HostReady { ws, .. } if *ws == elsewhere),
    )
    .await;
    // The workspace this device hosts itself has no link: nothing of it pairs
    // with a host elsewhere.
    let opened = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = events.recv().await {
            if matches!(event, NetEvent::HostReady { ws, .. } if ws == here) {
                panic!("the workspace this device hosts is never dialled");
            }
        }
    })
    .await;
    assert!(
        opened.is_err(),
        "no host of the workspace this device hosts reports ready: {opened:?}"
    );

    // The member links to this device for the workspace it hosts itself, and
    // writes.
    let member_here = DirectSync::over(
        guest.replica(here).expect("the member holds the workspace"),
        guest_key.clone(),
    );
    let dialed_here = tokio::spawn({
        let (member, endpoint, host) = (
            member_here.clone(),
            guest_endpoint.clone(),
            addr(&host_endpoint),
        );
        async move { member.connect(&endpoint, host).await }
    });
    until(&member_here, "the member catches up", |peer| {
        peer.document(APP, RECORD).is_some()
    })
    .await;
    edit(&member_here, &guest_key, "from the member");
    let written = member_here.read(|peer| peer.document_heads(APP, RECORD));
    assert!(written.is_some());
    // This device took the member's change into its own copy…
    until_held(
        &host,
        here,
        "this device took the member's change in",
        |peer| peer.document_heads(APP, RECORD) == written,
    )
    .await;
    // …and its apps read it, as what it wrote itself.
    let NetEvent::NativeInput(input) = event(
        &mut events,
        "this device's apps read the member's change",
        |event| matches!(event, NetEvent::NativeInput(input) if input.workspace() == here && input.author() == guest_key.verifying_key().to_bytes() && matches!(input.message(), NeighbourMsg::AppDocument { .. })),
    )
    .await
    else {
        panic!("a member's change reaches this device's apps as its input");
    };
    assert!(
        matches!(input.message(), NeighbourMsg::AppDocument { .. }),
        "the member's document write is what its apps read: {:?}",
        input.message()
    );

    // The same, in the workspace the separate host holds: this device's link
    // carries the member's change on to that host, and its apps read it too.
    let member_elsewhere = DirectSync::over(
        guest
            .replica(elsewhere)
            .expect("the member holds the workspace"),
        guest_key.clone(),
    );
    let dialed_elsewhere = tokio::spawn({
        let (member, endpoint, host) = (
            member_elsewhere.clone(),
            guest_endpoint.clone(),
            addr(&host_endpoint),
        );
        async move { member.connect(&endpoint, host).await }
    });
    until(
        &member_elsewhere,
        "the member catches up elsewhere",
        |peer| peer.document(APP, RECORD).is_some(),
    )
    .await;
    edit(&member_elsewhere, &guest_key, "from the member elsewhere");
    let written = member_elsewhere.read(|peer| peer.document_heads(APP, RECORD));
    assert!(written.is_some());
    let NetEvent::NativeInput(input) = event(
        &mut events,
        "this device's apps read the member's change elsewhere",
        |event| matches!(event, NetEvent::NativeInput(input) if input.workspace() == elsewhere && input.author() == guest_key.verifying_key().to_bytes() && matches!(input.message(), NeighbourMsg::AppDocument { .. })),
    )
    .await
    else {
        panic!("a member's change reaches this device's apps as its input");
    };
    assert!(matches!(input.message(), NeighbourMsg::AppDocument { .. }));
    // The separate host holds the member's change, sent on over this device's
    // link.
    until_held(
        &far,
        elsewhere,
        "the separate host holds the member's change",
        |peer| peer.document_heads(APP, RECORD) == written,
    )
    .await;

    dialed_here.abort();
    dialed_elsewhere.abort();
    actor.abort();
    far_router.shutdown().await.expect("the router stops");
    host_endpoint.close().await;
    far_endpoint.close().await;
    guest_endpoint.close().await;
}
