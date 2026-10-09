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
use ui::permissions::{PermissionContext, Role, UserId};
use ui::record::{RecordCheckout, RecordId, RecordOpenError, RecordOwners};

#[cfg(test)]
mod feedback_tests;
pub mod messaging;

/// The apps the shell mounts in-process, with the store their records and
/// views live in, as one person on one device.
pub struct InProcessApps {
    feedback: Option<tilo_desktop::feedback::PrivateFeedback>,
    store: Rc<dyn durable_store::DurableStore>,
    owners: RecordOwners,
    views: HashMap<String, Rc<InstanceViewStore>>,
    device_id: String,
    author: UserId,
    /// How an app mounted here reaches the session's broker, as an app the
    /// session launched; `None` keeps them off any broker.
    broker: Option<SessionBroker>,
    messaging: Option<tilo_messaging::MessagingClient>,
    first_accounts: Option<Rc<dyn messaging::first_account::FirstAccounts>>,
    messaging_registry: Option<std::sync::Arc<dyn tilo_messaging::MessagingRegistry>>,
    credential_owner: Option<accounts::CredentialOwner>,
    messaging_records: Option<Rc<dyn messaging::records::RecordQueries>>,
    messaging_record_wake: Option<crate::session_runtime::Wake>,
    messaging_transport:
        Option<tokio::sync::oneshot::Receiver<crate::network::private_messages::Transport>>,
    native_messaging_transport: Option<crate::network::private_messages::Transport>,
    messaging_starts: Option<Rc<dyn messaging::activation::PendingAccounts>>,
    messaging_lifetime: std::sync::Arc<()>,
    messaging_network_prompts: Option<crate::portal::PromptQueueHandle>,
    messaging_providers: Vec<launcher::owned_service::ServiceCancellation>,
    messaging_native_accounts: Vec<std::sync::Arc<dyn messaging::NativeAccountCustody>>,
    native_messaging_peers: Option<crate::network::PeerIdentityStore>,
    native_messaging_root: Option<std::path::PathBuf>,
    native_broker_socket: Option<std::path::PathBuf>,
    receivers: Vec<tilo_messaging_protocol::signal_broker::ReceiveGuard>,
    messaging_services: Vec<(
        tilo_messaging::ServiceAccount,
        tilo_messaging::MessagingClient,
    )>,
}

/// Connects an app the session shows in its own process to the session's
/// broker for `(app, room)`.
pub type SessionBroker = Rc<dyn Fn(&str, &str) -> ui::collaboration::HostSessionConnector>;

impl InProcessApps {
    pub(crate) fn bind_messaging_network_prompts(
        &mut self,
        prompts: crate::portal::PromptQueueHandle,
    ) {
        self.messaging_network_prompts = Some(prompts);
    }

    /// Apps keeping their records in `store`, mounted for `author` on
    /// `device_id`.
    pub fn new(store: Rc<dyn durable_store::DurableStore>, device_id: &str, author: &str) -> Self {
        Self {
            feedback: None,
            owners: RecordOwners::with_store(Rc::clone(&store)),
            store,
            views: HashMap::new(),
            device_id: device_id.to_owned(),
            author: UserId(author.to_owned()),
            broker: None,
            messaging: None,
            first_accounts: None,
            messaging_registry: None,
            credential_owner: None,
            messaging_records: None,
            messaging_record_wake: None,
            messaging_transport: None,
            native_messaging_transport: None,
            messaging_starts: None,
            messaging_lifetime: std::sync::Arc::new(()),
            messaging_network_prompts: None,
            messaging_providers: Vec::new(),
            messaging_native_accounts: Vec::new(),
            native_messaging_peers: None,
            native_messaging_root: None,
            native_broker_socket: None,
            receivers: Vec::new(),
            messaging_services: Vec::new(),
        }
    }

    /// Bind the existing compositor wake once; this opens no metadata/custody.
    pub(crate) fn bind_messaging_record_wake(&mut self, wake: crate::session_runtime::Wake) {
        if self.messaging_record_wake.is_none() {
            if let Some(owner) = &self.credential_owner {
                owner.bind_request_wake(wake.clone());
            }
            self.messaging_record_wake = Some(wake);
        }
    }

    pub(crate) fn bind_private_messages_network(
        &mut self,
        network: &crate::shell::network::ShellNetwork,
    ) {
        if self.native_messaging_transport.is_some() || self.messaging_transport.is_some() {
            return;
        }
        self.native_messaging_peers = Some(network.private_message_identities());
        let (reply, receiver) = tokio::sync::oneshot::channel();
        if network
            .cmd_tx
            .try_send(crate::network::NetCmd::PrivateMessages(reply))
            .is_ok()
        {
            self.messaging_transport = Some(receiver);
        }
    }

    /// Service bounded worker reads on the actual UI owner, without a window.
    pub(crate) fn pump_messaging_records(&mut self) {
        if let Some(receiver) = self.messaging_transport.as_mut() {
            match receiver.try_recv() {
                Ok(transport) => {
                    self.native_messaging_transport = Some(transport);
                    self.messaging_transport = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.messaging_transport = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if let Some(records) = &self.messaging_records {
            records.pump();
        }
        if let Some(starts) = &self.messaging_starts {
            starts.pump();
        }
        if let Some(first) = self.first_accounts.clone() {
            first.pump(self);
        }
    }

    /// Install an already authenticated, device-owned Signal service. No
    /// metadata record creates this authority and another persona is refused.
    /// # Errors
    /// Refuses a broker owned by another person/device or a duplicate service/account installation.
    pub fn install_signal(
        &mut self,
        broker: &tilo_messaging_protocol::signal_broker::SignalBroker,
    ) -> Result<(), LaunchError> {
        let owner = broker.owner().map_err(|_| LaunchError::BindingMismatch)?;
        if owner.principal != self.author.0 || owner.device != self.device_id {
            return Err(LaunchError::BindingMismatch);
        }
        let mut services = self.messaging_services.clone();
        services.push((
            tilo_messaging::ServiceAccount::new("signal", &owner.account),
            broker.client(),
        ));
        let router = tilo_messaging::MessagingRouter::new(&self.author.0, services.clone())
            .map_err(|_| LaunchError::BindingMismatch)?;
        if let Some(registry) = &self.messaging_registry {
            let (identity, client) = services.last().ok_or(LaunchError::BindingMismatch)?;
            registry
                .register(identity.clone(), client.clone())
                .map_err(|_| LaunchError::BindingMismatch)?;
            self.messaging = Some(registry.clone().client());
        } else {
            self.messaging = Some(router.client());
        }
        self.messaging_services = services;
        Ok(())
    }

    /// Install an account-owned `WhatsApp` or SMS service under its frozen device.
    /// # Errors
    /// Refuses another person/device or duplicate service/account registration.
    pub fn install_external(
        &mut self,
        broker: &tilo_messaging_protocol::external_broker::ExternalBroker,
    ) -> Result<(), LaunchError> {
        let owner = broker.owner();
        if owner.principal != self.author.0 || owner.device != self.device_id {
            return Err(LaunchError::BindingMismatch);
        }
        let mut services = self.messaging_services.clone();
        let service = match owner.service {
            tilo_messaging_protocol::Service::Whatsapp => "whatsapp",
            tilo_messaging_protocol::Service::Sms => "sms",
            _ => return Err(LaunchError::BindingMismatch),
        };
        services.push((
            tilo_messaging::ServiceAccount::new(service, &owner.account),
            broker.client(),
        ));
        let router = tilo_messaging::MessagingRouter::new(&self.author.0, services.clone())
            .map_err(|_| LaunchError::BindingMismatch)?;
        if let Some(registry) = &self.messaging_registry {
            let (identity, client) = services.last().ok_or(LaunchError::BindingMismatch)?;
            registry
                .register(identity.clone(), client.clone())
                .map_err(|_| LaunchError::BindingMismatch)?;
            self.messaging = Some(registry.clone().client());
        } else {
            self.messaging = Some(router.client());
        }
        self.messaging_services = services;
        Ok(())
    }

    /// Lend the lifecycle-aware service while its owned native account is
    /// linking, locked or offline. Metadata never installs a usable adapter.
    /// # Errors
    /// Refuses another persona/device or duplicate exact service/account.
    pub fn install_messaging_supervisor(
        &mut self,
        supervisor: &std::sync::Arc<tilo_messaging_protocol::supervisor::AccountSupervisor>,
    ) -> Result<(), LaunchError> {
        let owner = supervisor.owner();
        if owner.principal != self.author.0 || owner.device != self.device_id {
            return Err(LaunchError::BindingMismatch);
        }
        let service = match owner.service {
            tilo_messaging_protocol::Service::Whatsapp => "whatsapp",
            tilo_messaging_protocol::Service::Signal => "signal",
            tilo_messaging_protocol::Service::Sms => "sms",
            tilo_messaging_protocol::Service::Tilo => "tilo",
        };
        let mut services = self.messaging_services.clone();
        services.push((
            tilo_messaging::ServiceAccount::new(service, &owner.account),
            supervisor.client(),
        ));
        let router = tilo_messaging::MessagingRouter::new(&self.author.0, services.clone())
            .map_err(|_| LaunchError::BindingMismatch)?;
        if let Some(registry) = &self.messaging_registry {
            let (identity, client) = services.last().ok_or(LaunchError::BindingMismatch)?;
            registry
                .register(identity.clone(), client.clone())
                .map_err(|_| LaunchError::BindingMismatch)?;
            self.messaging = Some(registry.clone().client());
        } else {
            self.messaging = Some(router.client());
        }
        self.messaging_services = services;
        Ok(())
    }

    /// Install sending and independent inbound persistence as one host setup.
    /// # Errors
    /// Refuses another owner/device, failed receive setup or duplicate service/account installation.
    pub fn install_signal_receiving(
        &mut self,
        broker: &tilo_messaging_protocol::signal_broker::SignalBroker,
        receiver: tilo_messaging_protocol::signal_runtime::SignalReceiver,
    ) -> Result<(), LaunchError> {
        let owner = broker.owner().map_err(|_| LaunchError::BindingMismatch)?;
        if owner.principal != self.author.0
            || owner.device != self.device_id
            || self.messaging_services.iter().any(|(identity, _)| {
                identity.service == "signal" && identity.account == owner.account
            })
        {
            return Err(LaunchError::BindingMismatch);
        }
        let guard = broker
            .start_receiver(receiver)
            .map_err(|_| LaunchError::BindingMismatch)?;
        self.install_signal(broker)?;
        self.receivers.push(guard);
        Ok(())
    }

    /// Reach the session's broker through `broker` for every app mounted
    /// from now on.
    #[must_use]
    pub fn reaching_broker(mut self, broker: SessionBroker) -> Self {
        self.broker = Some(broker);
        self
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
    /// records and views there, as this device.
    ///
    /// # Errors
    /// A relative root or corrupt device identity.
    pub fn at(profile_root: &std::path::Path, author: &str) -> Result<Self, LaunchError> {
        std::fs::create_dir_all(profile_root)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        let profile = ui::application::NativeProfile::open(profile_root, None)?;
        Ok(Self::new(
            Rc::new(app_state::FsStore::new(profile_root.to_path_buf())),
            profile.device_id(),
            author,
        ))
    }

    /// The session's apps: kept under the shell's own state, beside the
    /// records of its scratch tabs.
    ///
    /// # Errors
    /// As [`Self::at`].
    pub fn native(author: &str) -> Result<Self, LaunchError> {
        let mut apps = Self::native_for_profile(
            &cli::data_home()
                .join(crate::shell::SHELL_RECORDS_PACKAGE)
                .join("apps"),
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
        if let Some(wake) = &self.messaging_record_wake {
            owner.bind_request_wake(wake.clone());
        }
        self.credential_owner = Some(owner);
        Ok(())
    }

    /// The same native startup, with an explicit private profile and broker
    /// scope for owned native hosts and isolated integration fixtures.
    pub(crate) fn native_for_profile(
        profile: &std::path::Path,
        author: &str,
        socket: std::path::PathBuf,
    ) -> Result<Self, LaunchError> {
        let connector = socket.clone();
        let mut apps = Self::at(profile, author)?.reaching_broker(Rc::new(move |app, room| {
            session_connector(
                connector.clone(),
                launcher::services::identity_socket(),
                app,
                room,
            )
        }));
        apps.native_broker_socket = Some(socket);
        apps.native_messaging_root = Some(profile.join("private-messages"));
        let registry = tilo_messaging::messaging_registry(&apps.author.0)
            .map_err(|_| LaunchError::BindingMismatch)?;
        apps.messaging_registry = Some(registry);
        Ok(apps)
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
        if definition.app_id == tilo_desktop::feedback::PACKAGE {
            return match &self.feedback {
                Some(feedback) => feedback.prepare(&self.device_id, &self.author.0, tab),
                None => tilo_desktop::feedback::unconfigured(&self.device_id, &self.author.0, tab),
            };
        }
        if definition.app_id == "so.tilo.messages" {
            if self.first_accounts.is_none()
                && self.native_broker_socket.is_some()
                && let Some(wake) = &self.messaging_record_wake
            {
                self.first_accounts = Some(messaging::first_account::new(
                    self.author.0.clone(),
                    wake.clone(),
                ));
            }
            self.prepare_default_messaging();
        }
        let records = self
            .owners
            .open(definition.app_id, room)
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
            permissions: Rc::new(RefCell::new(PermissionContext::member(
                &self.author.0,
                Role::Owner,
            ))),
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
        if let Ok(person) = ui::record::person::PersonRecords::open(
            &mut self.owners,
            definition.app_id,
            self.author.clone(),
        ) {
            context.provide_person_records(person);
        }
        if definition.app_id == "so.tilo.messages" {
            if let Some(first) = &self.first_accounts {
                context.lend(first.client());
            }
            if let Some(service) = &self.messaging {
                context.lend(service.clone());
            } else if let Some(registry) = &self.messaging_registry {
                // Empty routing/setup lifetime, not an admitted account or a
                // ready provider. Protected admission still publishes neither.
                context.lend(registry.clone().client());
            }
        }
        if definition.app_id == "so.tilo.passwords"
            && let Some(owner) = &self.credential_owner
        {
            context.lend(owner.clone());
        }
        lend(&mut context);
        definition.instantiate(context, target, RestoreInput::None)
    }
}

impl InProcessApps {
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
        Ok(())
    }

    /// Storage/transport wakes service the approved outbox, including while
    /// its private window is closed. Failures remain in the private status UI.
    pub(crate) fn pump_feedback(&self) {
        if let Some(feedback) = &self.feedback {
            let _ = feedback.pump();
        }
    }

    pub(crate) fn feedback_deadline(&self) -> Option<ui::time::Instant> {
        self.feedback
            .as_ref()
            .and_then(tilo_desktop::feedback::PrivateFeedback::next_wake_deadline)
    }
}

/// A connection for `app` in `room` to the broker at `collab` and the
/// identity service at `identity`, made as the launcher makes an app's: the
/// session records the launch, the connection names the app and room, and
/// the broker attests it against that record before the app joins.
pub fn session_connector(
    collab: std::path::PathBuf,
    identity: std::path::PathBuf,
    app: &str,
    room: &str,
) -> ui::collaboration::HostSessionConnector {
    let (app, room) = (app.to_owned(), room.to_owned());
    ui::collaboration::HostSessionConnector::new(move |_route| async move {
        let failed = |error: std::io::Error| runtime_portal::NotAvailable::ConnectionFailed(error);
        crate::shell::launcher::register_in_process_launch(&app, &room);
        let mut stream = std::os::unix::net::UnixStream::connect(&collab).map_err(failed)?;
        launcher::services::write_collab_handoff(&mut stream, &app, &room).map_err(failed)?;
        stream.set_nonblocking(true).map_err(failed)?;
        let collaboration = tokio::net::UnixStream::from_std(stream).map_err(failed)?;
        let mut identity = std::os::unix::net::UnixStream::connect(&identity).map_err(failed)?;
        launcher::services::write_identity_handshake(&mut identity, &app, &room).map_err(failed)?;
        identity.set_nonblocking(true).map_err(failed)?;
        let identity = tokio::net::UnixStream::from_std(identity).map_err(failed)?;
        Ok((collaboration, identity))
    })
}

/// The in-process apps a host shares between the tiles it resolves.
pub type SharedInProcessApps = Rc<RefCell<InProcessApps>>;

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource::Resource as _;
    include!("in_process_files_clipboard_tests.rs");
    include!("in_process_publication_tests.rs");

    const ROOM: &str = "6a3f0c2e-0f1b-4c5d-9e8f-123456789abc";
    const TAB: u64 = 41;

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

    #[test]
    fn hosted_terminal_missing_ush_reports_startup_failure_and_refuses_input() {
        let profile = tempfile::tempdir().unwrap();
        let mut apps = InProcessApps::at(profile.path(), "test-person").unwrap();
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
        let mut apps = InProcessApps::at(profile.path(), "test-person").unwrap();
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
            .open(terminal::definition().app_id, ROOM)
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
        let mut apps = InProcessApps::at(profile.path(), "test-person").unwrap();
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
                .open(file_explorer::definition().app_id, ROOM)
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
        let mut apps = InProcessApps::at(profile.path(), "test-person").unwrap();
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
            let records = apps.owners.open(notes::definition().app_id, ROOM).unwrap();
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
        inputs.mount_apps_in_process(InProcessApps::at(profile.path(), "test-person").unwrap());

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
                text: "moor at the east quay\n".into(),
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
            editor.wait_until(ui::time::Duration::from_secs(10), |editor| {
                editor
                    .inspection_json()
                    .is_ok_and(|inspection| inspection.contains("harbour-plan.txt"))
            }),
            "the Editor tab opened the file it was opened on"
        );
    }

    /// A Notes tab shown as an app of its own runs what its launch queued:
    /// a Markdown file it is opened on becomes a note in the tab's notebook.
    #[test]
    fn a_notes_tab_runs_its_launch_and_keeps_the_note_it_opened() {
        let profile = tempfile::tempdir().unwrap();
        let mut apps = InProcessApps::at(profile.path(), "test-person").unwrap();
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

        let records = apps.owners.open(notes::definition().app_id, ROOM).unwrap();
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

    /// Frames of `shown` until `done`, bounded.
    fn until(shown: &mut [&mut Shown], what: &str, mut done: impl FnMut() -> bool) {
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
        let mut apps = InProcessApps::at(&profile.join("apps"), &session.actor)
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
                provider_of(notes::definition().app_id).is_some()
                    && provider_of(terminal::definition().app_id).is_some()
            },
        );
        let notes_provider = provider_of(notes::definition().app_id).unwrap();
        let terminal_provider = provider_of(terminal::definition().app_id).unwrap();

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
                Err(tilo_actions::remote::ActionHostError::InvalidRequest { reason })
                    if reason.contains("list it again") && std::time::Instant::now() < deadline => {
                }
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
