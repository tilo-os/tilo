//! A record's retained history stays short without any device checkpointing
//! it. A cleartext route the host folds into a snapshot of its own; a sealed
//! route, which the host cannot read, a member device compacts on the host's
//! request. Either way a late joiner converges from the snapshot and the
//! entries it does not contain, and nothing written concurrently is lost.

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
use tilo_relay_protocol::Envelope;
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

/// Resident set size in MiB, 0 where it cannot be read (CI images ship no
/// `ps`; Linux reads it from procfs).
fn rss_mib() -> u64 {
    let kib = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .and_then(|value| value.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .or_else(|| {
            let out = std::process::Command::new("ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output()
                .ok()?;
            String::from_utf8_lossy(&out.stdout).trim().parse().ok()
        });
    kib.unwrap_or(0) / 1024
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

fn noise(len: usize, seed: u64) -> String {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(b"abcdefghijklmnopqrstuvwxyz"[(state % 26) as usize])
        })
        .collect()
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

/// Every envelope the host sent, in order.
fn envelopes(pages: &[Vec<u8>]) -> Vec<Envelope> {
    pages
        .iter()
        .filter_map(|page| postcard::from_bytes::<tilo_relay_protocol::web::EventsPage>(page).ok())
        .flat_map(|page| page.envelopes)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_five_megabyte_history_reaches_a_late_joiner_as_a_host_snapshot() {
    const EDITS: usize = 640;
    const VALUE: usize = 8 * 1024;
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let host_key = host_device.cert.device_public_key;
    let author_device = enrol(&root, "browser", DeviceCapabilities::member());
    let author_key = author_device.cert.device_public_key;
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    // Room for a baseline join between the fold's read and its removal.
    host.relay
        .compactor()
        .hold_before_removing(Duration::from_secs(120));
    let author = start_peer(
        Host::Https(host.https()),
        author_device.cert,
        host_device.cert.clone(),
        author_device.key,
    );
    author.join(workspace()).unwrap();
    ready(&[&author]).await;

    // 640 edits of 8 KiB each and no checkpoint: 5 MiB of history.
    let mut doc = AutoCommit::new();
    let published = Instant::now();
    for edit in 0..EDITS {
        let before = doc.get_heads();
        doc.put(ROOT, format!("k{}", edit % 64), noise(VALUE, edit as u64))
            .unwrap();
        doc.commit();
        author
            .publish(
                route(),
                &DocumentMutation::Incremental(doc.save_after(&before)),
            )
            .unwrap();
    }
    durable(&author).await;
    eprintln!(
        "5 MiB history: {EDITS} x {VALUE} B durable in {} ms, {} entries retained",
        published.elapsed().as_millis(),
        lineage_len(state.path(), &author_key)
    );
    let rss_before = rss_mib();

    let compactor = host.relay.compactor().clone();
    let heads = doc.get_heads();
    // Every fold is held before it removes anything: this joiner sees the
    // whole history.
    let (_, baseline_documents, baseline_pages, baseline_time) = join_late(
        &host,
        enrol(&root, "tablet", DeviceCapabilities::member()),
        &host_device.cert,
        None,
        &heads,
    )
    .await;
    let rss_folding = rss_mib();
    compactor.hold_before_removing(Duration::ZERO);
    let released = Instant::now();
    settle("the history to be folded", || {
        let done = lineage_len(state.path(), &author_key) == 0;
        async move { done.then_some(()) }
    })
    .await;
    let folded_in = released.elapsed();
    let rss_after = rss_mib();
    let host_entries = lineage_len(state.path(), &host_key);
    assert!(
        (1..=4).contains(&host_entries),
        "{host_entries} host entries"
    );

    let (_, documents, pages, time) = join_late(
        &host,
        enrol(&root, "phone", DeviceCapabilities::member()),
        &host_device.cert,
        None,
        &heads,
    )
    .await;
    let bytes: usize = pages.iter().map(Vec::len).sum();
    let baseline_bytes: usize = baseline_pages.iter().map(Vec::len).sum();
    eprintln!(
        "late join before compaction: {baseline_documents} documents, {baseline_bytes} B of /events, {} ms",
        baseline_time.as_millis()
    );
    eprintln!(
        "late join after compaction: {documents} documents, {bytes} B of /events, {} ms",
        time.as_millis()
    );
    eprintln!(
        "{} host folds; the last finished {} ms after the hold was released",
        compactor.rounds(),
        folded_in.as_millis()
    );
    eprintln!(
        "process RSS (host and peers in one process): {rss_before} MiB before, {rss_folding} MiB while folds were held, {rss_after} MiB after"
    );
    assert_eq!(baseline_documents, EDITS);
    assert_eq!(documents, 1, "one reassembled host snapshot");

    // The host's certificate precedes the state it signed, so a native
    // shell that joins late holds it before the snapshot arrives.
    let sent = envelopes(&pages);
    let position = |wanted: &dyn Fn(&neighbour_wire::NeighbourMsg) -> bool| {
        sent.iter().position(|envelope| {
            let Envelope::Delta {
                author_device_public_key,
                delta,
                ..
            } = envelope
            else {
                return false;
            };
            let Some(
                tilo_relay::CoeditPayload::Checkpoint { body, .. }
                | tilo_relay::CoeditPayload::RetainedDelta { body, .. },
            ) = tilo_relay::CoeditPayload::decode(delta)
            else {
                return false;
            };
            *author_device_public_key == host_key
                && postcard::from_bytes(&body).is_ok_and(|message| wanted(&message))
        })
    };
    let certificate = position(&|message| {
        matches!(message, neighbour_wire::NeighbourMsg::IdentityHello { cert, .. }
            if cert.device_public_key == host_key)
    })
    .expect("the host's certificate is replayed");
    let snapshot =
        position(&|message| matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. }))
            .expect("the host's snapshot is replayed");
    assert!(
        certificate < snapshot,
        "certificate {certificate} before snapshot {snapshot}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_made_while_the_host_folds_reaches_late_joiners() {
    const EDITS: usize = 300;
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let (a, b) = (
        enrol(&root, "browser", DeviceCapabilities::member()),
        enrol(&root, "laptop", DeviceCapabilities::member()),
    );
    let b_key = b.cert.device_public_key;
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let compactor = host.relay.compactor().clone();
    compactor.hold_before_removing(Duration::from_secs(3));
    let https = || Host::Https(host.https());
    let a = start_peer(https(), a.cert, host_device.cert.clone(), a.key);
    let b = start_peer(https(), b.cert, host_device.cert.clone(), b.key);
    a.join(workspace()).unwrap();
    b.join(workspace()).unwrap();
    ready(&[&a, &b]).await;

    let mut base = AutoCommit::new();
    base.put(ROOT, "title", "base").unwrap();
    base.commit();
    a.publish(route(), &DocumentMutation::Snapshot(base.save()))
        .unwrap();
    let mut on_a = base.fork();
    for edit in 0..EDITS {
        let before = on_a.get_heads();
        on_a.put(
            ROOT,
            format!("k{}", edit % 20),
            i64::try_from(edit).unwrap(),
        )
        .unwrap();
        on_a.commit();
        a.publish(
            route(),
            &DocumentMutation::Incremental(on_a.save_after(&before)),
        )
        .unwrap();
    }
    durable(&a).await;
    settle("the fold to read the channel", || {
        let started = compactor.folds_started() > 0;
        async move { started.then_some(()) }
    })
    .await;
    // B writes after the host read the channel and before it removes.
    let mut on_b = base.fork();
    on_b.put(ROOT, "from_b", "concurrent").unwrap();
    on_b.commit();
    b.publish(
        route(),
        &DocumentMutation::Incremental(on_b.save_after(&base.get_heads())),
    )
    .unwrap();
    durable(&b).await;
    assert_eq!(compactor.rounds(), 0, "the edit landed during the fold");
    settle("the host snapshot", || {
        let done = compactor.rounds() > 0;
        async move { done.then_some(()) }
    })
    .await;
    assert_eq!(
        lineage_len(state.path(), &b_key),
        1,
        "the concurrent edit is kept"
    );

    let mut merged = on_a.fork();
    merged.merge_incoming(&mut on_b.fork()).unwrap();
    let heads = merged.get_heads();
    let (replica, documents, _, _) = join_late(
        &host,
        enrol(&root, "phone", DeviceCapabilities::member()),
        &host_device.cert,
        None,
        &heads,
    )
    .await;
    assert!(
        documents <= 3,
        "{documents} documents instead of {}",
        EDITS + 2
    );
    let from_b = replica
        .get(ROOT, "from_b")
        .unwrap()
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|scalar| scalar.into_string().ok());
    assert_eq!(from_b.as_deref(), Some("concurrent"));
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
