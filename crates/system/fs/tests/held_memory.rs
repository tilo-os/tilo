//! What a namespace replica holds after applying a long run of remote deltas.
//!
//! A peer creates 2,000 files in 20 rounds; the receiver applies each
//! round's delta as it would from gossip. Its growth is compared with the same namespace loaded
//! from a full save, which holds the document and the indexes built from it and nothing else.

use fs::{Namespace, ROOT_INO};
use tilo_automerge::heap::{self, ThreadCounting};

#[global_allocator]
static HEAP: ThreadCounting = ThreadCounting;

const ROUNDS: usize = 20;
const FILES_PER_ROUND: usize = 100;

#[test]
fn a_namespace_replica_holds_what_its_saved_namespace_holds() {
    let mut remote = Namespace::new("remote").unwrap();
    let mut receiver = Namespace::from_bytes("receiver", &remote.save()).unwrap();
    let _ = receiver.save_incremental();
    let _ = remote.save_incremental();

    let mut grown = 0isize;
    for round in 0..ROUNDS {
        for file in 0..FILES_PER_ROUND {
            remote
                .create_file(ROOT_INO, format!("r{round}-f{file}"), None, 0, 0)
                .unwrap();
        }
        let delta = remote.save_incremental();
        let before = heap::held();
        receiver.apply_remote_and_resolve(&delta).unwrap();
        grown += heap::held() - before;
    }

    let saved = receiver.save();
    let loaded = heap::held_by(|| Namespace::from_bytes("receiver", &saved).unwrap());
    eprintln!("namespace replica: grown {grown} bytes, loaded {loaded} bytes");
    assert!(
        grown < loaded + loaded / 2,
        "a replica fed {} remote files holds {grown} bytes against {loaded} for the same namespace loaded",
        ROUNDS * FILES_PER_ROUND
    );
}
