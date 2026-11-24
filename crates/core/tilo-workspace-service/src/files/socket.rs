//! This device's service's side of a workspace tree's sync, served on a socket to the files
//! daemon that mounts it ([`tilo_fs::tree_sync::remote`]).
//!
//! The daemon is a process of its own, and this device runs one workspace service. A connection
//! is taken only from a process of this user that its embedder admits (`attest`), and it
//! reaches only the Files resources of the one workspace it names first. What it writes, it writes as this device, as the daemon's own mount did when
//! it signed the tree's changes itself.
//!
//! The connection carries the rest of what the mount needs of the workspace too: the bytes its
//! files are made of, asked of the workspace on the tree's route ([`super::WorkspaceSource`]);
//! the keys of the tree's group; who may read and write the tree's folders, told again the moment
//! the workspace's member list changes; and this device's one store of file bytes, which the
//! service's record content reads and keeps through the connected daemon ([`Daemons`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use fs_protocol::TreeChunk;
use tilo_fs::tree_sync::TreePeer as _;
use tilo_fs::tree_sync::remote::{Ask, FRAMING, Told};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tokio::sync::{mpsc, watch};

use super::{Backend, Copy, FILES_APP};
use crate::content::RouteContent;
use crate::service::ServiceSlot;

/// Whether process `pid` is the Files endpoint the session recorded for workspace `workspace`.
pub type Attest = Arc<dyn Fn(u32, &str) -> bool + Send + Sync>;

/// The encoded content keys of a workspace's tree, or why they are not handed over.
pub type Keys = Arc<
    dyn Fn(WorkspaceId) -> futures::future::BoxFuture<'static, Result<Vec<u8>, String>>
        + Send
        + Sync,
>;

/// What the service offers an admitted connection beside its tree.
#[derive(Clone)]
pub struct Offers {
    /// The connected daemons, whose stores the service's record content reads and keeps.
    pub daemons: Arc<Daemons>,
    /// The keys of a workspace's tree.
    pub keys: Keys,
    /// The capability list (JSON) the mounts enforce, as the workspaces' member lists and the
    /// shell's grants decide it now; `None` until decided.
    pub access: watch::Receiver<Option<String>>,
}

/// How long a want of the workspace waits for its bytes.
const WANT_WITHIN: Duration = Duration::from_secs(60);
/// How long the service waits for a daemon to lend what it keeps.
const LEND_WITHIN: Duration = Duration::from_secs(10);

/// How often what the copy holds is told again when nothing else told it.
const PEER_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// The tree's sync served on a socket, until dropped.
pub struct Serving {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serve the trees of the workspaces `service` holds at `path`, to the processes `attest`
/// admits, with `content` asked again for the bytes whose answer stalled; where each folder
/// stands is shown in `folders`.
///
/// The socket is bound once the service is open: its appearing is what tells a files daemon to
/// dial, so it never appears while a connection would only be refused.
#[must_use]
pub fn serve(
    path: &Path,
    service: ServiceSlot,
    content: Arc<RouteContent>,
    attest: Attest,
    offers: Offers,
    folders: super::Folders,
) -> Serving {
    let path = path.to_path_buf();
    let task = tilo_rt::app().spawn(async move {
        service.installed().await;
        let bound = tilo_files::native::private::bind_socket(&path).and_then(|listener| {
            listener.set_nonblocking(true)?;
            tokio::net::UnixListener::from_std(listener)
        });
        let listener = match bound {
            Ok(listener) => listener,
            Err(error) => {
                tracing::error!(%error, path = %path.display(), "the workspace trees' socket cannot be bound: no files daemon reaches this device's workspaces");
                return;
            }
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let (service, content, attest, offers, folders) = (
                service.clone(),
                Arc::clone(&content),
                Arc::clone(&attest),
                offers.clone(),
                folders.clone(),
            );
            tokio::spawn(async move {
                if let Err(error) =
                    serve_one(stream, &service, content, &attest, offers, folders).await
                {
                    tracing::debug!(%error, "a workspace tree's connection ended");
                }
            });
        }
    });
    Serving { task }
}

/// One connection, admitted and served until it ends.
async fn serve_one(
    stream: tokio::net::UnixStream,
    service: &ServiceSlot,
    content: Arc<RouteContent>,
    attest: &Attest,
    offers: Offers,
    folders: super::Folders,
) -> Result<(), String> {
    let credentials = stream.peer_cred().map_err(|error| error.to_string())?;
    let pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0);
    let (mut reader, writer) = stream.into_split();
    let (tell, told) = mpsc::unbounded_channel();
    let sending = tokio::spawn(send(writer, told));
    let hello = FRAMING
        .read_value_async::<_, Ask>(&mut reader)
        .await
        .map_err(|error| error.to_string())?;
    let admitted = admit(credentials.uid(), pid, hello, service, attest);
    let (workspaces, workspace) = match admitted {
        Ok(admitted) => admitted,
        Err(why) => {
            let _ = tell.send(Told::Refused(why.clone()));
            drop(tell);
            let _ = sending.await;
            return Err(why);
        }
    };
    let governed = workspaces.governed_copy(workspace).await.is_some();
    let source =
        super::WorkspaceSource::new(Arc::clone(&content), &workspace.to_string(), WANT_WITHIN);
    let Backend {
        copy,
        reads,
        writer,
        wakes,
    } = Backend::of(&workspaces, workspace, Some(content), governed);
    let lender = Arc::new(Lender {
        tell: tell.clone(),
        lent: Mutex::new(BTreeMap::new()),
        next: AtomicU64::new(0),
    });
    offers.daemons.connected(&workspace.to_string(), &lender);
    let name = copy.name.clone();
    let served = Connection {
        copy,
        reads: Arc::new(reads),
        writer: Arc::new(tokio::sync::Mutex::new(writer)),
        named: Arc::new(Mutex::new(BTreeSet::new())),
        told: Arc::new(Mutex::new(None)),
        tell,
        workspace,
        source: Arc::new(source),
        keys: offers.keys,
        lender,
        folders: folders.clone(),
    }
    .run(reader, wakes, offers.access)
    .await;
    // The daemon that held the folder is gone: nothing says where it stands any more.
    folders.gone(&name);
    served
}

/// The workspace the connection's first frame names, in the service this device runs, once
/// the process behind it is this user's Files endpoint for it.
fn admit(
    uid: u32,
    pid: Option<u32>,
    hello: Option<Ask>,
    service: &ServiceSlot,
    attest: &Attest,
) -> Result<(crate::workspaces::Workspaces, WorkspaceId), String> {
    if uid != rustix::process::geteuid().as_raw() {
        return Err("a process of another user may not sync this device's trees".to_owned());
    }
    let pid = pid.ok_or("the connecting process has no usable pid")?;
    let Some(Ask::Hello { workspace }) = hello else {
        return Err("a tree's connection starts by naming its workspace".to_owned());
    };
    if !attest(pid, &workspace) {
        return Err(format!(
            "process {pid} is not the Files endpoint this session recorded for {workspace}"
        ));
    }
    let id = WorkspaceId::parse(&workspace).ok_or("not a workspace id")?;
    let service = service
        .service()
        .ok_or("this device's workspace service is not open yet")?;
    Ok((service.workspaces().clone(), id))
}

/// Write what the connection is told until there is nothing more to tell.
async fn send(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    mut told: mpsc::UnboundedReceiver<Told>,
) {
    while let Some(told) = told.recv().await {
        if FRAMING.write_value_async(&mut writer, &told).await.is_err() {
            return;
        }
    }
}

/// The parts of the tree the service has anything of, how many writes it still keeps, whether
/// this device holds the workspace, and how much of its content the workspace's holders lack.
type Holding = (Vec<TreeChunk>, u64, bool, Option<u32>);

/// One admitted connection's copy, and what it was told.
struct Connection {
    copy: Arc<Copy>,
    reads: Arc<super::Records>,
    writer: Arc<tokio::sync::Mutex<super::Writer>>,
    /// The parts of the tree the connection named: those it is told the service has.
    named: Arc<Mutex<BTreeSet<TreeChunk>>>,
    /// What it was last told the copy holds.
    told: Arc<Mutex<Option<Holding>>>,
    tell: mpsc::UnboundedSender<Told>,
    workspace: WorkspaceId,
    /// The workspace as the source of the bytes the daemon lacks.
    source: Arc<super::WorkspaceSource>,
    keys: Keys,
    /// The daemon's store, as the service's record content reaches it.
    lender: Arc<Lender>,
    folders: super::Folders,
}

impl Connection {
    /// Answer what the daemon asks on `reader`, and tell it what `wakes` brings, until either
    /// ends.
    async fn run(
        self,
        mut reader: tokio::net::unix::OwnedReadHalf,
        mut wakes: mpsc::UnboundedReceiver<tilo_fs::tree_sync::Wake>,
        mut access: watch::Receiver<Option<String>>,
    ) -> Result<(), String> {
        let mut look = tokio::time::interval(PEER_EVERY);
        // What the mount enforces goes first, before anything of the tree.
        access.mark_changed();
        let mut deciding = true;
        loop {
            tokio::select! {
                changed = access.changed(), if deciding => {
                    if changed.is_err() {
                        // Nothing decides any more: the mount keeps what it was told last.
                        deciding = false;
                        continue;
                    }
                    let now = access.borrow_and_update().clone();
                    if let Some(now) = now
                        && self.tell.send(Told::Access(now)).is_err()
                    {
                        return Ok(());
                    }
                }
                asked = FRAMING.read_value_async::<_, Ask>(&mut reader) => {
                    match asked.map_err(|error| error.to_string())? {
                        Some(ask) => self.take(ask)?,
                        None => return Ok(()),
                    }
                }
                wake = wakes.recv() => {
                    let Some(wake) = wake else { return Ok(()) };
                    // What the copy holds goes first: the sync decides on the wake with it.
                    self.peer(true);
                    if self.tell.send(Told::Wake(wake)).is_err() {
                        return Ok(());
                    }
                }
                _ = look.tick() => self.peer(false),
            }
        }
    }

    fn name(&self, chunk: TreeChunk) {
        self.named.lock_recover().insert(chunk);
    }

    /// Tell what the copy holds, when it changed or `always`.
    fn peer(&self, always: bool) {
        tell_peer(&self.copy, &self.named, &self.told, &self.tell, always);
    }

    /// Answer ask `id` with the keys of this connection's workspace, once they are handed over.
    fn send_keys(&self, id: u64) {
        let (keys, tell) = ((self.keys)(self.workspace), self.tell.clone());
        tokio::spawn(async move {
            let _ = tell.send(Told::Keys {
                id,
                keys: keys.await,
            });
        });
    }

    /// Show where the daemon's folder stands, under the names a person knows its workspace and
    /// server by.
    fn show(&self, status: tilo_fs::tree_sync::Status) {
        let (copy, folders) = (Arc::clone(&self.copy), self.folders.clone());
        tokio::spawn(async move {
            let (name, server) = names(&copy).await;
            folders.show(super::Folder {
                workspace: copy.name.clone(),
                name,
                server,
                path: status.path.clone(),
                status,
            });
        });
    }

    fn take(&self, ask: Ask) -> Result<(), String> {
        match ask {
            Ask::Hello { .. } => return Err("a connection names its workspace once".to_owned()),
            Ask::Open(chunk) => {
                self.name(chunk);
                self.copy.open(chunk);
            }
            Ask::ReaskContent => self.copy.reask_content(),
            Ask::Status(status) => self.show(status),
            Ask::Read {
                id,
                resource,
                since,
            } => {
                let Some(chunk) = fs_protocol::tree::chunk_of_record(&resource) else {
                    return Err(format!("{resource} is no part of the tree"));
                };
                self.name(chunk);
                let (reads, tell) = (Arc::clone(&self.reads), self.tell.clone());
                let name = self.copy.name.clone();
                tokio::task::spawn_blocking(move || {
                    let reading = tilo_fs::tree_sync::Records::read(
                        &*reads,
                        &name,
                        FILES_APP,
                        &resource,
                        since.as_deref(),
                    );
                    let _ = tell.send(Told::Read { id, reading });
                });
            }
            Ask::Write {
                id,
                chunk,
                changes,
                founding,
                snapshot,
            } => {
                self.name(chunk);
                let writer = Arc::clone(&self.writer);
                let (copy, named, told, tell) = (
                    Arc::clone(&self.copy),
                    Arc::clone(&self.named),
                    Arc::clone(&self.told),
                    self.tell.clone(),
                );
                tokio::spawn(async move {
                    let written = {
                        let mut writer = writer.lock().await;
                        tilo_fs::tree_sync::TreeWriter::write(
                            &mut *writer,
                            chunk,
                            &changes,
                            founding,
                            snapshot,
                        )
                        .await
                    };
                    // The copy holds what it took before the write is answered.
                    tell_peer(&copy, &named, &told, &tell, true);
                    let _ = tell.send(Told::Written { id, written });
                });
            }
            Ask::Want { id, hash } => {
                let (source, tell) = (Arc::clone(&self.source), self.tell.clone());
                tokio::spawn(async move {
                    let bytes = tilo_fs::hydration::ContentSource::fetch(
                        &*source,
                        iroh_blobs::Hash::from_bytes(hash),
                    )
                    .await
                    .map(|bytes| bytes.to_vec());
                    let _ = tell.send(Told::Content { id, bytes });
                });
            }
            Ask::Keys { id, workspace } => {
                if WorkspaceId::parse(&workspace) != Some(self.workspace) {
                    return Err(format!(
                        "a connection for {} asked for the keys of {workspace}",
                        self.workspace
                    ));
                }
                self.send_keys(id);
            }
            Ask::Lent { id, bytes } => self.lender.lent(id, bytes),
            Ask::Served { id, chunks } => {
                for chunk in &chunks {
                    self.name(*chunk);
                }
                let waiting = super::served(&self.copy, &chunks);
                let tell = self.tell.clone();
                tokio::spawn(async move {
                    let served = match waiting {
                        Some(waiting) => waiting.await,
                        // The copy's own store kept them: nothing to wait for.
                        None => Ok(()),
                    };
                    let _ = tell.send(Told::Served { id, served });
                });
            }
        }
        Ok(())
    }
}

/// One connected daemon's store, as the service reaches it.
struct Lender {
    tell: mpsc::UnboundedSender<Told>,
    /// The asks for bytes the daemon has not answered yet.
    lent: Mutex<BTreeMap<u64, std::sync::mpsc::SyncSender<Option<Vec<u8>>>>>,
    next: AtomicU64,
}

impl Lender {
    /// The bytes the daemon keeps under `hash`, waited for on this thread.
    fn lend(&self, hash: [u8; 32]) -> Option<Vec<u8>> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = std::sync::mpsc::sync_channel(1);
        self.lent.lock_recover().insert(id, answer);
        if self.tell.send(Told::Lend { id, hash }).is_err() {
            self.lent.lock_recover().remove(&id);
            return None;
        }
        let bytes = answered.recv_timeout(LEND_WITHIN).ok().flatten();
        self.lent.lock_recover().remove(&id);
        bytes.filter(|bytes| *blake3::hash(bytes).as_bytes() == hash)
    }

    fn lent(&self, id: u64, bytes: Option<Vec<u8>>) {
        let answer = self.lent.lock_recover().remove(&id);
        if let Some(answer) = answer {
            let _ = answer.try_send(bytes);
        }
    }

    fn keep(&self, bytes: &[u8]) -> bool {
        self.tell
            .send(Told::Keep {
                bytes: bytes.to_vec(),
            })
            .is_ok()
    }
}

/// The Files daemons connected to this service, by the workspace each mounts: the one store of
/// this device's file bytes, which the service's record content reads and keeps through them.
#[derive(Default)]
pub struct Daemons {
    connected: Mutex<HashMap<String, Vec<Weak<Lender>>>>,
    /// Called when a daemon connects for a workspace, with what keeps in its store.
    on_connect: Mutex<Option<OnConnect>>,
}

/// Hands a daemon that connected for a workspace what is to be kept in its store.
pub type OnConnect = Arc<dyn Fn(&str, &dyn Fn(&[u8]) -> bool) + Send + Sync>;

impl Daemons {
    /// Call `on_connect` each time a daemon connects for a workspace, with what keeps bytes in
    /// that daemon's store: content an earlier store of this device held moves there once.
    pub fn on_connect(&self, on_connect: OnConnect) {
        *self.on_connect.lock_recover() = Some(on_connect);
    }

    fn connected(&self, workspace: &str, lender: &Arc<Lender>) {
        self.connected
            .lock_recover()
            .entry(workspace.to_owned())
            .or_default()
            .retain(|lender| lender.strong_count() > 0);
        self.connected
            .lock_recover()
            .entry(workspace.to_owned())
            .or_default()
            .push(Arc::downgrade(lender));
        let on_connect = self.on_connect.lock_recover().clone();
        if let Some(on_connect) = on_connect {
            on_connect(workspace, &|bytes| lender.keep(bytes));
        }
    }

    fn lenders(&self, workspace: &str) -> Vec<Arc<Lender>> {
        self.connected
            .lock_recover()
            .get(workspace)
            .map(|lenders| lenders.iter().filter_map(Weak::upgrade).collect())
            .unwrap_or_default()
    }

    /// Whether a daemon keeps `workspace`'s file bytes on this device now.
    #[must_use]
    pub fn serves(&self, workspace: &str) -> bool {
        !self.lenders(workspace).is_empty()
    }

    /// The bytes named `hash` a connected daemon of `workspace` keeps, waited for on this thread:
    /// never a fetch of what none keeps.
    #[must_use]
    pub fn read(&self, workspace: &str, hash: &str) -> Option<Vec<u8>> {
        let hash: [u8; 32] = hex::decode(hash).ok()?.try_into().ok()?;
        self.lenders(workspace)
            .into_iter()
            .find_map(|lender| lender.lend(hash))
    }

    /// Keep `bytes`, named `hash`, in the store of a connected daemon of `workspace`.
    ///
    /// # Errors
    /// The bytes are not the ones `hash` names, or no daemon of the workspace is connected.
    pub fn keep(&self, workspace: &str, hash: &str, bytes: &[u8]) -> Result<(), String> {
        if blake3::hash(bytes).to_hex().as_str() != hash.to_ascii_lowercase() {
            return Err("the bytes are not the ones their address names".to_owned());
        }
        if self
            .lenders(workspace)
            .iter()
            .any(|lender| lender.keep(bytes))
        {
            Ok(())
        } else {
            Err(format!(
                "no Files mount of {workspace} runs on this device to keep its files' bytes"
            ))
        }
    }
}

/// The names a person knows `copy`'s workspace by, and the device that keeps its content (the
/// first its governance names), each empty while none is said.
async fn names(copy: &Copy) -> (String, String) {
    let Some(state) = copy.workspaces.state(&copy.name).await else {
        return (String::new(), String::new());
    };
    let governance = state.governance();
    let name = governance.workspace_name().unwrap_or_default();
    let server = governance
        .keeping_devices()
        .into_iter()
        .next()
        .map(|kept| kept.host.device_label)
        .unwrap_or_default();
    (name, server)
}

/// Tell `tell` which of the `named` parts `copy` has and how many writes it still keeps,
/// when that changed since `told` or `always`.
fn tell_peer(
    copy: &Copy,
    named: &Mutex<BTreeSet<TreeChunk>>,
    told: &Mutex<Option<Holding>>,
    tell: &mpsc::UnboundedSender<Told>,
    always: bool,
) {
    let named = named.lock_recover().clone();
    let holds: Vec<TreeChunk> = named
        .into_iter()
        .filter(|chunk| copy.holds(*chunk))
        .collect();
    let pending = u64::try_from(copy.pending()).unwrap_or(u64::MAX);
    // A workspace this device governs is here from the start, as the copy says in-process too.
    let workspace = tilo_fs::tree_sync::TreePeer::holds_workspace(copy).unwrap_or(false);
    let lacks = tilo_fs::tree_sync::TreePeer::host_lacks(copy);
    let now = (holds, pending, workspace, lacks);
    let mut told = told.lock_recover();
    if !always && told.as_ref() == Some(&now) {
        return;
    }
    let _ = tell.send(Told::Peer {
        holds: now.0.clone(),
        pending: now.1,
        workspace: now.2,
        lacks: now.3,
    });
    *told = Some(now);
}
