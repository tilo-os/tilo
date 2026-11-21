//! The bytes a workspace's files are made of, as this computer keeps and fetches them.
//!
//! A file in the tree names its content by hash: a manifest, and the chunks it lists
//! ([`fs_content`]). This device keeps what it wrote or read in its content store, and its own
//! workspace service routes the rest ([`tilo_workspace_service::content`]): it answers a member
//! that asks the workspace for content from that store (the workspace's server asks for every
//! file a tree names, and keeps it, so a member that comes up later reads it there), and asks
//! the device serving a workspace for what a read here lacks.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use iroh_blobs::Hash;
use tilo_fs::hydration::ContentSource;
use tilo_workspace_service::content::{
    ContentScope, ContentShelf, ContentState, CrdtRoute, RouteContent,
};

use super::tree::FILES_APP;

/// How long a read waits for content the workspace is asked for.
const CONTENT_WAIT: Duration = Duration::from_secs(20);

/// This computer's content store as the service's shelf: the bytes of the files of the
/// workspaces it keeps folders of, and nothing of any other app's records.
pub(crate) struct StoreShelf(pub(crate) Arc<tilo_fs::ContentStore>);

impl StoreShelf {
    /// Run `work` on the store from a thread of its own: the service asks from its own tasks,
    /// and the store is never read by blocking a runtime worker on another runtime.
    fn on_store<T: Send>(
        &self,
        work: impl FnOnce(Arc<tilo_fs::ContentStore>) -> futures_util::future::BoxFuture<'static, T>
        + Send,
    ) -> Option<T> {
        let store = Arc::clone(&self.0);
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .ok()?;
                    Some(runtime.block_on(work(store)))
                })
                .join()
                .ok()
                .flatten()
        })
    }
}

impl ContentShelf for StoreShelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        if scope.app != FILES_APP {
            return None;
        }
        let hash: Hash = hash.parse().ok()?;
        self.on_store(move |store| {
            Box::pin(async move { store.local(hash).await.map(|bytes| bytes.to_vec()) })
        })
        .flatten()
    }

    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        if scope.app != FILES_APP {
            return Err("this computer keeps only the bytes of the workspaces' files".to_owned());
        }
        let hash: Hash = hash
            .parse()
            .map_err(|_| "not a content address".to_owned())?;
        let bytes = Bytes::copy_from_slice(bytes);
        self.on_store(move |store| {
            Box::pin(async move {
                store
                    .store_fetched(hash, bytes)
                    .await
                    .map_err(|error| error.to_string())
            })
        })
        .unwrap_or_else(|| Err("the content store could not be reached".to_owned()))
    }
}

/// One workspace's files as the source of the bytes this device lacks: asked of the device
/// serving the workspace, through the service's record content.
pub(crate) struct WorkspaceSource {
    content: Arc<RouteContent>,
    route: CrdtRoute,
}

impl WorkspaceSource {
    pub(crate) fn new(content: Arc<RouteContent>, workspace: &str) -> Self {
        Self {
            content,
            route: CrdtRoute::app(FILES_APP, workspace),
        }
    }
}

impl ContentSource for WorkspaceSource {
    fn fetch(&self, hash: Hash) -> futures_util::future::BoxFuture<'static, Option<Bytes>> {
        let (content, route) = (Arc::clone(&self.content), self.route.clone());
        Box::pin(async move {
            let hash = hash.to_hex();
            // Subscribed before the want goes out: an answer that comes at once is heard.
            let mut news = content.subscribe(&route);
            {
                let (content, route, hash) = (Arc::clone(&content), route.clone(), hash.clone());
                tokio::task::spawn_blocking(move || content.want(&route, &hash))
                    .await
                    .ok()?;
            }
            tokio::time::timeout(CONTENT_WAIT, async {
                while let Some(news) = news.recv().await {
                    if news.hash != hash {
                        continue;
                    }
                    match news.state {
                        ContentState::Resolved => return Some(Bytes::from(news.bytes)),
                        ContentState::Unavailable => return None,
                        _ => {}
                    }
                }
                None
            })
            .await
            .ok()
            .flatten()
        })
    }
}
