//! The browser as a `channel-crypto` participant of a channel a native
//! device created: it asks for access with its device key, joins from the
//! Welcome of the participant that admitted it, and then opens the native
//! device's sealed documents and seals its own, with the host carrying only
//! ciphertext and governance.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; each test is one transcript between a native participant and a browser"
)]

mod support;

// Placing the workspace has the host build its governance in an automerge worker.
tilo_automerge_isolate::test_worker!();

use std::collections::BTreeSet;

use automerge::AutoCommit;
use channel_crypto::{Access, ChannelPeer, VaultKey};
use collab_types::{
    ContentRetention, DocumentFrame, Ed25519PublicKey, MlsGovernanceFrame, OpKind, SignedOp,
    document_artifact_id, document_attestation_id, document_channel_id,
};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_relay::{Inbound, RelayClient, RelayCoeditSession};
use tilo_session_host::PeerStore as _;
use tilo_user_identity::{DeviceCapabilities, RevocationList, UserIdentity};

use support::{
    Device, TestHost, WORKSPACE, enrol, hears, ready, refused, route, said, say, start_host,
    start_peer, start_peer_with_store, wait_for, workspace,
};

/// A native participant's side of the workspace, driven by hand: publish a
/// message through the host the way a shell does.
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
fn take(peer: &mut ChannelPeer) -> Vec<MlsGovernanceFrame> {
    let frames = peer.outbox().to_vec();
    peer.published();
    frames
}

/// The epoch `peer` seals under.
fn epoch(peer: &ChannelPeer) -> u64 {
    match peer.access() {
        Access::Ready { epoch } => epoch,
        other => panic!("not in the group: {other:?}"),
    }
}

/// `device`'s own session with the workspace on `host`, announced with its
/// certificate as every device announces itself.
async fn session(host: &TestHost, root: &SigningKey, device: &Device) -> RelayCoeditSession {
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    support::place(&host.relay, &device.cert).await;
    let client = RelayClient::connect(
        endpoint,
        host.relay.node_addr(),
        root.verifying_key().to_bytes(),
        device.cert.clone(),
        device.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    let session =
        RelayCoeditSession::join(client, workspace(), device.key.verifying_key().to_bytes())
            .unwrap();
    publish(
        &session,
        &NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(device.cert.clone()),
            revocations: Vec::new(),
        },
    );
    session
}

/// The laptop: the native device that created the workspace's key scope,
/// writing the note sealed and admitting the browser by hand.
struct Laptop {
    device: Device,
    peer: ChannelPeer,
    session: RelayCoeditSession,
    vault: VaultKey,
    note: AutoCommit,
    seq: u64,
    delivery: u64,
    _state: tempfile::TempDir,
}

impl Laptop {
    async fn start(host: &TestHost, root: &SigningKey, device: Device) -> Self {
        let state = tempfile::tempdir().unwrap();
        let vault = VaultKey::new([7; 32]);
        let peer = ChannelPeer::create_now(
            Some(std::sync::Arc::new(channel_crypto::Files::new(
                state.path(),
            ))),
            collab_types::workspace_scope(WORKSPACE),
            Box::new(device.key.clone()),
            &vault,
        )
        .unwrap();
        let session = session(host, root, &device).await;
        Self {
            device,
            peer,
            session,
            vault,
            note: AutoCommit::new(),
            seq: 0,
            delivery: 0,
            _state: state,
        }
    }

    fn key(&self) -> [u8; 32] {
        self.device.key.verifying_key().to_bytes()
    }

    /// Publish what the key scope queued, as its next governance delivery.
    fn govern(&mut self) {
        self.delivery = self.delivery.saturating_add(1);
        for frame in take(&mut self.peer) {
            publish(
                &self.session,
                &NeighbourMsg::MlsGovernance {
                    workspace: WORKSPACE.into(),
                    frame: Box::new(frame),
                    delivery: self.delivery,
                },
            );
        }
    }

    /// Write `text` to the note, sealed under the scope's current epoch
    /// unless `clear`.
    fn write(&mut self, text: &str, clear: bool) {
        let route = route();
        let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record);
        let attestation = document_attestation_id(&route.app_id, &route.cluster_id, &route.record);
        let mutation = say(&mut self.note, text);
        // A cleartext change names its action; one in the clear is the whole note instead.
        let mutation = if clear {
            collab_types::DocumentMutation::Snapshot(self.note.save())
        } else {
            mutation
        };
        let frame = mutation.encode().unwrap();
        self.seq = self.seq.saturating_add(1);
        let author = Ed25519PublicKey::from_bytes(self.key());
        let now = std::time::SystemTime::now();
        let mut op = if clear {
            SignedOp::new_unsigned(
                frame,
                artifact,
                author,
                attestation,
                OpKind::Change,
                now,
                self.seq,
            )
        } else {
            let (metadata, sealed) = self
                .peer
                .content_keys()
                .unwrap()
                .for_unit(document_channel_id(
                    &route.app_id,
                    &route.cluster_id,
                    &route.record,
                ))
                .seal_content(artifact, ContentRetention::of_frame(&frame), &frame)
                .unwrap();
            SignedOp::new_unsigned_with_encryption(
                sealed,
                artifact,
                author,
                attestation,
                OpKind::Change,
                now,
                self.seq,
                metadata,
            )
        };
        collab_types::sign_op(&mut op, &self.device.key).unwrap();
        publish(
            &self.session,
            &NeighbourMsg::AppDocument {
                peer: neighbour_wire::peer_handle(&self.key()),
                app_id: route.app_id.clone(),
                cluster_id: route.cluster_id.clone(),
                record_id: route.record,
                envelope: Box::new(op),
            },
        );
    }

    /// Admit `browser` to the scope when its signed request arrives, until
    /// its edit of the note does: answers the edit, opened. The browser never
    /// writes the sealed note in the clear.
    async fn admit_until_edit(&mut self, browser: &Device) -> DocumentFrame {
        let browser = Ed25519PublicKey::from_bytes(browser.key.verifying_key().to_bytes());
        let allowed = BTreeSet::from([Ed25519PublicKey::from_bytes(self.key()), browser]);
        let scope = collab_types::workspace_scope(WORKSPACE);
        let route = route();
        let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
        loop {
            let inbound = tokio::time::timeout_at(deadline, self.session.recv())
                .await
                .expect("the browser's sealed edit")
                .expect("host session open");
            let Inbound::Delta { author, data } = inbound else {
                continue;
            };
            // The workspace's host takes part as well: its own writes are not
            // the browser's.
            if support::is_host(&author) {
                continue;
            }
            match postcard::from_bytes::<NeighbourMsg>(&data) {
                // The page asks for the channels of the workspace's other
                // records too.
                Ok(NeighbourMsg::MlsAccessRequest { request, .. }) if request.channel == scope => {
                    assert_eq!(author, *browser.as_bytes());
                    request
                        .verify()
                        .expect("signed by the browser's device key");
                    self.peer.learn_request(*request).unwrap();
                    self.peer.reconcile_now(&allowed, &self.vault).unwrap();
                    self.govern();
                }
                Ok(NeighbourMsg::AppDocument {
                    envelope,
                    record_id,
                    ..
                }) if record_id == route.record => {
                    assert_eq!(author, *browser.as_bytes());
                    let metadata = envelope
                        .encryption
                        .expect("the browser never writes the sealed note in the clear");
                    let clear = self
                        .peer
                        .content_keys()
                        .unwrap()
                        .for_unit(document_channel_id(
                            &route.app_id,
                            &route.cluster_id,
                            &route.record,
                        ))
                        .open_content(
                            document_artifact_id(&route.app_id, &route.cluster_id, &route.record),
                            &metadata,
                            &envelope.payload,
                        )
                        .unwrap();
                    return DocumentFrame::decode(&clear).unwrap();
                }
                _ => {}
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_browser_joins_a_native_channel_and_edits_its_sealed_record() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;

    // The laptop creates the note's channel, announces itself and writes a
    // sealed edit of the note before the browser arrives.
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let mut laptop = Laptop::start(&host, &root, laptop).await;
    laptop.govern();
    laptop.write("from the laptop", false);

    // Publishing only queues the frames for the host. Until it retains the
    // genesis, the workspace's replay names no governor and reads as ungoverned,
    // so let a fresh member's replay prove both are held first. The laptop's
    // identity hello is not replayed: the workspace's governance carries its
    // certificate, and a newcomer takes it from there.
    let observer = enrol(&root, "observer", DeviceCapabilities::member());
    let observer_session = session(&host, &root, &observer).await;
    let (mut genesis, mut sealed) = (false, false);
    let retained_by = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    while !(genesis && sealed) {
        let inbound = tokio::time::timeout_at(retained_by, observer_session.recv())
            .await
            .expect("the host to retain the laptop's frames")
            .expect("observer session open");
        let Inbound::Delta { author, data } = inbound else {
            continue;
        };
        if author != laptop.key() {
            continue;
        }
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            Ok(NeighbourMsg::IdentityHello { .. }) => {
                panic!("the laptop's hello was replayed beside the governance carrying it");
            }
            Ok(NeighbourMsg::MlsGovernance { .. }) => genesis = true,
            Ok(NeighbourMsg::AppDocument { .. }) => sealed = true,
            _ => {}
        }
    }
    observer_session.close().await;

    // The browser joins the workspace and opens the note, its key in custody:
    // every signature it asks for answers later, as a `WebCrypto` key's does.
    let peer = support::start_page(
        host.iroh(),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
        std::sync::Arc::new(tilo_session_host::MemoryPeerStore::default()),
    );
    peer.join_placed(support::WORKSPACE, "Workspace").unwrap();
    peer.open_route(route()).unwrap();
    ready(&[&peer]).await;
    // Written before it holds the keys: held, then sealed once admitted.
    let edit = say(&mut AutoCommit::new(), "from the browser");
    peer.publish(route(), &edit).unwrap();

    // The laptop admits the browser when its signed request arrives, and
    // opens what the browser sealed.
    assert_eq!(
        laptop.admit_until_edit(&browser).await,
        DocumentFrame::Whole(edit)
    );

    // What the laptop sealed before and after the browser joined opens.
    hears(&peer, &route(), "from the laptop").await;
    laptop.write("after the welcome", false);
    hears(&peer, &route(), "after the welcome").await;

    // Another device is admitted and the laptop writes under the new epoch
    // at once. The edit reaches the browser before the commit that carries
    // that epoch's key: it waits for the key instead of being refused.
    let phone = enrol(&root, "phone", DeviceCapabilities::member());
    let phone_device = Ed25519PublicKey::from_bytes(phone.key.verifying_key().to_bytes());
    // The phone announces itself in the workspace, as every device does: the
    // browser vouches only for devices whose certificate the host relayed,
    // and holds a commit adding any other until its certificate arrives.
    let phone_session = session(&host, &root, &phone).await;
    let mut phone_peer = ChannelPeer::waiting(
        None,
        collab_types::workspace_scope(WORKSPACE),
        Box::new(phone.key.clone()),
        &laptop.vault,
    )
    .unwrap();
    let phone_request = phone_peer
        .access_request_now(1, false, &laptop.vault)
        .unwrap()
        .unwrap();
    laptop.peer.learn_request(phone_request).unwrap();
    let allowed = BTreeSet::from([
        Ed25519PublicKey::from_bytes(laptop.key()),
        Ed25519PublicKey::from_bytes(browser.key.verifying_key().to_bytes()),
        phone_device,
    ]);
    let before = epoch(&laptop.peer);
    laptop.peer.reconcile_now(&allowed, &laptop.vault).unwrap();
    assert!(epoch(&laptop.peer) > before);
    laptop.write("under the new epoch", false);
    laptop.govern();
    hears(&peer, &route(), "under the new epoch").await;

    // A write in the clear on the sealed channel is refused, never kept.
    laptop.write("in the clear", true);
    let refusal = wait_for("cleartext on the sealed channel to be refused", || {
        let refusal = refused(&peer);
        async move { refusal }
    })
    .await;
    assert!(
        refusal.contains("plaintext downgrade forbidden"),
        "{refusal}"
    );
    assert!(!said(&peer, &route()).contains("in the clear"));
    phone_session.close().await;
    peer.close();
    host.relay.shutdown().await;
}

/// An edit the browser wrote before a participant admitted it survives a
/// page reload: the reloaded page keeps it with its channel state and seals
/// and commits it once admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_held_for_the_keys_survives_a_reload() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let mut laptop = Laptop::start(&host, &root, laptop).await;
    laptop.govern();

    let store = std::sync::Arc::new(tilo_session_host::MemoryPeerStore::default());
    let start = || {
        start_peer_with_store(
            host.iroh(),
            browser.cert.clone(),
            host_device.cert.clone(),
            browser.key.clone(),
            store.clone(),
        )
    };
    let first = start();
    first.join_placed(support::WORKSPACE, "Workspace").unwrap();
    first.open_route(route()).unwrap();
    ready(&[&first]).await;
    let edit = say(&mut AutoCommit::new(), "before the reload");
    first.publish(route(), &edit).unwrap();
    // The edit is kept with the channel state while it waits for its keys.
    wait_for("the held edit to be stored", || {
        let stored = store.load_channels().is_some_and(|bytes| {
            bytes
                .windows(b"before the reload".len())
                .any(|window| window == b"before the reload")
        });
        async move { stored.then_some(()) }
    })
    .await;
    first.close();

    let reloaded = start();
    reloaded
        .join_placed(support::WORKSPACE, "Workspace")
        .unwrap();
    reloaded.open_route(route()).unwrap();
    assert_eq!(
        laptop.admit_until_edit(&browser).await,
        DocumentFrame::Whole(edit)
    );
    reloaded.close();
    host.relay.shutdown().await;
}

/// A page that joined before the workspace's key scope had its genesis holds
/// its edit until the laptop's genesis and admission, then seals it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_that_joined_first_holds_its_edit_until_the_genesis_admits_it() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let mut laptop = Laptop::start(&host, &root, laptop).await;

    let peer = start_peer(
        host.iroh(),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    peer.join_placed(support::WORKSPACE, "Workspace").unwrap();
    peer.open_route(route()).unwrap();
    ready(&[&peer]).await;
    let edit = say(&mut AutoCommit::new(), "before the genesis");
    peer.publish(route(), &edit).unwrap();

    laptop.govern();
    assert_eq!(
        laptop.admit_until_edit(&browser).await,
        DocumentFrame::Whole(edit),
        "the page's channels:\n{}",
        peer.status().channels
    );
    peer.close();
    host.relay.shutdown().await;
}
