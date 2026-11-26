//! The resources of the person's own workspace, as a part of a session reads and writes them.
//!
//! What is the person's alone — their devices, their Wi-Fi, the records an app keeps of them,
//! the shell they arranged — is a resource of their own workspace. A session asks for it from its
//! main thread, before the store that keeps the workspace is reached as much as after: the
//! device's own workspace service in the daemon ([`Workspaces`]), or that daemon as a session
//! reaches it ([`PersonStore`]). This keeps the last state of each resource it was asked for, read
//! again whenever the workspace takes in a write to it, and hands each write to the store: at once
//! once this device holds the workspace, else when it does. A write the store refused is said and
//! dropped; one it could not make yet waits.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use tilo_lock::MutexRecover as _;
use tilo_peer_state::PersonDocuments;
use tilo_resource::{Invoke, Replica, Resource};
use tilo_user_identity::DeviceCert;

use super::person_workspace::devices::{DEVICES, Devices};
use super::{Workspaces, person_workspace};

/// What a store answers, once it has.
pub type Answer<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A change made to a document of the person's workspace: what it is founded on when it holds none
/// yet, and the edit itself.
pub struct Authoring {
    /// The kind of the resource, when it is a typed one: the store writes it as its kind.
    pub kind: Option<&'static str>,
    /// The document a resource starts as, where the workspace holds none of it yet.
    pub genesis: Option<Box<automerge::AutoCommit>>,
    /// The change, made on the document as it stands.
    pub edit: Edit,
}

/// One change made to a document of the person's workspace, as [`Authoring::edit`] makes it.
pub type Edit = Box<dyn FnOnce(&mut automerge::AutoCommit) -> Result<(), String> + Send>;

/// What the store says moved in the person's workspace.
pub enum Moved {
    /// One resource, by its app and name, took a write.
    Resource(String, String),
    /// Something moved that names no one resource: every resource is read again.
    Everything,
    /// Whether this device holds the workspace may have moved: what waits for it is tried again.
    Standing,
}

/// Where the person's own workspace is kept, as [`PersonResources`] reads and writes it: the
/// device's workspace service itself, or the device's daemon as a session reaches it.
pub trait PersonStore: Send + Sync + 'static {
    /// This device's certificate, as the store signs the person's writes under it.
    fn certificate(&self) -> DeviceCert;

    /// Whether this device holds the person's own workspace now.
    fn held(&self) -> Answer<'_, bool>;

    /// Every resource of the person's workspace, by its app and name.
    fn routes(&self) -> Answer<'_, Result<Vec<(String, String)>, String>>;

    /// The resource `(app, name)` of the person's workspace, saved; `None` while it holds none.
    fn document<'a>(
        &'a self,
        app: &'a str,
        name: &'a str,
    ) -> Answer<'a, Result<Option<Vec<u8>>, String>>;

    /// Make `authoring` to the resource `(app, name)` of the person's workspace, as this device.
    fn author(
        &self,
        app: String,
        name: String,
        authoring: Authoring,
    ) -> Answer<'_, Result<(), String>>;

    /// What moves in the person's workspace from now on, told until the store stops.
    fn moves(&self) -> tokio::sync::mpsc::UnboundedReceiver<Moved>;
}

/// A write waiting for the store: it makes its action afresh each time it is tried.
type Write = Arc<dyn Fn(Arc<dyn PersonStore>) -> Answer<'static, Result<(), String>> + Send + Sync>;

/// The person's own workspace's resources on this device.
#[derive(Clone, Default)]
pub struct PersonResources {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The store, once it is reached, and the runtime its writes run on.
    service: OnceLock<(Arc<dyn PersonStore>, tokio::runtime::Handle)>,
    /// Each resource as the service last had it, saved, by `(app, name)`.
    saved: Mutex<BTreeMap<(String, String), Vec<u8>>>,
    /// The writes the service did not make yet, oldest first.
    waiting: Mutex<Vec<Write>>,
    /// Counts every change to a resource here.
    changes: tokio::sync::watch::Sender<u64>,
}

impl PersonResources {
    /// Moves each time a resource changed here: a reader keeps what it read until it does.
    pub fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    /// The state of the resource `R` at `(app, name)` as this device last had it: its initial
    /// state while it has none.
    pub fn read<R: Resource>(&self, (app, name): (&str, &str)) -> Option<R::State> {
        let saved = self
            .inner
            .saved
            .lock_recover()
            .get(&(app.to_owned(), name.to_owned()))
            .cloned();
        let replica = match saved {
            Some(bytes) => Replica::<R>::load(&bytes).ok()?,
            None => Replica::<R>::from_doc(person_workspace::genesis::<R>("").ok()?),
        };
        replica.state().ok()
    }

    /// Apply the action `action` makes to the resource `(app, name)`, now or once the service
    /// holds the person's workspace.
    pub fn apply<R, A>(&self, resource: (&'static str, &'static str), action: A)
    where
        R: Resource + 'static,
        A: Fn() -> Invoke<R> + Send + Sync + 'static,
        Invoke<R>: Send,
    {
        let write: Write = Arc::new(move |store: Arc<dyn PersonStore>| {
            let invoke = action();
            // The store tells its own write as it tells any other: the follower reads it back.
            Box::pin(async move {
                let cert = store.certificate();
                let (app, name) = resource;
                // The person's own workspace, which the store keeps.
                let workspace =
                    tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key);
                let genesis = Box::new(person_workspace::founded::<R>(&workspace, name)?);
                store
                    .author(
                        app.to_owned(),
                        name.to_owned(),
                        Authoring {
                            kind: Some(R::KIND),
                            genesis: Some(genesis),
                            edit: Box::new(move |doc| {
                                person_workspace::invoke(doc, &cert, app, invoke)
                            }),
                        },
                    )
                    .await
            })
        });
        self.queue(write);
    }

    /// The documents of `app` this device holds of the person's workspace, by name.
    #[must_use]
    pub fn documents(&self, app: &str) -> Vec<String> {
        self.inner
            .saved
            .lock_recover()
            .keys()
            .filter(|(held, _)| held.as_str() == app)
            .map(|(_, name)| name.clone())
            .collect()
    }

    /// The document `(app, name)` of the person's workspace as the service last had it, `None`
    /// while it holds none of it.
    pub fn document(&self, (app, name): (&str, &str)) -> Option<Vec<u8>> {
        self.inner
            .saved
            .lock_recover()
            .get(&(app.to_owned(), name.to_owned()))
            .cloned()
    }

    /// Edit the document `(app, name)` of the person's workspace, now or once the service holds
    /// the workspace: for what a caller keeps its own Automerge structure under a resource
    /// address, rather than a typed [`Resource`]. A typed record names its `kind`.
    pub fn edit_document<F>(&self, resource: (&str, &str), kind: Option<&'static str>, edit: F)
    where
        F: Fn(&mut automerge::AutoCommit) -> Result<(), String> + Send + Sync + 'static,
    {
        let (app, name) = (resource.0.to_owned(), resource.1.to_owned());
        let edit = Arc::new(edit);
        let write: Write = Arc::new(move |store: Arc<dyn PersonStore>| {
            let (app, name, edit) = (app.clone(), name.clone(), Arc::clone(&edit));
            Box::pin(async move {
                store
                    .author(
                        app,
                        name,
                        Authoring {
                            kind,
                            genesis: None,
                            edit: Box::new(move |document| edit.as_ref()(document)),
                        },
                    )
                    .await
            })
        });
        self.queue(write);
    }

    /// Make `write` now, once the store holds the workspace, or hold it until it does.
    fn queue(&self, write: Write) {
        match self.inner.service.get() {
            Some((store, runtime)) => {
                let (inner, store) = (Arc::clone(&self.inner), Arc::clone(store));
                runtime.spawn(async move { inner.make(write, store).await });
            }
            None => self.inner.waiting.lock_recover().push(write),
        }
    }

    /// Hand writes and reads to `store` from now on, on `runtime`: what waited is written once
    /// this device holds the person's workspace, and each resource is read again whenever the
    /// workspace takes in a write to it. The first store handed stays.
    pub fn attach(&self, store: Arc<dyn PersonStore>, runtime: &tokio::runtime::Handle) {
        if self
            .inner
            .service
            .set((Arc::clone(&store), runtime.clone()))
            .is_err()
        {
            return;
        }
        let inner = Arc::clone(&self.inner);
        runtime.spawn(async move { inner.follow(store).await });
    }

    /// The person's devices whose certificates hold now, as their own workspace says them.
    #[must_use]
    pub fn kept_devices(&self) -> Vec<DeviceCert> {
        self.kept_devices_at()
            .into_iter()
            .map(|(cert, _)| cert)
            .collect()
    }

    /// The person's devices whose certificates hold now, each with where it says its endpoint
    /// answers: the addresses another of their devices dials it at when no relay and no
    /// discovery find it.
    #[must_use]
    pub fn kept_devices_at(&self) -> Vec<(DeviceCert, Vec<std::net::SocketAddr>)> {
        let Some(own) = self.certificate() else {
            return Vec::new();
        };
        let Some(devices) = self.read::<Devices>(DEVICES) else {
            return Vec::new();
        };
        let now = tilo_clock::unix_now_ns();
        devices
            .devices
            .iter()
            .filter_map(|(_, kept)| {
                let bytes = hex::decode(&kept.certificate).ok()?;
                let cert: DeviceCert = postcard::from_bytes(&bytes).ok()?;
                (cert.user_public_key == own.user_public_key
                    && tilo_user_identity::verify_device_cert_at(&cert, now).is_ok())
                .then(|| (cert, kept.endpoint_addrs()))
            })
            .collect()
    }

    /// This device's own certificate, as the store that keeps its person's workspace signs
    /// under it: `None` while no store is reached.
    #[must_use]
    pub fn certificate(&self) -> Option<DeviceCert> {
        self.inner
            .service
            .get()
            .map(|(store, _)| store.certificate())
    }
}

impl PersonDocuments for PersonResources {
    fn device(&self) -> [u8; 32] {
        self.certificate()
            .map_or([0; 32], |cert| cert.device_public_key)
    }

    fn person(&self) -> [u8; 32] {
        self.certificate()
            .map_or([0; 32], |cert| cert.user_public_key)
    }

    fn devices(&self) -> Vec<DeviceCert> {
        Self::kept_devices(self)
    }

    fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        Self::changes(self)
    }

    fn document(&self, app: &str, id: &str) -> Option<Vec<u8>> {
        Self::document(self, (app, id))
    }

    fn edit(
        &self,
        app: &str,
        id: &str,
        kind: Option<&'static str>,
        edit: tilo_peer_state::DocumentEdit,
    ) {
        Self::edit_document(self, (app, id), kind, edit);
    }
}

impl Inner {
    /// Make `write`; one this device cannot make until it holds the workspace waits for it.
    async fn make(&self, write: Write, store: Arc<dyn PersonStore>) {
        if !store.held().await {
            self.waiting.lock_recover().push(write);
            return;
        }
        if let Err(error) = write(store).await {
            tracing::warn!(%error, "a write to the person's own workspace was refused");
        }
    }

    /// Keep each resource read here current, and make what waits once the workspace is held.
    async fn follow(self: Arc<Self>, store: Arc<dyn PersonStore>) {
        let mut moves = store.moves();
        self.reload(store.as_ref(), None).await;
        loop {
            // What waits is made once the workspace is held here; nothing waiting asks nothing.
            let waits = !self.waiting.lock_recover().is_empty();
            if waits && store.held().await {
                let waiting = std::mem::take(&mut *self.waiting.lock_recover());
                for write in waiting {
                    if let Err(error) = write(Arc::clone(&store)).await {
                        tracing::warn!(%error, "a write to the person's own workspace was refused");
                    }
                }
                self.reload(store.as_ref(), None).await;
            }
            match moves.recv().await {
                Some(Moved::Resource(app, name)) => {
                    self.reload(store.as_ref(), Some((app, name))).await;
                }
                Some(Moved::Everything) => self.reload(store.as_ref(), None).await,
                Some(Moved::Standing) => {}
                None => return,
            }
        }
    }

    /// Read again `only`, or every resource of the person's workspace when `None`.
    async fn reload(&self, store: &dyn PersonStore, only: Option<(String, String)>) {
        let routes = match only {
            Some(route) => vec![route],
            None => match store.routes().await {
                Ok(routes) => routes,
                Err(error) => {
                    tracing::warn!(%error, "the person's workspace could not be listed; its resources stay as last read");
                    return;
                }
            },
        };
        let mut moved = false;
        for (app, name) in routes {
            let Ok(Some(bytes)) = store.document(&app, &name).await else {
                continue;
            };
            let previous = self.saved.lock_recover().insert((app, name), bytes.clone());
            moved |= previous.as_ref() != Some(&bytes);
        }
        if moved {
            self.changes
                .send_modify(|count| *count = count.wrapping_add(1));
        }
    }
}

/// The device's own workspace service keeps the person's workspace itself.
impl PersonStore for Workspaces {
    fn certificate(&self) -> DeviceCert {
        self.device_cert().clone()
    }

    fn held(&self) -> Answer<'_, bool> {
        Box::pin(person_workspace::holds_person_workspace(self))
    }

    fn routes(&self) -> Answer<'_, Result<Vec<(String, String)>, String>> {
        Box::pin(async move {
            self.workspace_document_routes(&person_workspace::workspace_id(self))
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn document<'a>(
        &'a self,
        app: &'a str,
        name: &'a str,
    ) -> Answer<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(async move {
            self.resource_document(&person_workspace::workspace_id(self), app, name)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn author(
        &self,
        app: String,
        name: String,
        authoring: Authoring,
    ) -> Answer<'_, Result<(), String>> {
        Box::pin(async move {
            let workspace = person_workspace::workspace_id(self);
            if let Some(kind) = authoring.kind {
                let edit = authoring.edit;
                return Box::pin(self.author_record(&workspace, (&app, kind, &name), edit)).await;
            }
            if self.is_member(&workspace).await? {
                return person_workspace::edit_as_member(
                    self,
                    &workspace,
                    (&app, &name),
                    authoring.genesis,
                    authoring.edit,
                )
                .await;
            }
            let cert = self.device_cert().clone();
            let applied = Box::pin(self.author_workspace_document_from(
                (&workspace, &cert),
                (&app, &name),
                authoring.genesis,
                authoring.edit,
            ))
            .await
            .map_err(|error| error.to_string());
            if applied.is_ok() {
                self.wrote(&workspace, &app, &name);
            }
            applied
        })
    }

    fn moves(&self) -> tokio::sync::mpsc::UnboundedReceiver<Moved> {
        use tokio::sync::broadcast::error::RecvError;
        let (tell, told) = tokio::sync::mpsc::unbounded_channel();
        let id = person_workspace::workspace_id(self);
        let mut writes = self.document_writes();
        let mut governance = self.governance_changes();
        tokio::spawn(async move {
            loop {
                let moved = tokio::select! {
                    write = writes.recv() => match write {
                        Ok(write) if write.workspace == id => Moved::Resource(write.app, write.record),
                        Ok(_) => continue,
                        Err(RecvError::Lagged(_)) => Moved::Everything,
                        Err(RecvError::Closed) => return,
                    },
                    changed = governance.recv() => match changed {
                        Err(RecvError::Closed) => return,
                        Ok(_) | Err(RecvError::Lagged(_)) => Moved::Standing,
                    },
                };
                if tell.send(moved).is_err() {
                    return;
                }
            }
        });
        told
    }
}
