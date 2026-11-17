//! Writing one file of a workspace's folder costs the same whether the folder holds a thousand
//! files or twenty thousand: on the device that writes it, on the workspace's host, and on a
//! page that shows the folder.
//!
//! A computer's folder writes its tree through its session as the records the tree is kept in
//! (`fs_protocol::tree`): the tree's own record and its chunks, each change going to the chunk
//! of the file it changes, a burst of writes leaving as one write per record. The test writes
//! a repository's worth of files the way a checkout does, folder after folder, each file with
//! its version and the agent that wrote it, and compares the first thousands of files with the
//! last thousands: the time each write takes until the host has it, the bytes it sends, what
//! the host keeps on disk for it, what a reader following the folder reads for it, and how the
//! process's memory grows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::time::Instant;

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use fs_protocol::{FileActor, TreeChunk, TreeChunks};
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_fs::namespace::{Namespace, ROOT_INO};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_relay_client::Host;
use tilo_session_client::device::{Entering, entry_request};
use tilo_session_client::engine::ResourceReading;
use tilo_session_client::{Cmd, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::UserIdentity;

use super::support::{durable, enrol, enter_page, start_workspace_host_relaying, wait_for};

/// Files written in all.
const FILES: usize = 20_000;
/// Files a window of the comparison holds.
const WINDOW: usize = 1_000;
/// Files in each folder, as a repository's folders hold them.
const PER_FOLDER: usize = 100;
/// Files one write carries: what a computer's folder writes in a burst's batch.
const BATCH: usize = 50;

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

/// This process's resident memory, in KiB.
fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmRSS:"))
                .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

/// The bytes the files under `path` hold.
fn disk_bytes(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => disk_bytes(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

async fn arrive(
    host: Host,
    host_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    name: &str,
) -> Link {
    let (root, key) = (new_key(), new_key());
    let (cert, entry) = entry_request(
        host_cert,
        workspace,
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
    enter_page(
        host,
        cert,
        host_cert.clone(),
        key,
        std::sync::Arc::new(MemoryPeerStore::default()),
        entry,
    )
}

fn route(workspace: &str, chunk: TreeChunk) -> DocumentRoute {
    DocumentRoute {
        app_id: fs_protocol::tree::FILES_APP.into(),
        cluster_id: workspace.into(),
        record: fs_protocol::tree::chunk_document_id(chunk),
    }
}

/// A computer's folder, writing its tree through its session as the folder sync does.
struct Writer {
    link: Link,
    workspace: String,
    namespace: Namespace,
    /// What each record has of this namespace's changes.
    sent: BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
    actor: FileActor,
    /// Bytes handed to the session.
    bytes: usize,
}

impl Writer {
    /// Write one file into `folder`, with its version and the agent that wrote it.
    fn file(&mut self, folder: u64, n: usize) {
        let ino = self
            .namespace
            .create_file(
                folder,
                format!("file-{n}.rs"),
                Some(&iroh_blobs::Hash::new(n.to_le_bytes())),
                n as u64,
                1_790_000_000_000 + n as u64,
            )
            .unwrap();
        let document_id = self.namespace.document_id_for(ino).unwrap();
        let version = fs_protocol::FileVersionV1 {
            id: fs_protocol::FileVersionId::new(),
            document_id,
            parents: Vec::new(),
            actor: self.actor,
            timestamp_ms: 1_790_000_000_000 + n as u64,
            content_hash: Some(fs_protocol::ContentHash([7; 32])),
            metadata_hash: fs_protocol::ContentHash([9; 32]),
            provenance: fs_protocol::FileProvenance::Create,
        };
        let version_id = version.id;
        assert!(self.namespace.append_version(version).unwrap());
        self.namespace
            .attribute_version(
                document_id,
                &fs_protocol::FileVersionAttributionV1 {
                    version_id,
                    attribution: fs_protocol::FileAttributionV1 {
                        writer: fs_protocol::AttributedWriter::agent(format!("agent-{}", n % 3)),
                        label: format!("Agent {}", n % 3),
                        color: [1, 2, 3],
                    },
                },
            )
            .unwrap();
    }

    /// Hand the session what the namespace changed since the last write: one write per record,
    /// a record nobody has yet started with the empty start every device makes alike.
    fn write(&mut self) {
        for write in self.namespace.document_writes(&self.sent) {
            let route = route(&self.workspace, write.chunk);
            if !self.sent.contains_key(&write.chunk) {
                self.link.send(Cmd::OpenRoute(route.clone())).unwrap();
                let genesis = AutoCommit::new().save();
                self.bytes += genesis.len();
                self.link
                    .write(route.clone(), &DocumentMutation::Snapshot(genesis))
                    .unwrap();
            }
            self.bytes += write.changes.len();
            self.link
                .write(route, &DocumentMutation::Incremental(write.changes))
                .unwrap();
            self.sent.insert(write.chunk, write.heads);
        }
    }
}

/// A page following the folder: the tree's own record, and each chunk it lists, read as they
/// change.
struct Reader {
    link: Link,
    /// What landed in the page's replica: the records to read again.
    landed: tilo_session_client::Events,
    workspace: String,
    docs: BTreeMap<TreeChunk, AutoCommit>,
    heads: BTreeMap<TreeChunk, Vec<[u8; 32]>>,
    tree: TreeChunks,
    /// Bytes read from the session.
    bytes: usize,
    /// The records changed since they were read last.
    unread: std::collections::BTreeSet<TreeChunk>,
    /// The files each chunk holds, as it read last.
    files: BTreeMap<TreeChunk, usize>,
    /// The time spent reading.
    busy: std::time::Duration,
}

impl Reader {
    fn open(&mut self, chunk: TreeChunk) {
        let route = route(&self.workspace, chunk);
        let workspace = tilo_relay_protocol::WorkspaceId::parse(&self.workspace).unwrap();
        self.link.send(Cmd::OpenRoute(route.clone())).unwrap();
        self.link.send(Cmd::OpenDocumentAt(workspace, route)).unwrap();
        self.docs.entry(chunk).or_default();
        self.unread.insert(chunk);
    }

    /// Read what changed; how many files the tree now holds.
    fn read(&mut self) -> usize {
        let began = Instant::now();
        for event in self.landed.drain() {
            if let tilo_session_client::Event::Landed { route, .. } = event
                && let Some(chunk) = fs_protocol::tree::chunk_of_record(&route.record)
            {
                self.unread.insert(chunk);
            }
        }
        let chunks = std::mem::take(&mut self.unread);
        for chunk in chunks {
            let route = route(&self.workspace, chunk);
            let reading = self.link.engine().record_update(
                &route.cluster_id,
                &route.app_id,
                &route.record,
                self.heads.get(&chunk).map(Vec::as_slice),
            );
            let Ok(
                ResourceReading::Captured { heads, bytes, .. }
                | ResourceReading::Changes { heads, bytes, .. },
            ) = reading
            else {
                continue;
            };
            self.bytes += bytes.len();
            let doc = self.docs.entry(chunk).or_default();
            tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes).unwrap();
            if chunk == 0 {
                self.tree.read_index(doc);
            } else {
                self.tree.read_chunk(chunk, doc);
                let files = self.tree.chunk(chunk).map_or(0, |nodes| {
                    nodes
                        .values()
                        .filter(|node| node.kind == fs_protocol::FileNodeKind::File)
                        .count()
                });
                self.files.insert(chunk, files);
            }
            self.heads.insert(chunk, heads);
        }
        for chunk in self.tree.listed() {
            if !self.docs.contains_key(&chunk) {
                self.open(chunk);
            }
        }
        self.busy += began.elapsed();
        self.files.values().sum()
    }
}

/// What one window of the comparison measured, per file.
#[derive(Debug, Default, Clone, Copy)]
struct Window {
    /// Time from writing the file into the namespace until the host had it, the page's
    /// reading set apart.
    write_us: f64,
    /// Bytes sent.
    sent: f64,
    /// Bytes the host keeps on disk.
    kept: f64,
    /// Time the page spent following the folder.
    read_us: f64,
    /// Time from the host having the window's last file until the page showed it, in
    /// microseconds, for the window.
    show_us: f64,
    /// Bytes the reader read.
    read: f64,
    /// Memory the process grew by, in bytes.
    memory: f64,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "writes twenty thousand files; run it by name"]
async fn a_write_costs_the_same_in_a_folder_of_twenty_thousand_files() {
    let (owner, _) = UserIdentity::generate("Owner", 1);
    let host_device = enrol(
        &owner,
        "host",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let state = super::support::scratch_dir();
    let host = start_workspace_host_relaying(&owner, &host_device, state.path(), false, true).await;
    let workspaces = host.relay.workspaces().unwrap();
    let company = workspaces.create("Company").await.unwrap();
    workspaces
        .publish(
            &company.workspace,
            &host_device.cert,
            tilo_relay_protocol::directory::Listing {
                description: "The company's folder".into(),
                default_role: tilo_relay_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    let address = host.relay.node_addr();
    let relay_url = address.relay_urls().next().cloned();
    let page_host = || Host::through(iroh::EndpointAddr::new(address.id), relay_url.clone());
    let index = route(&company.workspace, 0);
    let writer_link = arrive(page_host(), &host_device.cert, &company.workspace, "Laptop").await;
    let reader_link = arrive(page_host(), &host_device.cert, &company.workspace, "Page").await;
    for link in [&writer_link, &reader_link] {
        link.send(Cmd::Join(company.workspace_id)).unwrap();
        link.send(Cmd::OpenRoute(index.clone())).unwrap();
        super::support::wait_keyed(link, &index).await;
    }
    let mut writer = Writer {
        link: writer_link,
        workspace: company.workspace.clone(),
        namespace: Namespace::new("laptop").unwrap(),
        sent: BTreeMap::new(),
        actor: FileActor {
            person: collab_types::Ed25519PublicKey([1; 32]),
            device: collab_types::Ed25519PublicKey([2; 32]),
        },
        bytes: 0,
    };
    let landed = reader_link
        .events_where(|event| matches!(event, tilo_session_client::Event::Landed { .. }));
    let mut reader = Reader {
        landed,
        unread: std::collections::BTreeSet::new(),
        files: BTreeMap::new(),
        busy: std::time::Duration::ZERO,
        link: reader_link,
        workspace: company.workspace.clone(),
        docs: BTreeMap::new(),
        heads: BTreeMap::new(),
        tree: TreeChunks::default(),
        bytes: 0,
    };
    reader.open(0);

    let mut windows = Vec::new();
    let mut folder = ROOT_INO;
    for start in (0..FILES).step_by(WINDOW) {
        let (sent, kept, read, busy, memory) = (
            writer.bytes,
            disk_bytes(state.path()),
            reader.bytes,
            reader.busy,
            rss_kib(),
        );
        let began = Instant::now();
        for n in start..start + WINDOW {
            if n % PER_FOLDER == 0 {
                folder = writer
                    .namespace
                    .create_dir(ROOT_INO, format!("dir-{}", n / PER_FOLDER), 1)
                    .unwrap();
            }
            writer.file(folder, n);
            if n % BATCH == BATCH - 1 {
                writer.write();
                // The page follows as the writes land.
                reader.read();
            }
        }
        writer.write();
        durable(&writer.link).await;
        let wrote = began
            .elapsed()
            .saturating_sub(reader.busy.saturating_sub(busy));
        let reading = Instant::now();
        let shown = start + WINDOW;
        wait_for("the page to show the window's files", || {
            let files = reader.read();
            async move { (files >= shown).then_some(()) }
        })
        .await;
        let shown_after = reading.elapsed();
        let window = Window {
            write_us: wrote.as_secs_f64() * 1e6 / WINDOW as f64,
            sent: (writer.bytes - sent) as f64 / WINDOW as f64,
            kept: disk_bytes(state.path()).saturating_sub(kept) as f64 / WINDOW as f64,
            read_us: reader.busy.saturating_sub(busy).as_secs_f64() * 1e6 / WINDOW as f64,
            show_us: shown_after.as_secs_f64() * 1e6,
            read: (reader.bytes - read) as f64 / WINDOW as f64,
            memory: rss_kib().saturating_sub(memory) as f64 * 1024.0 / WINDOW as f64,
        };
        eprintln!(
            "files {shown:>6}: write {:>5.0} us  sent {:>4.0} B  kept {:>5.0} B  page {:>4.0} us {:>4.0} B  memory {:>6.0} B  per file; shown {:>5.1} ms after the host had it; rss {} MiB, {} records",
            window.write_us,
            window.sent,
            window.kept,
            window.read_us,
            window.read,
            window.memory,
            window.show_us / 1000.0,
            rss_kib() / 1024,
            reader.docs.len(),
        );
        windows.push(window);
    }

    // Who wrote each file reads as it was written, batched or not.
    let tree = reader.tree.tree();
    let sample = tree
        .nodes
        .iter()
        .find(|(_, node)| node.name == "file-19999.rs")
        .and_then(|(_, node)| node.document_id)
        .unwrap();
    let doc = reader.docs.get(&sample.chunk()).unwrap();
    let versions = fs_protocol::history::document_versions(doc, sample);
    assert_eq!(versions.len(), 1);
    assert_eq!(versions.values().next().unwrap().actor, writer.actor);
    let attributed = fs_protocol::history::record_attributions(doc, sample);
    assert_eq!(
        attributed.values().next().unwrap().attribution.label,
        "Agent 1"
    );

    // The last thousand files cost what the first thousand did. A shared machine stalls now and
    // then, so each cost is the median of five windows at each end: one slow window is the
    // machine, every late window slower is the folder.
    let early = windows.get(..5).unwrap();
    let late = windows.get(windows.len() - 5..).unwrap();
    let median = |windows: &[Window], cost: fn(&Window) -> f64| {
        let mut costs: Vec<f64> = windows.iter().map(cost).collect();
        costs.sort_by(f64::total_cmp);
        costs[costs.len() / 2]
    };
    let flat = |what: &str, cost: fn(&Window) -> f64, slack: f64| {
        let (first, last) = (median(early, cost).max(1.0), median(late, cost));
        assert!(
            last <= first * slack,
            "{what} grew with the folder: {first:.0} per file early on, {last:.0} at the end"
        );
    };
    flat("the time a write takes", |w| w.write_us, 2.0);
    flat("the bytes a write sends", |w| w.sent, 1.25);
    flat("what the host keeps of a write", |w| w.kept, 2.0);
    flat("what the page reads of a write", |w| w.read, 1.25);
    flat(
        "the time the page takes to follow a write",
        |w| w.read_us,
        2.5,
    );
    // Memory grows with the files held (the writer's tree, the host's and the page's, all in
    // this process), and by no more per file at the end than early on. The allocator grows in
    // steps, so the five windows are averaged rather than their median taken.
    let memory = |windows: &[Window]| {
        windows.iter().map(|window| window.memory).sum::<f64>() / windows.len() as f64
    };
    let (early_memory, late_memory) = (memory(early), memory(late));
    assert!(
        late_memory <= early_memory * 1.5,
        "the memory a file takes grew with the folder: {early_memory:.0} B per file early on, {late_memory:.0} B at the end"
    );
    for link in [reader.link, writer.link] {
        link.close();
    }
}
