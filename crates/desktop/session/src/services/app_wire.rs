//! The app wire apps reach this session's services on: one published side for all of them, and the
//! keys, authority and turns the loop answers their calls with.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_compositor::Error;
use tilo_service::Started;
use tilo_session_core::surfaces::{Published, Surfaces};
use ui::host_operation::ResourceOperationStart;

use tilo_workspace_service::apps::collab_broker::FanoutState;

use crate::app_wire::AppWire;
use crate::app_wire::{LayoutAuthority, Sealed};
use crate::{
    AppSocket, Options, Service, Services, activity, activity_provider, app_wire, surfaces,
};
use tilo_session_core::desktop::state::DesktopHandle;

/// What a service answered, as a caller of the action protocol reads it. Work that answers later
/// is taken: it goes on, and what it changes lands in the readings.
pub(crate) fn answered(started: Started) -> ResourceOperationStart {
    match started {
        Started::Done(output) => ResourceOperationStart::Completed(output),
        Started::Accepted | Started::Later(_) => ResourceOperationStart::Accepted,
    }
}

/// The system services' published side, on the app wire like every hosted surface: one action
/// host for all of them.
struct ServiceSurface {
    services: Rc<RefCell<Services>>,
}

/// A surface app reaches the desktop's actions exactly as a person's palette does: through the app wire
/// over every surface it hosts.
impl surfaces::ActionApi for tilo_session_core::surfaces::Surfaces {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        tilo_session_core::surfaces::Surfaces::resources(self)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, ApiError> {
        tilo_session_core::surfaces::Surfaces::invoke(
            self,
            address,
            arguments,
            &mut tilo_session_core::surfaces::Look::none(),
        )
    }
}

impl Published for ServiceSurface {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.services.borrow().resources()
    }

    /// The lock screen asks the session through here while it is locked, so the services stay
    /// off the app wire; only the lock's own resource is offered then.
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
        _look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<ResourceOperationStart, ApiError> {
        match self
            .services
            .borrow_mut()
            .invoke(&address.resource, &address.action, arguments)
        {
            Some(started) => started.map(answered),
            None => Err(ApiError::Unknown(address.clone())),
        }
    }

    /// An agent's or a script's invocation reaches the service as theirs, so the change a record's
    /// model makes is attributed to them, not to the person the service runs for.
    fn invoke_as(
        &mut self,
        actor: &tilo_actions::Actor,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<ResourceOperationStart, ApiError> {
        let invoker = tilo_actions::ActionInvoker::admitted(&actor.clone().into());
        match self.services.borrow_mut().invoke_by(
            &invoker,
            &address.resource,
            &address.action,
            arguments,
        ) {
            Some(started) => started.map(answered),
            None => Err(ApiError::Unknown(address.clone())),
        }
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// What the app wire decides against on the collaboration side: what each of this device's workspaces
/// revokes, the client to the person's peer network when there is one, and the broker and
/// keys a workspace's sealed records are opened with.
pub(crate) struct Collaboration<'a> {
    pub revocations: crate::engine::Revocations,
    pub sync: Option<&'a crate::sync::Client>,
    /// The shell link the device's daemon handed this session, or why it has none.
    pub daemon: tilo_workspace_service::apps::services::DeviceDaemon,
    /// Each write the daemon takes, as the session follows them.
    pub daemon_writes: tilo_session_client::daemon_documents::DaemonWritesFeed,
    /// Where this device's own workspace service is installed: the workspaces it holds take an
    /// app's window write, the one store the session's own writes and reads go through.
    pub service: tilo_workspace_service::service::ServiceSlot,
    /// This session's own workspaces, as its engine holds them, when it has an identity to hold
    /// them with: what a change to a workspace's people is written through, whatever the session's
    /// peer network.
    pub engine: Option<std::sync::Arc<crate::engine::Engine>>,
    /// How the person's workspaces stand, as the device's daemon publishes them, and the actions
    /// that change them.
    pub view: Option<std::sync::Arc<crate::workspace_view::WorkspaceView>>,
    pub sealed: Sealed,
    /// What the broker hands the apps that have a record open: the one this session hands a
    /// sibling's writes to, so those reach the apps on the route this device's own writes do.
    pub fanout: std::sync::Arc<FanoutState>,
    /// The records' content this session keeps and asks the workspaces for.
    pub content: Option<std::sync::Arc<crate::content::ContentService>>,
    /// The records' content in search's index, read from the records.
    pub records: Option<std::sync::Arc<crate::resource_index::RecordIndex>>,
    /// The programs agents write in this session's tasks, run by it.
    pub task_programs:
        Option<std::sync::Arc<tilo_workspace_service::workspaces::task_programs::TaskPrograms>>,
    /// The session's own actions, offered to a turn's agent as the broker's steps.
    pub session_actions: Option<std::sync::Arc<crate::session_actions::SessionActions>>,
    /// The person's settings, offered where Settings' window invokes them: the provider its
    /// controls name, answered by the session's settings service.
    pub settings_steps: Option<std::sync::Arc<crate::settings_steps::SettingsSteps>>,
    /// This session's Nearby Send: the fabric it reaches the person's other devices on, and the
    /// spool a file leaves from.
    pub nearby: Option<std::sync::Arc<crate::nearby_transfer::NearbyTransfer>>,
}

/// Where this session's apps reach its services, when it serves them: the
/// socket the launcher hands them, or the one it was told to serve.
///
/// # Errors
///
/// The services root the launcher hands apps could not be made this user's.
fn app_wire_path(options: &Options) -> Result<Option<std::path::PathBuf>, Error> {
    Ok(match &options.app_socket {
        AppSocket::Off => return Ok(None),
        AppSocket::Launcher => {
            // The launcher hands an app the socket under the services root,
            // which only its owner may reach.
            launcher::services::ensure_services_root()?;
            Some(launcher::services::collab_socket())
        }
        AppSocket::At(path) => Some(path.clone()),
    })
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
) -> Option<crate::platform::fabric::SharedLink> {
    options
        .fabric
        .clone()
        .or_else(|| sync.and_then(crate::sync::Client::fabric))
}

/// The app wire the apps read and invoke this session's services on, when it serves one: the socket
/// the launcher hands its apps, and the link the loop answers their calls through.
/// The broker it serves on is `collaboration` 's, which the session's channel service checks a
/// workspace's sealed records with, and whose keys an endpoint asks a record's keys from. What the
/// app wire takes from the session besides its broker: who keeps its agents, the sessions they work
/// in, and the tools service apps reach their lists' changes through.
pub(crate) struct Knitted {
    pub(crate) agents: Option<std::sync::Arc<dyn tilo_broker_core::services::AgentRoster>>,
    pub(crate) agent_sessions:
        Option<std::sync::Arc<dyn tilo_broker_core::services::AgentSessions>>,
    pub(crate) tools: Option<crate::tools::Handle>,
    /// Where each folder the files daemon holds stands, as it says over the tree socket.
    pub(crate) folders: tilo_workspace_service::files::Folders,
}

#[expect(
    clippy::too_many_lines,
    reason = "the app wire's services are all decided in one place, so a service added is not missed"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the service slot the agents start on is moved into their start once the broker \
              serves, not held to the end"
)]
pub(crate) fn start_app_wire(
    options: &Options,
    live_activities: &std::sync::Arc<runtime_collab::LiveActivities>,
    handle: &DesktopHandle,
    shortcuts: Option<
        std::sync::Arc<dyn tilo_workspace_service::apps::services::ShortcutLibraryServices>,
    >,
    activity: &activity::Activity,
    services: Knitted,
    collaboration: Collaboration<'_>,
) -> Result<Option<AppWire>, Error> {
    let Collaboration {
        revocations,
        sync,
        daemon,
        daemon_writes,
        engine,
        view,
        service,
        sealed,
        fanout,
        content,
        records,
        task_programs,
        session_actions,
        settings_steps,
        nearby,
    } = collaboration;
    let Some(path) = app_wire_path(options)? else {
        return Ok(None);
    };
    let agents_service = service.clone();
    let link = app_wire::Link::new(options.wake.handle());
    let layout = layout_authority(options, handle, revocations, engine.clone());
    // The workspace's activity as this session's apps read it: the shell's own model.
    let activity_wire = activity_provider::wire(activity.requests());
    activity.serve_apps(std::sync::Arc::clone(&activity_wire));
    // Calls run over the peer network: an app joins a channel through the service, the
    // session says the channel in its presence, and what the workspace says of its calls is what
    // the app's channels show.
    let calls = crate::calls::Calls::new();
    let calls_service = calls_service(sync, &calls, options);
    // An app's action on another of the person's own devices leaves through the app wire the
    // collaboration endpoint runs.
    let actions = sync.and_then(crate::sync::Client::device_actions);
    let own_device = actions.clone().map(|actions| {
        std::sync::Arc::new(actions)
            as std::sync::Arc<dyn tilo_broker_core::services::OwnDeviceActions>
    });
    // An offloadable action runs where a device of its workspace would run it sooner: the round
    // asks them over that same app wire, and what the device that wins produced is kept in the content
    // this session keeps.
    let action_offload = offload_route(actions.as_ref(), content.as_ref());
    // Files' clipboard lives beside the broker, its copies reaching the compositor's loop alike.
    let file_clipboard = std::sync::Arc::new(crate::file_clipboard::NativeFileClipboard::new(
        options.wake.handle(),
    ));
    // What Browser's asks for vault items are answered with: kept here as well, so a session
    // that holds a workspace's Browser can say so and name who asks its server.
    let item_lending = std::sync::Arc::new(crate::item_lending::ItemLending::default());
    // The workspace's events and its assistant's look-round, served beside the shortcuts they
    // are saved as.
    let initiative = shortcuts.clone().map(|library| {
        let events = std::sync::Arc::new(
            tilo_workspace_service::agents::events::WorkspaceEvents::new(service.clone(), library),
        );
        tilo_workspace_service::agents::initiative::Initiative::new(service.clone(), events)
    });
    let (broker, terminals) = app_wire::serve(
        path.clone(),
        std::sync::Arc::clone(&link),
        std::sync::Arc::clone(&options.launches),
        // The fabric this session acts on, and what its services offer on it.
        acting_fabric(options, sync),
        app_wire::Hosted {
            live_activities: std::sync::Arc::clone(live_activities),
            native_media: calls_service.clone(),
            // The index search reads is the one apps publish into: a closed app's content is found
            // from here on.
            content: Some(std::sync::Arc::clone(&options.content)),
            records,
            layout: std::sync::Arc::clone(&layout),
            shortcuts,
            initiative: initiative.clone(),
            activity: activity_wire,
            agents: services.agents,
            agent_sessions: services.agent_sessions,
            sealed,
            own_device,
            document_content: content.clone(),
            action_offload,
            // What the broker writes a workspace's people through, and where it keeps what it
            // admits: this session's own workspaces, else the ones its peer network holds.
            engine: engine.or_else(|| sync.and_then(crate::sync::Client::engine).cloned()),
            view,
            daemon,
            daemon_writes,
            service: service.clone(),
            endpoint: sync.map(|sync| sync.endpoint_handle()),
            device_actions: sync.map(crate::sync::Client::actions_handle),
            journal_root: match options.state.as_deref() {
                Some(state) => {
                    std::fs::create_dir_all(state.join("tilo"))?;
                    Some(app_wire::journal_root(state))
                }
                None => None,
            },
            // What each installed app declares it may do on the action API,
            // read by search's own worker off the session's thread.
            lists: options.lists.clone(),
            // Files' waits for the drive and app lists to change, which the tools service answers.
            tools: services.tools,
            task_programs,
            session_actions,
            settings_steps,
            nearby,
            // What a person copies or cuts in Files, pasted here, and what the loop puts on the
            // desktop's clipboard for another app.
            file_clipboard: std::sync::Arc::clone(&file_clipboard),
            item_lending: std::sync::Arc::clone(&item_lending),
            fanout: std::sync::Arc::clone(&fanout),
        },
    )?;
    // The person's other devices are dialled as this session's siblings, and the session's pass
    // wakes when their own workspace moves: the session between two of theirs is where they tell
    // each other what they hold, and what their device fabric says.
    if let Some(sync) = sync {
        sync.follow_person(tilo_rt::app().handle(), options.wake.handle());
    }
    // What arrives on this device's app wire is answered by the broker that now serves: another
    // of the person's devices runs nothing here the person in front of it could not.
    if let (Some(actions), Some(broker)) = (actions, broker.app_action_broker()) {
        actions.install(broker.device_host());
        attach_remote_use(&actions, options);
    }
    let files_tree = serve_files_tree(service, content.as_deref(), options, services.folders)?;
    // This device runs its workspaces' agents in its own workspace service once that opens: the
    // one engine a workspace's host runs, in this process, each turn's calls decided by this
    // broker as the person's own calls are.
    if let Some(broker) = broker.app_action_broker() {
        // What the assistant proposes is filed in the workspace's tracker through this broker.
        if let Some((initiative, actions)) =
            initiative
                .as_ref()
                .zip(tilo_workspace_service::agents::device_actions(
                    broker.device_host(),
                ))
        {
            initiative.act_through(actions);
        }
        tilo_rt::app()
            .handle()
            .spawn(tilo_workspace_service::agents::on_device(
                agents_service,
                broker.device_host(),
            ));
    }
    Ok(Some(AppWire {
        link,
        path,
        layout,
        broker,
        calls,
        calls_service,
        carets: crate::carets::Carets::new(fanout, options.wake.handle()),
        file_clipboard,
        _files_tree: files_tree,
        item_lending,
        terminals,
    }))
}

/// The trees of the workspaces this device's service holds, served where the files daemon
/// dials them ([`tilo_device_identity::login::files_tree_socket`]) to the daemon this session
/// recorded as the Files endpoint: the daemon's mounts sync through this device's one service.
/// A session that keeps no resource content has no workspaces, and serves none.
fn serve_files_tree(
    service: tilo_workspace_service::service::ServiceSlot,
    content: Option<&crate::content::ContentService>,
    options: &Options,
    folders: tilo_workspace_service::files::Folders,
) -> Result<Option<tilo_workspace_service::files::socket::Serving>, Error> {
    use tilo_workspace_service::apps::services::LauncherServices as _;
    let Some(content) = content else {
        return Ok(None);
    };
    let launches = std::sync::Arc::clone(&options.launches);
    let socket = tilo_device_identity::login::files_tree_socket().map_err(std::io::Error::other)?;
    // The keys of a tree's group, as this person's identity service hands them to this device:
    // only ever for the workspace the connection was admitted for.
    let identity = tilo_paths::xdg::service_socket("identity");
    let keys: tilo_workspace_service::files::socket::Keys = std::sync::Arc::new(move |workspace| {
        let keys = fs::hydration::FileKeys::new(workspace, identity.clone());
        Box::pin(async move {
            let keys = keys.access().await.map_err(|error| format!("{error:#}"))?;
            keys.to_bytes()
                .map(|bytes| bytes.to_vec())
                .map_err(|error| format!("{error:#}"))
        })
    });
    Ok(Some(tilo_workspace_service::files::socket::serve(
        &socket,
        service,
        content.route(),
        std::sync::Arc::new(move |pid, workspace| {
            launches.attest_process(pid, ui::apps::FILE_EXPLORER, workspace)
        }),
        content.files_offers(keys),
        folders,
    )))
}

/// What the broker decides against, from this pass on: the shell as it is
/// before the person touches anything, and every shell it shows after — and,
/// beside it, every workspace this device holds that no shell shows.
fn layout_authority(
    options: &Options,
    handle: &DesktopHandle,
    revocations: crate::engine::Revocations,
    held: Option<std::sync::Arc<crate::engine::Engine>>,
) -> std::sync::Arc<LayoutAuthority> {
    std::sync::Arc::new(
        LayoutAuthority::new(
            handle.read(Clone::clone),
            revocations,
            crate::app_wire::Names {
                person: options.person.clone(),
                earlier: options.earlier.clone(),
            },
        )
        .holding(held),
    )
}

/// The calls the apps reach on this session's peer network, or none when it has no endpoint.
fn calls_service(
    sync: Option<&crate::sync::Client>,
    calls: &std::sync::Arc<crate::calls::Calls>,
    options: &Options,
) -> Option<tilo_native_media::NativeMediaService> {
    let sync = sync?;
    calls.follow(sync, None);
    let service = tilo_native_media::NativeMediaService::new();
    // A call's stream runs on the session's own endpoint, sealed with the workspace's channel keys
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

/// Where an app's offloadable action runs: the round this device's app wire asks the invoking
/// workspace's other devices, keeping what the device that wins produced in `content` — the store
/// the invoking app reads its output from. `None` when this session has no device action service or keeps no
/// content to weigh the job by, so every offloadable action runs here.
fn offload_route(
    actions: Option<&crate::device_actions::DeviceActions>,
    content: Option<&std::sync::Arc<crate::content::ContentService>>,
) -> Option<std::sync::Arc<dyn tilo_actions::offload::ActionOffload>> {
    let (actions, content) = (actions?, content?);
    actions.offload_route(std::sync::Arc::clone(content))
}

/// The services, published on the app wire.
pub(crate) fn mount_services(
    surfaces: &mut Surfaces,
    started: Vec<Box<dyn Service>>,
) -> Rc<RefCell<Services>> {
    let mut services = Services::default();
    for service in started {
        services.add(service);
    }
    let services = Rc::new(RefCell::new(services));
    surfaces.add(Rc::new(RefCell::new(ServiceSurface {
        services: Rc::clone(&services),
    })));
    services
}
