//! The resources of the person's own workspace, as this session reads and writes them.
//!
//! The session asks for what is the person's alone (their devices, their Wi-Fi) from its main
//! thread, before its workspace service has opened as much as after. This keeps the last state of
//! each resource it was asked for, read again whenever the workspace takes in a write to it, and
//! hands each write to the service: at once once this device holds the workspace, else when it
//! does. A write the service refused is said and dropped; one it could not make yet waits.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use tilo_lock::MutexRecover as _;
use tilo_resource::{Invoke, Replica, Resource};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::person_workspace;

/// A write waiting for the service: it makes its action afresh each time it is tried.
type Write = Arc<
    dyn Fn(Workspaces) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

/// Whether this device holds the person's own workspace: its replica of it is installed here, and
/// the device takes part in the workspace's channels.
type Holds = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The person's own workspace's resources on this device.
#[derive(Clone, Default)]
pub(crate) struct PersonResources {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The service, once it opened, and the runtime its writes run on.
    service: OnceLock<(Workspaces, tokio::runtime::Handle)>,
    /// Whether this device holds the person's own workspace, once the session can say.
    holds: OnceLock<Holds>,
    /// Each resource as the service last had it, saved, by `(app, name)`.
    saved: Mutex<BTreeMap<(String, String), Vec<u8>>>,
    /// The writes the service did not make yet, oldest first.
    waiting: Mutex<Vec<Write>>,
    /// Counts every change to a resource here.
    changes: tokio::sync::watch::Sender<u64>,
}

impl PersonResources {
    /// Moves each time a resource changed here: a reader keeps what it read until it does.
    pub(crate) fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    /// The state of the resource `R` at `(app, name)` as this device last had it: its initial
    /// state while it has none.
    pub(crate) fn read<R: Resource>(&self, (app, name): (&str, &str)) -> Option<R::State> {
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
    pub(crate) fn apply<R, A>(&self, resource: (&'static str, &'static str), action: A)
    where
        R: Resource + 'static,
        A: Fn() -> Invoke<R> + Send + Sync + 'static,
        Invoke<R>: Send,
    {
        let write: Write = Arc::new(move |workspaces: Workspaces| {
            let invoke = action();
            // The service tells its own write as it tells any other: the follower reads it back.
            Box::pin(
                async move { person_workspace::apply::<R>(&workspaces, resource, invoke).await },
            )
        });
        self.queue(write);
    }

    /// The document `(app, name)` of the person's workspace as the service last had it, `None`
    /// while it holds none of it.
    pub(crate) fn document(&self, (app, name): (&str, &str)) -> Option<Vec<u8>> {
        self.inner
            .saved
            .lock_recover()
            .get(&(app.to_owned(), name.to_owned()))
            .cloned()
    }

    /// Edit the document `(app, name)` of the person's workspace, now or once the service holds
    /// the workspace: for what a caller keeps its own Automerge structure under a resource
    /// address, rather than a typed [`Resource`].
    pub(crate) fn edit_document<F>(&self, resource: (&str, &str), edit: F)
    where
        F: Fn(&mut automerge::AutoCommit) -> Result<(), String> + Send + Sync + 'static,
    {
        let (app, name) = (resource.0.to_owned(), resource.1.to_owned());
        let edit = Arc::new(edit);
        let write: Write = Arc::new(move |workspaces: Workspaces| {
            let (app, name, edit) = (app.clone(), name.clone(), Arc::clone(&edit));
            Box::pin(async move {
                person_workspace::edit_document(&workspaces, (&app, &name), move |document| {
                    edit.as_ref()(document)
                })
                .await
            })
        });
        self.queue(write);
    }

    /// Make `write` now, once the service holds the workspace, or hold it until it does.
    fn queue(&self, write: Write) {
        match self.inner.service.get() {
            Some((workspaces, runtime)) => {
                let (inner, workspaces) = (Arc::clone(&self.inner), workspaces.clone());
                runtime.spawn(async move { inner.make(write, workspaces).await });
            }
            None => self.inner.waiting.lock_recover().push(write),
        }
    }

    /// Hand writes and reads to `workspaces`, this device's own service, from now on, on
    /// `runtime`, and take `holds` as what says this device is in the person's own workspace: what
    /// waited is written once it is, and each resource is read again whenever the workspace takes
    /// in a write to it.
    ///
    /// The service holds the person's workspace as its governor from the moment it makes it, while
    /// this device is not in it yet; a write made then is refused, and the service's governor has
    /// no channel keys to seal it with. So the gate is this device's hold, which is also where an
    /// ordinary workspace's writes wait.
    pub(crate) fn attach(
        &self,
        workspaces: Workspaces,
        runtime: &tokio::runtime::Handle,
        holds: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) {
        if self
            .inner
            .service
            .set((workspaces.clone(), runtime.clone()))
            .is_err()
        {
            return;
        }
        let _ = self.inner.holds.set(Arc::new(holds));
        let inner = Arc::clone(&self.inner);
        runtime.spawn(async move { inner.follow(workspaces).await });
    }
}

impl Inner {
    /// Whether this device holds `workspace`: its replica is installed here.
    fn held(&self, workspace: &str) -> bool {
        self.holds.get().is_some_and(|holds| holds(workspace))
    }

    /// Make `write`; one this device cannot make until it holds the workspace waits for it.
    async fn make(&self, write: Write, workspaces: Workspaces) {
        if !self.held(&person_workspace::workspace_id(&workspaces)) {
            self.waiting.lock_recover().push(write);
            return;
        }
        if let Err(error) = write(workspaces).await {
            tracing::warn!(%error, "a write to the person's own workspace was refused");
        }
    }

    /// Keep each resource read here current, and make what waits once the workspace is held here.
    async fn follow(self: Arc<Self>, workspaces: Workspaces) {
        use tokio::sync::broadcast::error::RecvError;
        let id = person_workspace::workspace_id(&workspaces);
        let mut writes = workspaces.document_writes();
        let mut moved = workspaces.governance_changes();
        self.reload(&workspaces, None).await;
        loop {
            if self.held(&id) {
                let waiting = std::mem::take(&mut *self.waiting.lock_recover());
                for write in waiting {
                    if let Err(error) = write(workspaces.clone()).await {
                        tracing::warn!(%error, "a write to the person's own workspace was refused");
                    }
                }
                self.reload(&workspaces, None).await;
            }
            tokio::select! {
                write = writes.recv() => match write {
                    Ok(write) if write.workspace == id => {
                        self.reload(&workspaces, Some((write.app, write.record))).await;
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => self.reload(&workspaces, None).await,
                    Err(RecvError::Closed) => return,
                },
                changed = moved.recv() => if matches!(changed, Err(RecvError::Closed)) {
                    return;
                },
            }
        }
    }

    /// Read again `only`, or every resource of the person's workspace when `None`.
    async fn reload(&self, workspaces: &Workspaces, only: Option<(String, String)>) {
        let id = person_workspace::workspace_id(workspaces);
        let routes = match only {
            Some(route) => vec![route],
            None => match workspaces.workspace_document_routes(&id).await {
                Ok(routes) => routes,
                Err(error) => {
                    tracing::warn!(%error, "the person's workspace could not be listed; its resources stay as last read");
                    return;
                }
            },
        };
        let mut moved = false;
        for (app, name) in routes {
            let Ok(Some(bytes)) = workspaces.resource_document(&id, &app, &name).await else {
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
