//! The host serves its workspaces on the same direct link a device runs: a
//! member syncs a document with it, a late joiner catches up from the history
//! the host retains, and a device the workspace's root revoked is refused.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
tilo_automerge_isolate::test_worker!();

mod common;

use common::{addr, device, localhost_endpoint};

use automerge::{AutoCommit, ROOT, transaction::Transactable};
use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::CommitNamed as _;
use collab_types::{
    ChannelId, ContentRetention, Ed25519PublicKey, MlsAccessRequestV1, OpKind, SignedOp,
    document_artifact_id, document_attestation_id, document_channel_id, sign_op,
};
use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use neighbour_wire::{NeighbourMsg, peer_handle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync, Serving};
use tilo_peer_state::store::{Appetite, Budget, Store};
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_workspace};
use tilo_user_identity::{
    DeviceCapabilities, RevocationEntry, RevocationList, RevocationReason, sign_revocation_entry,
};
use web_time::SystemTime;

const WORKSPACE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

fn workspace_number() -> u64 {
    tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap()
}

/// Write `value` into the workspace's record as `key`'s device, making the record
/// when the replica holds none yet.
fn edit(peer: &DirectSync, key: &SigningKey, value: &str) {
    let mut edit = match peer.read(|peer| peer.document(APP, RECORD)) {
        Some(record) => AutoCommit::load(&record).unwrap(),
        None => AutoCommit::new(),
    };
    edit.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = edit.get_heads();
    edit.put(ROOT, "value", value).unwrap();
    edit.commit_edit("fixture", "value");
    let change = edit.save_after(&heads);
    peer.publish(|peer, key, now| {
        peer.publication_mut()
            .publish_incremental(APP, RECORD, change, key, now)
    })
    .unwrap();
}

/// Wait until `done` holds of `sync`'s replica.
async fn until(sync: &DirectSync, step: &str, done: impl Fn(&StatePeer) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !sync.read(&done) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the replicas converge: {step}"));
}

/// One edit of `key`, sealed under the record's content keys: what a store
/// keeps and only a key holder reads.
fn sealed(key: &SigningKey, keys: &ChannelContentKeys, text: &str) -> NeighbourMsg {
    sealed_at(key, keys, text, web_time::SystemTime::now(), 1)
}

/// [`sealed`], written at `when` as the author's `seq`th op.
fn sealed_at(
    key: &SigningKey,
    keys: &ChannelContentKeys,
    text: &str,
    when: web_time::SystemTime,
    seq: u64,
) -> NeighbourMsg {
    // The plaintext a sealed edit carries: an incremental of the record.
    let mut edit = AutoCommit::new();
    edit.put(ROOT, "value", text).unwrap();
    edit.commit_edit("fixture", "value");
    let plaintext = tilo_peer_state::DocumentMutation::Incremental(edit.save_incremental())
        .encode()
        .unwrap();
    let (metadata, payload) = keys
        .seal_content(
            document_artifact_id(APP, WORKSPACE, RECORD),
            ContentRetention::Delta,
            &plaintext,
        )
        .unwrap();
    let mut op = SignedOp::new_unsigned(
        payload,
        document_artifact_id(APP, WORKSPACE, RECORD),
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        document_attestation_id(APP, WORKSPACE, RECORD),
        OpKind::Change,
        when,
        seq,
    );
    op.encryption = Some(metadata);
    sign_op(&mut op, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(&key.verifying_key().to_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        record_id: RECORD.into(),
        envelope: Box::new(op),
    }
}

fn files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, found);
        } else {
            found.push(path);
        }
    }
}

fn channel() -> ChannelId {
    document_channel_id(APP, WORKSPACE, RECORD)
}

#[expect(
    clippy::too_many_lines,
    reason = "one member's sync with the host, then a late joiner catching up from it"
)]
#[tokio::test(flavor = "multi_thread")]
async fn a_member_syncs_with_the_host_and_a_late_joiner_catches_up_from_its_store() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (host_key, host_cert) = common::device_with(&root, 2, DeviceCapabilities::host());
    let (a_key, a_cert) = device(&root, 3);
    let (b_key, b_cert) = device(&root, 4);
    let (c_key, c_cert) = device(&root, 5);
    let now = SystemTime::now();
    let devices = vec![
        host_cert.clone(),
        a_cert.clone(),
        b_cert.clone(),
        c_cert.clone(),
    ];

    let mut govern = StatePeer::new(
        WORKSPACE,
        host_cert.clone(),
        devices.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    govern.select_admission_v5().unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "from the host").unwrap();
    document.commit_edit("fixture", "value");
    govern
        .install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &host_cert, &devices).unwrap(),
                documents: vec![SeedDocument {
                    app_id: APP.into(),
                    record_id: RECORD.into(),
                    kind: "test".into(),
                    bytes: document.save(),
                }],
            },
            &host_key,
            now,
        )
        .unwrap();
    // The workspace's root revoked C's device: C is in the workspace's roster
    // and may not reach it.
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: c_key.verifying_key().to_bytes(),
        revoked_at_ns: 1,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    assert!(
        govern
            .governance_mut()
            .publish_governed_revocations(&[entry], &host_key, now)
            .unwrap()
    );
    let scratch = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(scratch.path(), Appetite::StoreAll).unwrap());
    let host = DirectSync::over(
        tilo_peer_state::direct::Replica::new(govern),
        host_key.clone(),
    )
    .with_store(Arc::clone(&store));
    let serving = Serving::new();
    serving.serve(host.clone()).unwrap();
    let host_endpoint = localhost_endpoint(&host_key).await;
    let router = Router::builder(host_endpoint.clone())
        .accept(DIRECT_ALPN, serving.clone())
        .spawn();

    // A member device dials the host and syncs the workspace with it.
    let a = StatePeer::from_join(WORKSPACE, host_cert.clone(), devices.clone(), &[], now).unwrap();
    let a = DirectSync::new(a, a_key.clone());
    let a_endpoint = localhost_endpoint(&a_key).await;
    let a_link = tokio::spawn({
        let a = a.clone();
        let a_endpoint = a_endpoint.clone();
        let host_addr = addr(&host_endpoint);
        async move { a.connect(&a_endpoint, host_addr).await }
    });
    let host_rows = host.read(StatePeer::rows);
    until(&a, "a has the host's rows", |a| a.rows() == host_rows).await;

    // What the member writes, the host takes in.
    edit(&a, &a_key, "from the member");
    let edited = a.read(|a| a.document_heads(APP, RECORD));
    until(&host, "the host has the member's edit", |host| {
        host.document_heads(APP, RECORD) == edited
    })
    .await;

    // A sealed edit reaches the host, which keeps it as the history a member
    // that was away catches up from; the host holds no channel key.
    let keys = ChannelGroup::create(channel(), MemberKey::generate("owner").unwrap())
        .unwrap()
        .content_keys()
        .unwrap();
    let kept = sealed(&a_key, &keys, "member history");
    a.publish(|_, _, _| Ok(kept.clone())).unwrap();
    let retained = || {
        store
            .replay(workspace_number(), &[], Budget::default())
            .unwrap()
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        while retained().frames.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the host retains the member's sealed edit");
    assert_eq!(retained().frames, vec![kept.clone()]);
    // What the host wrote is ciphertext: neither the edit's plaintext nor a
    // readable copy of it is on disk, one file per frame.
    let mut kept_files = Vec::new();
    files(scratch.path(), &mut kept_files);
    assert_eq!(kept_files.len(), 1, "a file per frame");
    for file in &kept_files {
        let bytes = std::fs::read(file).unwrap();
        assert!(
            !bytes
                .windows(b"member history".len())
                .any(|window| window == b"member history"),
            "{} carries plaintext",
            file.display()
        );
    }
    assert!(
        host.read(|host| host.document(APP, RECORD)).is_some(),
        "the host holds the workspace's record"
    );

    // The member goes away; a late joiner of the workspace dials the host
    // alone and catches up from what the host kept.
    a_link.abort();
    let b = StatePeer::from_join(WORKSPACE, host_cert.clone(), devices.clone(), &[], now).unwrap();
    assert!(b.records().is_empty());
    let b = DirectSync::new(b, b_key.clone());
    let b_endpoint = localhost_endpoint(&b_key).await;
    let b_link = tokio::spawn({
        let b = b.clone();
        let b_endpoint = b_endpoint.clone();
        let host_addr = addr(&host_endpoint);
        async move { b.connect(&b_endpoint, host_addr).await }
    });
    let host_rows = host.read(StatePeer::rows);
    until(&b, "b has the rows", |b| b.rows() == host_rows).await;
    until(&b, "b has the edit", |b| {
        b.document_heads(APP, RECORD) == edited
    })
    .await;
    assert!(
        store
            .replay(workspace_number(), &b.cursors(), Budget::default())
            .unwrap()
            .frames
            .is_empty(),
        "a caught-up member is handed nothing"
    );

    // A revoked device is refused, and its replica gains nothing.
    let c = StatePeer::from_join(WORKSPACE, host_cert.clone(), devices.clone(), &[], now).unwrap();
    let c = DirectSync::new(c, c_key.clone());
    let c_endpoint = localhost_endpoint(&c_key).await;
    let refused = c.connect(&c_endpoint, addr(&host_endpoint)).await;
    assert!(refused.is_err(), "a revoked device is refused");
    assert!(c.read(|c| c.rows().is_empty()));

    b_link.abort();
    router.shutdown().await.unwrap();
    host_endpoint.close().await;
    a_endpoint.close().await;
    b_endpoint.close().await;
    c_endpoint.close().await;
}

/// This device's owner, kept at `state` as a session keeps it.
fn owner_at(state: &Path) -> tilo_peer_state::owner::NativeWorkspaceOwner {
    tilo_peer_state::owner::open_file(&state.join("workspaces.json")).expect("this device's owner")
}

/// The hint that `certificate`'s device answers `workspace`'s joins at
/// `endpoint`: what a link hands out so whoever holds it knows where to go.
fn hint(
    workspace: tilo_desktop_model::WorkspaceId,
    endpoint: &iroh::Endpoint,
    certificate: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
) -> tilo_peer_state::join::TransportHint {
    use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
    let room = workspace.to_string();
    IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        workspace_id: tilo_peer_state::join::room_workspace_id(&room).unwrap(),
        room,
        endpoint_addr: addr(endpoint),
        host_user_public_key: certificate.user_public_key,
        host_device_certificate: certificate.clone(),
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(key)
    .unwrap()
}

/// The invitation `administrator` issues into `workspace`, as the text a person
/// hands over.
fn invitation(
    owner: &tilo_peer_state::owner::NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
    administrator: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
    transport: tilo_peer_state::join::TransportHint,
    request_id: [u8; 32],
) -> String {
    let issued = owner
        .issue_invitation(
            workspace,
            administrator,
            tilo_peer_state::join::invitations::IssueInvitationRequest {
                request_id,
                role: tilo_peer_state::join::MemberRole::Editor,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                inviter_name: None,
            },
            vec![transport],
            key,
            SystemTime::now(),
        )
        .expect("the workspace's device issues an invitation");
    tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation)
}

/// A device that redeems `invitation` into a workspace it takes in, answering
/// it through `service`: what a device does with a link a person hands it.
async fn joins(
    state: &Path,
    cert: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
    invitation: String,
    service: std::sync::Arc<dyn tilo_peer_state::join_transport::LocalJoinService>,
) -> (
    tilo_peer_state::owner::NativeWorkspaceOwner,
    tilo_desktop_model::WorkspaceId,
) {
    use tilo_identity_host::custody::HostIdentity;
    let owner = owner_at(state);
    let identity = HostIdentity::kept(cert.clone(), key.clone());
    let topic = tilo_peer_state::join::room_workspace_id(WORKSPACE).expect("a workspace topic");
    let enrollment = tilo_peer_state::owner::enroll::prepare_and_complete(
        tilo_peer_state::join_transport::WorkspaceJoinClient::local(
            service,
            key.verifying_key().to_bytes(),
        ),
        identity,
        topic,
        None,
        invitation,
        owner.clone(),
    )
    .await
    .expect("the workspace's device admits this one");
    let workspace = tilo_desktop_model::WorkspaceId::parse(&enrollment.bootstrap().room)
        .expect("a workspace id");
    owner
        .install(&enrollment, SystemTime::now())
        .expect("the join installs here");
    (owner, workspace)
}

/// A device serves the workspace its engine holds: another device joins by
/// invitation, syncs a document with it, and a device that joins later catches
/// up from what the serving device kept.
#[expect(
    clippy::too_many_lines,
    reason = "a join, a sync either way, a late joiner catching up and a workspace left unserved"
)]
#[tokio::test(flavor = "multi_thread")]
async fn a_device_serves_the_workspace_its_engine_holds() {
    use tilo_peer_state::direct::Owning;
    use tilo_peer_state::join_answer::JoinAnswer;
    use tilo_peer_state::owner::OwnerJoinWorkspaces;

    let root = SigningKey::from_bytes(&[1; 32]);
    // A device that holds the workspaces and admits the devices joining them: an always-on
    // device of a person's fleet.
    let (host_key, host_cert) = common::device_with(
        &root,
        2,
        DeviceCapabilities {
            can_act_as_host: true,
            ..DeviceCapabilities::primary()
        },
    );
    let (b_key, b_cert) = device(&root, 3);
    let (c_key, c_cert) = device(&root, 4);
    let now = SystemTime::now();
    let host_endpoint = localhost_endpoint(&host_key).await;
    let host_state = tempfile::tempdir().unwrap();
    let host = owner_at(host_state.path());
    let workspace = tilo_desktop_model::WorkspaceId::parse(WORKSPACE).unwrap();
    let route = hint(workspace, &host_endpoint, &host_cert, &host_key);
    host.create_workspace(
        workspace,
        "Hosted",
        &host_cert,
        &host_key,
        Some(route.clone()),
        now,
    )
    .expect("the workspace is founded here");
    // Both other devices join through this device's own join exchange.
    let service: Arc<dyn tilo_peer_state::join_transport::LocalJoinService> = Arc::new(JoinAnswer(
        OwnerJoinWorkspaces::new(host.clone(), host_key.clone()),
    ));
    let frames = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(frames.path(), Appetite::StoreAll).unwrap());
    let serving = Owning::new(host.clone(), host_key.clone(), Arc::clone(&store));
    // The device's session hears what its members bring, in the workspace they write to: its own
    // apps and channels read it, and it sends it on to the workspace's own host. A member's write
    // and its channel governance went nowhere past this device's links.
    let (tell, told) = std::sync::mpsc::channel();
    let tell = std::sync::Mutex::new(tell);
    serving.hear(Arc::new(
        move |workspace, heard: &tilo_peer_state::direct::Heard| {
            let _ = tell
                .lock()
                .unwrap()
                .send((workspace, heard.author, heard.message.clone()));
        },
    ));
    let heard_here = |wanted: &dyn Fn(&NeighbourMsg) -> bool| loop {
        let (in_workspace, author, message) = told
            .recv_timeout(Duration::from_secs(30))
            .expect("the device's session hears what its members bring");
        if wanted(&message) {
            return (in_workspace, author, message);
        }
    };
    let router = Router::builder(host_endpoint.clone())
        .accept(DIRECT_ALPN, serving.clone())
        .spawn();
    // What the device holds of the workspace before anyone asks for it: a record the joiner has to
    // catch up on.
    let message = host
        .with_current(workspace, |view| {
            view.peer().clone().publication_mut().publish_document(
                APP,
                RECORD,
                AutoCommit::new().save(),
                &host_key,
                now,
            )
        })
        .expect("the record's first write signs");
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a record's first write is a document");
    };
    host.commit_document(
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
    .expect("the owner admits the record's first write as it is");
    let held = DirectSync::over(host.replica(workspace).unwrap(), host_key.clone())
        .owning(host.clone(), workspace)
        .unwrap();
    assert!(held.read(|peer| peer.document(APP, RECORD)).is_some());

    let b_state = tempfile::tempdir().unwrap();
    let (b, joined) = joins(
        b_state.path(),
        &b_cert,
        &b_key,
        invitation(
            &host,
            workspace,
            &host_cert,
            &host_key,
            route.clone(),
            [7; 32],
        ),
        Arc::clone(&service),
    )
    .await;
    assert_eq!(joined, workspace);
    let b = DirectSync::over(b.replica(workspace).unwrap(), b_key.clone());
    let b_endpoint = localhost_endpoint(&b_key).await;
    let b_link = tokio::spawn({
        let b = b.clone();
        let b_endpoint = b_endpoint.clone();
        let host_addr = addr(&host_endpoint);
        async move { b.connect(&b_endpoint, host_addr).await }
    });
    until(&b, "the joiner has the device's record", |peer| {
        peer.document(APP, RECORD).is_some()
    })
    .await;

    // What the joiner writes lands in the serving device's own workspace.
    edit(&b, &b_key, "from the joiner");
    let written = b.read(|peer| peer.document_heads(APP, RECORD));
    assert!(written.is_some());
    tokio::time::timeout(Duration::from_secs(30), async {
        while held.read(|peer| peer.document_heads(APP, RECORD)) != written {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the device that holds the workspace admits the joiner's write");
    let (in_workspace, author, _) =
        heard_here(&|message| matches!(message, NeighbourMsg::AppDocument { .. }));
    assert_eq!(
        (in_workspace, author),
        (workspace, b_key.verifying_key().to_bytes())
    );

    // What the device writes, or hears of the workspace another way, it sends to its members, and
    // keeps for the ones that link later: a member that linked after the device wrote never heard
    // of the write.
    let keys = ChannelGroup::create(channel(), MemberKey::generate("owner").unwrap())
        .unwrap()
        .content_keys()
        .unwrap();
    let own = sealed(&host_key, &keys, "the device's own");
    serving.carry(workspace, host_key.verifying_key().to_bytes(), own.clone());
    assert!(
        store
            .replay(
                tilo_peer_state::join::room_workspace_id(WORKSPACE).unwrap(),
                &[],
                Budget::default()
            )
            .unwrap()
            .frames
            .contains(&own),
        "what the device sends its members is kept for the ones that link later"
    );

    // A device that joins later catches up from the device that holds the workspace: what it wrote,
    // and what the joiner wrote through it.
    let c_state = tempfile::tempdir().unwrap();
    let (c, _) = joins(
        c_state.path(),
        &c_cert,
        &c_key,
        invitation(&host, workspace, &host_cert, &host_key, route, [8; 32]),
        Arc::clone(&service),
    )
    .await;
    let c = DirectSync::over(c.replica(workspace).unwrap(), c_key.clone());
    let c_endpoint = localhost_endpoint(&c_key).await;
    let c_link = tokio::spawn({
        let c = c.clone();
        let c_endpoint = c_endpoint.clone();
        let host_addr = addr(&host_endpoint);
        async move { c.connect(&c_endpoint, host_addr).await }
    });
    until(&c, "the late joiner has the joiner's edit", |peer| {
        peer.document_heads(APP, RECORD) == written
    })
    .await;

    // A channel's governance one member sends reaches the others through the device that holds
    // the workspace, which admits none of it into the workspace: a request to join a record's
    // channel that only its sender heard left each member founding the channel's group alone.
    let mut c_heard = c.heard();
    let asking = NeighbourMsg::MlsAccessRequest {
        workspace: WORKSPACE.into(),
        request: Box::new(MlsAccessRequestV1::signed(channel(), vec![1], 1, &b_key).unwrap()),
        delivery: 0,
    };
    b.send(asking.clone()).unwrap();
    let heard = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match c_heard.recv().await {
                Ok(heard) if heard.message == asking => return heard,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => panic!("the late joiner's link closed: {error}"),
            }
        }
    })
    .await
    .expect("the joiner's channel governance reaches the late joiner");
    assert_eq!(heard.author, b_key.verifying_key().to_bytes());
    assert!(
        !heard.landed,
        "channel governance is never the workspace's state"
    );
    let (_, _, taken) = heard_here(&|message| *message == asking);
    assert_eq!(
        taken, asking,
        "the device's own channels hear the joiner's governance"
    );
    let genesis = NeighbourMsg::MlsGovernance {
        workspace: WORKSPACE.into(),
        frame: Box::new(
            collab_types::MlsGovernanceFrame::signed(
                channel(),
                [0; 32],
                0,
                collab_types::MlsGovernanceAction::Genesis,
                &host_key,
            )
            .unwrap(),
        ),
        delivery: 1,
    };
    serving.carry_governance(&genesis);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match c_heard.recv().await {
                Ok(heard) if heard.message == genesis => return,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => panic!("the late joiner's link closed: {error}"),
            }
        }
    })
    .await
    .expect("what the device holding the workspace says of its channels reaches its members");

    // A workspace this device does not hold is refused, rather than answered for.
    let elsewhere =
        tilo_desktop_model::WorkspaceId::parse("dddddddd-dddd-4ddd-8ddd-dddddddddddd").unwrap();
    let stranger = StatePeer::new(
        &elsewhere.to_string(),
        c_cert.clone(),
        vec![c_cert.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let refused = DirectSync::new(stranger, c_key.clone())
        .connect(&c_endpoint, addr(&host_endpoint))
        .await;
    assert!(
        refused.is_err(),
        "a device answers no workspace it does not hold: {refused:?}"
    );

    b_link.abort();
    c_link.abort();
    router.shutdown().await.unwrap();
    host_endpoint.close().await;
    b_endpoint.close().await;
    c_endpoint.close().await;
}

/// What the host wrote long before a member links reaches the member as history, however old:
/// the host's catch-up travels with the history the member asks for, never as a late live
/// frame. Judged live, a sealed edit written more than the live clock-skew window ago was
/// refused, and the refusal filed back to its author ("will not take your change" on
/// e2e-branch's box, after both boxes rebooted).
#[tokio::test(flavor = "multi_thread")]
async fn what_the_host_wrote_long_ago_reaches_a_member_that_links_later() {
    let root = SigningKey::from_bytes(&[2; 32]);
    let (host_key, host_cert) = common::device_with(&root, 6, DeviceCapabilities::host());
    let (a_key, a_cert) = device(&root, 7);
    let now = SystemTime::now();
    let devices = vec![host_cert.clone(), a_cert.clone()];
    let mut govern = StatePeer::new(
        WORKSPACE,
        host_cert.clone(),
        devices.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    govern.select_admission_v5().unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "from the host").unwrap();
    document.commit_edit("fixture", "value");
    govern
        .install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &host_cert, &devices).unwrap(),
                documents: vec![SeedDocument {
                    app_id: APP.into(),
                    record_id: RECORD.into(),
                    kind: "test".into(),
                    bytes: document.save(),
                }],
            },
            &host_key,
            now,
        )
        .unwrap();
    let host = DirectSync::new(govern, host_key.clone());
    let keys = ChannelGroup::create(channel(), MemberKey::generate("owner").unwrap())
        .unwrap()
        .content_keys()
        .unwrap();
    // Ten minutes before the member links: twice the live window.
    let old = sealed_at(
        &host_key,
        &keys,
        "written long ago",
        now - Duration::from_secs(600),
        1_000,
    );
    // The host holds it as one of its own sealed publications, which its catch-up carries.
    let host_device = host_key.verifying_key().to_bytes();
    host.publish(|peer, _, now| {
        peer.receive_from_report(
            tilo_peer_state::TransportOrigin::RetainedRelayDelta(host_device),
            old.clone(),
            now,
        )
        .map_err(|error| format!("{error:?}"))?;
        Ok(old.clone())
    })
    .unwrap();

    let host_endpoint = localhost_endpoint(&host_key).await;
    let router = Router::builder(host_endpoint.clone())
        .accept(DIRECT_ALPN, host.clone())
        .spawn();
    let a = StatePeer::from_join(WORKSPACE, host_cert.clone(), devices, &[], now).unwrap();
    let a = DirectSync::new(a, a_key.clone());
    let mut heard = a.heard();
    let a_endpoint = localhost_endpoint(&a_key).await;
    let link = tokio::spawn({
        let a = a.clone();
        let a_endpoint = a_endpoint.clone();
        let host_addr = addr(&host_endpoint);
        async move { a.connect(&a_endpoint, host_addr).await }
    });
    let taken = tokio::time::timeout(Duration::from_secs(30), async {
        let mut refused = Vec::new();
        loop {
            match heard.recv().await {
                Ok(frame) if frame.message == old => match frame.report {
                    Ok(_) => return Ok(()),
                    Err(reason) => refused.push(reason),
                },
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => return Err(format!("the link closed: {error}; refused {refused:?}")),
            }
        }
    })
    .await;
    assert!(
        matches!(taken, Ok(Ok(()))),
        "the member takes in the host's old write as history: {taken:?}"
    );

    link.abort();
    router.shutdown().await.unwrap();
    host_endpoint.close().await;
    a_endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_workspace_ends_its_links_and_refuses_the_next_dial_as_deleted() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let (host_key, host_cert) = common::device_with(&root, 2, DeviceCapabilities::host());
    let (a_key, a_cert) = device(&root, 3);
    let now = SystemTime::now();
    let devices = vec![host_cert.clone(), a_cert];
    let mut govern = StatePeer::new(
        WORKSPACE,
        host_cert.clone(),
        devices.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    govern.select_admission_v5().unwrap();
    govern
        .install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &host_cert, &devices).unwrap(),
                documents: Vec::new(),
            },
            &host_key,
            now,
        )
        .unwrap();
    let host = DirectSync::over(
        tilo_peer_state::direct::Replica::new(govern),
        host_key.clone(),
    );
    let serving = Serving::new();
    serving.serve(host.clone()).unwrap();
    let host_endpoint = localhost_endpoint(&host_key).await;
    let _router = Router::builder(host_endpoint.clone())
        .accept(DIRECT_ALPN, serving.clone())
        .spawn();
    let a = StatePeer::from_join(WORKSPACE, host_cert, devices, &[], now).unwrap();
    let a = DirectSync::new(a, a_key.clone());
    let a_endpoint = localhost_endpoint(&a_key).await;
    let a_link = tokio::spawn({
        let (a, a_endpoint, host_addr) = (a.clone(), a_endpoint.clone(), addr(&host_endpoint));
        async move { a.connect(&a_endpoint, host_addr).await }
    });
    let host_rows = host.read(StatePeer::rows);
    until(&a, "a has the host's rows", |a| a.rows() == host_rows).await;

    // The workspace's owner deletes it: the member's live link ends, saying so.
    serving.delete(workspace_number());
    let ended = tokio::time::timeout(Duration::from_secs(30), a_link)
        .await
        .expect("the member's link ends")
        .unwrap();
    assert_eq!(
        ended,
        Err(tilo_relay_protocol::refusal::WORKSPACE_DELETED.to_owned())
    );
    // A member that was away dials it again, and hears the same.
    let again = tokio::time::timeout(
        Duration::from_secs(30),
        a.connect(&a_endpoint, addr(&host_endpoint)),
    )
    .await
    .expect("the dial is answered");
    assert_eq!(
        again,
        Err(tilo_relay_protocol::refusal::WORKSPACE_DELETED.to_owned())
    );
    assert!(serving.link(workspace_number()).is_none());
}
