//! Framework apps the shell mounts in its own process.
//!
//! A tab of an app the shell hosts itself (the terminal) is the app's own
//! component: its definition, on a record of its own in the workspace's
//! room, with the dispatcher, history and published actions the app has
//! anywhere else. So the palette and agents reach the tab through the same
//! door as the app's own window, and what the tab keeps (the terminal's
//! last target, a Notes tab's notebook) is in its record. What only the shell can give the app (a
//! connection to the session's process supervisor) it lends
//! ([`HostContext::lend`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::launch::LaunchError;
use ui::permissions::{PermissionContext, Role, SharedPermissions, UserId};
use ui::record::{RecordCheckout, RecordId, RecordOpenError, RecordOwners};

mod feedback_native;
#[cfg(target_os = "linux")]
mod feedback_review;
#[cfg(test)]
mod feedback_tests;
#[cfg(target_os = "linux")]
mod messaging;

/// The apps the shell mounts in-process, with the store their records and
/// views live in, as one person on one device.
pub struct InProcessApps {
    feedback: Option<tilo_desktop::feedback::PrivateFeedback>,
    native_feedback: Option<Box<dyn feedback_native::Producer>>,
    feedback_destination: Option<tilo_desktop::feedback::Destination>,
    /// The window the person last asked to give feedback about.
    feedback_subject: Option<tilo_desktop::feedback::Subject>,
    store: Rc<dyn durable_store::DurableStore>,
    owners: RecordOwners,
    /// Where the apps' records beside their own are kept: apart from the
    /// records the apps show, so an app edits one while its own is being
    /// changed.
    person_owners: RecordOwners,
    views: HashMap<String, Rc<InstanceViewStore>>,
    device_id: String,
    author: UserId,
    /// How an app mounted here reaches the session's broker, as an app the
    /// session launched; `None` keeps them off any broker.
    broker: Option<SessionBroker>,
    /// The person's messaging accounts, in their own service.
    #[cfg(target_os = "linux")]
    messaging: Option<Box<dyn messaging::Service>>,
    /// Where the messaging service keeps the accounts' private state.
    #[cfg(target_os = "linux")]
    messaging_profile: Option<std::path::PathBuf>,
    /// The person's records the session keeps (their accounts and contacts
    /// among them), held on this thread where its settings are: the apps
    /// open the person's records of other apps there.
    person_records: crate::shell::settings_records::Holder,
    session_wake: Option<crate::session_runtime::Wake>,
    native_broker_socket: Option<std::path::PathBuf>,
    /// The window each mounted tab is shown in, as lent to its app: the
    /// slots it declares for its bar and its asks to close its tab.
    windows: HashMap<u64, ui::hosted_window::HostedWindow>,
    /// The records the session's windows hold, by the window instance the
    /// launcher started each for: what a report about a window records.
    window_routes: Option<WindowRoutes>,
    /// The apps mounted here that declare their windows withheld from what
    /// leaves the device (`ComponentBehaviour::capture_protected`).
    capture_protected: DeclaredProtected,
}

/// Apps that declare their windows capture-protected, by id.
pub(crate) type DeclaredProtected = Rc<RefCell<std::collections::HashSet<String>>>;

/// The records the session's broker knows its windows to hold, by window
/// instance.
pub type WindowRoutes =
    std::sync::Arc<dyn Fn() -> Vec<(u64, tilo_action_broker::route::CrdtRoute)> + Send + Sync>;

/// Connects an app the session shows in its own process to the session's
/// broker for `(app, room)`.
pub type SessionBroker = Rc<dyn Fn(&str, &str) -> ui::collaboration::HostSessionConnector>;

impl InProcessApps {
    /// The records every app of `room` opens (the workspace's emoji), in
    /// the store the apps mounted here share, changed as `author`: what a
    /// host that is no app window, such as workspace settings, opens them
    /// from, so it and the room's windows hold the one record.
    pub(crate) fn workspace_records(
        &self,
        room: &str,
        author: &str,
    ) -> ui::record::workspace::WorkspaceRecords {
        ui::record::workspace::WorkspaceRecords::in_room(
            self.owners.clone(),
            room.to_owned(),
            UserId(author.to_owned()),
        )
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn bind_messaging_network_prompts(&self, prompts: crate::portal::PromptQueueHandle) {
        if let Some(service) = &self.messaging {
            service.bind_prompts(prompts);
        }
    }

    /// Apps keeping their records in `store`, mounted for `author` on
    /// `device_id`, opening the person's records of other apps in
    /// `person_records`.
    pub(crate) fn new(
        store: Rc<dyn durable_store::DurableStore>,
        device_id: &str,
        author: &str,
        person_records: crate::shell::settings_records::Holder,
    ) -> Self {
        Self {
            feedback: None,
            native_feedback: None,
            feedback_destination: None,
            feedback_subject: None,
            owners: RecordOwners::with_store(Rc::clone(&store)),
            person_owners: RecordOwners::with_store(Rc::clone(&store)),
            store,
            views: HashMap::new(),
            device_id: device_id.to_owned(),
            author: UserId(author.to_owned()),
            broker: None,
            #[cfg(target_os = "linux")]
            messaging: None,
            #[cfg(target_os = "linux")]
            messaging_profile: None,
            person_records,
            session_wake: None,
            native_broker_socket: None,
            windows: HashMap::new(),
            window_routes: None,
            capture_protected: DeclaredProtected::default(),
        }
    }

    /// The window tab `tab` is shown in, to lend its app: the same one for
    /// every mount of the tab, until the tab is gone.
    #[cfg(test)]
    pub(crate) fn hosted_window(&mut self, tab: u64) -> ui::hosted_window::HostedWindow {
        let window = self.windows.entry(tab).or_default().clone();
        // An app asking from its own thread (a shell that exited) wakes the
        // loop, which looks at its window.
        if let Some(wake) = self.session_wake.clone() {
            window.set_wake(move || wake());
        }
        window
    }

    /// Every mounted tab's window, by tab.
    pub(crate) fn hosted_windows(&self) -> Vec<(u64, ui::hosted_window::HostedWindow)> {
        self.windows
            .iter()
            .map(|(tab, window)| (*tab, window.clone()))
            .collect()
    }

    /// Tab `tab` is gone: its window goes with it, and a tab that comes
    /// back gets a fresh one.
    pub(crate) fn forget_hosted_window(&mut self, tab: u64) {
        self.windows.remove(&tab);
    }

    /// Bind the session loop's wake once, and start the messaging service
    /// with it.
    pub(crate) fn bind_session_wake(&mut self, wake: crate::session_runtime::Wake) {
        if self.session_wake.is_some() {
            return;
        }
        #[cfg(target_os = "linux")]
        if let (Some(profile), Some(socket)) = (&self.messaging_profile, &self.native_broker_socket)
        {
            self.messaging = messaging::start(
                &self.author.0,
                &self.device_id,
                profile,
                socket.clone(),
                wake.clone(),
            );
        }
        self.session_wake = Some(wake);
    }

    /// Carry the messaging service's private Tilo texts on `network`.
    pub(crate) fn bind_private_messages_network(
        &mut self,
        network: &crate::shell::network::ShellNetwork,
    ) {
        #[cfg(target_os = "linux")]
        if let Some(service) = self.messaging.as_mut() {
            service.bind_network(network);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = network;
    }

    /// Answer what the messaging service asks of the session's loop.
    pub(crate) fn pump_messaging_records(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(service) = self.messaging.as_mut() {
            service.pump(&self.person_records);
        }
    }

    /// Reach the session's broker through `broker` for every app mounted
    /// from now on.
    #[must_use]
    pub fn reaching_broker(mut self, broker: SessionBroker) -> Self {
        self.reach_broker(broker);
        self
    }

    /// [`Self::reaching_broker`] on apps already shared with a host.
    pub fn reach_broker(&mut self, broker: SessionBroker) {
        self.broker = Some(broker);
    }

    /// A collaboration manager for `component`, mounted in `room`, on the
    /// session's broker: what the App showing it publishes and is invoked
    /// through, as a launched app is.
    pub fn collaboration(
        &self,
        component: &AppComponent,
        room: &str,
    ) -> Option<ui::collaboration::CollaborationManager> {
        if component.app_id() == tilo_desktop::feedback::PACKAGE {
            return None;
        }
        let broker = self.broker.as_ref()?;
        component.collaborate_through(broker(component.app_id(), room))
    }

    /// Block until the store has committed every write the apps' records
    /// handed it, the debounced ones included, as its own thread would
    /// commit them a moment later.
    #[cfg(test)]
    pub(crate) fn await_accepted_writes(&self) -> Result<(), String> {
        self.store.flush(None).map_err(|error| error.to_string())
    }

    /// Acknowledge startup writes through the fixture's real canonical owner.
    #[cfg(test)]
    pub(crate) fn flush_fixture_records(
        &mut self,
        app_id: &str,
        room: &str,
    ) -> Result<usize, String> {
        let records = self
            .owners
            .open(app_id, room)
            .map_err(|error| error.to_string())?;
        let count = records.open_records().len();
        if count == 0 {
            return Err("fixture must observe mounted records before flushing".into());
        }
        records.try_flush()?;
        Ok(count)
    }

    /// The apps of a session whose state lives in `profile_root`: their
    /// records and views there, as this device, with the person's records
    /// in `person` held on this thread.
    ///
    /// # Errors
    /// A relative root, corrupt device identity or unreadable person's
    /// records.
    pub(crate) fn at(
        profile_root: &std::path::Path,
        person: &std::path::Path,
        author: &str,
    ) -> Result<Self, LaunchError> {
        std::fs::create_dir_all(profile_root)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        let profile = ui::application::NativeProfile::open(profile_root, None)?;
        let held = crate::shell::settings_records::Holder::held(
            (person.to_path_buf(), author.to_owned()),
            profile_root,
        )
        .map_err(LaunchError::Operation)?;
        Ok(Self::new(
            Rc::new(app_state::FsStore::new(profile_root.to_path_buf())),
            profile.device_id(),
            author,
            held,
        ))
    }

    /// The session's apps: kept under the shell's own state, beside the
    /// records of its scratch tabs.
    ///
    /// # Errors
    /// As `Self::at`.
    pub fn native(author: &str) -> Result<Self, LaunchError> {
        let person = crate::shell::settings_records::person_dir()
            .ok_or_else(|| LaunchError::Operation("no data home".into()))?;
        Self::native_for_profile(
            &crate::shell::settings_records::session_apps(),
            &person,
            author,
            launcher::services::collab_socket(),
        )
    }

    /// The same native startup, with an explicit private profile, directory
    /// of the person's records and broker scope for owned native hosts and
    /// isolated integration fixtures.
    pub(crate) fn native_for_profile(
        profile: &std::path::Path,
        person: &std::path::Path,
        author: &str,
        socket: std::path::PathBuf,
    ) -> Result<Self, LaunchError> {
        let connector = socket.clone();
        let mut apps =
            Self::at(profile, person, author)?.reaching_broker(Rc::new(move |app, room| {
                session_connector(
                    connector.clone(),
                    launcher::services::identity_socket(),
                    app,
                    room,
                )
            }));
        apps.native_broker_socket = Some(socket);
        apps.feedback_destination = feedback_native::configured_destination(profile)?;
        #[cfg(target_os = "linux")]
        {
            apps.messaging_profile = Some(profile.to_path_buf());
        }
        Ok(apps)
    }

    /// The store `app`'s records in `room` live in, as its mounted tabs
    /// share it.
    ///
    /// # Errors
    /// The store cannot be opened.
    pub fn records(
        &mut self,
        app: &str,
        room: &str,
    ) -> Result<ui::record::RecordStore, RecordOpenError> {
        self.owners.open(app, room)
    }

    /// Mount `definition` for tab `tab` of `room` on `record` (the tab's
    /// own, or the room's default the app shares between its windows),
    /// created by the app's genesis the first time, reopened every time
    /// after. `lend` hands the context what the shell lends the app.
    ///
    /// # Errors
    /// The record cannot be opened or created, or the app refuses the
    /// context.
    pub fn mount(
        &mut self,
        definition: &'static ApplicationDefinition,
        room: &str,
        tab: u64,
        record: RecordId,
        lend: impl FnOnce(&mut HostContext),
    ) -> Result<AppComponent, LaunchError> {
        let component = self.mount_component(definition, room, tab, record, lend)?;
        if component.capture_protected() {
            self.capture_protected
                .borrow_mut()
                .insert(definition.app_id().to_owned());
        }
        Ok(component)
    }

    fn mount_component(
        &mut self,
        definition: &'static ApplicationDefinition,
        room: &str,
        tab: u64,
        record: RecordId,
        lend: impl FnOnce(&mut HostContext),
    ) -> Result<AppComponent, LaunchError> {
        super::work_accounting::app_mounted();
        if definition.app_id() == tilo_desktop::feedback::PACKAGE {
            return match &self.feedback {
                Some(feedback) => {
                    feedback
                        .about(self.feedback_subject.clone())
                        .map_err(|error| LaunchError::Operation(error.to_string()))?;
                    feedback.prepare(&self.device_id, &self.author.0, tab)
                }
                None => tilo_desktop::feedback::unconfigured(&self.device_id, &self.author.0, tab),
            };
        }
        let records = self
            .owners
            .open(definition.app_id(), room)
            .map_err(LaunchError::Record)?;
        let views = if let Some(views) = self.views.get(room) {
            Rc::clone(views)
        } else {
            let views = Rc::new(
                InstanceViewStore::with_store(Rc::clone(&self.store), &self.device_id, room)
                    .map_err(LaunchError::Restore)?,
            );
            self.views.insert(room.to_owned(), Rc::clone(&views));
            views
        };
        let inputs = || HostContextInputs {
            records: records.clone(),
            views: Rc::clone(&views),
            device_id: self.device_id.clone(),
            instance_id: tab,
            permissions: SharedPermissions::new(PermissionContext::member(
                &self.author.0,
                Role::Owner,
            )),
            author: self.author.clone(),
            action_context: tilo_actions::ActionContext::default(),
        };
        let checkout = RecordCheckout::new(record, definition.record_kind.clone());
        let (mut context, target) = match records.try_open_record(&checkout) {
            Ok(_) => {
                let target = LaunchTarget::Record {
                    checkout,
                    reveal: None,
                };
                (HostContext::prepare(definition, inputs(), &target)?, target)
            }
            Err(RecordOpenError::MissingRecord) => (
                HostContext::prepare_new_record(definition, inputs(), checkout.id)?,
                LaunchTarget::New,
            ),
            Err(error) => return Err(LaunchError::Record(error)),
        };
        // The person's records of other apps (their accounts and contacts)
        // are the ones the session holds.
        let held = self
            .person_records
            .person_records()
            .map_err(LaunchError::Operation)?;
        if let Ok(person) = ui::record::person::PersonRecords::open(
            &mut self.person_owners,
            definition.app_id(),
            self.author.clone(),
        ) {
            context.provide_person_records(person.sharing(&held));
        }
        // The records every app of the room opens (the workspace's emoji),
        // in the owners every in-process app shares: one writer per room.
        context.provide_workspace_records(ui::record::workspace::WorkspaceRecords::in_room(
            self.owners.clone(),
            room.to_owned(),
            self.author.clone(),
        ));
        lend(&mut context);
        definition.instantiate(context, target, RestoreInput::None)
    }

    /// The apps mounted here that declare their windows capture-protected.
    pub(crate) fn declared_capture_protected(&self) -> DeclaredProtected {
        Rc::clone(&self.capture_protected)
    }
}

impl InProcessApps {
    /// Mount `definition` as a sheet the shell shows over the desktop
    /// ([`mount_sheet`]), as this person on this device, with the
    /// collaboration route that publishes its actions in `room`.
    ///
    /// # Errors
    /// The app refuses the context.
    pub(crate) fn mount_sheet(
        &self,
        definition: &'static ApplicationDefinition,
        room: &str,
        instance: u64,
        lend: impl FnOnce(&mut HostContext),
    ) -> Result<
        (
            AppComponent,
            Option<ui::collaboration::CollaborationManager>,
        ),
        LaunchError,
    > {
        let component = mount_sheet(
            definition,
            &self.author.0,
            &self.device_id,
            room,
            instance,
            lend,
        )?;
        let collaboration = self.collaboration(&component, room);
        Ok((component, collaboration))
    }
}

/// Mount `definition` as a sheet the shell shows over the desktop, as
/// `author` on `device`. A sheet keeps nothing worth a record on disk: its
/// record and its view live in memory, so it is there the moment the shell
/// asks for it and never waits on the store. `lend` hands it what the shell
/// lends it.
///
/// # Errors
/// The app refuses the context.
pub(crate) fn mount_sheet(
    definition: &'static ApplicationDefinition,
    author: &str,
    device: &str,
    room: &str,
    instance: u64,
    lend: impl FnOnce(&mut HostContext),
) -> Result<AppComponent, LaunchError> {
    let store: Rc<dyn durable_store::DurableStore> = Rc::new(durable_store::MemoryStore::new());
    let records = RecordOwners::with_store(Rc::clone(&store))
        .open(definition.app_id(), room)
        .map_err(LaunchError::Record)?;
    let views = InstanceViewStore::with_store(store, device, room)
        .map_err(LaunchError::Restore)?
        .without_persistence();
    let inputs = HostContextInputs {
        records,
        views: Rc::new(views),
        device_id: device.to_owned(),
        instance_id: instance,
        permissions: SharedPermissions::new(PermissionContext::member(author, Role::Owner)),
        author: UserId(author.to_owned()),
        action_context: tilo_actions::ActionContext::default(),
    };
    let mut context =
        HostContext::prepare_new_record(definition, inputs, RecordId::room_default())?;
    lend(&mut context);
    definition.instantiate(context, LaunchTarget::New, RestoreInput::None)
}

impl InProcessApps {
    /// Consume enrollment-owned authenticated feedback startup custody.
    /// A different profile person or replacement of existing custody is refused.
    /// # Errors
    /// Refuses mismatched identity, admission, storage or existing custody.
    pub fn install_feedback_startup(
        &mut self,
        authenticated_reporter: &tilo_actions::remote::Caller,
        startup: tilo_desktop::feedback::transport::Startup,
    ) -> Result<(), LaunchError> {
        if startup.reporter() != authenticated_reporter
            || startup.reporter().principal()
                != &tilo_actions::remote::CallerPrincipal::Person(self.author.0.clone())
        {
            return Err(LaunchError::BindingMismatch);
        }
        startup
            .install(Rc::clone(&self.store), &mut self.feedback)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        self.lend_review_model();
        Ok(())
    }

    /// Lend the report composer the model the person's settings choose for
    /// it, to look over reports before they are sent.
    fn lend_review_model(&self) {
        #[cfg(target_os = "linux")]
        if let (Some(feedback), Some((models, name))) =
            (&self.feedback, feedback_review::review_model())
        {
            feedback.services().set_models(models, name);
        }
    }

    /// Bind a trusted host's already-admitted feedback adapter to this profile.
    /// This is host configuration, never an action or a URL-supplied destination.
    ///
    /// # Errors
    /// Refuses replacement of existing custody or an invalid/recovering outbox.
    pub fn install_feedback(
        &mut self,
        destination: tilo_desktop::feedback::Destination,
        host: Box<dyn tilo_desktop::feedback::FeedbackHost>,
    ) -> Result<(), LaunchError> {
        if self.feedback.is_some() {
            return Err(LaunchError::BindingMismatch);
        }
        self.feedback = Some(
            tilo_desktop::feedback::PrivateFeedback::new(Rc::clone(&self.store), destination, host)
                .map_err(|error| LaunchError::Operation(error.to_string()))?,
        );
        self.lend_review_model();
        Ok(())
    }

    /// Learn the records the session's windows hold from its broker.
    pub(crate) fn bind_window_routes(&mut self, routes: WindowRoutes) {
        self.window_routes = Some(routes);
    }

    /// The record window `instance` of `app` in `room` shows: the one record
    /// it holds there besides the app's own record of the workspace, or that
    /// one alone. `None` when it holds none, or several.
    pub(crate) fn window_record(&self, room: &str, app: &str, instance: u64) -> Option<String> {
        let routes = self.window_routes.as_ref()?();
        let mut held: Vec<String> = routes
            .into_iter()
            .filter(|(at, route)| {
                *at == instance && route.app_id == app && route.cluster_id == room
            })
            .map(|(_, route)| route.record_id)
            .collect();
        held.sort();
        held.dedup();
        match held.as_slice() {
            [only] => Some(only.clone()),
            [primary, item] if primary.is_empty() => Some(item.clone()),
            _ => None,
        }
    }

    /// Feedback opened from now on, and an open composer, is about `subject`.
    pub(crate) fn feedback_about(&mut self, subject: Option<tilo_desktop::feedback::Subject>) {
        if let Some(feedback) = &self.feedback
            && let Err(error) = feedback.about(subject.clone())
        {
            tracing::debug!(%error, "feedback subject not applied to the open composer");
        }
        self.feedback_subject = subject;
    }

    /// Whether reports have somewhere to go: "Report a problem…" is offered
    /// only then.
    pub(crate) fn feedback_configured(&self) -> bool {
        self.feedback.is_some()
    }

    /// Start a report about what the person picked; the composer shows it
    /// when it opens.
    pub(crate) fn begin_feedback_report(
        &self,
        picked: tilo_app::feedback::PickedReport,
    ) -> Result<(), String> {
        let feedback = self
            .feedback
            .as_ref()
            .ok_or("feedback has no destination here")?;
        feedback
            .services()
            .begin_report(picked)
            .map_err(|error| error.to_string())
    }

    /// Storage/transport wakes service the approved outbox, including while
    /// its private window is closed. Failures remain in the private status UI.
    pub(crate) fn pump_feedback(&mut self) {
        if let Some(mut delivery) = self.native_feedback.take() {
            let result = delivery.pump(self);
            if let Err(error) = result {
                tracing::debug!(%error, "native feedback remains unavailable");
            }
            self.native_feedback = Some(delivery);
        }
        if let Some(feedback) = &self.feedback {
            let _ = feedback.pump();
        }
    }

    pub(crate) fn feedback_deadline(&self) -> Option<ui::time::Instant> {
        let private = self
            .feedback
            .as_ref()
            .and_then(tilo_desktop::feedback::PrivateFeedback::next_wake_deadline);
        let native = self
            .native_feedback
            .as_ref()
            .and_then(|_| ui::time::now().checked_add(ui::time::Duration::from_secs(2)));
        private.into_iter().chain(native).min()
    }

    /// Bind configured feedback to this session's already enrolled network.
    /// Missing configuration leaves the private composer unconfigured.
    pub(crate) fn bind_feedback_network(
        &mut self,
        network: std::sync::Arc<crate::shell::network::ShellNetwork>,
    ) {
        if self.native_feedback.is_some() || self.feedback.is_some() {
            return;
        }
        let Some(destination) = self.feedback_destination.clone() else {
            return;
        };
        match feedback_native::Delivery::new(network, destination, &self.author.0) {
            Ok(delivery) => self.native_feedback = Some(Box::new(delivery)),
            Err(error) => tracing::debug!(%error, "native feedback startup refused"),
        }
    }
}

/// Whether this process's session services — the collaboration broker and
/// the identity service its in-process apps connect to — are listening yet.
///
/// The native session builds its desktop, and so mounts every hosted tab it
/// restores (a Notes notebook, a Terminal), before it binds those services:
/// the broker needs the desktop's chrome to start. A restored tab's one
/// connection attempt then found no socket and the app stayed offline for
/// the whole session: nothing it published reached the palette, search or an
/// agent until the tab was reopened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionServices {
    /// No session in this process binds them (a test, a hosted host): a
    /// missing socket is final.
    NotHere,
    /// The session will bind them shortly: a connection waits for them,
    /// however long their start takes.
    Starting,
    /// Both are bound, or their start was attempted: a connection that
    /// still fails is final.
    Listening,
}

static SESSION_SERVICES: std::sync::OnceLock<tokio::sync::watch::Sender<SessionServices>> =
    std::sync::OnceLock::new();

fn session_services() -> &'static tokio::sync::watch::Sender<SessionServices> {
    SESSION_SERVICES.get_or_init(|| tokio::sync::watch::channel(SessionServices::NotHere).0)
}

/// Say where this process's session services are; [`session_connector`]s
/// waiting for them wake on the change. A session that says `Starting` must
/// say `Listening` once its start is over, however it went.
pub fn set_session_services(state: SessionServices) {
    session_services().send_replace(state);
}

/// Once `services` says the session's start of its services is over: at
/// once where no session starts them here or it already has, otherwise when
/// it does, woken by the change. Unbounded on purpose: the session always
/// ends `Starting`, whether its broker came up or not.
async fn session_services_settled(services: &tokio::sync::watch::Sender<SessionServices>) {
    let mut changes = services.subscribe();
    // The sender is a process static, so the wait ends only on the change.
    let _ = changes
        .wait_for(|state| *state != SessionServices::Starting)
        .await;
}

/// Block until this process's session services are settled
/// ([`session_services_settled`]), for a worker thread that needs them.
pub fn wait_for_session_services() {
    pollster::block_on(session_services_settled(session_services()));
}

/// A connection for `app` in `room` to the broker at `collab` and the
/// identity service at `identity`, made as the launcher makes an app's: the
/// session records the launch, the connection names the app and room, and
/// the broker attests it against that record before the app joins. A tab
/// the session restores before it binds those services waits for them
/// ([`SessionServices`]).
pub fn session_connector(
    collab: std::path::PathBuf,
    identity: std::path::PathBuf,
    app: &str,
    room: &str,
) -> ui::collaboration::HostSessionConnector {
    let (app, room) = (app.to_owned(), room.to_owned());
    // The collaboration socket carries Files' selected descriptors to the
    // broker, as a launched app's inherited connection does.
    ui::collaboration::HostSessionConnector::with_descriptor_lane(move |_route| async move {
        let failed = |error: std::io::Error| runtime_portal::NotAvailable::ConnectionFailed(error);
        let mut stream = std::os::unix::net::UnixStream::connect(&collab).map_err(failed)?;
        crate::shell::launcher::register_in_process_launch(&app, &room);
        if let Err(error) = launcher::services::write_collab_handoff(&mut stream, &app, &room) {
            crate::shell::launcher::withdraw_in_process_launch(&app, &room, None);
            return Err(failed(error));
        }
        stream.set_nonblocking(true).map_err(failed)?;
        let collaboration = tokio::net::UnixStream::from_std(stream).map_err(failed)?;
        let mut identity = std::os::unix::net::UnixStream::connect(&identity).map_err(failed)?;
        launcher::services::write_identity_handshake(&mut identity, &app, &room).map_err(failed)?;
        identity.set_nonblocking(true).map_err(failed)?;
        let identity = tokio::net::UnixStream::from_std(identity).map_err(failed)?;
        Ok((collaboration, identity))
    })
    .once_ready(|| session_services_settled(session_services()))
}

/// The in-process apps a host shares between the tiles it resolves.
pub type SharedInProcessApps = Rc<RefCell<InProcessApps>>;

#[cfg(test)]
mod tests {
    use super::*;
    include!("in_process_files_clipboard_tests.rs");
    include!("in_process_publication_tests.rs");

    fn session_socket_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// A tab the session restores before it binds its services connects
    /// once they listen, woken by the change, however long their start
    /// takes.
    #[test]
    fn a_hosted_tab_restored_before_the_session_binds_its_services_connects_once_they_listen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("collab.sock");
        let (services, _) = tokio::sync::watch::channel(SessionServices::Starting);
        let runtime = session_socket_runtime();
        let settled = session_services_settled(&services);
        let mut settled = std::pin::pin!(settled);
        assert!(
            futures_util::FutureExt::now_or_never(settled.as_mut()).is_none(),
            "a connection waits while the session starts its services"
        );
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        services.send_replace(SessionServices::Listening);
        runtime.block_on(async {
            tokio::time::timeout(tilo_test_env::hang_bound(), settled)
                .await
                .expect("the change wakes the wait");
        });
        std::os::unix::net::UnixStream::connect(&path).expect("connects once they listen");
        drop(listener);
    }

    /// Where no session starts the services here, or once it has, nothing
    /// waits: a missing socket then fails the connection at once.
    #[test]
    fn nothing_waits_unless_the_session_is_starting_its_services() {
        for state in [SessionServices::NotHere, SessionServices::Listening] {
            let (services, _) = tokio::sync::watch::channel(state);
            assert!(
                futures_util::FutureExt::now_or_never(session_services_settled(&services))
                    .is_some(),
                "{state:?}: the connect waited"
            );
        }
    }

    /// The process the broker test runs in: a profile of its own, with the
    /// identity environment its session provisions, so nothing of the test
    /// process's environment changes. Returns the profile to run in, or
    /// `None` in the parent once the child passed.
    fn isolated_session_root(test: &str) -> Option<std::path::PathBuf> {
        const ROOT: &str = "TILO_IN_PROCESS_TEST_ROOT";
        if let Some(root) = std::env::var_os(ROOT) {
            return Some(std::path::PathBuf::from(root));
        }
        let dir = tempfile::tempdir().unwrap();
        let environment = dir.path().join("environment");
        let log = dir.path().join("child.log");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(ROOT, dir.path().join("profile"))
            .env("XDG_CONFIG_HOME", environment.join("config"))
            .env("XDG_DATA_HOME", environment.join("data"))
            .env("XDG_CACHE_HOME", environment.join("cache"))
            .env("XDG_STATE_HOME", environment.join("state"))
            .env("TILO_IDENTITY_ROOT", environment.join("identity"))
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(std::process::Stdio::inherit())
            .status()
            .unwrap();
        let output = std::fs::read_to_string(&log).unwrap();
        assert!(status.success(), "{output}");
        assert!(
            output.contains("test result: ok. 1 passed"),
            "the isolated child ran no test: {output}"
        );
        None
    }

    /// An app the session shows in-process, drawn frame after frame in a
    /// tile of its own.
    struct Shown {
        tile: Box<dyn ui::widget::AppWidget>,
        taffy: taffy::TaffyTree<()>,
        text: ui::text_render::TextRenderer,
    }

    impl Shown {
        fn mount(
            apps: &mut InProcessApps,
            definition: &'static ApplicationDefinition,
            room: &str,
            tab: u64,
            lend: impl FnOnce(&mut HostContext),
        ) -> Self {
            let component = apps
                .mount(definition, room, tab, RecordId::for_tab(tab), lend)
                .unwrap();
            let collaboration = apps.collaboration(&component, room);
            assert!(
                collaboration.is_some(),
                "the app reaches the session's broker"
            );
            let mut tile =
                tilo_desktop::resolver::app_tile(component, collaboration, |_| {}).unwrap();
            let mut taffy = taffy::TaffyTree::new();
            let mut nodes = std::collections::HashMap::new();
            let node = tile.build_layout(&mut taffy, &ui::theme::Theme::default(), &mut nodes);
            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(900.0),
                        height: taffy::AvailableSpace::Definite(600.0),
                    },
                )
                .unwrap();
            tile.apply_layout(&taffy, node, vello::kurbo::Point::ZERO);
            Self {
                tile,
                taffy,
                text: ui::text_render::TextRenderer::new(),
            }
        }

        fn frame(&mut self) {
            let _ = &self.taffy;
            self.tile.draw(
                &mut vello::Scene::new(),
                vello::kurbo::Affine::IDENTITY,
                &mut self.text,
                &ui::theme::Theme::default(),
                &ui::presence::PresenceCtx::empty(),
            );
        }
    }

    /// Frames of `shown` until `done`, bounded.
    fn until(
        shown: &mut [&mut Shown],
        what: impl std::fmt::Display,
        mut done: impl FnMut() -> bool,
    ) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            for app in shown.iter_mut() {
                app.frame();
            }
            if done() {
                return;
            }
            let states: Vec<String> = shown
                .iter()
                .map(|app| {
                    app.tile
                        .as_any()
                        .and_then(|any| any.downcast_ref::<ui::app::EmbeddedApp>())
                        .map_or_else(String::new, |embedded| {
                            format!("{:?}", embedded.app().collaboration_state())
                        })
                })
                .collect();
            assert!(std::time::Instant::now() < deadline, "{what}: {states:?}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
