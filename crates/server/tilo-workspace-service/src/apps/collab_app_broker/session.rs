#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

/// Postcard-encode [`PresenceKey::pointer_default`]; cheap enough not
/// to cache.
pub(super) fn default_pointer_key_bytes() -> Vec<u8> {
    postcard::to_stdvec(&PresenceKey::pointer_default()).unwrap_or_default()
}

// Server implementations

/// Root capability server. One instance per connected RPC session.
pub(super) struct AppConnection {
    pub(super) worker: Option<WorkerBinding>,
    /// The worker registration of a connection its launcher authenticated,
    /// minted at connect for the app the shell launched: each route of
    /// that app the connection joins as its person is one of its worker
    /// routes ([`host_workers`]).
    pub(super) app_worker: Option<(uuid::Uuid, String)>,
    pub(super) state: Arc<AppBrokerState>,
    pub(super) directories: Rc<LocalActionDirectoryRegistry>,
    /// Peer id assigned at accept time. Fanout uses it to skip the
    /// author when replaying its own frames back at itself.
    pub(super) peer_id: u64,
    pub(super) lease: Rc<ConnectionLease>,
    /// The routes this connection holds ([`route_holds`]).
    pub(super) holds: route_holds::ConnectionHolds,
}

pub(super) struct ConnectionLease {
    pub(super) local_media: Option<Arc<native_media::LocalAppConnection>>,
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
/// kind, which the route's id already derives from and every window joining
/// it holds whether or not it says so, else what the join `named`. A join
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
            "the workspace record of kind {} was joined as {kind}",
            record.kind
        ))),
        _ => Ok(Some(record.kind.to_owned())),
    }
}

/// Where one join's time went, logged with every join (`COLLAB-JOIN`): an app's
/// cold start waits for its route's join, and its own startup line only sees the
/// round trip. The time before `verify` is the request's decoding; a join slower on
/// the app's side than here waited for the broker's thread.
pub(super) struct JoinPhases {
    pub(super) started: std::time::Instant,
    pub(super) last: std::time::Instant,
    pub(super) spans: Vec<(&'static str, std::time::Duration)>,
}

impl JoinPhases {
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
            "COLLAB-JOIN channel={channel} total={}us {split}",
            self.started.elapsed().as_micros()
        );
    }
}

/// The resource kind a join names for its record: none when empty, and
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

/// Per-channel handle established by `join_channel`; every subsequent
/// method call inherits its (`channel_id`, `peer_id`, identity) context.
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
    /// Whether the app may change what this channel carries, or only read
    /// it (another app's record it declared it reads).
    pub(super) route_access: RouteAccess,
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
    /// Refuse a change on a channel the app holds only to read: no app
    /// writes on another app's route.
    pub(super) fn writable(&self) -> Result<(), BrokerError> {
        match self.route_access {
            RouteAccess::Own => Ok(()),
            RouteAccess::ReadOnly => Err(BrokerError::failed(format!(
                "this connection reads {}'s record and changes nothing on it; another app's \
                 resources change only through its actions",
                self.route().app_id
            ))),
        }
    }

    /// Hands the resource command `body` to the device's one store, for this route's app in its
    /// workspace.
    pub(super) fn resources_dispatch(
        &self,
        body: &[u8],
        on_behalf_of: &[u8],
    ) -> Result<crate::apps::services::ResourceAnswer, BrokerError> {
        if body.len() > tilo_resource_store::wire::MAX_WIRE_BYTES {
            return Err(BrokerError::failed(
                "a resource command exceeds the wire bound".into(),
            ));
        }
        let command: tilo_resource_store::wire::Command = serde_json::from_slice(body)
            .map_err(|error| BrokerError::failed(format!("no resource command: {error}")))?;
        let route = self.route();
        let current = self
            .state
            .revalidate_admission(&route, &self.verified_identity)?;
        if command.writes() {
            self.writable()?;
            if !on_behalf_of.is_empty() || current.agent_id.is_some() {
                return Err(BrokerError::failed(
                    "this resource route has no caller-scoped signer for the write".into(),
                ));
            }
        }
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(BrokerError::failed(
                "resources are written and read on a certified app and workspace route".into(),
            ));
        }
        let service = self.state.services.resources.clone().ok_or_else(|| {
            BrokerError::failed("this device keeps no store for apps' resources".into())
        })?;
        let caller = current.certified_caller(&route.cluster_id, false);
        let context = crate::apps::services::ResourceRequestContext::authenticated(
            route.app_id,
            route.cluster_id,
            caller,
        )
        .serving(self.state.delivered_to(self.peer_id));
        Ok(service.command(&context, command))
    }

    /// Opens the query `body` in the device's one store, for this route's app in its workspace:
    /// its pages, as the app may read them.
    pub(super) fn resources_open(
        &self,
        body: &[u8],
    ) -> Result<crate::apps::services::ResourcePages, BrokerError> {
        if body.len() > tilo_resource_store::wire::MAX_WIRE_BYTES {
            return Err(BrokerError::failed(
                "a resource query exceeds the wire bound".into(),
            ));
        }
        let query: tilo_resource_store::Query = serde_json::from_slice(body)
            .map_err(|error| BrokerError::failed(format!("no resource query: {error}")))?;
        let route = self.route();
        let current = self
            .state
            .revalidate_admission(&route, &self.verified_identity)?;
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(BrokerError::failed(
                "resources are read on a certified app and workspace route".into(),
            ));
        }
        let service = self.state.services.resources.clone().ok_or_else(|| {
            BrokerError::failed("this device keeps no store for apps' resources".into())
        })?;
        let caller = current.certified_caller(&route.cluster_id, false);
        let context = crate::apps::services::ResourceRequestContext::authenticated(
            route.app_id,
            route.cluster_id,
            caller,
        );
        Ok(service.open(&context, query))
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

    pub(super) fn realtime_context(
        &self,
    ) -> Result<crate::apps::services::RealtimeRequestContext, BrokerError> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let route = self.route();
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(BrokerError::failed(
                "realtime continuity requires an exact certified app/workspace route".into(),
            ));
        }
        Ok(
            crate::apps::services::RealtimeRequestContext::authenticated(
                self.connection_id,
                route.app_id,
                route.cluster_id,
            ),
        )
    }
}

/// The `AppSettings` capability of the app `route` certifies: its own
/// entries and the shared sections, in the host's settings source, for as
/// long as the channel stays admitted.
pub(super) fn app_settings_capability(
    state: &Arc<AppBrokerState>,
    route: CrdtRoute,
    identity: AdmittedJoinIdentity,
) -> Result<runtime_collab::settings::host::AppSettingsServer, BrokerError> {
    if route.app_id.is_empty() {
        return Err(BrokerError::failed(
            "settings belong to a certified app route".into(),
        ));
    }
    let Some(source) = state.services.settings.clone() else {
        return Err(BrokerError::failed(
            "this host keeps no settings for apps".into(),
        ));
    };
    let app_id = route.app_id.clone();
    let display = private_display_control(state, &route, &identity);
    let reach = settings_reach(state, &route, &identity);
    let state = Arc::clone(state);
    let revalidate: runtime_collab::settings::host::Revalidate = Arc::new(move || {
        state
            .revalidate_admission(&route, &identity)
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    Ok(
        runtime_collab::settings::host::AppSettingsServer::new(app_id, source, revalidate)
            .with_reach(reach)
            .with_display(display),
    )
}

/// What the app `route` certifies reaches of the settings: every section
/// for the device's settings app, when its signed manifest asks
/// (`settings_store::app_scope::SYSTEM_SETTINGS_CAPABILITY`) and the image
/// grants it ([`settings_store::app_scope::SYSTEM_SETTINGS_APPS`]); its own
/// entries otherwise. An agent working in an app never gets more than the
/// app's own entries.
pub(super) fn settings_reach(
    state: &Arc<AppBrokerState>,
    route: &CrdtRoute,
    identity: &AdmittedJoinIdentity,
) -> settings_store::app_scope::Reach {
    use settings_store::app_scope::Reach;
    if identity.agent_id.is_some() {
        return Reach::App;
    }
    state
        .services
        .launcher
        .as_ref()
        .and_then(|launcher| {
            launcher
                .applications()
                .into_iter()
                .find(|app| app.package_id == route.app_id && !app.unconfined)
        })
        .map_or(Reach::App, |app| {
            Reach::of(&route.app_id, &app.capabilities)
        })
}

pub(super) fn private_display_control(
    state: &Arc<AppBrokerState>,
    route: &CrdtRoute,
    identity: &AdmittedJoinIdentity,
) -> Option<runtime_collab::settings::host::DisplayControl> {
    if route.app_id != tilo_app_id::apps::SETTINGS || identity.agent_id.is_some() {
        return None;
    }
    let registration = identity.worker_registration.clone()?;
    if registration.route != *route
        || identity.certificate.as_ref() != Some(&registration.certificate)
    {
        return None;
    }
    let service = state.services.display_control.clone()?;
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
                original.display_caller(
                    &state,
                    peer,
                    &registration,
                    serde_json::from_slice::<settings_store::display_control::DisplayCommand>(
                        command,
                    )
                    .map_err(|_| "The display command is invalid.")?
                    .mutates(),
                )?
            }
        };
        service.command(
            &crate::apps::services::DisplayControlContext {
                workspace: route.cluster_id.clone(),
                root: certificate.user_public_key,
                device: certificate.device_public_key,
            },
            command,
        )
    }))
}

/// The `LiveActivity` capability of the app `route` certifies, over the
/// host's board, for as long as the channel stays admitted.
pub(super) fn live_activity_capability(
    state: &Arc<AppBrokerState>,
    route: CrdtRoute,
    identity: AdmittedJoinIdentity,
) -> Result<runtime_collab::live_activity::LiveActivityServer, BrokerError> {
    if route.app_id.is_empty() {
        return Err(BrokerError::failed(
            "a live activity belongs to a certified app route".into(),
        ));
    }
    let Some(board) = state.services.live_activities.clone() else {
        return Err(BrokerError::failed(
            "this host presents no live activities".into(),
        ));
    };
    let app_id = route.app_id.clone();
    let state = Arc::clone(state);
    let revalidate: runtime_collab::live_activity::Revalidate = Arc::new(move || {
        state
            .revalidate_admission(&route, &identity)
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    Ok(runtime_collab::live_activity::LiveActivityServer::new(
        app_id, board, revalidate,
    ))
}

pub(super) fn delegate_realtime_command(
    services: &ProcessServices,
    context: &crate::apps::services::RealtimeRequestContext,
    claimed_app: &str,
    command: &[u8],
) -> Result<Vec<u8>, BrokerError> {
    if claimed_app != context.app_id() {
        return Err(BrokerError::failed(
            "realtime app id must match the certified connection route".into(),
        ));
    }
    let service = services.realtime.as_ref().ok_or_else(|| {
        BrokerError::failed("realtime continuity is unsupported by this host".into())
    })?;
    service
        .command(context, command)
        .map_err(BrokerError::failed)
}

/// The only application that may send files to nearby devices.
use tilo_app_id::apps::FILE_EXPLORER as NEARBY_TRANSFER_APP_ID;

/// The only application that may carry Wi-Fi credentials between devices.
use tilo_app_id::apps::SETTINGS as WIFI_CONTINUITY_APP_ID;

pub(super) fn delegate_wifi_continuity_command(
    services: &ProcessServices,
    route_app: &str,
    claimed_app: &str,
    command: &[u8],
) -> Result<Vec<u8>, BrokerError> {
    if route_app != WIFI_CONTINUITY_APP_ID {
        return Err(BrokerError::failed(
            "Wi-Fi continuity is restricted to the certified Settings application".into(),
        ));
    }
    if claimed_app != route_app {
        return Err(BrokerError::failed(
            "Wi-Fi continuity app id must match the certified connection route".into(),
        ));
    }
    if command.len() > runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES {
        return Err(BrokerError::failed(
            "Wi-Fi continuity command exceeds the local protocol bound".into(),
        ));
    }
    let command = match serde_json::from_slice::<runtime_collab::WifiContinuityCommand>(command) {
        Ok(command) if command.valid() => command,
        Ok(_) => {
            return Err(BrokerError::failed(
                "Wi-Fi continuity command is structurally invalid".into(),
            ));
        }
        Err(error) => return Err(BrokerError::failed(error.to_string())),
    };
    let service = services.wifi_continuity.as_ref().ok_or_else(|| {
        BrokerError::failed("Wi-Fi continuity is unsupported by this host".into())
    })?;
    match serde_json::to_vec(&service.command(command)) {
        Ok(reply) if reply.len() <= runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES => Ok(reply),
        Ok(_) => Err(BrokerError::failed(
            "Wi-Fi continuity reply exceeds the local protocol bound".into(),
        )),
        Err(error) => Err(BrokerError::failed(error.to_string())),
    }
}

pub(super) fn delegate_browser_service_session_command(
    services: &ProcessServices,
    context: &crate::apps::services::BrowserServiceSessionRequestContext,
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
    let reply = service
        .command(context, command.clone())
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

/// The wait for Nearby Send's next change past `after` for the connection
/// `context`: only the certified Files route in a workspace may wait, as only
/// it may command.
pub(super) fn nearby_transfer_change(
    services: &ProcessServices,
    context: &crate::apps::services::NearbyTransferRequestContext,
    after: u64,
) -> Result<crate::apps::services::NearbyTransferChange, BrokerError> {
    if context.app_id() != NEARBY_TRANSFER_APP_ID || context.workspace_id().is_empty() {
        return Err(BrokerError::failed(
            "Nearby Send requires the certified Files application route".into(),
        ));
    }
    services
        .nearby_transfer
        .as_ref()
        .and_then(|service| service.changed(context, after))
        .ok_or_else(|| {
            BrokerError::unimplemented("this host does not say when Nearby Send changes".into())
        })
}

/// The only application that manages the person's speaker groups.
use tilo_app_id::apps::SETTINGS as SPEAKER_APP_ID;

/// The wait for the speaker groups' next change past `after` for a
/// connection on `route_app`: only the certified Settings route may wait, as
/// only it may command.
pub(super) fn speaker_change(
    services: &ProcessServices,
    route_app: &str,
    after: u64,
) -> Result<crate::apps::services::SpeakerChange, BrokerError> {
    if route_app != SPEAKER_APP_ID {
        return Err(BrokerError::failed(
            "speaker groups are managed only by the certified Settings application".into(),
        ));
    }
    services
        .speakers
        .as_ref()
        .and_then(|service| service.changed(after))
        .ok_or_else(|| {
            BrokerError::unimplemented("this host does not say when speaker groups change".into())
        })
}

pub(super) fn delegate_nearby_transfer_command(
    services: &ProcessServices,
    context: &crate::apps::services::NearbyTransferRequestContext,
    claimed_app: &str,
    command: &[u8],
) -> Result<Vec<u8>, BrokerError> {
    if context.app_id() != NEARBY_TRANSFER_APP_ID || context.workspace_id().is_empty() {
        return Err(BrokerError::failed(
            "Nearby Send requires the certified Files application route".into(),
        ));
    }
    if claimed_app != context.app_id() {
        return Err(BrokerError::failed(
            "Nearby Send app id must match the certified connection route".into(),
        ));
    }
    if command.len() > runtime_collab::MAX_NEARBY_TRANSFER_WIRE_BYTES {
        return Err(BrokerError::failed(
            "Nearby Send command exceeds the local protocol bound".into(),
        ));
    }
    let command = match serde_json::from_slice::<runtime_collab::NearbyTransferCommand>(command) {
        Ok(command) if command.valid() => command,
        Ok(_) => {
            return Err(BrokerError::failed(
                "Nearby Send command is structurally invalid".into(),
            ));
        }
        Err(error) => return Err(BrokerError::failed(error.to_string())),
    };
    let service = services
        .nearby_transfer
        .as_ref()
        .ok_or_else(|| BrokerError::failed("Nearby Send is unsupported by this host".into()))?;
    let reply = service
        .command(context, command)
        .map_err(BrokerError::failed)?;
    if !reply.valid() {
        return Err(BrokerError::failed(
            "Nearby Send produced an invalid bounded reply".into(),
        ));
    }
    match serde_json::to_vec(&reply) {
        Ok(bytes) if bytes.len() <= runtime_collab::MAX_NEARBY_TRANSFER_WIRE_BYTES => Ok(bytes),
        Ok(_) => Err(BrokerError::failed(
            "Nearby Send reply exceeds the local protocol bound".into(),
        )),
        Err(error) => Err(BrokerError::failed(error.to_string())),
    }
}

impl JoinedChannel {
    /// Open a workspace control session with `target`, for an editor of
    /// this channel's workspace, checked again at every step.
    pub(super) fn workspace_control(
        &self,
        target: [u8; 32],
    ) -> Result<Rc<dyn crate::apps::services::WorkspaceControlSession>, BrokerError> {
        let route = self.route();
        revalidate_workspace_control(&self.state, &route, &self.verified_identity)?;
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id) else {
            return Err(BrokerError::failed(
                "workspace control requires a workspace-scoped channel".into(),
            ));
        };
        let Some(service) = self.state.services.workspace_control.as_ref() else {
            return Err(BrokerError::failed("workspace control unsupported".into()));
        };
        let state = self.state.clone();
        let identity = self.verified_identity.clone();
        let revalidate: crate::apps::services::ControlRevalidate = Arc::new(move || {
            revalidate_workspace_control(&state, &route, &identity)
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        service
            .open(workspace, target, revalidate)
            .map_err(BrokerError::failed)
    }
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
    /// Native private views retain their original document's current Read ownership.
    pub(super) document: Option<NativeDocumentReadContext>,
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
            if let Some(context) = &guard.document {
                context.revalidate().map_err(BrokerError::failed)?;
                guard
                    .state
                    .services
                    .committed_documents
                    .as_ref()
                    .ok_or_else(|| {
                        BrokerError::failed("native private document owner ended".into())
                    })?
                    .authorize(context)
                    .map_err(BrokerError::failed)?;
                context.revalidate().map_err(BrokerError::failed)?;
            }
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
    stream: tokio::net::UnixStream,
    state: Arc<AppBrokerState>,
    directories: Rc<LocalActionDirectoryRegistry>,
    peer_id: u64,
    worker: Option<WorkerBinding>,
) -> Result<(), BrokerError> {
    let _exit = ConnectionExit {
        state: Arc::clone(&state),
        directories: Rc::clone(&directories),
        peer_id,
    };
    let mut stream = stream;
    // A person's shell, where this broker answers one, opens with its own marker.
    if worker.is_none()
        && let Some(shell) = state.services.shell.clone()
        && super::socket::has_shell_marker(&stream).await?
    {
        return super::shell::serve(stream, shell).await;
    }
    let local_media = if worker.is_none() {
        match authenticate_launcher_handoff(&mut stream, &state.services).await {
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
    let _media_monitor = native_media::ProcessMonitor::start(local_media.clone(), &state, peer_id);
    let handed_descriptors = local_media
        .as_ref()
        .and_then(|local| local.descriptors.clone());
    let app_worker = local_media
        .as_ref()
        .map(|app| (uuid::Uuid::new_v4(), app.app_id.clone()));
    let (connection, outgoing) = wire::WireConnection::new(Some(Rc::new(AppConnection {
        worker,
        app_worker,
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
        let (reader, writer) = stream.into_split();
        wire::serve(connection, outgoing, reader, writer).await
    };
    served.map_err(|error| BrokerError::failed(error.to_string()))
}
