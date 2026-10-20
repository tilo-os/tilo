//! A browser that stops reading keeps its session: the host holds a bounded
//! number of event bytes for it, leaves the rest on disk while it lags, and
//! resumes its room from its position once it reads again.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives as long as the scenario"
)]

#[path = "support/steward.rs"]
mod steward;
mod support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use collab_types::DocumentMutation;
use tilo_browser_peer::{Host, HttpClient, HttpFuture, PeerEvent, PeerHandle};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{admitted, enrol, ready, route, start_host, start_peer, wait_for, workspace};

/// Holds `/events` polls while paused and counts sessions.
struct Gated {
    inner: Arc<support::https::DevHttps>,
    paused: Arc<AtomicBool>,
    sessions: Arc<AtomicUsize>,
}

impl HttpClient for Gated {
    fn post(&self, path: &str, session: Option<&str>, body: Vec<u8>) -> HttpFuture<'_> {
        if path.ends_with("/session/finish") {
            self.sessions.fetch_add(1, Ordering::Relaxed);
        }
        let events = path.ends_with("/events");
        let path = path.to_owned();
        let session = session.map(str::to_owned);
        Box::pin(async move {
            while events && self.paused.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            self.inner.post(&path, session.as_deref(), body).await
        })
    }
}

const CHANGE: usize = 64 * 1024;

fn documents(peer: &PeerHandle, into: &mut Vec<DocumentMutation>) {
    into.extend(peer.drain().into_iter().filter_map(|event| match event {
        PeerEvent::Document(document) => document.mutation(),
        _ => None,
    }));
}

/// The published changes in `received`: the peer joins a run of incremental
/// changes that arrive together into one, so split them back apart.
fn changes(received: &[DocumentMutation]) -> Vec<Vec<u8>> {
    received
        .iter()
        .flat_map(|mutation| {
            let DocumentMutation::Incremental(body) = mutation else {
                panic!("an incremental change");
            };
            body.chunks(CHANGE).map(<[u8]>::to_vec).collect::<Vec<_>>()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario: a flood past the session budget, the catch-up, then live delivery"
)]
async fn a_browser_that_stops_reading_lags_behind_its_queue_and_keeps_its_session() {
    // More than the host holds for a session (16 MiB) plus its 4 MiB duplex.
    const COMMITS: usize = 400;
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let author = enrol(&root, "browser", DeviceCapabilities::member());
    let observer = enrol(&root, "phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;

    let author = start_peer(
        Host::Https(host.https()),
        author.cert,
        host_device.cert.clone(),
        author.key,
    );
    let paused = Arc::new(AtomicBool::new(false));
    let sessions = Arc::new(AtomicUsize::new(0));
    let observer = start_peer(
        Host::Https(Arc::new(Gated {
            inner: host.https(),
            paused: paused.clone(),
            sessions: sessions.clone(),
        })),
        observer.cert,
        host_device.cert.clone(),
        observer.key,
    );
    author.join(workspace()).unwrap();
    observer.join(workspace()).unwrap();
    ready(&[&author, &observer]).await;
    // Both hold the channel's keys before the flood: what follows measures
    // the host's queue, not admission.
    author.open_route(route()).unwrap();
    observer.open_route(route()).unwrap();
    admitted(&author).await;
    admitted(&observer).await;

    paused.store(true, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let tail: Vec<DocumentMutation> = (0..COMMITS)
        .map(|index| {
            let mut body = vec![u8::try_from(index % 251).unwrap(); CHANGE];
            body[..8].copy_from_slice(&(index as u64).to_le_bytes());
            DocumentMutation::Incremental(body)
        })
        .collect();
    let fsyncs = tilo_relay::store::fsyncs();
    for mutation in &tail {
        author.publish(route(), mutation).unwrap();
    }
    let receipt = author.barrier().unwrap();
    let published = Instant::now();
    // The host ends a session nobody polled for 45 s; stop pausing well
    // before that however slowly the commits become durable.
    let unpause = paused.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        unpause.store(false, Ordering::Relaxed);
    });
    // Every commit is fsynced into the retained log and each lagging
    // subscriber's pending queue, which outlasts `wait_for` on some disks.
    tokio::task::spawn_blocking(move || receipt.recv_timeout(Duration::from_secs(90)))
        .await
        .unwrap()
        .expect("the commits to be durable")
        .unwrap();
    eprintln!(
        "{COMMITS} x 64 KiB durable in {:?}, {} host fsyncs",
        published.elapsed(),
        tilo_relay::store::fsyncs() - fsyncs
    );

    let resumed = Instant::now();
    paused.store(false, Ordering::Relaxed);
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while changes(&received).len() < COMMITS {
        documents(&observer, &mut received);
        assert!(
            Instant::now() < deadline,
            "{} of {COMMITS} changes, {} sessions",
            changes(&received).len(),
            sessions.load(Ordering::Relaxed)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let caught_up = resumed.elapsed();
    tokio::time::sleep(Duration::from_millis(300)).await;
    documents(&observer, &mut received);
    let received = changes(&received);
    assert_eq!(received.len(), COMMITS, "none repeated");
    let indices: BTreeSet<usize> = received
        .iter()
        .map(|body| {
            let index = usize::try_from(u64::from_le_bytes(body[..8].try_into().unwrap())).unwrap();
            assert!(
                DocumentMutation::Incremental(body.clone()) == tail[index],
                "delivered as published"
            );
            index
        })
        .collect();
    assert_eq!(indices, (0..COMMITS).collect(), "none lost");
    assert_eq!(
        sessions.load(Ordering::Relaxed),
        1,
        "the observer kept its session"
    );
    eprintln!("lagging observer: {COMMITS} x 64 KiB caught up in {caught_up:?} on one session");

    author
        .publish(route(), &DocumentMutation::Incremental(b"live".to_vec()))
        .unwrap();
    let mut live = Vec::new();
    wait_for("a live delta", || {
        documents(&observer, &mut live);
        let done = !live.is_empty();
        async move { done.then_some(()) }
    })
    .await;
    assert_eq!(live, vec![DocumentMutation::Incremental(b"live".to_vec())]);
}
