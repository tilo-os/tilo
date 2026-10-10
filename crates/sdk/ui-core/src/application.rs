//! Checked application construction shared by hosts, independent of any runner.
use crate::application_view::{
    ApplicationFocusRestore, ApplicationLocalView, InstanceViewStore, SavedInstanceView,
    validate_identity,
};
use crate::launch::LaunchError;
use crate::permissions::{PermissionContext, PermissionScope, UserId};
use crate::record::{
    RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordReservation, RecordStore,
};
use crate::view::{ThawError, View};
use crate::wake::{WakeEvent, WakeHandle, WakeScope};
use crate::widget::{AppWidget, IconDescriptor};
use automerge::AutoCommit;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

mod host;
mod open_file;
pub mod registry;
pub use host::{ApplicationHost, NativeProcessHost, NativeProfile};
pub use open_file::{MAX_OPEN_FILE_BYTES, OpenFile, OpenFileReceipt};

pub type InstanceId = u64;
pub type ComponentFactory =
    fn(HostContext, LaunchTarget, RestoreInput) -> Result<AppComponent, LaunchError>;
pub struct ApplicationDefinition {
    pub app_id: &'static str,
    pub title: &'static str,
    pub icon: IconDescriptor,
    pub record_kind: RecordKind,
    pub required: Vec<PermissionScope>,
    pub optional: Vec<PermissionScope>,
    /// Media types (`type/subtype`, lowercase) the component opens when a host
    /// hands it a file ([`HostContext::take_open_file`]); [`ANY_MEDIA_TYPE`]
    /// for a component that opens every file (a hex editor).
    pub opens: &'static [&'static str],
    /// The app keeps one record per room, the room's own (a conversation
    /// everyone in the room shares), rather than a record per window: every
    /// window of the app in the room opens it, and the first creates it.
    pub room_record: bool,
    pub create: ComponentFactory,
}

/// The entry of [`ApplicationDefinition::opens`] that opens files of every
/// type.
pub const ANY_MEDIA_TYPE: &str = "*/*";
#[non_exhaustive]
pub enum LaunchTarget {
    New,
    /// Explicit creation of the one room-scoped package-primary document.
    PrimaryNew,
    Record {
        checkout: RecordCheckout,
        reveal: Option<tilo_actions::ActionAddress>,
    },
}
#[derive(Clone)]
pub enum RestoreInput {
    None,
    Local(SavedInstanceView),
}

/// Host-supplied storage, identity and authority for one instance, checked by
/// [`HostContext::prepare`] before any factory runs.
pub struct HostContextInputs {
    pub records: RecordStore,
    pub views: Rc<InstanceViewStore>,
    pub device_id: String,
    pub instance_id: InstanceId,
    pub permissions: Rc<RefCell<PermissionContext>>,
    pub author: UserId,
    pub action_context: tilo_actions::ActionContext,
}

/// Explicit identity, authority and storage. Apps receive no ambient profile resolver.
pub struct HostContext {
    /// The app's package id, matching both its definition and its record namespace.
    app_id: String,
    instance_id: InstanceId,
    device_id: String,
    records: RecordStore,
    views: Rc<InstanceViewStore>,
    record: RecordCheckout,
    reservation: Option<RecordReservation>,
    initialization: Option<crate::collaboration::native_initial::RecordInitialization>,
    permissions: Rc<RefCell<PermissionContext>>,
    author: UserId,
    action_context: tilo_actions::ActionContext,
    /// See [`Self::write_as_author`].
    writes_as_author: bool,
    wake_scope: WakeScope,
    construction: Rc<()>,
    lifetime_id: uuid::Uuid,
    queued_controls: Rc<RefCell<std::collections::VecDeque<tilo_actions::ControlInvocation>>>,
    search_requests: Rc<RefCell<std::collections::VecDeque<crate::search::SearchRequest>>>,
    control_domain: crate::ControlDomain,
    closing: Rc<Cell<bool>>,
    services: crate::host::HostServices,
    retirement: Option<crate::semantic_document_read::retirement::MountRetirement>,
    content: Option<tilo_content::ContentStore>,
    open_file: Option<OpenFile>,
    capture: Option<Capture>,
    settings: crate::storage::SettingsDocument,
    /// Where the app keeps the person's own records on this device, when
    /// the host keeps them ([`Self::provide_person_records`]).
    person: Option<crate::record::person::PersonRecords>,
    /// The file areas the host gave it ([`Self::provide_files`]); the
    /// person's own otherwise.
    files: Option<crate::storage::AppFiles>,
    /// The component the app built to show another record in this one's
    /// place, waiting for the host to swap it in ([`Reopener`]).
    replacement: Rc<RefCell<Option<PendingReplacement>>>,
    launch_arguments: Vec<String>,
    handoff: Option<PlaybackHandoff>,
    launch_options: Vec<LaunchOption>,
    private: bool,
    lent: Vec<Box<dyn std::any::Any>>,
}

/// Where playback stood on the device or app that handed the launch's media
/// over: the launch continues it from there. The media itself is what the
/// launch names (its argument or file); this is the transport and the name
/// it played under.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackHandoff {
    /// The position to continue from.
    pub position_ms: u64,
    /// Whether playback was running and continues running here.
    pub resume_playing: bool,
    /// What the media was called where it played, when the launch's
    /// argument alone does not say (a stream's URL).
    pub title: Option<String>,
}

/// One of an app's own command-line options a launch named: `--purpose
/// number` carries a value, a switch such as `--debug-hit-areas` none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOption {
    /// The option as written, with its dashes: `--purpose`.
    pub name: String,
    /// Its value; `None` for a switch.
    pub value: Option<String>,
}

/// What a host prepares an instance for when it renders a screenshot
/// (`--shots`, `--shots-dir`, `--responsive-matrix`) rather than a window
/// someone uses: the app shows a deterministic scene from fixtures and
/// starts nothing that reaches outside the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    scenario: Option<String>,
}

impl Capture {
    /// A capture of the named scenario; `None` is a plain `--shots`.
    pub fn new(scenario: Option<String>) -> Self {
        Self { scenario }
    }

    /// The scenario captured; `None` for a plain `--shots`.
    pub fn scenario(&self) -> Option<&str> {
        self.scenario.as_deref()
    }
}

/// Presentation owned by a runner. Authority comes only from the component context.
pub struct ComponentRunnerInputs {
    /// Effective snapshot, with scale metadata describing its baked values, as when
    /// each scale builder is applied once to a fresh theme. Runners preserve its
    /// palette, custom tokens, UI scale, and motion while replacing its text factor.
    pub theme: crate::Theme,
    /// The single owned renderer supplies the requested text factor and font locale.
    pub text_renderer: crate::TextRenderer,
}

impl ComponentRunnerInputs {
    /// Explicit native-runner discovery of the system's presentation settings
    /// and fonts. Factories and capture hosts should use supplied inputs instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_native_process() -> Self {
        crate::App::native_component_presentation()
    }
}

/// Explicit host preparation completed before invoking the application factory.
pub struct PreparedComponentInputs {
    pub context: HostContext,
    pub target: LaunchTarget,
    pub restore: RestoreInput,
    pub runner_inputs: ComponentRunnerInputs,
}

/// A component an app built to show another record in the place of the one
/// it shows now, and the launch that names that record (the path a person
/// chose to open). The host closes the current component and mounts this
/// one in the same window ([`AppComponent::take_replacement`]), then answers
/// the request ([`Self::into_parts`]).
pub struct ComponentReplacement {
    pub component: AppComponent,
    pub arguments: Vec<String>,
    outcome: ReplacementOutcome,
}

impl ComponentReplacement {
    /// `component`, opened on the launch naming `arguments`.
    pub fn new(component: AppComponent, arguments: Vec<String>) -> Self {
        Self {
            component,
            arguments,
            outcome: ReplacementOutcome::default(),
        }
    }

    /// The component to mount, and where the host answers whether the
    /// window shows it now.
    pub fn into_parts(self) -> (AppComponent, ReplacementOutcome) {
        (self.component, self.outcome)
    }
}

/// What became of a request to show another record in a window: nothing
/// yet, shown, or refused with the reason. The host answers before it
/// closes the asking component and polls the operation that asked, so that
/// operation can be answered from it; a refusal leaves the component open to
/// tell the person on its next frame.
#[derive(Clone, Default)]
pub struct ReplacementOutcome(Rc<RefCell<Option<Result<(), String>>>>);

impl ReplacementOutcome {
    /// The answer, once there is one.
    pub fn get(&self) -> Option<Result<(), String>> {
        self.0.borrow().clone()
    }

    /// Answer the request.
    pub fn answer(&self, result: Result<(), String>) {
        *self.0.borrow_mut() = Some(result);
    }

    /// Withdraw a request the host has not taken yet (the operation that
    /// asked was cancelled): the host then neither builds nor shows the
    /// record. Whether it was withdrawn in time.
    pub fn withdraw(&self) -> bool {
        let mut answer = self.0.borrow_mut();
        if answer.is_some() {
            return false;
        }
        *answer = Some(Err(WITHDRAWN.into()));
        true
    }

    fn withdrawn(&self) -> bool {
        matches!(self.0.borrow().as_ref(), Some(Err(reason)) if reason == WITHDRAWN)
    }
}

/// The answer of a request its asker withdrew.
const WITHDRAWN: &str = "the open was cancelled";

/// A replacement asked for, built now or when the host takes it.
enum PendingReplacement {
    Built(ComponentReplacement),
    /// Built when the host takes it, outside the frame that asked: the
    /// record store the request opens is busy while the asking component
    /// handles an event or an operation.
    Deferred(
        Box<dyn FnOnce() -> Result<ComponentReplacement, LaunchError>>,
        ReplacementOutcome,
    ),
}

impl PendingReplacement {
    fn take(self) -> Option<ComponentReplacement> {
        match self {
            Self::Built(replacement) if replacement.outcome.withdrawn() => {
                let (mut component, _) = replacement.into_parts();
                let _ = component.close();
                None
            }
            Self::Built(replacement) => Some(replacement),
            Self::Deferred(_, outcome) if outcome.withdrawn() => None,
            Self::Deferred(build, outcome) => match build() {
                Ok(mut replacement) => {
                    replacement.outcome = outcome;
                    Some(replacement)
                }
                Err(error) => {
                    outcome.answer(Err(error.to_string()));
                    None
                }
            },
        }
    }

    fn discard(self) {
        match self {
            Self::Built(replacement) => {
                let (mut component, outcome) = replacement.into_parts();
                let _ = component.close();
                outcome.answer(Err("another record was asked for instead".into()));
            }
            Self::Deferred(_, outcome) => {
                outcome.answer(Err("another record was asked for instead".into()));
            }
        }
    }
}

/// What a component needs to open another record of its app in its own
/// window: the same store, identity, authority and services as the record
/// it shows, for another record. Taken from the [`HostContext`] before the
/// factory consumes it ([`HostContext::reopener`]).
#[derive(Clone)]
pub struct Reopener {
    records: RecordStore,
    views: Rc<InstanceViewStore>,
    device_id: String,
    instance_id: InstanceId,
    permissions: Rc<RefCell<PermissionContext>>,
    author: UserId,
    action_context: tilo_actions::ActionContext,
    writes_as_author: bool,
    services: crate::host::HostServices,
    content: Option<tilo_content::ContentStore>,
    settings: crate::storage::SettingsDocument,
    person: Option<crate::record::person::PersonRecords>,
    files: Option<crate::storage::AppFiles>,
    private: bool,
    replacement: Rc<RefCell<Option<PendingReplacement>>>,
}

impl Reopener {
    /// A context for `record` of `definition`'s kind, opened when it exists
    /// and reserved when it does not, with the target the factory builds it
    /// for.
    ///
    /// # Errors
    /// Rejects foreign kinds, contested writers, corrupt records and denied
    /// authority.
    pub fn prepare(
        &self,
        definition: &ApplicationDefinition,
        record: RecordId,
    ) -> Result<(HostContext, LaunchTarget), LaunchError> {
        let checkout = RecordCheckout::new(record.clone(), definition.record_kind.clone());
        let (target, new_record) = match self.records.try_open_record(&checkout) {
            Ok(_) => (
                LaunchTarget::Record {
                    checkout,
                    reveal: None,
                },
                None,
            ),
            Err(crate::record::RecordOpenError::MissingRecord) => (LaunchTarget::New, Some(record)),
            Err(error) => return Err(LaunchError::Record(error)),
        };
        let mut context = HostContext::prepare_scoped(
            definition,
            HostContextInputs {
                records: self.records.clone(),
                views: Rc::clone(&self.views),
                device_id: self.device_id.clone(),
                instance_id: self.instance_id,
                permissions: Rc::clone(&self.permissions),
                author: self.author.clone(),
                action_context: self.action_context.clone(),
            },
            &target,
            new_record,
        )?;
        context.writes_as_author = self.writes_as_author;
        context.services = self.services.clone();
        context.content.clone_from(&self.content);
        context.settings = self.settings.clone();
        context.person.clone_from(&self.person);
        context.files.clone_from(&self.files);
        context.private = self.private;
        Ok((context, target))
    }

    /// Ask the host to show `component` in this one's place. A request the
    /// host has not taken yet is replaced, and its component closed.
    pub fn replace(&self, component: AppComponent, arguments: Vec<String>) -> ReplacementOutcome {
        let replacement = ComponentReplacement::new(component, arguments);
        let outcome = replacement.outcome.clone();
        self.request(PendingReplacement::Built(replacement));
        outcome
    }

    /// Ask the host to show the component `build` makes in this one's
    /// place, built when the host takes the request: what a component asks
    /// while it handles an event or an operation, when the store its own
    /// record lives in is busy.
    pub fn replace_with(
        &self,
        build: impl FnOnce(&Self) -> Result<ComponentReplacement, LaunchError> + 'static,
    ) -> ReplacementOutcome {
        let reopener = self.clone();
        let outcome = ReplacementOutcome::default();
        self.request(PendingReplacement::Deferred(
            Box::new(move || build(&reopener)),
            outcome.clone(),
        ));
        outcome
    }

    fn request(&self, pending: PendingReplacement) {
        let previous = self.replacement.borrow_mut().replace(pending);
        if let Some(previous) = previous {
            previous.discard();
        }
    }
}

impl HostContext {
    /// What the component needs to open another record of its app in its
    /// window later ([`Reopener`]).
    pub fn reopener(&self) -> Reopener {
        Reopener {
            records: self.records.clone(),
            views: Rc::clone(&self.views),
            device_id: self.device_id.clone(),
            instance_id: self.instance_id,
            permissions: Rc::clone(&self.permissions),
            author: self.author.clone(),
            action_context: self.action_context.clone(),
            writes_as_author: self.writes_as_author,
            services: self.services.clone(),
            content: self.content.clone(),
            settings: self.settings.clone(),
            person: self.person.clone(),
            files: self.files.clone(),
            private: self.private,
            replacement: Rc::clone(&self.replacement),
        }
    }

    fn retire_semantic_mount(&self) {
        if let Some(retirement) = &self.retirement {
            retirement.retire();
        }
    }
    pub(crate) fn bind_retirement(
        &mut self,
        retirement: crate::semantic_document_read::retirement::MountRetirement,
    ) -> Result<(), LaunchError> {
        if self.retirement.is_some() || self.closing.get() {
            return Err(LaunchError::BindingMismatch);
        }
        self.retirement = Some(retirement);
        Ok(())
    }
    pub(crate) fn require_invocation_provenance(&self) -> Result<(), crate::launch::LaunchError> {
        self.records
            .require_invocation_provenance(&self.record)
            .map_err(crate::launch::LaunchError::Operation)
    }
    /// Reserve New or open Existing before the factory, using only supplied identities.
    ///
    /// # Errors
    /// Rejects denied authority, invalid/foreign namespaces and unavailable records.
    pub fn prepare(
        definition: &ApplicationDefinition,
        inputs: HostContextInputs,
        target: &LaunchTarget,
    ) -> Result<Self, LaunchError> {
        Self::prepare_scoped(definition, inputs, target, None)
    }

    /// Reserve a host-named new record through the same checked construction
    /// path. Useful for a workspace's default document, whose identity must
    /// survive a restart even before the first local View has been saved.
    /// Existing records are never adopted or reinitialized by this operation.
    ///
    /// # Errors
    /// Rejects foreign namespaces, denied authority and occupied identities.
    pub fn prepare_new_record(
        definition: &ApplicationDefinition,
        inputs: HostContextInputs,
        record_id: RecordId,
    ) -> Result<Self, LaunchError> {
        Self::prepare_scoped(definition, inputs, &LaunchTarget::New, Some(record_id))
    }

    fn prepare_scoped(
        definition: &ApplicationDefinition,
        inputs: HostContextInputs,
        target: &LaunchTarget,
        new_record_id: Option<RecordId>,
    ) -> Result<Self, LaunchError> {
        let HostContextInputs {
            records,
            views,
            device_id,
            instance_id,
            permissions,
            author,
            action_context,
        } = inputs;
        validate_identity(definition.app_id).map_err(LaunchError::Restore)?;
        validate_identity(definition.record_kind.as_str()).map_err(LaunchError::Restore)?;
        validate_identity(&device_id).map_err(LaunchError::Restore)?;
        validate_identity(&author.0).map_err(LaunchError::Restore)?;
        let (profile, package, workspace) = records
            .canonical_namespace()
            .ok_or(LaunchError::BindingMismatch)?;
        if package != definition.app_id
            || !views.matches_namespace(&profile, &device_id, &workspace)
        {
            return Err(LaunchError::BindingMismatch);
        }
        for required in &definition.required {
            if !permissions.borrow().check(required) {
                return Err(LaunchError::CapabilityDenied(required.clone()));
            }
        }
        let (record, reservation) = match target {
            LaunchTarget::New => {
                let checkout = RecordCheckout::new(
                    new_record_id.unwrap_or_else(RecordId::minted),
                    definition.record_kind.clone(),
                );
                if RecordId::parse(checkout.id.as_str()).as_ref() != Some(&checkout.id) {
                    return Err(LaunchError::BindingMismatch);
                }
                let reservation = records
                    .reserve_record(&checkout)
                    .map_err(LaunchError::Record)?;
                (checkout, Some(reservation))
            }
            LaunchTarget::PrimaryNew => {
                if new_record_id.is_some() {
                    return Err(LaunchError::BindingMismatch);
                }
                let checkout = RecordCheckout::new(
                    RecordId::package_primary(),
                    definition.record_kind.clone(),
                );
                let reservation = records
                    .reserve_record(&checkout)
                    .map_err(LaunchError::Record)?;
                (checkout, Some(reservation))
            }
            LaunchTarget::Record { checkout, .. } => {
                if checkout.kind != definition.record_kind
                    || !checkout.id.is_package_primary()
                        && RecordId::parse(checkout.id.as_str()).as_ref() != Some(&checkout.id)
                {
                    return Err(LaunchError::BindingMismatch);
                }
                records
                    .try_open_record(checkout)
                    .map_err(LaunchError::Record)?;
                (checkout.clone(), None)
            }
        };
        let initialization = crate::collaboration::native_initial::take_initialization(
            &package,
            views.workspace_id(),
            &record,
        )
        .map_err(|error| LaunchError::Record(RecordOpenError::Initialization(error)))?;
        Ok(Self {
            app_id: package,
            instance_id,
            device_id,
            records,
            views,
            record,
            reservation,
            initialization,
            permissions,
            author,
            action_context,
            writes_as_author: false,
            wake_scope: WakeScope::new(),
            construction: Rc::new(()),
            lifetime_id: uuid::Uuid::new_v4(),
            queued_controls: Rc::default(),
            search_requests: Rc::default(),
            control_domain: crate::control_custody::ControlDomain::new(),
            closing: Rc::new(Cell::new(false)),
            services: crate::host::current(),
            retirement: None,
            content: None,
            open_file: None,
            capture: None,
            settings: crate::storage::SettingsDocument::Session,
            person: None,
            files: None,
            replacement: Rc::default(),
            launch_arguments: Vec::new(),
            handoff: None,
            launch_options: Vec::new(),
            private: false,
            lent: Vec::new(),
        })
    }
    /// Commit this instance's edits to its record under an actor of its
    /// author's own ([`crate::record::author_writer`]) rather than the
    /// record's writer, which every instance sharing the record's store
    /// writes as. For a host that mounts several authors on one store (a
    /// page's mounts of one profile and device), so history can tell whose
    /// edit each change is. The record's writer, which the store persists,
    /// is unchanged, as is everything written outside the instance.
    pub fn write_as_author(&mut self) {
        self.writes_as_author = true;
    }
    /// Lend the component the host's content store for this storage scope:
    /// opaque bytes addressed by hash that outlive the component and that
    /// other components of the same scope read too.
    pub fn provide_content(&mut self, store: tilo_content::ContentStore) {
        // The window's code finds it wherever it runs: an action applying,
        // an action's arguments being built.
        tilo_resource::content::register(&self.wake_scope.handle(), store.clone());
        // The app's surface is content too: whoever decides its changes
        // reads it by the hash its ops name.
        if let Some(bytes) = crate::app_surface::bytes_of(&self.app_id)
            && let Err(error) = store.put(bytes)
        {
            tracing::warn!(%error, "the app's surface could not be kept");
        }
        // The large parts of the actions this window records, and of those
        // it replays, live here too.
        let keep = store.clone();
        let read = store.clone();
        tilo_actions::keep_content_in(
            move |content: tilo_actions::ContentParts| {
                for (_, bytes) in content {
                    if let Err(error) = keep.put(&bytes) {
                        tracing::warn!(%error, "an action's content could not be kept");
                    }
                }
            },
            move |hash| read.read(hash),
        );
        self.content = Some(store);
    }
    /// The content store the host lent, if any.
    pub fn content(&self) -> Option<&tilo_content::ContentStore> {
        self.content.as_ref()
    }
    /// Hand the component a file to open. [`ApplicationDefinition::instantiate`]
    /// refuses the launch unless the definition opens its media type.
    pub fn provide_open_file(&mut self, file: OpenFile) {
        self.open_file = Some(file);
    }
    /// The file the host handed over to open, once.
    pub fn take_open_file(&mut self) -> Option<OpenFile> {
        self.open_file.take()
    }
    /// Prepare the instance for a screenshot of `capture` instead of a
    /// window someone uses.
    pub fn provide_capture(&mut self, capture: Capture) {
        self.capture = Some(capture);
    }
    /// The screenshot this instance is prepared for, if it is one.
    pub fn capture(&self) -> Option<&Capture> {
        self.capture.as_ref()
    }
    /// Hand the component what its launch named besides a file: the
    /// arguments of a command line or an "open with" (paths, URLs), in order.
    /// The host has already checked how many the component accepts.
    pub fn provide_launch_arguments(&mut self, arguments: Vec<String>) {
        self.launch_arguments = arguments;
    }
    /// The launch arguments the host handed over, once.
    pub fn take_launch_arguments(&mut self) -> Vec<String> {
        std::mem::take(&mut self.launch_arguments)
    }
    /// Hand the component the playback another device or app handed over
    /// with the media its launch names.
    pub fn provide_handoff(&mut self, handoff: PlaybackHandoff) {
        self.handoff = Some(handoff);
    }
    /// The playback handoff the host handed over, once.
    pub fn take_handoff(&mut self) -> Option<PlaybackHandoff> {
        self.handoff.take()
    }
    /// Lend the component a service of the host that mounts it in its own
    /// process: a connection to the session's process supervisor for a
    /// terminal the shell hosts. The component takes it by type; a host
    /// that lends nothing leaves the app on its own services.
    pub fn lend<T: std::any::Any>(&mut self, service: T) {
        self.lent.push(Box::new(service));
    }
    /// The services the host lent, once.
    pub fn take_lent(&mut self) -> Vec<Box<dyn std::any::Any>> {
        std::mem::take(&mut self.lent)
    }
    /// Hand the component the options of its own that its launch named
    /// (`--purpose number`). The host has already checked that the
    /// component takes each of them.
    pub fn provide_launch_options(&mut self, options: Vec<LaunchOption>) {
        self.launch_options = options;
    }
    /// The launch options the host handed over, once.
    pub fn take_launch_options(&mut self) -> Vec<LaunchOption> {
        std::mem::take(&mut self.launch_options)
    }
    /// Prepare the instance as a private window: its profile is thrown away
    /// with the process, and nothing it shows reaches the person's other
    /// windows, devices or collaborators.
    pub fn provide_private(&mut self) {
        self.private = true;
    }
    /// Whether the instance is a private window.
    pub fn is_private(&self) -> bool {
        self.private
    }
    /// Explicit owner-bound producer capability, available before factory work.
    pub fn control_domain(&self) -> crate::ControlDomain {
        self.control_domain
            .set_owner(crate::control_custody::ControlOwner::Component(
                self.callback_binding(),
            ));
        self.control_domain.clone()
    }
    pub fn record(&self) -> &RecordCheckout {
        &self.record
    }
    /// The application this context was prepared for.
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
    pub fn author(&self) -> &UserId {
        &self.author
    }
    /// A snapshot of the participant's permissions at this moment.
    pub fn permissions(&self) -> PermissionContext {
        self.permissions.borrow().clone()
    }
    /// The participant's permissions as they change while the component runs
    /// (a role change, a revoked share). Components that present themselves
    /// read-only hold this instead of deciding once at launch.
    pub fn live_permissions(&self) -> crate::permissions::LivePermissions {
        crate::permissions::LivePermissions::shared(self.permissions.clone())
    }
    pub fn action_context(&self) -> &tilo_actions::ActionContext {
        &self.action_context
    }
    pub fn wake_handle(&self) -> WakeHandle {
        self.wake_scope.handle()
    }
    /// The host services this component was prepared under — the ones
    /// [`crate::host::current`] resolved when the host prepared it.
    pub fn services(&self) -> &crate::host::HostServices {
        &self.services
    }
    /// This app's secrets, in the store its host provides: the system's
    /// (the Secret Service, the Keychain, the browser's wrapped store) or a
    /// host's own. Other apps' secrets are out of reach.
    pub fn secrets(&self) -> crate::secrets::SecretStore {
        self.services.secrets.store(&self.app_id)
    }
    /// This app's private file areas (data, config, cache, state): the
    /// per-user directories natively, the origin-private file system in a
    /// page. See [`crate::storage`] for what belongs where.
    pub fn files(&self) -> crate::storage::AppFiles {
        self.files
            .clone()
            .unwrap_or_else(|| crate::storage::AppFiles::for_app(&self.app_id))
    }
    /// This app's typed settings of type `T`, in the settings document the
    /// host gave it ([`Self::provide_settings`]); `T` declares how far they
    /// travel.
    pub fn settings<T: crate::storage::SettingsSchema>(&self) -> crate::storage::Settings<T> {
        self.storage().settings()
    }
    /// This app's files, secrets and settings in one handle, as framework
    /// views and surfaces receive them.
    pub fn storage(&self) -> crate::storage::AppStorage {
        crate::storage::AppStorage::new(&self.app_id, self.secrets(), self.settings.clone())
            .with_files(self.files())
    }
    /// Give the component this settings document instead of the person's
    /// session document: a test or capture profile's own.
    pub fn provide_settings(&mut self, document: crate::storage::SettingsDocument) {
        self.settings = document;
    }
    /// Give the component these file areas instead of the person's: a test
    /// or capture profile's own, so nothing it writes lands in theirs.
    pub fn provide_files(&mut self, files: crate::storage::AppFiles) {
        self.files = Some(files);
    }
    /// Give the component the person's own records of this app
    /// ([`crate::record::person`]), kept beside the record it shows.
    pub fn provide_person_records(&mut self, records: crate::record::person::PersonRecords) {
        self.person = Some(records);
    }
    /// This device, as the host names it: stable across launches of the app
    /// on it, distinct on each of the person's devices.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }
    /// The person's own records of this app, when the host keeps them.
    pub fn person_records(&self) -> Option<&crate::record::person::PersonRecords> {
        self.person.as_ref()
    }
    /// Bring the record up to the storage version its app reads, before it
    /// is read: `migrate` makes at most one deterministic housekeeping change
    /// (two devices migrating the same document make the same one) and
    /// answers whether it changed anything.
    /// # Errors
    /// The record is not open here, or the migration failed.
    pub fn migrate_record(
        &self,
        migrate: impl FnOnce(&mut AutoCommit) -> Result<bool, String>,
    ) -> Result<bool, String> {
        self.records
            .with(&self.record.id, migrate)
            .ok_or_else(|| "the record is not open".to_owned())?
    }
    /// Read/project an existing or initialized document. No untracked sync mutation.
    /// # Errors
    /// A reserved or missing record cannot be read.
    pub fn read_record<R>(
        &self,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Result<R, RecordOpenError> {
        self.records
            .read(&self.record.id, read)
            .ok_or(RecordOpenError::MissingRecord)
    }
    /// Merge another participant's copy of the record, as their changes
    /// arriving over sync are merged: validated first, attributed to them in
    /// history, and never on this participant's undo stack. Returns whether
    /// anything changed.
    /// # Errors
    /// The document is not a valid copy of this record, or it is not open.
    pub fn merge_peer_document(&self, document: Vec<u8>) -> Result<bool, String> {
        self.records.merge_peer_document(&self.record, document)
    }
    /// The unique owner transition from Reserved to Ready. Existing is never initialized.
    /// # Errors
    /// Refuses repeated initialization and preserves the reservation on schema/storage failure.
    pub fn initialize_new_record(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), RecordOpenError> {
        let reservation = self.reservation.as_mut().ok_or(RecordOpenError::Busy)?;
        reservation.initialize(initialize)?;
        self.reservation = None;
        Ok(())
    }
    /// [`Self::initialize_new_record`], then `seed` the record as this
    /// author on this device. The genesis `initialize` writes is the same on
    /// every device; the seed is what depends on who creates the record or
    /// where, and is nobody's undo step. See
    /// [`RecordReservation::initialize_seeded`].
    /// # Errors
    /// As [`Self::initialize_new_record`]; a failing seed fails it too.
    pub fn initialize_new_record_seeded(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        seed: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), RecordOpenError> {
        if let Some(initialization) = self.initialization.as_ref() {
            self.records.initialize_admitted_record(
                &self.record,
                initialization,
                initialize,
                seed,
            )?;
            self.initialization = None;
            return Ok(());
        }
        let reservation = self.reservation.as_mut().ok_or(RecordOpenError::Busy)?;
        reservation.initialize_seeded(initialize, seed)?;
        self.reservation = None;
        Ok(())
    }
    /// The host-derived original creation scope, absent on every ordinary restore.
    pub fn creation_workspace(
        &self,
    ) -> Option<&runtime_collab::committed_document::CreationWorkspace> {
        self.initialization
            .as_ref()
            .and_then(|initialization| initialization.workspace())
    }
    pub(crate) fn callback_binding(&self) -> crate::bound_dispatch::CallbackBinding {
        crate::bound_dispatch::CallbackBinding {
            lifetime: self.wake_scope.handle(),
            closing: self.closing.clone(),
            registry_owner: Rc::downgrade(&self.construction),
            lifetime_id: self.lifetime_id,
            queued_controls: self.queued_controls.clone(),
            search_requests: self.search_requests.clone(),
            control_domain: self.control_domain.downgrade(),
            instance_id: self.instance_id,
            app_id: self.app_id.clone(),
            records: self.records.clone(),
            checkout: self.record.clone(),
            permissions: self.permissions.clone(),
            author: self.author.clone(),
            operation_context: self.action_context.clone(),
            writes_as_author: self.writes_as_author,
        }
    }
    fn envelope(&self, view: View, application: Option<ApplicationLocalView>) -> SavedInstanceView {
        SavedInstanceView {
            version: 2,
            app_id: self.app_id.clone(),
            device_id: self.device_id.clone(),
            workspace_id: self.views.workspace_id().into(),
            instance_id: self.instance_id,
            record_id: self.record.id.as_str().into(),
            record_kind: self.record.kind.as_str().into(),
            view,
            application,
        }
    }
    fn validate_restore(&self, restore: &RestoreInput) -> Result<(), LaunchError> {
        match restore {
            RestoreInput::None => Ok(()),
            RestoreInput::Local(saved) => {
                saved.validate().map_err(LaunchError::Restore)?;
                if saved.app_id != self.app_id
                    || saved.device_id != self.device_id
                    || saved.workspace_id != self.views.workspace_id()
                    || saved.instance_id != self.instance_id
                    || saved.record_id != self.record.id.as_str()
                    || !self.record.kind.names(&saved.record_kind)
                {
                    return Err(LaunchError::BindingMismatch);
                }
                Ok(())
            }
        }
    }
}

impl Drop for HostContext {
    fn drop(&mut self) {
        self.retire_semantic_mount();
        self.control_domain.close();
        self.wake_scope.close();
    }
}
impl ApplicationDefinition {
    /// Whether the app keeps one record per room, its package primary,
    /// rather than a record per window ([`Self::room_record`]).
    pub fn keeps_room_record(&self) -> bool {
        self.room_record
    }

    /// The launch target of an instance with no saved record: a new record
    /// of its own, or the room's record (created on first use) for an app
    /// that [keeps one](Self::keeps_room_record).
    pub fn unsaved_target(&self, records: &RecordStore) -> LaunchTarget {
        if !self.keeps_room_record() {
            return LaunchTarget::New;
        }
        let checkout = RecordCheckout::package_primary(self.record_kind.clone());
        if records.try_open_record(&checkout).is_ok() {
            LaunchTarget::Record {
                checkout,
                reveal: None,
            }
        } else {
            LaunchTarget::PrimaryNew
        }
    }

    /// Whether the component opens files of `media_type` (parameters and case ignored).
    /// [`ANY_MEDIA_TYPE`] among [`Self::opens`] opens every file.
    pub fn opens_media_type(&self, media_type: &str) -> bool {
        open_file::media_type_essence(media_type).is_some_and(|essence| {
            self.opens
                .iter()
                .any(|opens| *opens == ANY_MEDIA_TYPE || *opens == essence.as_str())
        })
    }
    /// Validate, invoke the one factory under its lifetime, then restore presentation only.
    /// The factory projects through `HostContext::read_record`; no mutable `Widget::sync`
    /// or second history/autosave initialization follows its View restoration.
    ///
    /// # Errors
    /// Ready records survive any checked validation or setup failure through
    /// `ComponentSetup`'s strong owner; Reserved failures cancel without publication.
    pub fn instantiate(
        &self,
        context: HostContext,
        target: LaunchTarget,
        restore: RestoreInput,
    ) -> Result<AppComponent, LaunchError> {
        let records = context.records.clone();
        let record = context.record.clone();
        let result = self.instantiate_checked(context, target, restore);

        result.map_err(|source| {
            if records.is_ready(&record.id) {
                LaunchError::ComponentSetup {
                    source: Box::new(source),
                    record: Box::new(record),
                    recovery: crate::launch::LaunchRecovery::records(records),
                }
            } else {
                source
            }
        })
    }

    fn instantiate_checked(
        &self,
        context: HostContext,
        target: LaunchTarget,
        restore: RestoreInput,
    ) -> Result<AppComponent, LaunchError> {
        if context.app_id != self.app_id || context.record.kind != self.record_kind {
            return Err(LaunchError::BindingMismatch);
        }
        if matches!(target, LaunchTarget::New) && context.record.id.is_package_primary()
            || matches!(target, LaunchTarget::PrimaryNew) && !context.record.id.is_package_primary()
        {
            return Err(LaunchError::BindingMismatch);
        }
        match &target {
            LaunchTarget::New | LaunchTarget::PrimaryNew if context.reservation.is_none() => {
                return Err(LaunchError::BindingMismatch);
            }
            LaunchTarget::Record { checkout, .. }
                if context.reservation.is_some()
                    || checkout.id != context.record.id
                    || checkout.kind != context.record.kind =>
            {
                return Err(LaunchError::BindingMismatch);
            }
            _ => {}
        }
        for required in &self.required {
            if !context.permissions.borrow().check(required) {
                return Err(LaunchError::CapabilityDenied(required.clone()));
            }
        }
        if let Some(file) = &context.open_file
            && !self.opens_media_type(file.media_type())
        {
            return Err(LaunchError::Operation(format!(
                "{} does not open {} files ({})",
                self.title,
                file.media_type(),
                file.name()
            )));
        }
        context.validate_restore(&restore)?;
        let records = context.records.clone();
        let record = context.record.clone();
        let construction = context.construction.clone();
        let local = match &restore {
            RestoreInput::Local(saved) => Some(saved.clone()),
            _ => None,
        };
        let entry = context.wake_scope.enter();
        let result = (self.create)(context, target, restore).and_then(|mut component| {
            if !Rc::ptr_eq(&construction, &component.context.construction)
                || !records.is_ready(&record.id)
            {
                return Err(LaunchError::BindingMismatch);
            }
            if let Some(saved) = local {
                component
                    .restore_local_pair_owned(&saved)
                    .map_err(LaunchError::Restore)?;
            } else {
                // Retain both validated contributions atomically on fresh construction too.
                let (view, application) = component
                    .capture_local_pair_owned(&component.local_view)
                    .map_err(LaunchError::Restore)?;
                component.local_view = view;
                component.application_view = application;
            }
            component.prepared = true;
            Ok(component)
        });
        drop(entry);
        result
    }
}

/// How the window hosting a component treats its tree. Part of the
/// component, so every host that runs it applies the same behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentBehaviour {
    /// After a structural rebuild, focus the first focusable field again so a
    /// filter that rebuilds the tree keeps its caret.
    pub autofocus_on_structure_change: bool,
    /// Apply the tree's [`crate::widget::Widget::appearance_intent`] live:
    /// scale, theme, contrast and motion follow it each tick.
    pub appearance_reconcile: bool,
    /// Withhold the window from screenshots, recordings, screen sharing and
    /// remote viewers, as [`crate::App::set_capture_protected`] does: for
    /// apps that show secrets, such as a password manager.
    pub capture_protected: bool,
    /// What the window is to the session: an ordinary app window, or the
    /// session's on-screen input surface.
    pub window: WindowRole,
}

/// What a component's window is to the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowRole {
    /// An app window a person opens, arranges and closes.
    #[default]
    Ordinary,
    /// The session's on-screen input surface, such as an on-screen keyboard.
    /// Its window announces the component's own app id, by which the
    /// compositor (on the private display it launched the surface on) floats
    /// it outside the workspace and shows it only while a text field is
    /// focused. It is a session service started without a room, so it
    /// attaches to no workspace runtime. Its controls type into other apps'
    /// fields, so no headless drive may operate it.
    InputSurface,
}

impl ComponentBehaviour {
    /// Every behaviour off; a component spells out only what it turns on,
    /// `ComponentBehaviour { capture_protected: true, ..ComponentBehaviour::DEFAULT }`.
    pub const DEFAULT: Self = Self {
        autofocus_on_structure_change: false,
        appearance_reconcile: false,
        capture_protected: false,
        window: WindowRole::Ordinary,
    };
}

impl Default for ComponentBehaviour {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Real app content and its lifetime; hosts cannot extract an unbound root.
pub struct AppComponent {
    host_owner_wakes: Option<crate::owner_wake::OwnerWakeRegistration>,
    pub(crate) behaviour: ComponentBehaviour,
    pub(crate) history: Option<crate::history_mode::HistoryRegistration>,
    context: HostContext,
    pub(crate) root: Option<Box<dyn AppWidget>>,
    registry: Option<tilo_actions::ActionRegistry>,
    local_view: View,
    application_view: Option<ApplicationLocalView>,
    close_report: Option<ComponentCloseReport>,
    prepared: bool,
    pub(crate) invocations: Vec<tilo_actions::InvocationId>,
    pending_reports: Vec<ComponentCloseReport>,
    report_drained: bool,
    registry_owner: Rc<()>,
    providers: Rc<RefCell<ProviderPayloads>>,
    /// The record durability last delivered to the root, and when it was
    /// sampled.
    record_save: Option<(crate::record::RecordSaveStatus, crate::time::Instant)>,
    /// Whether a change to the delivered durability has yet to be presented.
    save_status_changed: bool,
}

/// How soon a component whose record is still being written samples its
/// durability again. The writer settles off the event loop and nothing wakes
/// the window when it does, so a "Saving…" would otherwise stay on screen,
/// and in the accessibility tree, until the next unrelated event.
const SAVING_RESAMPLE: crate::time::Duration = crate::time::Duration::from_millis(100);

impl AppComponent {
    /// The component the app built to show another record in this one's
    /// place, once: the host closes this component and mounts that one.
    pub fn take_replacement(&mut self) -> Option<ComponentReplacement> {
        let pending = self.context.replacement.borrow_mut().take()?;
        pending.take()
    }
    /// What this component's app would open another record with.
    #[cfg(test)]
    pub(crate) fn reopener(&self) -> Reopener {
        self.context.reopener()
    }
    /// Set how the hosting window treats this component's tree.
    pub const fn set_behaviour(&mut self, behaviour: ComponentBehaviour) {
        self.behaviour = behaviour;
    }
    /// The id of the application this component is an instance of.
    pub fn app_id(&self) -> &str {
        self.context.app_id()
    }
    /// A collaboration manager for this component's record, reached through
    /// the host's `connector` as an app the host launched: what a host
    /// showing the component inside its own process hands the App it runs
    /// it in, so the component publishes and is invoked through the session
    /// broker as a launched app is. `None` when the component's records have
    /// no route the broker could acknowledge.
    #[cfg(all(unix, feature = "broker-transport"))]
    pub fn collaborate_through(
        &self,
        connector: crate::collaboration::HostSessionConnector,
    ) -> Option<crate::collaboration::CollaborationManager> {
        let binding = self.callback_binding();
        let route = binding
            .broker_route()
            .or_else(|| binding.admission_route())?;
        // Built inside the component's wake scope, so the broker's traffic
        // wakes whoever the component wakes.
        let _entry = self.context.wake_scope.enter();
        Some(
            crate::collaboration::CollaborationManager::for_route_host_in_background(
                connector,
                route,
                Some(binding.checkout.kind.as_str().to_owned()),
            ),
        )
    }
    pub(crate) fn take_root_retirement(
        &mut self,
    ) -> Option<crate::semantic_document_read::retirement::RootRetirementPrelude> {
        self.context
            .retirement
            .as_mut()
            .and_then(|retirement| retirement.take_app())
    }
    /// Register presentation for this component's actual document custody. Every
    /// domain scope reads that record; external scopes retain their stated reason.
    /// This metadata carries no new authority and follows native/headless/tile mounts.
    /// # Errors
    /// Refuses missing, stale or foreign document custody and invalid scope metadata.
    pub fn register_document_history(
        &mut self,
        definition: &'static crate::history_mode::HistoryDefinition,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        self.register_history_of(definition, None)
    }
    /// Register presentation for this component's document, as
    /// [`Self::register_document_history`], and for the files of the mounted
    /// filesystem it shows: every checked-file scope reads the file `files`
    /// names.
    /// # Errors
    /// As [`Self::register_document_history`].
    pub fn register_document_and_file_history(
        &mut self,
        definition: &'static crate::history_mode::HistoryDefinition,
        files: crate::history_mode::checked_files::FileHistorySource,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        self.register_history_of(definition, Some(files))
    }
    fn register_history_of(
        &mut self,
        definition: &'static crate::history_mode::HistoryDefinition,
        files: Option<crate::history_mode::checked_files::FileHistorySource>,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        use crate::history_mode::{
            HistoryOwner, HistoryPresentation, HistoryRegistration, HistorySourceRegistration,
        };
        let owner = HistoryOwner::component(self.callback_binding());
        let sources = definition
            .scopes
            .iter()
            .map(|scope| match scope.presentation {
                HistoryPresentation::Domain { .. }
                | HistoryPresentation::DomainWithAssets { .. } => {
                    HistorySourceRegistration::owned(scope.key, definition.app_id, owner.clone())
                }
                HistoryPresentation::External { reason } => {
                    Ok(HistorySourceRegistration::Unversioned {
                        scope: scope.key,
                        reason,
                    })
                }
                HistoryPresentation::CheckedFiles { .. } => match &files {
                    Some(source) => Ok(HistorySourceRegistration::CheckedFiles {
                        scope: scope.key,
                        source: source.clone(),
                    }),
                    None => Err(crate::history_mode::HistoryRegistrationError::UnsupportedOwner),
                },
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.history = Some(HistoryRegistration::new(definition, sources)?);
        Ok(())
    }

    /// Preserve every recovery owner while a runner still has custody of this component.
    pub(crate) fn setup_error(&mut self, source: LaunchError) -> LaunchError {
        let binding = self.callback_binding();
        self.close();
        LaunchError::ComponentSetup {
            source: Box::new(source),
            record: Box::new(binding.checkout),
            recovery: crate::launch::LaunchRecovery::new(
                Some(binding.records),
                self.take_close_reports(),
            ),
        }
    }

    pub(crate) fn callback_binding(&self) -> crate::bound_dispatch::CallbackBinding {
        self.context.callback_binding()
    }

    /// The only action-facing loan of component root and registry. Captured
    /// lifetime, registry owner and canonical identity are checked before lending.
    pub(crate) fn with_live_component<R>(
        &mut self,
        captured: &crate::bound_dispatch::CallbackBinding,
        callback: impl FnOnce(
            &crate::bound_dispatch::CallbackBinding,
            &mut dyn AppWidget,
            &tilo_actions::ActionRegistry,
        ) -> R,
    ) -> Result<R, crate::bound_dispatch::BoundDispatchError> {
        let _entry = captured.enter()?;
        let current = self.callback_binding();
        if !captured.same_component(&current) {
            return Err(crate::bound_dispatch::BoundDispatchError::ClosedLifetime);
        }
        let root = self
            .root
            .as_deref_mut()
            .ok_or(crate::bound_dispatch::BoundDispatchError::ClosedLifetime)?;
        let registry = self
            .registry
            .as_ref()
            .ok_or(crate::bound_dispatch::BoundDispatchError::ClosedLifetime)?;
        Ok(callback(&current, root, registry))
    }

    /// Sample and deliver only after the caller has released its document loan.
    /// The same checked callback lifetime owns both the sample and the hook.
    pub(crate) fn project_record_save_status(&mut self) {
        let captured = self.callback_binding();
        let sampled = self
            .with_live_component(&captured, |binding, root, _registry| {
                if root.take_save_retry()
                    && let Err(error) = binding.records.try_flush_record(&binding.checkout.id)
                {
                    tracing::warn!(%error, "retrying a failed record save");
                }
                let status = binding.records.save_status(&binding.checkout.id).ok()?;
                root.record_save_status(status.clone());
                Some(status)
            })
            .ok()
            .flatten();
        let Some(status) = sampled else {
            return;
        };
        let changed = self
            .record_save
            .as_ref()
            .is_some_and(|(previous, _)| *previous != status);
        self.record_save = Some((status, crate::time::now()));
        if changed {
            // The root synced before it was handed the new state; a frame of
            // its own, with a sync of its own, shows it.
            self.save_status_changed = true;
            self.context.wake_handle().wake();
        }
    }

    /// When the window must sync again without input: shortly after the
    /// last sample while the record is still being written, so the moment
    /// it becomes durable is shown. The deadline is fixed to the sample: one
    /// computed from the time it is asked for would never come due.
    pub(crate) fn save_status_wake_deadline(&self) -> Option<crate::time::Instant> {
        match &self.record_save {
            Some((crate::record::RecordSaveStatus::Saving, sampled)) => {
                sampled.checked_add(SAVING_RESAMPLE)
            }
            _ => None,
        }
    }

    /// Whether the delivered durability changed since this was last asked,
    /// so the window presents the new state instead of keeping the old one.
    pub(crate) fn take_save_status_changed(&mut self) -> bool {
        std::mem::take(&mut self.save_status_changed)
    }

    /// The durability last delivered, as sampled.
    #[cfg(test)]
    pub(crate) fn sampled_record_save(&self) -> Option<&crate::record::RecordSaveStatus> {
        self.record_save.as_ref().map(|(status, _)| status)
    }

    /// Move the last durability sample back past its resample interval, so a
    /// test reaches the moment its deadline comes due without waiting for it.
    #[cfg(test)]
    pub(crate) fn age_record_save_sample(&mut self) {
        if let Some((_, sampled)) = &mut self.record_save {
            *sampled = sampled
                .checked_sub(SAVING_RESAMPLE * 2)
                .expect("the monotonic clock started before the sample's resample interval");
        }
    }

    pub fn new(
        context: HostContext,
        mut root: Box<dyn AppWidget>,
        registry: tilo_actions::ActionRegistry,
    ) -> Self {
        // The actual root already exists. Retire before its destructor if its
        // first SDK control-install callback unwinds, even before Self exists.
        let construction =
            crate::semantic_document_read::retirement::MountConstructionRetirement::new(
                context.retirement.as_ref(),
            );
        crate::widget::controls::install_control_custody(
            root.as_mut(),
            &context.control_domain(),
            crate::control_custody::CustodyAttachment::Initial,
        );
        context.control_domain().finish_initial_installation();
        let registry_owner = context.construction.clone();
        construction.disarm();
        Self {
            behaviour: ComponentBehaviour::default(),
            history: None,
            context,
            root: Some(root),
            registry: Some(registry),
            local_view: View::default(),
            application_view: None,
            close_report: None,
            prepared: false,
            invocations: vec![],
            pending_reports: vec![],
            report_drained: false,
            providers: Rc::new(RefCell::new(ProviderPayloads {
                owner: Rc::downgrade(&registry_owner),
                next: Some(0),
                payloads: HashMap::new(),
            })),
            registry_owner,
            record_save: None,
            save_status_changed: false,
            host_owner_wakes: None,
        }
    }

    /// Take the view and action decoders of newly loaded code in place of
    /// the running ones. The document stays: the new view reads it, and edits
    /// the new view encodes are decoded and applied by the new code.
    #[cfg(feature = "native")]
    pub(crate) fn install_reloaded(&mut self, root: crate::app::DevRoot) {
        self.root = Some(root.view);
        self.registry = Some(root.registry);
    }

    /// Return only weak proxies; payload ownership remains with this component.
    pub(crate) fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        let binding = self.callback_binding();
        let Ok(_entry) = binding.enter() else {
            return vec![];
        };
        let Some(root) = self.root.as_deref() else {
            return vec![];
        };
        crate::search::capability::search_providers(root)
            .into_iter()
            .filter_map(|provider| {
                // The host independently collects nested component boundaries.
                // A forwarded proxy must not acquire this outer component's owner.
                if provider.callback_binding().is_some() {
                    return None;
                }
                let name = provider.name().to_owned();
                let mut owned = self.providers.borrow_mut();
                let slot = owned.next?;
                owned.next = slot.checked_add(1);
                owned.payloads.insert(slot, Rc::from(provider));
                Some(Box::new(ComponentProvider {
                    name,
                    binding: binding.clone(),
                    owner: Rc::downgrade(&self.registry_owner),
                    store: Rc::downgrade(&self.providers),
                    slot,
                }) as Box<dyn crate::search::SearchProvider>)
            })
            .collect()
    }
    pub fn record(&self) -> &RecordCheckout {
        self.context.record()
    }
    /// # Errors
    /// A component can be mounted once and a closed lifetime cannot be reopened.
    pub fn mount(
        &mut self,
        sink: impl Fn(WakeEvent) + Send + Sync + 'static,
    ) -> Result<(), LaunchError> {
        if !self.prepared {
            return Err(LaunchError::BindingMismatch);
        }
        if let Some(root) = &mut self.root {
            crate::widget::controls::install_control_custody(
                root.as_mut(),
                &self.context.control_domain(),
                crate::control_custody::CustodyAttachment::Dynamic,
            );
        }
        self.context
            .wake_scope
            .bind(sink)
            .map_err(|e| LaunchError::Operation(e.to_string()))
    }
    /// Mount with exact-owner queue intake while retaining the original host sink.
    /// Registration owns no application callback and its sink retains only weak metadata.
    /// # Errors
    /// Refuses unprepared, already registered, stale or closed components.
    pub fn mount_with_owner_queue(
        &mut self,
        queue: &crate::owner_wake::OwnerWakeQueue,
        sink: impl Fn(WakeEvent) + Send + Sync + 'static,
    ) -> Result<(), LaunchError> {
        if !self.prepared || self.host_owner_wakes.is_some() {
            return Err(LaunchError::BindingMismatch);
        }
        let binding = self.callback_binding();
        self.with_live_component(&binding, |_, _, _| ())
            .map_err(|error| LaunchError::Operation(format!("{error:?}")))?;
        let (registration, intake) = queue
            .register(binding)
            .map_err(|error| LaunchError::Operation(format!("{error:?}")))?;
        self.host_owner_wakes = Some(registration);
        let result = self.mount(move |event| {
            intake.enqueue(event.clone());
            sink(event);
        });
        if result.is_err() {
            drop(self.host_owner_wakes.take());
        }
        result
    }

    /// # Errors
    /// Rejects closed components and invalid widget geometry.
    pub fn capture_local_view(&self) -> Result<View, ThawError> {
        if self.context.closing.get() {
            return Err(ThawError::InvalidView("component is closing".into()));
        }
        self.capture_local_pair_owned(&self.local_view)
            .map(|(view, _)| view)
    }
    fn capture_local_pair_owned(
        &self,
        inherited: &View,
    ) -> Result<(View, Option<ApplicationLocalView>), ThawError> {
        let _entry = self.context.wake_scope.enter();
        let root = self
            .root
            .as_deref()
            .ok_or_else(|| ThawError::InvalidView("component is closed".into()))?;
        let mut view = crate::view::capture_widget_view_bound(
            root,
            crate::view::capture_tree_focus_bound(root),
            inherited.viewport,
        );
        if view.open_resource.is_none() {
            view.open_resource.clone_from(&inherited.open_resource);
        }
        view.validate()?;
        let application = root.capture_application_view()?;
        if application
            .as_ref()
            .is_some_and(|local| local.app_id != self.context.app_id)
        {
            return Err(ThawError::BindingMismatch);
        }
        self.context
            .envelope(view.clone(), application.clone())
            .validate()?;
        Ok((view, application))
    }

    fn restore_local_pair_owned(&mut self, saved: &SavedInstanceView) -> Result<(), ThawError> {
        saved.validate()?;
        let _entry = self.context.wake_scope.enter();
        let root = self
            .root
            .as_deref_mut()
            .ok_or_else(|| ThawError::InvalidView("component is closed".into()))?;
        // Split root/context loans: preparation, generic restore and finalization share
        // exactly one immutable latest-record loan and one component lifetime.
        self.context
            .read_record(|doc| {
                let policy = if let Some(local) = &saved.application {
                    if local.app_id != self.context.app_id {
                        return Err(ThawError::BindingMismatch);
                    }
                    root.restore_application_view(local, doc)?
                } else {
                    ApplicationFocusRestore::Generic
                };
                let mut view = saved.view.clone();
                if policy == ApplicationFocusRestore::Application
                    && let Some(focus) = &mut view.focus
                {
                    focus.ephemeral.clear();
                }
                crate::view::restore_widget_view_bound(root, &view);
                if let Some(focus) = &view.focus {
                    crate::view::restore_tree_focus_bound(root, focus);
                }
                if saved.application.is_some() {
                    root.finish_application_view_restore()?;
                }
                Ok(())
            })
            .map_err(|error| ThawError::State(error.to_string()))??;
        let (view, application) = self.capture_local_pair_owned(&saved.view)?;
        self.local_view = view;
        self.application_view = application;
        Ok(())
    }
    /// Local hooks cover scroll, mode and focused ephemeral state. App-specific fields
    /// absent from `View` are unsupported; they must not be smuggled in as document bytes.
    /// # Errors
    /// Rejects malformed geometry or closed components before invoking widget hooks.
    pub fn restore_local_view(&mut self, view: &View) -> Result<(), ThawError> {
        if self.context.closing.get() {
            return Err(ThawError::InvalidView("component is closing".into()));
        }
        view.validate()?;
        let _entry = self.context.wake_scope.enter();
        let root = self
            .root
            .as_deref_mut()
            .ok_or_else(|| ThawError::InvalidView("component is closed".into()))?;
        crate::view::restore_widget_view_bound(root, view);
        if let Some(focus) = &view.focus {
            crate::view::restore_tree_focus_bound(root, focus);
        }
        self.local_view = view.clone();
        Ok(())
    }
    pub(crate) fn unsaved_documents(&self) -> Vec<crate::close_admission::UnsavedDocument> {
        let Some(root) = self.root.as_deref() else {
            return Vec::new();
        };
        let _entry = self.context.wake_scope.enter();
        self.context
            .read_record(|doc| crate::component_lifecycle::unsaved_documents(root, doc))
            .unwrap_or_default()
    }

    /// Persist the current document and view through their original owners.
    /// Unlike `close()`, refusal leaves callbacks, publication and the live tree
    /// intact. Retry captures the latest view and document, not a stale snapshot.
    pub(crate) fn checkpoint_for_close(&mut self) -> Vec<String> {
        let mut errors = crate::close_admission::retry_reports(&mut self.pending_reports);
        if self.context.closing.get() {
            return errors;
        }
        let _entry = self.context.wake_scope.enter();
        match self.capture_local_pair_owned(&self.local_view) {
            Ok((view, application)) => {
                let saved = self.context.envelope(view.clone(), application.clone());
                match self.context.views.save(&saved) {
                    Ok(()) => {
                        self.local_view = view;
                        self.application_view = application;
                    }
                    Err(error) => errors.push(error.to_string()),
                }
            }
            Err(error) => errors.push(error.to_string()),
        }
        if let Err(error) = self.context.records.try_flush() {
            errors.push(error);
        }
        if let Some(root) = self.root.as_deref_mut() {
            errors.extend(crate::component_lifecycle::checkpoint_components(root));
            self.pending_reports
                .extend(crate::component_lifecycle::take_component_close_reports(
                    root,
                ));
        }
        errors.extend(crate::close_admission::retry_reports(
            &mut self.pending_reports,
        ));
        errors
    }

    /// Finalization after a successful host checkpoint. No second fallible
    /// write is introduced after the host has committed to destroying the tree.
    pub(crate) fn finish_admitted_close(&mut self) {
        drop(self.host_owner_wakes.take());
        self.context.retire_semantic_mount();
        if self.close_report.is_some() {
            return;
        }
        self.context.closing.set(true);
        self.context.wake_scope.close();
        let _entry = self.context.wake_scope.enter();
        if let Some(root) = self.root.as_deref_mut() {
            self.pending_reports
                .extend(crate::component_lifecycle::finish_admitted_close(root));
            self.pending_reports
                .extend(crate::component_lifecycle::take_component_close_reports(
                    root,
                ));
        }
        let mut report = ComponentCloseReport {
            recovery: Some(
                self.context
                    .envelope(self.local_view.clone(), self.application_view.clone()),
            ),
            view_error: None,
            view_retry: None,
            record_error: None,
            record_retry: None,
            record_retry_target: None,
            teardown_error: None,
        };
        // Set the report before teardown: Drop must not fall back to a write.
        self.close_report = Some(report.clone());
        report.teardown_error = self.teardown().err();
        self.close_report = Some(report);
    }

    pub fn suspend(&mut self) -> ComponentCloseReport {
        self.close()
    }
    /// Save through the original store, retain failures, and permanently close this lifetime.
    pub fn close(&mut self) -> ComponentCloseReport {
        if let Some(report) = &self.close_report {
            return report.clone();
        }
        // The close borrow can unwind before cancellation starts (for example,
        // in View capture). Keep cleanup owned here even if the host catches it.
        CloseComponentOnExit(self).0.close_owned()
    }
    fn close_owned(&mut self) -> ComponentCloseReport {
        drop(self.host_owner_wakes.take());
        self.context.retire_semantic_mount();
        let mut report = ComponentCloseReport {
            recovery: None,
            view_error: None,
            view_retry: None,
            record_error: None,
            record_retry: None,
            record_retry_target: None,
            teardown_error: None,
        };
        self.context.control_domain.close();
        self.context.closing.set(true);
        match self.capture_local_pair_owned(&self.local_view) {
            Ok((view, application)) => {
                self.local_view = view.clone();
                self.application_view.clone_from(&application);
                report.recovery = Some(self.context.envelope(view, application));
            }
            Err(error) => {
                report.view_error = Some(error.to_string());
                report.recovery = Some(
                    self.context
                        .envelope(self.local_view.clone(), self.application_view.clone()),
                );
            }
        }
        self.context.wake_scope.close();
        let _entry = self.context.wake_scope.enter();
        if report.view_error.is_none()
            && let Some(saved) = &report.recovery
            && let Err(error) = self.context.views.save(saved)
        {
            report.view_error = Some(error.to_string());
            report.view_retry = Some(self.context.views.clone());
        }
        if let Err(error) = self.context.records.try_flush() {
            report.record_error = Some(error);
            report.record_retry = Some(self.context.records.clone());
        }
        // Parent View has already been captured. Each child still owns its live
        // tree and captures/closes exactly once before any parent root is dropped.
        if let Some(root) = self.root.as_deref_mut() {
            self.pending_reports
                .extend(crate::component_lifecycle::close_components(root));
            self.pending_reports
                .extend(crate::component_lifecycle::take_component_close_reports(
                    root,
                ));
        }
        self.close_report = Some(report.clone());
        if let Err(error) = self.teardown() {
            report.teardown_error = Some(error);
        }
        self.close_report = Some(report.clone());
        report
    }
    pub(crate) fn take_close_reports(&mut self) -> Vec<ComponentCloseReport> {
        let mut reports = std::mem::take(&mut self.pending_reports);
        // Transfer is inert ownership metadata: an open parent must not hide
        // recovery retained by a replaced nested tile. Never capture or close
        // the parent here, and never consult a root after teardown removed it.
        if let Some(root) = self.root.as_deref_mut() {
            let _entry = self.context.wake_scope.enter();
            reports.extend(crate::component_lifecycle::take_component_close_reports(
                root,
            ));
        }
        if !self.report_drained
            && let Some(report) = &self.close_report
        {
            reports.push(report.clone());
            self.report_drained = true;
        }
        reports
    }
    pub fn close_report(&self) -> Option<&ComponentCloseReport> {
        self.close_report.as_ref()
    }
    fn teardown(&mut self) -> Result<(), String> {
        drop(self.host_owner_wakes.take());
        self.context.retire_semantic_mount();
        self.context.control_domain.close();
        self.context.closing.set(true);
        self.context.wake_scope.close();
        let _entry = self.context.wake_scope.enter();
        // Ownership locals are declared after the closed route guard. If app
        // cancellation unwinds, every remaining capture drops before that guard.
        let mut owned = (
            self.root.take(),
            self.registry.take(),
            std::mem::take(&mut self.invocations),
            std::mem::take(&mut self.providers.borrow_mut().payloads)
                .into_values()
                .collect::<Vec<_>>(),
        );
        let result = if let Some(root) = owned.0.as_deref_mut() {
            let result = self
                .context
                .records
                .with_checked(&self.context.record.id, |doc| {
                    for invocation in &owned.2 {
                        root.poll_resource_operation(*invocation, true, doc, &automerge::ROOT);
                    }
                })
                .map_err(|error| error.to_string());
            crate::widget::unmount_native_views(root);
            result
        } else {
            Ok(())
        };
        drop_component_owned(owned.0, (owned.1, owned.2, owned.3));
        result
    }
}

struct ProviderPayloads {
    owner: std::rc::Weak<()>,
    next: Option<u64>,
    payloads: HashMap<u64, Rc<dyn crate::search::SearchProvider>>,
}
struct ComponentProvider {
    name: String,
    binding: crate::bound_dispatch::CallbackBinding,
    owner: std::rc::Weak<()>,
    store: std::rc::Weak<RefCell<ProviderPayloads>>,
    slot: u64,
}
impl ComponentProvider {
    fn invoke<R>(
        &self,
        callback: impl FnOnce(&dyn crate::search::SearchProvider) -> R,
    ) -> Option<R> {
        let _entry = self.binding.enter().ok()?;
        let store = self.store.upgrade()?;
        let provider = {
            let owned = store.try_borrow().ok()?;
            if !std::rc::Weak::ptr_eq(&self.owner, &owned.owner) {
                return None;
            }
            owned.payloads.get(&self.slot)?.clone()
        };
        Some(callback(provider.as_ref()))
    }
}
impl crate::search::SearchProvider for ComponentProvider {
    fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
        Some(self.binding.clone())
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn action_directory_generation(&self) -> Option<u64> {
        self.invoke(|provider| provider.action_directory_generation())
            .flatten()
    }

    fn action_directory_invocation_domain(&self) -> crate::widget::ActionDirectoryInvocationDomain {
        self.invoke(|provider| provider.action_directory_invocation_domain())
            .unwrap_or_default()
    }
    fn action_directory_cursor_is_current(&self, cursor: &crate::search::SearchCursor) -> bool {
        self.invoke(|provider| provider.action_directory_cursor_is_current(cursor))
            .unwrap_or(false)
    }
    fn search(&self, query: &str) -> Vec<crate::search::SearchResult> {
        self.invoke(|provider| provider.search(query))
            .unwrap_or_default()
    }
    fn search_limited(&self, query: &str, limit: usize) -> Vec<crate::search::SearchResult> {
        self.invoke(|provider| provider.search_limited(query, limit))
            .unwrap_or_default()
    }
    fn search_page(
        &self,
        query: &crate::search::SearchQuery,
        cursor: Option<&crate::search::SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> crate::search::SearchPage {
        self.invoke(|provider| provider.search_page(query, cursor, work_budget, result_limit))
            .unwrap_or_else(crate::search::SearchPage::empty)
    }
    fn poll_page(&self, token: &crate::search::PendingToken) -> crate::search::PagePoll {
        self.invoke(|provider| provider.poll_page(token))
            .unwrap_or_else(|| crate::search::PagePoll::Ready(crate::search::SearchPage::empty()))
    }
    fn facets(&self) -> Vec<crate::search::Facet> {
        self.invoke(|provider| provider.facets())
            .unwrap_or_default()
    }
    fn search_scopes(&self) -> Vec<crate::search::ScopeDecl> {
        self.invoke(|provider| provider.search_scopes())
            .unwrap_or_default()
    }
    fn resolve_action_resource(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::Resource> {
        self.invoke(|provider| provider.resolve_action_resource(address))
            .flatten()
            .and_then(|resource| crate::bound_dispatch::filter_resource(&self.binding, resource))
    }
}
impl Drop for ComponentProvider {
    fn drop(&mut self) {
        let Ok(_entry) = self.binding.enter() else {
            return;
        };
        let Some(store) = self.store.upgrade() else {
            return;
        };
        let payload = {
            let Ok(mut owned) = store.try_borrow_mut() else {
                return;
            };
            if !std::rc::Weak::ptr_eq(&self.owner, &owned.owner) {
                return;
            }
            owned.payloads.remove(&self.slot)
        };
        drop(payload);
    }
}
struct CloseComponentOnExit<'a>(&'a mut AppComponent);
impl Drop for CloseComponentOnExit<'_> {
    fn drop(&mut self) {
        let _ = self.0.teardown();
    }
}

impl Drop for AppComponent {
    fn drop(&mut self) {
        // Failed setup must not save a supplied bad View over the persisted envelope.
        let _ = self.teardown();
    }
}
#[derive(Clone)]
pub struct ComponentCloseReport {
    pub recovery: Option<SavedInstanceView>,
    pub view_error: Option<String>,
    pub view_retry: Option<Rc<InstanceViewStore>>,
    pub record_error: Option<String>,
    pub record_retry: Option<RecordStore>,
    /// Exact legacy owner for scoped close/retry; None keeps component close behavior.
    pub record_retry_target: Option<crate::RecordId>,
    /// Reserved for fallible lifecycle results. Arbitrary app panics propagate.
    pub teardown_error: Option<String>,
}
impl ComponentCloseReport {
    /// Retry the same dirty canonical owner, retaining it until durability succeeds.
    /// # Errors
    /// Returns the original owner's current persistence failure.
    pub fn retry_record(&mut self) -> Result<(), String> {
        if let Some(records) = &self.record_retry {
            match &self.record_retry_target {
                Some(id) => records.try_flush_record(id),
                None => records.try_flush(),
            }
            .inspect_err(|error| self.record_error = Some(error.clone()))?;
            self.record_retry = None;
            self.record_retry_target = None;
            self.record_error = None;
        }
        Ok(())
    }

    /// # Errors
    /// Retains the original store and envelope until the atomic save succeeds.
    pub fn retry_view(&mut self) -> Result<(), ThawError> {
        let Some(store) = &self.view_retry else {
            return Ok(());
        };
        let saved = self
            .recovery
            .as_ref()
            .ok_or_else(|| ThawError::InvalidView("missing recovery envelope".into()))?;
        store
            .save(saved)
            .inspect_err(|e| self.view_error = Some(e.to_string()))?;
        self.view_error = None;
        self.view_retry = None;
        Ok(())
    }
}
#[derive(Default)]
pub struct ApplicationCatalog {
    definitions: HashMap<&'static str, &'static ApplicationDefinition>,
}
impl ApplicationCatalog {
    pub fn new() -> Self {
        Self::default()
    }
    /// # Errors
    /// Duplicate package identities never replace the previously registered factory.
    pub fn register(
        &mut self,
        definition: &'static ApplicationDefinition,
    ) -> Result<(), LaunchError> {
        if self.definitions.contains_key(definition.app_id) {
            return Err(LaunchError::DuplicateApplication(definition.app_id.into()));
        }
        self.definitions.insert(definition.app_id, definition);
        Ok(())
    }
    /// # Errors
    /// Unknown applications remain unavailable; no placeholder factory is synthesized.
    pub fn resolve(&self, app_id: &str) -> Result<&'static ApplicationDefinition, LaunchError> {
        self.definitions
            .get(app_id)
            .copied()
            .ok_or_else(|| LaunchError::UnknownApplication(app_id.into()))
    }
}

// The tuple owns both captures before either Drop begins. Rust's field cleanup
// drops the registry even if the root panics, then propagates the original unwind.
// The caller keeps its closed-scope guard alive throughout this cleanup.
fn drop_component_owned<R, C>(root: R, registry: C) {
    drop((root, registry));
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;

    // Name, previously captured handle activity, and activity captured during Drop.
    type DropObservation = (&'static str, bool, bool);

    struct DropProbe {
        name: &'static str,
        panics: bool,
        handle: WakeHandle,
        observations: Rc<RefCell<Vec<DropObservation>>>,
    }
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.observations.borrow_mut().push((
                self.name,
                self.handle.is_active(),
                WakeHandle::new().is_active(),
            ));
            if self.panics {
                std::panic::panic_any(self.name);
            }
        }
    }
    fn assert_closed_cleanup(panicking: &'static str) {
        let mut scope = WakeScope::new();
        let observations = Rc::new(RefCell::new(Vec::new()));
        let root = DropProbe {
            name: "root",
            panics: panicking == "root",
            handle: scope.handle(),
            observations: observations.clone(),
        };
        let registry = DropProbe {
            name: "registry",
            panics: panicking == "registry",
            handle: scope.handle(),
            observations: observations.clone(),
        };
        scope.close();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _entry = scope.enter();
            drop_component_owned(root, registry);
        }));
        let payload =
            result.expect_err("owned cleanup must propagate the original destructor panic");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&panicking));
        assert_eq!(
            *observations.borrow(),
            vec![("root", false, false), ("registry", false, false)]
        );
    }
    #[test]
    fn panics_propagate_after_the_registry_drops_under_a_closed_scope() {
        // Case `root_panic_still_drops_registry_under_closed_scope_and_propagates`.
        {
            assert_closed_cleanup("root");
        }

        // Case `registry_panic_propagates_after_both_closed_scope_drops`.
        {
            assert_closed_cleanup("registry");
        }
    }
}

#[cfg(test)]
mod bound_lifetime_tests {
    use super::*;
    use crate::widget::{AppKind, Column, Widget};

    use taffy::{NodeId, TaffyTree};
    struct Root(Column);
    impl Widget for Root {
        fn sync(&mut self, doc: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
            self.0.sync(doc, path);
        }
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            theme: &crate::Theme,
            nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.0.build_layout(tree, theme, nodes)
        }
        fn apply_layout(
            &mut self,
            tree: &TaffyTree<()>,
            node: NodeId,
            origin: vello::kurbo::Point,
        ) {
            self.0.apply_layout(tree, node, origin);
        }
        fn draw(
            &mut self,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            text: &mut crate::TextRenderer,
            theme: &crate::Theme,
            presence: &crate::PresenceCtx,
        ) {
            self.0.draw(scene, transform, text, theme, presence);
        }
        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            ctx: &mut crate::widget::EventCtx<'_>,
        ) {
            self.0.handle_event(event, ctx);
        }
        fn name(&self) -> &'static str {
            "MountProbe"
        }
        fn get_rect(&self) -> vello::kurbo::Rect {
            self.0.get_rect()
        }
    }
    impl AppWidget for Root {
        fn app_kind(&self) -> AppKind {
            AppKind::Editor
        }
        fn app_title(&self) -> &'static str {
            "Mount probe"
        }
        fn app_icon(&self) -> IconDescriptor {
            IconDescriptor::Glyph("P")
        }
        fn permission_hint(&self) -> Option<AppKind> {
            None
        }
    }
    #[allow(clippy::needless_pass_by_value, reason = "component factory contract")]
    fn factory(
        mut context: HostContext,
        target: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, LaunchError> {
        if matches!(target, LaunchTarget::New) {
            context
                .initialize_new_record(|_| Ok(()))
                .map_err(LaunchError::Record)?;
        }
        Ok(AppComponent::new(
            context,
            Box::new(Root(Column::new())),
            tilo_actions::ActionRegistry::new(),
        ))
    }

    #[test]
    fn host_named_new_and_package_primary_keep_distinct_identity_contracts() {
        let temp = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(temp.path().into());
        let records = owners.open("dev.tilo.mount-probe", "A").unwrap();
        let views = Rc::new(InstanceViewStore::new(temp.path().into(), "D", "A").unwrap());
        let definition = ApplicationDefinition {
            app_id: "dev.tilo.mount-probe",
            title: "Mount probe",
            icon: IconDescriptor::Glyph("P"),
            record_kind: RecordKind::new("probe"),
            required: vec![],
            optional: vec![],
            opens: &[],
            room_record: false,
            create: factory,
        };
        let supplied = RecordId::for_tab(55);
        let prepare = |target, supplied| {
            HostContext::prepare_scoped(
                &definition,
                HostContextInputs {
                    records: records.clone(),
                    views: views.clone(),
                    device_id: "D".into(),
                    instance_id: 7,
                    permissions: Rc::new(RefCell::new(PermissionContext::permissive())),
                    author: UserId("probe-author".into()),
                    action_context: tilo_actions::ActionContext::default(),
                },
                target,
                supplied,
            )
        };

        let named = prepare(&LaunchTarget::New, Some(supplied.clone())).unwrap();
        assert_eq!(named.record.id, supplied);
        drop(named);

        assert!(matches!(
            prepare(&LaunchTarget::PrimaryNew, Some(RecordId::for_tab(56))),
            Err(LaunchError::BindingMismatch)
        ));
        let primary = prepare(&LaunchTarget::PrimaryNew, None).unwrap();
        assert!(primary.record.id.is_package_primary());
    }

    #[test]
    fn existing_package_primary_is_local_while_invalid_ordinary_records_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(temp.path().into());
        let records = owners.open("dev.tilo.mount-probe", "A").unwrap();
        let views = Rc::new(InstanceViewStore::new(temp.path().into(), "D", "A").unwrap());
        let definition = ApplicationDefinition {
            app_id: "dev.tilo.mount-probe",
            title: "Mount probe",
            icon: IconDescriptor::Glyph("P"),
            record_kind: RecordKind::new("probe"),
            required: vec![],
            optional: vec![],
            opens: &[],
            room_record: false,
            create: factory,
        };
        let prepare = |checkout| {
            HostContext::prepare(
                &definition,
                HostContextInputs {
                    records: records.clone(),
                    views: views.clone(),
                    device_id: "D".into(),
                    instance_id: 7,
                    permissions: Rc::new(RefCell::new(PermissionContext::permissive())),
                    author: UserId("probe-author".into()),
                    action_context: tilo_actions::ActionContext::default(),
                },
                &LaunchTarget::Record {
                    checkout,
                    reveal: None,
                },
            )
        };

        let malformed = RecordCheckout::new(
            RecordId::derived("invalid role", &RecordId::for_tab(55)),
            definition.record_kind.clone(),
        );
        assert!(matches!(
            prepare(malformed),
            Err(LaunchError::BindingMismatch)
        ));
        let foreign = RecordCheckout::new(RecordId::for_tab(56), definition.record_kind.clone());
        assert!(matches!(
            prepare(foreign),
            Err(LaunchError::Record(
                crate::record::RecordOpenError::MissingRecord
            ))
        ));

        let primary = RecordCheckout::package_primary(definition.record_kind.clone());
        records.create_record(&primary).unwrap();
        let context = prepare(primary).expect("an existing local package primary must reopen");
        assert!(context.record.id.is_package_primary());
    }

    #[test]
    fn component_lifetimes_refuse_stale_wakes_loans_and_busy_entries() {
        // Case `actual_component_reopen_refuses_queued_old_wakes_and_busy_store_entry`.
        {
            let temp = tempfile::tempdir().unwrap();
            let mut owners = crate::record::RecordOwners::new(temp.path().into());
            let records = owners.open("so.tilo.mount-probe", "A").unwrap();
            let views =
                Rc::new(InstanceViewStore::new(temp.path().to_path_buf(), "D", "A").unwrap());
            let definition = ApplicationDefinition {
                app_id: "so.tilo.mount-probe",
                title: "Mount probe",
                icon: IconDescriptor::Glyph("P"),
                record_kind: RecordKind::new("probe"),
                required: vec![],
                optional: vec![],
                opens: &[],
                room_record: false,
                create: factory,
            };
            let prepare = |target: &LaunchTarget| {
                HostContext::prepare(
                    &definition,
                    HostContextInputs {
                        records: records.clone(),
                        views: views.clone(),
                        device_id: "D".into(),
                        instance_id: 7,
                        permissions: Rc::new(RefCell::new(PermissionContext::permissive())),
                        author: UserId("probe-author".into()),
                        action_context: tilo_actions::ActionContext::default(),
                    },
                    target,
                )
                .unwrap()
            };
            let mut old = definition
                .instantiate(
                    prepare(&LaunchTarget::New),
                    LaunchTarget::New,
                    RestoreInput::None,
                )
                .unwrap();
            let binding = old.callback_binding();
            let target = LaunchTarget::Record {
                checkout: old.record().clone(),
                reveal: None,
            };
            let (tx, rx) = std::sync::mpsc::channel();
            old.mount(move |event| {
                tx.send(event).unwrap();
            })
            .unwrap();
            let local = binding
                .read_document(&AutoCommit::new(), |_, _| crate::LocalState::new(0))
                .unwrap();
            assert!(local.writer().try_set(1));
            let queued = rx.try_recv().unwrap();
            assert!(old.context.wake_scope.accepts(&queued));
            records
                .with(&old.record().id, |_| {
                    assert!(matches!(
                        binding
                            .with_document(&mut AutoCommit::new(), |_, _| panic!("busy entry ran")),
                        Err(crate::bound_dispatch::BoundDispatchError::Record(
                            RecordOpenError::Busy
                        ))
                    ));
                })
                .unwrap();
            old.close();
            let mut reopened = definition
                .instantiate(prepare(&target), target, RestoreInput::None)
                .unwrap();
            let (tx, new_rx) = std::sync::mpsc::channel();
            reopened
                .mount(move |event| {
                    tx.send(event).unwrap();
                })
                .unwrap();
            assert!(!old.context.wake_scope.accepts(&queued));
            assert!(!reopened.context.wake_scope.accepts(&queued));
            assert!(!local.writer().try_set(2));
            assert!(new_rx.try_recv().is_err());
            assert!(matches!(
                binding.with_document(&mut AutoCommit::new(), |_, _| panic!("closed entry ran")),
                Err(crate::bound_dispatch::BoundDispatchError::ClosedLifetime)
            ));
        }

        // Case `task4_component_loan_checks_captured_registry_and_lifetime_before_delivery`.
        {
            let temp = tempfile::tempdir().unwrap();
            let mut owners = crate::record::RecordOwners::new(temp.path().into());
            let records = owners.open("so.tilo.mount-probe", "A").unwrap();
            let views =
                Rc::new(InstanceViewStore::new(temp.path().to_path_buf(), "D", "A").unwrap());
            let definition = ApplicationDefinition {
                app_id: "so.tilo.mount-probe",
                title: "Mount probe",
                icon: IconDescriptor::Glyph("P"),
                record_kind: RecordKind::new("probe"),
                required: vec![],
                optional: vec![],
                opens: &[],
                room_record: false,
                create: factory,
            };
            let context = HostContext::prepare(
                &definition,
                HostContextInputs {
                    records,
                    views,
                    device_id: "D".into(),
                    instance_id: 7,
                    permissions: Rc::new(RefCell::new(PermissionContext::permissive())),
                    author: UserId("probe-author".into()),
                    action_context: tilo_actions::ActionContext::default(),
                },
                &LaunchTarget::New,
            )
            .unwrap();
            let mut component = definition
                .instantiate(context, LaunchTarget::New, RestoreInput::None)
                .unwrap();
            let binding = component.callback_binding();
            component
                .with_live_component(&binding, |admitted, root, _registry| {
                    assert_eq!(admitted.instance_id, 7);
                    assert_eq!(root.app_title(), "Mount probe");
                })
                .unwrap();
            let mut foreign = binding.clone();
            foreign.registry_owner = std::rc::Weak::new();
            assert!(matches!(
                component
                    .with_live_component(&foreign, |_, _, _| panic!("foreign registry delivered")),
                Err(crate::bound_dispatch::BoundDispatchError::ClosedLifetime)
            ));
            component.close();
            assert!(matches!(
                component
                    .with_live_component(&binding, |_, _, _| panic!("closed component delivered")),
                Err(crate::bound_dispatch::BoundDispatchError::ClosedLifetime)
            ));
        }
    }
}

#[cfg(test)]
mod portable_tests {
    use super::*;
    use crate::application_test_support as probe;
    #[test]
    fn portable_host_checks_namespaces_authority_and_persists_real_component() {
        let cache =
            crate::portable_storage::PortableStorage::new("profile", probe::DEVICE).unwrap();
        let records = crate::record::RecordOwners::portable(cache.clone())
            .open(probe::APP, probe::ROOM)
            .unwrap();
        let views = Rc::new(InstanceViewStore::portable(cache.clone(), probe::ROOM).unwrap());
        let definition = probe::definition(probe::probe_factory);
        let prepare = |views, permissions| {
            HostContext::prepare(
                &definition,
                HostContextInputs {
                    records: records.clone(),
                    views,
                    device_id: probe::DEVICE.into(),
                    instance_id: 7,
                    permissions,
                    author: probe::author(),
                    action_context: probe::provenance(),
                },
                &LaunchTarget::New,
            )
        };
        let foreign =
            crate::portable_storage::PortableStorage::new("foreign", probe::DEVICE).unwrap();
        assert!(matches!(
            prepare(
                Rc::new(InstanceViewStore::portable(foreign, probe::ROOM).unwrap()),
                probe::permissions()
            ),
            Err(LaunchError::BindingMismatch)
        ));
        let context = prepare(views.clone(), probe::permissions()).unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let checkout = component.record().clone();
        assert!(records.is_ready(&checkout.id));
        let report = component.close();
        assert!(report.record_error.is_some());
        assert!(report.view_error.is_some());
        for write in cache.pending_writes() {
            cache.acknowledge(&write, Ok(())).unwrap();
        }
        records.try_flush().unwrap();
        let saved = views.load(7).unwrap().unwrap();
        assert_eq!(saved.record_id, checkout.id.as_str());
        views.save(&saved).unwrap();
        let mut denied = probe::definition(probe::probe_factory);
        denied.required.push(probe::denied_scope());
        assert!(matches!(
            HostContext::prepare(
                &denied,
                HostContextInputs {
                    records,
                    views,
                    device_id: probe::DEVICE.into(),
                    instance_id: 8,
                    permissions: probe::permissions(),
                    author: probe::author(),
                    action_context: probe::provenance()
                },
                &LaunchTarget::New,
            ),
            Err(LaunchError::CapabilityDenied(_))
        ));
    }
}
