//! A record's retained history stays short without any device checkpointing
//! it. A browser only writes sealed routes, which the host cannot read, so a
//! member device compacts them on the host's request, and a late joiner
//! converges from the snapshot and the entries it does not contain.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::future_not_send,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "support/participant.rs"]
mod participant;

use std::path::Path;
use std::time::{Duration, Instant};
use tilo_automerge::ApplyIncoming as _;
use tilo_session_client::Outbox;

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::DocumentMutation;
use tilo_session_client::Event;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::support::{durable, enrol, ready, route, start_host, start_peer, wait_for, workspace};

fn channel() -> [u8; 32] {
    *blake3::hash(b"app\0so.tilo.notes\0note-1").as_bytes()
}

/// Retained entries of `author` for the note's route.
fn lineage_len(state: &Path, author: &[u8; 32]) -> usize {
    let lineage = state
        .join("workspaces")
        .join(workspace().to_string())
        .join("retained/v2")
        .join(format!(
            "{}-{}",
            hex::encode(author),
            hex::encode(channel())
        ));
    let Ok(name) = std::fs::read_to_string(lineage.join("CURRENT")) else {
        return 0;
    };
    std::fs::read_dir(lineage.join(name.trim())).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".delta"))
            .count()
    })
}

/// Join late until the replica holds everything `target` holds; returns the
/// writes that landed and the duration.
async fn join_late(
    host: &crate::support::TestHost,
    device: crate::support::Device,
    host_cert: &tilo_user_identity::DeviceCert,
    target: &AutoCommit,
) -> (AutoCommit, usize, Duration) {
    let started = Instant::now();
    let joiner = start_peer(host.iroh(), device.cert, host_cert.clone(), device.key);
    let events = joiner.events();
    joiner
        .join_placed(crate::support::WORKSPACE, "Workspace")
        .unwrap();
    let replica = crate::support::holds(&joiner, &route(), target).await;
    let elapsed = started.elapsed();
    let documents = events
        .drain()
        .into_iter()
        .filter(|event| {
            matches!(event, Event::Landed { route: landed, author: Some(_) } if *landed == route())
        })
        .count();
    joiner.close();
    (replica, documents, elapsed)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_compacts_a_sealed_route_the_host_cannot_read() {
    const EDITS: usize = 300;
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let (a, b, c) = (
        enrol(&root, "browser", DeviceCapabilities::member()),
        enrol(&root, "tablet", DeviceCapabilities::member()),
        enrol(&root, "desktop", DeviceCapabilities::member()),
    );
    let a_key = a.cert.device_public_key;
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let route = route();
    let _laptop = participant::Participant::start(&host.relay, &root).await;
    let https = || host.iroh();
    let [a, b, c] = [a, b, c].map(|device| {
        let peer = start_peer(https(), device.cert, host_device.cert.clone(), device.key);
        peer.join_placed(crate::support::WORKSPACE, "Workspace")
            .unwrap();
        peer
    });
    ready(&[&a, &b, &c]).await;

    // A writes a sealed history the host holds only as ciphertext.
    let mut on_a = AutoCommit::new();
    on_a.put(ROOT, "title", "sealed").unwrap();
    on_a.commit();
    a.write(route.clone(), &DocumentMutation::Snapshot(on_a.save()))
        .unwrap();
    for edit in 0..EDITS {
        let before = on_a.get_heads();
        on_a.put(
            ROOT,
            format!("k{}", edit % 20),
            i64::try_from(edit).unwrap(),
        )
        .unwrap();
        on_a.commit();
        a.write(
            route.clone(),
            &DocumentMutation::Incremental(on_a.save_after(&before)),
        )
        .unwrap();
    }
    durable(&a).await;

    // C writes while the host asks a member to compact what it retained; a
    // member's session answers from its replica on its own.
    let mut on_c = AutoCommit::new();
    on_c.put(ROOT, "from_c", "concurrent").unwrap();
    on_c.commit();
    let asked = Instant::now();
    c.write(
        route.clone(),
        &DocumentMutation::Incremental(on_c.save_incremental()),
    )
    .unwrap();
    durable(&c).await;
    let compactor = host.relay.compactor().clone();
    wait_for("the host to accept a member's statement", || {
        let done = compactor.rounds() > 0;
        async move { done.then_some(()) }
    })
    .await;
    let after_compaction = lineage_len(state.path(), &a_key);
    eprintln!(
        "sealed route: {EDITS} edits of A, {after_compaction} entries left; compacted within {} ms",
        asked.elapsed().as_millis()
    );
    // The host asks once a record's tail grows past what it keeps uncompacted, and a
    // member's session answers on its own: what A wrote since is all that is left.
    assert!(
        after_compaction < EDITS / 3,
        "A's covered history is gone ({after_compaction} left)"
    );
    let mut merged = on_a.fork();
    merged.merge_incoming(&mut on_c).unwrap();
    let (replica, documents, _) = join_late(
        &host,
        enrol(&root, "phone", DeviceCapabilities::member()),
        &host_device.cert,
        &merged,
    )
    .await;
    assert!(
        documents < EDITS / 3,
        "{documents} documents instead of {}",
        EDITS + 2
    );
    assert!(replica.get(ROOT, "from_c").unwrap().is_some());
}
