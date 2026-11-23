//! A tree's copy whose store is this device's workspace service, in another process.
//!
//! The session holds the device's one workspace service; the files daemon that mounts a
//! workspace is a process of its own. The session serves the store's side of a workspace's tree
//! on a socket, for the Files endpoint it recorded the daemon's process as, and only for the
//! workspace the daemon names. [`start`] is the daemon's side: a [`StoreSync`] whose pages and
//! verbs cross the socket.
//!
//! The same connection carries everything else the daemon needs of the workspace: the bytes of
//! files this device lacks, the tree's content keys, who may read and write its folders, and the
//! one store of file bytes this device keeps.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs_protocol::FileAction;
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tokio::sync::{mpsc, oneshot};

use super::{Config, NodePage, Peer, SOCKET, Status, StoreSync, Wake};

/// The frames both sides send: a little-endian `u32` length, then one postcard value.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 << 20);

/// How long the daemon waits before dialing the session again after its connection ended.
const RECONNECT: Duration = Duration::from_secs(2);

/// What the daemon asks the session.
#[derive(Debug, Serialize, Deserialize)]
pub enum Ask {
    /// The first frame: the workspace whose tree this connection syncs.
    Hello { workspace: String },
    /// Retired: a record of the tree was read.
    RetiredRead,
    /// Retired: a record was opened.
    RetiredOpen,
    /// Retired: a record was written.
    RetiredWrite,
    /// Retired: a record's write was waited for.
    RetiredServed,
    /// Ask the workspace again for file content whose answer stalled.
    ReaskContent,
    /// The bytes named `hash`, which this device lacks.
    Want { id: u64, hash: [u8; 32] },
    /// The content keys of the tree's workspace.
    Keys { id: u64, workspace: String },
    /// The answer to [`Told::Lend`]: the bytes this device keeps under that hash, if any.
    Lent { id: u64, bytes: Option<Vec<u8>> },
    /// Where the folder stands now, which the session shows.
    Status(Status),
    /// One page of the nodes directly under `parent`.
    List {
        id: u64,
        parent: String,
        after: Option<Vec<u8>>,
        limit: usize,
    },
    /// How many nodes are directly under `parent`.
    Count { id: u64, parent: String },
    /// Write `actions` as one burst named `burst`, as this device.
    Apply {
        id: u64,
        burst: String,
        actions: Vec<FileAction>,
    },
}

/// What the session tells the daemon.
#[derive(Debug, Serialize, Deserialize)]
pub enum Told {
    /// The connection is refused, and why.
    Refused(String),
    /// Retired: a record of the tree was read.
    RetiredRead,
    /// Retired: a record was written.
    RetiredWritten,
    /// Retired: a record's write was waited for.
    RetiredServed,
    /// Whether this device holds the workspace, and how many blobs of its content its holders
    /// lack.
    Peer { held: bool, lacks: Option<u32> },
    /// Retired: a wake named a record.
    RetiredWake,
    /// The answer to [`Ask::Want`]: the bytes, or none when the workspace did not hand them over.
    Content { id: u64, bytes: Option<Vec<u8>> },
    /// The answer to [`Ask::Keys`]: the keys' encoding, or why they are not handed over.
    Keys {
        id: u64,
        keys: Result<Vec<u8>, String>,
    },
    /// Who may read and write the tree's folders now: the capability list (JSON).
    Access(String),
    /// The bytes this device keeps under `hash`, for the workspace's record content.
    Lend { id: u64, hash: [u8; 32] },
    /// Keep `bytes` under their hash in this device's store.
    Keep { bytes: Vec<u8> },
    /// One page of the nodes directly under `parent`.
    List {
        id: u64,
        page: Result<NodePage, String>,
    },
    /// How many nodes are directly under `parent`.
    Count {
        id: u64,
        count: Result<usize, String>,
    },
    /// The answer to [`Ask::Apply`]: whether the store took the burst.
    Applied {
        id: u64,
        applied: Result<(), String>,
    },
    /// A node of the tree changed in the store: read the tree again.
    Changed,
}

/// Hold `workspace`'s tree as `mount`, with the session's service on `socket` as its store; its
/// file bytes are kept in `content`.
///
/// # Errors
/// The socket cannot be dialled at all is not an error: the sync waits and dials again.
pub async fn start(
    socket: PathBuf,
    workspace: String,
    mount: crate::namespace::MountAuthor,
    content: Arc<crate::ContentStore>,
) -> std::io::Result<(StoreSync, Service)> {
    let link = Arc::new(Link {
        workspace: workspace.clone(),
        out: Mutex::new(None),
        waiting: Mutex::new(BTreeMap::new()),
        next: AtomicU64::new(0),
        state: Mutex::new(PeerState::default()),
        store: content,
        access: tokio::sync::watch::channel(None).0,
        shown: Mutex::new(None),
    });
    let (wakes, wake_events) = mpsc::unbounded_channel();
    tokio::spawn(keep_linked(Arc::clone(&link), socket, wakes));
    let tree = StoreSync::start(Config {
        workspace,
        mount,
        peer: Arc::new(Remote(Arc::clone(&link))),
        wakes: wake_events,
    })
    .map_err(std::io::Error::other)?;
    tokio::spawn(tell_status(Arc::clone(&link), tree.status_changes()));
    Ok((tree, Service(link)))
}

async fn tell_status(link: Arc<Link>, mut changes: tokio::sync::watch::Receiver<Status>) {
    loop {
        let now = changes.borrow_and_update().clone();
        *link.shown.lock_recover() = Some(now.clone());
        link.send(Ask::Status(now));
        if changes.changed().await.is_err() {
            return;
        }
    }
}

/// What the session's connection carries beside the tree.
#[derive(Clone)]
pub struct Service(Arc<Link>);

impl Service {
    /// The workspace as the source of the bytes this device lacks.
    #[must_use]
    pub fn source(&self) -> Arc<dyn crate::hydration::ContentSource> {
        Arc::new(Remote(Arc::clone(&self.0)))
    }

    /// The content keys of `workspace`'s tree, as the session hands them to this Files endpoint.
    ///
    /// # Errors
    /// The session is not reachable, or refused the keys.
    pub async fn keys(
        &self,
        workspace: collab_types::WorkspaceId,
    ) -> anyhow::Result<key_group::ContentKeys> {
        let told = self
            .0
            .ask(|id| Ask::Keys {
                id,
                workspace: workspace.to_string(),
            })
            .ok_or_else(|| anyhow::anyhow!(UNREACHABLE))?;
        match told.await {
            Ok(Told::Keys {
                keys: Ok(bytes), ..
            }) => {
                let bytes = zeroize::Zeroizing::new(bytes);
                Ok(key_group::ContentKeys::from_bytes(&bytes)?)
            }
            Ok(Told::Keys { keys: Err(why), .. }) => Err(anyhow::anyhow!(why)),
            _ => Err(anyhow::anyhow!(UNREACHABLE)),
        }
    }

    /// The capability list (JSON) the session last said the mount enforces.
    #[must_use]
    pub fn access(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.0.access.subscribe()
    }
}

/// What the session said of the tree, as the sync asks it without waiting.
#[derive(Default)]
struct PeerState {
    /// Whether this device holds the workspace, as the session last said.
    held: Option<bool>,
    /// How many blobs of the workspace's content its holders lack.
    lacks: Option<u32>,
}

/// The connection to the session, while there is one.
struct Link {
    workspace: String,
    out: Mutex<Option<mpsc::UnboundedSender<Ask>>>,
    waiting: Mutex<BTreeMap<u64, oneshot::Sender<Told>>>,
    next: AtomicU64,
    state: Mutex<PeerState>,
    /// This device's store of file bytes, which the session's content reads and keeps.
    store: Arc<crate::ContentStore>,
    access: tokio::sync::watch::Sender<Option<String>>,
    /// Where the folder stood when it last said: what a new connection says first.
    shown: Mutex<Option<Status>>,
}

/// What a sync is told when the session is not reachable.
const UNREACHABLE: &str = "the session serving this device's workspaces is not reachable";

impl Link {
    fn send(&self, ask: Ask) -> bool {
        self.out
            .lock_recover()
            .as_ref()
            .is_some_and(|out| out.send(ask).is_ok())
    }

    /// Send the ask `make` builds and the answer it waits for; `None` while there is no
    /// connection.
    fn ask(&self, make: impl FnOnce(u64) -> Ask) -> Option<oneshot::Receiver<Told>> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tell, told) = oneshot::channel();
        self.waiting.lock_recover().insert(id, tell);
        if self.send(make(id)) {
            Some(told)
        } else {
            self.waiting.lock_recover().remove(&id);
            None
        }
    }

    /// Take one answer or state of the session's: `false` once the sync is gone.
    fn take(&self, told: Told, wakes: &mpsc::UnboundedSender<Wake>) -> Result<bool, String> {
        let id = match &told {
            Told::Refused(why) => return Err(why.clone()),
            Told::Changed => {
                let _ = wakes.send(Wake::Changed);
                return Ok(true);
            }
            Told::Peer { held, lacks } => {
                let mut state = self.state.lock_recover();
                state.held = Some(*held);
                state.lacks = *lacks;
                drop(state);
                let _ = wakes.send(Wake::Changed);
                return Ok(true);
            }
            Told::Access(capabilities) => {
                self.access.send_replace(Some(capabilities.clone()));
                return Ok(true);
            }
            Told::Lend { id, hash } => {
                self.lend(*id, *hash);
                return Ok(true);
            }
            Told::Keep { bytes } => {
                self.keep(bytes.clone());
                return Ok(true);
            }
            Told::List { id, .. }
            | Told::Count { id, .. }
            | Told::Applied { id, .. }
            | Told::Content { id, .. }
            | Told::Keys { id, .. } => *id,
            Told::RetiredRead | Told::RetiredWritten | Told::RetiredServed | Told::RetiredWake => {
                return Ok(true);
            }
        };
        let waiting = self.waiting.lock_recover().remove(&id);
        if let Some(waiting) = waiting {
            let _ = waiting.send(told);
        }
        Ok(!wakes.is_closed())
    }

    /// Answer the session's ask for the bytes kept here under `hash`.
    fn lend(&self, id: u64, hash: [u8; 32]) {
        let store = Arc::clone(&self.store);
        let out = self.out.lock_recover().clone();
        tokio::spawn(async move {
            let bytes = store
                .local(iroh_blobs::Hash::from_bytes(hash))
                .await
                .map(|bytes| bytes.to_vec());
            if let Some(out) = out {
                let _ = out.send(Ask::Lent { id, bytes });
            }
        });
    }

    /// Keep what the session hands over for the tree, checked against its hash.
    fn keep(&self, bytes: Vec<u8>) {
        let store = Arc::clone(&self.store);
        tokio::spawn(async move {
            let hash = iroh_blobs::Hash::new(&bytes);
            if let Err(error) = store.store_fetched(hash, bytes.into()).await {
                tracing::debug!(%hash, %error, "the session's content for the tree is not kept");
            }
        });
    }

    /// The connection ended: nothing waits for its answers, and what it said is unsaid.
    fn lost(&self) {
        *self.out.lock_recover() = None;
        self.waiting.lock_recover().clear();
        let mut state = self.state.lock_recover();
        state.held = None;
        state.lacks = None;
    }
}

/// Keep a connection to the session while the sync runs, dialing again whenever one ends.
async fn keep_linked(link: Arc<Link>, socket: PathBuf, wakes: mpsc::UnboundedSender<Wake>) {
    let _unlinked = Unlinked(Arc::clone(&link));
    let mut said: Option<String> = None;
    loop {
        let ended = tokio::select! {
            ended = linked_once(&link, &socket, &wakes) => ended,
            () = wakes.closed() => Ok(()),
        };
        link.lost();
        if wakes.is_closed() {
            return;
        }
        if let Err(error) = ended
            && said.as_ref() != Some(&error)
        {
            tracing::warn!(workspace = link.workspace, %error, "the workspace's tree does not reach the session");
            said = Some(error);
        }
        tokio::time::sleep(RECONNECT).await;
    }
}

/// The connection lost when the task keeping it is dropped.
struct Unlinked(Arc<Link>);

impl Drop for Unlinked {
    fn drop(&mut self) {
        self.0.lost();
    }
}

/// One connection to the session at `socket`, until it ends.
async fn linked_once(
    link: &Arc<Link>,
    socket: &Path,
    wakes: &mpsc::UnboundedSender<Wake>,
) -> Result<(), String> {
    let stream = tilo_files::native::socket::connect(socket)
        .and_then(|stream| {
            stream.set_nonblocking(true)?;
            tokio::net::UnixStream::from_std(stream)
        })
        .map_err(|error| format!("connect to {}: {error}", socket.display()))?;
    let (mut reader, mut writer) = stream.into_split();
    FRAMING
        .write_value_async(
            &mut writer,
            &Ask::Hello {
                workspace: link.workspace.clone(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let (out, mut outgoing) = mpsc::unbounded_channel();
    let shown = link.shown.lock_recover().clone();
    if let Some(shown) = shown {
        let _ = out.send(Ask::Status(shown));
    }
    *link.out.lock_recover() = Some(out);
    // The store may have changed while the connection was down.
    let _ = wakes.send(Wake::Changed);
    let sending = async {
        while let Some(ask) = outgoing.recv().await {
            FRAMING
                .write_value_async(&mut writer, &ask)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    };
    let receive = async {
        loop {
            let told = FRAMING
                .read_value_async::<_, Told>(&mut reader)
                .await
                .map_err(|error| error.to_string())?;
            let Some(told) = told else {
                return Err("the session closed the connection".to_owned());
            };
            if !link.take(told, wakes)? {
                return Ok(());
            }
        }
    };
    tokio::select! {
        sent = sending => sent,
        received = receive => received,
    }
}

/// The session's store, as the sync's peer and the content's source.
struct Remote(Arc<Link>);

impl Peer for Remote {
    fn list(&self, parent: &str, after: Option<Vec<u8>>, limit: usize) -> Result<NodePage, String> {
        let told = self
            .0
            .ask(|id| Ask::List {
                id,
                parent: parent.to_owned(),
                after: after.clone(),
                limit,
            })
            .ok_or(UNREACHABLE)?;
        match told.blocking_recv() {
            Ok(Told::List { page, .. }) => page,
            _ => Err(UNREACHABLE.to_owned()),
        }
    }

    fn count(&self, parent: &str) -> Result<usize, String> {
        let told = self
            .0
            .ask(|id| Ask::Count {
                id,
                parent: parent.to_owned(),
            })
            .ok_or(UNREACHABLE)?;
        match told.blocking_recv() {
            Ok(Told::Count { count, .. }) => count,
            _ => Err(UNREACHABLE.to_owned()),
        }
    }

    fn apply(&self, burst: &str, actions: &[FileAction]) -> Result<(), String> {
        let told = self
            .0
            .ask(|id| Ask::Apply {
                id,
                burst: burst.to_owned(),
                actions: actions.to_vec(),
            })
            .ok_or(UNREACHABLE)?;
        match told.blocking_recv() {
            Ok(Told::Applied { applied, .. }) => applied,
            _ => Err(UNREACHABLE.to_owned()),
        }
    }

    fn held(&self) -> Option<bool> {
        self.0.state.lock_recover().held
    }

    fn host_lacks(&self) -> Option<u32> {
        self.0.state.lock_recover().lacks
    }

    fn reask_content(&self) {
        self.0.send(Ask::ReaskContent);
    }
}

impl crate::hydration::ContentSource for Remote {
    fn fetch(
        &self,
        hash: iroh_blobs::Hash,
    ) -> futures_util::future::BoxFuture<'static, Option<bytes::Bytes>> {
        let link = Arc::clone(&self.0);
        Box::pin(async move {
            let told = link.ask(|id| Ask::Want {
                id,
                hash: *hash.as_bytes(),
            })?;
            match told.await {
                Ok(Told::Content { bytes, .. }) => bytes.map(bytes::Bytes::from),
                _ => None,
            }
        })
    }
}

/// The socket a session serves a workspace's tree on, below `services`.
#[must_use]
pub fn socket_path(services: &Path) -> PathBuf {
    services.join(format!("{SOCKET}.sock"))
}
