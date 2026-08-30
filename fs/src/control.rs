//! Local control socket: lets apps (e.g. an editor) fine-grained-edit a
//! document in the workspace, applying structural ops to the CRDT instead of
//! FUSE's coarse whole-file replace, so peers converge at the text level.
//! Edits broadcast to P2P peers via namespace sync.
//!
//! It is the same enforcement point as the FUSE mount, not a second door: it
//! consults the same [`WorkspaceAcl`] for the same identity, and answers
//! denials the same way — [`Resp::NotFound`] outside the view (so it can't
//! enumerate what's filtered), [`Resp::Err`] with the read-only-filesystem
//! message for a write without capability, CRDT untouched.
//!
//! An unattached ACL leaves this unenforced too, same as the single-user case
//! in [`crate::acl`]. The socket is 0600 either way.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::acl::{Access, AclHandle, IdentityKey};
use crate::namespace::Namespace;
use crate::sync::SyncHandle;

/// The ACL this service enforces and the identity it answers for — an [`AclHandle`], not a value, so it doesn't freeze mount-time policy.
pub type Enforcement = Option<(AclHandle, IdentityKey)>;

/// The message an edit refused for want of write capability carries — deliberately `EROFS` wording, matching the mount.
const READ_ONLY: &str = "read-only file system";

#[derive(Debug, Serialize, Deserialize)]
pub enum Req {
    Open {
        path: String,
    },
    Snapshot {
        ino: u64,
    },
    /// Insert a block after `after` (or at the start), returning its id.
    InsertBlock {
        ino: u64,
        after: Option<String>,
        kind: String,
    },
    RemoveBlock {
        ino: u64,
        block: String,
    },
    /// Splice text within a block (`del` chars deleted, `ins` inserted at `idx`).
    Splice {
        ino: u64,
        block: String,
        idx: u64,
        del: i64,
        ins: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Resp {
    Opened {
        ino: u64,
    },
    NotFound,
    Snapshot(qdoc::DocSnapshot),
    Inserted {
        block: String,
    },
    Ok,
    Err(String),
    /// Pushed unsolicited when the workspace changed (re-`Snapshot` to refresh).
    Changed,
}

async fn write_msg<W: AsyncWriteExt + Unpin>(w: &mut W, msg: &Resp) -> std::io::Result<()> {
    let bytes = postcard::to_allocvec(msg).map_err(std::io::Error::other)?;
    w.write_all(&(bytes.len() as u32).to_le_bytes()).await?;
    w.write_all(&bytes).await?;
    Ok(())
}

async fn read_msg<R: AsyncReadExt + Unpin, T: DeserializeOwned>(r: &mut R) -> std::io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let n = u32::from_le_bytes(len) as usize;
    if n > 16 * 1024 * 1024 {
        return Err(std::io::Error::other("control frame too large"));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).await?;
    postcard::from_bytes(&buf).map_err(std::io::Error::other)
}

/// The access decision for one handle: `Ok(())`, `Err(Resp::NotFound)` outside the view, or `Err(Resp::Err(READ_ONLY))` if not writable.
fn check(ns: &Namespace, enforcement: &Enforcement, ino: u64, access: Access) -> Result<(), Resp> {
    let Some((acl, identity)) = enforcement else {
        return Ok(());
    };
    let Some(path) = ns.path_for(ino) else {
        return Err(Resp::NotFound);
    };
    if acl.permits(identity, &path, access) {
        return Ok(());
    }
    if access == Access::Write && acl.visible(identity, &path) {
        return Err(Resp::Err(READ_ONLY.into()));
    }
    Err(Resp::NotFound)
}

/// Apply a request under the namespace lock: returns the reply and any CRDT delta to broadcast (empty for reads).
fn apply(
    req: Req,
    namespace: &Arc<Mutex<Namespace>>,
    enforcement: &Enforcement,
) -> (Resp, Vec<u8>) {
    let mut ns = namespace.lock().unwrap();
    match req {
        Req::Open { path } => {
            let resp = match ns.resolve_path_ino(&path) {
                Some(ino)
                    if ns.document_codec(ino).is_some()
                        && check(&ns, enforcement, ino, Access::Read).is_ok() =>
                {
                    Resp::Opened { ino }
                }
                _ => Resp::NotFound,
            };
            (resp, Vec::new())
        }
        Req::Snapshot { ino } => {
            if let Err(resp) = check(&ns, enforcement, ino, Access::Read) {
                return (resp, Vec::new());
            }
            let resp = match ns.document_snapshot(ino) {
                Some(s) => Resp::Snapshot(s),
                None => Resp::NotFound,
            };
            (resp, Vec::new())
        }
        Req::InsertBlock { ino, after, kind } => {
            if let Err(resp) = check(&ns, enforcement, ino, Access::Write) {
                return (resp, Vec::new());
            }
            let r = ns.document_insert_block(ino, after.as_deref(), &kind);
            let delta = ns.save_incremental();
            let resp = match r {
                Some(block) => Resp::Inserted { block },
                None => Resp::Err("not a document".into()),
            };
            (resp, delta)
        }
        Req::RemoveBlock { ino, block } => {
            if let Err(resp) = check(&ns, enforcement, ino, Access::Write) {
                return (resp, Vec::new());
            }
            let ok = ns.document_remove_block(ino, &block);
            let delta = ns.save_incremental();
            let resp = if ok {
                Resp::Ok
            } else {
                Resp::Err("not a document".into())
            };
            (resp, delta)
        }
        Req::Splice {
            ino,
            block,
            idx,
            del,
            ins,
        } => {
            if let Err(resp) = check(&ns, enforcement, ino, Access::Write) {
                return (resp, Vec::new());
            }
            let ok = ns.document_splice(ino, &block, idx as usize, del as isize, &ins);
            let delta = ns.save_incremental();
            let resp = if ok {
                Resp::Ok
            } else {
                Resp::Err("not a document".into())
            };
            (resp, delta)
        }
    }
}

async fn handle_conn(
    stream: UnixStream,
    namespace: Arc<Mutex<Namespace>>,
    sync: Option<Arc<SyncHandle>>,
    enforcement: Arc<Enforcement>,
) {
    let (mut rd, mut wr) = stream.into_split();
    // Bounded so a client that stops reading its socket can't grow the response/notification queue unbounded (daemon OOM).
    let (tx, mut rx) = mpsc::channel::<Resp>(64);

    // Single writer for the socket (responses + pushes go through the channel).
    let writer = tokio::spawn(async move {
        while let Some(resp) = rx.recv().await {
            if write_msg(&mut wr, &resp).await.is_err() {
                break;
            }
        }
    });
    // Push `Changed` on any workspace document change (this app, another, FUSE, or a remote peer) — detected by polling doc heads.
    let push_tx = tx.clone();
    let poll_ns = namespace.clone();
    let notifier = tokio::spawn(async move {
        let mut last = poll_ns.lock().unwrap().heads();
        loop {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let now = poll_ns.lock().unwrap().heads();
            if now != last {
                last = now;
                // Coalesce: drop the signal if the client is backed up (it re-reads latest next time); stop if it's gone.
                if let Err(mpsc::error::TrySendError::Closed(_)) = push_tx.try_send(Resp::Changed) {
                    break;
                }
            }
        }
    });

    loop {
        let req: Req = match read_msg(&mut rd).await {
            Ok(r) => r,
            Err(_) => break, // client disconnected
        };
        let (resp, delta) = apply(req, &namespace, &enforcement);
        if !delta.is_empty()
            && let Some(s) = &sync
        {
            s.broadcast_delta(delta); // propagate the edit to P2P peers
        }
        // Backpressure: awaits if the client is slow (bounds memory).
        if tx.send(resp).await.is_err() {
            break;
        }
    }
    notifier.abort();
    writer.abort();
}

/// Max concurrent app connections — bounds per-connection tasks/polling and the frame buffers a connection flood could pin.
const MAX_CONNECTIONS: usize = 128;

pub async fn serve(
    listener: UnixListener,
    namespace: Arc<Mutex<Namespace>>,
    sync: Option<Arc<SyncHandle>>,
    enforcement: Enforcement,
) {
    let enforcement = Arc::new(enforcement);
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        // Acquire a slot first, so we stop accepting past the cap.
        let Ok(permit) = slots.clone().acquire_owned().await else {
            break;
        };
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let (ns, sync) = (namespace.clone(), sync.clone());
        let enforcement = enforcement.clone();
        tokio::spawn(async move {
            let _permit = permit; // released when the connection ends
            handle_conn(stream, ns, sync, enforcement).await;
        });
    }
}

/// Did an edit apply? A read participant renders as a viewer either way — read-only wording, not a permission error to fix.
fn applied(resp: Resp) -> std::io::Result<()> {
    match resp {
        Resp::Err(msg) => Err(std::io::Error::other(msg)),
        Resp::NotFound => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        _ => Ok(()),
    }
}

/// A ready-to-use client for the document control service; request methods skip unsolicited `Changed` pushes (use [`Client::wait_changed`]).
pub struct Client {
    stream: UnixStream,
}

impl Client {
    pub async fn connect(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        Ok(Self {
            stream: UnixStream::connect(path).await?,
        })
    }

    async fn request(&mut self, req: &Req) -> std::io::Result<Resp> {
        let bytes = postcard::to_allocvec(req).map_err(std::io::Error::other)?;
        self.stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .await?;
        self.stream.write_all(&bytes).await?;
        loop {
            match read_msg(&mut self.stream).await? {
                Resp::Changed => continue, // skip pushes; this is a request/response
                other => return Ok(other),
            }
        }
    }

    pub async fn open(&mut self, path: &str) -> std::io::Result<Option<u64>> {
        match self.request(&Req::Open { path: path.into() }).await? {
            Resp::Opened { ino } => Ok(Some(ino)),
            _ => Ok(None),
        }
    }

    pub async fn snapshot(&mut self, ino: u64) -> std::io::Result<Option<qdoc::DocSnapshot>> {
        match self.request(&Req::Snapshot { ino }).await? {
            Resp::Snapshot(s) => Ok(Some(s)),
            _ => Ok(None),
        }
    }

    pub async fn insert_block(
        &mut self,
        ino: u64,
        after: Option<&str>,
        kind: &str,
    ) -> std::io::Result<Option<String>> {
        let req = Req::InsertBlock {
            ino,
            after: after.map(String::from),
            kind: kind.into(),
        };
        match self.request(&req).await? {
            Resp::Inserted { block } => Ok(Some(block)),
            Resp::Err(msg) => Err(std::io::Error::other(msg)),
            _ => Ok(None),
        }
    }

    pub async fn splice(
        &mut self,
        ino: u64,
        block: &str,
        idx: u64,
        del: i64,
        ins: &str,
    ) -> std::io::Result<()> {
        let req = Req::Splice {
            ino,
            block: block.into(),
            idx,
            del,
            ins: ins.into(),
        };
        applied(self.request(&req).await?)
    }

    pub async fn remove_block(&mut self, ino: u64, block: &str) -> std::io::Result<()> {
        applied(
            self.request(&Req::RemoveBlock {
                ino,
                block: block.into(),
            })
            .await?,
        )
    }

    /// Block until the workspace changes (a server-pushed `Changed`).
    pub async fn wait_changed(&mut self) -> std::io::Result<()> {
        loop {
            if let Resp::Changed = read_msg(&mut self.stream).await? {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkspaceAcl;
    use crate::namespace::ROOT_INO;

    /// Two apps open the same document over the socket and both edit it — each sees the other's edit (collaborative editing via the FS).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_clients_edit_one_document() {
        let namespace = Arc::new(Mutex::new(Namespace::new("server")));
        let bid = {
            let mut ns = namespace.lock().unwrap();
            let ino = ns.create_document(ROOT_INO, "doc.md", "md", 1).unwrap();
            let b = ns.document_insert_block(ino, None, "paragraph").unwrap();
            ns.document_splice(ino, &b, 0, 0, "Hello");
            b
        };

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("ctl.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        tokio::spawn(serve(listener, namespace.clone(), None, None));

        let mut a = Client::connect(&sock).await.unwrap();
        let mut b = Client::connect(&sock).await.unwrap();

        let ino = a.open("doc.md").await.unwrap().expect("document opened");
        b.open("doc.md").await.unwrap();

        a.splice(ino, &bid, 5, 0, " A").await.unwrap();
        b.splice(ino, &bid, 7, 0, " B").await.unwrap();

        let snap = b.snapshot(ino).await.unwrap().expect("snapshot");
        let text: String = snap.blocks.iter().map(|bl| bl.text.clone()).collect();
        assert!(text.contains("Hello"), "base text present: {text}");
        assert!(text.contains(" A"), "A's edit visible to B: {text}");
        assert!(text.contains(" B"), "B's edit visible: {text}");
    }

    /// A document outside the view can't be opened, and can't be told apart from one that doesn't exist (`Open` answers `NotFound` either way).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_control_socket_is_not_a_way_around_the_view() {
        use collab_types::{CapOverride, Ed25519PublicKey, Effect, Scope, Subject, Verb};

        const ALICE: IdentityKey = [0xA1; 32];

        let namespace = Arc::new(Mutex::new(Namespace::new("server")));
        let hidden_block = {
            let mut ns = namespace.lock().unwrap();
            let shared = ns.create_dir(ROOT_INO, "shared", 1).unwrap();
            let visible = ns.create_document(shared, "notes.md", "md", 1).unwrap();
            let b = ns
                .document_insert_block(visible, None, "paragraph")
                .unwrap();
            ns.document_splice(visible, &b, 0, 0, "Shared");
            let secret_dir = ns.create_dir(ROOT_INO, "secret", 1).unwrap();
            let secret = ns.create_document(secret_dir, "plan.md", "md", 1).unwrap();
            let sb = ns.document_insert_block(secret, None, "paragraph").unwrap();
            ns.document_splice(secret, &sb, 0, 0, "Confidential");
            sb
        };

        let acl = WorkspaceAcl::from_capabilities(
            ALICE,
            &[CapOverride::new(
                Subject::User(Ed25519PublicKey(ALICE)),
                Scope::FsPath("/shared".into()),
                Effect::Grant,
                Verb::Read,
            )],
            &[],
            None,
        );

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("ctl.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        tokio::spawn(serve(
            listener,
            namespace.clone(),
            None,
            Some((acl.into(), ALICE)),
        ));
        let mut c = Client::connect(&sock).await.unwrap();

        let visible = c
            .open("/shared/notes.md")
            .await
            .unwrap()
            .expect("a granted document opens");
        let snap = c.snapshot(visible).await.unwrap().expect("and reads");
        assert!(snap.blocks.iter().any(|b| b.text.contains("Shared")));

        // The one outside the view does not — answers exactly as a path that was never created.
        assert_eq!(
            c.open("/secret/plan.md").await.unwrap(),
            None,
            "a document outside the view must not open"
        );
        assert_eq!(
            c.open("/never/existed.md").await.unwrap(),
            None,
            "and a path that never existed answers the same way"
        );

        // Nor is the handle reachable by guessing an inode: a real-but-hidden handle replies the same as a nonsense one.
        let secret_ino = namespace
            .lock()
            .unwrap()
            .resolve_path_ino("/secret/plan.md")
            .expect("the fixture really has it");
        assert!(
            c.snapshot(secret_ino).await.unwrap().is_none(),
            "a snapshot of a hidden document must be refused"
        );
        assert!(
            c.snapshot(9_999_999).await.unwrap().is_none(),
            "and a handle that names nothing is refused identically"
        );

        // Editing it must change nothing — this is the fine-grained authoring path a read participant must not have.
        let heads = namespace.lock().unwrap().heads();
        assert!(
            c.splice(secret_ino, &hidden_block, 0, 0, "TAMPERED")
                .await
                .is_err()
                || namespace.lock().unwrap().heads() == heads,
            "an edit to a hidden document must not reach the CRDT"
        );
        assert_eq!(
            namespace.lock().unwrap().heads(),
            heads,
            "the change graph must not have advanced"
        );
        let text: String = namespace
            .lock()
            .unwrap()
            .document_snapshot(secret_ino)
            .expect("still there")
            .blocks
            .iter()
            .map(|b| b.text.clone())
            .collect();
        assert_eq!(
            text, "Confidential",
            "the hidden document is byte-for-byte as it was"
        );
    }

    /// A read participant sees the document but cannot author it — this is where "read participation" either holds or does not.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_read_participant_cannot_author_over_the_socket() {
        use collab_types::{CapOverride, Ed25519PublicKey, Effect, Scope, Subject, Verb};

        const ALICE: IdentityKey = [0xA1; 32];

        let namespace = Arc::new(Mutex::new(Namespace::new("server")));
        let block = {
            let mut ns = namespace.lock().unwrap();
            let shared = ns.create_dir(ROOT_INO, "shared", 1).unwrap();
            let ino = ns.create_document(shared, "notes.md", "md", 1).unwrap();
            let b = ns.document_insert_block(ino, None, "paragraph").unwrap();
            ns.document_splice(ino, &b, 0, 0, "Shared paragraph");
            b
        };

        let read_cap = CapOverride::new(
            Subject::User(Ed25519PublicKey(ALICE)),
            Scope::FsPath("/shared".into()),
            Effect::Grant,
            Verb::Read,
        );
        let acl = WorkspaceAcl::from_capabilities(ALICE, &[read_cap], &[], None);

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("ro.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        tokio::spawn(serve(
            listener,
            namespace.clone(),
            None,
            Some((acl.into(), ALICE)),
        ));
        let mut c = Client::connect(&sock).await.unwrap();

        let ino = c.open("/shared/notes.md").await.unwrap().expect("opens");
        let snap = c.snapshot(ino).await.unwrap().expect("reads");
        assert!(
            snap.blocks
                .iter()
                .any(|b| b.text.contains("Shared paragraph")),
            "read participation is participation: the real content is served"
        );

        let heads = namespace.lock().unwrap().heads();
        assert!(c.splice(ino, &block, 0, 0, "MINE").await.is_err());
        assert!(c.insert_block(ino, None, "paragraph").await.is_err());
        assert!(c.remove_block(ino, &block).await.is_err());
        assert_eq!(
            namespace.lock().unwrap().heads(),
            heads,
            "no authoring op may advance the change graph"
        );

        // The same document with a write grant does accept the edit, so the refusals above are the capability, not a broken fixture.
        let write_cap = CapOverride::new(
            Subject::User(Ed25519PublicKey(ALICE)),
            Scope::FsPath("/shared".into()),
            Effect::Grant,
            Verb::Write,
        );
        let rw = WorkspaceAcl::from_capabilities(ALICE, &[write_cap], &[], None);
        let sock2 = dir.path().join("rw.sock");
        let listener2 = UnixListener::bind(&sock2).unwrap();
        tokio::spawn(serve(
            listener2,
            namespace.clone(),
            None,
            Some((rw.into(), ALICE)),
        ));
        let mut w = Client::connect(&sock2).await.unwrap();
        let ino = w.open("/shared/notes.md").await.unwrap().expect("opens");
        w.splice(ino, &block, 0, 0, "OURS ")
            .await
            .expect("an authoring participant may edit");
        assert_ne!(
            namespace.lock().unwrap().heads(),
            heads,
            "an authored edit must advance the change graph"
        );
    }

    /// The full story: an editor on peer A edits through the control socket; it propagates over P2P; an editor on peer B sees it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn edit_via_control_reaches_a_remote_editor() {
        use crate::sync::{Workspace, bind_ephemeral_endpoint, fetch_remote_doc};
        use crate::{ContentStore, Hydrator};
        use std::time::Duration;

        let (ep_a, look_a) = bind_ephemeral_endpoint().await.unwrap();
        let store_a = Arc::new(ContentStore::memory());
        let hyd_a = Arc::new(Hydrator::new(ep_a.clone(), store_a.clone()));
        let ns_a = Arc::new(Mutex::new(Namespace::new("A")));
        let bid = {
            let mut n = ns_a.lock().unwrap();
            let ino = n.create_document(ROOT_INO, "doc.md", "md", 1).unwrap();
            let b = n.document_insert_block(ino, None, "paragraph").unwrap();
            n.document_splice(ino, &b, 0, 0, "base");
            b
        };
        let (ws_a, ticket) = Workspace::host(
            ep_a.clone(),
            look_a,
            store_a.clone(),
            hyd_a.clone(),
            ns_a.clone(),
            None,
        )
        .await
        .unwrap();

        let (ep_b, look_b) = bind_ephemeral_endpoint().await.unwrap();
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
            None,
        )
        .await
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let sock_a = dir.path().join("a.sock");
        let sock_b = dir.path().join("b.sock");
        tokio::spawn(serve(
            UnixListener::bind(&sock_a).unwrap(),
            ns_a.clone(),
            Some(ws_a.sync.clone()),
            None,
        ));
        tokio::spawn(serve(
            UnixListener::bind(&sock_b).unwrap(),
            ns_b.clone(),
            Some(ws_b.sync.clone()),
            None,
        ));

        let mut ed_a = Client::connect(&sock_a).await.unwrap();
        let ino_a = ed_a.open("doc.md").await.unwrap().unwrap();
        ed_a.splice(ino_a, &bid, 4, 0, " +A").await.unwrap(); // "base +A"

        // editor on B must see it after P2P propagation
        let mut ed_b = Client::connect(&sock_b).await.unwrap();
        let ino_b = ed_b.open("doc.md").await.unwrap().unwrap();
        let mut seen = false;
        for _ in 0..50 {
            let snap = ed_b.snapshot(ino_b).await.unwrap().unwrap();
            let text: String = snap.blocks.iter().map(|b| b.text.clone()).collect();
            if text.contains("+A") {
                seen = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            seen,
            "A's control-socket edit must reach B's editor over P2P"
        );
        let _ = (ws_a, ws_b); // keep the workspaces alive
    }
}
