//! A chain's head, as the store answers the device that opens it: the newest
//! part this store holds, whichever read learned it.

use super::*;
use crate::payloads::Route;
use tilo_relay_protocol::WorkspaceId;

const APP: &str = "so.tilo.chat";
const BASE: &str = "derived:messages@derived:default@tab:0000000000000000";

fn part(index: u64) -> String {
    format!("derived:part.{index}@{BASE}")
}

fn route(record: &str) -> Route {
    Route {
        app_id: APP.to_owned(),
        cluster_id: "7".to_owned(),
        record_id: record.to_owned(),
    }
}

fn channel(record: &str) -> [u8; 32] {
    neighbour_wire::app_document_channel(APP, record)
}

fn head() -> [u8; 32] {
    channel(&neighbour_wire::chain_head(BASE))
}

/// A lineage of `channel` holding `record`'s first publication, written the way
/// another device's retention leaves one: the store finds it on disk, not
/// through its own retention, and learns the chain only by reading it.
fn retained(store: &WorkspaceStore, key: &ed25519_dalek::SigningKey, record: &str) {
    let frames = crate::compaction::document_frames(
        &*store.payloads,
        key,
        WorkspaceId::from_u128(7),
        &route(record),
        record.as_bytes(),
    )
    .expect("the frames of a first publication");
    let lineage = store.root.join(RETAINED_DIR).join("v2").join(format!(
        "{}-{}",
        hex::encode([9; 32]),
        hex::encode(channel(record))
    ));
    let generation = "00000000000000000000000000000000";
    fs::create_dir_all(lineage.join(generation)).expect("a lineage");
    fs::write(lineage.join("CURRENT"), generation).expect("its current generation");
    for (index, frame) in frames.iter().enumerate() {
        fs::write(
            lineage.join(generation).join(format!("{index:016x}.delta")),
            frame,
        )
        .expect("a retained frame");
    }
}

/// The lineages of the store as last read, and the directory's time then: what
/// the next read compares against to decide that nothing changed.
fn scanned(store: &WorkspaceStore) -> std::time::SystemTime {
    store
        .root
        .join(RETAINED_DIR)
        .join("v2")
        .metadata()
        .and_then(|meta| meta.modified())
        .expect("the lineages' time")
}

fn hold_the_time(store: &WorkspaceStore, at: std::time::SystemTime) {
    fs::File::open(store.root.join(RETAINED_DIR).join("v2"))
        .expect("the lineages' directory")
        .set_modified(at)
        .expect("its time held where it was");
}

/// A part another device wrote is the chain's head even when it was retained
/// without this store reading it: the answer is the chain a device follows from
/// then on, and one told an earlier part — or that the chain has no part at all
/// — never sees what the chain holds now.
#[test]
fn a_part_the_store_has_not_read_is_still_found_as_the_chain_head() {
    use ed25519_dalek::SigningKey;
    let tmp = tempfile::tempdir().expect("a store of its own");
    let store = WorkspaceStore::open(tmp.path(), WorkspaceId::from_u128(7), false).expect("a store");
    let key = SigningKey::from_bytes(&[4; 32]);

    retained(&store, &key, &part(0));
    let read_at = scanned(&store);
    assert_eq!(
        store.chain_head_channel(&head()).expect("a head"),
        Some(channel(&part(0))),
        "the first part is the chain's head"
    );

    // The next part is written before the directory's time moved on, as two
    // parts of a chain the crowd is writing leave it.
    retained(&store, &key, &part(1));
    hold_the_time(&store, read_at);

    assert_eq!(
        store.chain_head_channel(&head()).expect("a head"),
        Some(channel(&part(1))),
        "the newest part is the head, whether or not a read saw the directory change"
    );
}
