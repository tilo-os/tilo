//! Where the session's door starts a connection's task host, and the apps that host compiles a
//! task's programs against.

use std::io;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use tilo_lock::MutexRecover as _;

use super::{DoorAsk, SessionDoors, WAIT};

/// Every installed app a task's programs may name, besides the packages the session runs itself
/// (`kept`): each tilolang app's own package, verified as a launch verifies it, which declares
/// the names a program calls its actions and roots by, and the signed surface of every other
/// app. With them, the id each tilolang app keeps its records under when it was renamed.
/// One catalogue, whoever reads it: `tilolang check` reads the same packages and surfaces from
/// the apps' directories.
fn installed_apps(
    kept: &std::collections::BTreeSet<String>,
) -> (
    Vec<runtime_portal::tilolang_host::App>,
    std::collections::BTreeMap<String, String>,
) {
    use runtime_portal::tilolang_host::App;
    let mut apps = Vec::new();
    let mut records_under = std::collections::BTreeMap::new();
    let mut named: std::collections::BTreeSet<String> = kept.clone();
    for package in tilo_app_launch::tilolang_hosts::task_packages() {
        if !named.insert(package.app.clone()) {
            continue;
        }
        if let Some(under) = package.records_under {
            records_under.insert(package.app, under);
        }
        apps.push(App::Module(package.module));
    }
    let installed = match app_catalogue::apps::signed_surfaces_from_env() {
        Ok(installed) => installed,
        Err(error) => {
            tracing::warn!(%error, "agent door: the installed apps' surfaces are not read");
            Vec::new()
        }
    };
    for (app, title, surface) in installed {
        if named.contains(&app) {
            continue;
        }
        let Ok(surface) = String::from_utf8(surface) else {
            tracing::warn!(%app, "agent door: an app's surface is not text");
            continue;
        };
        named.insert(app.clone());
        apps.push(App::Surface {
            app,
            title,
            surface,
        });
    }
    (apps, records_under)
}

/// Where the session's door starts a connection's task host: a thread of the session's own running
/// the task's tilolang host (`tilolang_host::task`) over the program connection. The host holds no
/// authority: it decides nothing and writes no record, and everything its programs read or change
/// comes back over the connection to the door, which does it as the program's author, through the
/// same gate as every other call.
pub(crate) struct TaskHosts {
    /// The person the connection was admitted for: `me` in its programs.
    pub(super) person: std::sync::Mutex<Option<String>>,
    /// What a connection asks the session's own doors, for the records the packages it runs itself
    /// keep: its task host names the modules they are read by, before it starts, on the loop's
    /// pass.
    session: Arc<SessionDoors>,
}

impl TaskHosts {
    /// The hosts of the task connections `person` asks about, whose programs run as `person`,
    /// reading the session's own records through `session`.
    pub(crate) fn new(person: &str, session: Arc<SessionDoors>) -> Self {
        Self {
            person: std::sync::Mutex::new(Some(person.to_owned())),
            session,
        }
    }
}

impl tilo_runtime::mcp::TaskHosts for TaskHosts {
    fn start(&self, workspace: &str, task: &str) -> io::Result<std::os::fd::OwnedFd> {
        let (door_end, host_end) = UnixStream::pair()?;
        // The person's own connection into the same session: the lines they type run there,
        // where the `let`s the agent's programs bound are.
        let (_person_ours, person_host) = UnixStream::pair()?;
        // The apps the task's programs are compiled against: every package the session runs itself
        // (the shell), whose records a program reads by the app, every installed tilolang app by
        // its package, every native app this device has, from its signed surface, and the
        // platform's own (the device and its arrangement, Settings).
        let (done, whole) = tilo_actions::remote::pending();
        self.session.ask(DoorAsk::Apps(done));
        let mut apps = whole.wait_timeout(WAIT).ok().flatten().unwrap_or_default();
        let kept = self.session.kept.lock_recover().clone();
        let (installed, records_under) = installed_apps(&kept);
        apps.extend(installed);
        let encoded = runtime_portal::tilolang_host::encode_apps(&apps);
        let apps = tilo_render_protocol::shm::sealed_blob("tilolang-apps", &encoded)?;
        let request = runtime_portal::tilolang_host::TaskRequest {
            task: task.to_owned(),
            room: workspace.to_owned(),
            person: self.person.lock_recover().clone(),
            apps_len: encoded.len() as u64,
            records_under,
            changed_at_record: kept,
        };
        std::thread::Builder::new()
            .name("tilo-task-host".into())
            .spawn(move || {
                let fds = runtime_portal::tilolang_host::TaskFds {
                    program: host_end.into(),
                    apps,
                };
                let opened =
                    tilolang_host::task::Task::open_with(&request, fds, Some(person_host));
                let task = match opened {
                    Ok(task) => task,
                    Err(error) => {
                        tracing::warn!(task = request.task, %error, "agent door: a task's host did not start");
                        return;
                    }
                };
                // Every line the door or the person sends is answered; the host ends when the door
                // closes the connection, with the client that started it.
                while task.serve_all() {}
                tracing::debug!(task = request.task, "agent door: a task's host ended");
            })?;
        Ok(door_end.into())
    }
}
