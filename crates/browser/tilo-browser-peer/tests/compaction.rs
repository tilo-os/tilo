//! A record's retained history stays short without any device checkpointing
//! it. A browser only writes sealed routes, which the host cannot read, so a
//! member device compacts them on the host's request, and a late joiner
//! converges from the snapshot and the entries it does not contain.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::future_not_send,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
use collab_types::{DocumentMutation, document_channel_id};
use tilo_automerge::ApplyIncoming as _;
use tilo_browser_peer::{ContentCipher, Host, PeerEvent, PeerHandle};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, ready, route, start_host, start_peer, workspace};

fn channel() -> [u8; 32] {
    *blake3::hash(b"app\0so.tilo.notes\0note-1").as_bytes()
}

/// Retained entries of `author` for the note's route.
fn lineage_len(state: &Path, author: &[u8; 32]) -> usize {
    let lineage = state
        .join("workspaces")
        .join(workspace().to_string())
        .join("retained/v2")
        .join(format!(
            "{}-{}",
            hex::encode(author),
            hex::encode(channel())
        ));
    let Ok(name) = std::fs::read_to_string(lineage.join("CURRENT")) else {
        return 0;
    };
    std::fs::read_dir(lineage.join(name.trim())).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".delta"))
            .count()
    })
}

fn apply(doc: &mut AutoCommit, mutation: DocumentMutation) {
    match mutation {
        DocumentMutation::Snapshot(bytes) => {
            doc.merge_incoming(&mut AutoCommit::load(&bytes).unwrap())
                .unwrap();
        }
        DocumentMutation::Incremental(bytes) => {
            doc.load_incoming(&bytes).unwrap();
        }
    }
}

/// `support::wait_for` with room for phases that move megabytes.
async fn settle<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = Instant::now();
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(
            started.elapsed() < Duration::from_secs(180),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn durable(peer: &PeerHandle) {
    let receipt = peer.barrier().unwrap();
    settle("durable", || async { receipt.try_recv().ok() })
        .await
        .unwrap();
}

/// An HTTPS client that keeps every `/events` answer.
struct Pages {
    inner: Arc<support::https::DevHttps>,
    pages: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl tilo_browser_peer::HttpClient for Pages {
    fn post(
        &self,
        path: &str,
        session: Option<&str>,
        body: Vec<u8>,
    ) -> tilo_browser_peer::HttpFuture<'_> {
        let events = path.ends_with("/events");
        let answer = self.inner.post(path, session, body);
        Box::pin(async move {
            let answer = answer.await;
            if events && let Ok((_, bytes)) = &answer {
                self.pages.lock().unwrap().push(bytes.clone());
            }
            answer
        })
    }
}

/// Join late and merge the note until its heads are `target`; returns the
/// replica, the documents received, the `/events` bytes and the duration.
async fn join_late(
    host: &support::TestHost,
    device: support::Device,
    host_cert: &tilo_user_identity::DeviceCert,
    cipher: Option<Box<dyn ContentCipher>>,
    target: &[automerge::ChangeHash],
) -> (AutoCommit, usize, Vec<Vec<u8>>, Duration) {
    let pages = Arc::new(Mutex::new(Vec::new()));
    let started = Instant::now();
    let joiner = start_peer(
        Host::Https(Arc::new(Pages {
            inner: host.https(),
            pages: pages.clone(),
        })),
        device.cert,
        host_cert.clone(),
        device.key,
    );
    if let Some(cipher) = cipher {
        joiner.install_cipher(route(), cipher).unwrap();
    }
    joiner.join(workspace()).unwrap();
    let mut replica = AutoCommit::new();
    let mut documents = 0usize;
    settle("the late joiner to converge", || {
        for event in joiner.drain() {
            if let PeerEvent::Document(document) = event {
                documents = documents.saturating_add(1);
                apply(&mut replica, document.mutation().unwrap());
            }
        }
        let done = replica.get_heads() == target;
        async move { done.then_some(()) }
    })
    .await;
    let elapsed = started.elapsed();
    joiner.close();
    let pages = pages.lock().unwrap().clone();
    (replica, documents, pages, elapsed)
}

/// A route's content keys as the page's cipher, as a member holds them.
struct Keys(channel_crypto::ChannelContentKeys);

impl ContentCipher for Keys {
    fn seal(
        &mut self,
        artifact: collab_types::ArtifactId,
        retention: collab_types::ContentRetention,
        cleartext: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        self.0
            .seal_content(artifact, retention, cleartext)
            .map_err(|error| error.to_string())
    }

    fn open(
        &mut self,
        artifact: collab_types::ArtifactId,
        metadata: &collab_types::EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.0
            .open_content(artifact, metadata, payload)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }
}

/// One member's app: its replica of the note, merged from every document
/// it was delivered, answering compaction requests with it.
struct Member {
    peer: PeerHandle,
    replica: AutoCommit,
    answered: usize,
}

impl Member {
    /// Apply what arrived; returns the compaction requests seen.
    fn pump(&mut self) -> Vec<[u8; 16]> {
        let mut requests = Vec::new();
        for event in self.peer.drain() {
            match event {
                PeerEvent::Document(document) => {
                    apply(&mut self.replica, document.mutation().unwrap());
                }
                PeerEvent::CompactionRequested { request, .. } => requests.push(request),
                _ => {}
            }
        }
        requests
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_compacts_a_sealed_route_the_host_cannot_read() {
    const EDITS: usize = 300;
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::primary());
    let (a, b, c) = (
        enrol(&root, "browser", DeviceCapabilities::member()),
        enrol(&root, "tablet", DeviceCapabilities::member()),
        enrol(&root, "desktop", DeviceCapabilities::member()),
    );
    let (a_key, c_key) = (a.cert.device_public_key, c.cert.device_public_key);
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let route = route();
    let steward_dir = tempfile::tempdir().unwrap();
    let steward = channel_crypto::ChannelSteward::open_or_create(
        steward_dir.path(),
        document_channel_id(&route.app_id, &route.cluster_id, &route.record),
        laptop.key.clone(),
        &channel_crypto::VaultKey::new([7; 32]),
    )
    .unwrap();
    let keys = || -> Box<dyn ContentCipher> { Box::new(Keys(steward.content_keys().unwrap())) };
    let https = || Host::Https(host.https());
    let mut members: Vec<Member> = [a, b]
        .into_iter()
        .map(|device| {
            let peer = start_peer(https(), device.cert, host_device.cert.clone(), device.key);
            peer.install_cipher(route.clone(), keys()).unwrap();
            peer.join(workspace()).unwrap();
            Member {
                peer,
                replica: AutoCommit::new(),
                answered: 0,
            }
        })
        .collect();
    let c = start_peer(https(), c.cert, host_device.cert.clone(), c.key);
    c.install_cipher(route.clone(), keys()).unwrap();
    c.join(workspace()).unwrap();
    ready(&[&members[0].peer, &members[1].peer, &c]).await;

    // A writes a sealed history the host holds only as ciphertext.
    {
        let a = &mut members[0];
        a.replica.put(ROOT, "title", "sealed").unwrap();
        a.replica.commit();
        a.peer
            .publish(route.clone(), &DocumentMutation::Snapshot(a.replica.save()))
            .unwrap();
        for edit in 0..EDITS {
            let before = a.replica.get_heads();
            a.replica
                .put(
                    ROOT,
                    format!("k{}", edit % 20),
                    i64::try_from(edit).unwrap(),
                )
                .unwrap();
            a.replica.commit();
            a.peer
                .publish(
                    route.clone(),
                    &DocumentMutation::Incremental(a.replica.save_after(&before)),
                )
                .unwrap();
        }
        durable(&a.peer).await;
    }
    let before_compaction = lineage_len(state.path(), &a_key);
    assert!(
        before_compaction > EDITS,
        "{before_compaction} sealed entries"
    );

    // Whoever the host asks first sees C write concurrently, then answers.
    let mut c_doc = AutoCommit::new();
    let mut concurrent = None;
    let asked = Instant::now();
    settle("a member to answer", || {
        let mut answered = false;
        for member in &mut members {
            for request in member.pump() {
                if concurrent.is_none() {
                    c_doc.put(ROOT, "from_c", "concurrent").unwrap();
                    c_doc.commit();
                    let bytes = c_doc.save_incremental();
                    c.publish(route.clone(), &DocumentMutation::Incremental(bytes))
                        .unwrap();
                    concurrent = Some(());
                }
                member.peer.compact(request, member.replica.save()).unwrap();
                member.answered += 1;
                answered = true;
            }
        }
        async move { answered.then_some(()) }
    })
    .await;
    durable(&c).await;
    let compactor = host.relay.compactor().clone();
    settle("the host to accept the statement", || {
        for member in &mut members {
            member.pump();
        }
        let done = compactor.rounds() > 0;
        async move { done.then_some(()) }
    })
    .await;
    let volunteer = members
        .iter()
        .position(|member| member.answered > 0)
        .unwrap();
    let after_compaction = lineage_len(state.path(), &a_key);
    eprintln!(
        "sealed route: {before_compaction} entries of A before, {after_compaction} after; member {volunteer} answered {} ms after A's history was durable",
        asked.elapsed().as_millis()
    );
    assert!(
        after_compaction <= 3,
        "A's covered history is gone ({after_compaction} left; the volunteer's snapshot may be A's)"
    );
    assert_eq!(
        lineage_len(state.path(), &c_key),
        1,
        "C's uncovered edit is kept"
    );

    let mut merged = members[0].replica.fork();
    merged.merge_incoming(&mut c_doc.fork()).unwrap();
    let heads = merged.get_heads();
    let (replica, documents, _, _) = join_late(
        &host,
        enrol(&root, "phone", DeviceCapabilities::member()),
        &host_device.cert,
        Some(keys()),
        &heads,
    )
    .await;
    assert!(
        documents <= 3,
        "{documents} documents instead of {}",
        EDITS + 2
    );
    assert!(replica.get(ROOT, "from_c").unwrap().is_some());
}
