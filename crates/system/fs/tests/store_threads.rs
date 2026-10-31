//! Opening and closing a content store must give the process back the threads
//! it took.
//!
//! An iroh-blobs file store builds a runtime of its own for the actor that
//! owns its database, with a worker thread per CPU, and the store's
//! background gc task holds a handle to the store: while that task lives, the
//! actor and its runtime live with it. The task parks in the content store's
//! gc gate, so closing the store has to wake it and let it end, or a process
//! that opens the store once per operation reaches tens of thousands of
//! threads.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use fs::ContentStore;

/// Rounds enough that a runtime per closed store shows as growth.
const ROUNDS: usize = 8;

/// What iroh-blobs 0.103.0 leaves of a store's runtime it drops from inside
/// itself: one of the runtime's threads stays parked for good. The rest of the
/// runtime, the gc task and the actor end with the store, and that is what
/// closing a store has to bring about; the parked thread is the crate's own
/// teardown and is left as it is, counted so that a closed store keeping a
/// whole runtime (one thread per CPU) can never pass unnoticed.
const PARKED_PER_STORE: usize = 1;

/// How a round ends its store: the two ways a caller does.
#[derive(Clone, Copy)]
enum Closing {
    Closed,
    Dropped,
}

/// This process's threads right now.
fn process_threads() -> usize {
    tilo_test_env::idle::sample_process(std::process::id())
        .expect("this process's threads")
        .len()
}

/// Opens a store, reports the threads it runs on, then ends it as `closing`
/// says.
fn open_and_end(directory: &Path, closing: Closing) -> usize {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the asynchronous open");
    runtime.block_on(async {
        let store = ContentStore::open(directory)
            .await
            .expect("open the content store");
        let open = process_threads();
        match closing {
            Closing::Closed => store.close().await.expect("close the content store"),
            Closing::Dropped => drop(store),
        }
        open
    })
}

#[test]
fn a_closed_store_gives_back_the_threads_it_opened() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let baseline = process_threads();
    let mut peak = baseline;
    for round in 0..ROUNDS {
        let closing = if round % 2 == 0 {
            Closing::Closed
        } else {
            Closing::Dropped
        };
        peak = peak.max(open_and_end(directory.path(), closing));
        assert!(
            peak > baseline,
            "an open store runs threads of its own: {peak} threads, {baseline} before"
        );
    }

    let settled = tilo_test_env::wait_for("the closed stores to end their runtimes", || {
        let threads = process_threads();
        (threads <= baseline + PARKED_PER_STORE * ROUNDS).then_some(threads)
    });
    eprintln!(
        "{ROUNDS} stores opened and closed: {baseline} threads before, {peak} with one open, \
         {settled} after"
    );
}
