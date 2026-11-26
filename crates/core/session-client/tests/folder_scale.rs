//! Writing one file of a workspace's folder costs the same whether the folder holds a thousand
//! files or five thousand: on the device that writes it, on the workspace's host, and on a
//! page that shows the folder.
//!
//! A computer's folder writes its tree through its session as the records the tree is kept in
//! (`fs_protocol::tree`): the tree's own record and its chunks, each change going to the chunk
//! of the file it changes, a burst of writes leaving as one write per record. The test writes
//! a repository's worth of files the way a checkout does, folder after folder, each file
//! named for the agent that wrote it, and compares its first windows of files with its last:
//! the time each write takes until the host has it, the bytes it sends, what
//! the host keeps on disk for it, the records a reader following the folder reads and the
//! bytes it reads for it, and how the process's memory grows.

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
use fs_protocol::{TreeChunk, TreeChunks};
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_fs::namespace::{Namespace, ROOT_INO};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_server_client::ServerAddress;
use tilo_session_client::device::{Entering, entry_request};
use tilo_session_client::{Cmd, Link, MemoryPeerStore, Outbox as _};
use tilo_user_identity::UserIdentity;

use super::support::{durable, enrol, enter_page, start_workspace_host_relaying, wait_for};

/// Files written in all.
const FILES: usize = 5_000;
/// Files a window of the comparison holds.
const WINDOW: usize = 250;
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
    host: ServerAddress,
    root: &SigningKey,
    server_cert: &tilo_user_identity::DeviceCert,
    workspace: &str,
    name: &str,
) -> Link {
    let key = new_key();
    let (cert, entry) = entry_request(
        server_cert,
        workspace,
        &key,
        Entering {
            held: None,
            root: Some(root),
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
        server_cert.clone(),
        key,
        std::sync::Arc::new(MemoryPeerStore::default()),
        entry,
    )
}

fn route(workspace: &str, chunk: TreeChunk) -> DocumentRoute {
    DocumentRoute {
        app_id: fs_protocol::tree::FILES_APP.into(),
        cluster_id: workspace.into(),
        document: fs_protocol::tree::chunk_document_id(chunk),
    }
}

/// A computer's folder, writing its tree through its session as the folder sync does.
struct Writer {
    link: Link,
    workspace: String,
    namespace: Namespace,
    /// What each record has of this namespace's changes.
    sent: BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
    /// Bytes handed to the session.
    bytes: usize,
    /// Records of the tree written, one per record a write changed.
    writes: usize,
    /// The agents that write the files, each with the key its person certified for it.
    agents: Vec<super::support::Device>,
}

/// What the person whose root is `root` lets their three agents do, as their Settings records
/// it: write the workspace's files. An agent acts within that and within its person's own role.
async fn grant_agents(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    root: &SigningKey,
) {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    let person = hex::encode(root.verifying_key().to_bytes());
    let grants: Vec<Grant> = (0..3)
        .map(|n| {
            Grant::new(
                format!("agent-{n}-files"),
                Grantee::agent(format!("agent-{n}"), person.clone()),
                TreePath::workspace(workspace).app(fs_protocol::tree::FILES_APP),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
                Grantee::person(person.clone()),
            )
        })
        .collect();
    tilo_workspace_service::workspaces::fixtures::record_grants(workspaces, workspace, &grants)
        .await;
}

/// Three agents of the person whose root is `root`, each with a key the root certified for it
/// alone in `workspace`, as the person's device makes one for an agent it starts.
fn agents(
    root: &SigningKey,
    workspace: tilo_server_protocol::WorkspaceId,
) -> Vec<super::support::Device> {
    (0..3)
        .map(|n| {
            let agent = format!("agent-{n}");
            let scope =
                tilo_user_identity::AgentScope::agent_in(*workspace.as_uuid().as_bytes(), &agent);
            super::support::flow::agent_device(root, &agent, scope)
        })
        .collect()
}

impl Writer {
    /// Write one file into `folder`, as the agent that wrote it: its call signed with the key
    /// its person certified for it, which every receiver checks the change against.
    fn file(&mut self, folder: u64, n: usize) {
        let id = format!("agent-{}", n % 3);
        let signer = &self.agents[n % 3];
        let mut request = tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: fs_protocol::tree::FILES_APP.into(),
                address: tilo_actions::ActionAddress::new(
                    fs_protocol::tree::TREE_KIND,
                    fs_protocol::file_action::verbs::NEW_FILE,
                ),
            },
            argument: tilo_actions::remote::InvokeArgument::default(),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        request.signed = Some(Box::new(
            tilo_server_protocol::invocation::sign_invocation_now(
                &self.workspace,
                &request,
                &signer.cert,
                &signer.key,
                tilo_clock::unix_now_ns(),
            )
            .unwrap(),
        ));
        let person = tilo_actions::PersonId::from_root(signer.cert.user_public_key);
        let agent = tilo_actions::ActionInvoker::admitted(
            &tilo_actions::Acting::agent(id, "ada").agent_of(person),
        )
        .named(Some(format!("Agent {}", n % 3)));
        let mut context = tilo_actions::ActionContext::default().with_invoker(agent);
        context.source_invocation = Some(request.invocation);
        context.signed = tilo_actions::remote::SignedCall::of(&request).map(Box::new);
        let _acting = tilo_fs::namespace::acting_as(Some(context));
        self.namespace
            .create_file(
                folder,
                format!("file-{n}.rs"),
                Some(&iroh_blobs::Hash::new(n.to_le_bytes())),
                n as u64,
                1_790_000_000_000 + n as u64,
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
            self.writes += 1;
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
    /// Records read from the session.
    reads: usize,
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
        let workspace = tilo_server_protocol::WorkspaceId::parse(&self.workspace).unwrap();
        self.link.send(Cmd::OpenRoute(route.clone())).unwrap();
        self.link
            .send(Cmd::OpenDocumentAt(workspace, route))
            .unwrap();
        self.docs.entry(chunk).or_default();
        self.unread.insert(chunk);
    }

    /// Read what changed; how many files the tree now holds.
    fn read(&mut self) -> usize {
        let began = Instant::now();
        for event in self.landed.drain() {
            if let tilo_session_client::Event::Landed { route, .. } = event
                && let Some(chunk) = fs_protocol::tree::chunk_of_record(&route.document)
            {
                self.unread.insert(chunk);
            }
        }
        let chunks = std::mem::take(&mut self.unread);
        for chunk in chunks {
            let route = route(&self.workspace, chunk);
            let reading = crate::support::reading_of(
                &self.link,
                &route,
                self.heads.get(&chunk).map(Vec::as_slice),
            );
            let Ok(
                DocumentRead::Whole { heads, bytes, .. }
                | DocumentRead::Changes { heads, bytes, .. },
            ) = reading
            else {
                continue;
            };
            self.bytes += bytes.len();
            self.reads = self.reads.saturating_add(1);
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

/// What one window of the comparison measured.
#[derive(Debug, Default, Clone, Copy)]
struct Window {
    /// Time from writing the file into the namespace until the host had it, the page's
    /// reading set apart: printed, never judged, as a shared machine stalls now and then.
    write_us: f64,
    /// Records of the tree the window's writes changed.
    writes: f64,
    /// Bytes sent.
    sent: f64,
    /// Bytes the host keeps on disk.
    kept: f64,
    /// Time the page spent following the folder.
    read_us: f64,
    /// Records the page read to follow the folder, for the window.
    records: f64,
    /// Time from the host having the window's last file until the page showed it, in
    /// microseconds, for the window.
    show_us: f64,
    /// Bytes the reader read.
    read: f64,
    /// Memory the process grew by, in bytes.
    memory: f64,
}

/// A host keeping a company's folder in a workspace every page that enters is an editor of.
struct Folder {
    /// The running server, kept for as long as the folder is used.
    server: super::support::TestHost,
    state: tempfile::TempDir,
    owner_device: super::support::Device,
    company: tilo_server_protocol::workspaces::WorkspaceSummary,
    /// How a page reaches the host.
    page: ServerAddress,
}

async fn folder() -> Folder {
    let (owner, _) = UserIdentity::generate("Owner", 1);
    let owner_device = enrol(
        &owner,
        "host",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let state = super::support::scratch_dir();
    let server =
        start_workspace_host_relaying(&owner, &owner_device, state.path(), false, true).await;
    let workspaces = server.server.workspaces().unwrap();
    let company = workspaces.create("Company").await.unwrap();
    workspaces
        .publish(
            &company.workspace,
            &owner_device.cert,
            tilo_server_protocol::directory::Listing {
                description: "The company's folder".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    let address = server.server.node_addr();
    let via = address.relay_urls().next().cloned();
    let reach = || ServerAddress::through(iroh::EndpointAddr::new(address.id), via.clone());
    Folder {
        page: reach(),
        server,
        state,
        owner_device,
        company,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_costs_the_same_in_a_folder_of_five_thousand_files() {
    let Folder {
        server,
        state,
        owner_device,
        company,
        page,
    } = folder().await;
    let reach = || page.clone();
    let index = route(&company.workspace, 0);
    let writer_root = new_key();
    grant_agents(
        server.server.workspaces().unwrap(),
        &company.workspace,
        &writer_root,
    )
    .await;
    let writer_link = arrive(
        reach(),
        &writer_root,
        &owner_device.cert,
        &company.workspace,
        "Laptop",
    )
    .await;
    let reader_link = arrive(
        reach(),
        &new_key(),
        &owner_device.cert,
        &company.workspace,
        "Page",
    )
    .await;
    for link in [&writer_link, &reader_link] {
        link.send(Cmd::Join(company.workspace_id)).unwrap();
        link.send(Cmd::OpenRoute(index.clone())).unwrap();
        super::support::wait_keyed(link, &index).await;
    }
    let mut writer = Writer {
        namespace: laptop_folder(&company.workspace, &writer_link),
        agents: agents(&writer_root, company.workspace_id),
        link: writer_link,
        workspace: company.workspace.clone(),
        sent: BTreeMap::new(),
        bytes: 0,
        writes: 0,
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
        reads: 0,
    };
    reader.open(0);

    let mut windows = Vec::new();
    let mut folder = ROOT_INO;
    for start in (0..FILES).step_by(WINDOW) {
        let (changed, sent, kept, read, reads, busy, memory) = (
            writer.writes,
            writer.bytes,
            disk_bytes(state.path()),
            reader.bytes,
            reader.reads,
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
            writes: (writer.writes - changed) as f64,
            sent: (writer.bytes - sent) as f64 / WINDOW as f64,
            kept: disk_bytes(state.path()).saturating_sub(kept) as f64 / WINDOW as f64,
            read_us: reader.busy.saturating_sub(busy).as_secs_f64() * 1e6 / WINDOW as f64,
            records: (reader.reads - reads) as f64,
            show_us: shown_after.as_secs_f64() * 1e6,
            read: (reader.bytes - read) as f64 / WINDOW as f64,
            memory: rss_kib().saturating_sub(memory) as f64 * 1024.0 / WINDOW as f64,
        };
        eprintln!(
            "files {shown:>6}: write {:>5.0} us {:>3.0} records  sent {:>4.0} B  kept {:>5.0} B  page {:>4.0} us {:>4.0} B {:>4.0} records  memory {:>6.0} B  per file; shown {:>5.1} ms after the host had it; rss {} MiB, {} records",
            window.write_us,
            window.writes,
            window.sent,
            window.kept,
            window.read_us,
            window.read,
            window.records,
            window.memory,
            window.show_us / 1000.0,
            rss_kib() / 1024,
            reader.docs.len(),
        );
        windows.push(window);
    }

    // Who wrote each file reads as it was written, batched or not.
    let tree = reader.tree.tree();
    let newest = format!("file-{}.rs", FILES - 1);
    let sample = tree
        .nodes
        .iter()
        .find(|(_, node)| node.name == newest)
        .and_then(|(_, node)| node.document_id)
        .unwrap();
    let mut doc = reader.docs.get(&sample.chunk()).unwrap().clone();
    let named: Vec<String> = doc
        .get_changes(&[])
        .into_iter()
        .filter_map(|change| tilo_actions::ActionContext::from_change_message(change.message()?))
        .filter_map(|context| context.invoker?.name)
        .collect();
    assert!(
        named.iter().any(|name| name == "Agent 1"),
        "the file's chunk names the agent that wrote it: {named:?}"
    );

    // The last files cost what the first did, counted rather than timed: a shared machine stalls
    // for seconds now and then, and the records a write changes and makes the page read do not
    // change with the machine. Sizes are the median of five windows at each end.
    let early = windows.get(..5).unwrap();
    let late = windows.get(windows.len() - 5..).unwrap();
    let median = |windows: &[Window], cost: fn(&Window) -> f64| {
        let mut costs: Vec<f64> = windows.iter().map(cost).collect();
        costs.sort_by(f64::total_cmp);
        costs[costs.len() / 2]
    };
    let flat = |what: &str, unit: &str, cost: fn(&Window) -> f64, slack: f64| {
        let (first, last) = (median(early, cost).max(1.0), median(late, cost));
        assert!(
            last <= first * slack,
            "{what} grew with the folder: {first:.0} {unit} early on, {last:.0} {unit} at the end"
        );
    };
    flat("the bytes a write sends", "per file", |w| w.sent, 1.25);
    flat(
        "what the host keeps of a write",
        "per file",
        |w| w.kept,
        2.0,
    );
    flat(
        "what the page reads of a write",
        "per file",
        |w| w.read,
        1.25,
    );
    // The page's work to follow a write is the records it reads. The session batches a burst
    // into few records, so a window's count is a whole 1 to 5 whatever the folder holds: the
    // ends are compared as the five windows' sums, where a record more or less is the batching.
    // A page that followed a write through the whole folder would read every chunk, tens of
    // times more at five thousand files than at two.
    // A write that changed records across the whole folder would write every chunk, as many as
    // the folder has at five thousand files: what the writer changes is counted the same way.
    let written = |windows: &[Window]| windows.iter().map(|window| window.writes).sum::<f64>();
    let (early_written, late_written) = (written(early), written(late));
    assert!(
        late_written <= early_written.max(1.0) * 3.0,
        "the records a write changes grew with the folder: {early_written:.0} early on, {late_written:.0} at the end"
    );
    let records = |windows: &[Window]| windows.iter().map(|window| window.records).sum::<f64>();
    let (early_records, late_records) = (records(early), records(late));
    assert!(
        late_records <= early_records.max(1.0) * 3.0,
        "the records the page reads to follow a write grew with the folder: {early_records:.0} early on, {late_records:.0} at the end"
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

/// How often the other member makes a small change while the checkout goes in.
const OTHER_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// The `percent`th percentile of `sorted`.
fn percentile(sorted: &[std::time::Duration], percent: usize) -> std::time::Duration {
    sorted[(sorted.len() - 1) * percent / 100]
}

/// One member checks a repository out into the workspace's folder as fast as its computer
/// writes it, while another member makes a small change every tenth of a second. The host
/// refuses neither: a member's changes are not counted against a clock. Every file of the
/// checkout lands, and the other member's changes stay prompt, since a member's run of commits
/// is taken in bounded runs, in turn with every other member's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_checkout_is_never_refused_and_never_holds_another_member_up() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .with_writer(std::io::stderr)
        .try_init();
    let Folder {
        server,
        state: _state,
        owner_device,
        company,
        page,
    } = folder().await;
    let reach = || page.clone();
    let index = route(&company.workspace, 0);
    // Two people: one checks the repository out, the other keeps a record of their own.
    let checkout_root = new_key();
    grant_agents(
        server.server.workspaces().unwrap(),
        &company.workspace,
        &checkout_root,
    )
    .await;
    let checkout = arrive(
        reach(),
        &checkout_root,
        &owner_device.cert,
        &company.workspace,
        "Laptop",
    )
    .await;
    let other = arrive(
        reach(),
        &new_key(),
        &owner_device.cert,
        &company.workspace,
        "Phone",
    )
    .await;
    let theirs = route(&company.workspace, TreeChunk::MAX);
    for (link, record) in [(&checkout, &index), (&other, &theirs)] {
        link.send(Cmd::Join(company.workspace_id)).unwrap();
        link.send(Cmd::OpenRoute(record.clone())).unwrap();
        super::support::wait_keyed(link, record).await;
    }
    let refused = |link: &Link| {
        link.events_where(|event| matches!(event, tilo_session_client::Event::Refused { .. }))
    };
    let (checkout_refused, other_refused) = (refused(&checkout), refused(&other));

    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let changing = tokio::spawn({
        let done = std::sync::Arc::clone(&done);
        async move {
            let mut doc = AutoCommit::new();
            let mut took = Vec::new();
            let mut every = tokio::time::interval(OTHER_EVERY);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                every.tick().await;
                let mutation = super::support::say(
                    &mut doc,
                    super::support::person_of(&other),
                    &format!("change {}", took.len()),
                );
                let sent = Instant::now();
                other.write(theirs.clone(), &mutation).unwrap();
                durable(&other).await;
                took.push(sent.elapsed());
            }
            (other, took)
        }
    });

    let began = Instant::now();
    let mut writer = Writer {
        namespace: laptop_folder(&company.workspace, &checkout),
        agents: agents(&checkout_root, company.workspace_id),
        link: checkout,
        workspace: company.workspace.clone(),
        sent: BTreeMap::new(),
        bytes: 0,
        writes: 0,
    };
    let mut folder = ROOT_INO;
    for n in 0..FILES {
        if n % PER_FOLDER == 0 {
            folder = writer
                .namespace
                .create_dir(ROOT_INO, format!("dir-{}", n / PER_FOLDER), 1)
                .unwrap();
        }
        writer.file(folder, n);
        if n % BATCH == BATCH - 1 {
            writer.write();
            tokio::task::yield_now().await;
        }
    }
    writer.write();
    let handed = began.elapsed();
    durable(&writer.link).await;
    let landed = began.elapsed();
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let (other, mut took) = changing.await.unwrap();

    // A page that opens the folder afterwards finds every file of the checkout.
    let reader_link = arrive(
        reach(),
        &new_key(),
        &owner_device.cert,
        &company.workspace,
        "Page",
    )
    .await;
    reader_link.send(Cmd::Join(company.workspace_id)).unwrap();
    reader_link.send(Cmd::OpenRoute(index.clone())).unwrap();
    super::support::wait_keyed(&reader_link, &index).await;
    let mut reader = Reader {
        landed: reader_link
            .events_where(|event| matches!(event, tilo_session_client::Event::Landed { .. })),
        unread: std::collections::BTreeSet::new(),
        files: BTreeMap::new(),
        busy: std::time::Duration::ZERO,
        link: reader_link,
        workspace: company.workspace.clone(),
        docs: BTreeMap::new(),
        heads: BTreeMap::new(),
        tree: TreeChunks::default(),
        bytes: 0,
        reads: 0,
    };
    reader.open(0);
    wait_for("a page to show every file of the checkout", || {
        let files = reader.read();
        async move { (files >= FILES).then_some(()) }
    })
    .await;

    took.sort();
    let (p50, p99, max) = (
        percentile(&took, 50),
        percentile(&took, 99),
        took[took.len() - 1],
    );
    let refusals = (checkout_refused.drain(), other_refused.drain());
    eprintln!(
        "{FILES} files handed over in {handed:?}, all landed after {landed:?}; the other member's {} changes landed after p50 {p50:?}, p99 {p99:?}, max {max:?}; refusals: {} of the checkout's, {} of the other's; failures: {:?} / {:?}",
        took.len(),
        refusals.0.len(),
        refusals.1.len(),
        writer.link.status().failures,
        other.status().failures,
    );
    assert!(
        refusals.0.is_empty() && refusals.1.is_empty(),
        "the host refused a member's change: {refusals:?}"
    );
    assert!(
        took.len() >= 10,
        "the other member changed their record while the checkout went in"
    );
    assert!(
        p99 < std::time::Duration::from_secs(1),
        "the checkout held the other member's changes up: p99 {p99:?}"
    );
    for link in [reader.link, writer.link, other] {
        link.close();
    }
}

/// The folder of `workspace` the laptop behind `link` mounts, on the person's certified device:
/// each change of its tree names them, as the computer's own mount does.
fn laptop_folder(workspace: &str, link: &Link) -> Namespace {
    let cert = &link.engine().identity().device_cert;
    Namespace::new(tilo_fs::namespace::MountAuthor::person(
        workspace,
        hex::encode(cert.device_public_key),
        tilo_actions::MemberId::person(tilo_actions::PersonId::from_root(cert.user_public_key)),
    ))
    .unwrap()
}
