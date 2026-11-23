//! Where a task's tilolang programs run: the tilolang host of one task, started over a socket of
//! its own, for any API whose workspace hosts programs — a desktop session's MCP server, a
//! device's, or the API server's over a workspace the server keeps.
//!
//! The host holds no authority: it decides nothing and writes no record, and everything its
//! programs read or change comes back over the program connection to the API, which does it as
//! the program's author, through the same check as every other call ([`crate::mcp_tilo`]). What
//! differs between hosts is what a program may name there: the apps of the workspace the task
//! runs in, which [`TaskCatalogue`] answers.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

use runtime_portal::process_link::App;
#[cfg(target_os = "linux")]
use runtime_portal::process_link::{Packet as _, TaskFds, TaskRequest, ToProcess, encode_apps};
use tilo_lock::MutexRecover as _;

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

/// The hosts of one connection's task connections: each task's tilolang host, started on its
/// first program as a process of its own ([`crate::TaskHosts`]).
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
            documents_under: catalogue.documents_under,
            changed_at_document: catalogue.changed_at_document,
        };
        let api = start_program()?;
        ToProcess::Task {
            request,
            fds: TaskFds {
                program: host_end.into(),
                apps,
            },
        }
        .send(&api)?;
        // The host ends when the server closes the program connection, with the client that
        // started it: it needs its api for nothing more.
        Ok(server_end.into())
    }
}

/// The task host's program: `tilolang-process` beside this program, as a build or an install puts
/// them, or beside the directory it is in, as Cargo keeps a test's program.
#[cfg(target_os = "linux")]
fn program() -> io::Result<std::path::PathBuf> {
    const NAME: &str = "tilolang-process";
    let exe = std::env::current_exe()?;
    let dir = exe.parent();
    [dir, dir.and_then(std::path::Path::parent)]
        .into_iter()
        .flatten()
        .map(|dir| dir.join(NAME))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no tilolang-process is installed beside this program to run a task's programs",
            )
        })
}

/// Start a task host of its own: the tilolang host's program, holding the other end of the api
/// this returns and nothing else of this process. It ends with this process, and with its last
/// task.
///
/// It is handed its link as every tilolang host is ([`runtime_portal::process_link::spawn`]), but
/// started straight from its program rather than through the device's launcher: it runs in
/// whatever serves a task's programs (an API server, `tilo mcp`, a session), where no launcher or
/// runtime bundle need be installed, holds no storage and no connection but the program's, and
/// lives for one task, ending with the thread that started it.
#[cfg(target_os = "linux")]
fn start_program() -> io::Result<OwnedFd> {
    use std::os::unix::process::CommandExt as _;
    let mut command = std::process::Command::new(program()?);
    command
        .env_remove(runtime_portal::process_link::STORAGE_ENV)
        // This process's own output may be a protocol (`tilo mcp` speaks on it).
        .stdout(std::process::Stdio::null());
    // SAFETY: runs in the forked child before exec and calls only an async-signal-safe system
    // call (prctl), allocating nothing.
    unsafe {
        command.pre_exec(|| {
            rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::TERM))?;
            Ok(())
        });
    }
    let (ours, mut child) = runtime_portal::process_link::spawn(command)?;
    std::thread::Builder::new()
        .name("tilo-task-host".into())
        .spawn(move || match child.wait() {
            Ok(status) if status.success() => tracing::debug!("a task's host ended"),
            Ok(status) => tracing::warn!(%status, "a task's host failed"),
            Err(error) => tracing::warn!(%error, "a task's host was lost"),
        })?;
    Ok(ours)
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
        Catalogue {
            apps,
            ..Catalogue::default()
        }
    }
}
