//! Opening and closing a content store must give the process back the threads it
//! took: the store's background gc task holds a handle to the store, and its
//! runtime lives while that task does, so closing has to wake it and let it end.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use fs::ContentStore;

const ROUNDS: usize = 8;

const PARKED_PER_STORE: usize = 1;

#[derive(Clone, Copy)]
enum Closing {
    Closed,
    Dropped,
}

fn process_threads() -> usize {
    tilo_test_env::idle::sample_process(std::process::id())
        .expect("this process's threads")
        .len()
}

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
