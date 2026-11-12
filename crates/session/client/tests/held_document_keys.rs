//! A page that joins a record whose retained writes it cannot decide yet --
//! here because each names an app surface this page does not run -- still
//! takes part in the record's channel and takes its content keys. Which
//! devices read a channel is its workspace's governance, never a single
//! write: asking only once a write is decided would leave the page holding
//! everything forever, for the keys that decide it are the ones a channel
//! it never joined would hand out.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; one test is one transcript between a native participant and a page"
)]

use std::collections::BTreeSet;
use std::time::SystemTime;
use tilo_session_client::Outbox;

use channel_crypto::{ChannelPeer, VaultKey};
use collab_types::surface::ContentSurfaces;
use collab_types::{
    ContentRetention, Ed25519PublicKey, MlsGovernanceFrame, OpKind, SignedOp, Surfaces,
    document_artifact_id, document_attestation_id, document_channel_id,
};
use neighbour_wire::NeighbourMsg;
use tilo_relay::{Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList, UserIdentity};

use crate::support::{Device, WORKSPACE, enrol, ready, route, start_host, start_peer, workspace};

/// The surface the laptop's write names: a version of the app this page does
/// not run, whose `surface.toml` is not here.
const ELSEWHERE: [u8; 32] = [0x42; 32];
const WRITTEN: &str = "written before the page joined";

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
fn take(peer: &mut ChannelPeer) -> Vec<MlsGovernanceFrame> {
    let frames = peer.outbox().to_vec();
    peer.published();
    frames
}

fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_takes_the_record_keys_though_every_write_it_received_is_held() {
    // The page runs the app at one version only; the writes it is replayed
    // name another's surface, which this process cannot find.
    tilo_peer_state::decide_with_surfaces(
        Surfaces::from_source(std::sync::Arc::new(ContentSurfaces::new(
            |_: &collab_types::Record<'_>, _: &str| -> Option<Vec<u8>> { None },
        ))),
        |_| None,
    );

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

    // The laptop creates the workspace's key scope, announces itself and
    // writes the record's only write, sealed and naming another version's
    // surface.
    let note = route();
    let scope = collab_types::workspace_scope(WORKSPACE);
    let channel = document_channel_id(&note.app_id, &note.cluster_id, &note.record);
    let artifact = document_artifact_id(&note.app_id, &note.cluster_id, &note.record);
    let laptop_dir = tempfile::tempdir().unwrap();
    let mut laptop_peer = ChannelPeer::create_now(
        Some(std::sync::Arc::new(channel_crypto::Files::new(
            laptop_dir.path(),
        ))),
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
    for frame in take(&mut laptop_peer) {
        publish(
            &session,
            &NeighbourMsg::MlsGovernance {
                workspace: WORKSPACE.into(),
                frame: Box::new(frame),
                delivery: 1,
            },
        );
    }
    let frame = crate::support::say(&mut automerge::AutoCommit::new(), WRITTEN)
        .encode()
        .unwrap();
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
        1,
        metadata,
    )
    .with_surface(Some(ELSEWHERE));
    collab_types::sign_op(&mut op, &laptop.key).unwrap();
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

    // The page joins and opens nothing on its own: the app finds no record to
    // open, and its only write for the note is the one the host replays.
    let page = start_peer(
        host.iroh(),
        browser.cert.clone(),
        host_device.cert.clone(),
        browser.key.clone(),
    );
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    ready(&[&page]).await;

    // The page asks for the note's channel though it holds the note's only
    // write, undecided; the laptop admits it.
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
        if crate::support::is_host(&author) {
            continue;
        }
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            // The page asks for the channels of the workspace's other records too.
            Ok(NeighbourMsg::MlsAccessRequest { request, .. }) if request.channel == scope => {
                assert_eq!(author, *browser_device.as_bytes());
                request
                    .verify()
                    .expect("signed by the browser's device key");
                laptop_peer.learn_request(*request).unwrap();
                laptop_peer.reconcile_now(&allowed, &vault()).unwrap();
                for frame in take(&mut laptop_peer) {
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
    crate::support::admitted(&page).await;

    // The keys are the page's; the write itself still waits for its surface,
    // which is a version of the app the page does not run.
    assert!(
        !crate::support::said(&page, &note).contains(WRITTEN),
        "the write is held for its app surface, not opened"
    );
}

/// A laptop's session with `host`, placed in the workspace as `device`.
async fn connect(
    host: &tilo_relay::daemon::Relay,
    root: &ed25519_dalek::SigningKey,
    device: &Device,
) -> RelayCoeditSession {
    crate::support::place(host, &device.cert).await;
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
