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

use std::time::SystemTime;
use tilo_session_client::Outbox;

use collab_types::surface::ContentSurfaces;
use collab_types::{
    Ed25519PublicKey, OpKind, SignedOp, Surfaces, document_artifact_id, document_attestation_id,
};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::native::NativeDevice;
use crate::support::{WORKSPACE, enrol, ready, route, start_host, start_peer, workspace};

/// The surface the laptop's write names: a version of the app this page does
/// not run, whose `surface.toml` is not here.
const ELSEWHERE: [u8; 32] = [0x42; 32];
const WRITTEN: &str = "written before the page joined";

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
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
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

    // The laptop joins the group the host founded, announces itself and
    // writes the record's only write, sealed and naming another version's
    // surface.
    let note = route();
    let artifact = document_artifact_id(&note.app_id, &note.cluster_id, &note.record);
    let laptop_device = laptop.key.verifying_key().to_bytes();
    crate::support::place(&host.relay, &laptop.cert).await;
    let mut laptop = NativeDevice::start(
        &host.relay,
        root.verifying_key().to_bytes(),
        laptop,
        workspace(),
    )
    .await;
    laptop.join().await;
    let frame = crate::support::say(&mut automerge::AutoCommit::new(), WRITTEN)
        .encode()
        .unwrap();
    let (metadata, sealed) = laptop.seal(&note, &frame);
    let op = SignedOp::new_unsigned_with_encryption(
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
    laptop.write(&note, op);

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

    // The page asks for the workspace's keys though it holds the note's
    // only write, undecided; the host admits it.
    crate::support::admitted(&page).await;

    // The keys are the page's; the write itself still waits for its surface,
    // which is a version of the app the page does not run.
    assert!(
        !crate::support::said(&page, &note).contains(WRITTEN),
        "the write is held for its app surface, not opened"
    );
}
