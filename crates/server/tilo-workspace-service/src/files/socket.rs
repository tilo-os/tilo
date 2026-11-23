//! This device's service's side of a workspace tree, served on a socket to the files daemon
//! that mounts it ([`tilo_fs::files_sync::remote`]).
//!
//! The daemon is a process of its own, and this device runs one workspace service: the
//! session's. A connection is taken only from a process of this user that the session recorded
//! as the Files endpoint (`attest`), and it reaches only the Files nodes of the one workspace it
//! names first. What it writes, it writes as this device, as the daemon's own mount did.
//!
//! The connection carries the rest of what the mount needs of the workspace too: the bytes its
//! files are made of, asked of the workspace on the tree's route ([`super::WorkspaceSource`]);
//! the keys of the tree's group; who may read and write the tree's folders; and this device's
//! one store of file bytes, which the service's content reads and keeps through the connected
//! daemon ([`Daemons`]).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tilo_fs::files_sync::Peer as _;
use tilo_fs::files_sync::remote::{Ask, FRAMING, Told};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tokio::sync::{mpsc, watch};

use super::{StorePeer, WorkspaceSource};
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

/// The tree served on a socket, until dropped.
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
/// # Errors
/// The socket cannot be bound.
pub fn serve(
    path: &Path,
    service: ServiceSlot,
    content: Arc<RouteContent>,
    attest: Attest,
    offers: Offers,
    folders: super::Folders,
) -> std::io::Result<Serving> {
    let listener = tilo_files::native::private::bind_socket(path)?;
    listener.set_nonblocking(true)?;
    let task = tilo_rt::app().spawn(async move {
        let listener = match tokio::net::UnixListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, "the workspace trees' socket does not serve");
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
    Ok(Serving { task })
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
    let source = Arc::new(WorkspaceSource::new(
        Arc::clone(&content),
        &workspace.to_string(),
        WANT_WITHIN,
    ));
    let peer = Arc::new(StorePeer::new(
        workspaces.clone(),
        workspace,
        Some(Arc::clone(&content)),
    ));
    let lender = Arc::new(Lender {
        tell: tell.clone(),
        lent: Mutex::new(BTreeMap::new()),
        next: AtomicU64::new(0),
    });
    offers.daemons.connected(&workspace.to_string(), &lender);
    let name = workspace.to_string();
    let served = Arc::new(Connection {
        peer,
        name: name.clone(),
        workspace,
        source,
        keys: offers.keys,
        lender,
        folders: folders.clone(),
        tell,
        writes: workspaces.document_writes(),
    })
    .run(reader, offers.access)
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

/// One admitted connection's store, and what it was told.
struct Connection {
    peer: Arc<StorePeer>,
    name: String,
    workspace: WorkspaceId,
    /// The workspace as the source of the bytes the daemon lacks.
    source: Arc<WorkspaceSource>,
    keys: Keys,
    /// The daemon's store, as the service's content reaches it.
    lender: Arc<Lender>,
    folders: super::Folders,
    tell: mpsc::UnboundedSender<Told>,
    /// Each write a workspace held here took in: a store write is told to the daemon as a
    /// change of its tree.
    writes: tokio::sync::broadcast::Receiver<crate::workspaces::ResourceWrite>,
}

impl Connection {
    /// Answer what the daemon asks on `reader`, tell it what changed, until either ends.
    async fn run(
        self: Arc<Self>,
        mut reader: tokio::net::unix::OwnedReadHalf,
        mut access: watch::Receiver<Option<String>>,
    ) -> Result<(), String> {
        let mut writes = self.writes.resubscribe();
        // What the mount enforces goes first, before anything of the tree.
        access.mark_changed();
        let mut deciding = true;
        loop {
            tokio::select! {
                changed = access.changed(), if deciding => {
                    if changed.is_err() {
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
                written = writes.recv() => match written {
                    Ok(written) if written.workspace == self.name && written.app == tilo_fs::files_sync::FILES_APP => {
                        if self.tell.send(Told::Changed).is_err() {
                            return Ok(());
                        }
                    }
                    // A follower that fell behind reads the tree again: any node may have changed.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if self.tell.send(Told::Changed).is_err() {
                            return Ok(());
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                    Ok(_) => {}
                }
            }
        }
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

    /// Show where the daemon's folder stands, under the names a person knows its workspace by.
    fn show(&self, status: tilo_fs::files_sync::Status) {
        let folder = super::Folder {
            workspace: self.name.clone(),
            name: self.name.clone(),
            server: String::new(),
            status,
        };
        self.folders.show(folder);
    }

    fn take(self: &Arc<Self>, ask: Ask) -> Result<(), String> {
        match ask {
            Ask::Hello { .. } => return Err("a connection names its workspace once".to_owned()),
            Ask::RetiredRead | Ask::RetiredOpen | Ask::RetiredWrite | Ask::RetiredServed => {}
            Ask::List {
                id,
                parent,
                after,
                limit,
            } => {
                let (peer, tell) = (Arc::clone(&self.peer), self.tell.clone());
                tokio::task::spawn_blocking(move || {
                    let page = peer.list(&parent, after, limit);
                    let _ = tell.send(Told::List { id, page });
                });
            }
            Ask::Count { id, parent } => {
                let (peer, tell) = (Arc::clone(&self.peer), self.tell.clone());
                tokio::task::spawn_blocking(move || {
                    let count = peer.count(&parent);
                    let _ = tell.send(Told::Count { id, count });
                });
            }
            Ask::Apply { id, burst, actions } => {
                let (peer, tell) = (Arc::clone(&self.peer), self.tell.clone());
                tokio::task::spawn_blocking(move || {
                    let applied = peer.apply(&burst, &actions);
                    let _ = tell.send(Told::Applied { id, applied });
                });
            }
            Ask::ReaskContent => self.source.reask(),
            Ask::Status(status) => self.show(status),
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
/// this device's file bytes, which the service's content reads and keeps through them.
#[derive(Default)]
pub struct Daemons {
    connected: Mutex<HashMap<String, Vec<Weak<Lender>>>>,
    /// Called when a daemon connects for a workspace, with what keeps in its store.
    on_connect: Mutex<Option<OnConnect>>,
}

/// Hands a daemon that connected for a workspace what is to be kept in its store.
pub type OnConnect = Arc<dyn Fn(&str, &dyn Fn(&[u8]) -> bool) + Send + Sync>;

impl Daemons {
    /// Call `on_connect` each time a daemon connects for a workspace.
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

    /// The bytes named `hash` a connected daemon of `workspace` keeps, waited for on this thread.
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "a failed step is the assertion"
    )]

    use super::*;
    use crate::content::{ContentPeer, ContentPeers, ContentPiece, ContentScope, ContentShelf};
    use crate::content::{ContentSources, CrdtRoute};

    /// Content nobody keeps or hands over: what a connection that is refused never asks.
    struct Nowhere;

    impl ContentShelf for Nowhere {
        fn read(&self, _scope: &ContentScope, _hash: &str) -> Option<Vec<u8>> {
            None
        }
        fn keep(&self, _scope: &ContentScope, _hash: &str, _bytes: &[u8]) -> Result<(), String> {
            Err("kept nowhere".to_owned())
        }
    }

    impl ContentPeers for Nowhere {
        fn want(&self, _route: &CrdtRoute, _hash: &str, _have: &[neighbour_wire::content::Span]) {}
        fn answer(&self, _route: &CrdtRoute, _peer: ContentPeer, _piece: ContentPiece) {}
        fn lacks(&self, _route: &CrdtRoute, _peer: ContentPeer, _hash: &str) {}
        fn sources(&self, _route: &CrdtRoute) -> ContentSources {
            ContentSources::default()
        }
    }

    /// What a process naming `workspace` is told by a socket whose session recorded `endpoint`.
    async fn told(endpoint: bool, workspace: &str) -> Option<Told> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files-tree.sock");
        let _serving = serve(
            &path,
            ServiceSlot::default(),
            RouteContent::new(Arc::new(Nowhere), Arc::new(Nowhere)),
            Arc::new(move |pid, _| endpoint && pid == std::process::id()),
            Offers {
                daemons: Arc::default(),
                keys: Arc::new(|_| Box::pin(async { Err("no keys here".to_owned()) })),
                access: watch::channel(None).1,
            },
            crate::files::Folders::new(Arc::new(|| {})),
        )
        .unwrap();
        let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
        let (mut reader, mut writer) = stream.into_split();
        FRAMING
            .write_value_async(
                &mut writer,
                &Ask::Hello {
                    workspace: workspace.to_owned(),
                },
            )
            .await
            .unwrap();
        FRAMING.read_value_async(&mut reader).await.unwrap()
    }

    #[tokio::test]
    async fn a_process_the_session_did_not_record_as_files_is_refused() {
        let workspace = uuid::Uuid::new_v4().to_string();
        match told(false, &workspace).await {
            Some(Told::Refused(why)) => assert!(why.contains("not the Files endpoint"), "{why}"),
            other => panic!("an unrecorded process is answered {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_files_endpoint_waits_for_a_service_that_is_not_open() {
        let workspace = uuid::Uuid::new_v4().to_string();
        match told(true, &workspace).await {
            Some(Told::Refused(why)) => assert!(why.contains("not open yet"), "{why}"),
            other => panic!("a connection with no service is answered {other:?}"),
        }
    }
}
