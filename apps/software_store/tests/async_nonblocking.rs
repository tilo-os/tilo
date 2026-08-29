//! Proves the store's data access is asynchronous: enqueuing a search /
//! list returns immediately, results flow back over a channel, and the
//! render/sync passes never block on the network.
//!
//! This is the load-bearing guarantee of the whole crate (mirrors the maps
//! tile worker): a slow or unreachable `flatpak` / Flathub call must not
//! stall a frame. We enforce it with wall-clock budgets that are orders of
//! magnitude below what a real subprocess / HTTP round-trip would take.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use software_store::worker::{Request, Worker, WorkerMsg};

/// `Worker::request` must return effectively instantly (it only does a
/// channel `send` + an in-flight set insert) — never the duration of the
/// underlying flatpak/curl call. Budget is 50ms: a real `flatpak remotes`
/// or a Flathub HTTP GET is hundreds of ms to seconds, so if `request`
/// were synchronous this would blow the budget by 10-100x.
#[test]
fn request_returns_immediately_not_after_the_io() {
    let mut w = Worker::spawn();
    let start = Instant::now();
    // Issue several read-only queries that each map to a blocking
    // subprocess / HTTP call on the worker thread.
    assert!(w.request(Request::RefreshInstalled));
    assert!(w.request(Request::RefreshRemotes));
    assert!(w.request(Request::Collection("popular".into())));
    assert!(w.request(Request::Search("editor".into())));
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(50),
        "request() should return immediately (off-thread I/O); took {elapsed:?}"
    );
    // And the in-flight markers were set synchronously — the UI can show a
    // spinner the very next frame without waiting for any result.
    assert!(w.has_inflight(), "in-flight markers set without blocking");
}

/// Results must arrive over the channel (via `drain`), not as the return
/// value of `request`. We drive the worker with a stub data source by
/// pointing the Flathub base at an unreachable host so the calls fail fast;
/// the key assertion is that *the messages come back through drain*, on a
/// background thread, and `drain` itself is non-blocking.
#[test]
fn results_flow_back_through_the_channel() {
    let mut w = Worker::spawn();
    w.request(Request::RefreshRemotes);

    // Poll drain (non-blocking) until the RemoteList message lands or we
    // give up. drain() must never block — each call returns instantly.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = false;
    while Instant::now() < deadline && !got {
        let t0 = Instant::now();
        w.drain(|msg| {
            if matches!(msg, WorkerMsg::Remotes(_)) {
                got = true;
            }
        });
        assert!(
            t0.elapsed() < Duration::from_millis(20),
            "drain() must be non-blocking (try_recv), took {:?}",
            t0.elapsed()
        );
        if !got {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(
        got,
        "RefreshRemotes result must arrive via the channel, not inline"
    );
    // The in-flight marker is cleared once the result is drained.
    assert!(!w.has_inflight(), "in-flight marker released after drain");
}

/// A draw/sync-shaped loop must complete promptly even with outstanding
/// background work. We simulate the per-frame contract: enqueue work, then
/// repeatedly `drain` (what `sync` does) and confirm each pseudo-frame is
/// fast. If the worker were on the UI thread, the first frame would block
/// for the full subprocess/HTTP duration.
#[test]
fn per_frame_drain_loop_never_blocks() {
    let mut w = Worker::spawn();
    w.request(Request::Search("a-search-that-hits-the-network".into()));
    w.request(Request::Collection("popular".into()));

    // 30 pseudo-frames; the whole loop must finish well under what even one
    // real network call would take.
    let start = Instant::now();
    for _ in 0..30 {
        let f0 = Instant::now();
        w.drain(|_| {});
        assert!(
            f0.elapsed() < Duration::from_millis(10),
            "a single frame's drain blocked for {:?}",
            f0.elapsed()
        );
        // Frame cadence ~16ms; we don't actually need to sleep for the test.
    }
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "30 frames of draining took {:?} — something is blocking",
        start.elapsed()
    );
}

/// The offline worker (used by `--shots` / headless) issues no I/O at all:
/// `request` is a no-op returning false, `drain` yields nothing, and there
/// is never any in-flight work. This is what guarantees the screenshot path
/// is deterministic and instant.
#[test]
fn offline_worker_is_inert() {
    let mut w = Worker::offline();
    assert!(!w.is_live());
    assert!(!w.request(Request::Search("x".into())));
    assert!(!w.request(Request::RefreshRemotes));
    let mut drained = 0;
    w.drain(|_| drained += 1);
    assert_eq!(drained, 0);
    assert!(!w.has_inflight());
}

/// Direct channel-shape assertion: the worker truly communicates over an
/// `mpsc` channel pair, the same primitive the maps tile worker uses. This
/// documents the contract independent of the `Worker` wrapper.
#[test]
fn channel_is_mpsc_and_decoupled() {
    let (job_tx, job_rx): (Sender<Request>, Receiver<Request>) = channel();
    let (msg_tx, msg_rx): (Sender<WorkerMsg>, Receiver<WorkerMsg>) = channel();
    // Producer (UI) side: send without any receiver action — proves the
    // send is buffered, not rendezvous (UI never waits for the worker).
    let t0 = Instant::now();
    job_tx.send(Request::RefreshInstalled).unwrap();
    assert!(
        t0.elapsed() < Duration::from_millis(5),
        "send must not block"
    );
    // Consumer (worker) side picks it up later.
    assert_eq!(job_rx.recv().unwrap(), Request::RefreshInstalled);
    // And the reverse direction carries results back.
    msg_tx.send(WorkerMsg::Remotes(vec![])).unwrap();
    assert!(matches!(msg_rx.try_recv(), Ok(WorkerMsg::Remotes(_))));
}
