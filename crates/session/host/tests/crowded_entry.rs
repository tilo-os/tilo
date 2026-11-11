//! A fresh guest entering a crowded workspace through its door, as a new
//! visitor's page enters the Lounge: the workspace has hundreds of members whose
//! devices are in its channel group and megabytes of records, and the guest's
//! page reaches the host only through its packet relay. What a person waits for
//! is the desktop: the page is in when the host admitted it, its governance is
//! here and the shared desktop record opened.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::significant_drop_tightening,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

tilo_automerge_isolate::test_worker!();

use std::time::{Duration, Instant};

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session_host::device::{Entering, door_entry};
use tilo_session_host::{Host, MemoryPeerStore, PeerEvent, PeerHandle, TransportPhase};
use tilo_user_identity::UserIdentity;

use support::{enrol, enter_page, start_page, start_workspace_host_relaying, wait_for};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// The shared desktop's first state: an automerge document carrying `filler`
/// bytes, as a desktop of many windows does.
fn desktop_base(filler: usize) -> automerge::AutoCommit {
    record_of(filler, 1)
}

/// A record of `size` bytes: an automerge document, as every app's is.
fn record_of(size: usize, seed: u64) -> automerge::AutoCommit {
    use automerge::transaction::Transactable as _;
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "filler", noise(size, seed))
        .unwrap();
    doc
}

/// One member device's own part of the shared desktop, written over `base`:
/// an incremental change of `size` bytes in an entry of its own.
fn desktop_part(base: &mut automerge::AutoCommit, member: usize, size: usize) -> Vec<u8> {
    use automerge::transaction::Transactable as _;
    let mut doc = base.fork();
    doc.save_incremental();
    doc.put(
        automerge::ROOT,
        format!("member-{member}"),
        noise(size, member as u64 + 10_000),
    )
    .unwrap();
    doc.save_incremental()
}

/// The host's HTTPS as the guest's page reaches it: each request takes a
/// network round trip of `round_trip` beside what the host spends on it, and
/// is counted with what it asked.
struct Network<'a> {
    inner: &'a dyn tilo_session_host::HttpClient,
    round_trip: Duration,
    asked: std::sync::Mutex<Vec<(String, Duration, usize)>>,
}

impl tilo_session_host::HttpClient for Network<'_> {
    fn post(&self, path: &str, body: Vec<u8>) -> tilo_session_host::HttpFuture<'_> {
        let path = path.to_owned();
        let sent = body.len();
        Box::pin(async move {
            let started = Instant::now();
            tokio::time::sleep(self.round_trip / 2).await;
            let answer = self.inner.post(&path, body).await;
            tokio::time::sleep(self.round_trip / 2).await;
            let received = answer.as_ref().map_or(0, |(_, body)| body.len());
            self.asked
                .lock()
                .unwrap()
                .push((path, started.elapsed(), sent + received));
            answer
        })
    }
}

/// A network between the page and the host's packet relay: every byte each way
/// arrives `round_trip / 2` after it was sent, in order, and the bytes that
/// reach the page are counted. A page holds no UDP socket, so this is all of
/// its session's path to the host.
struct Link {
    addr: std::net::SocketAddr,
    received: std::sync::Arc<std::sync::atomic::AtomicU64>,
    sent: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Link {
    async fn start(upstream: std::net::SocketAddr, round_trip: Duration) -> Self {
        use std::sync::atomic::AtomicU64;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = std::sync::Arc::new(AtomicU64::new(0));
        let sent = std::sync::Arc::new(AtomicU64::new(0));
        let (down, up) = (received.clone(), sent.clone());
        tokio::spawn(async move {
            while let Ok((page, _)) = listener.accept().await {
                let Ok(relay) = tokio::net::TcpStream::connect(upstream).await else {
                    continue;
                };
                let _ = page.set_nodelay(true);
                let _ = relay.set_nodelay(true);
                let (page_read, page_write) = page.into_split();
                let (relay_read, relay_write) = relay.into_split();
                tokio::spawn(Self::carry(
                    page_read,
                    relay_write,
                    round_trip / 2,
                    up.clone(),
                ));
                tokio::spawn(Self::carry(
                    relay_read,
                    page_write,
                    round_trip / 2,
                    down.clone(),
                ));
            }
        });
        Self {
            addr,
            received,
            sent,
        }
    }

    /// Carry `from` to `to`, each chunk `delay` after it was read.
    async fn carry(
        mut from: tokio::net::tcp::OwnedReadHalf,
        mut to: tokio::net::tcp::OwnedWriteHalf,
        delay: Duration,
        counted: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let (queue, mut due) =
            tokio::sync::mpsc::unbounded_channel::<(tokio::time::Instant, Vec<u8>)>();
        let writer = tokio::spawn(async move {
            while let Some((at, bytes)) = due.recv().await {
                tokio::time::sleep_until(at).await;
                if to.write_all(&bytes).await.is_err() {
                    return;
                }
            }
            let _ = to.shutdown().await;
        });
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match from.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    counted.fetch_add(read as u64, std::sync::atomic::Ordering::Relaxed);
                    let at = tokio::time::Instant::now() + delay;
                    if queue.send((at, buffer[..read].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
        drop(queue);
        let _ = writer.await;
    }

    fn bytes(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering;
        (
            self.received.load(Ordering::Relaxed),
            self.sent.load(Ordering::Relaxed),
        )
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Whether `page`'s channel for `route` is ready.
fn keyed(page: &PeerHandle, route: &DocumentRoute) -> bool {
    let prefix = format!("route {}/{} ", route.app_id, route.record);
    page.status()
        .channels
        .lines()
        .any(|line| line.starts_with(&prefix) && line.contains(": ready;"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_fresh_guest_enters_a_crowded_workspace() {
    let members = env_usize("TILO_CROWD_MEMBERS", 300);
    let online = env_usize("TILO_CROWD_KEYED", 1);
    let record_bytes = env_usize("TILO_CROWD_BYTES", 3_500_000);
    let (owner, _) = UserIdentity::generate("Owner", 1);
    // The Lounge's host is certified as a hosted workspace host is: it owns its
    // workspaces and takes part in their channels, so it admits each page.
    let host_device = enrol(
        &owner,
        "lounge-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = support::scratch_dir();
    let host = start_workspace_host_relaying(&owner, &host_device, state.path(), false, true).await;
    let workspaces = host.relay.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    workspaces
        .publish(
            &lounge.workspace,
            &host_device.cert,
            tilo_relay_protocol::directory::Listing {
                description: "Everyone".into(),
                default_role: tilo_relay_protocol::workspaces::WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let https = host.https();
    let address = host.relay.node_addr();
    let relay_url = address.relay_urls().next().cloned();
    let page_host = || Host::through(iroh::EndpointAddr::new(address.id), relay_url.clone());
    let desktop = DocumentRoute {
        app_id: "so.tilo.browser-desktop".into(),
        cluster_id: lounge.workspace.clone(),
        record: "desktop".into(),
    };

    // The crowd: people who came through the door, each a device of their own,
    // a few at a time as people arrive.
    let seeded = Instant::now();
    let mut crowd = Vec::new();
    let mut arriving = tokio::task::JoinSet::new();
    for n in 0..members {
        if arriving.len() >= 10
            && let Some(entered) = arriving.join_next().await
        {
            crowd.push(entered.unwrap());
        }
        let host_cert = host_device.cert.clone();
        let workspace = lounge.workspace.clone();
        let doors = workspaces.clone();
        arriving.spawn(async move {
            let (key, cert) = through_the_door(&doors, &workspace, &format!("Member {n}")).await;
            (key, cert, host_cert)
        });
    }
    while let Some(entered) = arriving.join_next().await {
        crowd.push(entered.unwrap());
    }
    eprintln!(
        "seeded {members} members through the door in {:?}",
        seeded.elapsed()
    );

    // The first member writes the workspace's records: the shared desktop and
    // a spread of others, `record_bytes` in all.
    // The members still here when the guest arrives: the first, who wrote.
    let mut staying: Vec<PeerHandle> = Vec::new();
    let mut base = desktop_base(64 * 1024);
    let keyed_at = Instant::now();
    for (index, chunk) in crowd
        .iter()
        .take(online)
        .collect::<Vec<_>>()
        .chunks(100)
        .enumerate()
    {
        let batch: Vec<PeerHandle> = chunk
            .iter()
            .map(|(key, cert, host_cert)| {
                let page = start_page(
                    page_host(),
                    cert.clone(),
                    host_cert.clone(),
                    key.clone(),
                    std::sync::Arc::new(MemoryPeerStore::default()),
                );
                page.join(lounge.workspace_id).unwrap();
                page.open_route(desktop.clone()).unwrap();
                page
            })
            .collect();
        let waiting = Instant::now();
        while !batch.iter().all(|page| keyed(page, &desktop)) {
            assert!(
                waiting.elapsed() < Duration::from_secs(240),
                "batch {index} never keyed; a page's channels:\n{}",
                batch
                    .iter()
                    .find(|page| !keyed(page, &desktop))
                    .map(|page| page.status().channels)
                    .unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        eprintln!("batch {index} keyed in {:?}", waiting.elapsed());
        if index == 0 {
            let author = &batch[0];
            author
                .publish(desktop.clone(), &DocumentMutation::Snapshot(base.save()))
                .unwrap();
            let records = record_bytes / (100 * 1024);
            for record in 0..records {
                let route = DocumentRoute {
                    app_id: "so.tilo.notes".into(),
                    cluster_id: lounge.workspace.clone(),
                    record: format!("note-{record}"),
                };
                author.open_route(route.clone()).unwrap();
                author
                    .publish(
                        route,
                        &DocumentMutation::Snapshot(
                            record_of(100 * 1024, record as u64 + 7).save(),
                        ),
                    )
                    .unwrap();
            }
        }
        // Every member's device writes its own part of the shared desktop.
        for (offset, page) in batch.iter().enumerate() {
            page.publish(
                desktop.clone(),
                &DocumentMutation::Incremental(desktop_part(&mut base, index * 100 + offset, 600)),
            )
            .unwrap();
        }
        for page in &batch {
            let receipt = page.barrier().unwrap();
            wait_for("the batch's writes durable", || async {
                receipt.try_recv().ok()
            })
            .await
            .unwrap();
        }
        // Most of the crowd is away: their pages close once keyed.
        for (offset, page) in batch.into_iter().enumerate() {
            if index == 0 && offset == 0 {
                staying.push(page);
            } else {
                page.close();
            }
        }
    }
    eprintln!("keyed {online} member devices in {:?}", keyed_at.elapsed());

    // A fresh guest, first over a network that costs nothing, then over one
    // whose every round trip takes `ROUND_TRIP`: the difference is how many
    // round trips the guest waited through, one after another.
    let place = Place {
        https: &*https,
        host_cert: &host_device.cert,
        workspace: &lounge.workspace,
        workspace_id: lounge.workspace_id,
        host: address.id,
        relay: relay_url.clone().expect("the host runs a packet relay"),
        desktop: &desktop,
    };
    let quick = guest_enters(&place, Duration::ZERO).await;
    quick.report("no delay");
    let slow = guest_enters(&place, ROUND_TRIP).await;
    slow.report("round trip");
    let waited = slow.shown.saturating_sub(quick.shown);
    let round_trips = waited.as_secs_f64() / ROUND_TRIP.as_secs_f64();
    eprintln!(
        "ENTRY round trips: {round_trips:.1} ({} HTTP requests); page received {} B, sent {} B",
        slow.http.len(),
        slow.received,
        slow.sent
    );
    // The budget: a regression that adds a step in sequence, sends the whole
    // workspace before the desktop, or asks the host again fails here.
    assert!(
        round_trips <= MAX_ROUND_TRIPS,
        "a fresh guest waited {round_trips:.1} round trips for the desktop, more than {MAX_ROUND_TRIPS}"
    );
    assert!(
        slow.http.len() <= MAX_REQUESTS,
        "a fresh guest asked the host {} times over HTTPS, more than {MAX_REQUESTS}",
        slow.http.len()
    );
    for entry in [&quick, &slow] {
        assert!(
            entry.received <= MAX_BYTES,
            "a fresh guest received {} B before its desktop showed, more than {MAX_BYTES} B",
            entry.received
        );
    }
    for member in staying {
        member.close();
    }
}

/// A person coming through `workspace`'s door with a device of their own, as the host's door
/// and the workspace's join take them, in process: the crowd that came before leaves no session
/// behind it, only the members it made.
async fn through_the_door(
    workspaces: &tilo_relay::workspaces::Workspaces,
    workspace: &str,
    name: &str,
) -> (SigningKey, tilo_user_identity::DeviceCert) {
    use ed25519_dalek::Signer as _;
    use tilo_relay_protocol::workspaces::{JoinReply, JoinStep, join_proof_digest};
    let root = new_key();
    let key = new_key();
    let cert = tilo_relay_client::invite::guest_certificate(
        &root,
        key.verifying_key().to_bytes(),
        "Browser",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let (issued, token) = workspaces
        .admit_listed(
            workspace,
            &tilo_relay_protocol::directory::DoorJoin {
                user_public_key: hex::encode(cert.user_public_key),
                display_name: name.to_owned(),
            },
        )
        .await
        .unwrap();
    let JoinReply::Challenge { challenge } = workspaces
        .page_join(JoinStep::Ask {
            invitation: issued.signed_invitation,
            device_cert: cert.clone(),
            request_id: rand::random(),
        })
        .await
    else {
        panic!("{name} is not challenged");
    };
    let signature = key.sign(&join_proof_digest(&challenge)).to_bytes();
    let admitted = workspaces
        .page_join(JoinStep::ProvePreAdmitted {
            challenge,
            signature,
            name: name.to_owned(),
            token: token.into_bytes(),
        })
        .await;
    assert!(
        matches!(admitted, JoinReply::Admitted { .. }),
        "{admitted:?}"
    );
    (key, cert)
}

/// What every guest enters: the host's door and session, and the record it
/// shows first.
struct Place<'a> {
    https: &'a dyn tilo_session_host::HttpClient,
    /// The host's certificate, as it answers who it is.
    host_cert: &'a tilo_user_identity::DeviceCert,
    workspace: &'a str,
    workspace_id: u64,
    host: iroh::EndpointId,
    relay: iroh::RelayUrl,
    desktop: &'a DocumentRoute,
}

/// The network's round trip for the guest that counts them.
const ROUND_TRIP: Duration = Duration::from_millis(400);

/// How many round trips a fresh guest may wait, one after another, from its
/// first request until its desktop shows: the host's certificate over HTTPS,
/// the session's handshake through the packet relay, which carries the entry
/// through the door with it, and the replay of the workspace's governance and
/// desktop, which a new connection's congestion window takes several round
/// trips to carry. It waits about 7; the host's own work for each entry varies
/// between the two guests and moves the count by about one.
const MAX_ROUND_TRIPS: f64 = 8.5;

/// The HTTPS requests a fresh guest makes to enter: the host's certificate.
/// The door, the workspace's challenge and the admission ride the session.
const MAX_REQUESTS: usize = 1;

/// What a fresh guest may receive before its desktop shows. Its keys to the
/// desktop come only after the workspace's eager records (3.5 MB here) and its
/// governance: a compressed base with the changes since, about 0.2 to 0.5 MB
/// for 300 members, where the uncompressed history was 1.4 MB. So this bounds
/// the whole workspace once over: a frame per record, governance sent twice or
/// uncompressed, or records sent again cross it.
const MAX_BYTES: u64 = 4_500_000;

/// One guest's entry, as it went.
struct Entry {
    /// Each step, and when it was done after the guest began.
    marks: Vec<(&'static str, Duration)>,
    /// When the desktop could show: the page held its keys and its record.
    shown: Duration,
    /// Each HTTP request: its path, how long it took, its bytes both ways.
    http: Vec<(String, Duration, usize)>,
    /// What the page's session received and sent until then, and over HTTP.
    received: u64,
    sent: u64,
}

impl Entry {
    fn report(&self, network: &str) {
        for (path, took, bytes) in &self.http {
            eprintln!(
                "ENTRY [{network}] HTTP {:>8.1} ms {bytes:>7} B {path}",
                took.as_secs_f64() * 1000.0
            );
        }
        for (what, at) in &self.marks {
            eprintln!(
                "ENTRY [{network}] {:>8.1} ms  {what}",
                at.as_secs_f64() * 1000.0
            );
        }
        eprintln!(
            "ENTRY [{network}] shown at {:.1} ms; received {} B, sent {} B",
            self.shown.as_secs_f64() * 1000.0,
            self.received,
            self.sent
        );
    }
}

/// A new visitor's page entering `place` through its door, over a network
/// whose round trip is `round_trip`, until it shows the desktop.
async fn guest_enters(place: &Place<'_>, round_trip: Duration) -> Entry {
    let upstream = place
        .relay
        .socket_addrs(|| None)
        .unwrap()
        .into_iter()
        .next()
        .expect("the packet relay's address");
    let link = Link::start(upstream, round_trip).await;
    let mut relay: url::Url = (*place.relay).clone();
    relay.set_port(Some(link.addr.port())).unwrap();
    let network = Network {
        inner: place.https,
        round_trip,
        asked: std::sync::Mutex::default(),
    };
    let started = Instant::now();
    let mut marks: Vec<(&'static str, Duration)> = Vec::new();
    let root = new_key();
    let key = new_key();
    // The page asks the host who it is, the one HTTPS request of its entry; its session enters
    // through the door as it opens.
    let info = tilo_relay_client::https::host_info(&network).await.unwrap();
    assert_eq!(&info.host_cert, place.host_cert);
    let (cert, entry) = door_entry(
        &info.host_cert,
        place.workspace,
        &key,
        Entering {
            held: None,
            root: Some(&root),
            label: "Browser",
            name: "Fresh visitor",
            token: None,
        },
    )
    .await
    .unwrap();
    marks.push(("host asked and certificate made", started.elapsed()));
    let page = enter_page(
        // Only through the relay, as a page reaches its host: it holds no UDP
        // socket, so its session never takes a direct path.
        Host::through(iroh::EndpointAddr::new(place.host), Some(relay.into()))
            .over(tilo_session_host::Carrier::Relay),
        cert,
        info.host_cert,
        key,
        std::sync::Arc::new(MemoryPeerStore::default()),
        entry,
    );
    page.join(place.workspace_id).unwrap();
    page.open_route(place.desktop.clone()).unwrap();
    let (mut connected, mut admitted, mut ready, mut keyed_desktop, mut desktop_doc) =
        (None, None, None, None, None);
    let deadline = Instant::now() + Duration::from_secs(300);
    while desktop_doc.is_none() || keyed_desktop.is_none() {
        let status = page.status();
        if connected.is_none() && status.phase == TransportPhase::Connected && status.authenticated
        {
            connected = Some(started.elapsed());
        }
        if admitted.is_none() {
            match &status.entry {
                Some(tilo_session_host::Entry::Admitted { workspace, .. }) => {
                    assert_eq!(workspace, place.workspace);
                    admitted = Some(started.elapsed());
                }
                Some(refused) => panic!("the door did not let the guest in: {refused:?}"),
                None => {}
            }
        }
        if ready.is_none() && status.workspaces.get(&place.workspace_id) == Some(&true) {
            ready = Some(started.elapsed());
        }
        if keyed_desktop.is_none() && keyed(&page, place.desktop) {
            keyed_desktop = Some(started.elapsed());
        }
        for event in page.drain() {
            if let PeerEvent::Document(document) = &event
                && document.route == *place.desktop
            {
                desktop_doc.get_or_insert_with(|| started.elapsed());
            }
        }
        assert!(
            Instant::now() < deadline,
            "the guest never had the desktop: {}",
            page.status().channels
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let shown = started.elapsed();
    let (received, sent) = link.bytes();
    marks.push(("session connected", connected.unwrap_or_default()));
    marks.push(("door admitted the guest", admitted.unwrap_or_default()));
    marks.push(("host admitted the subscription", ready.unwrap_or(shown)));
    marks.push(("desktop keyed", keyed_desktop.unwrap_or_default()));
    marks.push(("desktop record", desktop_doc.unwrap_or_default()));
    page.close();
    let http = std::mem::take(&mut *network.asked.lock().unwrap());
    let http_bytes: usize = http.iter().map(|(_, _, bytes)| bytes).sum();
    Entry {
        marks,
        shown,
        http,
        received: received + http_bytes as u64,
        sent,
    }
}
