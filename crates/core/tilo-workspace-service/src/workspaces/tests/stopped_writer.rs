//! The part that ends writes nothing beside the part that starts next: the person's services run
//! on a runtime that outlives the part, so a copy of the service a task waited on is still kept
//! after the part that made it ended. That copy builds and numbers no change any more, so one
//! record's device sequence never names two changes of this device.
#![allow(
    clippy::significant_drop_tightening,
    reason = "each fixture is kept for the part it serves, and dropped with it"
)]

use tilo_resource::Invoke;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_user_identity::DeviceCert;

use super::*;
use crate::workspaces::fixtures::{group_keys, test_host_holding};
use crate::workspaces::person_workspace::{
    self,
    devices::{DEVICES, Devices},
    read_on,
};

/// The write that keeps `fact` of `cert`'s device in the record of the person's devices, its
/// value one byte.
fn keeps(cert: &DeviceCert, fact: &str) -> Invoke<Devices> {
    Devices::KEEP_FACT.with((
        hex::encode(cert.device_public_key),
        hex::encode(postcard::to_allocvec(cert).expect("a certificate encodes")),
        fact.to_owned(),
        "01".to_owned(),
    ))
}

/// Every change `workspace`'s store keeps, as its bytes and the device and number it is named
/// by: what two changes must never share.
fn kept_changes(
    store_root: &std::path::Path,
    workspace: &str,
) -> Vec<((tilo_peer_state::store::Lineage, u64), Vec<u8>)> {
    use tilo_peer_state::store::{Appetite, Budget, Store, advance};
    let store = Store::open(store_root, Appetite::StoreAll).expect("the store opens");
    let id = tilo_server_protocol::WorkspaceId::parse(workspace).expect("a workspace id");
    let mut cursors = Vec::new();
    let mut changes = Vec::new();
    loop {
        let replay = store
            .replay(
                id,
                &cursors,
                Budget {
                    max_frames: 100_000,
                    max_bytes: u64::MAX,
                },
            )
            .expect("the store reads back what it kept");
        advance(&mut cursors, &replay.frames);
        for message in &replay.frames {
            if let Ok(frame) = tilo_peer_state::store::Frame::of(message) {
                let bytes = postcard::to_allocvec(&frame.message).expect("a frame encodes");
                changes.push(((frame.lineage, frame.seq), bytes));
            }
        }
        if !replay.more {
            return changes;
        }
    }
}

/// The device that wrote `bytes`.
fn author_of(bytes: &[u8]) -> [u8; 32] {
    let Ok(neighbour_wire::NeighbourMsg::AppDocument { envelope, .. }) =
        postcard::from_bytes::<neighbour_wire::NeighbourMsg>(bytes)
    else {
        unreachable!("a kept change is a record's write")
    };
    *envelope.author.as_bytes()
}

/// Whether `bytes` is a write of `record`, the record's own document rather than its founding.
fn is_devices_record(bytes: &[u8], suffix: &str) -> bool {
    let Ok(neighbour_wire::NeighbourMsg::AppDocument {
        app_id,
        document_id,
        ..
    }) = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(bytes)
    else {
        return false;
    };
    app_id == DEVICES.0 && document_id.ends_with(suffix)
}

/// A part ends and the next starts over the same stores: the copy of the service the person's
/// services waited on builds no change beside the next part's, every write the next part makes
/// reaches the record, and the device gives one number of the record to one change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_parts_copy_numbers_no_change_beside_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let first = test_host_holding(dir.path(), &key, &cert, group_keys()).await;
    let workspace = person_workspace::found(first.workspaces())
        .await
        .expect("the part founds its person's own workspace");
    assert_eq!(workspace, person_workspace_id(&cert.user_public_key));
    person_workspace::apply_on(
        first.workspaces(),
        &workspace,
        DEVICES,
        Devices::keeping(&cert).expect("the device's entry"),
    )
    .await
    .expect("the device is in its person's record");
    // The copy a person service task keeps: its runtime outlives the part, so it is here after
    // the part ended.
    let left_behind: Workspaces = first.workspaces().clone();
    // The person's part ends, and the part that starts next opens the same stores.
    first.shutdown().await;
    let next = test_host_holding(dir.path(), &key, &cert, group_keys()).await;
    assert!(
        left_behind.stopped(),
        "a copy of the service the part ended is stopped"
    );
    // The copy that was left behind writes the record the next part writes: it builds and
    // numbers nothing, so the device's numbers are the next part's alone.
    let afterwards = person_workspace::apply_on(
        &left_behind,
        &workspace,
        DEVICES,
        keeps(&cert, "left-behind"),
    )
    .await;
    // Every write of the next part, each on what the one before it wrote.
    let facts = ["first", "second", "third", "fourth"];
    for fact in facts {
        person_workspace::apply_on(next.workspaces(), &workspace, DEVICES, keeps(&cert, fact))
            .await
            .unwrap_or_else(|error| panic!("the next part keeps {fact}: {error}"));
    }
    // Every write reached the record the next part reads.
    let record = read_on::<Devices>(next.workspaces(), &workspace, DEVICES)
        .await
        .expect("the next part reads the record");
    let kept = &record
        .devices
        .get(&tilo_resource::Id::from_raw(hex::encode(cert.device_public_key)))
        .expect("the device's entry")
        .facts;
    for fact in facts {
        assert!(kept.contains_key(fact), "the record holds {fact}");
    }
    // No device names two changes at one number of one record, and the record's line carries
    // every change it made, one number after another.
    drop(next);
    let changes = kept_changes(&dir.path().join("store"), &workspace);
    let mut at_one_number: std::collections::BTreeMap<_, Vec<u8>> =
        std::collections::BTreeMap::new();
    for (at, bytes) in &changes {
        if let Some(before) = at_one_number.insert(*at, bytes.clone()) {
            assert_eq!(
                &before, bytes,
                "two changes of one device at one sequence number: {at:?}"
            );
        }
    }
    let record_suffix = format!("/{}~0", DEVICES.1);
    let devices: Vec<(&u64, &[u8])> = changes
        .iter()
        .filter(|(_, bytes)| is_devices_record(bytes, &record_suffix))
        .map(|((_, seq), bytes)| (seq, bytes.as_slice()))
        .collect();
    assert!(!devices.is_empty(), "the record's changes are kept");
    let device = author_of(devices[0].1);
    assert!(
        devices.iter().all(|(_, bytes)| author_of(bytes) == device),
        "the record's changes are all one device's"
    );
    let mut sequences: Vec<u64> = devices.iter().map(|(seq, _)| **seq).collect();
    sequences.sort_unstable();
    let wanted: Vec<u64> = (1..=u64::try_from(sequences.len()).expect("a count")).collect();
    assert_eq!(
        sequences, wanted,
        "one device numbers the record's changes one after another"
    );
    // The copy the part that ended left behind builds no change at all.
    assert!(
        afterwards.is_err(),
        "a part that ended builds no change: {afterwards:?}"
    );
}
