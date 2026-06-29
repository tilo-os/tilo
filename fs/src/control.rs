//! Local control socket: a Unix-socket service that lets apps (e.g. an editor)
//! open and **fine-grained-edit** a document stored in the workspace.
//!
//! Whole-file FUSE writes replace a document coarsely (last-writer-wins). This
//! service instead applies structural edit ops directly to the document's CRDT,
//! so several apps — and several peers — converge at the text level. Each edit
//! is broadcast to P2P peers via the namespace sync, and every connected app is
//! notified to re-render.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::namespace::Namespace;
use crate::sync::SyncHandle;

/// A request from an app to the document service.
#[derive(Debug, Serialize, Deserialize)]
pub enum Req {
    /// Resolve a workspace path to a document handle (inode).
    Open { path: String },
    /// Fetch the rendered document for `ino`.
    Snapshot { ino: u64 },
    /// Insert a block after `after` (or at the start), returning its id.
    InsertBlock { ino: u64, after: Option<String>, kind: String },
    RemoveBlock { ino: u64, block: String },
    /// Splice text within a block (`del` chars deleted, `ins` inserted at `idx`).
    Splice { ino: u64, block: String, idx: u64, del: i64, ins: String },
}

/// A reply (or server-initiated push) to an app.
#[derive(Debug, Serialize, Deserialize)]
pub enum Resp {
    Opened { ino: u64 },
    NotFound,
    Snapshot(qdoc::DocSnapshot),
    Inserted { block: String },
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

/// Apply a request under the namespace lock, returning the reply and any CRDT
/// delta to broadcast (empty for read-only requests).
fn apply(req: Req, namespace: &Arc<Mutex<Namespace>>) -> (Resp, Vec<u8>) {
    let mut ns = namespace.lock().unwrap();
    match req {
        Req::Open { path } => {
            let resp = match ns.resolve_path_ino(&path) {
                Some(ino) if ns.document_codec(ino).is_some() => Resp::Opened { ino },
                _ => Resp::NotFound,
            };
            (resp, Vec::new())
        }
        Req::Snapshot { ino } => {
            let resp = match ns.document_snapshot(ino) {
                Some(s) => Resp::Snapshot(s),
                None => Resp::NotFound,
            };
            (resp, Vec::new())
        }
        Req::InsertBlock { ino, after, kind } => {
            let r = ns.document_insert_block(ino, after.as_deref(), &kind);
            let delta = ns.save_incremental();
            let resp = match r {
                Some(block) => Resp::Inserted { block },
                None => Resp::Err("not a document".into()),
            };
            (resp, delta)
        }
        Req::RemoveBlock { ino, block } => {
            ns.document_remove_block(ino, &block);
            (Resp::Ok, ns.save_incremental())
        }
        Req::Splice { ino, block, idx, del, ins } => {
            ns.document_splice(ino, &block, idx as usize, del as isize, &ins);
            (Resp::Ok, ns.save_incremental())
        }
    }
}

async fn handle_conn(
    stream: UnixStream,
    namespace: Arc<Mutex<Namespace>>,
    sync: Option<Arc<SyncHandle>>,
) {
    let (mut rd, mut wr) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Resp>();

    // Single writer for the socket (responses + pushes go through the channel).
    let writer = tokio::spawn(async move {
        while let Some(resp) = rx.recv().await {
            if write_msg(&mut wr, &resp).await.is_err() {
                break;
            }
        }
    });
    // Push `Changed` when the workspace document changes (from this or another
    // app, a FUSE write, or a remote peer) — detected by polling the doc heads.
    let push_tx = tx.clone();
    let poll_ns = namespace.clone();
    let notifier = tokio::spawn(async move {
        let mut last = poll_ns.lock().unwrap().heads();
        loop {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let now = poll_ns.lock().unwrap().heads();
            if now != last {
                last = now;
                if push_tx.send(Resp::Changed).is_err() {
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
        let (resp, delta) = apply(req, &namespace);
        if !delta.is_empty() {
            if let Some(s) = &sync {
                s.broadcast_delta(delta); // propagate the edit to P2P peers
            }
        }
        if tx.send(resp).is_err() {
            break;
        }
    }
    notifier.abort();
    writer.abort();
}

/// Serve the document control protocol on `listener` until it closes.
pub async fn serve(
    listener: UnixListener,
    namespace: Arc<Mutex<Namespace>>,
    sync: Option<Arc<SyncHandle>>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        tokio::spawn(handle_conn(stream, namespace.clone(), sync.clone()));
    }
}

/// A ready-to-use client for the document control service (used by apps like
/// the editor). Request methods skip unsolicited `Changed` pushes; an app that
/// wants live refresh can call [`Client::wait_changed`].
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
        self.stream.write_all(&(bytes.len() as u32).to_le_bytes()).await?;
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
        self.request(&req).await.map(|_| ())
    }

    pub async fn remove_block(&mut self, ino: u64, block: &str) -> std::io::Result<()> {
        self.request(&Req::RemoveBlock { ino, block: block.into() }).await.map(|_| ())
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
    use crate::namespace::ROOT_INO;

    /// Two apps open the same document over the socket and both edit it; each
    /// sees the other's edit (fine-grained collaborative editing via the FS).
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
        tokio::spawn(serve(listener, namespace.clone(), None));

        let mut a = Client::connect(&sock).await.unwrap();
        let mut b = Client::connect(&sock).await.unwrap();

        // both resolve the document handle
        let ino = a.open("doc.md").await.unwrap().expect("document opened");
        b.open("doc.md").await.unwrap();

        // A and B each insert text into the shared paragraph
        a.splice(ino, &bid, 5, 0, " A").await.unwrap();
        b.splice(ino, &bid, 7, 0, " B").await.unwrap();

        // B reads the document and must see both edits
        let snap = b.snapshot(ino).await.unwrap().expect("snapshot");
        let text: String = snap.blocks.iter().map(|bl| bl.text.clone()).collect();
        assert!(text.contains("Hello"), "base text present: {text}");
        assert!(text.contains(" A"), "A's edit visible to B: {text}");
        assert!(text.contains(" B"), "B's edit visible: {text}");
    }
}
