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
        nearby,
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
    let hosts = daemon.as_ref().ok().zip(view.as_ref()).map(|(shell, view)| {
        std::sync::Arc::new(crate::broker_services::DaemonHosts {
            view: std::sync::Arc::clone(view),
            dispatch: shell.dispatch(),
        }) as std::sync::Arc<dyn tilo_workspace_service::apps::services::HostedWorkspaces>
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
                    tools: services.tools,
                    fabric: fabric.clone(),
                    nearby,
                    view: view.as_ref(),
                    engine: engine.as_ref(),
                    shell,
                    file_clipboard: &file_clipboard,
                    item_lending: &item_lending,
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
                let runtime = tilo_rt::app().handle().clone();
                let dispatch = {
                    let _inside = runtime.enter();
                    shell.dispatch()
                };
                crate::workspace_terminals::Terminals {
                    view,
                    dispatch,
                    endpoint: sync.map(|sync| sync.endpoint_handle()),
                    device: engine.identity().device_cert.device_public_key,
                    runtime,
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
                    agents: services.agents,
                    agent_sessions: services.agent_sessions,
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
                    tools: services.tools,
                    task_programs,
                    session_actions,
                    settings_steps,
                    nearby,
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
    Ok(Some(AppWire {
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
    tools: Option<crate::tools::Handle>,
    fabric: Option<crate::platform::fabric::SharedLink>,
    nearby: Option<std::sync::Arc<crate::nearby_transfer::NearbyTransfer>>,
    view: Option<&'a std::sync::Arc<crate::workspace_view::WorkspaceView>>,
    engine: Option<&'a std::sync::Arc<crate::engine::Engine>>,
    shell: &'a runtime_collab::ShellClient,
    file_clipboard: &'a std::sync::Arc<crate::file_clipboard::NativeFileClipboard>,
    item_lending: &'a std::sync::Arc<crate::item_lending::ItemLending>,
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
        tools,
        fabric,
        nearby,
        view,
        engine,
        shell,
        file_clipboard,
        item_lending,
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
        audio_output: Some(std::sync::Arc::new(crate::audio_output::AppAudioOutput::new())),
        #[cfg(not(target_os = "linux"))]
        audio_output: None,
        native_media: calls_service.map(|media| {
            std::sync::Arc::new(media.clone())
                as std::sync::Arc<dyn tilo_workspace_service::apps::services::NativeMediaServices>
        }),
        continuations: options.launches.notes(),
        records,
        tools,
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
                as std::sync::Arc<dyn tilo_workspace_service::apps::services::WifiContinuityServices>
        }),
        fabric,
        nearby,
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

/// The session's services as the device's dispatch calls them over the shell link. The dispatch
/// runs on the session's runtime, while the services live on its loop: a call is handed over
/// here, the loop answers it on its next pass, and the dispatch reads the answer.
pub(crate) struct ServedServices {
    calls: std::sync::Mutex<Vec<ServiceCall>>,
    wake: tilo_service::Wake,
    /// Who the person reads as asking when one of the services' actions waits for them.
    by: tilo_actions::remote::asked::AskedBy,
}

impl ServedServices {
    /// The services' calls answered on the loop, and who a question shows as asking.
    pub(crate) fn new(
        wake: tilo_service::Wake,
        by: tilo_actions::remote::asked::AskedBy,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            calls: std::sync::Mutex::default(),
            wake,
            by,
        })
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
    ) -> Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError> {
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
        Ok(InvocationReceipt {
            invocation,
            status,
        })
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
        Result<
            tilo_actions::remote::CataloguePage,
            tilo_actions::remote::ActionCallError,
        >,
    > {
        tilo_actions::remote::Pending::ready(Ok(self.page.clone()))
    }

    fn invoke(
        &self,
        _caller: &tilo_actions::remote::Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<
            tilo_actions::remote::InvocationReceipt,
            tilo_actions::remote::ActionCallError,
        >,
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
        return;
    };
    let Ok(runtime) = tilo_rt::try_app() else {
        tracing::warn!("the session's services reach no device dispatch");
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
        served,
        page: page.clone(),
        runtime: runtime.handle().clone(),
    });
    let link = link.clone();
    runtime.spawn(async move {
        if let Err(error) = link
            .provide(
                SERVICES_PROVIDER,
                &page,
                provider as std::sync::Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
            )
            .await
        {
            tracing::warn!(
                %error,
                "the session's services reach no device dispatch"
            );
        }
    });
}
