//! Where an agent task's tilolang programs run: a tilolang host of the task's own, which the
//! device starts like every other tilolang process, through [`super::processes::Processes`], for
//! any API whose workspace runs programs — a desktop session's MCP server, the daemon's, or Tilo's
//! API served by the daemon.
//!
//! The host holds no authority: it decides nothing and writes no record, and everything its
//! programs read or change comes back over the program connection to the API, which does it as
//! the program's author, through the same check as every other call (`tilo_mcp::mcp_tilo`). What
//! differs between APIs is what a program may name there: the apps of the workspace the task
//! runs in, which [`TaskCatalogue`] answers.

use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use runtime_portal::process_link::{App, TaskFds, TaskRequest, encode_apps};
use tilo_lock::MutexRecover as _;

use super::processes::Processes;

/// What a task's programs may name in one workspace: the apps it compiles against, and what the
/// host reads of the records the API itself changes.
#[derive(Default)]
pub struct Catalogue {
    pub apps: Vec<App>,
    /// The id each app keeps its records under, when it was renamed from an earlier one.
    pub documents_under: BTreeMap<String, String>,
    /// The apps whose records the API changes itself, rather than invoking their actions.
    pub changed_at_document: BTreeSet<String>,
}

/// Where a task's catalogue comes from: the workspace's apps, as its host runs them.
pub trait TaskCatalogue: Send + Sync {
    /// The apps a task's programs compile against in `workspace`.
    fn catalogue(&self, workspace: &str) -> Catalogue;
}

/// What starts one connection's task hosts: each task's host, started on its first program by
/// the device's processes, which run it, follow it and end it like every tilolang process. This
/// keeps no process of its own.
pub struct TaskHosts {
    processes: Arc<Processes>,
    /// The person the connection was admitted for: `me` in its programs.
    person: Mutex<Option<String>>,
    catalogue: Arc<dyn TaskCatalogue>,
}

impl TaskHosts {
    /// The hosts of `catalogue`'s task connections, which `processes` start, whose programs run
    /// as `person`.
    #[must_use]
    pub fn new(
        processes: Arc<Processes>,
        person: Option<&str>,
        catalogue: Arc<dyn TaskCatalogue>,
    ) -> Self {
        Self {
            processes,
            person: Mutex::new(person.map(str::to_owned)),
            catalogue,
        }
    }

    /// The person `person` the connection is admitted for, once it is: `me` in its programs.
    pub fn set_person(&self, person: Option<&str>) {
        *self.person.lock_recover() = person.map(str::to_owned);
    }
}

impl tilo_mcp::TaskHosts for TaskHosts {
    fn start(&self, workspace: &str, task: &str) -> std::io::Result<OwnedFd> {
        let (api_end, host_end) = std::os::unix::net::UnixStream::pair()?;
        let catalogue = self.catalogue.catalogue(workspace);
        let encoded = encode_apps(&catalogue.apps);
        let apps = tilo_render_protocol::shm::sealed_blob("tilolang-apps", &encoded)?;
        let request = TaskRequest {
            task: task.to_owned(),
            workspace: workspace.to_owned(),
            person: self.person.lock_recover().clone(),
            apps_len: encoded.len() as u64,
            documents_under: catalogue.documents_under,
            changed_at_document: catalogue.changed_at_document,
        };
        let runs_on = super::package_runs::task_runtimes()
            .map_err(|why| std::io::Error::new(std::io::ErrorKind::NotFound, why))?;
        self.processes.start_task(
            request,
            TaskFds {
                program: host_end.into(),
                apps,
            },
            runs_on,
        )?;
        // The host ends when the API closes the program connection, with the client that
        // started it.
        Ok(api_end.into())
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
    H: tilo_actions::remote::ActionProvider + Send + Sync,
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
            .wait_timeout(tilo_session_actions::REPLY_DEADLINE)
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
        // The workspace's own actions: its root row carries the surface a program compiles
        // against, so the workspace is reached from itself and no app has to ship one.
        if let Some(surface) = self.workspace_surface(workspace) {
            apps.push(App::Surface {
                app: tilo_action_policy::agent_answers::WORKSPACE_APP.to_owned(),
                title: "Workspace".to_owned(),
                surface,
            });
        }
        Catalogue {
            apps,
            ..Catalogue::default()
        }
    }
}

impl<H> WorkspaceCatalogue<H>
where
    H: tilo_actions::remote::ActionProvider + Send + Sync,
{
    /// The surface the workspace publishes for its own actions, from the row they are
    /// catalogued under: what a program using them compiles against.
    fn workspace_surface(&self, workspace: &str) -> Option<String> {
        let app = tilo_action_policy::agent_answers::WORKSPACE_APP;
        let query = tilo_actions::remote::CatalogueQuery {
            provider: Some(app.to_owned()),
            app_id: Some(app.to_owned()),
            max_results: 8,
            ..tilo_actions::remote::CatalogueQuery::default()
        };
        let Ok(Some(Ok(page))) = self
            .host
            .query(&self.caller, query)
            .wait_timeout(tilo_session_actions::REPLY_DEADLINE)
        else {
            tracing::debug!(
                %workspace,
                "a task reads no workspace surface: the workspace did not answer"
            );
            return None;
        };
        page.rows.into_iter().find_map(|row| row.surface)
    }
}
