//! The sync plane: gossip propagation + bootstrap for the namespace CRDT.
//!
//! Three protocols ride one iroh endpoint:
//! - **iroh-gossip** broadcasts Automerge deltas as they happen → live push.
//! - **`qos-fs-sync/0`** (custom ALPN) serves the full document so a freshly
//!   joined peer can bootstrap before riding gossip for deltas.
//! - **iroh-blobs** (wired elsewhere) hydrates content on read.
//!
//! Crucially, a joiner must *load the host's document* (so both share the same
//! Automerge object ids); independently-created docs would fork on merge. The
//! bootstrap exchange guarantees that shared history.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh_base::EndpointAddr;
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use iroh_gossip::net::{Gossip, GOSSIP_ALPN};
use iroh_gossip::proto::TopicId;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::hydration::Hydrator;
use crate::namespace::Namespace;
use crate::store::ContentStore;

/// Custom ALPN for the document exchange (bootstrap + incremental sync).
pub const QOS_FS_SYNC_ALPN: &[u8] = b"qos-fs-sync/0";

/// Mode byte: transfer the full document (first-join bootstrap).
const MODE_FULL: u8 = b'F';
/// Mode byte: incremental Automerge sync-protocol exchange (anti-entropy).
const MODE_SYNC: u8 = b'S';
/// Hard cap on a single sync frame (DoS guard).
const MAX_FRAME: usize = 64 * 1024 * 1024;

async fn write_frame(send: &mut SendStream, data: &[u8]) -> std::io::Result<()> {
    send.write_all(&(data.len() as u32).to_le_bytes())
        .await
        .map_err(std::io::Error::other)?;
    if !data.is_empty() {
        send.write_all(data).await.map_err(std::io::Error::other)?;
    }
    Ok(())
}

async fn read_frame(recv: &mut RecvStream) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await.map_err(std::io::Error::other)?;
    let n = u32::from_le_bytes(len) as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    if n > MAX_FRAME {
        return Err(std::io::Error::other(format!("sync frame too large: {n}")));
    }
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf).await.map_err(std::io::Error::other)?;
    Ok(buf)
}

/// A shareable invitation to join a workspace: how to reach a peer + which
/// gossip topic the workspace uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub node: EndpointAddr,
    pub workspace: [u8; 32],
}

impl Ticket {
    pub fn encode(&self) -> String {
        hex::encode(postcard::to_allocvec(self).expect("serialize ticket"))
    }

    pub fn decode(s: &str) -> Result<Self> {
        let raw = hex::decode(s.trim())?;
        Ok(postcard::from_bytes(&raw)?)
    }
}

/// Bind an endpoint with an in-memory address lookup (so gossip can dial peers
/// we learn about by id). Returns both so callers can register peer addresses.
pub async fn bind_endpoint() -> Result<(Endpoint, MemoryLookup)> {
    let lookup = MemoryLookup::new();
    let endpoint = Endpoint::builder(presets::N0)
        .address_lookup(lookup.clone())
        .bind()
        .await?;
    Ok((endpoint, lookup))
}

/// Fetch the full namespace document from a peer over the bootstrap ALPN.
/// Bounded by a timeout so a stalled/unresponsive host can't hang the caller.
pub async fn fetch_remote_doc(endpoint: &Endpoint, host: EndpointAddr) -> Result<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let conn = endpoint.connect(host, QOS_FS_SYNC_ALPN).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(&[MODE_FULL]).await?;
        send.finish()?;
        // Documents are small (metadata only); 64 MiB is a generous ceiling.
        let doc = recv.read_to_end(MAX_FRAME).await?;
        Ok::<_, anyhow::Error>(doc)
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out fetching workspace document"))?
}

/// Run the incremental Automerge sync protocol against a peer, applying any
/// changes each side is missing into `namespace`. Used for periodic anti-entropy
/// (only the delta is exchanged, not the whole document).
pub async fn sync_with_peer(
    endpoint: &Endpoint,
    peer: EndpointAddr,
    namespace: &Arc<Mutex<Namespace>>,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let conn = endpoint.connect(peer, QOS_FS_SYNC_ALPN).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(&[MODE_SYNC]).await?;
        let mut state = automerge::sync::State::new();
        // Initiator: write-then-read each round; the responder mirrors.
        for _ in 0..200 {
            let msg = { namespace.lock().unwrap().sync_generate(&mut state) };
            write_frame(&mut send, msg.as_deref().unwrap_or(&[])).await?;
            let peer_msg = read_frame(&mut recv).await?;
            if !peer_msg.is_empty() {
                let _ = namespace.lock().unwrap().sync_receive(&mut state, &peer_msg);
            }
            if msg.is_none() && peer_msg.is_empty() {
                break;
            }
        }
        let _ = send.finish();
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("sync exchange timed out"))?
}

/// Serves the current namespace document to bootstrapping peers.
#[derive(Clone)]
struct SyncProtocol {
    namespace: Arc<Mutex<Namespace>>,
}

impl std::fmt::Debug for SyncProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncProtocol")
    }
}

impl ProtocolHandler for SyncProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        // Bound the exchange so a client that opens a stream but never sends
        // (or never closes) can't leak an accept task / connection forever.
        let exchange = async {
            let mut mode = [0u8; 1];
            recv.read_exact(&mut mode).await.map_err(AcceptError::from_err)?;
            match mode[0] {
                MODE_FULL => {
                    let _ = recv.read_to_end(64).await;
                    let doc = self.namespace.lock().unwrap().save();
                    send.write_all(&doc).await.map_err(AcceptError::from_err)?;
                    let _ = send.finish();
                }
                MODE_SYNC => {
                    // Responder: read-then-write each round (mirrors the initiator).
                    let mut state = automerge::sync::State::new();
                    for _ in 0..200 {
                        let peer_msg = read_frame(&mut recv).await.map_err(AcceptError::from_err)?;
                        if !peer_msg.is_empty() {
                            let _ = self
                                .namespace
                                .lock()
                                .unwrap()
                                .sync_receive(&mut state, &peer_msg);
                        }
                        let msg = { self.namespace.lock().unwrap().sync_generate(&mut state) };
                        write_frame(&mut send, msg.as_deref().unwrap_or(&[]))
                            .await
                            .map_err(AcceptError::from_err)?;
                        if peer_msg.is_empty() && msg.is_none() {
                            break;
                        }
                    }
                }
                _ => {}
            }
            Ok::<(), AcceptError>(())
        };
        match tokio::time::timeout(Duration::from_secs(30), exchange).await {
            Ok(res) => res?,
            Err(_) => return Ok(()), // timed-out client; just drop the connection
        }
        connection.closed().await;
        Ok(())
    }
}

/// Enqueues local Automerge deltas for broadcast over gossip. Cheap to clone.
#[derive(Clone)]
pub struct SyncHandle {
    local_tx: mpsc::UnboundedSender<Bytes>,
}

impl SyncHandle {
    /// Queue a freshly-produced local delta for broadcast (no-op if empty).
    pub fn broadcast_delta(&self, delta: Vec<u8>) {
        if !delta.is_empty() {
            let _ = self.local_tx.send(Bytes::from(delta));
        }
    }
}

/// Aborts the pump task when the workspace is dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A live, replicated workspace: a CRDT namespace kept in sync with peers.
pub struct Workspace {
    pub namespace: Arc<Mutex<Namespace>>,
    pub sync: Arc<SyncHandle>,
    _router: Router,
    _tasks: Vec<AbortOnDrop>,
}

impl Workspace {
    /// Begin hosting a new workspace. Returns a ticket to share with peers.
    pub async fn host(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        store: Arc<ContentStore>,
        hydrator: Arc<Hydrator>,
        namespace: Arc<Mutex<Namespace>>,
    ) -> Result<(Workspace, Ticket)> {
        let topic = *endpoint.id().as_bytes();
        let ticket = Ticket {
            node: endpoint.addr(),
            workspace: topic,
        };
        let ws = Self::start(endpoint, lookup, store, hydrator, namespace, topic, vec![]).await?;
        Ok((ws, ticket))
    }

    /// Join an existing workspace described by `ticket`. The caller must already
    /// have bootstrapped `namespace` from the host (see [`fetch_remote_doc`]).
    pub async fn join(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        store: Arc<ContentStore>,
        hydrator: Arc<Hydrator>,
        namespace: Arc<Mutex<Namespace>>,
        ticket: Ticket,
    ) -> Result<Workspace> {
        let topic = ticket.workspace;
        Self::start(endpoint, lookup, store, hydrator, namespace, topic, vec![ticket.node]).await
    }

    async fn start(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        store: Arc<ContentStore>,
        hydrator: Arc<Hydrator>,
        namespace: Arc<Mutex<Namespace>>,
        topic_bytes: [u8; 32],
        bootstrap: Vec<EndpointAddr>,
    ) -> Result<Workspace> {
        // After bootstrap/seed, set the delta watermark so future local writes
        // produce minimal deltas (and the seed isn't re-broadcast).
        namespace.lock().unwrap().prime_baseline();

        // Register bootstrap peers so gossip can dial them and we can hydrate.
        let mut boot_ids = Vec::new();
        for addr in &bootstrap {
            lookup.add_endpoint_info(addr.clone());
            hydrator.add_peer(addr.clone());
            boot_ids.push(addr.id);
        }

        let gossip = Gossip::builder().spawn(endpoint.clone());
        let sync_proto = SyncProtocol {
            namespace: namespace.clone(),
        };
        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, store.blobs_protocol(None))
            .accept(GOSSIP_ALPN, gossip.clone())
            .accept(QOS_FS_SYNC_ALPN, sync_proto)
            .spawn();

        let topic = TopicId::from_bytes(topic_bytes);
        let gtopic = if boot_ids.is_empty() {
            gossip.subscribe(topic, vec![]).await?
        } else {
            gossip.subscribe_and_join(topic, boot_ids).await?
        };
        let (sender, receiver) = gtopic.split();

        let (local_tx, local_rx) = mpsc::unbounded_channel();
        let pump = tokio::spawn(pump_loop(
            receiver,
            sender.clone(),
            local_rx,
            namespace.clone(),
            hydrator.clone(),
        ));
        // Anti-entropy backstop: periodically pull+merge peers' full docs and
        // resolve conflicts, so a missed gossip delta or a mid-join window
        // can't leave a peer permanently diverged.
        let reconcile = tokio::spawn(reconcile_loop(
            endpoint,
            namespace.clone(),
            hydrator.clone(),
            sender,
        ));

        Ok(Workspace {
            namespace,
            sync: Arc::new(SyncHandle { local_tx }),
            _router: router,
            _tasks: vec![AbortOnDrop(pump), AbortOnDrop(reconcile)],
        })
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The bidirectional pump: broadcast local deltas, apply remote deltas, and
/// learn peer addresses for hydration as neighbors come up.
async fn pump_loop(
    mut receiver: GossipReceiver,
    sender: GossipSender,
    mut local_rx: mpsc::UnboundedReceiver<Bytes>,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
) {
    loop {
        tokio::select! {
            maybe = local_rx.recv() => {
                match maybe {
                    Some(delta) => { let _ = sender.broadcast(delta).await; }
                    None => break, // all SyncHandles dropped
                }
            }
            ev = receiver.next() => {
                match ev {
                    Some(Ok(Event::Received(msg))) => {
                        // Apply the delta + break any move-cycle it introduced
                        // (cheap, pure-CPU). Content-conflict resolution and
                        // anti-entropy run in reconcile_loop so a slow hydration
                        // never blocks local delta propagation.
                        let resolve_delta = {
                            let mut ns = namespace.lock().unwrap();
                            ns.apply_remote_and_resolve(msg.content.as_ref())
                                .unwrap_or_default()
                        };
                        if !resolve_delta.is_empty() {
                            let _ = sender.broadcast(Bytes::from(resolve_delta)).await;
                        }
                    }
                    Some(Ok(Event::NeighborUp(id))) => {
                        hydrator.add_peer(EndpointAddr::new(id));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => {
                        // Transient gossip error: don't kill the pump (that
                        // would silently dead-sync the workspace) — back off
                        // briefly and keep going.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    None => break, // stream genuinely ended
                }
            }
        }
    }
}

/// Periodic anti-entropy + conflict resolution. Pulls each known peer's full
/// document and merges it (recovering from dropped gossip or a mid-join
/// window), then materializes conflict copies for any concurrent content edits.
async fn reconcile_loop(
    endpoint: Endpoint,
    namespace: Arc<Mutex<Namespace>>,
    hydrator: Arc<Hydrator>,
    sender: GossipSender,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;

        for peer in hydrator.known_peers() {
            // Incremental anti-entropy: exchange only the missing changes.
            if sync_with_peer(&endpoint, peer, &namespace).await.is_ok() {
                let resolve_delta = { namespace.lock().unwrap().post_sync_resolve() };
                if !resolve_delta.is_empty() {
                    let _ = sender.broadcast(Bytes::from(resolve_delta)).await;
                }
            }
        }

        let conflicts = crate::conflict::detect(&namespace.lock().unwrap());
        for c in conflicts {
            // Fetch just the loser's manifest for its length (chunks hydrate
            // lazily on read). Skipped if no peer can serve it yet; retried next pass.
            if let Ok(len) = hydrator.content_len(c.loser).await {
                let delta = {
                    let mut ns = namespace.lock().unwrap();
                    crate::conflict::materialize(&mut ns, &c, len, now_ms())
                };
                if !delta.is_empty() {
                    let _ = sender.broadcast(Bytes::from(delta)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::ROOT_INO;
    use std::time::Duration;

    async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..100 {
            if cond() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        cond()
    }

    #[test]
    fn ticket_roundtrips() {
        let id = iroh::SecretKey::from_bytes(&[7u8; 32]).public();
        let ticket = Ticket {
            node: EndpointAddr::new(id),
            workspace: [3u8; 32],
        };
        let decoded = Ticket::decode(&ticket.encode()).unwrap();
        assert_eq!(decoded.node.id, id);
        assert_eq!(decoded.workspace, [3u8; 32]);
    }

    /// The crux of Phase D: two real nodes bootstrap, then structural changes
    /// propagate live over gossip in BOTH directions.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn structural_changes_propagate_both_ways() {
        // --- host A: seed a dir, then start hosting ---
        let (ep_a, look_a) = bind_endpoint().await.unwrap();
        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A")));
        ns_a.lock().unwrap().create_dir(ROOT_INO, "shared", 1).unwrap();
        let (ws_a, ticket) =
            Workspace::host(ep_a.clone(), look_a, store_a.clone(), hyd_a.clone(), ns_a.clone())
                .await
                .unwrap();

        // --- joiner B: bootstrap the doc, then join ---
        let (ep_b, look_b) = bind_endpoint().await.unwrap();
        let store_b = Arc::new(ContentStore::memory());
        let hyd_b = Arc::new(Hydrator::new(ep_b.clone(), store_b.clone()));
        let doc = fetch_remote_doc(&ep_b, ticket.node.clone()).await.unwrap();
        let ns_b = Arc::new(Mutex::new(Namespace::from_bytes("B", &doc).unwrap()));
        let ws_b = Workspace::join(
            ep_b.clone(),
            look_b,
            store_b.clone(),
            hyd_b.clone(),
            ns_b.clone(),
            ticket,
        )
        .await
        .unwrap();

        // Bootstrap projection: B sees the seeded dir immediately.
        assert!(
            ns_b.lock().unwrap().entry_at("shared").is_some(),
            "joiner must see the host's seeded tree via bootstrap"
        );

        // A -> B: host creates a file post-sync (simulating a FUSE write).
        {
            let delta = {
                let mut n = ns_a.lock().unwrap();
                n.create_file(ROOT_INO, "from_a.txt", None, 0, 2).unwrap();
                n.save_incremental()
            };
            ws_a.sync.broadcast_delta(delta);
        }
        assert!(
            wait_until(|| ns_b.lock().unwrap().entry_at("from_a.txt").is_some()).await,
            "A's change must propagate to B over gossip"
        );

        // B -> A: joiner creates a file; host must see it.
        {
            let delta = {
                let mut n = ns_b.lock().unwrap();
                n.create_file(ROOT_INO, "from_b.txt", None, 0, 3).unwrap();
                n.save_incremental()
            };
            ws_b.sync.broadcast_delta(delta);
        }
        assert!(
            wait_until(|| ns_a.lock().unwrap().entry_at("from_b.txt").is_some()).await,
            "B's change must propagate back to A (bidirectional)"
        );
    }

    /// The full collaborative read-write loop: A writes a file with real
    /// content; B's namespace syncs the entry over gossip; B then hydrates the
    /// content over iroh-blobs and reads A's actual bytes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn content_written_on_one_peer_hydrates_on_another() {
        // host A
        let (ep_a, look_a) = bind_endpoint().await.unwrap();
        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A")));
        let (ws_a, ticket) =
            Workspace::host(ep_a.clone(), look_a, store_a.clone(), hyd_a.clone(), ns_a.clone())
                .await
                .unwrap();

        // joiner B
        let (ep_b, look_b) = bind_endpoint().await.unwrap();
        let store_b = Arc::new(ContentStore::memory());
        let hyd_b = Arc::new(Hydrator::new(ep_b.clone(), store_b.clone()));
        let doc = fetch_remote_doc(&ep_b, ticket.node.clone()).await.unwrap();
        let ns_b = Arc::new(Mutex::new(Namespace::from_bytes("B", &doc).unwrap()));
        let _ws_b = Workspace::join(
            ep_b.clone(),
            look_b,
            store_b.clone(),
            hyd_b.clone(),
            ns_b.clone(),
            ticket,
        )
        .await
        .unwrap();

        // A writes a file: chunk+ingest content into A's store, set the entry, broadcast.
        let content = b"a shared collaborative document body";
        let hash = store_a.write_content(content).await.unwrap().unwrap();
        {
            let delta = {
                let mut n = ns_a.lock().unwrap();
                n.create_file(ROOT_INO, "doc.txt", Some(&hash), content.len() as u64, 7)
                    .unwrap();
                n.save_incremental()
            };
            ws_a.sync.broadcast_delta(delta);
        }

        // B sees the entry via gossip...
        assert!(
            wait_until(|| ns_b.lock().unwrap().entry_at("doc.txt").is_some()).await,
            "doc entry must sync to B"
        );

        // ...and hydrates the real bytes from A over iroh-blobs.
        let bhash = {
            let mut n = ns_b.lock().unwrap();
            let attr = n.lookup(ROOT_INO, "doc.txt").unwrap();
            n.hash_for(attr.ino.0).unwrap()
        };
        assert!(!store_b.has(bhash).await, "precondition: B lacks the content");
        let got = hyd_b
            .read(bhash, 0, content.len() as u64)
            .await
            .expect("B hydrates content from A");
        assert_eq!(got.as_slice(), content, "B must read A's real bytes");
    }
}
