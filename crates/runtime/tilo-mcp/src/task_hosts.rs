//! Where a task's tilolang programs run: the tilolang host of one task, started over a socket of
//! its own, for any API whose workspace hosts programs — a desktop session's MCP server, a
//! device's, or the API server's over a workspace the relay keeps.
//!
//! The host holds no authority: it decides nothing and writes no record, and everything its
//! programs read or change comes back over the program connection to the API, which does it as
//! the program's author, through the same gate as every other call ([`crate::mcp_tilo`]). What
//! differs between hosts is what a program may name there: the apps of the workspace the task
//! runs in, which [`TaskCatalogue`] answers.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use runtime_portal::tilolang_host::App;
#[cfg(target_os = "linux")]
use runtime_portal::tilolang_host::{TaskFds, TaskRequest, encode_apps};
use tilo_lock::MutexRecover as _;

/// What a task's programs may name in one workspace: the apps it compiles against, and what the
/// host reads of the records the API itself changes.
#[derive(Default)]
pub struct Catalogue {
    pub apps: Vec<App>,
    /// The id each app keeps its records under, when it was renamed from an earlier one.
    pub records_under: BTreeMap<String, String>,
    /// The apps whose records the API changes itself, rather than invoking their actions.
    pub changed_at_record: BTreeSet<String>,
}

/// Where a task's catalogue comes from: the workspace's apps, as its host runs them.
pub trait TaskCatalogue: Send + Sync {
    /// The apps a task's programs compile against in `workspace`.
    fn catalogue(&self, workspace: &str) -> Catalogue;
}

/// The hosts of one connection's task connections: each task's tilolang host, started on its
/// first program in a thread of the API's own ([`crate::TaskHosts`]).
pub struct TaskHosts {
    /// The person the connection was admitted for: `me` in its programs.
    person: Mutex<Option<String>>,
    catalogue: Arc<dyn TaskCatalogue>,
}

impl TaskHosts {
    /// The hosts of `catalogue`'s task connections, whose programs run as `person`.
    #[must_use]
    pub fn new(person: Option<&str>, catalogue: Arc<dyn TaskCatalogue>) -> Self {
        Self {
            person: Mutex::new(person.map(str::to_owned)),
            catalogue,
        }
    }

    /// The person `person` the connection is admitted for, once it is: `me` in its programs.
    pub fn set_person(&self, person: Option<&str>) {
        *self.person.lock_recover() = person.map(str::to_owned);
    }
}

impl crate::TaskHosts for TaskHosts {
    /// A task's host is a Linux program: anywhere else no program runs here, and the caller is
    /// told so rather than the API failing to build.
    #[cfg(not(target_os = "linux"))]
    fn start(&self, workspace: &str, task: &str) -> io::Result<OwnedFd> {
        let _ = (workspace, task, &self.person, &self.catalogue);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a task's programs run on a Linux host",
        ))
    }

    #[cfg(target_os = "linux")]
    fn start(&self, workspace: &str, task: &str) -> io::Result<OwnedFd> {
        let (server_end, host_end) = std::os::unix::net::UnixStream::pair()?;
        let catalogue = self.catalogue.catalogue(workspace);
        let encoded = encode_apps(&catalogue.apps);
        let apps = tilo_render_protocol::shm::sealed_blob("tilolang-apps", &encoded)?;
        let request = TaskRequest {
            task: task.to_owned(),
            workspace: workspace.to_owned(),
            person: self.person.lock_recover().clone(),
            apps_len: encoded.len() as u64,
            records_under: catalogue.records_under,
            changed_at_record: catalogue.changed_at_record,
        };
        std::thread::Builder::new()
            .name("tilo-task-host".into())
            .spawn(move || {
                let fds = TaskFds {
                    program: host_end.into(),
                    apps,
                };
                let opened = tilolang_host::task::Task::open(&request, fds);
                let task = match opened {
                    Ok(task) => task,
                    Err(error) => {
                        tracing::warn!(task = request.task, %error, "MCP server: a task's host did not start");
                        return;
                    }
                };
                // Every line the server sends is answered; the host ends when the server closes
                // the connection, with the client that started it.
                while task.serve_all() {}
                tracing::debug!(task = request.task, "MCP server: a task's host ended");
            })?;
        Ok(server_end.into())
    }
}

/// The apps a workspace's host publishes, as a task's programs compile against them there: every
/// app it lists to open, from the surface of the fleet's build of it, which is the one that host
/// runs.
pub struct WorkspaceCatalogue<H> {
    /// The workspace's actions, as the API above reaches them.
    host: H,
    /// Who asks: the key the connection was admitted by, as the host authenticated it.
    caller: tilo_actions::remote::Caller,
}

impl<H> WorkspaceCatalogue<H> {
    #[must_use]
    pub const fn new(host: H, caller: tilo_actions::remote::Caller) -> Self {
        Self { host, caller }
    }
}

impl<H> TaskCatalogue for WorkspaceCatalogue<H>
where
    H: tilo_actions::remote::ActionHost + Send + Sync,
{
    /// The apps the workspace lists, each with the surface this build of the app publishes: a
    /// program compiles against the app's own declarations, so an app whose surface this build
    /// does not ship is left out rather than named by a guess.
    fn catalogue(&self, workspace: &str) -> Catalogue {
        let query = tilo_actions::remote::CatalogueQuery {
            provider: Some(tilo_actions::remote::workspace_provider(workspace)),
            resource_name: Some(tilo_actions::remote::APPLICATION_RESOURCE_KIND.to_owned()),
            max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
            ..tilo_actions::remote::CatalogueQuery::default()
        };
        let page = match self
            .host
            .query(&self.caller, query)
            .wait_timeout(tilo_session_client::remote::REPLY_DEADLINE)
        {
            Ok(Some(Ok(page))) => page,
            Ok(Some(Err(error))) => {
                tracing::warn!(%workspace, %error, "a task reads no app: the workspace listed none");
                return Catalogue::default();
            }
            _ => {
                tracing::warn!(%workspace, "a task reads no app: the workspace did not answer");
                return Catalogue::default();
            }
        };
        let mut apps = Vec::new();
        let mut named = BTreeSet::new();
        for row in page.rows {
            if row.action.is_none() {
                continue;
            }
            let Some(app) = row
                .resource
                .id
                .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
                .filter(|app| !app.starts_with("host:"))
            else {
                continue;
            };
            if !named.insert(app.to_owned()) {
                continue;
            }
            let Some(surface) = app_fleet::surface(app) else {
                tracing::debug!(%app, "a task leaves out an app this build ships no surface of");
                continue;
            };
            apps.push(App::Surface {
                app: app.to_owned(),
                title: app_fleet::metadata(app)
                    .map_or(app, |metadata| metadata.name)
                    .to_owned(),
                surface: surface.to_owned(),
            });
        }
        Catalogue {
            apps,
            ..Catalogue::default()
        }
    }
}
