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
    /// Messages shown in this process reaches the service as the app would.
    #[cfg(target_os = "linux")]
    messaging_connection: Option<std::sync::Arc<tilo_messaging_service::client::Connection>>,
    credential_owner: Option<accounts::CredentialOwner>,
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
            #[cfg(target_os = "linux")]
            messaging_connection: None,
            credential_owner: None,
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
    #[cfg(feature = "in-process-apps")]
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
        if let Some(owner) = &self.credential_owner {
            owner.bind_request_wake(wake.clone());
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

    /// A collaboration manager for `component`, mounted in `room` as an
    /// instance of its app with no window ([`crate::shell::windowless`]), on
    /// the session's broker. `None` for apps that reach no broker of a
    /// session.
    pub(crate) fn windowless_collaboration(
        &self,
        component: &AppComponent,
        room: &str,
    ) -> Option<ui::collaboration::CollaborationManager> {
        let socket = self.native_broker_socket.clone()?;
        component.collaborate_through(windowless_connector(
            socket,
            launcher::services::identity_socket(),
            component.app_id(),
            room,
        ))
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
        let mut apps = Self::native_for_profile(
            &crate::shell::settings_records::session_apps(),
            &person,
            author,
            launcher::services::collab_socket(),
        )?;
        let owner = accounts::CredentialOwner::deferred(
            &apps.author.0,
            &apps.device_id,
            accounts::SecretStore::open,
        )
        .map_err(|_| LaunchError::BindingMismatch)?;
        apps.install_credential_owner(owner)?;
        Ok(apps)
    }

    /// Install one original per-person/device vault owner before lending apps.
    /// This performs no storage I/O and never creates a credential permission.
    /// # Errors
    /// Refuses another person/device or replacement of an active native owner.
    pub fn install_credential_owner(
        &mut self,
        owner: accounts::CredentialOwner,
    ) -> Result<(), LaunchError> {
        if owner.principal() != self.author.0
            || owner.device() != self.device_id
            || self.credential_owner.is_some()
        {
            return Err(LaunchError::BindingMismatch);
        }
        if let Some(wake) = &self.session_wake {
            owner.bind_request_wake(wake.clone());
        }
        self.credential_owner = Some(owner);
        Ok(())
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
        let (mut context, target) = if record.is_package_primary() && definition.keeps_room_record()
        {
            // The workspace's own record of an app that keeps one, opened as
            // a window of the app opens it: the record every window and the
            // room's readers share, created by the first to open it.
            let target = definition
                .unsaved_target(&records)
                .map_err(LaunchError::Record)?;
            (HostContext::prepare(definition, inputs(), &target)?, target)
        } else {
            let checkout = RecordCheckout::new(record, definition.record_kind.clone());
            match records.try_open_record(&checkout) {
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
            }
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
        // Messages shown here reaches the messaging service as it would in
        // its own process: the same enrollment and accounts.
        #[cfg(target_os = "linux")]
        if definition.app_id() == tilo_messaging_service::MESSAGES_APP
            && let Some(connection) = self.messaging_connection()
        {
            context.lend(connection.enrollment());
            context.lend(connection.messaging());
        }
        if definition.app_id() == "so.tilo.passwords"
            && let Some(owner) = &self.credential_owner
        {
            context.lend(owner.clone());
        }
        lend(&mut context);
        definition.instantiate(context, target, RestoreInput::None)
    }

    /// A connection to the messaging service, made once it runs.
    #[cfg(target_os = "linux")]
    fn messaging_connection(
        &mut self,
    ) -> Option<std::sync::Arc<tilo_messaging_service::client::Connection>> {
        if self.messaging_connection.is_none() && self.messaging.is_some() {
            self.messaging_connection = tilo_messaging_service::client::Connection::connect(
                &launcher::services::socket_for(tilo_messaging_service::SERVICE),
                tilo_messaging_service::MESSAGES_APP,
            )
            .ok();
        }
        self.messaging_connection.clone()
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
    connector_recorded_by(
        collab,
        identity,
        app,
        room,
        crate::shell::launcher::register_in_process_launch,
    )
}

/// [`session_connector`] for an instance of `app` the session runs with no
/// window ([`crate::shell::windowless`]): its launch says so, so the broker
/// hands it the app's actions on its records and never an `open`.
fn windowless_connector(
    collab: std::path::PathBuf,
    identity: std::path::PathBuf,
    app: &str,
    room: &str,
) -> ui::collaboration::HostSessionConnector {
    connector_recorded_by(
        collab,
        identity,
        app,
        room,
        crate::shell::launcher::register_windowless_launch,
    )
}

fn connector_recorded_by(
    collab: std::path::PathBuf,
    identity: std::path::PathBuf,
    app: &str,
    room: &str,
    record: fn(&str, &str),
) -> ui::collaboration::HostSessionConnector {
    let (app, room) = (app.to_owned(), room.to_owned());
    // The collaboration socket carries Files' selected descriptors to the
    // broker, as a launched app's inherited connection does.
    ui::collaboration::HostSessionConnector::with_descriptor_lane(move |_route| async move {
        let failed = |error: std::io::Error| runtime_portal::NotAvailable::ConnectionFailed(error);
        let mut stream = std::os::unix::net::UnixStream::connect(&collab).map_err(failed)?;
        record(&app, &room);
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
    use tilo_resource::Resource as _;
    include!("in_process_files_clipboard_tests.rs");
    include!("in_process_publication_tests.rs");
    include!("in_process_terminal_open_tests.rs");
    include!("in_process_window_search_tests.rs");

    const ROOM: &str = "6a3f0c2e-0f1b-4c5d-9e8f-123456789abc";
    const TAB: u64 = 41;

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

    fn mount_terminal(apps: &mut InProcessApps) -> ui::headless::FrameHarness {
        let component = apps
            .mount(
                terminal::definition(),
                ROOM,
                TAB,
                RecordId::for_tab(TAB),
                |context| {
                    context.lend(terminal::HostedProcessServices::unavailable());
                },
            )
            .unwrap();
        let mut harness = ui::headless::FrameHarness::from_component(
            component,
            (900, 600),
            ui::application::ComponentRunnerInputs::from_native_process(),
        )
        .unwrap();
        harness.relayout();
        harness
    }

    /// A terminal opened on the frame loop, whose store opens records on its own thread, is
    /// still opening while its records are, and mounts once they are ready.
    #[test]
    fn a_terminal_mounted_without_waiting_on_the_disk_mounts_once_its_record_is_ready() {
        let profile = tempfile::tempdir().unwrap();
        let mut apps = InProcessApps::at(
            profile.path(),
            &profile.path().join("person"),
            "test-person",
        )
        .unwrap();
        let (woken, wakes) = std::sync::mpsc::channel();
        let woken = std::sync::Mutex::new(woken);
        durable_store::waiting::decline_on_this_thread(std::sync::Arc::new(move || {
            let _ = woken.lock().unwrap().send(());
        }));
        let deadline = tilo_test_env::hang_deadline();
        loop {
            match apps.mount(
                terminal::definition(),
                ROOM,
                TAB,
                RecordId::for_tab(TAB),
                |context| context.lend(terminal::HostedProcessServices::unavailable()),
            ) {
                Ok(_) => break,
                Err(error) if error.is_opening() => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the terminal never stopped opening"
                    );
                    tilo_test_env::recv("the store to wake the frame loop", &wakes);
                }
                Err(error) => panic!("the terminal failed instead of opening: {error}"),
            }
        }
    }

    #[test]
    fn hosted_terminal_missing_ush_reports_startup_failure_and_refuses_input() {
        let profile = tempfile::tempdir().unwrap();
        let mut apps = InProcessApps::at(
            profile.path(),
            &profile.path().join("person"),
            "test-person",
        )
        .unwrap();
        let mut harness = mount_terminal(&mut apps);
        let address = |operation| ui::bound_dispatch::BoundActionAddress {
            instance_id: TAB,
            address: tilo_actions::ActionAddress::new(terminal::Terminal::KIND, operation),
        };
        let inspection = harness
            .invoke_bound_action(&address("inspect-session"), &serde_json::Map::new())
            .expect("the mounted Terminal exports read-only process inspection")
            .completed()
            .expect("inspection completes without starting a process")
            .value
            .expect("inspection returns its process state");
        assert_eq!(inspection["lifecycle"], "no_runtime");
        assert_eq!(inspection["input_available"], false);
        assert!(
            inspection["startup_reason"]
                .as_str()
                .unwrap()
                .contains("ush"),
            "the missing required service is visible through the public resource"
        );
        assert!(
            harness
                .invoke_bound_action_confirmed(
                    &address("run"),
                    serde_json::json!({ "command": "printf startup-check" })
                        .as_object()
                        .unwrap(),
                )
                .is_err(),
            "even reviewed input must not complete against a capture-only sink"
        );
        harness.close_components();
    }

    /// A terminal tab the shell hosts is the terminal's own component: it
    /// offers the terminal's actions through the shell's door, and what it
    /// keeps is in the tab's record, there again when the tab is mounted
    /// again.
    #[test]
    fn a_hosted_terminal_tab_publishes_its_actions_and_keeps_its_record() {
        let profile = tempfile::tempdir().unwrap();
        let mut apps = InProcessApps::at(
            profile.path(),
            &profile.path().join("person"),
            "test-person",
        )
        .unwrap();
        let mut harness = mount_terminal(&mut apps);
        let session = harness
            .resources()
            .into_iter()
            .find(|resource| resource.kind == terminal::Terminal::KIND)
            .expect("the tab publishes the terminal's session");
        for action in ["run", "send", "interrupt", "open-in"] {
            assert!(
                session.actions.iter().any(|spec| spec.id == action),
                "{action} reaches the shell's palette and agents"
            );
        }
        let target = terminal::target::SessionTarget::Legacy { vm: "build".into() };
        harness
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: TAB,
                    address: tilo_actions::ActionAddress::new(
                        terminal::Terminal::KIND,
                        "record-target",
                    ),
                },
                serde_json::json!({ "target": target.encode() })
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        harness.relayout();
        // The tab closes as the shell closes it: its record stored first.
        harness.flush_component_records().unwrap();
        harness.close_components();
        drop(harness);

        let _again = mount_terminal(&mut apps);
        let records = apps
            .owners
            .open(terminal::definition().app_id(), ROOM)
            .unwrap();
        let kept = records
            .with(&RecordId::for_tab(TAB), |doc| {
                terminal::Terminal::read(doc).map(|state| state.session_target())
            })
            .expect("the tab's record is open")
            .unwrap();
        assert_eq!(kept, Some(target), "the tab's record keeps its last target");
    }

    /// Files mounted in the shell starts in the folder the shell names, and
    /// a folder pinned to its Favourites is in the room's record of places,
    /// there for the next Files tab.
    #[test]
    fn a_hosted_files_tab_starts_where_the_shell_says_and_keeps_its_favourites() {
        let profile = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("harbour-plan.txt"), "plan").unwrap();
        let folder = folder.path().canonicalize().unwrap();
        let mut apps = InProcessApps::at(
            profile.path(),
            &profile.path().join("person"),
            "test-person",
        )
        .unwrap();
        let mount = |apps: &mut InProcessApps, tab: u64| {
            let startup = file_explorer::StartupInputs {
                initial_dir: folder.clone(),
                workspace_roots: Vec::new(),
                record_roots: Vec::new(),
                remote_accounts: Vec::new(),
                trusted_humans: file_explorer::identity_directory::TrustedHumanDirectory::default(),
            };
            let component = apps
                .mount(
                    file_explorer::definition(),
                    ROOM,
                    tab,
                    RecordId::room_default(),
                    |context| {
                        context.lend(startup);
                    },
                )
                .unwrap();
            let mut harness = ui::headless::FrameHarness::from_component(
                component,
                (1000, 700),
                ui::application::ComponentRunnerInputs::from_native_process(),
            )
            .unwrap();
            harness.relayout();
            harness
        };
        let mut harness = mount(&mut apps, 51);
        assert!(
            harness
                .inspection_json()
                .unwrap()
                .contains("harbour-plan.txt"),
            "Files lists the folder the shell named"
        );
        let places = |apps: &mut InProcessApps| {
            let records = apps
                .owners
                .open(file_explorer::definition().app_id(), ROOM)
                .unwrap();
            records
                .with(&RecordId::room_default(), |doc| {
                    file_explorer::FileExplorer::read(doc).map(|state| state.bookmarks())
                })
                .expect("the room's record of places is open")
                .unwrap()
        };
        let before = places(&mut apps);
        let place = folder.to_string_lossy().into_owned();
        harness
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: 51,
                    address: tilo_actions::ActionAddress::new("files.folder", "add-bookmark"),
                },
                serde_json::json!({ "place": place }).as_object().unwrap(),
            )
            .unwrap();
        harness.relayout();
        harness.flush_component_records().unwrap();
        harness.close_components();
        drop(harness);

        let _next = mount(&mut apps, 52);
        let after = places(&mut apps);
        assert_eq!(
            after.len(),
            before.len() + 1,
            "the pinned folder is in the room's places: {after:?}"
        );
    }

    /// The shell's tab `tab` of `kind`, mounted as the shell's tile resolver
    /// mounts it.
    fn mount_tab(
        inputs: &crate::shell::hosted_inputs::HostedTileInputs,
        tab: u64,
        kind: crate::shell::state::AppKind,
    ) -> ui::headless::FrameHarness {
        let tab = crate::shell::state::Tab {
            id: tab,
            title: "Tab".into(),
            tile: crate::shell::state::TileContent::AppPlaceholder {
                kind,
                label: "Tab".into(),
            },
            app_title: None,
        };
        let room = crate::shell::state::RoomId::new();
        let request = tilo_desktop::resolver::TileRequest { room, tab: &tab };
        let Some(tilo_desktop::resolver::TileResolution::App(component, _)) =
            crate::shell::mount_in_process(&request, inputs)
        else {
            panic!("the tab mounts its app's own component");
        };
        let mut harness = ui::headless::FrameHarness::from_component(
            component,
            (1000, 700),
            ui::application::ComponentRunnerInputs::from_native_process(),
        )
        .unwrap();
        harness.relayout();
        harness
    }

    fn notes_kind() -> crate::shell::state::AppKind {
        crate::shell::state::AppKind::Other("Notes".into())
    }

    /// Every Notes tab is a notebook of its own, on the tab's record: a note
    /// written into one is not in the other, and each is there again when
    /// its tab is mounted again.
    #[test]
    fn each_notes_tab_is_a_notebook_of_its_own_kept_in_its_record() {
        let profile = tempfile::tempdir().unwrap();
        let mut apps = InProcessApps::at(
            profile.path(),
            &profile.path().join("person"),
            "test-person",
        )
        .unwrap();
        let write = |apps: &mut InProcessApps, tab: u64, title: &str| {
            let component = apps
                .mount(
                    notes::definition(),
                    ROOM,
                    tab,
                    RecordId::for_tab(tab),
                    |_| {},
                )
                .unwrap();
            let mut harness = ui::headless::FrameHarness::from_component(
                component,
                (1000, 700),
                ui::application::ComponentRunnerInputs::from_native_process(),
            )
            .unwrap();
            harness.relayout();
            let import = notes::actions::import_markdown_action(
                Some(uuid::Uuid::new_v4()),
                title,
                &format!("{title} body"),
                Vec::new(),
            );
            harness
                .invoke_bound_action(
                    &ui::bound_dispatch::BoundActionAddress {
                        instance_id: tab,
                        address: tilo_actions::ActionAddress::new(
                            notes::Notebook::KIND,
                            tilo_resource::APPLY,
                        ),
                    },
                    serde_json::json!({
                        tilo_resource::APPLY_PARAM: serde_json::to_value(import).unwrap()
                    })
                    .as_object()
                    .unwrap(),
                )
                .unwrap();
            harness.relayout();
            harness.flush_component_records().unwrap();
            harness.close_components();
        };
        write(&mut apps, 61, "Alpha");
        write(&mut apps, 62, "Beta");

        let titles = |apps: &mut InProcessApps, tab: u64| -> Vec<String> {
            let records = apps
                .owners
                .open(notes::definition().app_id(), ROOM)
                .unwrap();
            records
                .try_open_record(&RecordCheckout::new(
                    RecordId::for_tab(tab),
                    notes::definition().record_kind.clone(),
                ))
                .expect("the tab's record was kept");
            records
                .with(&RecordId::for_tab(tab), |doc| {
                    notes::Notebook::read(doc).map(|state| {
                        state
                            .pages
                            .values()
                            .map(|page| page.title.clone())
                            .collect::<Vec<_>>()
                    })
                })
                .expect("the tab's notebook is in its record")
                .unwrap()
        };
        assert_eq!(titles(&mut apps, 61), vec!["Alpha".to_owned()]);
        assert_eq!(
            titles(&mut apps, 62),
            vec!["Beta".to_owned()],
            "the second tab's notebook is its own"
        );
    }

    /// The shell's tile resolver mounts Notes and Editor tabs as those apps'
    /// own components; an Editor tab opened on a file is the editor's
    /// project of that file.
    #[test]
    fn notes_and_editor_tabs_are_their_apps_own_components() {
        let profile = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("harbour-plan.txt");
        std::fs::write(&file, "moor at the east quay\n").unwrap();
        let file = file.canonicalize().unwrap();
        let mut inputs = crate::shell::hosted_inputs::HostedTileInputs::default();
        inputs.mount_apps_in_process(
            InProcessApps::at(
                profile.path(),
                &profile.path().join("person"),
                "test-person",
            )
            .unwrap(),
        );

        let notes = mount_tab(&inputs, 71, notes_kind());
        assert!(
            notes
                .resources()
                .iter()
                .any(|resource| resource.kind == notes::Notebook::KIND),
            "a Notes tab publishes its notebook"
        );

        inputs.bind_document(
            72,
            crate::shell::hosted_inputs::HostedDocument {
                provider_id: "org.nirios.Editor",
                path: file,
            },
        );
        let editor = mount_tab(
            &inputs,
            72,
            crate::shell::state::AppKind::Other("Editor".into()),
        );
        assert!(
            editor
                .resources()
                .iter()
                .any(|resource| resource.kind == code_editor::Project::KIND),
            "an Editor tab publishes its project"
        );
        let mut editor = editor;
        assert!(
            editor.wait_until(tilo_test_env::hang_bound(), |editor| {
                editor
                    .inspection_json()
                    .is_ok_and(|inspection| inspection.contains("harbour-plan.txt"))
            }),
            "the Editor tab opened the file it was opened on"
        );
    }

    /// The hosted Editor's text selection and clipboard commands are driven
    /// through the keyboard, headlessly: select all, copy, and paste into a
    /// second document that already holds text of its own.
    ///
    /// A teacher that has to edit a document can select and copy text instead
    /// of deleting it one character at a time, which is what a person does.
    /// The checks read the published accessible selection, so the route is the
    /// one a screen reader and an agent also take.
    /// An Editor tab `tab` showing the file at `path`, with the keyboard on
    /// its document.
    fn focused_editor_on(
        inputs: &crate::shell::hosted_inputs::HostedTileInputs,
        tab: u64,
        path: &std::path::Path,
    ) -> ui::headless::FrameHarness {
        inputs.bind_document(
            tab,
            crate::shell::hosted_inputs::HostedDocument {
                provider_id: "org.nirios.Editor",
                path: path.canonicalize().unwrap(),
            },
        );
        let mut editor = mount_tab(
            inputs,
            tab,
            crate::shell::state::AppKind::Other("Editor".into()),
        );
        assert!(
            editor
                .wait_for_named("editor_view", tilo_test_env::hang_bound())
                .is_ok(),
            "Editor tab {tab} shows its document"
        );
        assert!(
            editor
                .perform_named("editor_view", &ui::a11y::Action::Focus)
                .is_ok(),
            "the keyboard reaches tab {tab}'s document"
        );
        editor
    }

    #[test]
    fn the_hosted_editor_selects_copies_and_pastes_through_the_keyboard() {
        use ui::headless::Action;

        let profile = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let mut inputs = crate::shell::hosted_inputs::HostedTileInputs::default();
        inputs.mount_apps_in_process(
            InProcessApps::at(
                profile.path(),
                &profile.path().join("person"),
                "test-person",
            )
            .unwrap(),
        );

        let line = "moor at the east quay\n";
        let source = folder.path().join("harbour-plan.txt");
        std::fs::write(&source, line).unwrap();
        let mut editor = focused_editor_on(&inputs, 81, &source);

        let outcome = ui::headless::driver::drive(
            &mut editor,
            &[
                Action::Modifiers(vec!["ctrl".into()]).into(),
                Action::Key("a".into()).into(),
                Action::Key("c".into()).into(),
                Action::Modifiers(Vec::new()).into(),
                Action::Expect(
                    ui::headless::SemanticExpectation::named("editor_view")
                        .value(line)
                        .text_selection(0, line.len()),
                )
                .into(),
            ],
        );
        assert!(
            outcome.is_pass(),
            "select all and copy must select the whole document: {:?}",
            outcome.failure()
        );

        let drafted = "";
        let destination = folder.path().join("tide-notes.txt");
        std::fs::write(&destination, drafted).unwrap();
        let mut second = focused_editor_on(&inputs, 82, &destination);

        // Paste the copied line into the empty destination: one command that
        // writes text a character-by-character route would have typed.
        let outcome = ui::headless::driver::drive(
            &mut second,
            &[
                Action::Modifiers(vec!["ctrl".into()]).into(),
                Action::Key("v".into()).into(),
                Action::Key("s".into()).into(),
                Action::Modifiers(Vec::new()).into(),
                Action::Expect(
                    ui::headless::SemanticExpectation::named("editor_view")
                        .value(line)
                        .text_selection(line.len(), line.len()),
                )
                .into(),
            ],
        );
        assert!(
            outcome.is_pass(),
            "paste must write the copied line at the caret: {:?}",
            outcome.failure()
        );
        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            line,
            "the pasted text is the document the copy came from"
        );
    }

    /// A Notes tab shown as an app of its own runs what its launch queued:
    /// a Markdown file it is opened on becomes a note in the tab's notebook.
    #[test]
    fn a_notes_tab_runs_its_launch_and_keeps_the_note_it_opened() {
        let profile = tempfile::tempdir().unwrap();
        let mut apps = InProcessApps::at(
            profile.path(),
            &profile.path().join("person"),
            "test-person",
        )
        .unwrap();
        let (file, _receipt) = ui::application::OpenFile::new(
            "Harbour plan.md",
            "text/markdown",
            b"Moor at the east quay.".to_vec(),
        )
        .unwrap();
        let component = apps
            .mount(
                notes::definition(),
                ROOM,
                81,
                RecordId::for_tab(81),
                |context| {
                    context.provide_open_file(file);
                },
            )
            .unwrap();
        let mut app = tilo_desktop::resolver::app_tile(component, None, |_| {}).unwrap();
        let mut taffy = taffy::TaffyTree::new();
        let mut nodes = std::collections::HashMap::new();
        let theme = ui::theme::Theme::default();
        let node = app.build_layout(&mut taffy, &theme, &mut nodes);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(900.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, vello::kurbo::Point::ZERO);
        let mut text = ui::text_render::TextRenderer::new();
        for _ in 0..3 {
            let mut scene = vello::Scene::new();
            app.draw(
                &mut scene,
                vello::kurbo::Affine::IDENTITY,
                &mut text,
                &theme,
                &ui::presence::PresenceCtx::empty(),
            );
        }
        let _reports = ui::component_lifecycle::close_components(app.as_mut());

        let records = apps
            .owners
            .open(notes::definition().app_id(), ROOM)
            .unwrap();
        records
            .try_open_record(&RecordCheckout::new(
                RecordId::for_tab(81),
                notes::definition().record_kind.clone(),
            ))
            .expect("the tab's record was kept");
        let titles = records
            .with(&RecordId::for_tab(81), |doc| {
                notes::Notebook::read(doc).map(|state| {
                    state
                        .pages
                        .values()
                        .map(|page| page.title.clone())
                        .collect::<Vec<_>>()
                })
            })
            .expect("the tab's notebook is open")
            .unwrap();
        assert_eq!(titles, vec!["Harbour plan".to_owned()]);
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

    /// What a wait last saw, said when it never ends.
    struct LastSeen<'a>(&'static str, &'a std::cell::RefCell<String>);

    impl std::fmt::Display for LastSeen<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{} ({})", self.0, self.1.borrow())
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

    /// Apps the session shows in its own process publish through the
    /// session's broker as launched apps do: an agent finds a Notes tab's
    /// note by a word of its body and invokes the notebook's actions there,
    /// and a Terminal tab is reachable the same way.
    /// A session with no workspace host configured hosts its workspace on
    /// this device's own room host, as the desktop does, and an app it runs
    /// creates its record in that room.
    #[test]
    fn a_session_without_a_collab_host_hosts_its_workspace_and_its_apps_create_records() {
        const TEST: &str = "shell::in_process::tests::a_session_without_a_collab_host_hosts_its_workspace_and_its_apps_create_records";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        assert!(
            !crate::shell::network::collab_host_configured(),
            "no workspace host is configured"
        );
        std::fs::create_dir_all(&profile).unwrap();
        let session = crate::runtime_host::Session::start(&profile).unwrap();
        let room = crate::shell::state::RoomId::parse(&session.room).unwrap();
        assert!(
            session.network().serves_here(room),
            "the session's workspace is hosted on this device's own host"
        );
        assert!(
            !session
                .workspace_tree()
                .read()
                .pending_placements
                .contains(&room),
            "nothing waits to be placed"
        );
        let collab = session.broker.capnp_socket().to_path_buf();
        let identity = session.identity_socket().to_path_buf();
        let mut apps = InProcessApps::at(
            &profile.join("apps"),
            &profile.join("apps").join("person"),
            &session.actor,
        )
        .unwrap()
        .reaching_broker(Rc::new(move |app, room| {
            session_connector(collab.clone(), identity.clone(), app, room)
        }));
        // The Notes tab opens a note, which it keeps as its record.
        let (file, _receipt) = ui::application::OpenFile::new(
            "Harbour plan.md",
            "text/markdown",
            b"Moor at the quayside before the tide turns.".to_vec(),
        )
        .unwrap();
        let mut notes = Shown::mount(
            &mut apps,
            notes::definition(),
            &session.room,
            93,
            |context| {
                context.provide_open_file(file);
            },
        );
        // The room's host holds the record: committed in the clear, or, in
        // a room whose records are sealed channels, as the sealed writes
        // the host keeps under the record's wire name.
        let app: &str = &notes::definition().app_id;
        let record = RecordId::for_tab(93).to_string();
        let wire = session
            .network()
            .register_local_record(app, &session.room, &record);
        let host = session
            .network()
            .room_host()
            .expect("the session runs its host");
        let runtime = session.network().runtime_handle().unwrap();
        let held = std::cell::RefCell::new(String::new());
        until(
            &mut [&mut notes],
            LastSeen("the Notes tab's record exists in the room", &held),
            || {
                let routes = runtime.block_on(host.rooms().record_routes(&session.room));
                let found = routes.as_ref().is_ok_and(|routes| {
                    routes.iter().any(|(held_app, held_record)| {
                        held_app == app && (*held_record == record || *held_record == wire)
                    })
                });
                let (sealed, committed) = {
                    let handle = session.workspace_tree();
                    let tree = handle.read();
                    (
                        tree.seals_records(room),
                        tree.committed_record_rooms.contains(&room),
                    )
                };
                *held.borrow_mut() = format!(
                    "host routes {routes:?}, record {record}, wire {wire}, sealed here {sealed}, \
                     committed here {committed}"
                );
                found
            },
        );
        drop(host);
        drop(notes);
        drop(apps);
        drop(session);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        clippy::significant_drop_tightening,
        reason = "one end-to-end walk through a live session, which owns its services for the whole test"
    )]
    fn agents_reach_in_process_apps_through_the_session_broker() {
        const TEST: &str =
            "shell::in_process::tests::agents_reach_in_process_apps_through_the_session_broker";
        let Some(profile) = isolated_session_root(TEST) else {
            return;
        };
        std::fs::create_dir_all(&profile).unwrap();
        let session = crate::runtime_host::Session::start(&profile).unwrap();
        let collab = session.broker.capnp_socket().to_path_buf();
        let identity = session.identity_socket().to_path_buf();
        let mut apps = InProcessApps::at(
            &profile.join("apps"),
            &profile.join("apps").join("person"),
            &session.actor,
        )
        .unwrap()
        .reaching_broker(Rc::new(move |app, room| {
            session_connector(collab.clone(), identity.clone(), app, room)
        }));
        let room = session.room.clone();
        // The Notes tab is opened on a note whose body has a word no title has.
        let (file, _receipt) = ui::application::OpenFile::new(
            "Harbour plan.md",
            "text/markdown",
            b"Moor at the quayside before the tide turns.".to_vec(),
        )
        .unwrap();
        let mut notes = Shown::mount(&mut apps, notes::definition(), &room, 91, |context| {
            context.provide_open_file(file);
        });
        let mut terminal = Shown::mount(&mut apps, terminal::definition(), &room, 92, |context| {
            context.lend(terminal::HostedProcessServices::unavailable());
        });

        let broker = session.broker.app_action_broker().unwrap();
        let host = crate::shell::remote_actions::BrokerActionHost::new(broker.clone());
        let caller = tilo_actions::remote::Caller::local_person(&session.actor).in_room(&room);
        let provider_of = |app: &str| {
            broker
                .state()
                .action_directory_root_page(None, 16)
                .roots
                .into_iter()
                .find(|root| root.app_id == app)
                .map(|root| root.peer_id.to_string())
        };
        until(
            &mut [&mut notes, &mut terminal],
            "both apps publish",
            || {
                provider_of(notes::definition().app_id()).is_some()
                    && provider_of(terminal::definition().app_id()).is_some()
            },
        );
        let notes_provider = provider_of(notes::definition().app_id()).unwrap();
        let terminal_provider = provider_of(terminal::definition().app_id()).unwrap();

        let query = |provider: &str, needle: &str| {
            tilo_actions::remote::ActionHost::query(
                &host,
                &caller,
                tilo_actions::remote::CatalogueQuery {
                    provider: Some(provider.to_owned()),
                    needle: needle.to_owned(),
                    ..Default::default()
                },
            )
        };
        let wait_page = |shown: &mut [&mut Shown],
                         pending: tilo_actions::remote::Pending<
            Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionHostError>,
        >| {
            let mut pending = pending;
            let mut page = None;
            until(shown, "the directory answers", || {
                match pending.poll_ready() {
                    tilo_actions::remote::PendingPoll::Ready(result) => {
                        page = Some(result.unwrap());
                        true
                    }
                    tilo_actions::remote::PendingPoll::Waiting => false,
                    tilo_actions::remote::PendingPoll::Abandoned => panic!("the query was dropped"),
                }
            });
            page.unwrap()
        };

        // An agent adds a note to the Notes tab through the broker.
        // As an agent does: find the action in the notebook's directory,
        // then invoke it at that listing's revision, listing again when the
        // directory moved on in between.
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let listed = wait_page(
                &mut [&mut notes, &mut terminal],
                query(&notes_provider, "new"),
            );
            let address = listed
                .rows
                .iter()
                .filter_map(|row| row.action.as_ref())
                .find(|action| action.address.action == "new-note")
                .map(|action| action.address.clone())
                .expect("the notebook offers a new note");
            let mut started = tilo_actions::remote::ActionHost::invoke(
                &host,
                &caller,
                tilo_actions::remote::InvokeRequest {
                    on_behalf_of: None,
                    invocation: tilo_actions::InvocationId::new(),
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: notes_provider.clone(),
                        address,
                    },
                    argument: tilo_actions::remote::InvokeArgument::Json(
                        serde_json::json!({ "title": "Tide table" })
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                    context: None,
                    confirmation: None,
                    revision: Some(listed.revision),
                    wait_ms: None,
                    appearance: None,
                },
            );
            let mut outcome = None;
            until(
                &mut [&mut notes, &mut terminal],
                "the new note is answered",
                || match started.poll_ready() {
                    tilo_actions::remote::PendingPoll::Ready(result) => {
                        outcome = Some(result);
                        true
                    }
                    tilo_actions::remote::PendingPoll::Waiting => false,
                    tilo_actions::remote::PendingPoll::Abandoned => {
                        panic!("the invocation was dropped")
                    }
                },
            );
            match outcome.unwrap() {
                Ok(receipt) => {
                    // Accepted is admission; the note is made when the
                    // invocation succeeds.
                    let mut state = None;
                    until(
                        &mut [&mut notes, &mut terminal],
                        "the new note is made",
                        || {
                            state = tilo_actions::remote::ActionHost::observe(
                                &host,
                                &caller,
                                &receipt.invocation,
                            );
                            state
                                .as_ref()
                                .is_some_and(|state| state.update.is_terminal())
                        },
                    );
                    let update = state.map(|state| state.update);
                    assert!(
                        matches!(
                            update,
                            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
                        ),
                        "the new note is made: {update:?}"
                    );
                    break;
                }
                Err(tilo_actions::remote::ActionHostError::StaleRevision)
                    if std::time::Instant::now() < deadline => {}
                Err(error) => panic!("the new note was refused: {error:?}"),
            }
        }

        // The directory lists the note the launch imported, by its title,
        // with the verbs that act on it.
        let found = {
            let mut found = false;
            let deadline = tilo_test_env::hang_deadline();
            while !found {
                let page = wait_page(
                    &mut [&mut notes, &mut terminal],
                    query(&notes_provider, "Harbour plan"),
                );
                found = page.rows.iter().any(|row| {
                    row.action
                        .as_ref()
                        .is_some_and(|action| action.address.action == "rename")
                });
                assert!(
                    found || std::time::Instant::now() < deadline,
                    "no row for the imported note"
                );
            }
            found
        };
        assert!(found);

        // The Terminal tab answers its directory through the same broker.
        let page = wait_page(
            &mut [&mut notes, &mut terminal],
            query(&terminal_provider, "run"),
        );
        assert!(
            page.rows.iter().any(|row| row
                .action
                .as_ref()
                .is_some_and(|action| action.address.action == "run")),
            "the terminal's actions reach agents: {:?}",
            page.rows
                .iter()
                .filter_map(|row| row.action.as_ref().map(|a| a.address.clone()))
                .collect::<Vec<_>>()
        );
    }
}
