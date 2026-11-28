//! A tree sync whose peer is this device's workspace service, in another process.
//!
//! The device daemon holds the device's one workspace service; the files daemon that mounts a
//! workspace is a process of its own. The device daemon serves the service's side of a tree sync
//! (its reads, its writes as this device, the serving device's word that it holds them, and what
//! changed) on a socket, to processes of its own account, and only for the Files resources of the
//! workspace the files daemon names. [`start`] is the files daemon's side: the same [`TreeSync`]
//! every folder runs, with the socket as its peer.
//!
//! The same connection carries everything else the daemon needs of the workspace, so the daemon
//! opens nothing else to reach it:
//!
//! - the bytes of files this device lacks, asked of the workspace by the session's record
//!   content ([`Service::source`]), and the bytes the session's content asks of this device's
//!   store, which is the one store a device keeps file bytes in;
//! - each version written through the mount ([`Service::written`]), which the session pushes to
//!   the device serving the workspace;
//! - the tree's content keys, as the session's identity hands them to the Files endpoint
//!   ([`Service::keys`]);
//! - who may read and write which of the tree's folders, as the session decides it from the
//!   workspace's signed member list, told again the moment that list changes
//!   ([`Service::access`]).
//!
//! The connection comes and goes with the device daemon. While it is down, reads and writes fail
//! and the sync waits as it waits for any peer that cannot be reached. The daemon binds the socket
//! once it serves, so the socket appearing is its word that it is ready: the files daemon dials
//! when it appears, and again each time the daemon binds it anew, and opens every part of the
//! tree again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use fs_protocol::TreeChunk;
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::DocumentAnswer;
use tokio::sync::{mpsc, oneshot};

use super::{
    Config, FILES_APP, Reading, Records, Served, Status, TreePeer, TreeSync, TreeWriter, Wake,
    WriteFuture,
};

/// The frames both sides send: a little-endian `u32` length, then one postcard value. A whole
/// part read can be large; this bounds what either side takes.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 << 20);

/// What the daemon asks the session.
#[derive(Debug, Serialize, Deserialize)]
pub enum Ask {
    /// The first frame: the workspace whose tree this connection syncs.
    Hello { workspace: String },
    /// Part `resource` of the tree whole, or what changed after `since`.
    Read {
        id: u64,
        resource: String,
        since: Option<Vec<[u8; 32]>>,
    },
    /// Open part `chunk`: the session answers what the workspace has of it.
    Open(TreeChunk),
    /// Write `changes` to part `chunk` as this device.
    Write {
        id: u64,
        chunk: TreeChunk,
        changes: Vec<u8>,
    },
    /// Answer once the serving device holds every earlier write to `chunks`.
    Served { id: u64, chunks: Vec<TreeChunk> },
    /// Ask the workspace again for file content whose answer stalled.
    ReaskContent,
    /// The bytes named `hash`, which this device lacks.
    Want { id: u64, hash: [u8; 32] },
    /// The content keys of `workspace`'s tree.
    Keys { id: u64, workspace: String },
    /// The answer to [`Told::Lend`]: the bytes this device keeps under that hash, if any.
    Lent { id: u64, bytes: Option<Vec<u8>> },
    /// Where the folder stands now, which the session shows: said when it changes, and on each
    /// connection.
    Status(super::Status),
    /// Which workspace this device's session shows: the one whose files its mount serves. Asked
    /// before the first [`Ask::Hello`], and told again whenever the session shows another.
    Shown,
    /// File `file` (its inode in the mount) now holds `version`, written through the mount: the
    /// session pushes it to the device serving the workspace, reading its bytes from this
    /// mount's store, which is the only place they are until that device keeps them.
    Wrote { file: u64, version: [u8; 32] },
}

/// What the session tells the daemon.
#[derive(Debug, Serialize, Deserialize)]
pub enum Told {
    /// The connection is refused, and why.
    Refused(String),
    Read {
        id: u64,
        reading: Result<Reading, String>,
    },
    Written {
        id: u64,
        written: Result<(), super::Refusal>,
    },
    Served {
        id: u64,
        served: Result<(), super::Refusal>,
    },
    /// What the service has now: the parts of the tree it has anything of, among those
    /// this connection named, how many of this device's writes it still keeps, whether this
    /// device holds the workspace, and how many blobs of its content its holders lack.
    Peer {
        holds: Vec<TreeChunk>,
        pending: u64,
        workspace: bool,
        lacks: Option<u32>,
    },
    Wake(Wake),
    /// The answer to [`Ask::Want`]: the bytes, checked against their hash before they are kept,
    /// or none when the workspace did not hand them over.
    Content {
        id: u64,
        bytes: Option<Vec<u8>>,
    },
    /// The answer to [`Ask::Keys`]: the keys' encoding, or why they are not handed over.
    Keys {
        id: u64,
        keys: Result<Vec<u8>, String>,
    },
    /// Who may read and write the tree's folders now: the capability list (JSON) the mount
    /// enforces, replacing the one it was told before.
    Access(String),
    /// The bytes this device keeps under `hash`, for the workspace's record content.
    Lend {
        id: u64,
        hash: [u8; 32],
    },
    /// Keep `bytes` under their hash in this device's store: content the session's record
    /// content received for the tree, or kept before this store was the one.
    Keep {
        bytes: Vec<u8>,
    },
    /// The workspace this device's session shows, as its daemon keeps it: the mount serves this
    /// one's files, and opens its tree once it is named.
    Shown(Option<String>),
}

/// Hold `workspace`'s tree, kept in `kept_at` between runs, as the device whose key is `device`
/// (of person `person`), with the session's service on `socket` as its peer; its file bytes
/// are kept in `content`. The [`Service`] is the rest of what the connection carries.
///
/// # Errors
/// The kept namespace cannot be read.
pub async fn start(
    socket: PathBuf,
    workspace: String,
    (device, person): ([u8; 32], [u8; 32]),
    kept_at: PathBuf,
    content: Arc<crate::ContentStore>,
    path: PathBuf,
) -> std::io::Result<(TreeSync, Service)> {
    let service = link(socket, (device, person), content);
    let tree = service.attach(workspace, kept_at, None, None, path).await?;
    Ok((tree, service))
}

/// Dial the device daemon at `socket` as the device whose key is `device` (of person `person`),
/// and keep the connection up: what the session says of this device's workspaces reaches the
/// [`Service`] from now on, and a tree opens on it with [`Service::attach`] once the workspace
/// this device's session shows is named. Its file bytes are kept in `content`.
#[must_use]
pub fn link(
    socket: PathBuf,
    (device, person): ([u8; 32], [u8; 32]),
    content: Arc<crate::ContentStore>,
) -> Service {
    let link = Arc::new(Link::new(None, Arc::clone(&content)));
    let (wakes, wake_events) = mpsc::unbounded_channel();
    tokio::spawn(keep_linked(Arc::clone(&link), socket, wakes));
    Service {
        link,
        wake_events: Arc::new(Mutex::new(Some(wake_events))),
        device,
        person,
        content,
    }
}

/// Tell the session where the folder stands each time that changes, until the sync is gone: with
/// the mount it stands at, which is the place a file manager shows the workspace by.
async fn tell_status(
    link: Arc<Link>,
    mut changes: tokio::sync::watch::Receiver<Status>,
    path: PathBuf,
) {
    let path = path.display().to_string();
    loop {
        let now = changes.borrow_and_update().clone();
        let now = Status {
            path: path.clone(),
            ..now
        };
        *link.shown.lock_recover() = Some(now.clone());
        link.send(Ask::Status(now));
        if changes.changed().await.is_err() {
            return;
        }
    }
}

/// What the session's connection carries beside the tree: file bytes, keys and access.
#[derive(Clone)]
pub struct Service {
    link: Arc<Link>,
    /// The wakes the attached tree takes, once one is.
    wake_events: Arc<Mutex<Option<mpsc::UnboundedReceiver<Wake>>>>,
    device: [u8; 32],
    person: [u8; 32],
    content: Arc<crate::ContentStore>,
}

impl Service {
    /// Open the tree of `workspace`, kept in `kept_at` (or in `kept` when a mount keeps it
    /// already), on this service's connection: [`start`] for a tree whose workspace is known
    /// before it is dialed.
    ///
    /// `fill`, when named, is the namespace a mount serves before the workspace is known: the
    /// tree's documents replace what it holds, and the mount and the sync share it from then on.
    ///
    /// # Errors
    /// The kept namespace cannot be read, or the tree was attached already.
    pub async fn attach(
        &self,
        workspace: String,
        kept_at: PathBuf,
        fill: Option<Arc<Mutex<crate::namespace::Namespace>>>,
        kept: Option<Arc<crate::kept_documents::KeptDocuments>>,
        path: PathBuf,
    ) -> std::io::Result<TreeSync> {
        *self.link.workspace.lock_recover() = Some(workspace.clone());
        self.link.chosen.notify_waiters();
        let wake_events = self
            .wake_events
            .lock_recover()
            .take()
            .ok_or_else(|| std::io::Error::other("this session's tree was attached already"))?;
        let tree = TreeSync::start(Config {
            workspace,
            device: self.device,
            person: self.person,
            kept_at,
            documents: Arc::new(Remote(Arc::clone(&self.link))),
            writer: Box::new(Remote(Arc::clone(&self.link))),
            peer: Arc::new(Remote(Arc::clone(&self.link))),
            content: Some(Arc::clone(&self.content)),
            wakes: wake_events,
            kept,
            fill,
        })
        .await?;
        tokio::spawn(tell_status(
            Arc::clone(&self.link),
            tree.status_changes(),
            path,
        ));
        Ok(tree)
    }

    /// The workspace this device's session shows, as its daemon said last: `None` until it says
    /// one, and each change after.
    #[must_use]
    pub fn shown(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.link.shown_workspace.subscribe()
    }

    /// The workspace as the source of the bytes this device lacks.
    #[must_use]
    pub fn source(&self) -> Arc<dyn crate::hydration::ContentSource> {
        Arc::new(Remote(Arc::clone(&self.link)))
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
            .link
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

    /// What is told each version written through the mount ([`crate::QuickFS::with_written_content`]):
    /// the session, which pushes it to the device serving the workspace. A version written while
    /// the session is away is said once it is back, only each file's newest.
    #[must_use]
    pub fn written(&self) -> Arc<crate::WrittenContent> {
        let link = Arc::clone(&self.link);
        Arc::new(move |file, version| link.wrote(file, *version.as_bytes()))
    }

    /// The capability list (JSON) the session last said the mount enforces: `None` until it said
    /// one on this run.
    #[must_use]
    pub fn access(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.link.access.subscribe()
    }
}

/// What the session said of the tree's parts, as the sync asks it without waiting.
#[derive(Default)]
struct PeerState {
    answers: BTreeMap<TreeChunk, Option<DocumentAnswer>>,
    holds: BTreeSet<TreeChunk>,
    pending: usize,
    /// Whether this device holds the workspace, as the session last said: `None` before it
    /// said.
    workspace: Option<bool>,
    /// How many blobs of the workspace's content its holders lack, as the session last said.
    lacks: Option<u32>,
}

/// The connection to the session, while there is one.
struct Link {
    /// The workspace whose tree this connection syncs, once it is known: a mount whose session
    /// shows none yet waits for one.
    workspace: Mutex<Option<String>>,
    /// Told when a workspace is named, so a connection waiting to say [`Ask::Hello`] says it.
    chosen: tokio::sync::Notify,
    out: Mutex<Option<mpsc::UnboundedSender<Ask>>>,
    waiting: Mutex<BTreeMap<u64, oneshot::Sender<Told>>>,
    next: AtomicU64,
    state: Mutex<PeerState>,
    /// This device's store of file bytes, which the session's record content reads and keeps.
    store: Arc<crate::ContentStore>,
    /// The access the session said last.
    access: tokio::sync::watch::Sender<Option<String>>,
    /// Where the folder stood when it last said: what a new connection says first.
    shown: Mutex<Option<Status>>,
    /// The workspace this device's session shows, as its daemon said last.
    shown_workspace: tokio::sync::watch::Sender<Option<String>>,
    /// The newest version of each file written while there was no connection to say it on: said
    /// as the next one starts.
    unsaid: Mutex<BTreeMap<u64, [u8; 32]>>,
}

/// What a sync is told when the session is not reachable.
const UNREACHABLE: &str = "the session serving this device's workspaces is not reachable";

impl Link {
    /// A link not connected yet, for `workspace`'s tree once one is named, keeping file bytes in
    /// `store`.
    fn new(workspace: Option<String>, store: Arc<crate::ContentStore>) -> Self {
        Self {
            workspace: Mutex::new(workspace),
            chosen: tokio::sync::Notify::new(),
            out: Mutex::new(None),
            waiting: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(0),
            state: Mutex::new(PeerState::default()),
            store,
            access: tokio::sync::watch::channel(None).0,
            shown: Mutex::new(None),
            shown_workspace: tokio::sync::watch::channel(None).0,
            unsaid: Mutex::new(BTreeMap::new()),
        }
    }

    /// The workspace this connection syncs, for a log line: the one named once the session's
    /// is known.
    fn named(&self) -> String {
        self.workspace
            .lock_recover()
            .clone()
            .unwrap_or_else(|| "no workspace yet".to_owned())
    }

    fn send(&self, ask: Ask) -> bool {
        self.out
            .lock_recover()
            .as_ref()
            .is_some_and(|out| out.send(ask).is_ok())
    }

    /// Say that `file` now holds `version`, or keep it to say on the next connection.
    fn wrote(&self, file: u64, version: [u8; 32]) {
        let mut unsaid = self.unsaid.lock_recover();
        if !self.send(Ask::Wrote { file, version }) {
            unsaid.insert(file, version);
        }
    }

    /// Say on `out` from now on, first what was written while there was no connection.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the unsaid versions stay locked until `out` is the connection, or a version written meanwhile would be kept for a connection that already started"
    )]
    fn connected(&self, out: mpsc::UnboundedSender<Ask>) {
        let mut unsaid = self.unsaid.lock_recover();
        for (file, version) in std::mem::take(&mut *unsaid) {
            let _ = out.send(Ask::Wrote { file, version });
        }
        *self.out.lock_recover() = Some(out);
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
            Told::Peer {
                holds,
                pending,
                workspace,
                lacks,
            } => {
                let holds = holds.iter().copied().collect();
                let pending = usize::try_from(*pending).unwrap_or(usize::MAX);
                let mut state = self.state.lock_recover();
                state.holds = holds;
                state.pending = pending;
                let arrived = state.workspace != Some(*workspace);
                state.workspace = Some(*workspace);
                state.lacks = *lacks;
                drop(state);
                // The workspace came to this device: the tree is read again from the start, as
                // on a connection that just opened, and the folder stops waiting at once.
                if arrived {
                    let _ = wakes.send(Wake::HostReady);
                }
                return Ok(!wakes.is_closed());
            }
            Told::Wake(_) => return Ok(true),
            Told::Access(capabilities) => {
                self.access.send_replace(Some(capabilities.clone()));
                return Ok(true);
            }
            Told::Shown(workspace) => {
                self.shown_workspace.send_replace(workspace.clone());
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
            Told::Read { id, .. }
            | Told::Written { id, .. }
            | Told::Served { id, .. }
            | Told::Content { id, .. }
            | Told::Keys { id, .. } => *id,
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
        for answer in state.answers.values_mut() {
            *answer = None;
        }
        state.holds.clear();
        state.pending = 0;
        state.workspace = None;
        state.lacks = None;
    }
}

/// Keep a connection to the device daemon while the sync runs, dialing again whenever it binds
/// its socket anew after one ended.
async fn keep_linked(link: Arc<Link>, socket: PathBuf, wakes: mpsc::UnboundedSender<Wake>) {
    let _unlinked = Unlinked(Arc::clone(&link));
    let bound = Arc::new(tokio::sync::Notify::new());
    let watching = {
        let (bound, named) = (Arc::clone(&bound), socket.clone());
        socket.parent().map(|dir| {
            tilo_files::native::watch_presence(
                dir,
                move |path| path == named,
                move || {
                    bound.notify_one();
                },
            )
        })
    };
    let _watching = match watching {
        Some(Ok(watching)) => watching,
        Some(Err(error)) => {
            tracing::error!(workspace = link.named(), %error, socket = %socket.display(), "the files daemon cannot follow the device daemon's socket: its tree syncs with nothing until it starts again");
            return;
        }
        None => {
            tracing::error!(workspace = link.named(), socket = %socket.display(), "the device daemon's socket names no directory");
            return;
        }
    };
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
        // A session that is down stays down for a while: it is said once, not at every dial.
        if let Err(error) = ended
            && said.as_ref() != Some(&error)
        {
            tracing::warn!(workspace = link.named(), %error, "the workspace's tree does not reach the device daemon; it is dialed again once the daemon binds its socket");
            said = Some(error);
        }
        tokio::select! {
            () = bound.notified() => {}
            () = wakes.closed() => return,
        }
    }
}

/// The connection lost when the task keeping it is dropped, as a stopping runtime drops it: a
/// read waiting for the session's answer then ends at once instead of waiting for good, as does
/// whoever waits for that read (a stopping runtime waits for its blocking tasks).
struct Unlinked(Arc<Link>);

impl Drop for Unlinked {
    fn drop(&mut self) {
        self.0.lost();
    }
}

/// One connection to the device daemon at `socket`, until it ends.
async fn linked_once(
    link: &Arc<Link>,
    socket: &std::path::Path,
    wakes: &mpsc::UnboundedSender<Wake>,
) -> Result<(), String> {
    let stream = tilo_files::native::socket::connect(socket)
        .and_then(tilo_files::native::socket::tokio::from_std)
        .map_err(|error| format!("connect to {}: {error}", socket.display()))?;
    let (reader, mut writer) = tokio::io::split(stream);
    // The frames are read on a task of their own, each whole: a read dropped between a frame's
    // length and its body while the sender waits for a workspace, or for an answer, would leave
    // the stream reading an answer as a length.
    let mut reading = {
        let (link, wakes) = (Arc::clone(link), mpsc::UnboundedSender::clone(wakes));
        tokio::spawn(async move { read_tolds(reader, &link, &wakes).await })
    };
    // Which workspace this device's session shows comes first: the daemon keeps it, and the
    // tree this mount serves can only be opened once the session said one.
    FRAMING
        .write_value_async(&mut writer, &Ask::Shown)
        .await
        .map_err(|error| error.to_string())?;
    loop {
        // Registered before the workspace is read, so a workspace named as the connection
        // starts is never missed between the check and the wait.
        let chosen = link.chosen.notified();
        tokio::pin!(chosen);
        let named = link.workspace.lock_recover().clone();
        if let Some(workspace) = named {
            FRAMING
                .write_value_async(&mut writer, &Ask::Hello { workspace })
                .await
                .map_err(|error| error.to_string())?;
            break;
        }
        tokio::select! {
            () = &mut chosen => {}
            read = &mut reading => {
                read.map_err(|error| error.to_string())??;
                return Ok(());
            }
        }
    }
    let (out, mut outgoing) = mpsc::unbounded_channel();
    let shown = link.shown.lock_recover().clone();
    if let Some(shown) = shown {
        let _ = out.send(Ask::Status(shown));
    }
    link.connected(out);
    // Whatever was opened before is opened again on this connection.
    let _ = wakes.send(Wake::HostReady);
    let sending = async {
        while let Some(ask) = outgoing.recv().await {
            FRAMING
                .write_value_async(&mut writer, &ask)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    };
    tokio::select! {
        sent = sending => sent,
        read = &mut reading => read.map_err(|error| error.to_string())?,
    }
}

/// Take every answer the session sends on `reader` until the connection ends: each frame whole,
/// on a task of its own.
async fn read_tolds(
    mut reader: tokio::io::ReadHalf<tilo_files::native::socket::tokio::Stream>,
    link: &Arc<Link>,
    wakes: &mpsc::UnboundedSender<Wake>,
) -> Result<(), String> {
    loop {
        let told = FRAMING
            .read_value_async::<_, Told>(&mut reader)
            .await
            .map_err(|error| error.to_string())?;
        let Some(told) = told else {
            // The sender may be waiting for the workspace this connection never named: it is
            // told the connection ended, and dials again.
            link.chosen.notify_waiters();
            return Err("the session closed the connection".to_owned());
        };
        if let Told::Wake(wake) = told {
            if let Wake::Opened { chunk, answer } = &wake {
                link.state
                    .lock_recover()
                    .answers
                    .insert(*chunk, Some(*answer));
            }
            if wakes.send(wake).is_err() {
                return Ok(());
            }
        } else if !link.take(told, wakes)? {
            return Ok(());
        }
    }
}

/// The session's service, as the sync's reader, writer and peer.
struct Remote(Arc<Link>);

impl Records for Remote {
    fn read(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        since: Option<&[[u8; 32]]>,
    ) -> Result<Reading, String> {
        let synced = self.0.named();
        if workspace != synced || app != FILES_APP {
            return Err(format!(
                "this connection syncs {synced}'s tree, not {app}'s resources of {workspace}"
            ));
        }
        let told = self
            .0
            .ask(|id| Ask::Read {
                id,
                resource: document.to_owned(),
                since: since.map(<[_]>::to_vec),
            })
            .ok_or(UNREACHABLE)?;
        match told.blocking_recv() {
            Ok(Told::Read { reading, .. }) => reading,
            _ => Err(UNREACHABLE.to_owned()),
        }
    }
}

impl TreePeer for Remote {
    fn open(&self, chunk: TreeChunk) {
        self.0.state.lock_recover().answers.insert(chunk, None);
        self.0.send(Ask::Open(chunk));
    }

    fn answer(&self, chunk: TreeChunk) -> Option<DocumentAnswer> {
        self.0
            .state
            .lock_recover()
            .answers
            .get(&chunk)
            .copied()
            .flatten()
    }

    fn holds(&self, chunk: TreeChunk) -> bool {
        self.0.state.lock_recover().holds.contains(&chunk)
    }

    /// A write is in the service, and read back, once the session answered it.
    fn holding(&self, _chunk: TreeChunk) -> bool {
        false
    }

    fn pending(&self) -> usize {
        self.0.state.lock_recover().pending
    }

    fn reask_content(&self) {
        self.0.send(Ask::ReaskContent);
    }

    fn holds_workspace(&self) -> Option<bool> {
        self.0.state.lock_recover().workspace
    }

    fn host_lacks(&self) -> Option<u32> {
        self.0.state.lock_recover().lacks
    }
}

impl crate::hydration::ContentSource for Remote {
    fn fetch(
        &self,
        hash: iroh_blobs::Hash,
    ) -> futures_util::future::BoxFuture<'static, Option<bytes::Bytes>> {
        let told = self.0.ask(|id| Ask::Want {
            id,
            hash: *hash.as_bytes(),
        });
        Box::pin(async move {
            match told?.await {
                Ok(Told::Content {
                    bytes: Some(bytes), ..
                }) => Some(bytes.into()),
                _ => None,
            }
        })
    }
}

impl TreeWriter for Remote {
    fn write<'a>(&'a mut self, chunk: TreeChunk, changes: &'a [u8]) -> WriteFuture<'a> {
        let told = self.0.ask(|id| Ask::Write {
            id,
            chunk,
            changes: changes.to_vec(),
        });
        Box::pin(async move {
            match told {
                Some(told) => match told.await {
                    Ok(Told::Written { written, .. }) => written,
                    _ => Err(UNREACHABLE.to_owned().into()),
                },
                None => Err(UNREACHABLE.to_owned().into()),
            }
        })
    }

    fn served(&self, chunks: &[TreeChunk]) -> Option<Served> {
        let told = self.0.ask(|id| Ask::Served {
            id,
            chunks: chunks.to_vec(),
        });
        Some(Box::pin(async move {
            match told {
                Some(told) => match told.await {
                    Ok(Told::Served { served, .. }) => served,
                    _ => Err(UNREACHABLE.to_owned().into()),
                },
                None => Err(UNREACHABLE.to_owned().into()),
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The files daemon dials the device daemon once its socket appears, and again each time the
    /// daemon binds it anew after a connection ended: on the daemon's word, never on a timer.
    #[test]
    fn the_sync_dials_each_time_the_daemon_binds_its_socket() {
        const WITHIN: Duration = Duration::from_secs(10);
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("tree.sock");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let store = runtime
            .block_on(crate::ContentStore::open(dir.path().join("content")))
            .unwrap();
        let link = Arc::new(Link::new(Some("studio".to_owned()), Arc::new(store)));
        let (wakes, _wake_events) = mpsc::unbounded_channel();
        // Started before the daemon serves: nothing to dial yet.
        runtime.spawn(keep_linked(Arc::clone(&link), socket.clone(), wakes));
        for serving in ["its start", "its restart"] {
            let _ = std::fs::remove_file(&socket);
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let (said, hello) = std::sync::mpsc::channel();
            let daemon = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                // The workspace the session shows is asked before the tree's workspace is named.
                assert!(matches!(
                    FRAMING.read_value::<_, Ask>(&mut stream).unwrap(),
                    Some(Ask::Shown)
                ));
                let _ = said.send(FRAMING.read_value::<_, Ask>(&mut stream).unwrap());
            });
            let first = hello
                .recv_timeout(WITHIN)
                .unwrap_or_else(|_| panic!("the sync dials the daemon after {serving}"));
            assert!(
                matches!(first, Some(Ask::Hello { ref workspace }) if workspace == "studio"),
                "{first:?}"
            );
            // The daemon goes away, and the connection with it.
            daemon.join().unwrap();
        }
    }

    /// A read the session never answers ends when the sync's runtime stops: a stopping runtime
    /// waits for its blocking tasks, and a read left waiting would hold it for good.
    #[test]
    fn a_read_the_session_never_answers_ends_with_the_runtime() {
        const WITHIN: Duration = Duration::from_secs(10);
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("tree.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        // The session takes every ask and answers none.
        let (asked, read_asked) = std::sync::mpsc::channel();
        let _session = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            while let Ok(Some(ask)) = FRAMING.read_value::<_, Ask>(&mut stream) {
                if matches!(ask, Ask::Read { .. }) {
                    let _ = asked.send(());
                }
            }
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let store = runtime
            .block_on(crate::ContentStore::open(dir.path().join("content")))
            .unwrap();
        let link = Arc::new(Link::new(Some("studio".to_owned()), Arc::new(store)));
        let (wakes, _wake_events) = mpsc::unbounded_channel();
        runtime.spawn(keep_linked(Arc::clone(&link), socket, wakes));
        let started = std::time::Instant::now();
        while link.out.lock_recover().is_none() {
            assert!(
                started.elapsed() < WITHIN,
                "the sync connects to the session"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        let remote = Remote(Arc::clone(&link));
        let (done, read_done) = std::sync::mpsc::channel();
        let _reader = std::thread::spawn(move || {
            let _ = done.send(remote.read("studio", FILES_APP, "tree", None));
        });
        read_asked
            .recv_timeout(WITHIN)
            .expect("the session is asked the read");
        drop(runtime);
        let read = read_done
            .recv_timeout(WITHIN)
            .expect("the read ends once the runtime stopped");
        assert_eq!(read.err().as_deref(), Some(UNREACHABLE));
    }
}
