//! The browser as a `channel-crypto` member of the workspace's group: it asks
//! for access with its device key, joins from the Welcome of the member that
//! admitted it, and then opens a native device's sealed documents and seals
//! its own. The workspace's host created the workspace, so it founds the group.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; each test is one transcript between a native participant and a browser"
)]

use tilo_session_client::{Cmd, Outbox};

use automerge::AutoCommit;
use channel_crypto::{Access, ChannelPeer};
use collab_types::{
    DocumentFrame, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
    document_attestation_id,
};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_relay::{Inbound, RelayCoeditSession};
use tilo_session_client::PeerStore as _;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::native::{NativeDevice, connect, vault};
use crate::support::{
    Device, TestHost, enrol, hears, ready, restart_host, route, said, say, start_host, start_peer,
    start_peer_with_store, wait_for, workspace,
};

/// The epoch `peer` seals under.
fn epoch(peer: &ChannelPeer) -> u64 {
    match peer.access() {
        Access::Ready { epoch } => epoch,
        other => panic!("not in the group: {other:?}"),
    }
}

/// `device`'s own session with the workspace on `host`.
async fn session(host: &TestHost, root: &SigningKey, device: &Device) -> RelayCoeditSession {
    crate::support::place(&host.relay, &device.cert).await;
    connect(
        &host.relay,
        root.verifying_key().to_bytes(),
        device,
        workspace(),
    )
    .await
}

/// The laptop: a native device that writes the note sealed and opens what
/// the browser seals.
struct Laptop {
    native: NativeDevice,
    note: AutoCommit,
    seq: u64,
}

impl Laptop {
    async fn start(host: &TestHost, root: &SigningKey, device: Device) -> Self {
        crate::support::place(&host.relay, &device.cert).await;
        let native = NativeDevice::start(
            &host.relay,
            root.verifying_key().to_bytes(),
            device,
            workspace(),
        )
        .await;
        Self {
            native,
            note: AutoCommit::new(),
            seq: 0,
        }
    }

    /// Write `text` to the note, sealed under the group's current epoch
    /// unless `clear`.
    fn write(&mut self, text: &str, clear: bool) {
        let route = route();
        let artifact = document_artifact_id(&route.app_id, &route.cluster_id, &route.record);
        let attestation = document_attestation_id(&route.app_id, &route.cluster_id, &route.record);
        let mutation = say(&mut self.note, text);
        // A sealed change names its action; one in the clear is the whole note instead.
        let mutation = if clear {
            collab_types::DocumentMutation::Snapshot(self.note.save())
        } else {
            mutation
        };
        let frame = mutation.encode().unwrap();
        self.seq = self.seq.saturating_add(1);
        let author = Ed25519PublicKey::from_bytes(self.native.key());
        let now = std::time::SystemTime::now();
        let op = if clear {
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
            let (metadata, sealed) = self.native.seal(&route, &frame);
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
        self.native.write(&route, op);
    }

    /// The browser's next edit of the note.
    async fn browser_edit(&mut self, browser: &Device) -> DocumentFrame {
        self.native
            .written(browser.key.verifying_key().to_bytes(), &route())
            .await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_browser_joins_a_native_channel_and_edits_its_sealed_record() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;

    // The laptop joins the group the host founded, announces itself and
    // writes a sealed edit of the note before the browser arrives.
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let mut laptop = Laptop::start(&host, &root, laptop).await;
    laptop.native.join().await;
    laptop.write("from the laptop", false);

    // Publishing only queues the frames for the host, so let a fresh
    // member's replay prove the sealed edit is held first. The laptop's
    // identity hello is not replayed: the workspace's governance carries its
    // certificate, and a newcomer takes it from there.
    let observer = enrol(&root, "observer", DeviceCapabilities::member());
    let observer_session = session(&host, &root, &observer).await;
    let retained_by = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
    loop {
        let inbound = tokio::time::timeout_at(retained_by, observer_session.recv())
            .await
            .expect("the host to retain the laptop's edit")
            .expect("observer session open");
        let Inbound::Delta { author, data } = inbound else {
            continue;
        };
        if author != laptop.native.key() {
            continue;
        }
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            Ok(NeighbourMsg::IdentityHello { .. }) => {
                panic!("the laptop's hello was replayed beside the governance carrying it");
            }
            Ok(NeighbourMsg::AppDocument { .. }) => break,
            _ => {}
        }
    }
    observer_session.close().await;

    // The browser joins the workspace and opens the note, its key in custody:
    // every signature it asks for answers later, as a `WebCrypto` key's does.
    let store = std::sync::Arc::new(tilo_session_client::MemoryPeerStore::default());
    let page = || {
        let peer = crate::support::start_page(
            host.iroh(),
            browser.cert.clone(),
            host_device.cert.clone(),
            browser.key.clone(),
            store.clone(),
        );
        peer.join_placed(crate::support::WORKSPACE, "Workspace")
            .unwrap();
        peer.send(Cmd::OpenRoute(route())).unwrap();
        peer
    };
    let peer = page();
    ready(&[&peer]).await;
    // Written before it holds the keys: held, then sealed once admitted.
    let edit = say(&mut AutoCommit::new(), "from the browser");
    peer.write(route(), &edit).unwrap();

    // The host admits the browser when its signed request arrives, and the
    // laptop opens what the browser sealed.
    assert_eq!(
        laptop.browser_edit(&browser).await,
        DocumentFrame::Whole(edit)
    );

    // What the laptop sealed before and after the browser joined opens.
    hears(&peer, &route(), "from the laptop").await;
    laptop.write("after the welcome", false);
    hears(&peer, &route(), "after the welcome").await;

    // The laptop admits another device and writes under the new epoch at
    // once. The edit reaches the browser before the commit that carries
    // that epoch's key: it waits for the key instead of being refused.
    let phone = enrol(&root, "phone", DeviceCapabilities::member());
    let phone_device = Ed25519PublicKey::from_bytes(phone.key.verifying_key().to_bytes());
    // The phone announces itself in the workspace, as every device does: the
    // browser vouches only for devices whose certificate the host relayed,
    // and holds a commit adding any other until its certificate arrives.
    let phone_session = session(&host, &root, &phone).await;
    let mut phone_peer = ChannelPeer::waiting(
        None,
        collab_types::WorkspaceId::parse(crate::support::WORKSPACE).unwrap(),
        Box::new(phone.key.clone()),
        &vault(),
    )
    .unwrap();
    let phone_request = phone_peer
        .access_request_now(1, false, &vault())
        .unwrap()
        .unwrap();
    let native = &mut laptop.native;
    native.peer.learn_request(phone_request).unwrap();
    let mut allowed = native.peer.members();
    allowed.insert(phone_device);
    let before = epoch(&native.peer);
    native.peer.reconcile_now(&allowed, &vault()).unwrap();
    assert!(epoch(&native.peer) > before);
    laptop.write("under the new epoch", false);
    laptop.native.govern();
    hears(&peer, &route(), "under the new epoch").await;

    // A write in the clear on the sealed channel is a downgrade: the host
    // neither keeps nor forwards it, so the page never meets it. The
    // laptop's next sealed write, sent after it, says the host decided it.
    let unsent = laptop.note.clone();
    laptop.write("in the clear", true);
    laptop.note = unsent;
    laptop.write("sealed after it", false);
    hears(&peer, &route(), "sealed after it").await;
    assert!(!said(&peer, &route()).contains("in the clear"));

    // The page reloads and opens the note again: what the host kept of it
    // holds no write in the clear after the seal.
    peer.close();
    let peer = page();
    ready(&[&peer]).await;
    hears(&peer, &route(), "sealed after it").await;
    assert!(!said(&peer, &route()).contains("in the clear"));
    phone_session.close().await;
    peer.close();
    host.relay.shutdown().await;
}

/// An edit the browser wrote before it was admitted survives a page reload:
/// the reloaded page keeps it with its channel state and seals and commits it
/// once admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_held_for_the_keys_survives_a_reload() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser = enrol(&root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    // The host is down while the page writes, so nothing can admit it.
    let iroh = host.iroh();
    drop(host.web);
    host.relay.shutdown().await;

    let store = std::sync::Arc::new(tilo_session_client::MemoryPeerStore::default());
    let start = |host: tilo_relay_client::Host| {
        start_peer_with_store(
            host,
            browser.cert.clone(),
            host_device.cert.clone(),
            browser.key.clone(),
            store.clone(),
        )
    };
    let first = start(iroh);
    first
        .join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    first.send(Cmd::OpenRoute(route())).unwrap();
    let edit = say(&mut AutoCommit::new(), "before the reload");
    first.write(route(), &edit).unwrap();
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

    let host = restart_host(&root, &host_device, state.path()).await;
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let mut laptop = Laptop::start(&host, &root, laptop).await;
    laptop.native.join().await;
    let reloaded = start(host.iroh());
    reloaded
        .join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    reloaded.send(Cmd::OpenRoute(route())).unwrap();
    assert_eq!(
        laptop.browser_edit(&browser).await,
        DocumentFrame::Whole(edit)
    );
    reloaded.close();
    host.relay.shutdown().await;
}

/// A page that joined before the workspace's group existed holds its edit
/// until the host founds the group and admits it, then seals it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_that_joined_first_holds_its_edit_until_the_genesis_admits_it() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
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
    peer.join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    peer.send(Cmd::OpenRoute(route())).unwrap();
    ready(&[&peer]).await;
    let edit = say(&mut AutoCommit::new(), "before the genesis");
    peer.write(route(), &edit).unwrap();

    laptop.native.join().await;
    assert_eq!(
        laptop.browser_edit(&browser).await,
        DocumentFrame::Whole(edit),
        "the page's channels:\n{}",
        peer.status().channels
    );
    peer.close();
    host.relay.shutdown().await;
}
