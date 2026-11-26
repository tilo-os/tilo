//! One device starts its workspace's history over from a base while the other device's copy
//! still stands on the history the base replaced: both keep writing, the copy moves to the base
//! through the workspace's own sync, and the two converge with every change kept.

use crate::common::{addr, device, localhost_endpoint};

use ed25519_dalek::SigningKey;
use std::time::Duration;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync};
use tilo_peer_state::workspace::base::generation;
use tilo_peer_state::{SeedState, StatePeer, seed_workspace, unranked};
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::SystemTime;

const WORKSPACE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
/// How long a lease taken here lasts: far past the test.
const LEASE: Duration = Duration::from_secs(600);
/// The first job each device takes before the base, and one after it.
const A_BEFORE: &str = "bot:a-before";
const B_BEFORE: &str = "bot:b-before";
const A_AFTER: &str = "bot:a-after";

/// Take the lease of `job` here as `cert`'s device and send it on this device's link, as a
/// workspace change of its own is published.
fn take(sync: &DirectSync, cert: &DeviceCert, job: &str, now: SystemTime) {
    let before = sync.read(|peer| peer.workspace_heads().unwrap_or_default());
    let claimed = sync
        .replica()
        .write(|peer| {
            peer.workspace_mut()
                .take_lease(cert, job, LEASE, unranked(), now)
        })
        .expect("the workspace takes the claim");
    assert!(claimed.is_some(), "{job} is taken");
    let changes = sync.read(|peer| peer.workspace_changes_since(&before));
    if !changes.is_empty() {
        sync.send(neighbour_wire::NeighbourMsg::SyncResp {
            changes,
            workspace: WORKSPACE.to_owned(),
        })
        .expect("the claim is sent");
    }
}

/// Whether this copy's workspace holds `job` taken by `cert`'s device.
fn holds(sync: &DirectSync, cert: &DeviceCert, job: &str, now: SystemTime) -> bool {
    sync.read(|peer| {
        peer.workspace()
            .lease(job, now)
            .holder
            .map(|claim| claim.device)
            == Some(cert.device_public_key)
    })
}

/// How many times `peer`'s workspace started its history over.
fn generation_of(peer: &StatePeer) -> u64 {
    peer.admitted_workspace().map_or(0, generation)
}

/// How many times this copy's workspace started its history over.
fn at(sync: &DirectSync) -> u64 {
    sync.read(generation_of)
}

/// This copy's workspace changes, by hash: what the two copies must hold alike.
fn held(sync: &DirectSync) -> std::collections::BTreeSet<automerge::ChangeHash> {
    sync.read(|peer| {
        peer.workspace_heads()
            .unwrap_or_default()
            .into_iter()
            .collect()
    })
}

/// Wait until `done` holds of `sync`'s copy; `what` names the wait that gave up.
async fn until(sync: &DirectSync, what: &str, done: impl Fn(&StatePeer) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !sync.read(&done) {
            changes.changed().await.expect("the device stays up");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the copies converge: {what}"));
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one convergence scenario: three copies, their writes and the base that joins them"
)]
async fn a_copy_moves_to_the_base_the_other_made_with_every_change_kept() {
    let root = SigningKey::from_bytes(&[0x31; 32]);
    let (a_key, a_cert) = device(&root, 0x32);
    let (b_key, b_cert) = device(&root, 0x33);
    let roster = vec![a_cert.clone(), b_cert.clone()];
    let now = SystemTime::now();

    let mut a = StatePeer::new(
        WORKSPACE,
        a_cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    a.select_exact_admissions().unwrap();
    a.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &a_cert, &roster).unwrap(),
            documents: Vec::new(),
        },
        &a_key,
        now,
    )
    .unwrap();
    let b = StatePeer::from_join(WORKSPACE, a_cert.clone(), roster, &[], now).unwrap();

    let a = DirectSync::new(a, a_key.clone());
    let b = DirectSync::new(b, b_key.clone());
    let a_endpoint = localhost_endpoint(&a_key).await;
    let a_router = iroh::protocol::Router::builder(a_endpoint.clone())
        .accept(DIRECT_ALPN, a.clone())
        .spawn();
    let b_endpoint = localhost_endpoint(&b_key).await;
    let linked = tokio::spawn({
        let b = b.clone();
        let a_addr = addr(&a_endpoint);
        async move { b.connect(&b_endpoint, a_addr).await }
    });
    until(&b, "B holds the workspace", |b| {
        b.workspace_heads().is_some()
    })
    .await;

    // Each device takes a job on the history they share, and the other takes it in.
    take(&a, &a_cert, A_BEFORE, now);
    until(&b, "B holds A's job", |b| {
        b.workspace().lease(A_BEFORE, now).holder.is_some()
    })
    .await;

    // B takes its job while it stands on the history A is about to replace, and does not send
    // it yet: the base A makes cannot hold it.
    let before = b.read(|peer| peer.workspace_heads().unwrap_or_default());
    let claimed = b
        .replica()
        .write(|peer| {
            peer.workspace_mut()
                .take_lease(&b_cert, B_BEFORE, LEASE, unranked(), now)
        })
        .expect("B takes its job");
    assert!(claimed.is_some());

    // A starts its history over, as a device that administers it does when the history is due,
    // and sends the workspace as the host sends it after a compaction: the whole document.
    let compacted = a
        .replica()
        .write(|peer| peer.compact_workspace(&a_cert, &a_key, now))
        .expect("A compacts the workspace it administers");
    assert!(compacted, "A's history starts over");
    assert_eq!(at(&a), 1, "A stands on the base it made");
    a.send(a.replica().read(StatePeer::workspace_message))
        .expect("the base is sent");

    // B moves to the base through the workspace's own sync, and what it wrote on the history it
    // leaves is written again on the base rather than dropped.
    until(&b, "B adopts the base", |b| generation_of(b) == 1).await;
    until(&b, "B keeps its job on the base", |b| {
        b.workspace()
            .lease(B_BEFORE, now)
            .holder
            .map(|claim| claim.device)
            == Some(b_cert.device_public_key)
    })
    .await;
    let changes = b.read(|peer| peer.workspace_changes_since(&before));
    assert!(
        !changes.is_empty(),
        "B's job is written again on the base, as a change of its own"
    );
    b.send(neighbour_msg(WORKSPACE, changes))
        .expect("B sends its copy");

    // The two keep writing on the base, and each takes in the other's.
    until(&a, "A holds B's job", |a| {
        a.workspace()
            .lease(B_BEFORE, now)
            .holder
            .map(|claim| claim.device)
            == Some(b_cert.device_public_key)
    })
    .await;
    take(&a, &a_cert, A_AFTER, now);
    until(&b, "B holds A's later job", |b| {
        b.workspace().lease(A_AFTER, now).holder.is_some()
    })
    .await;

    assert_eq!(at(&a), at(&b), "both stand on the same generation");
    assert_eq!(
        held(&a),
        held(&b),
        "the two copies hold the same heads: nothing either wrote was left out"
    );
    assert!(
        holds(&b, &a_cert, A_BEFORE, now),
        "B holds the job A took before the base"
    );

    linked.abort();
    a_router.shutdown().await.unwrap();
}

/// The message carrying `changes` of `workspace`, as a device sends what it holds.
fn neighbour_msg(workspace: &str, changes: Vec<Vec<u8>>) -> neighbour_wire::NeighbourMsg {
    neighbour_wire::NeighbourMsg::SyncResp {
        changes,
        workspace: workspace.to_owned(),
    }
}
