//! This device's service's side of a workspace tree's sync, served on a socket to the files
//! daemon that mounts it ([`tilo_fs::tree_sync::remote`]).
//!
//! The daemon is a process of its own, and this device runs one workspace service: the
//! session's. A connection is taken only from a process of this user that the session recorded
//! as the Files endpoint (`attest`), and it reaches only the Files resources of the one workspace
//! it names first. What it writes, it writes as this device, as the daemon's own mount did when
//! it signed the tree's changes itself.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use fs_protocol::TreeChunk;
use tilo_fs::tree_sync::TreePeer as _;
use tilo_fs::tree_sync::remote::{Ask, FRAMING, Told};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tokio::sync::mpsc;

use super::{Backend, Copy, FILES_APP};
use crate::content::RouteContent;
use crate::service::ServiceSlot;

/// Whether process `pid` is the Files endpoint the session recorded for workspace `workspace`.
pub type Attest = Arc<dyn Fn(u32, &str) -> bool + Send + Sync>;

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
/// admits, with `content` asked again for the bytes whose answer stalled.
///
/// # Errors
/// The socket cannot be bound.
pub fn serve(
    path: &Path,
    service: ServiceSlot,
    content: Arc<RouteContent>,
    attest: Attest,
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
            let (service, content, attest) =
                (service.clone(), Arc::clone(&content), Arc::clone(&attest));
            tokio::spawn(async move {
                if let Err(error) = serve_one(stream, &service, content, &attest).await {
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
    let Backend {
        copy,
        reads,
        writer,
        wakes,
    } = Backend::of(&workspaces, workspace, content, governed);
    Connection {
        copy,
        reads: Arc::new(reads),
        writer: Arc::new(tokio::sync::Mutex::new(writer)),
        named: Arc::new(Mutex::new(BTreeSet::new())),
        told: Arc::new(Mutex::new(None)),
        tell,
    }
    .run(reader, wakes)
    .await
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
}

impl Connection {
    /// Answer what the daemon asks on `reader`, and tell it what `wakes` brings, until either
    /// ends.
    async fn run(
        self,
        mut reader: tokio::net::unix::OwnedReadHalf,
        mut wakes: mpsc::UnboundedReceiver<tilo_fs::tree_sync::Wake>,
    ) -> Result<(), String> {
        let mut look = tokio::time::interval(PEER_EVERY);
        loop {
            tokio::select! {
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

    fn take(&self, ask: Ask) -> Result<(), String> {
        match ask {
            Ask::Hello { .. } => return Err("a connection names its workspace once".to_owned()),
            Ask::Open(chunk) => {
                self.name(chunk);
                self.copy.open(chunk);
            }
            Ask::ReaskContent => self.copy.reask_content(),
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
    let workspace = copy.workspaces.member_caught_up(copy.workspace).is_some();
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
