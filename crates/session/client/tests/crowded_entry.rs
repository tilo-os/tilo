//! A fresh guest entering a crowded workspace through its door, as a new
//! visitor's page enters the Lounge: the workspace has hundreds of members whose
//! devices are in its channel group and megabytes of records. What a person
//! waits for is the desktop: the page is in when the host admitted it, its
//! governance is here and the shared desktop record opened.
//!
//! A page reaches its host over the browser's WebTransport, so that is the
//! way measured first: the native client opens its session as a browser's
//! does, over a network that delays every datagram. A native device reaches
//! its host over iroh, through the host's packet relay whenever no direct
//! path opens; that way is measured beside it, with its own budget.
//!
//! What a browser does beyond the native client, and why the round trips
//! counted here are still a browser's:
//!
//! - the QUIC handshake, its TLS inside it (one round trip): the same, the
//!   native client verifies the host's certificate by its hash as a page's
//!   `serverCertificateHashes` does;
//! - HTTP/3's SETTINGS: a browser, like the native client, waits for the
//!   host's before it asks for a WebTransport session, since they say
//!   whether the host takes one. The host sends them with its handshake, so
//!   that wait costs no round trip of its own;
//! - the extended CONNECT that opens the session at its path (one round
//!   trip): the same request, and the host writes the entry's binding as it
//!   answers it, so the hello waits for nothing more;
//! - a browser resolves the host's name first, and loaded the page over
//!   HTTPS before any of this. Neither is the session's, and neither is
//!   counted, as the iroh path counts neither.

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
use tilo_session_client::{Cmd, Event, Link, Outbox, TransportPhase};

use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::Host;
use tilo_session_client::MemoryPeerStore;
use tilo_session_client::device::{Entering, door_entry};
use tilo_user_identity::UserIdentity;

use support::{enrol, enter_page, keyed, start_workspace_host_relaying};

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
    inner: &'a dyn tilo_relay_client::https::HttpClient,
    round_trip: Duration,
    asked: std::sync::Mutex<Vec<(String, Duration, usize)>>,
}

impl tilo_relay_client::https::HttpClient for Network<'_> {
    fn post(&self, path: &str, body: Vec<u8>) -> tilo_relay_client::https::HttpFuture<'_> {
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

/// The network between a guest and its host, by the way the guest reaches
/// it, counting what crosses it.
enum Wire {
    Datagrams(Datagrams),
    Stream(DelayedLink),
}

impl Wire {
    /// What reached the guest, and what it sent.
    fn bytes(&self) -> (u64, u64) {
        match self {
            Self::Datagrams(datagrams) => datagrams.bytes(),
            Self::Stream(link) => link.bytes(),
        }
    }
}

/// Print one chunk the wire carried, with when it was sent, when traced.
fn trace_wire(way: &str, bytes: usize) {
    if wire_traced() {
        eprintln!(
            "WIRE {:>9.1} {way:>4} {bytes}",
            origin().elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// A network between a page and the host's WebTransport listener: every
/// datagram each way arrives `round_trip / 2` after it was sent, in order,
/// and the bytes of those that reach the page are counted, as [`DelayedLink`] counts
/// a stream's. One page's session crosses it: the first address to send is
/// the page's.
struct Datagrams {
    addr: std::net::SocketAddr,
    received: std::sync::Arc<std::sync::atomic::AtomicU64>,
    sent: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Datagrams {
    async fn start(upstream: std::net::SocketAddr, round_trip: Duration) -> Self {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        let page_side = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let host_side = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        host_side.connect(upstream).await.unwrap();
        let addr = page_side.local_addr().unwrap();
        let received = Arc::new(AtomicU64::new(0));
        let sent = Arc::new(AtomicU64::new(0));
        let page = Arc::new(std::sync::OnceLock::<std::net::SocketAddr>::new());
        let delay = round_trip / 2;
        // Up: from the page to the host, each datagram `delay` later.
        let up = Self::delayed({
            let host_side = host_side.clone();
            move |datagram: Vec<u8>| {
                let host_side = host_side.clone();
                async move {
                    let _ = host_side.send(&datagram).await;
                }
            }
        });
        {
            let (page_side, page, sent) = (page_side.clone(), page.clone(), sent.clone());
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 64 * 1024];
                while let Ok((read, from)) = page_side.recv_from(&mut buffer).await {
                    if *page.get_or_init(|| from) != from {
                        continue;
                    }
                    sent.fetch_add(read as u64, Ordering::Relaxed);
                    trace_wire("up", read);
                    let at = tokio::time::Instant::now() + delay;
                    if up.send((at, buffer[..read].to_vec())).is_err() {
                        break;
                    }
                }
            });
        }
        // Down: from the host to the page.
        let down = Self::delayed({
            move |datagram: Vec<u8>| {
                let (page_side, page) = (page_side.clone(), page.get().copied());
                async move {
                    if let Some(page) = page {
                        let _ = page_side.send_to(&datagram, page).await;
                    }
                }
            }
        });
        {
            let received = received.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 64 * 1024];
                while let Ok(read) = host_side.recv(&mut buffer).await {
                    received.fetch_add(read as u64, Ordering::Relaxed);
                    trace_wire("down", read);
                    let at = tokio::time::Instant::now() + delay;
                    if down.send((at, buffer[..read].to_vec())).is_err() {
                        break;
                    }
                }
            });
        }
        Self {
            addr,
            received,
            sent,
        }
    }

    /// A queue whose datagrams `deliver` takes each at its time, in order.
    fn delayed<F, Fut>(
        deliver: F,
    ) -> tokio::sync::mpsc::UnboundedSender<(tokio::time::Instant, Vec<u8>)>
    where
        F: Fn(Vec<u8>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let (queue, mut due) =
            tokio::sync::mpsc::unbounded_channel::<(tokio::time::Instant, Vec<u8>)>();
        tokio::spawn(async move {
            while let Some((at, datagram)) = due.recv().await {
                tokio::time::sleep_until(at).await;
                deliver(datagram).await;
            }
        });
        queue
    }

    fn bytes(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering;
        (
            self.received.load(Ordering::Relaxed),
            self.sent.load(Ordering::Relaxed),
        )
    }
}

/// A network between a device and the host's packet relay: every byte each
/// way arrives `round_trip / 2` after it was sent, in order, and the bytes
/// that reach the device are counted. A device reaching its host only
/// through the relay sends nothing else, so this is all of its session's
/// path to the host.
struct DelayedLink {
    addr: std::net::SocketAddr,
    received: std::sync::Arc<std::sync::atomic::AtomicU64>,
    sent: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl DelayedLink {
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
                    "up",
                ));
                tokio::spawn(Self::carry(
                    relay_read,
                    page_write,
                    round_trip / 2,
                    down.clone(),
                    "down",
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
        way: &'static str,
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
                    trace_wire(way, read);
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

/// Whether each chunk the link carries is printed, with when it was sent
/// (`TILO_CROWD_WIRE=1`): the round trips of an entry, as the wire saw them.
fn wire_traced() -> bool {
    static TRACED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACED.get_or_init(|| std::env::var_os("TILO_CROWD_WIRE").is_some())
}

/// Print the host's steps, beside the wire when it is traced, timed as it is:
/// its timings by default, or what `TILO_CROWD_LOG` asks for (a tracing
/// filter).
fn trace_steps() {
    struct SinceOrigin;
    impl tracing_subscriber::fmt::time::FormatTime for SinceOrigin {
        fn format_time(
            &self,
            w: &mut tracing_subscriber::fmt::format::Writer<'_>,
        ) -> std::fmt::Result {
            write!(w, "STEP {:>9.1}", origin().elapsed().as_secs_f64() * 1000.0)
        }
    }
    if !wire_traced() && std::env::var_os("TILO_CROWD_LOG").is_none() {
        return;
    }
    let filter =
        std::env::var("TILO_CROWD_LOG").unwrap_or_else(|_| "tilo_relay::timing=debug".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_timer(SinceOrigin)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

/// The instant the wire trace counts from.
fn origin() -> Instant {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_fresh_guest_enters_a_crowded_workspace() {
    trace_steps();
    let members = env_usize("TILO_CROWD_MEMBERS", 300);
    let online = env_usize("TILO_CROWD_KEYED", members);
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

    // The crowd: people who came through the door, each a device of their
    // own, a few at a time as people arrive. Each page enters as a visitor's
    // does, its entry carrying its request for the workspace's keys, writes its
    // own part of the shared desktop once it holds them, and goes away: the
    // workspace's channel group holds every one of them, as the Lounge's
    // holds every member who ever came. The first stays, and writes the
    // workspace's records: the shared desktop and a spread of others,
    // `record_bytes` in all.
    let seeded = Instant::now();
    let mut base = desktop_base(64 * 1024);
    let first = Member {
        host: page_host(),
        host_cert: host_device.cert.clone(),
        workspace: lounge.workspace.clone(),
        workspace_id: lounge.workspace_id,
        desktop: desktop.clone(),
    };
    let author = first.arrive("Member 0").await;
    author
        .write(desktop.clone(), &DocumentMutation::Snapshot(base.save()))
        .unwrap();
    let records = record_bytes / (100 * 1024);
    for record in 0..records {
        let route = DocumentRoute {
            app_id: "so.tilo.notes".into(),
            cluster_id: lounge.workspace.clone(),
            record: format!("note-{record}"),
        };
        author.send(Cmd::OpenRoute(route.clone())).unwrap();
        author
            .write(
                route,
                &DocumentMutation::Snapshot(record_of(100 * 1024, record as u64 + 7).save()),
            )
            .unwrap();
    }
    // The crowd's chat: its newest part, which a chain's reader finds by
    // opening the chain's head.
    let chat = chat_part(&lounge.workspace);
    author.send(Cmd::OpenRoute(chat.clone())).unwrap();
    author
        .write(
            chat,
            &DocumentMutation::Snapshot(record_of(CHAT_BYTES, 3).save()),
        )
        .unwrap();
    first
        .leave_part(&author, desktop_part(&mut base, 0, 600))
        .await;
    let staying = vec![author];
    let mut arriving = tokio::task::JoinSet::new();
    for n in 1..members {
        while arriving.len() >= ARRIVING {
            arriving.join_next().await.unwrap().unwrap();
        }
        if n % 50 == 0 {
            eprintln!("{n} members in after {:?}", seeded.elapsed());
        }
        let name = format!("Member {n}");
        if n >= online {
            // Beyond those who came with a page that took the keys: people
            // the door let in whose devices never came back.
            let doors = workspaces.clone();
            let workspace = lounge.workspace.clone();
            arriving.spawn(async move {
                through_the_door(&doors, &workspace, &name).await;
            });
            continue;
        }
        let part = desktop_part(&mut base, n, 600);
        let member = first.clone();
        arriving.spawn(async move {
            let page = member.arrive(&name).await;
            member.leave_part(&page, part).await;
            page.close();
        });
    }
    while let Some(arrived) = arriving.join_next().await {
        arrived.unwrap();
    }
    eprintln!(
        "seeded {members} members, {} with pages that took the keys, in {:?}",
        online.min(members),
        seeded.elapsed()
    );
    // What the workspace's governance weighs as a newcomer is sent it: its
    // base and the changes since.
    if let Some(peer) = workspaces.state(&lounge.workspace).await
        && let neighbour_wire::NeighbourMsg::SyncResp { changes, .. } = peer.workspace_message()
    {
        let sent: usize = changes.iter().map(Vec::len).sum();
        eprintln!(
            "GOVERNANCE {members} members: {sent} B in {} changes, {} B a member",
            changes.len(),
            sent / members.max(1)
        );
    }

    // What the site hands its page about the host, which its deployment asked
    // once, as `tilo-relay page-host` does: no guest asks it again.
    let site = tilo_relay_client::https::host_info(&*https)
        .await
        .unwrap()
        .for_site();
    assert_eq!(site.host_cert, host_device.cert);
    let place = Place {
        https: &*https,
        site: &site,
        workspace: &lounge.workspace,
        workspace_id: lounge.workspace_id,
        host: address.id,
        relay: relay_url.clone().expect("the host runs a packet relay"),
        desktop: &desktop,
        note: &DocumentRoute {
            app_id: "so.tilo.notes".into(),
            cluster_id: lounge.workspace.clone(),
            record: "note-0".into(),
        },
        chat: &chat_part(&lounge.workspace),
    };
    // A fresh guest each way a device reaches the host, at each of
    // `ROUND_TRIPS`: with no delay, what the host and the page spend; with
    // one, every round trip the guest waits through one after another.
    for way in [Way::WebTransport, Way::PacketRelay] {
        way.measure(&place).await;
    }
    for member in staying {
        member.close();
    }
}

/// How many people come through the door at once while the crowd is seeded.
const ARRIVING: usize = 16;

/// A member of the crowd arriving with a page of their own, as a new
/// visitor's page enters: through the door, its entry carrying its request
/// for the workspace's keys.
#[derive(Clone)]
struct Member {
    host: Host,
    host_cert: tilo_user_identity::DeviceCert,
    workspace: String,
    workspace_id: u64,
    desktop: DocumentRoute,
}

impl Member {
    /// The page of `name`, in the workspace and holding the desktop's keys.
    async fn arrive(&self, name: &str) -> Link {
        let (root, key) = (new_key(), new_key());
        let (cert, entry) = door_entry(
            &self.host_cert,
            &self.workspace,
            &key,
            Entering {
                held: None,
                root: Some(&root),
                label: "Browser",
                name,
                token: None,
            },
        )
        .await
        .unwrap();
        let page = enter_page(
            self.host.clone(),
            cert,
            self.host_cert.clone(),
            key,
            std::sync::Arc::new(MemoryPeerStore::default()),
            entry,
        );
        page.send(Cmd::Join(self.workspace_id)).unwrap();
        page.send(Cmd::OpenRoute(self.desktop.clone())).unwrap();
        let waiting = Instant::now();
        while !keyed(&page, &self.desktop) {
            if let Some(tilo_session_client::Entry::Refused { .. }) = &page.status().entry {
                panic!("the door turned {name} away: {:?}", page.status().entry);
            }
            assert!(
                waiting.elapsed() < Duration::from_secs(240),
                "{name}'s page never held the desktop's keys; its channels:\n{}",
                page.status().channels
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        page
    }

    /// Write `part`, `page`'s own part of the shared desktop, and wait until
    /// the host keeps it.
    async fn leave_part(&self, page: &Link, part: Vec<u8>) {
        page.write(self.desktop.clone(), &DocumentMutation::Incremental(part))
            .unwrap();
        let receipt = page.barrier().unwrap();
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Ok(kept) = receipt.try_recv() {
                kept.unwrap();
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a member's part of the desktop was never kept"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
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
    use tilo_relay_protocol::workspaces::JoinReply;
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
    let admitted = workspaces
        .admit_proven(
            issued.signed_invitation,
            cert.clone(),
            rand::random(),
            name,
            Some(token.as_bytes()),
        )
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
    https: &'a dyn tilo_relay_client::https::HttpClient,
    /// Who the host is and where it is dialled, as the page's site hands it
    /// over with the page (`tilo-relay page-host`).
    site: &'a tilo_relay_protocol::web::PageHost,
    workspace: &'a str,
    workspace_id: u64,
    host: iroh::EndpointId,
    relay: iroh::RelayUrl,
    desktop: &'a DocumentRoute,
    /// A record the members wrote, which a guest is sent once it opens it.
    note: &'a DocumentRoute,
    /// The newest part of the crowd's chat, which a guest is sent once it
    /// opens the chat's head.
    chat: &'a DocumentRoute,
}

/// The chain the crowd's chat keeps its messages in.
const CHAT_CHAIN: &str = "messages";

/// What the crowd wrote in its chat's newest part.
const CHAT_BYTES: usize = 300 * 1024;

/// The first part of the crowd's chat, its head.
fn chat_part(workspace: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.to_owned(),
        record: neighbour_wire::record_part(CHAT_CHAIN, 0),
    }
}

/// The round trips a guest's entry is measured at: none, which is what the
/// host and the page spend; a nearby visitor's; and a far one's, which
/// counts the round trips the guest waits through one after another.
const ROUND_TRIPS: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_millis(70),
    Duration::from_millis(400),
];

/// How a guest's device reaches the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    /// A page's way: the browser's WebTransport, to the host's listener.
    WebTransport,
    /// iroh through the host's packet relay alone: a native device's when
    /// no direct path to the host opens.
    PacketRelay,
}

impl Way {
    /// How many round trips a fresh guest may wait this way, one after
    /// another, beside [`COMPUTE`]: at every round trip of [`ROUND_TRIPS`]
    /// the desktop shows within this many of it plus that allowance.
    const fn round_trips(self) -> f64 {
        match self {
            Self::WebTransport => WEB_ROUND_TRIPS,
            Self::PacketRelay => RELAYED_ROUND_TRIPS,
        }
    }

    /// A fresh guest entering `place` this way at each of [`ROUND_TRIPS`],
    /// held to [`COMPUTE`], this way's round trips beside it and
    /// [`MAX_BYTES`]. A regression that adds a step in
    /// sequence, sends the whole workspace before the desktop, asks the host
    /// again or makes either side slower fails here.
    async fn measure(self, place: &Place<'_>) {
        let round_trips = self.round_trips();
        let mut entries = Vec::new();
        for round_trip in ROUND_TRIPS {
            let entry = guest_enters(place, self, round_trip).await;
            entry.report(&format!("{self:?}, {} ms", round_trip.as_millis()));
            entries.push((round_trip, entry));
        }
        let far = ROUND_TRIPS[ROUND_TRIPS.len() - 1];
        let (_, slow) = entries.last().unwrap();
        eprintln!(
            "ENTRY {self:?}: shown at {}; {:.1} round trips beside the compute allowance \
             at {far:?}; page received {} B, sent {} B",
            entries
                .iter()
                .map(|(round_trip, entry)| format!(
                    "{:.0} ms at {} ms",
                    entry.shown.as_secs_f64() * 1000.0,
                    round_trip.as_millis()
                ))
                .collect::<Vec<_>>()
                .join(", "),
            slow.shown.saturating_sub(COMPUTE).as_secs_f64() / far.as_secs_f64(),
            slow.received,
            slow.sent
        );
        for (round_trip, entry) in &entries {
            let allowed = COMPUTE + round_trip.mul_f64(round_trips);
            assert!(
                entry.shown <= allowed,
                "over {self:?}, a fresh guest's desktop showed after {:?} at a {round_trip:?} \
                 round trip, more than {round_trips} round trips beside {COMPUTE:?} of compute \
                 ({allowed:?})",
                entry.shown
            );
            // Its site says who the host is, and the door and the admission
            // ride the session: a fresh guest asks the host nothing over
            // HTTPS.
            assert!(
                entry.http.is_empty(),
                "over {self:?}, a fresh guest asked the host over HTTPS: {:?}",
                entry.http
            );
            assert!(
                entry.received <= MAX_BYTES,
                "over {self:?}, a fresh guest received {} B before its desktop showed, more \
                 than {MAX_BYTES} B",
                entry.received
            );
        }
        // The entry carried the guest's request for the workspace's keys, so
        // its Welcome is the first thing it downloads: the keys never wait
        // behind the desktop they open.
        assert!(
            slow.keyed < slow.record && slow.keyed < slow.ready,
            "over {self:?}, the guest held the desktop's keys at {:?}, after its record at {:?} \
             or the end of its replay at {:?}",
            slow.keyed,
            slow.record,
            slow.ready
        );
    }
}

/// How many round trips a fresh guest's page may wait over WebTransport,
/// one after another, beside [`COMPUTE`]. The slow entry is held to a fixed
/// allowance rather than measured against the quick one (see
/// [`RELAYED_ROUND_TRIPS`]). It counts:
///
/// - the QUIC handshake, with TLS inside it: 1. The host's HTTP/3 SETTINGS
///   go out with its handshake, so the page asks for its session as the
///   handshake completes;
/// - the extended CONNECT that opens the session: 1. The host writes the
///   entry's binding as it answers, so the hello waits for nothing more;
/// - the hello, which carries the entry with its proof and the guest's first
///   requests, and the host's answer with the Welcome first: 1;
/// - the replay of governance and the desktop, about 0.55 MB for a crowd of
///   300 that all hold the keys (see [`MAX_BYTES`]), from a 256 KiB initial
///   window, paced over the round trip: 1.5 to 2.
///
/// Four and a half to five in all, of which the fixed allowance reads about
/// three: the host and the page compute while the replay travels. At 400 ms
/// the desktop shows in about 2.6 s, at 70 ms in about 1.55 s. Before the
/// host sent its SETTINGS with its handshake and started from that window,
/// the same entry took 4.1 s at 400 ms.
const WEB_ROUND_TRIPS: f64 = 3.25;

/// What the host and the page may spend on a fresh guest's entry, from its
/// first request until its desktop shows: the session and the door (about
/// 0.1 s), the workspace admitting the guest (about 60 ms for 300 members),
/// the participants' commit that adds the guest to a channel group of 300 and
/// carries its Welcome (about 85 ms), the replay written, and the page taking
/// in its Welcome, the commits after it, the governance and the 300 members'
/// parts of the desktop. Every member's page holds the keys, as in the Lounge,
/// so each of those channel frames works through a group of 300. The quick
/// entry shows in 0.85 to 1.05 s on the builder.
///
/// Nothing here waits on a clock: a guest whose certificate was issued a
/// moment before its entry is in the workspace's channel group at once, so
/// the quick entry is held to this allowance alone. The allowance is kept
/// wider than the quick entry needs because the slow entry is held to it too,
/// with its round trips beside it.
const COMPUTE: Duration = Duration::from_millis(1_300);

/// How many round trips a fresh guest may wait over iroh through the packet
/// relay, one after another, beside [`COMPUTE`]: the slow entry's desktop
/// shows within this many of the round trip plus that allowance.
///
/// The slow entry is not measured against the quick one, as it once was:
/// host work that overlaps the slow entry's waits made the difference read
/// low, and taking work off the host made it read high though the desktop
/// showed sooner. Against a fixed allowance the count falls only as the
/// visitor's wait does. It counts:
///
/// - the packet relay's WebSocket upgrade, which brings the relay's
///   challenge: 1 (a browser adds TCP's and TLS's own round trips here);
/// - the session's QUIC handshake, its Initial right behind the answer to
///   that challenge rather than a round trip later, after the relay's
///   confirmation: 1;
/// - the hello, which carries the entry with its proof (bound to the QUIC
///   connection, so no challenge of the host's comes first) and has the
///   guest's first requests right behind it: 0.5;
/// - the host's answer and the replay of governance and the desktop behind
///   it, about 0.7 MB for a crowd of 300 that all hold the keys (see
///   [`MAX_BYTES`]), from a 256 KiB initial window rather than QUIC's 14 KB:
///   2.
///
/// The desktop's keys take none of their own: the hello carries the guest's
/// request for them, and the Welcome is the first thing its replay writes.
/// The host's certificate takes none either: the page's site hands it over
/// with the page. Four and a half in all, which the fixed allowance reads as
/// about three and a half; at 400 ms the desktop shows in about 2.7 s.
const RELAYED_ROUND_TRIPS: f64 = 3.5;

/// What a fresh guest may receive before its desktop shows, 0.54 to 0.56 MB
/// for 300 members who all hold the keys: its Welcome first, once (about
/// 65 KB, the group's ratchet tree of 300), then the workspace's bootstrap
/// (under 1 KB: its roster names only the guest and the host, as the
/// governance carries every member's certificate), its governance (about
/// 0.19 MB, some 625 B a member: each device's row with its keys and
/// signatures as bytes, and each receipt with the issuer's certificate and
/// transport details its door invitation shares named once; a compressed
/// base with the changes since, which a host starts
/// over once they weigh half the base, so between one and one and a half times
/// the base: how many changes a guest meets depends on how the crowd's
/// admissions fell into changes, which is what moves the total from run to
/// run) and the shared desktop, whose members' parts are compacted into its
/// checkpoint as they come (about 0.14 MB). No member device's identity hello
/// comes: the governance carries what each says. The Lounge is public, so the
/// members' records (3.5 MB here) and its chat (0.3 MB in its newest part)
/// come only when the guest opens them, and a page, which admits no one to a
/// channel's group, is sent no member's request to be admitted to one.
/// Governance sent twice or uncompressed, the members' certificates or hellos
/// sent again beside it, any of those records or the chat sent with the
/// desktop, or the Welcome sent again crosses this.
const MAX_BYTES: u64 = 650_000;

/// One guest's entry, as it went.
struct Entry {
    /// Each step, and when it was done after the guest began.
    marks: Vec<(&'static str, Duration)>,
    /// When the desktop could show: the page held its keys and its record.
    shown: Duration,
    /// When the page held the desktop's keys, and when its record.
    keyed: Duration,
    record: Duration,
    /// When the host said the workspace's replay was done (`Ready`).
    ready: Duration,
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

/// A new visitor's page entering `place` through its door, the `way` it
/// reaches the host, over a network whose round trip is `round_trip`, until
/// it shows the desktop.
async fn guest_enters(place: &Place<'_>, way: Way, round_trip: Duration) -> Entry {
    // The page's site says who the host is, so the page asks it nothing over
    // HTTPS: its session enters through the door as it opens.
    let named = place
        .site
        .address
        .as_ref()
        .expect("the site names the host's endpoint");
    assert_eq!(named.endpoint_addr.id, place.host);
    assert_eq!(
        named.relay_url.as_deref(),
        Some(place.relay.as_str()),
        "the site names the packet relay a device reaches the host through"
    );
    let (wire, host) = match way {
        Way::WebTransport => {
            // As a page reaches its host: the browser's WebTransport, to the
            // listener the site names, verified by the certificate it names.
            let web = named
                .web
                .clone()
                .expect("the site names the host's WebTransport listener");
            let mut origin: url::Url = web.origin.parse().unwrap();
            let listener = origin
                .socket_addrs(|| None)
                .unwrap()
                .into_iter()
                .next()
                .expect("the WebTransport listener's address");
            let datagrams = Datagrams::start(listener, round_trip).await;
            origin.set_port(Some(datagrams.addr.port())).unwrap();
            let proxied = tilo_relay_protocol::web::WebLink {
                origin: origin.as_str().trim_end_matches('/').to_owned(),
                ..web
            };
            let host = Host::published(&named.clone().into())
                .over(tilo_relay_client::Carrier::WebTransport)
                .with_web(Some(proxied));
            (Wire::Datagrams(datagrams), host)
        }
        Way::PacketRelay => {
            // Only through the relay, as a native device goes when no
            // direct path to the host opens.
            let upstream = place
                .relay
                .socket_addrs(|| None)
                .unwrap()
                .into_iter()
                .next()
                .expect("the packet relay's address");
            let link = DelayedLink::start(upstream, round_trip).await;
            let mut relay: url::Url = (*place.relay).clone();
            relay.set_port(Some(link.addr.port())).unwrap();
            let host = Host::through(iroh::EndpointAddr::new(place.host), Some(relay.into()))
                .over(tilo_relay_client::Carrier::Relay);
            (Wire::Stream(link), host)
        }
    };
    let network = Network {
        inner: place.https,
        round_trip,
        asked: std::sync::Mutex::default(),
    };
    let started = Instant::now();
    if wire_traced() {
        eprintln!(
            "WIRE {:>9.1} guest begins over {way:?}, round trip {round_trip:?}",
            origin().elapsed().as_secs_f64() * 1000.0
        );
    }
    let mut marks: Vec<(&'static str, Duration)> = Vec::new();
    let root = new_key();
    let key = new_key();
    let (cert, entry) = door_entry(
        &place.site.host_cert,
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
    marks.push(("certificate made", started.elapsed()));
    let page = enter_page(
        host,
        cert,
        place.site.host_cert.clone(),
        key,
        std::sync::Arc::new(MemoryPeerStore::default()),
        entry,
    );
    page.send(Cmd::Join(place.workspace_id)).unwrap();
    page.send(Cmd::OpenRoute(place.desktop.clone())).unwrap();
    let (mut connected, mut admitted, mut ready, mut keyed_desktop, mut desktop_doc) =
        (None, None, None, None, None);
    let mut when_opened = None;
    let mut notes_before = 0_usize;
    let deadline = Instant::now() + Duration::from_secs(300);
    while desktop_doc.is_none() || keyed_desktop.is_none() {
        let status = page.status();
        if connected.is_none() && status.phase == TransportPhase::Connected && status.authenticated
        {
            connected = Some(started.elapsed());
        }
        if admitted.is_none() {
            match &status.entry {
                Some(tilo_session_client::Entry::Admitted { workspace, .. }) => {
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
            match &event {
                Event::Landed {
                    route,
                    author: Some(_),
                } if route == place.desktop => {
                    desktop_doc.get_or_insert_with(|| started.elapsed());
                }
                Event::Landed {
                    route,
                    author: Some(_),
                } if route.app_id == place.note.app_id || route.app_id == place.chat.app_id => {
                    notes_before += 1;
                }
                Event::HostReady {
                    records_when_opened,
                    ..
                } => when_opened = Some(*records_when_opened),
                _ => {}
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
    let (received, sent) = wire.bytes();
    marks.push(("session connected", connected.unwrap_or_default()));
    marks.push(("door admitted the guest", admitted.unwrap_or_default()));
    marks.push(("host admitted the subscription", ready.unwrap_or(shown)));
    marks.push(("desktop keyed", keyed_desktop.unwrap_or_default()));
    marks.push(("desktop record", desktop_doc.unwrap_or_default()));
    // A public workspace sends its records when they are opened: none of the
    // members' notes nor any of their chat came with the desktop, and one
    // opened now arrives.
    assert_eq!(
        notes_before, 0,
        "a note or the chat reached the guest before it opened one"
    );
    page.send(Cmd::OpenRecordAt(place.workspace_id, place.note.clone()))
        .unwrap();
    let (mut note, mut opened) = (false, None);
    while !note || opened.is_none() || when_opened.is_none() {
        for event in page.drain() {
            match event {
                Event::Landed {
                    route,
                    author: Some(_),
                } if route == *place.note => note = true,
                Event::RecordOpened {
                    route: Some(route),
                    frames,
                    ..
                } if route == *place.note => {
                    opened = Some(frames);
                }
                Event::HostReady {
                    records_when_opened,
                    ..
                } => when_opened = Some(records_when_opened),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "the opened note never arrived");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    marks.push((
        "a note opened after",
        started.elapsed().saturating_sub(shown),
    ));
    assert_eq!(
        when_opened,
        Some(true),
        "the guest was told records load when opened"
    );
    assert!(
        opened.is_some_and(|frames| frames > 0),
        "the host sent the note's frames"
    );
    // Opening the chat: its head, which names whichever part is the newest,
    // brings that part in one round trip.
    let asked = Instant::now();
    let head = DocumentRoute {
        record: neighbour_wire::chain_head(CHAT_CHAIN),
        ..place.chat.clone()
    };
    page.send(Cmd::OpenRecordAt(place.workspace_id, head.clone()))
        .unwrap();
    let (mut part, mut answered) = (false, None);
    while !part || answered.is_none() {
        for event in page.drain() {
            match event {
                Event::Landed {
                    route,
                    author: Some(_),
                } if route == *place.chat => part = true,
                Event::RecordOpened {
                    route: Some(route),
                    frames,
                    ..
                } if route == head => {
                    answered = Some(frames);
                }
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "the chat's head never arrived");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    marks.push(("the chat opened after", asked.elapsed()));
    assert!(
        answered.is_some_and(|frames| frames > 0),
        "the host sent the chat's newest part for its head"
    );
    page.close();
    let http = std::mem::take(&mut *network.asked.lock().unwrap());
    let http_bytes: usize = http.iter().map(|(_, _, bytes)| bytes).sum();
    Entry {
        keyed: keyed_desktop.unwrap_or_default(),
        record: desktop_doc.unwrap_or_default(),
        ready: ready.unwrap_or(shown),
        marks,
        shown,
        http,
        received: received + http_bytes as u64,
        sent,
    }
}
