//! What a pinned relay workspace holds after materializing a long run of remote deltas.
//!
//! A device types and writes settings in 20 rounds of 1000 ops; the relay takes each round in
//! through anti-entropy ingest, as it would from a peer. Its growth is compared with the same
//! document loaded from a full save.

#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT};
use tilo_automerge::heap::{self, ThreadCounting};
use tilo_relay::workspace::{Membership, WorkspaceRuntime};

#[global_allocator]
static HEAP: ThreadCounting = ThreadCounting;

const WORKSPACE: u64 = 7;
const ROUNDS: u64 = 20;
const OPS_PER_ROUND: usize = 1000;
const SETTINGS_EVERY: usize = 10;
const TYPED_RUN: &str = "xxxxxxxxx";
// Every round is still 1000 ops: 100 settings writes and 900 typed characters.
const _: () = assert!(TYPED_RUN.len() == SETTINGS_EVERY - 1);

#[test]
fn a_pinned_workspace_holds_what_its_saved_document_holds() {
    let state = tempfile::tempdir().unwrap();
    let mut workspace =
        WorkspaceRuntime::open(state.path(), WORKSPACE, true, Membership::default()).unwrap();

    let mut remote = AutoCommit::new();
    let text = remote.put_object(ROOT, "text", ObjType::Text).unwrap();
    let settings = remote.put_object(ROOT, "settings", ObjType::Map).unwrap();
    let mut grown = 0isize;
    let mut typed = 0usize;
    for round in 0..ROUNDS {
        // Each settings write is followed by nine typed characters. One splice of the
        // nine records the same nine single-character inserts, each after the last, as
        // nine one-character splices do, without locating the insertion point nine times.
        for op in (0..OPS_PER_ROUND).step_by(SETTINGS_EVERY) {
            remote
                .put(&settings, format!("k{}", op % 50), round)
                .unwrap();
            remote.splice_text(&text, typed, 0, TYPED_RUN).unwrap();
            typed += TYPED_RUN.len();
        }
        let delta = remote.save_incremental();
        let before = heap::held();
        assert!(
            workspace
                .ingest_sync_changes(WORKSPACE, &delta, round + 1)
                .unwrap()
        );
        grown += heap::held() - before;
    }
    assert_eq!(
        workspace.current_heads(),
        remote
            .get_heads()
            .into_iter()
            .map(|h| h.0)
            .collect::<Vec<_>>()
    );

    let saved = remote.save();
    let loaded = heap::held_by(|| AutoCommit::load(&saved).unwrap());
    eprintln!("relay workspace: grown {grown} bytes, loaded {loaded} bytes");
    assert!(
        grown < loaded * 2,
        "a workspace fed 20,000 remote ops holds {grown} bytes against {loaded} loaded"
    );
}
