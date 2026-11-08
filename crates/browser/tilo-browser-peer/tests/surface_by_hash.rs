//! A page that holds a record's history written by an app version it does not
//! run asks its workspace for that version's surface by hash, on the same
//! content plane it asks for a picture with, keeps it, and decides what waited
//! for it. The hold names the surface as the reason it waits, so a reader is
//! told what a record wants instead of being told it wants its keys.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; one test is one transcript between a native participant and a page"
)]

mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::SystemTime;

use automerge::transaction::Transactable as _;
use channel_crypto::{ChannelPeer, VaultKey};
use collab_types::{
    ContentRetention, DocumentMutation, Ed25519PublicKey, MlsGovernanceFrame, OpKind, SignedOp,
    document_artifact_id, document_attestation_id, document_channel_id,
};
use neighbour_wire::NeighbourMsg;
use tilo_browser_peer::{PeerEvent, PeerHandle};
use tilo_relay::workspaces::WorkspaceContent;
use tilo_relay::{Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList, UserIdentity};

use support::{
    Device, WORKSPACE, enrol, ready, route, start_host, start_peer, wait_for, workspace,
};

/// The surface the laptop's write names: the notes app's own `surface.toml`,
/// a version of it this page does not run.
fn older_surface() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../apps/notes/surface.toml"
    ))
    .unwrap()
}

/// A host that keeps app surfaces, as one that decided changes against them
/// does, answering only once this test lets it: the page's hold on the write,
/// and the request it makes for the surface, are the same while the host has
/// not answered.
struct Keeps {
    bytes: Vec<u8>,
    answers: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl Keeps {
    /// Let the host answer what it was asked.
    fn answer(&self) {
        let (open, answered) = &*self.answers;
        *open.lock().unwrap() = true;
        answered.notify_all();
    }
}

impl WorkspaceContent for Keeps {
    fn read(&self, _workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        let (open, answered) = &*self.answers;
        let mut open = open.lock().unwrap();
        while !*open {
            open = answered.wait(open).unwrap();
        }
        (app == "so.tilo.notes" && hash == blake3::hash(&self.bytes).to_hex().as_str())
            .then(|| self.bytes.clone())
    }
}

/// Publish `message` through the host the way a shell does.
fn publish(session: &RelayCoeditSession, message: &NeighbourMsg) {
    let (channel, retention) = neighbour_wire::retention(message, false).unwrap();
    let body = postcard::to_allocvec(message).unwrap();
    match retention {
        neighbour_wire::Retention::Checkpoint => session.publish_checkpoint(channel, body),
        neighbour_wire::Retention::Delta => session.publish_retained_delta(channel, body),
    }
    .unwrap();
}

/// What `peer` queued to publish, taken.
fn take(peer: &mut ChannelPeer, vault: &VaultKey) -> Vec<MlsGovernanceFrame> {
    let frames = peer.outbox().to_vec();
    peer.published(vault).unwrap();
    frames
}

fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}

/// The notes record's changes `events` opened, in the order they arrived.
fn pages(events: Vec<PeerEvent>) -> Vec<DocumentMutation> {
    events
        .into_iter()
        .filter_map(|event| match event {
            PeerEvent::Document(document) if document.route.app_id == "so.tilo.notes" => {
                document.mutation()
            }
            _ => None,
        })
        .collect()
}

/// Whether the page says it holds something for the note, and why.
fn held(peer: &PeerHandle) -> Option<String> {
    let note = support::route();
    let held = format!("held {}/{}:", note.app_id, note.record);
    peer.status()
        .channels
        .lines()
        .find(|line| line.starts_with(&held))
        .map(str::to_owned)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_fetches_the_surface_the_history_it_holds_was_written_with_and_decides_it() {
    // The page runs no app of its own: every change it receives names a
    // surface it must fetch, the one this test's host keeps.
    tilo_peer_state::decide_with_surfaces(tilo_browser_peer::surfaces::surfaces(), |_| None);

    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let surface = older_surface();
    let keeps = Arc::new(Keeps {
        bytes: surface.clone(),
        answers: Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
    });
    host.relay
        .workspaces()
        .unwrap()
        .serve_content(keeps.clone());

    // The laptop creates the workspace's key scope, announces itself and
    // writes the record's history, sealed and naming the other version's
    // surface.
    let note = route();
    let scope = collab_types::workspace_scope(WORKSPACE);
    let channel = document_channel_id(&note.app_id, &note.cluster_id, &note.record);
    let artifact = document_artifact_id(&note.app_id, &note.cluster_id, &note.record);
    let laptop_dir = tempfile::tempdir().unwrap();
    let mut laptop_peer = ChannelPeer::create_now(
        Some(laptop_dir.path()),
        scope,
        Box::new(laptop.key.clone()),
        &vault(),
    )
    .unwrap();
    let laptop_device = laptop.key.verifying_key().to_bytes();
    let session = connect(&host.relay, &root, &laptop).await;
    publish(
        &session,
        &NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(laptop.cert.clone()),
            revocations: Vec::new(),
        },
    );
    for frame in take(&mut laptop_peer, &vault()) {
        publish(
            &session,
            &NeighbourMsg::MlsGovernance {
                workspace: WORKSPACE.into(),
                frame: Box::new(frame),
                delivery: 1,
            },
        );
    }
    // The record's history: three page edits, each naming a field of the
    // notes app as its executor does, so every op carries the claim the
    // receiver decides against the app's surface, and each sealed with the
    // record's content key.
    let mut document = automerge::AutoCommit::new();
    let mut written = Vec::new();
    for page_number in 1..=3 {
        document
            .put(automerge::ROOT, "pages", format!("page {page_number}"))
            .unwrap();
        collab_types::CommitNamed::commit_edit(&mut document, "notes.notebook", "pages");
        let mutation = DocumentMutation::Incremental(document.save_incremental());
        let claims = collab_types::mutation_claims(&mutation, collab_types::record_unit).unwrap();
        let frame = mutation.encode().unwrap();
        let (metadata, sealed) = laptop_peer
            .content_keys()
            .unwrap()
            .for_unit(channel)
            .seal_content(artifact, ContentRetention::Delta, &frame)
            .unwrap();
        let mut op = SignedOp::new_unsigned_with_encryption(
            sealed,
            artifact,
            Ed25519PublicKey::from_bytes(laptop_device),
            document_attestation_id(&note.app_id, &note.cluster_id, &note.record),
            OpKind::Change,
            SystemTime::now(),
            page_number,
            metadata,
        )
        .with_claims(claims)
        .with_surface(Some(*blake3::hash(&surface).as_bytes()));
        collab_types::sign_op(&mut op, &laptop.key).unwrap();
        written.push(DocumentMutation::decode(&frame).unwrap());
        publish(
            &session,
            &NeighbourMsg::AppDocument {
                peer: neighbour_wire::peer_handle(&laptop_device),
                app_id: note.app_id.clone(),
                cluster_id: note.cluster_id.clone(),
                record_id: note.record.clone(),
                envelope: Box::new(op),
            },
        );
    }

    // The page joins: it asks for the note's channel though it holds the
    // note's first write, undecided, and the laptop admits it.
    let page = start_peer(
        host.iroh(),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    ready(&[&page]).await;
    let browser_device = Ed25519PublicKey::from_bytes(browser.key.verifying_key().to_bytes());
    let allowed = BTreeSet::from([Ed25519PublicKey::from_bytes(laptop_device), browser_device]);
    let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    loop {
        let inbound = tokio::time::timeout_at(deadline, session.recv())
            .await
            .expect("the page to take part in the record's channels")
            .expect("host session open");
        let Inbound::Delta { author, data } = inbound else {
            continue;
        };
        if support::is_host(&author) {
            continue;
        }
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            Ok(NeighbourMsg::MlsAccessRequest { request, .. }) if request.channel == scope => {
                assert_eq!(author, *browser_device.as_bytes());
                request
                    .verify()
                    .expect("signed by the browser's device key");
                laptop_peer.learn_request(*request, &vault()).unwrap();
                laptop_peer.reconcile_now(&allowed, &vault()).unwrap();
                for frame in take(&mut laptop_peer, &vault()) {
                    publish(
                        &session,
                        &NeighbourMsg::MlsGovernance {
                            workspace: WORKSPACE.into(),
                            frame: Box::new(frame),
                            delivery: 2,
                        },
                    );
                }
                break;
            }
            _ => {}
        }
    }
    support::admitted(&page).await;

    // The page holds the write for the surface it does not run, and says so:
    // it waits for the app surface alone, not for keys or governance.
    wait_for("the write held for its app surface", || {
        let held = held(&page);
        async move {
            held.is_some_and(|held| held.contains("for its app surface"))
                .then_some(())
        }
    })
    .await;
    let waiting = page.status().routes.get(&note).map(|status| {
        (
            status.awaiting_keys,
            status.awaiting_surface,
            status.awaiting_governance,
        )
    });
    let opened = pages(page.drain());

    assert_eq!(
        waiting,
        Some((0, 3, 0)),
        "every write waits for its app surface alone"
    );
    assert!(
        opened.is_empty(),
        "nothing of the record is opened before its surface arrives"
    );

    // The page asked its workspace for that surface. The host answers, and
    // what the page waited for is decided: it is the bytes hashed to the
    // address asked for that decide it, which the page keeps by that hash.
    keeps.answer();
    wait_for(
        "the record's history decided once its surface arrived",
        || {
            let opened = pages(page.drain());
            let written = written.clone();
            async move { (opened == written).then_some(()) }
        },
    )
    .await;
    wait_for("the route holding nothing", || {
        let waiting = page
            .status()
            .routes
            .get(&note)
            .map_or(0, tilo_browser_peer::session::RouteStatus::waiting);
        async move { (waiting == 0).then_some(()) }
    })
    .await;
    wait_for("nothing held for the note", || async {
        held(&page).is_none().then_some(())
    })
    .await;
    assert_eq!(
        tilo_browser_peer::surfaces::read("so.tilo.notes", &blake3::hash(&surface).to_hex()),
        Some(surface),
        "the page keeps the surface it fetched, by its hash"
    );
}

/// A laptop's session with `host`, placed in the workspace as `device`.
async fn connect(
    host: &tilo_relay::daemon::Relay,
    root: &ed25519_dalek::SigningKey,
    device: &Device,
) -> RelayCoeditSession {
    support::place(host, &device.cert).await;
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect(
        endpoint,
        host.node_addr(),
        root.verifying_key().to_bytes(),
        device.cert.clone(),
        device.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    RelayCoeditSession::join(client, workspace(), device.key.verifying_key().to_bytes()).unwrap()
}
