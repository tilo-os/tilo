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

/// Holds `/events` polls while paused, and counts the polls it held and the
/// sessions.
struct Gated {
    inner: Arc<support::https::DevHttps>,
    paused: Arc<AtomicBool>,
    held: Arc<AtomicUsize>,
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
            if events && self.paused.load(Ordering::Relaxed) {
                self.held.fetch_add(1, Ordering::Relaxed);
            }
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
    let held = Arc::new(AtomicUsize::new(0));
    let sessions = Arc::new(AtomicUsize::new(0));
    let observer = start_peer(
        Host::Https(Arc::new(Gated {
            inner: host.https(),
            paused: paused.clone(),
            held: held.clone(),
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

    // The observer stops reading once one of its polls is held.
    paused.store(true, Ordering::Relaxed);
    wait_for("the observer's poll to be held", || {
        let holding = held.load(Ordering::Relaxed) > 0;
        async move { holding.then_some(()) }
    })
    .await;
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
    tokio::task::spawn_blocking(move || tilo_test_env::recv("the commits to be durable", &receipt))
        .await
        .unwrap()
        .unwrap();
    eprintln!(
        "{COMMITS} x 64 KiB durable in {:?}, {} host fsyncs",
        published.elapsed(),
        tilo_relay::store::fsyncs() - fsyncs
    );

    let resumed = Instant::now();
    paused.store(false, Ordering::Relaxed);
    let mut received = Vec::new();
    wait_for("the observer to catch up", || {
        documents(&observer, &mut received);
        let done = changes(&received).len() >= COMMITS;
        async move { done.then_some(()) }
    })
    .await;
    let caught_up = resumed.elapsed();
    // The room delivers in commit order: a change sent twice would arrive
    // before the next live one.
    author
        .publish(route(), &DocumentMutation::Incremental(b"live".to_vec()))
        .unwrap();
    wait_for("a live delta", || {
        documents(&observer, &mut received);
        let done = changes(&received).iter().any(|body| body == b"live");
        async move { done.then_some(()) }
    })
    .await;
    let mut received = changes(&received);
    assert_eq!(
        received.pop().as_deref(),
        Some(&b"live"[..]),
        "the live delta arrives last"
    );
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
}
