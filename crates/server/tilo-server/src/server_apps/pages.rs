//! The pages of a workspace's apps that this server runs itself: each one a viewer's page, run in
//! the workspace's process of the app, laid out there and asked for on a connection of its
//! own ([`tilo_server_protocol::page_window`]).
//!
//! A page is read by the one person who opened it, and everything its app's code asks of the
//! identity service is decided as them: its connection serves the page's call alone, and the call
//! names them only while the page is open ([`Readers`]). The keys that open what the workspace
//! keeps sealed are handed to the page while that person may read it, decided again each time it
//! asks; once the page closes, its call names nobody and nothing more is handed for it.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tilo_actions::remote::{Caller, Pending, pending};
use tilo_desktop_model::WorkspaceId;
use tilo_server_protocol::page::{PageAction, PagePatch, PageRefusal};
use tilo_server_protocol::page_window::{self, Answer, Ask, HostedOpened, HostedPage, HostedPages};
use tilo_workspace_service::apps::caller_signer::{Callers, VerifiedCaller};

use super::{ServerApps, WorkspacePackages};

/// How long a page's process may take to answer: the first ask waits for the page to open.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);

/// Who reads each page open here, by the call its identity connection serves.
#[derive(Default)]
pub struct Readers(Mutex<HashMap<[u8; 16], (WorkspaceId, String, VerifiedCaller)>>);

impl Readers {
    fn named(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<[u8; 16], (WorkspaceId, String, VerifiedCaller)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `reader` reads a page of `app` in `workspace` until the answer is dropped, by a call of
    /// its own that names them for as long.
    #[must_use]
    pub fn reading(
        self: &Arc<Self>,
        workspace: WorkspaceId,
        app: &str,
        reader: VerifiedCaller,
    ) -> Reading {
        let call: [u8; 16] = rand::random();
        self.named()
            .insert(call, (workspace, app.to_owned(), reader));
        Reading {
            call,
            readers: Arc::clone(self),
        }
    }
}

/// One reader reading one page ([`Readers::reading`]).
pub struct Reading {
    call: [u8; 16],
    readers: Arc<Readers>,
}

impl Reading {
    /// The call that names the reader while they read.
    #[must_use]
    pub const fn call(&self) -> [u8; 16] {
        self.call
    }
}

impl Drop for Reading {
    /// The reader has left: the call names nobody from now on.
    fn drop(&mut self) {
        self.readers.named().remove(&self.call);
    }
}

impl Callers for Readers {
    fn caller(&self, workspace: &WorkspaceId, app: &str, call: [u8; 16]) -> Option<VerifiedCaller> {
        self.named()
            .get(&call)
            .filter(|(read_in, read_app, _)| read_in == workspace && read_app == app)
            .map(|(_, _, reader)| *reader)
    }
}

/// The pages of the apps this server runs itself.
#[derive(Clone)]
pub struct ServerPages(Arc<Pages>);

struct Pages {
    apps: Arc<ServerApps>,
    packages: Arc<WorkspacePackages>,
    readers: Arc<Readers>,
    /// The name each installed package gives its app, by workspace, app and package, or `None`
    /// for one that is no app this server runs.
    names: Mutex<HashMap<(WorkspaceId, String, String), Option<String>>>,
}

impl ServerPages {
    /// The pages of the apps `apps` runs, whose packages `packages` keeps, each read by the
    /// reader `readers` names for it.
    #[must_use]
    pub fn new(
        apps: Arc<ServerApps>,
        packages: Arc<WorkspacePackages>,
        readers: Arc<Readers>,
    ) -> Self {
        Self(Arc::new(Pages {
            apps,
            packages,
            readers,
            names: Mutex::new(HashMap::new()),
        }))
    }
}

impl Pages {
    /// The apps of `workspace` this server runs, each its id and name: the packages it installed
    /// that verify as apps this server runs.
    fn hosted(&self, workspace: WorkspaceId) -> Vec<(String, String)> {
        let workspaces = self.apps.server_workspaces();
        let mut apps = Vec::new();
        for installed in workspaces.installed(workspace) {
            let release = installed.release;
            let key = (workspace, release.app.clone(), release.package.clone());
            let known = self
                .names
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&key)
                .cloned();
            let name = known.unwrap_or_else(|| {
                let name = workspaces
                    .installed_package(&workspace.to_string(), &release.app)
                    .and_then(|tapp| {
                        self.packages
                            .named(&workspace.to_string(), &release.app, &tapp)
                            .ok()
                    })
                    .map(|(_, name)| name.unwrap_or_else(|| release.app.clone()));
                self.names
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(key, name.clone());
                name
            });
            if let Some(name) = name {
                apps.push((release.app, name));
            }
        }
        apps
    }

    /// Open `app`'s page in `workspace` for `reader`, after activating what `show` targets.
    fn open_now(
        &self,
        workspace: WorkspaceId,
        app: &str,
        reader: VerifiedCaller,
        show: Option<String>,
    ) -> Result<HostedOpened, PageRefusal> {
        // A page names the app by its id, and only that app is shown: two apps may share any
        // other part of a name. A name it has by its id's last part resolves only while one app
        // here answers to it, as `notes` finds `so.tilo.notes`.
        let hosted = self.hosted(workspace);
        let named = hosted
            .iter()
            .find(|(id, _)| id == app)
            .cloned()
            .or_else(|| {
                let mut short = hosted
                    .iter()
                    .filter(|(id, _)| id.rsplit_once('.').is_some_and(|(_, last)| last == app));
                let found = short.next().cloned();
                if short.next().is_none() { found } else { None }
            });
        let (app, title) = named.ok_or(PageRefusal::NoSuchApp)?;
        let access = self
            .apps
            .server_workspaces()
            .allowed(workspace, &reader)
            .map_err(|_| PageRefusal::NotMember)?;
        // Someone the workspace does not let read the app is shown none of it, and is not told
        // it is there.
        if !may_read(&access, workspace, &app) {
            return Err(PageRefusal::NoSuchApp);
        }
        let launcher = self
            .apps
            .launcher(workspace)
            .ok_or(PageRefusal::Elsewhere)?;
        let unavailable = |error: String| PageRefusal::Unavailable(error);
        let (connection, theirs) =
            UnixStream::pair().map_err(|error| unavailable(error.to_string()))?;
        connection
            .set_read_timeout(Some(ANSWER_WITHIN))
            .map_err(|error| unavailable(error.to_string()))?;
        // From here the page is open, and its reader named by its call until it closes.
        let page = ServerPage {
            connection: Arc::new(Mutex::new(connection)),
            reading: self.readers.reading(workspace, &app, reader),
            runtime: self.apps.server_workspaces().runtime().clone(),
        };
        launcher
            .open_page(
                &app,
                &hex::encode(reader.user_public_key),
                page.reading.call(),
                std::os::fd::OwnedFd::from(theirs),
            )
            .map_err(unavailable)?;
        let shown = ask(&page.connection, &Ask::Show { show }).map_err(unavailable)?;
        match shown {
            Answer::Shown { css, body } => Ok(HostedOpened {
                page: Arc::new(page),
                title,
                css,
                body,
            }),
            Answer::Failed(error) => Err(unavailable(error)),
            Answer::Patched(_) => Err(unavailable("the page answered out of turn".to_owned())),
        }
    }
}

/// Whether `access` lets whoever has it read what `app` keeps in `workspace`.
fn may_read(access: &tilo_action_policy::MemberAccess, workspace: WorkspaceId, app: &str) -> bool {
    access.allows(
        &tilo_action_policy::TreePath::workspace(workspace.to_string()).app(app),
        tilo_actions::ActionKind::Read,
        std::time::SystemTime::now(),
    )
}

/// `ask` asked on `connection`, and its answer: one ask at a time, each answered in turn.
fn ask(connection: &Mutex<UnixStream>, ask: &Ask) -> Result<Answer, String> {
    let mut connection = connection.lock().unwrap_or_else(PoisonError::into_inner);
    page_window::write(&mut *connection, ask)
        .map_err(|error| format!("the page's process: {error}"))?;
    page_window::read(&mut *connection).map_err(|error| format!("the page's process: {error}"))
}

impl HostedPages for ServerPages {
    fn apps(&self, workspace_id: WorkspaceId) -> Vec<(String, String)> {
        self.0.hosted(workspace_id)
    }

    fn open(
        &self,
        workspace_id: WorkspaceId,
        app: &str,
        caller: &Caller,
        show: Option<String>,
    ) -> Pending<Result<HostedOpened, PageRefusal>> {
        let reader = caller
            .device_public_key()
            .zip(caller.user_public_key())
            .map(|(device, user)| VerifiedCaller {
                device_public_key: *device,
                user_public_key: *user,
            });
        let Some(reader) = reader else {
            return Pending::ready(Err(PageRefusal::NotMember));
        };
        let (done, answer) = pending();
        let (pages, app) = (Arc::clone(&self.0), app.to_owned());
        self.0
            .apps
            .server_workspaces()
            .runtime()
            .spawn_blocking(move || {
                done.complete(pages.open_now(workspace_id, &app, reader, show));
            });
        answer
    }
}

/// One page open here: its connection to the process laying it out, which ends the page once it
/// closes, and its reader, named by the page's call while it is open.
struct ServerPage {
    connection: Arc<Mutex<UnixStream>>,
    reading: Reading,
    runtime: tokio::runtime::Handle,
}

impl HostedPage for ServerPage {
    fn act(
        &self,
        act: Option<(u32, Option<String>, PageAction)>,
    ) -> Pending<Result<Vec<PagePatch>, PageRefusal>> {
        let asked = match act {
            Some((id, target, action)) => Ask::Act { id, target, action },
            None => Ask::Changes,
        };
        let connection = Arc::clone(&self.connection);
        let (done, answer) = pending();
        self.runtime.spawn_blocking(move || {
            done.complete(match ask(&connection, &asked) {
                Ok(Answer::Patched(patches)) => Ok(patches),
                Ok(Answer::Failed(error)) | Err(error) => Err(PageRefusal::Unavailable(error)),
                Ok(Answer::Shown { .. }) => Err(PageRefusal::Unavailable(
                    "the page answered out of turn".into(),
                )),
            });
        });
        answer
    }
}
