//! The system services, and the door apps reach this session's actions through: one published side
//! for all of them, and the plane they read it on.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::Error;
use tilo_service::Started;
use tilo_surface_host::{Doors, Published};
use ui::host_operation::ResourceOperationStart;

use tilo_action_broker::collab_broker::FanoutState;

use crate::plane::{DeskAuthority, Sealed, ShortcutLibrary};
use crate::{
    ActionPlane, DesktopHandle, Options, Plane, Service, Services, activity, activity_provider,
    plane, surfaces,
};

/// What a service answered, as a caller of the action protocol reads it.
pub(crate) fn answered(started: Started) -> ResourceOperationStart {
    match started {
        Started::Done(output) => ResourceOperationStart::Completed(output),
        Started::Accepted => ResourceOperationStart::Accepted,
    }
}

/// The system services' published side, behind the session's door like every hosted surface: one
/// action host for all of them.
struct ServiceDoor {
    services: Rc<RefCell<Services>>,
}

/// A surface app reaches the desktop's actions exactly as a person's palette does: through the door
/// over every surface it hosts.
impl surfaces::ActionDoor for tilo_surface_host::Doors {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        tilo_surface_host::Doors::resources(self)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        tilo_surface_host::Doors::invoke(
            self,
            address,
            arguments,
            &mut tilo_surface_host::Look::none(),
        )
    }
}

impl Published for ServiceDoor {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.services.borrow().resources()
    }

    /// The lock screen asks the session through here while it is locked, so the services stay
    /// behind the door; only the lock's own resource is offered then.
    fn serves_while_locked(&self) -> bool {
        true
    }

    fn locked_resources(&mut self) -> Vec<ResourceSpec> {
        self.services
            .borrow()
            .resources()
            .into_iter()
            .filter(|resource| resource.id.as_str() == crate::lock_service::LOCK)
            .collect()
    }

    fn revision(&self) -> Option<u64> {
        Some(self.services.borrow().revision())
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        match self
            .services
            .borrow_mut()
            .invoke(&address.resource, &address.action, arguments)
        {
            Some(Ok(Started::Done(output))) => Ok(ResourceOperationStart::Completed(output)),
            Some(Ok(Started::Accepted)) => Ok(ResourceOperationStart::Accepted),
            Some(Err(error)) => Err(error),
            None => Err(DoorError::Unknown(address.clone())),
        }
    }

    /// An agent's or a script's invocation reaches the service as theirs, so the change a record's
    /// model makes is attributed to them, not to the person the service runs for.
    fn invoke_as(
        &mut self,
        actor: &str,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ResourceOperationStart, DoorError> {
        // An agent is named `agent:<id>` , as the agent door names it.
        let invoker = tilo_actions::ActionInvoker::actor(actor);
        let invoker = if actor.starts_with("agent:") {
            invoker.with_kind(tilo_actions::ActorKind::Agent)
        } else {
            invoker
        };
        match self.services.borrow_mut().invoke_by(
            &invoker,
            &address.resource,
            &address.action,
            arguments,
        ) {
            Some(started) => started.map(answered),
            None => Err(DoorError::Unknown(address.clone())),
        }
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// What the plane decides against on the collaboration side: what each of this device's workspaces
/// revokes, the client to the person's collaboration plane when there is one, and the broker and
/// keys a workspace's sealed records are opened with.
pub(crate) struct Collaboration<'a> {
    pub revocations: crate::engine::Revocations,
    pub sync: Option<&'a crate::sync::Client>,
    /// This session's own workspaces, as its engine holds them, when it has an identity to hold
    /// them with: what a change to a workspace's people is written through, whatever the session's
    /// collaboration plane.
    pub engine: Option<std::sync::Arc<crate::engine::Engine>>,
    pub sealed: Sealed,
    /// What the broker hands the apps that have a record open: the one this session hands a
    /// sibling's writes to, so those reach the apps on the route this device's own writes do.
    pub fanout: std::sync::Arc<FanoutState>,
    /// The records' content this session keeps and asks the workspaces for.
    pub content: Option<std::sync::Arc<crate::content::Plane>>,
    /// The records' content in search's index, read from the records.
    pub records: Option<std::sync::Arc<crate::record_index::RecordIndex>>,
    /// The programs agents write in this session's tasks, run by its door.
    pub task_programs: Option<std::sync::Arc<crate::task_programs::TaskPrograms>>,
    /// This session's Nearby Send: the fabric it reaches the person's other devices on, and the
    /// spool a file leaves from.
    pub nearby: Option<std::sync::Arc<crate::nearby_transfer::NearbyTransfer>>,
}

/// Where this session's apps reach its services, when it serves them: the
/// socket the launcher hands them, the session's own, or the one it was told
/// to serve.
///
/// # Errors
///
/// The services root the launcher hands apps could not be made this user's,
/// or this login has no private directory to serve the session's own socket
/// in.
fn action_plane_path(options: &Options) -> Result<Option<std::path::PathBuf>, Error> {
    Ok(match &options.action_plane {
        ActionPlane::Off => return Ok(None),
        ActionPlane::Launcher => {
            // The launcher hands an app the socket under the services root,
            // which only its owner may reach.
            launcher::services::ensure_services_root()?;
            Some(launcher::services::collab_socket())
        }
        ActionPlane::Own => Some(own_plane_path()?),
        ActionPlane::At(path) => Some(path.clone()),
    })
}

/// The socket this session serves its own apps on when it draws in a window of
/// another desktop: [`own_plane_path_in`] for this process, in a directory
/// only this account may reach, as the broker that binds it requires.
///
/// # Errors
///
/// This login has no runtime directory to keep the socket private in, or the
/// directory could not be made.
fn own_plane_path() -> Result<std::path::PathBuf, std::io::Error> {
    let path = own_plane_path_in(std::env::var_os("XDG_RUNTIME_DIR")).ok_or_else(|| {
        std::io::Error::other("this login has no runtime directory to serve the session's apps in")
    })?;
    private_parent(&path)?;
    Ok(path)
}

/// [`own_plane_path`] for the runtime directory it reads, so what it decides
/// can be checked: the login's private session directory under `runtime_dir`,
/// with a socket named for this process, so two sessions of one login never
/// take each other's.
fn own_plane_path_in(runtime_dir: Option<std::ffi::OsString>) -> Option<std::path::PathBuf> {
    let directory = tilo_paths::login::session_dir(runtime_dir)?;
    Some(directory.join(format!("services-{}.sock", std::process::id())))
}

/// Make the directory `path` lives in exist and be this account's alone: the broker that binds a
/// socket there serves only a directory no one else may reach.
fn private_parent(path: &std::path::Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::DirBuilderExt as _;
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    if !parent.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    Ok(())
}

/// A request from another of the person's own devices is them using this machine: it holds suspend
/// off for a lease, so nothing falls asleep mid-task however far the person is from it.
fn attach_remote_use(actions: &crate::device_actions::DeviceActions, options: &Options) {
    if let Some(power) = options.power.clone() {
        actions.attach_power(power);
    }
}

/// The device fabric this session acts on: the one its collaboration endpoint runs, or a stand-in a
/// test handed its options.
fn acting_fabric(
    options: &Options,
    sync: Option<&crate::sync::Client>,
) -> Option<tilo_device_fabric::link::SharedLink> {
    options
        .fabric
        .clone()
        .or_else(|| sync.and_then(crate::sync::Client::fabric))
}

/// The plane the apps read and invoke this session's services on, when it serves one: the socket
/// the launcher hands its apps, and the link the loop answers their calls through.
/// The broker it serves on is `collaboration` 's, which the session's channel service gates a
/// workspace's sealed records with, and whose keys an endpoint asks a record's keys from. What the
/// plane takes from the session besides its broker: the turns its agents answer, who keeps those
/// agents, and the tools service apps reach their lists' changes through.
pub(crate) struct Knitted {
    pub(crate) turns: Option<std::sync::Arc<crate::conversations::Runs>>,
    pub(crate) agents: Option<std::sync::Arc<dyn tilo_action_broker::services::AgentRoster>>,
    pub(crate) tools: Option<crate::mount::tools::Handle>,
}

pub(crate) fn start_action_plane(
    options: &Options,
    live_activities: &std::sync::Arc<runtime_collab::LiveActivities>,
    handle: &DesktopHandle,
    shortcuts: std::sync::Arc<ShortcutLibrary>,
    activity: &activity::Activity,
    seams: Knitted,
    collaboration: Collaboration<'_>,
) -> Result<Option<Plane>, Error> {
    let Knitted {
        turns,
        agents,
        tools,
    } = seams;
    let Collaboration {
        revocations,
        sync,
        engine,
        sealed,
        fanout,
        content,
        records,
        task_programs,
        nearby,
    } = collaboration;
    let Some(path) = action_plane_path(options)? else {
        return Ok(None);
    };
    let link = plane::Link::new(options.wake.handle());
    let desk = desk_authority(options, handle, revocations)?;
    // The workspace's activity, as the apps this session runs read it: the same model the desk
    // serves its own windows.
    let activity_wire = activity_provider::wire(activity.requests());
    activity.serve_apps(std::sync::Arc::clone(&activity_wire));
    // Calls run over the collaboration plane: an app joins a channel through the service, the
    // session says the channel in its presence, and what the workspace says of its calls is what
    // the app's channels show.
    let calls = crate::calls::Calls::new();
    let calls_service = calls_service(sync, &calls, options);
    // An app's action on another of the person's own devices leaves through the plane the
    // collaboration endpoint runs.
    let actions = sync.and_then(crate::sync::Client::device_actions);
    let own_device = actions.clone().map(|actions| {
        std::sync::Arc::new(actions)
            as std::sync::Arc<dyn tilo_action_broker::services::OwnDeviceActions>
    });
    // An offloadable action runs where a device of its workspace would run it sooner: the round
    // asks them over that same plane, and what the device that wins produced is kept in the content
    // this session keeps.
    let action_offload = offload_route(actions.as_ref(), content.as_ref());
    // The fabric this session acts on, and what its services offer on it.
    let fabric = acting_fabric(options, sync);
    // Files' clipboard lives beside the broker that answers it, and its copies reach the loop that
    // has the compositor through the same wake.
    let file_clipboard = std::sync::Arc::new(crate::file_clipboard::NativeFileClipboard::new(
        options.wake.handle(),
    ));
    // The person's own records this session keeps, open to their other devices: what the broker
    // keeps them in as well as the desktop, and what a sibling's writes reach this session's apps
    // through.
    let personal = sync
        .and_then(crate::sync::Client::personal)
        .map(|personal| personal.sync().clone());
    let channels = std::sync::Arc::clone(&sealed.broker);
    let broker = plane::serve(
        path.clone(),
        std::sync::Arc::clone(&link),
        std::sync::Arc::clone(&options.launches),
        fabric,
        plane::Hosted {
            live_activities: std::sync::Arc::clone(live_activities),
            native_media: calls_service.clone(),
            // The index search reads is the one apps publish into: a closed app's content is found
            // from here on.
            content: options.search.content.clone(),
            records,
            desk: std::sync::Arc::clone(&desk),
            shortcuts,
            activity: activity_wire,
            // The turns the broker delivers to the agents admitted here, and hears back what they
            // report of them.
            turns,
            agents,
            sealed,
            own_device,
            record_content: content,
            action_offload,
            // What the broker writes a workspace's people through, and where it keeps what it
            // admits: this session's own workspaces, else the ones its collaboration plane holds.
            engine: engine.or_else(|| sync.and_then(crate::sync::Client::engine).cloned()),
            journal_root: options.state.as_deref().map(plane::journal_root),
            // What each installed app declares it may do on the action door,
            // read by search's own worker off the session's thread.
            lists: options.search.lists.clone(),
            // Files' waits for the drive and app lists to change, which the tools service answers.
            tools,
            task_programs,
            nearby,
            // What a person copies or cuts in Files, pasted here, and what the loop puts on the
            // desktop's clipboard for another app.
            file_clipboard: std::sync::Arc::clone(&file_clipboard),
            personal,
            fanout: std::sync::Arc::clone(&fanout),
        },
    )?;
    // The store's link is answered by this session's own endpoint, in `sync` : the door takes it
    // from there, once the broker is up to hand what a sibling writes to the apps that have the
    // record open.
    let personal = match (sync, sync.and_then(crate::sync::Client::endpoint)) {
        (Some(sync), Some(endpoint)) => sync.personal().map(|personal| {
            // The person's other devices are dialled as this session's siblings too: the session
            // between two of their desks tells each the workspaces the other holds.
            personal.serve(
                tilo_rt::app().handle(),
                endpoint,
                channels,
                fanout,
                sync.sibling_dials(),
            )
        }),
        _ => None,
    };
    // What arrives on this device's action plane is answered by the broker that now serves: another
    // of the person's devices runs nothing here the person in front of it could not.
    let arriving = (actions, broker.app_action_broker());
    if let (Some(actions), Some(host)) = arriving {
        actions.install(host.device_host());
        attach_remote_use(&actions, options);
    }
    Ok(Some(Plane {
        link,
        path,
        desk,
        broker,
        calls,
        calls_service,
        file_clipboard,
        _personal: personal,
    }))
}

/// What the broker decides against, from this pass on: the desk as it is
/// before the person touches anything, and every desk it shows after. It
/// journals what it admits on the person's behalf beside this device's desk,
/// so a shortcut a trigger starts survives the session stopping mid-run —
/// once, as it would have run.
fn desk_authority(
    options: &Options,
    handle: &DesktopHandle,
    revocations: crate::engine::Revocations,
) -> Result<std::sync::Arc<DeskAuthority>, Error> {
    let journal = match options.state.as_deref() {
        Some(state) => {
            let mut path = crate::desk_doc::path(state).into_os_string();
            path.push(".invocations");
            let path = std::path::PathBuf::from(path);
            if let Some(directory) = path.parent() {
                std::fs::create_dir_all(directory)?;
            }
            Some(path)
        }
        None => None,
    };
    Ok(std::sync::Arc::new(DeskAuthority::new(
        handle.read(Clone::clone),
        revocations,
        journal,
    )))
}

/// The calls the apps reach on this session's collaboration plane, or none when it has no endpoint.
fn calls_service(
    sync: Option<&crate::sync::Client>,
    calls: &std::sync::Arc<crate::calls::Calls>,
    options: &Options,
) -> Option<tilo_native_media::NativeMediaService> {
    let sync = sync?;
    calls.follow(sync, None);
    let service = tilo_native_media::NativeMediaService::new();
    // A call's lane runs on the session's own endpoint, sealed with the workspace's channel keys
    // and admitted by the workspace's governance: what every member of the workspace needs to be in
    // the channel's call.
    let roster = sync.engine().map_or_else(
        || {
            std::sync::Arc::new(|_: &str| Vec::new())
                as std::sync::Arc<dyn Fn(&str) -> Vec<[u8; 32]> + Send + Sync>
        },
        |engine| {
            let engine = std::sync::Arc::clone(engine);
            std::sync::Arc::new(move |workspace: &str| {
                engine.roster_devices(workspace).unwrap_or_default()
            })
        },
    );
    let sources = crate::calls::Sources::carrier(
        std::sync::Arc::clone(calls),
        sync.endpoint_handle(),
        sync.engine().map(|engine| engine.keys()),
        roster,
        options.questions.clone(),
        sync.own().user,
    );
    service.install_factory(std::sync::Arc::new(crate::calls::Factory::new(sources)));
    Some(service)
}

/// Where an app's offloadable action runs: the round this device's plane asks the invoking
/// workspace's other devices, keeping what the device that wins produced in `content` — the store
/// the invoking app reads its output from. `None` when this session has no device plane or keeps no
/// content to weigh the job by, so every offloadable action runs here.
fn offload_route(
    actions: Option<&crate::device_actions::DeviceActions>,
    content: Option<&std::sync::Arc<crate::content::Plane>>,
) -> Option<std::sync::Arc<dyn tilo_actions::offload::ActionOffload>> {
    let (actions, content) = (actions?, content?);
    actions.offload_route(std::sync::Arc::clone(content))
}

/// The services, published on the door.
pub(crate) fn mount_services(
    doors: &mut Doors,
    started: Vec<Box<dyn Service>>,
) -> Rc<RefCell<Services>> {
    let mut services = Services::default();
    for service in started {
        services.add(service);
    }
    let services = Rc::new(RefCell::new(services));
    doors.add(Rc::new(RefCell::new(ServiceDoor {
        services: Rc::clone(&services),
    })));
    services
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::Path;

    use super::own_plane_path_in;

    /// A session in another desktop's window serves its apps in the login's private session
    /// directory, under a name of its own; a login with no runtime directory of its own leaves it
    /// nowhere private to serve them.
    #[test]
    fn a_sessions_own_socket_lives_in_its_private_directory() {
        let path = own_plane_path_in(Some(OsString::from("/run/user/1000")))
            .expect("a login with a runtime directory has a private one");
        assert_eq!(
            path.parent(),
            Some(Path::new("/run/user/1000/tilo-session"))
        );
        assert_eq!(
            path.file_name(),
            Some(std::ffi::OsStr::new(&format!(
                "services-{}.sock",
                std::process::id()
            ))),
            "{} is a socket named for this session",
            path.display()
        );
        assert!(own_plane_path_in(None).is_none());
        assert!(own_plane_path_in(Some(OsString::from("relative"))).is_none());
    }
}
