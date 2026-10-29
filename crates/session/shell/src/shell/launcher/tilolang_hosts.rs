//! Tilolang hosts: one process per workspace and author that runs that author's tilolang apps
//! in that workspace, each as an instance (`docs/designs/app-language/ui-host.md`, step 2b).
//!
//! A tilolang app's window is still a launched process of its own, with its own launch
//! identity for its surface and accessibility. When its process serves a window door
//! (`window_doors`), the shell launches it as a window alone (`--tilolang-hosted`: the launcher
//! gives it no collaboration connection) and hands the window's instance to the host of its
//! workspace and author:
//!
//! - a patch connection: one end to the window, over its window door
//!   (`runtime_portal::windows::PATCH_VERB`), the other to the host;
//! - the instance's collaboration connection, attested against a launch record of the instance
//!   that carries the window's launch's creation record and ticket
//!   ([`register_tilolang_instance`](super::register_tilolang_instance)), exactly as a launched
//!   app's connection is: the broker attributes it from that record, never from the host;
//! - its identity-service connection, opened and named for the app and workspace as the
//!   launcher opens a launched app's;
//! - its module, read from the installation verified as a launch verifies it, in a sealed memfd.
//!
//! The author is the publisher whose key signed the package; a person's own build, signed with
//! their own key, runs in a host of that person's. A host is started through the launch kind
//! "tilolang host" (`tilo-launcher tilolang-host`), keyed by its workspace, its author and the
//! runtimes it runs on, with the storage of that workspace and author; it ends once the last of
//! its instances ended, which it says on its door.

use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::sync::{Arc, Mutex};

use runtime_portal::tilolang_host::{InstanceFds, InstanceRequest};

use super::{InstalledApp, RoomId, lock};

/// The launcher's flag for a window whose instance a tilolang host runs.
pub(super) const HOSTED_ARG: &str = "--tilolang-hosted";

/// Whose apps a host runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Author {
    /// The publisher whose key signed the packages.
    Publisher([u8; 32]),
    /// A person, for their own builds.
    Person(String),
    /// An agent task, for the programs it runs.
    Task(String),
}

impl Author {
    /// The author of a package signed by `publisher`: the person themselves when it is their
    /// own key, else the publisher.
    fn of(publisher: [u8; 32], person: &str) -> Self {
        if hex::encode(publisher) == person {
            Self::Person(person.to_owned())
        } else {
            Self::Publisher(publisher)
        }
    }

    /// The author's key in the hosts' names and storage: one path component.
    fn key(&self) -> String {
        match self {
            Self::Publisher(key) => format!("publisher-{}", hex::encode(key)),
            Self::Person(person) => format!("person-{}", hex::encode(person)),
            Self::Task(task) => format!("{TASK_KEY}{}", hex::encode(task)),
        }
    }
}

/// How a task's key starts.
const TASK_KEY: &str = "task-";

/// What identifies one host.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct HostKey {
    workspace: String,
    author: String,
    /// The tilolang runtime and the Tilo runtime it runs on: an instance runs only in a host of
    /// the runtimes its package names.
    runtime: String,
    app_runtime: String,
}

/// A running host.
struct Host {
    /// The session's end of its door.
    door: Arc<OwnedFd>,
    /// The windows whose instances it runs.
    instances: HashSet<u64>,
    /// Which start of the host this is, so the end of an earlier one never retires a later one.
    generation: u64,
}

static HOSTS: Mutex<Option<HashMap<HostKey, Host>>> = Mutex::new(None);

/// Whether `app` is a tilolang app: its manifest's entrypoint runs a tilolang runtime. Read
/// only to decide how its window is launched; what the host runs is read from the installation
/// verified as a launch verifies it.
pub(super) fn runs_in_host(app: &InstalledApp) -> bool {
    std::fs::read_to_string(&app.manifest_path)
        .ok()
        .and_then(|source| ::launcher::manifest::ManifestView::from_toml_str(&source).ok())
        .and_then(|manifest| manifest.entrypoint.runtime)
        .is_some_and(|runtime| ::launcher::run::tilolang_host::is_tilolang(&runtime))
}

/// A tilolang app whose process serves a window door (`served`) is its windows alone: a
/// tilolang host runs their instances. Its `command` says so, and its door is marked.
pub(super) fn present_only(
    app: &InstalledApp,
    room: RoomId,
    served: bool,
    command: &mut super::LaunchCommand,
) {
    if served && runs_in_host(app) {
        command.args.push(HOSTED_ARG.to_owned());
        super::window_doors::host_instances(app, room);
    }
}

/// A patch connection: the window's end and the host's.
pub(super) fn patch_pair() -> std::io::Result<(OwnedFd, OwnedFd)> {
    runtime_portal::tilolang_host::pair()
}

/// The first window of `app` in `room`, the process `launched` started: send it its patch
/// connection on its window door and hand its instance to its host.
pub(super) fn first_window(app: &InstalledApp, room: RoomId, instance: u64, launched: u32) {
    let sent = patch_pair().and_then(|(window, host)| {
        super::window_doors::send_patch(&app.package_id, &room.to_string(), instance, &window)?;
        Ok(host)
    });
    match sent {
        Ok(host) => hand_over(app, room, instance, launched, host),
        Err(error) => tracing::error!(
            %error,
            package_id = %app.package_id,
            "a tilolang window got no patch connection"
        ),
    }
}

/// Hand the instance of the window `instance` of `app` in `room`, whose launch is the process
/// `launched`, to the host of its workspace and author, with `patch`, the host's end of the
/// window's patch connection. Done off the caller's thread: it verifies the package and may
/// start the host.
pub(super) fn hand_over(
    app: &InstalledApp,
    room: RoomId,
    instance: u64,
    launched: u32,
    patch: OwnedFd,
) {
    let app = app.clone();
    let started = std::thread::Builder::new()
        .name("tilolang-handover".into())
        .spawn(move || {
            if let Err(error) = hand_over_now(&app, room, instance, launched, patch) {
                tracing::error!(
                    %error,
                    package_id = %app.package_id,
                    instance,
                    "a tilolang window's instance was not handed to its host"
                );
            }
        });
    if let Err(error) = started {
        tracing::error!(%error, "a tilolang window's instance could not be handed over");
    }
}

fn hand_over_now(
    app: &InstalledApp,
    room: RoomId,
    instance: u64,
    launched: u32,
    patch: OwnedFd,
) -> std::io::Result<()> {
    let refused = |what: String| std::io::Error::new(std::io::ErrorKind::PermissionDenied, what);
    let began = std::time::Instant::now();
    let workspace = room.to_string();
    let installed = super::installed_copy(app)
        .ok_or_else(|| refused("the app is not installed as launched".to_owned()))?;
    let evidence = super::original_application_creation(app, &installed)
        .ok_or_else(|| refused("the app has no verified creation record".to_owned()))?;
    let verified = ::launcher::run::tilolang_host::verified_instance(
        &installed,
        &app.manifest_path,
        &app.payload_root,
        &evidence.package_hash,
    )
    .map_err(|error| refused(error.to_string()))?;
    let person = crate::shell::state::local_user_id_for_room(&room);
    let key = HostKey {
        workspace: workspace.clone(),
        author: Author::of(verified.publisher, &person).key(),
        runtime: verified.runtime,
        app_runtime: verified.app_runtime,
    };
    let verified_us = began.elapsed().as_micros();
    let module = verified.module.encode();
    let module_len = module.len() as u64;
    let module = tilo_render_protocol::shm::sealed_blob("tilolang-module", &module)?;
    let module_us = began.elapsed().as_micros();
    let broker = instance_collaboration(&app.package_id, &workspace, instance, launched)?;
    // Opened and named for the app and workspace exactly as the launcher opens a launched
    // app's: the identity service signs for that app in that workspace only.
    let identity =
        ::launcher::services::resolve_identity_bind(&app.package_id, Some(&workspace), false)
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .ok_or_else(|| refused("the identity service is unavailable".to_owned()))?
            .host;
    let connections_us = began.elapsed().as_micros();
    let request = InstanceRequest {
        app: app.package_id.clone(),
        room: workspace,
        instance,
        person,
        module_len,
        records_under: Some(
            super::installed_earlier_ids()
                .records_under(&app.package_id)
                .to_owned(),
        )
        .filter(|id| *id != app.package_id),
    };
    // An app whose types hold an outside service's resources reaches its connector, which
    // holds the network and custody the host has none of.
    let origin = super::origin_connectors::connection(app, room).map_err(|error| {
        refused(format!(
            "the app's origin connector cannot be reached: {error}"
        ))
    })?;
    let fds = InstanceFds {
        broker,
        identity,
        module,
        window: patch,
        origin,
    };
    let started = send_to_host(&key, &request, &fds)?;
    tracing::info!(
        target: tilo_startup_metrics::LAUNCH_LATENCY_TARGET,
        package_id = %app.package_id,
        instance,
        verified_us,
        module_us,
        connections_us,
        handed_us = began.elapsed().as_micros(),
        host_started = started,
        "tilolang instance handed over"
    );
    Ok(())
}

/// Start the host of the agent task `task` in `room` (`docs/designs/app-language/agents.md`,
/// "How a program runs"): a tilolang host whose author is the task, through the same launch kind
/// and sandbox as every other, handed the task's program connection and the module of every
/// installed tilolang app, each verified as a launch verifies it. Answers the agent door's end of
/// the program connection; the host ends once the door closes it.
///
/// # Errors
/// No tilolang app is installed, or the host could not be started.
pub(super) fn start_task(room: RoomId, task: &str) -> std::io::Result<OwnedFd> {
    let refused = |what: String| std::io::Error::new(std::io::ErrorKind::NotFound, what);
    let apps = super::installed_apps().map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut modules = Vec::new();
    let mut records_under = std::collections::BTreeMap::new();
    let mut runtimes: Option<(String, String)> = None;
    for app in apps.iter().filter(|app| runs_in_host(app)) {
        let verified = super::installed_copy(app).and_then(|installed| {
            let evidence = super::original_application_creation(app, &installed)?;
            ::launcher::run::tilolang_host::verified_instance(
                &installed,
                &app.manifest_path,
                &app.payload_root,
                &evidence.package_hash,
            )
            .ok()
        });
        let Some(verified) = verified else {
            tracing::warn!(package_id = %app.package_id, "a task's host leaves out an app it could not verify");
            continue;
        };
        let pair = (verified.runtime.clone(), verified.app_runtime.clone());
        match &runtimes {
            Some(known) if *known != pair => {
                tracing::warn!(package_id = %app.package_id, "a task's host leaves out an app on other runtimes");
                continue;
            }
            Some(_) => {}
            None => runtimes = Some(pair),
        }
        let under = super::installed_earlier_ids()
            .records_under(&app.package_id)
            .to_owned();
        if under != app.package_id {
            records_under.insert(app.package_id.clone(), under);
        }
        modules.push(verified.module);
    }
    let (runtime, app_runtime) =
        runtimes.ok_or_else(|| refused("no tilolang app is installed".to_owned()))?;
    let key = HostKey {
        workspace: room.to_string(),
        author: Author::Task(task.to_owned()).key(),
        runtime,
        app_runtime,
    };
    let encoded = runtime_portal::tilolang_host::encode_modules(&modules);
    let request = runtime_portal::tilolang_host::TaskRequest {
        task: task.to_owned(),
        room: room.to_string(),
        person: Some(crate::shell::state::local_user_id_for_room(&room)),
        modules_len: encoded.len() as u64,
        records_under,
    };
    let modules = tilo_render_protocol::shm::sealed_blob("tilolang-modules", &encoded)?;
    let (door_end, host_end) = std::os::unix::net::UnixStream::pair()?;
    let host = start(&key)?;
    runtime_portal::tilolang_host::send_task(
        &host.door,
        &request,
        &runtime_portal::tilolang_host::TaskFds {
            program: host_end.into(),
            modules,
        },
    )?;
    // The host runs nothing else: it ends once the task does.
    // SAFETY: shutting down the writing half of a socket the session owns.
    let _ = unsafe { libc::shutdown(host.door.as_raw_fd(), libc::SHUT_WR) };
    tracing::info!(workspace = %room, task, apps = request.modules_len, "task host handed its task");
    Ok(door_end.into())
}

/// The instance's collaboration connection, attested as the launch of the window `instance` of
/// `app_id` in `workspace`, whose process is `launched`.
fn instance_collaboration(
    app_id: &str,
    workspace: &str,
    instance: u64,
    launched: u32,
) -> std::io::Result<OwnedFd> {
    let mut stream =
        std::os::unix::net::UnixStream::connect(::launcher::services::collab_socket())?;
    if !super::register_tilolang_instance(launched, app_id, workspace, instance) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the window is no live launch of the app in the workspace",
        ));
    }
    if let Err(error) =
        ::launcher::services::write_collab_window_handoff(&mut stream, app_id, workspace, instance)
    {
        super::withdraw_in_process_launch(app_id, workspace, Some(instance));
        return Err(error);
    }
    Ok(stream.into())
}

/// Send the instance to the host `key`, starting it first when none runs. Whether it started
/// the host.
#[allow(
    clippy::significant_drop_tightening,
    reason = "a host is chosen, started and handed the instance in one hold, so it never ends in between"
)]
fn send_to_host(
    key: &HostKey,
    request: &InstanceRequest,
    fds: &InstanceFds,
) -> std::io::Result<bool> {
    let mut hosts = lock(&HOSTS);
    let hosts = hosts.get_or_insert_with(HashMap::new);
    if let Some(host) = hosts.get_mut(key) {
        match runtime_portal::tilolang_host::send_instance(&host.door, request, fds) {
            Ok(()) => {
                host.instances.insert(request.instance);
                return Ok(false);
            }
            // The host is ending: start another.
            Err(error) => {
                tracing::info!(%error, "a tilolang host's door is closed");
                hosts.remove(key);
            }
        }
    }
    let mut host = start(key)?;
    runtime_portal::tilolang_host::send_instance(&host.door, request, fds)?;
    host.instances.insert(request.instance);
    hosts.insert(key.clone(), host);
    Ok(true)
}

/// Where the host `key` keeps what its instances keep.
fn storage(key: &HostKey) -> std::io::Result<std::path::PathBuf> {
    let data = tilo_paths::xdg::data_home()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no data directory"))?;
    let dir = data
        .join("tilo")
        .join("tilolang-hosts")
        .join(&key.workspace)
        .join(&key.author);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Start the host `key` through the launch kind, with its door at descriptor 3, and follow it
/// on a thread of its own until it ends.
fn start(key: &HostKey) -> std::io::Result<Host> {
    use std::os::unix::process::CommandExt as _;
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let storage = storage(key)?;
    let launcher =
        super::resolve_launcher_bin().map_err(|error| std::io::Error::other(error.to_string()))?;
    let (ours, theirs) = runtime_portal::tilolang_host::pair()?;
    let mut command = std::process::Command::new(launcher);
    command
        .arg(::launcher::run::tilolang_host::SUBCOMMAND)
        .arg("--runtime")
        .arg(&key.runtime)
        .arg("--app-runtime")
        .arg(&key.app_runtime)
        .arg("--storage")
        .arg(&storage)
        .env("XDG_RUNTIME_DIR", super::runtime_dir())
        .stdin(std::process::Stdio::null());
    let door = theirs.as_raw_fd();
    let slot = ::launcher::run::tilolang_host::DOOR_CHILD_FD;
    // SAFETY: only async-signal-safe calls between fork and exec: the door is placed at its
    // slot without close-on-exec.
    unsafe {
        command.pre_exec(move || {
            if door == slot {
                if libc::fcntl(slot, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            } else if libc::dup2(door, slot) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    drop(theirs);
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let door = Arc::new(ours);
    tracing::info!(
        workspace = %key.workspace,
        author = %key.author,
        pid = child.id(),
        "tilolang host started"
    );
    let (followed, key, kept) = (Arc::clone(&door), key.clone(), storage);
    std::thread::Builder::new()
        .name("tilolang-host".into())
        .spawn(move || {
            use runtime_portal::tilolang_host::{FromHost, receive_from_host};
            // Why the host says it exits; none when it ended without saying (killed, crashed).
            let mut reason: Option<String> = None;
            loop {
                match receive_from_host(&followed) {
                    Ok(Some(FromHost::Ended(instance))) => ended(&key, generation, instance),
                    Ok(Some(FromHost::Exiting(why))) => reason = Some(why),
                    Ok(None) => break,
                    // The packet was taken: the door carries the next one.
                    Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                        tracing::error!(%error, workspace = %key.workspace, author = %key.author, "a tilolang host said something unreadable");
                    }
                    // The door failed: it carries nothing more; the host is waited for.
                    Err(error) => {
                        tracing::error!(%error, workspace = %key.workspace, author = %key.author, "a tilolang host's door failed");
                        break;
                    }
                }
            }
            let status = child.wait();
            let clean = status.as_ref().is_ok_and(std::process::ExitStatus::success);
            let reason = reason.unwrap_or_else(|| "it ended without saying why".to_owned());
            if clean {
                tracing::info!(workspace = %key.workspace, author = %key.author, ?status, reason, "tilolang host ended");
            } else {
                tracing::error!(workspace = %key.workspace, author = %key.author, ?status, reason, "tilolang host ended");
                // Every end of a process, clean or not, is on the session's log of app exits.
                eprintln!(
                    "app-exit: tilolang-host workspace={} author={} status={status:?} reason={reason}",
                    key.workspace, key.author
                );
            }
            // A task keeps nothing: its storage goes with it.
            if key.author.starts_with(TASK_KEY) {
                let _ = std::fs::remove_dir_all(&kept);
            }
            let mut hosts = lock(&HOSTS);
            if let Some(hosts) = hosts.as_mut()
                && hosts.get(&key).is_some_and(|host| host.generation == generation)
            {
                hosts.remove(&key);
            }
        })?;
    Ok(Host {
        door,
        instances: HashSet::new(),
        generation,
    })
}

/// The host `key` (its start `generation`) says the instance of the window `instance` ended.
/// Once its last instance ended, it is closed: it ends when it has written what it keeps.
fn ended(key: &HostKey, generation: u64, instance: u64) {
    let mut guard = lock(&HOSTS);
    let Some(hosts) = guard.as_mut() else {
        return;
    };
    let Some(host) = hosts
        .get_mut(key)
        .filter(|host| host.generation == generation)
    else {
        return;
    };
    host.instances.remove(&instance);
    let closed = if host.instances.is_empty() {
        hosts.remove(key)
    } else {
        None
    };
    drop(guard);
    if let Some(host) = closed {
        // The host reads the end of its door and ends once it runs nothing.
        // SAFETY: shutting down the writing half of a socket the session owns.
        let _ = unsafe { libc::shutdown(host.door.as_raw_fd(), libc::SHUT_WR) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hosts_author_is_its_publisher_or_the_person_whose_own_build_it_is() {
        let key = [7_u8; 32];
        assert_eq!(Author::of(key, "ada"), Author::Publisher(key));
        assert_eq!(
            Author::of(key, &hex::encode(key)),
            Author::Person(hex::encode(key))
        );
        let names = [
            Author::Publisher(key).key(),
            Author::Person("ada".into()).key(),
            Author::Task("ada".into()).key(),
        ];
        assert!(names.iter().all(|name| !name.contains(['/', '\0', '.'])));
        assert_eq!(
            names.iter().collect::<HashSet<_>>().len(),
            3,
            "kinds never collide"
        );
    }
}
