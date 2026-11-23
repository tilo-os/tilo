//! A workspace's file tree on a device: its nodes in this device's one resource store, nested in
//! the folder that holds them ([`store`]), and the bytes its files are made of, travelled on the
//! workspace's content route.
//!
//! A device that mounts a workspace runs a [`tilo_fs::files_sync::StoreSync`]: the copy of the
//! tree it serves pages its folders from this service's store and writes its verbs here. The
//! same store answers the copies this service holds for its own puts
//! ([`Puts`]) and for a workspace whose folder a host mirrors ([`open`]). The bytes a tree's
//! files are made of travel beside its nodes: [`StoreShelf`] answers for them from a device's
//! content store, and [`WorkspaceSource`] asks the workspace for what a read lacks.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use fs_protocol::FileAction;
use iroh_blobs::Hash;
use tilo_fs::files_sync::{FILES_APP, NodePage, Peer, StoreSync};
use tilo_fs::hydration::ContentSource;
use tilo_resource_store::wire::{Command, Reply};
use tilo_resource_store::{Filter, Order, PARENT, Query, Value};
use tilo_server_protocol::WorkspaceId;

use crate::content::{ContentScope, ContentShelf, ContentState, CrdtRoute, RouteContent};
use crate::workspaces::Workspaces;

pub mod access;
mod folders;
mod puts;
pub mod socket;
pub mod store;

pub use folders::{Folder, Folders};
pub use puts::Puts;

/// Open `workspace`'s tree for a host that keeps its own copy: the pages and verbs are answered
/// by this service's store in this process. Its file bytes are asked of the workspace through
/// `content`, where this device keeps and asks for them.
///
/// # Errors
/// The copy cannot be made.
pub fn open(
    workspaces: &Workspaces,
    workspace: WorkspaceId,
    content: Option<Arc<RouteContent>>,
) -> std::io::Result<StoreSync> {
    let mount = tilo_fs::namespace::MountAuthor::device(workspace.to_string());
    let peer = Arc::new(StorePeer::new(workspaces.clone(), workspace, content));
    let (_wakes, wakes) = tokio::sync::mpsc::unbounded_channel();
    StoreSync::start(tilo_fs::files_sync::Config {
        workspace: workspace.to_string(),
        mount,
        peer,
        wakes,
    })
    .map_err(std::io::Error::other)
}

/// This service's store, as a tree copy's peer.
pub struct StorePeer {
    workspaces: Workspaces,
    workspace: WorkspaceId,
    content: Option<Arc<RouteContent>>,
    /// How many bursts were made here: the invocation each is named by.
    burst: AtomicU64,
}

impl StorePeer {
    #[must_use]
    pub fn new(
        workspaces: Workspaces,
        workspace: WorkspaceId,
        content: Option<Arc<RouteContent>>,
    ) -> Self {
        Self {
            workspaces,
            workspace,
            content,
            burst: AtomicU64::new(0),
        }
    }

    /// Carry out `command` on this service's store, as this device, from a thread of its own:
    /// the copy's sync asks from a runtime task, and the service's own store is reached through
    /// the service's tasks.
    fn command(&self, command: Command) -> Result<Reply, String> {
        let workspaces = self.workspaces.clone();
        let workspace = self.workspace.to_string();
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .ok()?;
                    Some(
                        runtime.block_on(
                            workspaces.own_resource_command(&workspace, FILES_APP, command),
                        ),
                    )
                })
                .join()
                .ok()
                .flatten()
        })
        .unwrap_or_else(|| Err("the store could not be reached".to_owned()))
    }

    fn page(&self, parent: &str, after: Option<Vec<u8>>, limit: usize) -> Result<NodePage, String> {
        let after = match after {
            Some(bytes) => Some(
                serde_json::from_slice::<tilo_resource_store::Cursor>(&bytes)
                    .map_err(|error| error.to_string())?,
            ),
            None => None,
        };
        let query = Query {
            kind: store::NODE_KIND.to_owned(),
            filters: vec![Filter::Is(
                PARENT.to_owned(),
                Value::Text(store::parent_resource(parent)),
            )],
            order: Order::default(),
            after,
            limit,
        };
        let Reply::Page(page) = self.command(Command::Query(query))? else {
            return Err("the store answered no page".to_owned());
        };
        // A name no text holds keeps its exact bytes in the node's content: read them for the
        // rows that need them, once each page.
        let raws = if page.resources.iter().any(|(_, held)| {
            held.order
                .as_deref()
                .is_some_and(|name| name.contains('\u{FFFD}'))
        }) {
            let ids: Vec<String> = page
                .resources
                .iter()
                .filter(|(_, held)| {
                    held.order
                        .as_deref()
                        .is_some_and(|name| name.contains('\u{FFFD}'))
                })
                .map(|(id, _)| id.clone())
                .collect();
            match self.command(Command::Contents {
                kind: store::NODE_KIND.to_owned(),
                ids: ids.clone(),
            })? {
                Reply::Contents(contents) => ids.into_iter().zip(contents).collect(),
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let raws: BTreeMap<String, Option<serde_json::Value>> = raws.into_iter().collect();
        let rows = page
            .resources
            .iter()
            .map(|(id, held)| {
                let raw_name = raws
                    .get(id)
                    .and_then(|content| content.as_ref())
                    .and_then(|content| content.get("raw_name"))
                    .and_then(|raw| raw.as_array())
                    .map(|bytes| {
                        bytes
                            .iter()
                            .filter_map(serde_json::Value::as_u64)
                            .filter_map(|byte| u8::try_from(byte).ok())
                            .collect()
                    });
                store::row(id, held, raw_name)
            })
            .collect();
        let next = page
            .next
            .map(|cursor| serde_json::to_vec(&cursor).unwrap_or_default());
        Ok(NodePage { rows, next })
    }
}

impl Peer for StorePeer {
    fn list(&self, parent: &str, after: Option<Vec<u8>>, limit: usize) -> Result<NodePage, String> {
        self.page(parent, after, limit)
    }

    fn count(&self, parent: &str) -> Result<usize, String> {
        let Reply::Count(count) = self.command(Command::Count {
            kind: store::NODE_KIND.to_owned(),
            filters: vec![Filter::Is(
                PARENT.to_owned(),
                Value::Text(store::parent_resource(parent)),
            )],
        })?
        else {
            return Err("the store answered no count".to_owned());
        };
        Ok(count)
    }

    fn apply(&self, burst: &str, actions: &[FileAction]) -> Result<(), String> {
        let invocation = format!("{burst}-{}", self.burst.fetch_add(1, Ordering::Relaxed));
        let certificate = self.workspaces.serving_certificate();
        let invoker = tilo_resource_store::Stamp::by(&tilo_actions::Actor::Member(
            tilo_actions::MemberId::person(tilo_actions::PersonId::from_root(
                certificate.user_public_key,
            )),
        ));
        let moment = tilo_clock::unix_now_ms();
        let mut commands = Vec::with_capacity(actions.len());
        for action in actions {
            let before = self.before(action)?;
            let Some(node) = store::node_after(action, before.as_ref()) else {
                continue;
            };
            let stamp = tilo_resource_store::Stamp {
                verb: action.verb().to_owned(),
                invoker: invoker.clone(),
                invocation: invocation.clone(),
                moment,
                ..tilo_resource_store::Stamp::default()
            };
            match store::write(action, &node, &stamp) {
                Some(command) => commands.push(command),
                None => {}
            }
        }
        if commands.is_empty() {
            return Ok(());
        }
        match self.command(Command::Batch(commands))? {
            Reply::Written => Ok(()),
            _ => Err("the store answered no write".to_owned()),
        }
    }

    fn host_lacks(&self) -> Option<u32> {
        let content = self.content.as_ref()?;
        let _ = content;
        None
    }
}

impl StorePeer {
    /// The node an action changes, as the store has it now.
    fn before(&self, action: &FileAction) -> Result<Option<tilo_resource_store::Indexed>, String> {
        if matches!(
            action,
            FileAction::Create { .. } | FileAction::ListChunk { .. }
        ) {
            return Ok(None);
        }
        match self.command(Command::Get {
            kind: store::NODE_KIND.to_owned(),
            id: action.node().to_owned(),
        })? {
            Reply::Resource(held) => Ok(held),
            _ => Ok(None),
        }
    }
}

/// A device's content store as the service's shelf: the bytes of the files of the
/// workspaces it keeps folders of, and nothing of any other app's records.
pub struct StoreShelf(pub Arc<tilo_fs::ContentStore>);

impl StoreShelf {
    /// Run `work` on the store from a thread of its own: the service asks from its own tasks,
    /// and the store is never read by blocking a runtime worker on another runtime.
    fn on_store<T: Send>(
        &self,
        work: impl FnOnce(Arc<tilo_fs::ContentStore>) -> futures::future::BoxFuture<'static, T> + Send,
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
            return Err("this device keeps only the bytes of the workspaces' files".to_owned());
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

/// One workspace's files as the source of the bytes this device lacks: asked of the workspace's
/// holders through the service's content.
pub struct WorkspaceSource {
    content: Arc<RouteContent>,
    route: CrdtRoute,
    /// How long a read waits for content the workspace is asked for.
    wait: Duration,
}

impl WorkspaceSource {
    #[must_use]
    pub fn new(content: Arc<RouteContent>, workspace: &str, wait: Duration) -> Self {
        Self {
            content,
            route: CrdtRoute::app(FILES_APP, workspace),
            wait,
        }
    }

    /// Ask the workspace again for content whose answer stalled.
    pub fn reask(&self) {
        self.content.retry(tilo_clock::unix_now_ms());
    }
}

impl ContentSource for WorkspaceSource {
    fn fetch(&self, hash: Hash) -> futures::future::BoxFuture<'static, Option<Bytes>> {
        let (content, route, wait) = (Arc::clone(&self.content), self.route.clone(), self.wait);
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
            tokio::time::timeout(wait, async {
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
