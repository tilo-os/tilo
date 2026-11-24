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

    /// The person answered the question this action's contract asks: the service's own confirmed
    /// path runs it, once.
    fn invoke_confirmed(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        _look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<ResourceOperationStart, ApiError> {
        match self.services.borrow_mut().invoke_confirmed(
            &address.resource,
            &address.action,
            arguments,
        ) {
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

    /// What notes the workspaces' records changing, for the roles that follow them.
    pub records: Option<std::sync::Arc<crate::resource_index::RecordChanges>>,
    /// The programs agents write in this session's tasks, run by it.
    pub task_programs:
        Option<std::sync::Arc<tilo_workspace_service::workspaces::task_programs::TaskPrograms>>,
    /// The session's own actions, offered to a turn's agent as the broker's steps.
    pub session_actions: Option<std::sync::Arc<crate::session_actions::SessionActions>>,
    /// The person's settings, offered where Settings' window invokes them: the provider its
    /// controls name, answered by the session's settings service.
    pub settings_steps: Option<std::sync::Arc<crate::settings_steps::SettingsSteps>>,
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
/// app wire takes from the session besides its broker: who keeps its agents and the sessions they
/// work in.
pub(crate) struct Knitted {
    pub(crate) agents: Option<std::sync::Arc<crate::agents::Roster>>,
    pub(crate) agent_sessions: Option<std::sync::Arc<crate::agent_sessions::AgentSessions>>,
}

#[expect(
    clippy::too_many_lines,
    reason = "the app wire's services are all decided in one place, so a service added is not missed"
)]
pub(crate) fn start_app_wire(
    options: &Options,
    live_activities: &std::sync::Arc<runtime_collab::LiveActivities>,
    handle: &DesktopHandle,
    activity: &activity::Activity,
    services: Knitted,
    collaboration: Collaboration<'_>,
) -> Result<Option<AppWire>, Error> {
    let Collaboration {
        revocations,
        sync,
        daemon,
        engine,
        view,
        sealed,
        fanout,
        records,
        task_programs,
        session_actions,
        settings_steps,
    } = collaboration;
    let Some(path) = app_wire_path(options)? else {
        return Ok(None);
    };
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
    // An app's action on another of the person's own devices is carried to the device's daemon
    // over the shell link it started this session on, which forwards it from there.
    let own_device = daemon.as_ref().ok().map(|link| {
        std::sync::Arc::new(crate::daemon_actions::AnotherDevice::over(link))
            as std::sync::Arc<dyn tilo_broker_core::services::OwnDeviceActions>
    });
    // An offloadable action is placed by the device's daemon over the same link: the daemon holds
    // the endpoint that reaches the workspace's other devices, and the session does not.
    let action_offload = daemon.as_ref().ok().map(|link| {
        std::sync::Arc::new(crate::daemon_actions::Offload::over(link))
            as std::sync::Arc<dyn tilo_actions::offload::ActionOffload>
    });
    // Files' clipboard lives beside the broker, its copies reaching the compositor's loop alike.
    let file_clipboard = std::sync::Arc::new(crate::file_clipboard::NativeFileClipboard::new(
        options.wake.handle(),
    ));
    // What Browser's asks for vault items are answered with: kept here as well, so a session
    // that holds a workspace's Browser can say so and name who asks its server.
    let item_lending = std::sync::Arc::new(crate::item_lending::ItemLending::default());
    // The model keys the person's Settings changes are this session's engine's, else the ones its
    // peer network holds.
    let engine = engine.or_else(|| sync.and_then(crate::sync::Client::engine).cloned());
    // The device's one dispatch as this session's person, over the shell's link: what the
    // session's own callers (its automations, its agent servers, the palette's actions) and the
    // person's other devices reach the workspaces through.
    let dispatch = daemon
        .as_ref()
        .ok()
        .map(|shell| std::sync::Arc::new(shell.dispatch()));
    // The Workspace package's own actions (`workspace/content`) run on that dispatch, where the
    // workspace's governance publishes them: an app's call on the package's roots goes there.
    if let Some(dispatch) = &dispatch {
        link.dispatching(std::sync::Arc::clone(dispatch)
            as std::sync::Arc<dyn crate::app_wire::WorkspaceActions>);
    }
    // What only this seat answers is served on that dispatch under the ids apps call it by.
    let seat = daemon.as_ref().ok().map(|shell| {
        crate::session_providers::serve_on(
            shell,
            crate::session_providers::SeatProviders {
                activity: std::sync::Arc::clone(&activity_wire),
                machines: crate::machine_steps::MachineSteps::session(view.clone()),
                models: crate::model_steps::ModelSteps::session(view.clone()),
                settings: settings_steps.clone(),
            },
        )
    });
    // A person session reaches the hosts of its workspaces through the daemon's dispatch.
    let hosts = daemon
        .as_ref()
        .ok()
        .zip(view.as_ref())
        .map(|(shell, view)| {
            std::sync::Arc::new(crate::broker_services::DaemonHosts {
                view: std::sync::Arc::clone(view),
                dispatch: shell.dispatch(),
            })
                as std::sync::Arc<dyn tilo_workspace_service::apps::services::HostedWorkspaces>
        });
    let fabric = acting_fabric(options, sync);
    let (hosted, socket, terminals) = match &daemon {
        // The device's daemon serves this session's apps: each connection is attested here and
        // handed to it, and what only this seat answers is served to it on the shell's link.
        Ok(shell) => {
            link.admitting(std::sync::Arc::clone(&options.launches));
            options.launches.watching(options.lists.clone());
            crate::seat_services::serve_on(
                shell.clone(),
                seat_parts(SeatInputs {
                    options,
                    live_activities,
                    link: &link,
                    layout: &layout,
                    calls_service: calls_service.as_ref(),
                    records,
                    view: view.as_ref(),
                    engine: engine.as_ref(),
                    shell,
                    file_clipboard: &file_clipboard,
                    item_lending: &item_lending,
                    agents: services
                        .agents
                        .clone()
                        .zip(services.agent_sessions.clone())
                        .map(|(roster, sessions)| crate::seat_services::SeatAgents {
                            roster,
                            sessions,
                        }),
                }),
            );
            // The session's own actions, as the daemon's endpoint and its agents ask them of the
            // layout, the windows and the notices.
            if let Some(actions) = &session_actions {
                actions.serve_on(shell.clone());
            }
            let socket = crate::app_socket::serve(
                &path,
                std::sync::Arc::clone(&options.launches),
                shell.clone(),
            )?;
            let terminals = view.clone().zip(engine.as_ref()).map(|(view, engine)| {
                crate::workspace_terminals::Terminals {
                    view,
                    dispatch: shell.dispatch(),
                    endpoint: sync.map(|sync| sync.endpoint_handle()),
                    device: engine.identity().device_cert.device_public_key,
                    runtime: tilo_rt::app().handle().clone(),
                }
            });
            (None, Some(socket), terminals)
        }
        // A session the host placed in a workspace's VM runs no daemon yet: its window's page is
        // served by the broker it keeps itself ([`app_wire::serve_hosted`]).
        Err(_) => {
            let (broker, terminals) = app_wire::serve_hosted(
                path.clone(),
                std::sync::Arc::clone(&link),
                std::sync::Arc::clone(&options.launches),
                fabric,
                app_wire::Hosted {
                    live_activities: std::sync::Arc::clone(live_activities),
                    native_media: calls_service.clone(),
                    records,
                    layout: std::sync::Arc::clone(&layout),
                    activity: activity_wire,
                    agents: services.agents.map(|roster| roster as _),
                    agent_sessions: services.agent_sessions.map(|sessions| sessions as _),
                    sealed,
                    own_device,
                    action_offload,
                    engine,
                    view,
                    endpoint: sync.map(|sync| sync.endpoint_handle()),
                    journal_root: match options.state.as_deref() {
                        Some(state) => {
                            std::fs::create_dir_all(state.join("tilo"))?;
                            Some(app_wire::journal_root(state))
                        }
                        None => None,
                    },
                    lists: options.lists.clone(),
                    task_programs,
                    session_actions,
                    settings_steps,
                    file_clipboard: std::sync::Arc::clone(&file_clipboard),
                    item_lending: std::sync::Arc::clone(&item_lending),
                    fanout: std::sync::Arc::clone(&fanout),
                },
            )?;
            (Some(broker), None, terminals)
        }
    };
    // The person's other devices are dialled as this session's siblings, and the session's pass
    // wakes when their own workspace moves: the session between two of theirs is where they tell
    // each other what they hold, and what their device fabric says.
    if let Some(sync) = sync {
        sync.follow_person(tilo_rt::app().handle(), options.wake.handle());
    }
    // No call outlives the daemon that knew it: a lane a session that ended left behind would
    // let the next launch's app send without one.
    launcher::egress_proxy::forget_calls(
        &launcher::dbus_proxy::session_runtime_dir().join("tilo/net"),
    );
    Ok(Some(AppWire {
        call_lane: std::sync::Mutex::new(None),
        dispatch,
        hosts,
        seat,
        link,
        path,
        layout,
        hosted,
        _socket: socket,
        calls,
        calls_service,
        carets: crate::carets::Carets::new(fanout, options.wake.handle()),
        file_clipboard,
        item_lending,
        terminals,
    }))
}

/// What [`seat_parts`] builds the seat's services from.
struct SeatInputs<'a> {
    options: &'a Options,
    live_activities: &'a std::sync::Arc<runtime_collab::LiveActivities>,
    link: &'a std::sync::Arc<app_wire::Link>,
    layout: &'a std::sync::Arc<LayoutAuthority>,
    calls_service: Option<&'a tilo_native_media::NativeMediaService>,
    records: Option<std::sync::Arc<crate::resource_index::RecordChanges>>,
    view: Option<&'a std::sync::Arc<crate::workspace_view::WorkspaceView>>,
    engine: Option<&'a std::sync::Arc<crate::engine::Engine>>,
    shell: &'a runtime_collab::ShellClient,
    file_clipboard: &'a std::sync::Arc<crate::file_clipboard::NativeFileClipboard>,
    item_lending: &'a std::sync::Arc<crate::item_lending::ItemLending>,
    agents: Option<crate::seat_services::SeatAgents>,
}

/// What only this seat answers for the apps the device's daemon serves: the screen, the sound,
/// the calls, the person's settings and keys, the windows this session opened, and the person's
/// account credentials in the vault this session opens.
fn seat_parts(inputs: SeatInputs<'_>) -> crate::seat_services::SeatParts {
    let SeatInputs {
        options,
        live_activities,
        link,
        layout,
        calls_service,
        records,
        view,
        engine,
        shell,
        file_clipboard,
        item_lending,
        agents,
    } = inputs;
    // This person's settings on this device: each app reaches its own entries and the shared
    // sections, and a display change is written here too.
    let settings = settings_store::source();
    let display_control = settings.as_ref().map(|source| {
        std::sync::Arc::new(crate::display_control::DisplayControl::native(
            std::sync::Arc::clone(source),
            layout,
        ))
            as std::sync::Arc<dyn tilo_workspace_service::apps::services::DisplayControlServices>
    });
    crate::seat_services::SeatParts {
        #[cfg(target_os = "linux")]
        audio_output: Some(std::sync::Arc::new(
            crate::audio_output::AppAudioOutput::new(),
        )),
        #[cfg(not(target_os = "linux"))]
        audio_output: None,
        native_media: calls_service.map(|media| {
            std::sync::Arc::new(media.clone())
                as std::sync::Arc<dyn tilo_workspace_service::apps::services::NativeMediaServices>
        }),
        continuations: options.launches.notes(),
        records,
        // Where this computer's networks wake it and what it saved: the passphrases are the
        // daemon's, which this session asks them of.
        wifi_continuity: crate::wifi_continuity::WifiContinuity::new(
            tilo_session_client::daemon_documents::DaemonShell::new(
                Ok(shell.clone()),
                tilo_rt::app().handle().clone(),
            ),
        )
        .map(|wifi| {
            std::sync::Arc::new(wifi)
                as std::sync::Arc<
                    dyn tilo_workspace_service::apps::services::WifiContinuityServices,
                >
        }),
        published: std::sync::Arc::clone(link) as _,
        workspace_nodes: crate::workspace_nodes::WorkspaceNodes::service(view),
        settings,
        display_control,
        live_activities: std::sync::Arc::clone(live_activities),
        launches: Some(std::sync::Arc::clone(&options.launches)),
        keyboard_shortcuts: crate::keyboard_shortcuts::servable(),
        service_sessions: engine.map(|engine| {
            std::sync::Arc::new(crate::service_sessions::ServiceSessions::new(
                std::sync::Arc::clone(engine),
                std::sync::Arc::clone(item_lending),
                tilo_session_client::daemon_documents::DaemonShell::new(
                    Ok(shell.clone()),
                    tilo_rt::app().handle().clone(),
                ),
            ))
                as std::sync::Arc<
                    dyn tilo_workspace_service::apps::services::BrowserServiceSessionServices,
                >
        }),
        file_clipboard: Some(std::sync::Arc::clone(file_clipboard)),
        agents,
    }
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
        options.asks.as_ref().map(|(asks, _)| asks.clone()),
        sync.own().user,
    );
    service.install_factory(std::sync::Arc::new(crate::calls::Factory::new(sources)));
    Some(service)
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

/// The provider the session's own services answer the device's dispatch under, over the shell
/// link: a press of one of their actions, and the person's answer to the question it asked.
pub(crate) const SERVICES_PROVIDER: &str = "session.services";

/// One call of the session's services the device's dispatch made: what to run, as whom, and
/// where its answer goes.
struct ServiceCall {
    request: tilo_actions::remote::InvokeRequest,
    reply: tokio::sync::oneshot::Sender<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    >,
}

/// Whether the device's dispatch serves the session's services ([`SERVICES_PROVIDER`]) yet. A
/// press of one of their actions made before it does waits for it, never dropped: the link to the
/// daemon is opened and the provider offered while the shell is already drawing, so a press can
/// reach the dispatch first.
#[derive(Clone, Debug)]
pub(crate) enum Registration {
    /// Offered no earlier than now: a press waits until it is.
    Pending,
    /// The dispatch serves them: a press goes through.
    Registered,
    /// They cannot be served this session: a press is told why rather than dropped.
    Failed(String),
}

/// The session's services as the device's dispatch calls them over the shell link. The dispatch
/// runs on the session's runtime, while the services live on its loop: a call is handed over
/// here, the loop answers it on its next pass, and the dispatch reads the answer.
pub(crate) struct ServedServices {
    calls: std::sync::Mutex<Vec<ServiceCall>>,
    wake: tilo_service::Wake,
    /// Who the person reads as asking when one of the services' actions waits for them.
    by: tilo_actions::remote::asked::AskedBy,
    /// Whether the device's dispatch serves these services, watched by a press that reached it
    /// before they were offered.
    registration: tokio::sync::watch::Sender<Registration>,
    /// What the dispatch refused of a press that was held for it, in its words, for the loop to
    /// show the person on its next pass.
    refused: std::sync::Mutex<Vec<String>>,
}

impl ServedServices {
    /// The services' calls answered on the loop, and who a question shows as asking.
    pub(crate) fn new(
        wake: tilo_service::Wake,
        by: tilo_actions::remote::asked::AskedBy,
    ) -> std::sync::Arc<Self> {
        let (registration, _) = tokio::sync::watch::channel(Registration::Pending);
        std::sync::Arc::new(Self {
            calls: std::sync::Mutex::default(),
            wake,
            by,
            registration,
            refused: std::sync::Mutex::default(),
        })
    }

    /// The device's dispatch serves these services from now on: a press waiting for them goes
    /// through.
    pub(crate) fn registered(&self) {
        // A press may wait before it subscribes: the value is kept for whoever asks next, even
        // with no watcher on it now.
        let _ = self.registration.send_replace(Registration::Registered);
    }

    /// The device's dispatch cannot serve these services this session, saying why: a press
    /// waiting for them is told instead of dropped.
    pub(crate) fn registration_failed(&self, reason: &str) {
        let _ = self
            .registration
            .send_replace(Registration::Failed(reason.to_owned()));
    }

    /// Wait until the device's dispatch serves these services, and say how that ended: what a
    /// press made before they were offered holds for.
    pub(crate) async fn await_registration(&self) -> Registration {
        let mut watched = self.registration.subscribe();
        loop {
            let state = watched.borrow_and_update().clone();
            match state {
                Registration::Pending => {
                    if watched.changed().await.is_err() {
                        return Registration::Failed(
                            "the session ended before its services reached the device's dispatch"
                                .to_owned(),
                        );
                    }
                }
                done => return done,
            }
        }
    }

    /// Send a press that waits for its person through `send` once the device's dispatch serves
    /// these services, and keep what it answered when it failed for the loop to show: a press
    /// made before they were offered waits, and a press the dispatch refuses is said, never
    /// dropped behind a log line.
    pub(crate) async fn deliver_press<F, Fut>(&self, send: F)
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<(), String>> + Send,
    {
        match self.await_registration().await {
            Registration::Registered => {
                if let Err(reason) = send().await {
                    self.refuse(&reason);
                }
            }
            Registration::Failed(reason) => self.refuse(&reason),
            Registration::Pending => {}
        }
    }

    /// Keep `reason`, what the dispatch refused of a held press, for the person to be told on the
    /// loop's next pass, and wake the loop to show it.
    fn refuse(&self, reason: &str) {
        self.refused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(reason.to_owned());
        (self.wake)();
    }

    /// What the dispatch refused of a press it was held for since the last pass, in its words: what
    /// the loop tells the person.
    pub(crate) fn take_refused(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .refused
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Answer every call the dispatch made since the last pass.
    pub(crate) fn serve(&self, services: &mut Services) {
        let calls = std::mem::take(
            &mut *self
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for call in calls {
            let _ = call.reply.send(self.answer(services, &call.request));
        }
    }

    /// Run `request` of the services, or say what the person is shown when it waits for them.
    fn answer(
        &self,
        services: &mut Services,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>
    {
        use tilo_actions::remote::{ActionCallError, InvocationReceipt, ReceiptStatus};
        let invocation = request.invocation;
        let address = &request.target.address;
        let arguments = match &request.argument {
            tilo_actions::remote::InvokeArgument::Json(arguments) => arguments.clone(),
            tilo_actions::remote::InvokeArgument::None => serde_json::Map::new(),
            tilo_actions::remote::InvokeArgument::Text(_) => {
                return Err(ActionCallError::InvalidRequest {
                    reason: "a service's action takes named arguments".to_owned(),
                });
            }
        };
        let resources = services.resources();
        let confirmed = request.approval().is_some();
        let started = if confirmed {
            services.invoke_confirmed(&address.resource, &address.action, &arguments)
        } else {
            services.invoke(&address.resource, &address.action, &arguments)
        };
        let status = match started {
            Some(Ok(Started::Done(output))) => ReceiptStatus::Completed { output },
            Some(Ok(Started::Accepted | Started::Later(_))) => ReceiptStatus::Accepted,
            // Its contract waits for the person: the dispatch keeps the call as their asked
            // record, showing what it asks and, where Undo cannot take it back, that it cannot.
            Some(Err(ApiError::ConfirmationRequired)) if !confirmed => {
                return Err(ActionCallError::ConfirmationRequired(Box::new(
                    self.facts(&resources, address),
                )));
            }
            Some(Err(error)) => {
                return Err(ActionCallError::Refused {
                    reason: error.to_string(),
                });
            }
            None => return Err(ActionCallError::UnknownAction),
        };
        Ok(InvocationReceipt { invocation, status })
    }

    /// What the person reads before answering a call of `address`: the action by its own label,
    /// the resource it acts on, and whether Undo can take it back.
    fn facts(
        &self,
        resources: &[tilo_actions::ResourceSpec],
        address: &ActionAddress,
    ) -> tilo_actions::remote::asked::AskedFacts {
        let mut facts = tilo_actions::remote::asked::AskedFacts::by(self.by.clone());
        if let Some((owner, spec)) = tilo_actions::api::locate(resources, address) {
            facts.verb = spec.label.clone();
            facts.on = Some(owner.label.trim().to_owned()).filter(|label| !label.is_empty());
            facts.destructive = !spec.contract.can_be_undone();
        }
        facts
    }
}

/// The services' calls, answered by the dispatch as the person: what the shell presses and the
/// person approves, nothing an app claims.
struct ServicesOnDispatch {
    served: std::sync::Arc<ServedServices>,
    page: tilo_actions::remote::CataloguePage,
    runtime: tokio::runtime::Handle,
}

impl tilo_actions::remote::ActionProvider for ServicesOnDispatch {
    fn serves(&self, provider: &str) -> bool {
        provider == SERVICES_PROVIDER
    }

    fn query(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _query: tilo_actions::remote::CatalogueQuery,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>,
    > {
        tilo_actions::remote::Pending::ready(Ok(self.page.clone()))
    }

    fn invoke(
        &self,
        _caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        use tilo_actions::remote::ActionCallError;
        let (done, answer) = tilo_actions::remote::pending();
        let (reply, answered) = tokio::sync::oneshot::channel();
        self.served
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(ServiceCall { request, reply });
        (self.served.wake)();
        self.runtime.spawn(async move {
            let served = answered.await.unwrap_or_else(|_| {
                Err(ActionCallError::Unavailable {
                    reason: "the session ended before it answered".to_owned(),
                })
            });
            done.complete(served);
        });
        answer
    }

    /// A call is answered in its receipt, so no invocation is kept to look at later.
    fn observe(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &tilo_actions::remote::Caller,
        _id: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownInvocation,
        ))
    }
}

/// Serve the session's services on the device's dispatch over the shell link `daemon`, so the
/// shell's press of one of their actions reaches it and a call that waits for the person is kept
/// as their asked record. Without a link nothing here can be asked, and the press runs as before.
pub(crate) fn serve_services(
    served: std::sync::Arc<ServedServices>,
    services: &Services,
    daemon: &Result<runtime_collab::ShellClient, String>,
) {
    let Ok(link) = daemon else {
        // The press path is not taken without a link either, so nothing waits for one.
        return;
    };
    let Ok(runtime) = tilo_rt::try_app() else {
        tracing::warn!("the session's services reach no device dispatch");
        served.registration_failed("this session has no runtime to reach the device's dispatch");
        return;
    };
    let placed = tilo_actions::api::survey_placed(&services.resources());
    let page = tilo_actions::remote::CataloguePage {
        revision: services.revision().to_string(),
        rows: placed
            .into_iter()
            .map(|placed| {
                tilo_actions::remote::CatalogueRow::for_placed_action(
                    SERVICES_PROVIDER,
                    SERVICES_PROVIDER,
                    placed,
                )
            })
            .collect(),
        exhausted: true,
        ..tilo_actions::remote::CataloguePage::default()
    };
    let provider = std::sync::Arc::new(ServicesOnDispatch {
        served: std::sync::Arc::clone(&served),
        page: page.clone(),
        runtime: runtime.handle().clone(),
    });
    let link = link.clone();
    runtime.spawn(async move {
        match link
            .provide(
                SERVICES_PROVIDER,
                &page,
                provider as std::sync::Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
            )
            .await
        {
            // A press made before now waited for this: it goes through.
            Ok(_) => served.registered(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the session's services reach no device dispatch"
                );
                served.registration_failed(&error.to_string());
            }
        }
    });
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    //! A press that reaches the device's dispatch before the session's services are offered waits
    //! for them and runs once they are; one the dispatch refuses is the person's to read.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Waker};

    use tilo_actions::remote::asked::AskedBy;

    use super::ServedServices;

    /// The services as the session makes them, waking a loop that does nothing.
    fn serving() -> Arc<ServedServices> {
        ServedServices::new(Arc::new(|| ()), AskedBy::default())
    }

    /// What a held press sends: one run counted, answered with nothing.
    fn counting(
        runs: &Arc<AtomicUsize>,
    ) -> impl FnOnce() -> std::future::Ready<Result<(), String>> {
        let runs = Arc::clone(runs);
        move || {
            runs.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(()))
        }
    }

    #[test]
    fn a_press_made_before_the_services_register_waits_and_runs_once() {
        let served = serving();
        let runs = Arc::new(AtomicUsize::new(0));
        let mut pressing = Box::pin(served.deliver_press(counting(&runs)));
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);

        assert!(
            pressing.as_mut().poll(&mut context).is_pending(),
            "the press waits for the services to be offered"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "nothing runs before they are"
        );

        served.registered();
        assert!(
            pressing.as_mut().poll(&mut context).is_ready(),
            "the press goes through once they are"
        );
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "the held press ran, and only once"
        );
        assert!(served.take_refused().is_empty());
    }

    #[test]
    fn a_press_the_dispatch_refuses_is_said_to_the_person() {
        let served = serving();
        served.registered();
        let refusal = "the dispatch did not take the call";
        let mut pressing = Box::pin(served.deliver_press(|| {
            let refusal = refusal.to_owned();
            async move { Err(refusal) }
        }));
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(pressing.as_mut().poll(&mut context).is_ready());

        assert_eq!(
            served.take_refused(),
            vec![refusal.to_owned()],
            "what the dispatch refused is kept for the person"
        );
        let notice = crate::notices::press_refused_notice(refusal);
        assert!(
            notice.body.contains(refusal),
            "the notice says what the dispatch did: {}",
            notice.body
        );
    }
}
