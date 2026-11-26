#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

use tilo_actions::remote::ActionCallError;

// Server implementations

/// What one app's connection is: the app, workspace and person it was admitted as, and the
/// records its calls named. One per connection.
pub(super) struct AppConnection {
    pub(super) worker: Option<WorkerBinding>,
    /// The worker registration of a connection its launcher authenticated,
    /// minted at connect for the app the shell launched: each route of
    /// that app the connection names as its person is one of its worker
    /// routes ([`host_workers`]).
    pub(super) app_worker: Option<(uuid::Uuid, String)>,
    /// Who the connection's calls are made by, as its launch, its shell's vouch or the host
    /// that made it named it; a connection naming nobody makes no call about a record.
    pub(super) identity: Option<ConnectionIdentity>,
    pub(super) state: Arc<AppBrokerState>,
    pub(super) directories: Rc<LocalActionDirectoryRegistry>,
    /// Peer id assigned at accept time. Fanout uses it to skip the
    /// author when replaying its own frames back at itself.
    pub(super) peer_id: u64,
    pub(super) lease: Rc<ConnectionLease>,
    /// The routes this connection holds ([`route_holds`]).
    pub(super) holds: route_holds::ConnectionHolds,
}

/// The certificate a connection acts under in a workspace, answered for the workspace it names.
type CertificateForWorkspace = Rc<dyn Fn(&str) -> Result<tilo_user_identity::DeviceCert, String>>;

/// Who an app's connection acts as: the person whose device it runs on, in whichever workspace
/// a record it names is in, and the workspace agent its launch named, if any.
#[derive(Clone)]
pub(super) struct ConnectionIdentity {
    /// The certificate the connection acts under in a workspace.
    pub(super) certificate: CertificateForWorkspace,
    /// The workspace agent the launch named; `None` for the person.
    pub(super) member: Option<String>,
}

impl ConnectionIdentity {
    /// The device's own person, as `state`'s certificate authority answers for each workspace
    /// ([`crate::certificate_authority::CertificateAuthority::app_certificate`]).
    pub(super) fn device(state: &Arc<AppBrokerState>, member: Option<String>) -> Self {
        let state = Arc::clone(state);
        Self {
            certificate: Rc::new(move |workspace| state.app_certificate(workspace)),
            member,
        }
    }

    /// The holder of `certificate`, in every workspace: a host-mounted worker the host admitted
    /// under it.
    pub(super) fn certified(
        certificate: tilo_user_identity::DeviceCert,
        member: Option<String>,
    ) -> Self {
        Self {
            certificate: Rc::new(move |_| Ok(certificate.clone())),
            member,
        }
    }
}

pub(super) struct ConnectionLease {
    pub(super) local_media: Option<Arc<app_process::LocalAppConnection>>,
    pub(super) state: Arc<AppBrokerState>,
    pub(super) directories: Rc<LocalActionDirectoryRegistry>,
    pub(super) peer_id: u64,
}

pub(super) struct ConnectionExit {
    pub(super) state: Arc<AppBrokerState>,
    pub(super) directories: Rc<LocalActionDirectoryRegistry>,
    pub(super) peer_id: u64,
}

impl Drop for ConnectionExit {
    fn drop(&mut self) {
        let holds = self.state.connection_holds(self.peer_id);
        self.state.remove_connection(self.peer_id);
        for hold in holds {
            self.directories.remove(hold);
        }
    }
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        if let Some(local) = self.local_media.as_ref() {
            local.stop();
        }
        let holds = self.state.connection_holds(self.peer_id);
        self.state.remove_connection(self.peer_id);
        for hold in holds {
            self.directories.remove(hold);
        }
    }
}

/// The resource kind of the record `route` names: a workspace record's own
/// kind, which the route's id already derives from and every window naming
/// it holds whether or not it says so, else what the call `named`. A call
/// naming a workspace record as another kind is refused.
pub(super) fn route_resource_type(
    route: &CrdtRoute,
    named: Option<String>,
) -> Result<Option<String>, BrokerError> {
    let Some(record) = collab_types::workspace_document(&route.app_id, &route.document_id) else {
        return Ok(named);
    };
    match named {
        Some(kind) if kind != record.kind => Err(BrokerError::failed(format!(
            "the workspace record of kind {} was named as {kind}",
            record.kind
        ))),
        _ => Ok(Some(record.kind.to_owned())),
    }
}

/// Where the time went when a connection first named a record, logged each time
/// (`COLLAB-OPEN`): an app's cold start waits for its first call about its record, and its own
/// startup line only sees the round trip. Slower on the app's side than here, the call waited
/// for the broker's thread.
pub(super) struct OpenPhases {
    pub(super) started: std::time::Instant,
    pub(super) last: std::time::Instant,
    pub(super) spans: Vec<(&'static str, std::time::Duration)>,
}

impl OpenPhases {
    pub(super) fn start() -> Self {
        let now = tilo_clock::now();
        Self {
            started: now,
            last: now,
            spans: Vec::with_capacity(6),
        }
    }

    pub(super) fn mark(&mut self, phase: &'static str) {
        let now = tilo_clock::now();
        self.spans.push((phase, now.duration_since(self.last)));
        self.last = now;
    }

    pub(super) fn report(&self, channel: &str) {
        let split = self
            .spans
            .iter()
            .map(|(phase, took)| format!("{phase}={}us", took.as_micros()))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "COLLAB-OPEN record={channel} total={}us {split}",
            self.started.elapsed().as_micros()
        );
    }
}

/// The resource kind a call names for its record: none when empty, and
/// refused unless it is a record kind the toolkit accepts.
pub(super) fn joined_resource_type(kind: &str) -> Result<Option<String>, BrokerError> {
    if kind.is_empty() {
        return Ok(None);
    }
    if !collab_types::document::ResourceType::new(kind).is_valid() {
        return Err(BrokerError::failed(format!("invalid record kind `{kind}`")));
    }
    Ok(Some(kind.to_owned()))
}

/// What a connection holds on one record it named; every call about the record
/// inherits its (`channel_id`, `peer_id`, identity) context.
pub(super) struct JoinedChannel {
    pub(super) committed_read: Rc<RefCell<committed_document::Slot>>,
    pub(super) state: Arc<AppBrokerState>,
    pub(super) directories: Rc<LocalActionDirectoryRegistry>,
    pub(super) channel_id: String,
    /// The route hold this channel publishes and works as ([`route_holds`]).
    pub(super) peer_id: u64,
    /// The connection: its transport identity for document fanout,
    /// presence, media and transfers.
    pub(super) connection_id: u64,
    pub(super) verified_identity: AdmittedJoinIdentity,
    /// The resource kind the app named for this route's record, if any.
    pub(super) resource_type: Option<String>,
    /// Production handles share the root RPC connection lease. Direct unit
    /// fixtures use `None` and retain the legacy one-handle cleanup behavior.
    pub(super) lease: Option<Rc<ConnectionLease>>,
    /// Production handles share their route's hold, kept for its drop;
    /// fixtures use `None`.
    pub(super) _hold: Option<Rc<route_holds::RouteHold>>,
}

impl Drop for JoinedChannel {
    fn drop(&mut self) {
        if self.lease.is_none() {
            self.state.remove_connection(self.peer_id);
            self.directories.remove(self.peer_id);
        }
    }
}

impl JoinedChannel {
    /// The app a launcher attested this channel's connection as, when one did.
    pub(super) fn launched_app(&self) -> Option<&str> {
        self.lease
            .as_ref()
            .and_then(|lease| lease.local_media.as_ref())
            .map(|local| local.app_id.as_str())
    }

    /// What the channel's app reaches of its record by what its manifest declares, as the
    /// device's one reader decides it ([`app_reach`]). A connection no launcher attested holds
    /// its routes as its admission does.
    pub(super) fn reach(&self) -> Result<AppReach, String> {
        let Some(app) = self.launched_app() else {
            return Ok(AppReach::Changes);
        };
        let route = self.route();
        let own_workspace = self
            .verified_identity
            .verified
            .owns_person_workspace(&route.cluster_id);
        app_reach(app, &route_address(&route), own_workspace)
    }

    /// The caller this channel's calls about its record are decided as: its admitted identity
    /// in the record's workspace, through the app a launcher attested it as.
    pub(super) fn record_caller(
        &self,
        admitted: &AdmittedJoinIdentity,
        route: &CrdtRoute,
    ) -> tilo_actions::remote::Caller {
        let caller = admitted.certified_caller(&route.cluster_id, false);
        match self.launched_app() {
            Some(app) => caller.through_app(app),
            None => caller,
        }
    }

    /// Refuse a publication for a route the app does not change: no app writes on another app's
    /// route.
    pub(super) fn writable(&self) -> Result<(), BrokerError> {
        match self.reach() {
            Ok(AppReach::Changes) => Ok(()),
            Ok(AppReach::Reads) | Err(_) => Err(BrokerError::failed(format!(
                "this connection reads {}'s record and changes nothing on it; another app's \
                 resources change only through its actions",
                self.route().app_id
            ))),
        }
    }

    /// Refuse a change on a channel the device's one reader does not let this channel's caller
    /// make: the caller at the route's record, decided with the write action kind. An app changes
    /// its own records; another app's record only where the caller's grants allow a change, so a
    /// grant that reads alone answers a refusal.
    pub(super) fn may_write(
        &self,
        admitted: &AdmittedJoinIdentity,
        route: &CrdtRoute,
    ) -> impl std::future::Future<Output = Result<(), BrokerError>> + Send + use<> {
        let address = route_address(route);
        let caller = self.record_caller(admitted, route);
        let reads = self.state.records();
        let app_id = route.app_id.clone();
        async move {
            let decided = match reads {
                Some(reads) => Some(reads.may_write(&caller, &address).await),
                None => None,
            };
            match decided {
                Some(Ok(())) => Ok(()),
                // No reader answered the write decision: none is registered at this device, or the
                // reader cannot decide a change. The record cannot be changed from here now.
                None | Some(Err(ActionCallError::Unavailable { .. })) => Err(BrokerError::failed(
                    "this record can't be changed right now".into(),
                )),
                Some(Err(refusal)) => Err(BrokerError::failed(format!(
                    "this connection may not change the record of {app_id} and changes nothing on \
                     it: {refusal}"
                ))),
            }
        }
    }

    /// Hands the resource command `body` to the device's one store, for this route's app in its
    /// workspace.
    ///
    /// A change sent as a run of parts is gathered in the order its parts arrive, and decided
    /// once it is whole: the reader's decision is awaited, so it must not reorder the parts, and
    /// nothing of a run reaches the store before the run is decided.
    pub(super) fn resources_dispatch(
        self: &std::rc::Rc<Self>,
        body: &[u8],
        on_behalf_of: &[u8],
        record_runs: &super::record_changes::RecordRuns,
    ) -> Result<super::wire::Answer<collab_types::store::wire::Reply>, BrokerError> {
        if body.len() > collab_types::store::wire::MAX_WIRE_BYTES {
            return Err(BrokerError::failed(
                "a resource command exceeds the wire bound".into(),
            ));
        }
        let command: collab_types::store::wire::Command = serde_json::from_slice(body)
            .map_err(|error| BrokerError::failed(format!("no resource command: {error}")))?;
        let opened = self.route();
        let current = self
            .state
            .revalidate_admission(&opened, &self.verified_identity)?;
        let route = super::record_changes::addressed(&command, &opened);
        if command.writes() && (!on_behalf_of.is_empty() || current.agent_id.is_some()) {
            return Err(BrokerError::failed(
                "this resource route has no caller-scoped signer for the write".into(),
            ));
        }
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(BrokerError::failed(
                "resources are written and read on a certified app and workspace route".into(),
            ));
        }
        let Some(command) = super::record_changes::gathered(record_runs, command) else {
            return Ok(Box::pin(std::future::ready(Ok(
                collab_types::store::wire::Reply::Written,
            ))));
        };
        let service = self.state.services.resources.clone();
        let channel = std::rc::Rc::clone(self);
        Ok(Box::pin(async move {
            channel
                .store_command(service, command, current, route)
                .await
        }))
    }

    /// `command`, decided for this channel's caller (`current`) at `route`, answered by the
    /// device's one store `service`.
    fn store_command(
        &self,
        service: Option<std::sync::Arc<dyn crate::services::ResourceServices>>,
        command: collab_types::store::wire::Command,
        current: AdmittedJoinIdentity,
        route: CrdtRoute,
    ) -> impl std::future::Future<Output = Result<collab_types::store::wire::Reply, BrokerError>>
    + Send
    + use<> {
        let write = self.may_write(&current, &route);
        let record_of = self.record_of(&command, &current, &route);
        let delivered_to = self.state.delivered_to(self.peer_id);
        let opened_for = self.opened_for();
        let resource_type = self.resource_type.clone();
        let founding_refusal = self.founding_refusal(&route);
        let state = std::sync::Arc::clone(&self.state);
        let writer = self.connection_id;
        async move {
            // A change the caller may not make is refused for that, whether or not a store is here.
            if command.writes() {
                write.await?;
            }
            let service = service.ok_or_else(|| {
                BrokerError::failed("this device keeps no store for apps' resources".into())
            })?;
            let record = record_of.await?;
            let caller = current.certified_caller(&route.cluster_id, false);
            let mut context = crate::services::ResourceRequestContext::authenticated(
                route.app_id.clone(),
                route.cluster_id.clone(),
                caller,
            )
            .of_app_process()
            .serving(delivered_to)
            .opened_for(opened_for);
            if let Some(record) = record {
                context = context.on_record(record).of_kind(resource_type);
            }
            if matches!(
                command,
                collab_types::store::wire::Command::RecordChange { .. }
            ) {
                context = context.refusing_founding(founding_refusal);
            }
            let changes = command.writes() && context.record().is_some();
            let answer = service.command(&context, command);
            if changes {
                super::record_changes::telling_record_changed(state, writer, answer, route).await
            } else {
                answer.await
            }
            .map_err(BrokerError::failed)
        }
    }

    /// The caller the host opened this connection's view for, when it opened one for a caller
    /// of the device's dispatch ([`app_process::LocalAppConnection::opened_for`]).
    pub(super) fn opened_for(&self) -> Option<tilo_actions::remote::Caller> {
        self.lease
            .as_ref()
            .and_then(|lease| lease.local_media.as_ref())
            .and_then(|local| local.opened_for.clone())
    }

    /// What this host says of the `actions` the window shows, as the person this channel was
    /// admitted for would run them: now, and again on each change.
    pub(super) fn shown_actions(
        &self,
        actions: Vec<runtime_collab::shown_actions::ShownAction>,
    ) -> Result<super::wire::Feed<Vec<runtime_collab::shown_actions::ActionDecision>>, BrokerError>
    {
        let route = self.route();
        let current = self
            .state
            .revalidate_admission(&route, &self.verified_identity)?;
        Ok(self
            .state
            .subscribe_shown_actions(current.broker_actor(), route.cluster_id, actions))
    }
}

/// The `AppSettings` capability of the app `route` certifies: its own
/// entries and the shared sections, in the host's settings source, for as
/// long as the channel stays admitted.
pub(super) fn app_settings_capability(
    state: &Arc<AppBrokerState>,
    route: &CrdtRoute,
    identity: &AdmittedJoinIdentity,
) -> Result<AppSettings, BrokerError> {
    if route.app_id.is_empty() {
        return Err(BrokerError::failed(
            "settings belong to a certified app route".into(),
        ));
    }
    let app_id = route.app_id.clone();
    let grants = settings_grants(state, route, identity);
    let authority = display_authority(state, route, identity);
    let revalidate: runtime_collab::settings::host::Revalidate = {
        let (state, route, identity) = (Arc::clone(state), route.clone(), identity.clone());
        Arc::new(move || {
            state
                .revalidate_admission(&route, &identity)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    };
    // A host keeping the person's settings itself answers them from its source; the device's
    // daemon asks the seat, which keeps them in the person's own home.
    let Some(source) = state.services.settings.clone() else {
        let seat = state
            .seat()
            .ok_or_else(|| BrokerError::failed("this host keeps no settings for apps".into()))?;
        return Ok(AppSettings::Seat {
            settings: crate::seat::SeatSettings {
                seat,
                app_id,
                grants,
                providers: false,
                display: authority,
            },
            revalidate,
        });
    };
    let display = authority.and_then(|authority| {
        let service = state.services.display_control.clone()?;
        Some(Arc::new(move |command: &[u8], origin| {
            let context = authority(command, origin)?;
            service.command(&context, command)
        }) as runtime_collab::settings::host::DisplayControl)
    });
    let server = runtime_collab::settings::host::AppSettingsServer::new(app_id, source, revalidate)
        .with_grants(grants)
        .with_display(display);
    Ok(AppSettings::Local(server))
}

/// One app's settings, as the broker answers its calls on them.
pub(super) enum AppSettings {
    /// From the settings source this host keeps.
    Local(runtime_collab::settings::host::AppSettingsServer),
    /// From the person's settings the seat keeps.
    Seat {
        settings: crate::seat::SeatSettings,
        revalidate: runtime_collab::settings::host::Revalidate,
    },
}

impl AppSettings {
    /// The view once its revision differs from `after`; at once for 0.
    pub(super) async fn read(&self, after: u64) -> Result<(u64, Vec<u8>), String> {
        match self {
            Self::Local(local) => local.read(after).await,
            Self::Seat {
                settings,
                revalidate,
            } => {
                revalidate()?;
                let read = settings.read(after).await;
                revalidate()?;
                read
            }
        }
    }

    /// Merge `values` into the app's entry in `section`, answering the view after the write.
    pub(super) async fn update(
        &self,
        section: &str,
        values: &[u8],
        action: Option<&str>,
    ) -> Result<(u64, Vec<u8>), String> {
        match self {
            Self::Local(local) => local.update(section, values, action),
            Self::Seat {
                settings,
                revalidate,
            } => {
                revalidate()?;
                settings.update(section, values, action).await
            }
        }
    }

    /// Run a display transaction for the device's settings app.
    pub(super) async fn display(
        &self,
        command: &[u8],
        origin: runtime_collab::DisplayOrigin,
    ) -> Result<Vec<u8>, String> {
        match self {
            Self::Local(local) => local.display(command, origin),
            Self::Seat {
                settings,
                revalidate,
            } => {
                revalidate()?;
                settings.display(command, origin).await
            }
        }
    }
}

/// One app's live activity, as the broker answers its calls on it.
pub(super) enum AppLiveActivity {
    /// On the board this host presents.
    Local(runtime_collab::live_activity::LiveActivityServer),
    /// On the seat's board.
    Seat {
        activity: crate::seat::SeatLiveActivity,
        revalidate: runtime_collab::live_activity::Revalidate,
    },
}

impl AppLiveActivity {
    /// Show `activity` (a JSON object) as the app's activity; empty ends it.
    pub(super) fn show(&self, activity: &[u8]) -> Result<(), String> {
        match self {
            Self::Local(local) => local.show(activity),
            Self::Seat {
                activity: seat,
                revalidate,
            } => {
                revalidate()?;
                seat.show(activity)
            }
        }
    }

    /// The controls pressed after `after`, once there is one, and the serial of the last.
    pub(super) async fn controls(&self, after: u64) -> Result<(u64, Vec<u8>), String> {
        match self {
            Self::Local(local) => local.controls(after).await,
            Self::Seat {
                activity,
                revalidate,
            } => {
                revalidate()?;
                let controls = activity.controls(after).await;
                revalidate()?;
                controls
            }
        }
    }
}

/// The setters of the settings the app `route` certifies may invoke: every one for the device's
/// settings app, when its signed manifest asks for them (`settings.system`) and the image grants
/// it them (the launcher lists the capability for that package alone); none otherwise. An agent
/// working in an app never sets more than the app's own preferences.
pub(super) fn settings_grants(
    state: &Arc<AppBrokerState>,
    route: &CrdtRoute,
    identity: &AdmittedJoinIdentity,
) -> Vec<String> {
    let installed = |granted: &dyn Fn(&[String]) -> bool| {
        identity.agent_id.is_none()
            && state.services.launcher.as_ref().is_some_and(|launcher| {
                launcher.applications().into_iter().any(|app| {
                    app.package_id == route.app_id && !app.unconfined && granted(&app.capabilities)
                })
            })
    };
    let setters: &[&str] = if installed(&|capabilities| {
        capabilities
            .iter()
            .any(|capability| capability == settings_store::workspace::SET_EVERY_SETTING)
    }) {
        settings_store::workspace::EVERY_SETTER
    } else {
        &[]
    };
    setters.iter().map(|&grant| grant.to_owned()).collect()
}

/// Who runs a display transaction the private Settings worker of `route` asks for: its context,
/// decided again for each, or why it is refused. `None` for any other app.
pub(super) fn display_authority(
    state: &Arc<AppBrokerState>,
    route: &CrdtRoute,
    identity: &AdmittedJoinIdentity,
) -> Option<crate::seat::SeatDisplay> {
    if route.app_id != tilo_app_id::apps::SETTINGS || identity.agent_id.is_some() {
        return None;
    }
    let registration = identity.worker_registration.clone()?;
    if registration.route != *route
        || identity.certificate.as_ref() != Some(&registration.certificate)
    {
        return None;
    }
    let state = state.clone();
    let route = route.clone();
    let identity = identity.clone();
    Some(Arc::new(move |command, origin| {
        if !state.is_accepting() {
            return Err("The display host has closed.".into());
        }
        state
            .revalidate_admission(&route, &identity)
            .map_err(|error| error.to_string())?;
        if !state
            .host_workers
            .lock()
            .map_err(|_| "Settings worker registry is unavailable.")?
            .values()
            .any(|worker| worker.worker_registration.as_ref() == Some(&registration))
        {
            return Err("The admitted Settings worker has closed.".into());
        }
        if command.len() > 64 * 1024 {
            return Err("The display request exceeds its wire bound.".into());
        }
        let mutates = tilo_failure::ignored!(
            "a request that is no display command is held to what a change is held to, and the seat refuses it as invalid",
            serde_json::from_slice::<settings_store::display_control::DisplayCommand>(command).ok()
        )
        .is_none_or(|command| command.mutates());
        let certificate = match origin {
            runtime_collab::settings::DisplayOrigin::LocalHost => registration.certificate.clone(),
            runtime_collab::settings::DisplayOrigin::Published(bytes) => {
                let invocation = tilo_actions::InvocationId(uuid::Uuid::from_bytes(bytes));
                let (peer, original) = {
                    let workers = state
                        .host_workers
                        .lock()
                        .map_err(|_| "Settings worker registry is unavailable.")?;
                    let peer = workers
                        .peer_of(&registration)
                        .ok_or("The admitted Settings worker has closed.")?;
                    drop(workers);
                    let pending = state
                        .pending_invocations
                        .lock()
                        .map_err(|_| "Original display invocation registry is unavailable.")?;
                    let mut matching = pending.values().filter(|value| {
                        value.peer_id == peer && value.update.invocation() == invocation
                    });
                    let entry = matching
                        .next()
                        .ok_or("The original display invocation is not retained.")?;
                    if matching.next().is_some() || entry.cancellation_requested_at.is_some() {
                        return Err("The original display invocation is no longer live.".into());
                    }
                    let original = entry
                        .original
                        .clone()
                        .ok_or("Original display authority is unavailable.")?;
                    drop(pending);
                    (peer, original)
                };
                serde_json::from_slice::<settings_store::display_control::DisplayCommand>(command)
                    .map_err(|_| "The display command is invalid.")?;
                original.display_caller(&state, peer, &registration, mutates)?
            }
        };
        // A computer's displays change only from its own Settings: a Settings worker another
        // device runs, or one acting for another device's caller, reads them and changes nothing.
        if mutates
            && state
                .certificate_authority
                .get()
                .and_then(|authority| authority.device())
                != Some(certificate.device_public_key)
        {
            return Err("Only this computer's own Settings changes its displays.".into());
        }
        Ok(crate::services::DisplayControlContext {
            workspace: route.cluster_id.clone(),
            root: certificate.user_public_key,
            device: certificate.device_public_key,
        })
    }))
}

/// The `LiveActivity` capability of the app `route` certifies, over the
/// host's board, for as long as the channel stays admitted.
pub(super) fn live_activity_capability(
    state: &Arc<AppBrokerState>,
    route: CrdtRoute,
    identity: AdmittedJoinIdentity,
) -> Result<AppLiveActivity, BrokerError> {
    if route.app_id.is_empty() {
        return Err(BrokerError::failed(
            "a live activity belongs to a certified app route".into(),
        ));
    }
    let app_id = route.app_id.clone();
    let board = state.services.live_activities.clone();
    // The device's daemon presents no board itself: the seat's shell does.
    let seat = if board.is_none() { state.seat() } else { None };
    let state = Arc::clone(state);
    let revalidate: runtime_collab::live_activity::Revalidate = Arc::new(move || {
        state
            .revalidate_admission(&route, &identity)
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    match (board, seat) {
        (Some(board), _) => Ok(AppLiveActivity::Local(
            runtime_collab::live_activity::LiveActivityServer::new(app_id, board, revalidate),
        )),
        (None, Some(seat)) => Ok(AppLiveActivity::Seat {
            activity: crate::seat::SeatLiveActivity {
                seat,
                app_id,
                shown: std::sync::atomic::AtomicBool::new(false),
            },
            revalidate,
        }),
        (None, None) => Err(BrokerError::failed(
            "this host presents no live activities".into(),
        )),
    }
}

pub(super) async fn delegate_browser_service_session_command(
    services: &ProcessServices,
    context: &crate::services::BrowserServiceSessionRequestContext,
    bytes: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, BrokerError> {
    let refused = || BrokerError::failed("private service-session request refused".into());
    if context.workspace_id().is_empty() || context.person().is_empty() {
        return Err(refused());
    }
    context.revalidate().map_err(|_| refused())?;
    let command =
        runtime_collab::BrowserServiceSessionCommand::decode(bytes).map_err(|_| refused())?;
    // Website sessions are Browser's alone; an account secret is authorized
    // by the owner for the app asking.
    if command.browser_only() && context.app_id() != "so.tilo.browser" {
        return Err(refused());
    }
    let service = services
        .browser_service_sessions
        .as_ref()
        .ok_or_else(refused)?;
    service
        .authorize_command(context, &command)
        .map_err(|_| refused())?;
    let reply = Arc::clone(service)
        .command(context, command.clone())
        .await
        .map_err(|_| refused())?;
    context.revalidate().map_err(|_| refused())?;
    service
        .authorize_command(context, &command)
        .map_err(|_| refused())?;
    if !reply.matches(&command) {
        return Err(refused());
    }
    if let runtime_collab::BrowserServiceSessionReply::Resolved { session } = &reply
        && session.identity_public_key != context.identity_public_key()
    {
        return Err(refused());
    }
    reply.encode().map_err(|_| refused())
}

/// What the host is told each time an invocation is handed to `app` in
/// `workspace`; nothing when no launcher hosts it.
pub(super) fn invocation_delivery_notice(
    services: &ProcessServices,
    app: String,
    workspace: String,
) -> Option<Box<dyn Fn()>> {
    let launcher = services.launcher.clone().filter(|_| !app.is_empty())?;
    Some(Box::new(move || launcher.note_acted_on(&app, &workspace)))
}

pub(super) type InvocationQueue = Rc<AsyncMutex<mpsc::UnboundedReceiver<InvocationEnvelope>>>;

#[cfg(test)]
thread_local! {
    /// Each peer's latest invocation queue, kept for [`unread_invocations`].
    static INVOCATION_QUEUES: std::cell::RefCell<HashMap<u64, std::rc::Weak<AsyncMutex<mpsc::UnboundedReceiver<InvocationEnvelope>>>>> =
        std::cell::RefCell::default();
}

pub(super) fn invocation_queue(
    peer: u64,
    rx: mpsc::UnboundedReceiver<InvocationEnvelope>,
) -> InvocationQueue {
    let queue = Rc::new(AsyncMutex::new(rx));
    #[cfg(test)]
    INVOCATION_QUEUES.with(|queues| {
        queues.borrow_mut().insert(peer, Rc::downgrade(&queue));
    });
    #[cfg(not(test))]
    let _ = peer;
    queue
}

/// How many invocations were handed to `peer`'s current stream and not read
/// yet. A test asks between reads, on the thread that serves the broker:
/// whatever the broker dispatched by then is counted, however slowly the
/// stream would carry it.
#[cfg(test)]
#[track_caller]
pub(crate) fn unread_invocations(peer: u64) -> usize {
    let queue = INVOCATION_QUEUES
        .with(|queues| queues.borrow().get(&peer).and_then(std::rc::Weak::upgrade))
        .unwrap_or_else(|| panic!("peer {peer} holds no invocation stream on this thread"));
    let queue = queue
        .try_lock()
        .unwrap_or_else(|_| panic!("a read of peer {peer}'s invocations is outstanding"));
    queue.len()
}

pub(super) struct InvocationStreamServer {
    pub(super) rx: InvocationQueue,
    /// Told each time an invocation is handed to the app.
    pub(super) delivered: Option<Box<dyn Fn()>>,
}

impl InvocationStreamServer {
    /// The next invocation handed to the app.
    #[allow(
        clippy::future_not_send,
        reason = "the stream is served on the connection's single-threaded local set"
    )]
    pub(super) async fn next(&self) -> Result<InvocationEnvelope, BrokerError> {
        let mut guard = self.rx.lock().await;
        let invocation = guard.recv().await.ok_or_else(|| {
            BrokerError::disconnected("application invocation stream closed".into())
        })?;
        drop(guard);
        if let Some(delivered) = &self.delivered {
            delivered();
        }
        Ok(invocation)
    }
}

pub(super) struct NativeContextReadGuard {
    pub(super) state: Arc<AppBrokerState>,
    pub(super) route: CrdtRoute,
    pub(super) identity: AdmittedJoinIdentity,
}

impl NativeContextReadGuard {
    pub(super) fn check(guard: Option<&Self>) -> Result<(), BrokerError> {
        if let Some(guard) = guard {
            if !guard
                .state
                .services
                .workspace_memberships
                .as_ref()
                .is_some_and(|owner| owner.requires_workspace(&guard.route.cluster_id))
            {
                return Err(BrokerError::failed(
                    "native context owner is no longer current".into(),
                ));
            }
            guard
                .state
                .revalidate_admission(&guard.route, &guard.identity)?;
        }
        Ok(())
    }
}

// Listener wiring

#[allow(
    clippy::future_not_send,
    reason = "the broker state and its Rc-backed action-directory registry intentionally run only on the broker LocalSet"
)]
pub(super) async fn drive_connection(
    stream: tilo_socket::tokio::Stream,
    state: Arc<AppBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    peer_id: u64,
    worker: Option<WorkerBinding>,
    handed: Option<super::shell::Handed>,
) -> Result<(), BrokerError> {
    let _exit = ConnectionExit {
        state: Arc::clone(&state),
        directories: Rc::clone(&directories),
        peer_id,
    };
    let mut stream = stream;
    let local_media = if worker.is_none() {
        // A person's shell, where this host answers one, opens with its own marker, and so does a
        // connection it opens to hand the device's processes an instance; an app's connection the
        // shell handed over was admitted by the shell's vouch; any other is an app the launcher
        // started, or a client admitted by what it presents.
        let admitted = if let Some(handed) = handed {
            handed.admitted().map_err(|error| LaunchRefusal {
                error: BrokerError::failed(error),
                peer: "an app the shell vouched for".to_owned(),
                answerable: true,
            })
        } else {
            if let Some(shell) = state.services.shell.clone() {
                let marker = super::socket::peeked_marker(&mut stream).await?;
                if marker == *runtime_collab::wire::shell::SHELL_MAGIC {
                    let broker = super::shell::Broker {
                        state: Arc::clone(&state),
                        directories: Rc::clone(&directories),
                    };
                    return super::shell::serve(stream, shell, broker).await;
                }
                if marker == *runtime_collab::wire::shell::PROCESSES_MAGIC {
                    return super::shell::processes(stream, shell).await;
                }
            }
            authenticate_launcher_handoff(&mut stream, &state.services).await
        };
        match admitted {
            Ok(AuthenticatedHandoff {
                local_media,
                launch,
            }) => {
                if let Some(launch) = launch {
                    state.launch_connected(peer_id, &launch);
                }
                if let Some(instance) = local_media.as_ref().and_then(|l| l.continuation_instance)
                    && let Ok(mut windows) = state.window_instances.lock()
                {
                    windows.insert(peer_id, instance);
                    state.windows_changed();
                }
                state.note_windowless(peer_id, local_media.as_deref());
                local_media
            }
            Err(refusal) => {
                // Error, not warning: an app the shell launched is about to
                // exit for this, and the default session log keeps errors only.
                tracing::error!(
                    peer_id,
                    peer = %refusal.peer,
                    reason = %refusal.error,
                    "collaboration broker: refused a collaboration connection before its first call",
                );
                if refusal.answerable {
                    answer_refused_launch(stream, refusal.error).await;
                }
                return Ok(());
            }
        }
    } else {
        None
    };
    let _media_monitor = app_process::ProcessMonitor::start(local_media.clone(), &state, peer_id);
    let (handed_descriptors, app_worker, identity) =
        connection_parts(&state, worker.as_ref(), local_media.as_deref());
    let (connection, outgoing) = wire::WireConnection::new(Some(Rc::new(AppConnection {
        worker,
        app_worker,
        identity,
        lease: Rc::new(ConnectionLease {
            local_media,
            state: Arc::clone(&state),
            directories: Rc::clone(&directories),
            peer_id,
        }),
        state,
        directories,
        peer_id,
        holds: route_holds::ConnectionHolds::default(),
    })));
    // Files hands over the objects it selects on its own connection; the
    // broker keeps them for the request naming them. Every other peer's
    // descriptors are closed on receipt.
    let served = if let Some(received) = handed_descriptors {
        let (reader, writer) = tokio::io::split(
            runtime_portal::descriptors::DescriptorStream::receiving(stream, received),
        );
        wire::serve(connection, outgoing, reader, writer).await
    } else {
        let (reader, writer) = tokio::io::split(stream);
        wire::serve(connection, outgoing, reader, writer).await
    };
    served.map_err(|error| BrokerError::failed(error.to_string()))
}

/// What a connection opened with `local_media` hands over, and the identity it acts as: a
/// launched app as the device's own person, and as the agent its launch named; a host-mounted
/// worker as the certificate the host admitted it under; anything else as nobody.
fn connection_parts(
    state: &Arc<AppBrokerState>,
    worker: Option<&WorkerBinding>,
    local_media: Option<&app_process::LocalAppConnection>,
) -> (
    Option<runtime_portal::descriptors::ReceivedDescriptors>,
    Option<(uuid::Uuid, String)>,
    Option<ConnectionIdentity>,
) {
    let handed_descriptors = local_media.and_then(|local| local.descriptors.clone());
    let app_worker = local_media.map(|app| (uuid::Uuid::new_v4(), app.app_id.clone()));
    let identity = match (worker, local_media) {
        (Some(worker), _) => Some(ConnectionIdentity::certified(
            worker.certificate.clone(),
            None,
        )),
        (None, Some(app)) => Some(ConnectionIdentity::device(state, app.member.clone())),
        (None, None) => None,
    };
    (handed_descriptors, app_worker, identity)
}

/// The record `route` names, as the device's one reader addresses it.
pub(super) fn route_address(route: &CrdtRoute) -> tilo_actions::remote::ResourceAddress {
    tilo_actions::remote::ResourceAddress {
        workspace: route.cluster_id.clone(),
        app: route.app_id.clone(),
        record: route.document_id.clone(),
    }
}
