//! A tree sync whose peer is this device's workspace service, in another process.
//!
//! The session holds the device's one workspace service; the files daemon that mounts a
//! workspace is a process of its own. The session serves the service's side of a tree sync
//! (its reads, its writes as this device, the serving device's word that it holds them, and what
//! changed) on a socket, for the Files endpoint it recorded the daemon's process as, and only
//! for the Files resources of the workspace the daemon names. [`start`] is the daemon's side: the
//! same [`TreeSync`] every folder runs, with the socket as its peer.
//!
//! The same connection carries everything else the daemon needs of the workspace, so the daemon
//! opens nothing else to reach it:
//!
//! - the bytes of files this device lacks, asked of the workspace by the session's record
//!   content ([`Service::source`]), and the bytes the session's content asks of this device's
//!   store, which is the one store a device keeps file bytes in;
//! - the tree's content keys, as the session's identity hands them to the Files endpoint
//!   ([`Service::keys`]);
//! - who may read and write which of the tree's folders, as the session decides it from the
//!   workspace's signed member list, told again the moment that list changes
//!   ([`Service::access`]).
//!
//! The connection comes and goes with the session. While it is down, reads and writes fail and
//! the sync waits as it waits for any peer that cannot be reached; each time it is back, the
//! sync opens every part of the tree again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs_protocol::TreeChunk;
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::DocumentAnswer;
use tokio::sync::{mpsc, oneshot};

use super::{
    Config, FILES_APP, Reading, Records, Served, TreePeer, TreeSync, TreeWriter, Wake, WriteFuture,
};

/// The frames both sides send: a little-endian `u32` length, then one postcard value. A whole
/// part read can be large; this bounds what either side takes.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 << 20);

/// The socket below a session's services the tree is served on.
pub const SOCKET: &str = "files-tree";

/// How long the daemon waits before dialing the session again after its connection ended: the
/// session may restart under a daemon that outlives it.
const RECONNECT: Duration = Duration::from_secs(2);

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
        founding: bool,
        snapshot: bool,
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
        written: Result<(), String>,
    },
    Served {
        id: u64,
        served: Result<(), String>,
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
) -> std::io::Result<(TreeSync, Service)> {
    let link = Arc::new(Link {
        workspace: workspace.clone(),
        out: Mutex::new(None),
        waiting: Mutex::new(BTreeMap::new()),
        next: AtomicU64::new(0),
        state: Mutex::new(PeerState::default()),
        store: Arc::clone(&content),
        access: tokio::sync::watch::channel(None).0,
    });
    let (wakes, wake_events) = mpsc::unbounded_channel();
    tokio::spawn(keep_linked(Arc::clone(&link), socket, wakes));
    let tree = TreeSync::start(Config {
        workspace,
        device,
        person,
        kept_at,
        documents: Arc::new(Remote(Arc::clone(&link))),
        writer: Box::new(Remote(Arc::clone(&link))),
        peer: Arc::new(Remote(Arc::clone(&link))),
        content,
        wakes: wake_events,
    })
    .await?;
    Ok((tree, Service(link)))
}

/// What the session's connection carries beside the tree: file bytes, keys and access.
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

    /// The capability list (JSON) the session last said the mount enforces: `None` until it said
    /// one on this run.
    #[must_use]
    pub fn access(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.0.access.subscribe()
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
    workspace: String,
    out: Mutex<Option<mpsc::UnboundedSender<Ask>>>,
    waiting: Mutex<BTreeMap<u64, oneshot::Sender<Told>>>,
    next: AtomicU64,
    state: Mutex<PeerState>,
    /// This device's store of file bytes, which the session's record content reads and keeps.
    store: Arc<crate::ContentStore>,
    /// The access the session said last.
    access: tokio::sync::watch::Sender<Option<String>>,
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

/// Keep a connection to the session while the sync runs, dialing again whenever one ends.
async fn keep_linked(link: Arc<Link>, socket: PathBuf, wakes: mpsc::UnboundedSender<Wake>) {
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
            tracing::warn!(workspace = link.workspace, %error, "the workspace's tree does not reach the session");
            said = Some(error);
        }
        tokio::time::sleep(RECONNECT).await;
    }
}

/// One connection to the session at `socket`, until it ends.
async fn linked_once(
    link: &Arc<Link>,
    socket: &std::path::Path,
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
    *link.out.lock_recover() = Some(out);
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
    let receive = async {
        loop {
            let told = FRAMING
                .read_value_async::<_, Told>(&mut reader)
                .await
                .map_err(|error| error.to_string())?;
            let Some(told) = told else {
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
    };
    tokio::select! {
        sent = sending => sent,
        received = receive => received,
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
        if workspace != self.0.workspace || app != FILES_APP {
            return Err(format!(
                "this connection syncs {}'s tree, not {app}'s resources of {workspace}",
                self.0.workspace
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
    fn write<'a>(
        &'a mut self,
        chunk: TreeChunk,
        changes: &'a [u8],
        founding: bool,
        snapshot: bool,
    ) -> WriteFuture<'a> {
        let told = self.0.ask(|id| Ask::Write {
            id,
            chunk,
            changes: changes.to_vec(),
            founding,
            snapshot,
        });
        Box::pin(async move {
            match told.ok_or(UNREACHABLE)?.await {
                Ok(Told::Written { written, .. }) => written,
                _ => Err(UNREACHABLE.to_owned()),
            }
        })
    }

    fn served(&self, chunks: &[TreeChunk]) -> Option<Served> {
        let told = self.0.ask(|id| Ask::Served {
            id,
            chunks: chunks.to_vec(),
        });
        Some(Box::pin(async move {
            match told.ok_or(UNREACHABLE)?.await {
                Ok(Told::Served { served, .. }) => served,
                _ => Err(UNREACHABLE.to_owned()),
            }
        }))
    }
}
