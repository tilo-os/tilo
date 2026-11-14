//! The bytes a workspace's files are made of, as this computer keeps and
//! fetches them.
//!
//! A file in the tree names its content by hash: a manifest, and the chunks it
//! lists ([`fs_content`]). This device keeps what it wrote or read in its
//! content store, and answers a member that asks the workspace for any of it
//! (the workspace's host asks for every file a tree names, and keeps it, so a
//! member that comes up later reads it there). What a read here needs and this
//! device lacks it asks the workspace for, over the same session, as a page
//! asks for a record's content.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use iroh_blobs::Hash;
use neighbour_wire::content::ContentWants;
use tilo_fs::hydration::ContentSource;
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;
use tilo_session_client::{Cmd, Link, Outbox as _};

use super::tree::FILES_APP;

/// How long a read waits for content the workspace is asked for.
const CONTENT_WAIT: Duration = Duration::from_secs(20);

/// One workspace's file content, between this device's store and its session.
pub(crate) struct SessionContent {
    workspace: WorkspaceId,
    device: [u8; 32],
    link: Link,
    store: Arc<tilo_fs::ContentStore>,
    wants: Mutex<ContentWants<()>>,
    waiting: Mutex<HashMap<String, Vec<tokio::sync::oneshot::Sender<Bytes>>>>,
}

impl SessionContent {
    pub(crate) fn new(
        workspace: WorkspaceId,
        device: [u8; 32],
        link: &Link,
        store: Arc<tilo_fs::ContentStore>,
    ) -> Arc<Self> {
        let content = Arc::new(Self {
            workspace,
            device,
            link: link.clone(),
            store,
            wants: Mutex::new(ContentWants::default()),
            waiting: Mutex::new(HashMap::new()),
        });
        let _ = link.send(Cmd::KeepContent(
            workspace,
            Arc::new(Answering(Arc::downgrade(&content))),
        ));
        content
    }

    /// A piece of content a member sent this device.
    pub(crate) fn piece(&self, frame: &[u8]) {
        let Some(arrived) =
            self.wants
                .lock_recover()
                .piece(&self.device, frame, tilo_clock::unix_now_ms())
        else {
            return;
        };
        let bytes = Bytes::from(arrived.bytes);
        let waiters = self
            .waiting
            .lock_recover()
            .remove(&arrived.hash)
            .unwrap_or_default();
        for waiter in waiters {
            let _ = waiter.send(bytes.clone());
        }
    }

    /// Ask again for content whose answer stalled.
    pub(crate) fn reask(&self, link: &Link) {
        let asks = self
            .wants
            .lock_recover()
            .reask(&self.device, tilo_clock::unix_now_ms());
        for ((), ask) in asks {
            let _ = link.send(Cmd::WantContent(self.workspace, ask));
        }
    }

    /// The bytes kept here under `hash` (hex), read off the store's own runtime.
    fn kept(&self, hash: &str) -> Option<Vec<u8>> {
        let hash: Hash = hash.parse().ok()?;
        let store = Arc::clone(&self.store);
        // The session asks from its own task: the store is read on a thread
        // of its own, never by blocking a runtime worker on another runtime.
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .ok()?;
                    runtime
                        .block_on(store.local(hash))
                        .map(|bytes| bytes.to_vec())
                })
                .join()
                .ok()
                .flatten()
        })
    }
}

/// What the session answers members with: what this device keeps.
struct Answering(std::sync::Weak<SessionContent>);

impl tilo_session_client::content::Kept for Answering {
    fn read(&self, app: &str, workspace: &str, hash: &str) -> Option<Vec<u8>> {
        let content = self.0.upgrade()?;
        if app != FILES_APP || !workspace.eq_ignore_ascii_case(&content.workspace.to_string()) {
            return None;
        }
        content.kept(hash)
    }
}

impl ContentSource for SessionContent {
    fn fetch(&self, hash: Hash) -> futures_util::future::BoxFuture<'static, Option<Bytes>> {
        let hex = hash.to_hex();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.waiting
            .lock_recover()
            .entry(hex.clone())
            .or_default()
            .push(sender);
        let ask = self.wants.lock_recover().ask(
            &self.device,
            (),
            (FILES_APP, &self.workspace.to_string()),
            &hex,
            tilo_clock::unix_now_ms(),
        );
        if let Some(ask) = ask {
            let _ = self.link.send(Cmd::WantContent(self.workspace, ask));
        }
        Box::pin(async move {
            tokio::time::timeout(CONTENT_WAIT, receiver)
                .await
                .ok()?
                .ok()
        })
    }
}
