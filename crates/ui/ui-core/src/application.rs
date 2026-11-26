//! Checked application construction shared by hosts, independent of any runner.
use crate::application_view::{
    ApplicationFocusRestore, ApplicationLocalView, InstanceViewStore, SavedInstanceView,
    validate_identity,
};
use crate::launch_error::LaunchError;
use crate::resource::{
    DocumentCheckout, DocumentId, ResourceOpenError, ResourceReservation, ResourceStore,
    ResourceType,
};
use crate::view::{ThawError, View};
use crate::wake::{WakeEvent, WakeHandle, WakeScope};
use crate::widget::{AppWidget, IconDescriptor};
use automerge::AutoCommit;
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use tilo_ui_base::{
    permissions::MemberId, permissions::PermissionContext, permissions::PermissionScope,
    permissions::SharedPermissions,
};

mod component_inputs;
mod open_file;
pub mod registry;
mod runner;
mod surface;
pub use open_file::{MAX_OPEN_FILE_BYTES, OpenFile, OpenFileReceipt};
pub use runner::{ApplicationOwner, NativeProcess, NativeProfile, PERSONAL_WORKSPACE};
#[cfg(test)]
pub(crate) use runner::{follow_process_authority, manual_authority_watch};
pub use surface::prepare_private_surface;

pub use collab_types::history::InstanceId;

pub type ComponentFactory =
    fn(ProcessContext, LaunchTarget, RestoreInput) -> Result<AppComponent, LaunchError>;
pub struct ApplicationDefinition {
    /// The app's package id.
    pub app_id: Cow<'static, str>,
    /// The app's name, as the definition's runtime learns it.
    pub title: Cow<'static, str>,
    pub icon: IconDescriptor,
    pub resource_type: ResourceType,
    pub required: Vec<PermissionScope>,
    pub optional: Vec<PermissionScope>,
    /// Media types (`type/subtype`, lowercase) the component opens when a host hands it a file
    /// ([`ProcessContext::take_open_file`]); [`ANY_MEDIA_TYPE`] for a component that opens every file (a
    /// hex editor).
    pub opens: &'static [&'static str],
    /// The app keeps one record per workspace, the workspace (a conversation everyone in the workspace
    /// shares), rather than a record per window: every window of the app in the workspace opens it, and
    /// the first creates it.
    pub workspace_resource: bool,
    /// The app's window shows before the workspace's record is read: the record opens off the
    /// frame, and the component takes it in when it is here
    /// ([`ProcessContext::resource_opening`]).
    pub opens_before_resource: bool,
    pub create: ComponentFactory,
}

/// The entry of [`ApplicationDefinition::opens`] that opens files of every type.
pub const ANY_MEDIA_TYPE: &str = "*/*";

impl ApplicationDefinition {
    /// The app's package id, borrowed from the definition: `'static` for the definitions every host
    /// keeps for the life of the process.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// The app's name, borrowed from the definition.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
}
#[non_exhaustive]
pub enum LaunchTarget {
    New,
    /// Explicit creation of the one workspace-scoped package-primary document.
    PrimaryNew,
    Record {
        checkout: DocumentCheckout,
        reveal: Option<tilo_actions::ActionAddress>,
    },
}
#[derive(Clone)]
pub enum RestoreInput {
    None,
    Local(SavedInstanceView),
}

/// Host-supplied storage, identity and authority for one instance, checked by [`ProcessContext::prepare`]
/// before any factory runs.
pub struct ProcessContextInputs {
    pub records: ResourceStore,
    pub views: Rc<InstanceViewStore>,
    pub device_id: String,
    pub instance_id: InstanceId,
    pub permissions: SharedPermissions,
    /// The trusted local person, retained independently of later action attribution.
    pub author: MemberId,
    pub action_context: tilo_actions::ActionContext,
}

/// Explicit identity, authority and storage.
pub struct ProcessContext {
    /// The app's package id, matching both its definition and its record namespace.
    app_id: String,
    instance_id: InstanceId,
    device_id: String,
    records: ResourceStore,
    views: Rc<InstanceViewStore>,
    record: DocumentCheckout,
    reservation: Option<ResourceReservation>,
    permissions: SharedPermissions,
    author: MemberId,
    local_person: MemberId,
    action_context: tilo_actions::ActionContext,
    /// See [`Self::write_as_author`].
    writes_as_author: bool,
    wake_scope: WakeScope,
    construction: Rc<()>,
    lifetime_id: uuid::Uuid,
    queued_controls: Rc<RefCell<std::collections::VecDeque<tilo_actions::ControlInvocation>>>,
    search_requests:
        Rc<RefCell<std::collections::VecDeque<crate::widget::search_request::SearchRequest>>>,
    control_domain: tilo_ui_base::ControlDomain,
    closing: Rc<Cell<bool>>,
    services: crate::process::ProcessServices,
    models: crate::models::Models,
    provider_admin: crate::models::ProviderAdministration,
    retirement: Option<crate::semantic_document_read::retirement::MountRetirement>,
    content: Option<tilo_content::ContentStore>,
    open_file: Option<OpenFile>,
    capture: Option<Capture>,
    settings: crate::storage::SettingsDocument,
    /// Where the app opens the records every app of the workspace opens, when the host keeps them
    /// ([`Self::provide_workspace_resources`]).
    workspace: Option<crate::resource::workspace::WorkspaceResources>,
    /// The file areas the host gave it ([`Self::provide_files`]); the
    /// person otherwise.
    files: Option<crate::storage::AppFiles>,
    /// The component the app built to show another record in this one's place, waiting for the host to
    /// swap it in ([`Reopener`]).
    replacement: Rc<RefCell<Option<PendingReplacement>>>,
    launch_arguments: Vec<String>,
    /// The file or folder the window shows ([`WindowShows`]).
    shows: Rc<RefCell<Option<String>>>,
    handoff: Option<PlaybackHandoff>,
    launch_options: Vec<LaunchOption>,
    private: bool,
    embedded: bool,
    lent: Vec<Box<dyn std::any::Any>>,
}

/// Where playback stood on the device or app that handed the launch's media over: the launch continues
/// it from there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackHandoff {
    pub position_ms: u64,
    /// Whether playback was running and continues running here.
    pub resume_playing: bool,
    /// What the media was called where it played, when the launch's argument alone does not say (a
    /// stream's URL).
    pub title: Option<String>,
}

/// One of an app command-line options a launch named: `--purpose number` has a value, a switch such as
/// `--debug-hit-areas` none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchOption {
    /// The option as written, with its dashes: `--purpose`.
    pub name: String,
    /// Its value; `None` for a switch.
    pub value: Option<String>,
}

/// What a host prepares an instance for when it renders a screenshot (`--shots`, `--shots-dir`,
/// `--responsive-matrix`) rather than a window someone uses: the app shows a deterministic scene from
/// fixtures and starts nothing that reaches outside the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    scenario: Option<String>,
    driven: bool,
}

impl Capture {
    /// A capture of the named scenario; `None` is a plain `--shots`.
    pub fn new(scenario: Option<String>) -> Self {
        Self {
            scenario,
            driven: false,
        }
    }

    /// A headless drive of the named scenario (`--headless-drive` with `--scenario`): built from the
    /// scenario's fixtures as its capture is, and still starting nothing on its, but an action the
    /// drive invokes runs, as a person's click or an agent's call does.
    pub fn driven(scenario: Option<String>) -> Self {
        Self {
            scenario,
            driven: true,
        }
    }

    /// The scenario captured; `None` for a plain `--shots`.
    pub fn scenario(&self) -> Option<&str> {
        self.scenario.as_deref()
    }

    /// Whether a headless drive invokes the window's actions ([`Self::driven`]) rather than only
    /// photographing it.
    pub fn is_driven(&self) -> bool {
        self.driven
    }
}

/// Presentation owned by a runner.
pub struct ComponentRunnerInputs {
    /// Effective snapshot, with scale metadata describing its baked values, as when
    /// each scale builder is applied once to a fresh theme. Runners preserve its
    /// palette, custom tokens, UI scale, and motion while replacing its text factor.
    pub theme: tilo_ui_base::Theme,
    /// The single owned renderer supplies the requested text factor and font locale.
    pub text_renderer: tilo_ui_base::TextRenderer,
}

impl ComponentRunnerInputs {
    /// Explicit native-runner discovery of the system's presentation settings and fonts.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_native_process() -> Self {
        let accessibility = crate::process::environment().accessibility;
        let theme = tilo_ui_base::Theme::configured().with_a11y(
            &tilo_ui_base::a11y::A11ySettings {
                high_contrast: accessibility.high_contrast,
                reduce_motion: accessibility.reduce_motion,
                ..Default::default()
            },
            accessibility.text_scale.unwrap_or(1.0),
            accessibility.ui_scale.unwrap_or(1.0),
        );
        let mut text_renderer = tilo_ui_base::TextRenderer::new();
        text_renderer.set_text_scale(theme.text_scale);
        text_renderer.apply_typography(&theme.tokens.typography);
        Self {
            theme,
            text_renderer,
        }
    }
}

/// Explicit host preparation completed before invoking the application factory.
pub struct PreparedComponentInputs {
    pub context: ProcessContext,
    pub target: LaunchTarget,
    pub restore: RestoreInput,
    pub runner_inputs: ComponentRunnerInputs,
}

/// A component an app built to show another record in the place of the one it shows now, and the launch
/// that names that record (the path a person chose to open).
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

    /// The component to mount, and where the host answers whether the window shows it now.
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

    /// Withdraw a request the host has not taken yet (the operation that asked was cancelled): the host
    /// then neither builds nor shows the record.
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

const WITHDRAWN: &str = "the open was cancelled";

/// A replacement asked for, built now or when the host takes it.
enum PendingReplacement {
    Built(ComponentReplacement),
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

/// What a component needs to open another record of its app in its window: the same store, identity,
/// authority and services as the record it shows, for another record.
#[derive(Clone)]
pub struct Reopener {
    local_person: MemberId,
    records: ResourceStore,
    views: Rc<InstanceViewStore>,
    device_id: String,
    instance_id: InstanceId,
    permissions: SharedPermissions,
    author: MemberId,
    action_context: tilo_actions::ActionContext,
    writes_as_author: bool,
    services: crate::process::ProcessServices,
    content: Option<tilo_content::ContentStore>,
    settings: crate::storage::SettingsDocument,
    workspace: Option<crate::resource::workspace::WorkspaceResources>,
    files: Option<crate::storage::AppFiles>,
    private: bool,
    embedded: bool,
    replacement: Rc<RefCell<Option<PendingReplacement>>>,
    /// The window this reopener opens in place of: what serves its image stores serves the window
    /// opened in its place too.
    window: WakeHandle,
}

impl Reopener {
    /// A context for `record` of `definition`'s kind, opened when it exists and reserved when it does
    /// not, with the target the factory builds it for.
    pub fn prepare(
        &self,
        definition: &ApplicationDefinition,
        record: DocumentId,
    ) -> Result<(ProcessContext, LaunchTarget), LaunchError> {
        let checkout = DocumentCheckout::new(record.clone(), definition.resource_type.clone());
        let (target, new_resource) = match self.records.try_open_resource(&checkout) {
            Ok(_) => (
                LaunchTarget::Record {
                    checkout,
                    reveal: None,
                },
                None,
            ),
            // The workspace's own record, the first time it is opened: a window that opened with
            // it on its way makes it once the workspace answered it holds none.
            Err(crate::resource::ResourceOpenError::MissingResource)
                if record.is_package_primary() =>
            {
                (LaunchTarget::PrimaryNew, None)
            }
            Err(crate::resource::ResourceOpenError::MissingResource) => {
                (LaunchTarget::New, Some(record))
            }
            Err(error) => return Err(LaunchError::Record(error)),
        };
        let mut context = ProcessContext::prepare_scoped(
            definition,
            ProcessContextInputs {
                records: self.records.clone(),
                views: Rc::clone(&self.views),
                device_id: self.device_id.clone(),
                instance_id: self.instance_id,
                permissions: self.permissions.clone(),
                author: self.author.clone(),
                action_context: self.action_context.clone(),
            },
            &target,
            new_resource,
        )?;
        context.local_person = self.local_person.clone();
        context.writes_as_author = self.writes_as_author;
        context.services = self.services.clone();
        context.content.clone_from(&self.content);
        context.settings = self.settings.clone();
        context.workspace.clone_from(&self.workspace);
        context.files.clone_from(&self.files);
        context.private = self.private;
        context.embedded = self.embedded;
        tilo_ui_base::image_store::ImageStore::serve_successor(
            &self.window,
            &context.wake_handle(),
        );
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

    /// Ask the host to show the component `build` makes in this one's place, built when the host takes
    /// the request: what a component asks while it handles an event or an operation, when the store its
    /// own record lives in is busy.
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

impl ProcessContext {
    /// What the component needs to open another record of its app in its window later ([`Reopener`]).
    pub fn reopener(&self) -> Reopener {
        Reopener {
            local_person: self.local_person.clone(),
            records: self.records.clone(),
            views: Rc::clone(&self.views),
            device_id: self.device_id.clone(),
            instance_id: self.instance_id,
            permissions: self.permissions.clone(),
            author: self.author.clone(),
            action_context: self.action_context.clone(),
            writes_as_author: self.writes_as_author,
            services: self.services.clone(),
            content: self.content.clone(),
            settings: self.settings.clone(),
            workspace: self.workspace.clone(),
            files: self.files.clone(),
            private: self.private,
            embedded: self.embedded,
            replacement: Rc::clone(&self.replacement),
            window: self.wake_handle(),
        }
    }

    fn retire_semantic_mount(&self) {
        if let Some(retirement) = &self.retirement {
            retirement.retire();
        }
    }
    #[cfg(test)]
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
    /// Reserve New or open Existing before the factory, using only supplied identities.
    pub fn prepare(
        definition: &ApplicationDefinition,
        inputs: ProcessContextInputs,
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
    pub fn prepare_new_resource(
        definition: &ApplicationDefinition,
        inputs: ProcessContextInputs,
        document_id: DocumentId,
    ) -> Result<Self, LaunchError> {
        Self::prepare_scoped(definition, inputs, &LaunchTarget::New, Some(document_id))
    }

    fn prepare_scoped(
        definition: &ApplicationDefinition,
        inputs: ProcessContextInputs,
        target: &LaunchTarget,
        new_document_id: Option<DocumentId>,
    ) -> Result<Self, LaunchError> {
        let ProcessContextInputs {
            records,
            views,
            device_id,
            instance_id,
            permissions,
            author,
            action_context,
        } = inputs;
        validate_identity(definition.app_id()).map_err(LaunchError::Restore)?;
        validate_identity(definition.resource_type.as_str()).map_err(LaunchError::Restore)?;
        validate_identity(&device_id).map_err(LaunchError::Restore)?;
        validate_identity(author.id()).map_err(LaunchError::Restore)?;
        let (package, workspace) = records.app_and_home();
        if package != definition.app_id() || !views.matches(&package, &device_id, &workspace) {
            return Err(mismatch("the record store and the views are kept apart"));
        }
        for required in &definition.required {
            if !permissions.borrow().check(required) {
                return Err(LaunchError::CapabilityDenied(required.clone()));
            }
        }
        let (record, reservation) = match target {
            LaunchTarget::New => {
                let checkout = DocumentCheckout::new(
                    new_document_id.unwrap_or_else(DocumentId::minted),
                    definition.resource_type.clone(),
                );
                if DocumentId::parse(checkout.id.as_str()).as_ref() != Some(&checkout.id) {
                    return Err(mismatch("a new record id that does not read back"));
                }
                let reservation = records
                    .reserve_resource(&checkout)
                    .map_err(LaunchError::Record)?;
                (checkout, Some(reservation))
            }
            LaunchTarget::PrimaryNew => {
                if new_document_id.is_some() {
                    return Err(mismatch("a package primary given another id"));
                }
                let checkout = DocumentCheckout::new(
                    DocumentId::package_primary(),
                    definition.resource_type.clone(),
                );
                let reservation = records
                    .reserve_resource(&checkout)
                    .map_err(LaunchError::Record)?;
                (checkout, Some(reservation))
            }
            LaunchTarget::Record { checkout, .. } => {
                // The app's own kind, or one of the kinds it declares (a document every app of
                // the workspace opens, which a host opens as the record it is): no other app's.
                let declared = crate::app_surface::app_of_kind(checkout.kind.as_str())
                    .is_some_and(|app| app == definition.app_id());
                if (!declared && checkout.kind != definition.resource_type)
                    || !checkout.id.is_package_primary()
                        && DocumentId::parse(checkout.id.as_str()).as_ref() != Some(&checkout.id)
                {
                    return Err(mismatch("a record of another kind, or a malformed id"));
                }
                if !records.is_opening(&checkout.id) {
                    records
                        .try_open_resource(checkout)
                        .map_err(LaunchError::Record)?;
                }
                (checkout.clone(), None)
            }
        };
        let wake_scope = WakeScope::new();
        records.wake_on_arrival(wake_scope.handle());
        Ok(Self {
            app_id: package,
            instance_id,
            device_id,
            records,
            views,
            record,
            reservation,
            permissions,
            local_person: author.clone(),
            author,
            action_context,
            writes_as_author: false,
            wake_scope,
            construction: Rc::new(()),
            lifetime_id: uuid::Uuid::new_v4(),
            queued_controls: Rc::default(),
            search_requests: Rc::default(),
            control_domain: tilo_ui_base::control_domain::ControlDomain::new(),
            closing: Rc::new(Cell::new(false)),
            services: crate::process::current(),
            models: crate::models::Models::default(),
            provider_admin: crate::models::ProviderAdministration::default(),
            retirement: None,
            content: None,
            open_file: None,
            capture: None,
            settings: crate::storage::SettingsDocument::Session,
            workspace: None,
            files: None,
            replacement: Rc::default(),
            launch_arguments: Vec::new(),
            shows: Rc::default(),
            handoff: None,
            launch_options: Vec::new(),
            private: false,
            embedded: false,
            lent: Vec::new(),
        })
    }
    /// Commit this instance's edits to its record under an actor of its author
    /// ([`crate::resource::author_writer`]) rather than the record's writer, which every instance sharing
    /// the record's store writes as.
    pub fn write_as_author(&mut self) {
        self.writes_as_author = true;
    }
    /// Lend the component the host's content store for this storage scope: opaque bytes addressed by
    /// hash that outlive the component and that other components of the same scope read too.
    pub fn provide_content(&mut self, store: tilo_content::ContentStore) {
        tilo_resource::content::register(&self.wake_scope.handle(), store.clone());
        if let Some(bytes) = crate::app_surface::bytes_of(&self.app_id)
            && let Err(error) = store.put_public(bytes)
        {
            tracing::warn!(%error, "the app's surface could not be kept");
        }
        self.content = Some(store);
    }
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
    /// Prepare the instance for a screenshot of `capture` instead of a window someone uses.
    pub fn provide_capture(&mut self, capture: Capture) {
        self.capture = Some(capture);
    }
    /// The screenshot this instance is prepared for, if it is one.
    pub fn capture(&self) -> Option<&Capture> {
        self.capture.as_ref()
    }
    /// Hand the component what its launch named besides a file: the arguments of a command line or an
    /// "open with" (paths, URLs), in order.
    pub fn provide_launch_arguments(&mut self, arguments: Vec<String>) {
        self.launch_arguments = arguments;
    }
    /// Where the window says which file or folder it shows, so it opens on that again after a restart.
    #[must_use]
    pub fn window_shows(&self) -> crate::application_view::WindowShows {
        crate::application_view::WindowShows {
            shown: Rc::clone(&self.shows),
            views: Rc::clone(&self.views),
            unsaved: self.envelope(View::default(), None),
        }
    }
    /// The launch arguments the host handed over, once.
    pub fn take_launch_arguments(&mut self) -> Vec<String> {
        std::mem::take(&mut self.launch_arguments)
    }
    /// Hand the component the playback another device or app handed over with the media its launch
    /// names.
    pub fn provide_handoff(&mut self, handoff: PlaybackHandoff) {
        self.handoff = Some(handoff);
    }
    /// The playback handoff the host handed over, once.
    pub fn take_handoff(&mut self) -> Option<PlaybackHandoff> {
        self.handoff.take()
    }
    /// Lend the component a service of the host that mounts it in its process: a connection to the
    /// session's process supervisor for a terminal the shell hosts.
    pub fn lend<T: std::any::Any>(&mut self, service: T) {
        self.lent.push(Box::new(service));
    }
    /// Lend the component a service already boxed: what a component that showed before its record hands
    /// the one that shows it (`T` stays the service type).
    pub fn lend_boxed(&mut self, service: Box<dyn std::any::Any>) {
        self.lent.push(service);
    }
    /// The record this context opened, while it is still opening off the frame
    /// ([`ResourceStore::begin_opening`]): the component shows before it, and takes it in.
    pub fn resource_opening(&self) -> Option<crate::resource::ResourceOpening> {
        self.records
            .is_opening(&self.record.id)
            .then(|| self.records.opening(&self.record))
    }
    /// The services the host lent, once.
    pub fn take_lent(&mut self) -> Vec<Box<dyn std::any::Any>> {
        std::mem::take(&mut self.lent)
    }
    /// Hand the component the options of it that its launch named (`--purpose number`).
    pub fn provide_launch_options(&mut self, options: Vec<LaunchOption>) {
        self.launch_options = options;
    }
    /// The launch options the host handed over, once.
    pub fn take_launch_options(&mut self) -> Vec<LaunchOption> {
        std::mem::take(&mut self.launch_options)
    }
    /// Prepare the instance as a private window: its profile is thrown away with the process, and
    /// nothing it shows reaches the person's other windows, devices or members.
    pub fn provide_private(&mut self) {
        self.private = true;
    }
    /// Whether the instance is a private window.
    pub fn is_private(&self) -> bool {
        self.private
    }
    /// Whether the instance is a view another app's window embeds.
    pub fn is_embedded(&self) -> bool {
        self.embedded
    }
    /// Explicit owner-bound producer capability, available before factory work.
    pub fn control_domain(&self) -> tilo_ui_base::ControlDomain {
        self.control_domain
            .set_owner(tilo_ui_base::control_domain::ControlOwner::Component(
                std::rc::Rc::new(self.callback_binding()),
            ));
        self.control_domain.clone()
    }
    pub fn record(&self) -> &DocumentCheckout {
        &self.record
    }
    /// The store the record is kept in, with the app's other records of the same workspace: what an app
    /// holding records beside its (the parts of a chain) opens them in, so they sync on the same
    /// routes.
    pub fn records(&self) -> &ResourceStore {
        &self.records
    }
    /// The application this context was prepared for.
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
    pub fn author(&self) -> &MemberId {
        &self.author
    }
    /// The immutable local person supplied at host preparation, not the author currently attributed to
    /// an action.
    pub fn local_person(&self) -> &MemberId {
        &self.local_person
    }
    /// A snapshot of the member's permissions at this moment.
    pub fn permissions(&self) -> PermissionContext {
        self.permissions.borrow().clone()
    }
    /// The member's permissions as they change while the component runs (a role change, a revoked
    /// share).
    pub fn restrict_to_reading(&mut self) {
        let reading = self.permissions.borrow().read_only();
        self.permissions = tilo_ui_base::permissions::SharedPermissions::new(reading);
    }
    pub fn live_permissions(&self) -> tilo_ui_base::permissions::LivePermissions {
        tilo_ui_base::permissions::LivePermissions::shared(self.permissions.clone())
    }
    pub fn action_context(&self) -> &tilo_actions::ActionContext {
        &self.action_context
    }
    pub fn wake_handle(&self) -> WakeHandle {
        self.wake_scope.handle()
    }
    /// The host services this component was prepared under — the ones [`crate::process::current`] resolved
    /// when the host prepared it.
    pub fn services(&self) -> &crate::process::ProcessServices {
        &self.services
    }
    /// This app's secrets, in the store its host provides: the system's (the Secret Service, the
    /// Keychain, the browser's wrapped store) or a host.
    pub fn secrets(&self) -> crate::secrets::SecretStore {
        self.services.secrets.store(&self.app_id)
    }
    /// Bind the model capability authorized by this host's authenticated session.
    pub fn provide_models(&mut self, models: crate::models::Models) {
        self.lent
            .retain(|service| !service.is::<crate::models::Models>());
        self.lend(models.clone());
        self.models = models;
    }
    /// Loan separately authorized provider administration to this component only.
    pub fn provide_provider_admin(&mut self, handle: crate::models::ProviderAdministration) {
        self.lent
            .retain(|service| !service.is::<crate::models::ProviderAdministration>());
        self.lend(handle.clone());
        self.provider_admin = handle;
    }

    /// This component's administration authority, unavailable unless explicitly loaned.
    pub fn provider_admin(&self) -> crate::models::ProviderAdministration {
        self.provider_admin.clone()
    }

    /// Asynchronous model access, unavailable unless the host explicitly supplied it.
    pub fn models(&self) -> crate::models::Models {
        self.models.clone()
    }
    /// This app's private file areas (data, config, cache, state): the per-user directories natively,
    /// the origin-private file system in a page.
    pub fn files(&self) -> crate::storage::AppFiles {
        self.files
            .clone()
            .unwrap_or_else(|| crate::storage::AppFiles::for_app(&self.app_id))
    }
    /// This app's typed settings of type `T`, in the settings document the host gave it
    /// ([`Self::provide_settings`]); `T` declares how far they travel.
    pub fn settings<T: settings_model::SettingsSchema>(&self) -> crate::storage::Settings<T> {
        self.storage().settings()
    }
    /// This app's files, secrets and settings in one handle, as framework views and surfaces receive
    /// them.
    pub fn storage(&self) -> crate::storage::AppStorage {
        crate::storage::AppStorage::new(&self.app_id, self.secrets(), self.settings.clone())
            .with_files(self.files())
    }
    /// Give the component this settings document instead of the person's session document: a test or
    /// capture profile.
    pub fn provide_settings(&mut self, document: crate::storage::SettingsDocument) {
        self.settings = document;
    }
    /// Give the component these file areas instead of the person's: a test or capture profile, so
    /// nothing it writes reaches theirs.
    pub fn provide_files(&mut self, files: crate::storage::AppFiles) {
        self.files = Some(files);
    }
    /// Give the component the records every app of its workspace opens ([`crate::resource::workspace`]),
    /// kept beside the record it shows.
    pub fn provide_workspace_resources(
        &mut self,
        records: crate::resource::workspace::WorkspaceResources,
    ) {
        self.workspace = Some(records);
    }
    /// The records every app of the workspace opens, when the host keeps them.
    pub fn workspace_resources(&self) -> Option<&crate::resource::workspace::WorkspaceResources> {
        self.workspace.as_ref()
    }
    /// This device, as the host names it: stable across launches of the app on it, distinct on each of
    /// the person's devices.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }
    /// Bring the record up to the storage version its app reads, before it
    /// is read: `migrate` makes at most one deterministic housekeeping change
    /// (two devices migrating the same document make the same one) and
    /// answers whether it changed anything.
    pub fn migrate_resource(
        &self,
        migrate: impl FnOnce(&mut AutoCommit) -> Result<bool, String>,
    ) -> Result<bool, String> {
        self.records.migrate(&self.record.id, migrate)
    }
    /// Read/project an existing or initialized document. No untracked sync mutation.
    /// # Errors
    /// A reserved or missing record cannot be read.
    pub fn read_resource<R>(
        &self,
        read: impl FnOnce(&AutoCommit) -> R,
    ) -> Result<R, ResourceOpenError> {
        self.records
            .read(&self.record.id, read)
            .ok_or(ResourceOpenError::MissingResource)
    }
    /// Merge another member's copy of the record, as their changes
    /// arriving over sync are merged: validated first, attributed to them in
    /// history, and never on this member's undo stack. Returns whether
    /// anything changed.
    pub fn merge_peer_document(&self, document: Vec<u8>) -> Result<bool, String> {
        self.records.merge_peer_document(&self.record, document)
    }
    /// The unique owner transition from Reserved to Ready. Existing is never initialized.
    /// # Errors
    /// Refuses repeated initialization and preserves the reservation on schema/storage failure.
    pub fn initialize_new_resource(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), ResourceOpenError> {
        let by = tilo_ui_base::permissions::Acting::member(self.author.clone());
        let reservation = self.reservation.as_mut().ok_or(ResourceOpenError::Busy)?;
        reservation.initialize_by(initialize, &by)?;
        self.reservation = None;
        Ok(())
    }
    /// [`Self::initialize_new_resource`], then `seed` the record as this author on this device.
    pub fn initialize_new_resource_seeded(
        &mut self,
        initialize: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
        seed: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> Result<(), ResourceOpenError> {
        let by = tilo_ui_base::permissions::Acting::member(self.author.clone());
        let reservation = self.reservation.as_mut().ok_or(ResourceOpenError::Busy)?;
        reservation.initialize_seeded_by(initialize, seed, &by)?;
        self.reservation = None;
        Ok(())
    }
    fn envelope(&self, view: View, application: Option<ApplicationLocalView>) -> SavedInstanceView {
        SavedInstanceView {
            version: SavedInstanceView::VERSION,
            app_id: self.app_id.clone(),
            device_id: self.device_id.clone(),
            workspace_id: self.views.workspace_id().into(),
            instance_id: self.instance_id,
            document_id: self.record.id.as_str().into(),
            resource_type: self.record.kind.as_str().into(),
            view,
            application,
            shows: self.shows.borrow().clone(),
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
                    || saved.document_id != self.record.id.as_str()
                    || self.record.kind.as_str() != saved.resource_type
                {
                    return Err(mismatch(
                        "a saved view of another device, workspace or record",
                    ));
                }
                Ok(())
            }
        }
    }
}

impl Drop for ProcessContext {
    fn drop(&mut self) {
        self.retire_semantic_mount();
        self.control_domain.close();
        self.wake_scope.close();
    }
}
impl ApplicationDefinition {
    /// Whether the app keeps one record per workspace, its package primary, rather than a record per
    /// window ([`Self::workspace_resource`]).
    pub fn keeps_workspace_resource(&self) -> bool {
        self.workspace_resource
    }

    /// The launch target of an instance with no saved record: a new record of it, or the workspace's
    /// record (created on first use) for an app that [keeps one](Self::keeps_workspace_resource).
    pub fn unsaved_target(
        &self,
        records: &ResourceStore,
    ) -> Result<LaunchTarget, ResourceOpenError> {
        if !self.keeps_workspace_resource() {
            return Ok(LaunchTarget::New);
        }
        let checkout = DocumentCheckout::package_primary(self.resource_type.clone());
        match records.try_open_resource(&checkout) {
            Ok(_) => Ok(LaunchTarget::Record {
                checkout,
                reveal: None,
            }),
            Err(ResourceOpenError::MissingResource) => Ok(LaunchTarget::PrimaryNew),
            Err(error) => Err(error),
        }
    }

    /// Whether the component opens files of `media_type` (parameters and case ignored).
    pub fn opens_media_type(&self, media_type: &str) -> bool {
        open_file::media_type_essence(media_type).is_some_and(|essence| {
            // Every app opens a recording of it records, to replay it;
            // its factory refuses one it cannot replay.
            essence == tilo_content::RECORDING.mime
                || self
                    .opens
                    .iter()
                    .any(|opens| *opens == ANY_MEDIA_TYPE || *opens == essence.as_str())
        })
    }
    /// Validate, invoke the one factory under its lifetime, then restore presentation only.
    pub fn instantiate(
        &self,
        context: ProcessContext,
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
                    recovery: crate::launch_error::LaunchRecovery::records(records),
                }
            } else {
                source
            }
        })
    }

    fn instantiate_checked(
        &self,
        context: ProcessContext,
        target: LaunchTarget,
        restore: RestoreInput,
    ) -> Result<AppComponent, LaunchError> {
        if context.app_id != self.app_id
            || context.record.kind != self.resource_type
                && crate::app_surface::app_of_kind(context.record.kind.as_str())
                    .is_none_or(|app| app != self.app_id())
        {
            return Err(mismatch("a context of another app or record kind"));
        }
        if matches!(target, LaunchTarget::New) && context.record.id.is_package_primary()
            || matches!(target, LaunchTarget::PrimaryNew) && !context.record.id.is_package_primary()
        {
            return Err(mismatch(
                "a target and record that disagree on the package primary",
            ));
        }
        match &target {
            LaunchTarget::New | LaunchTarget::PrimaryNew if context.reservation.is_none() => {
                return Err(mismatch("a new record with nothing reserved"));
            }
            LaunchTarget::Record { checkout, .. }
                if context.reservation.is_some()
                    || checkout.id != context.record.id
                    || checkout.kind != context.record.kind =>
            {
                return Err(mismatch(
                    "an existing record with a reservation, or another record",
                ));
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
        if let RestoreInput::Local(saved) = &restore
            && context.shows.borrow().is_none()
        {
            context.shows.borrow_mut().clone_from(&saved.shows);
        }
        let shows_existing = matches!(target, LaunchTarget::Record { .. });
        let records = context.records.clone();
        let record = context.record.clone();
        let construction = context.construction.clone();
        let local = match &restore {
            RestoreInput::Local(saved) => Some(saved.clone()),
            _ => None,
        };
        let entry = context.wake_scope.enter();
        let result = (self.create)(context, target, restore).and_then(|mut component| {
            if !Rc::ptr_eq(&construction, &component.context.construction) {
                return Err(mismatch("a component of another construction"));
            }
            if !(records.is_ready(&record.id) || records.is_opening(&record.id)) {
                // Which record, and what the store says of it: a launch refused here otherwise
                // names nothing to look for.
                tracing::warn!(
                    record = record.id.as_str(),
                    status = ?records.save_status(&record.id),
                    "the window's record is neither open nor opening"
                );
                return Err(mismatch("its record neither open nor opening"));
            }
            if let Some(saved) = local.filter(|_| !records.is_opening(&record.id)) {
                component
                    .restore_local_pair_owned(&saved)
                    .map_err(LaunchError::Restore)?;
            } else {
                // Retain both validated contributions atomically on fresh construction too.
                let (view, application) = component
                    .capture_local_pair_owned(&component.local_view)
                    .map_err(LaunchError::Restore)?;
                // The window remembers the record it shows from the start, not only once it
                // closes: a session that ends without closing it (a crash, power lost) brings it
                // back on this record rather than on a new, empty one.
                if shows_existing
                    && let Err(error) = component.context.views.save(
                        &component
                            .context
                            .envelope(view.clone(), application.clone()),
                    )
                {
                    tracing::warn!(%error, "the window's record is not remembered");
                }
                component.local_view = view;
                component.application_view = application;
            }
            component.prepared = true;
            tilo_ui_base::image_store::window_built(
                &component.context.wake_handle(),
                component.content(),
            );
            Ok(component)
        });
        drop(entry);
        result
    }
}

/// How the window hosting a component treats its tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentBehaviour {
    /// After a structural rebuild, focus the first focusable field again so a filter that rebuilds the
    /// tree keeps its caret.
    pub autofocus_on_structure_change: bool,
    /// Apply the tree's [`tilo_ui_base::widget::Widget::appearance_intent`] live: scale, theme, contrast and
    /// motion follow it each tick.
    pub appearance_reconcile: bool,
    /// Withhold the window from screenshots, recordings, screen sharing and remote viewers, as
    /// [`crate::App::set_capture_protected`] does: for apps that show secrets, such as a password
    /// manager.
    pub capture_protected: bool,
    /// What the window is to the session: an ordinary app window, the session's on-screen input
    /// surface, or a sheet over the shell.
    pub window: WindowRole,
}

/// What a component's window is to the session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowRole {
    /// An app window a person opens, arranges and closes.
    #[default]
    Ordinary,
    /// The session's on-screen input surface, such as an on-screen keyboard.
    InputSurface,
    /// A sheet the shell shows over the whole screen (the power menu).
    Sheet,
}

impl ComponentBehaviour {
    /// Every behaviour off; a component spells out only what it turns on, `ComponentBehaviour {
    /// capture_protected: true, ..ComponentBehaviour::DEFAULT }`.
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
    context: ProcessContext,
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
    /// The record durability last delivered to the root.
    record_save: Option<crate::resource::ResourceSaveStatus>,
    /// Whether a change to the delivered durability has yet to be presented.
    save_status_changed: bool,
    /// What the record's store calls, from its thread, when the writes a "Saving…" waits on have
    /// resolved: this component's wake, so the window syncs and samples the record's durability again
    /// then.
    save_wake: Option<crate::resource::ReadyWake>,
}

impl AppComponent {
    /// The settings document the app was given; a capture stores the look it draws in there.
    pub fn settings_document(&self) -> crate::storage::SettingsDocument {
        self.context.settings.clone()
    }

    /// The component the app built to show another record in this one's place, once: the host closes
    /// this component and mounts that one.
    pub fn take_replacement(&mut self) -> Option<ComponentReplacement> {
        let pending = self.context.replacement.borrow_mut().take();
        if let Some(pending) = pending {
            return pending.take();
        }
        let _entry = self.context.wake_scope.enter();
        self.root.as_deref_mut()?.take_component_replacement()
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
    /// Whether the app declares its windows withheld from everything that leaves the device
    /// ([`ComponentBehaviour::capture_protected`]).
    #[must_use]
    pub const fn capture_protected(&self) -> bool {
        self.behaviour.capture_protected
    }
    /// The id of the application this component is an instance of.
    pub fn app_id(&self) -> &str {
        self.context.app_id()
    }
    /// Whether the component was prepared as a private window ([`ProcessContext::provide_private`]).
    pub fn is_private(&self) -> bool {
        self.context.is_private()
    }
    pub(crate) fn take_root_retirement(
        &mut self,
    ) -> Option<crate::semantic_document_read::retirement::RootRetirementPrelude> {
        self.context
            .retirement
            .as_mut()
            .and_then(|retirement| retirement.take_app())
    }
    /// Register presentation for this component's actual document ownership. Every
    /// domain scope reads that record; external scopes retain their stated reason.
    /// This metadata has no new authority and follows native/headless/tile mounts.
    /// # Errors
    /// Refuses missing, stale or foreign document ownership and invalid scope metadata.
    pub fn register_document_history(
        &mut self,
        definition: &'static crate::history_mode::HistoryDefinition,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        self.register_history_of(definition, None)
    }

    /// Register presentation for the folder this component's window shows, as
    /// [`Self::register_document_history`]: the tree record the window's rows are, which
    /// its runner fills `folder` with. The folder's scope stands for the window's own
    /// record too, which keeps only which folder is open.
    pub fn register_document_folder_history(
        &mut self,
        definition: &'static crate::history_mode::HistoryDefinition,
        folder: crate::history_mode::folder::FolderHistorySource,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        self.register_history_of(definition, Some(folder))
    }
    /// Show and restore the versions of `parts`, the documents the app keeps beside its own, in the
    /// History of its document.
    pub fn list_parts_in_history(&mut self, parts: &Rc<dyn crate::history_mode::HistoryParts>) {
        if let Some(history) = &mut self.history {
            history.sources = std::mem::take(&mut history.sources)
                .into_iter()
                .map(|source| source.with_parts(Rc::clone(parts)))
                .collect();
        }
    }
    /// Open History at `entry` when the window starts, as a window replaying
    /// a recording opens at its first step. Refused before History is
    /// registered.
    ///
    /// # Errors
    /// [`crate::history_mode::HistoryRegistrationError::Missing`] when no
    /// History is registered.
    pub fn open_history_at(
        &mut self,
        entry: tilo_ui_base::HistoryEntryId,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        self.history
            .as_mut()
            .ok_or(crate::history_mode::HistoryRegistrationError::Missing)?
            .opening_at = Some(entry);
        Ok(())
    }
    fn register_history_of(
        &mut self,
        definition: &'static crate::history_mode::HistoryDefinition,
        folder: Option<crate::history_mode::folder::FolderHistorySource>,
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
                HistoryPresentation::Folder => match &folder {
                    Some(source) => Ok(HistorySourceRegistration::FolderFiles {
                        scope: scope.key,
                        source: source.clone(),
                        window: Some(owner.clone()),
                    }),
                    None => Err(crate::history_mode::HistoryRegistrationError::UnsupportedOwner),
                },
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.history = Some(HistoryRegistration::new(definition, sources)?);
        Ok(())
    }

    /// Preserve every recovery owner while a runner still owns this component.
    pub(crate) fn setup_error(&mut self, source: LaunchError) -> LaunchError {
        let binding = self.callback_binding();
        self.close();
        LaunchError::ComponentSetup {
            source: Box::new(source),
            record: Box::new(binding.checkout),
            recovery: crate::launch_error::LaunchRecovery::new(
                Some(binding.records),
                self.take_close_reports(),
            ),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn callback_binding_for_test(&self) -> crate::bound_dispatch::CallbackBinding {
        self.callback_binding()
    }

    pub(crate) fn callback_binding(&self) -> crate::bound_dispatch::CallbackBinding {
        self.context.callback_binding()
    }

    /// The only action-facing loan of component root and registry.
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
        // What the component code starts off the frame reports back to
        // it: a wake it captures here that reaches no component is a broken
        // wire, caught where it is made ([`crate::wake::FeedsComponent`]).
        let _feeding = crate::wake::FeedsComponent::begin();
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
    /// The changes the workspace refused and the record withdrew are delivered
    /// with it. Returns whether the delivered status changed or a refusal
    /// arrived.
    pub(crate) fn project_resource_save_status(&mut self) -> bool {
        let captured = self.callback_binding();
        let context = &self.context;
        let told = self.sampled_resource_save().cloned();
        let wake = std::sync::Arc::clone(self.save_wake.get_or_insert_with(|| {
            let handle = context.wake_handle();
            std::sync::Arc::new(move || handle.wake_save_progress())
        }));
        let sampled = self
            .with_live_component(&captured, |binding, root, _registry| {
                // The parts of a chain the window keeps beside its record (a
                // notebook's notes) are saved as the record is: one still
                // being written, or failed, is the window's to say, and its
                // retry retries them too.
                let parts = binding.chain_parts();
                if root.take_save_retry() {
                    for id in std::iter::once(&binding.checkout.id)
                        .chain(parts.iter().map(|part| &part.id))
                    {
                        if let Err(error) = binding.records.try_flush_resource(id) {
                            tracing::warn!(%error, "retrying a failed record save");
                        }
                    }
                }
                let refusals = binding.records.take_refusals(&binding.checkout.id);
                for refusal in &refusals {
                    root.record_refused(refusal);
                }
                for text in root.take_notices() {
                    if let Err(error) = binding.records.hold_notice(&binding.checkout.id, text) {
                        tracing::warn!(%error, "a notice could not wait for the workspace");
                    }
                }
                let said = binding.records.take_said(&binding.checkout.id);
                for text in &said {
                    root.record_said(text);
                }
                let status = binding.sampled_save_status(&parts, &wake).ok()?;
                // The root hears the durability it shows when it moves, once
                // per move: a frame samples it after its settle and its sync
                // both, and a status it already shows is no news. It says
                // whether the move changed anything it shows: a save that
                // lands within the stall window shows nothing.
                let shown =
                    told.as_ref() != Some(&status) && root.resource_save_status(status.clone());
                Some((status, shown, !refusals.is_empty() || !said.is_empty()))
            })
            .ok()
            .flatten();
        let Some((status, shown, refused)) = sampled else {
            return false;
        };
        // A refusal changed what the root shows, as a durability it shows did.
        let changed = refused || shown;
        self.record_save = Some(status);
        if changed {
            // The caller, a sync, syncs the root again with the new state,
            // and the frame that sync belongs to shows it: an `App` asks
            // through `take_save_status_presentation`, a host through the
            // widget's redraw. No wake: one would run the owner model's
            // update and sync again and draw another frame for every save
            // that arrives, a second frame after each keystroke.
            self.save_status_changed = true;
        }
        changed
    }

    /// Whether the durability the window shows is still being written.
    pub(crate) fn record_saving(&self) -> bool {
        if !matches!(
            &self.record_save,
            Some(crate::resource::ResourceSaveStatus::Saving)
        ) {
            return false;
        }
        let binding = self.callback_binding();
        binding
            .shown_save_status(&binding.chain_parts())
            .map_or(true, |status| {
                status == crate::resource::ResourceSaveStatus::Saving
            })
    }

    /// Whether the delivered durability changed since this was last asked, so the window presents the
    /// new state instead of keeping the old one.
    pub(crate) fn take_save_status_changed(&mut self) -> bool {
        std::mem::take(&mut self.save_status_changed)
    }

    pub fn new(
        context: ProcessContext,
        mut root: Box<dyn AppWidget>,
        mut registry: tilo_actions::ActionRegistry,
    ) -> Self {
        let construction =
            crate::semantic_document_read::retirement::MountConstructionRetirement::new(
                context.retirement.as_ref(),
            );
        tilo_ui_base::widget::controls::attach_controls(
            root.as_mut(),
            &context.control_domain(),
            tilo_ui_base::control_domain::DomainAttachment::Initial,
        );
        context.control_domain().finish_initial_installation();
        if let Some(content) = context.content() {
            registry.connect_content(content.clone());
        }
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
            save_wake: None,
            host_owner_wakes: None,
        }
    }

    /// Take the view and action decoders of newly loaded code in place of the running ones.
    #[cfg(feature = "native")]
    pub(crate) fn install_reloaded(&mut self, root: crate::app::DevRoot) {
        self.root = Some(root.view);
        let mut registry = root.registry;
        if let Some(content) = self.context.content() {
            registry.connect_content(content.clone());
        }
        self.registry = Some(registry);
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
    /// [`Self::search_providers`] as the action directory's observation reads them: the same names,
    /// generations and domains under this component's lifetime, without minting a proxy or making the
    /// root's providers.
    pub(crate) fn search_provider_descriptors(
        &self,
    ) -> Vec<crate::search::capability::SearchProviderDescriptor> {
        let binding = self.callback_binding();
        let Ok(_entry) = binding.enter() else {
            return vec![];
        };
        let Some(root) = self.root.as_deref() else {
            return vec![];
        };
        crate::search::capability::search_provider_descriptors(root)
            .into_iter()
            .filter(|descriptor| descriptor.owner.is_none())
            .map(|descriptor| descriptor.owned_by(binding.lifetime_id))
            .collect()
    }
    pub fn record(&self) -> &DocumentCheckout {
        self.context.record()
    }
    /// The content store this component reads its record's content from: the host's store joined to the
    /// content link this window keeps, so what the window asks for is its to ask.
    pub fn content(&self) -> Option<tilo_content::ContentStore> {
        self.context.content().cloned()
    }
    /// The component runs inside the window `host` names: what its work owes it, the window is
    /// owed too, so a host settling the window's replies waits for the component's.
    pub fn run_inside(&self, host: &WakeHandle) {
        self.context.wake_scope.run_inside(host);
    }
    pub fn mount(
        &mut self,
        sink: impl Fn(WakeEvent) + Send + Sync + 'static,
    ) -> Result<(), LaunchError> {
        if !self.prepared {
            return Err(LaunchError::BindingMismatch);
        }
        if let Some(root) = &mut self.root {
            tilo_ui_base::widget::controls::attach_controls(
                root.as_mut(),
                &self.context.control_domain(),
                tilo_ui_base::control_domain::DomainAttachment::Dynamic,
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
        let restore =
            |root: &mut dyn crate::widget::AppWidget, doc: &AutoCommit| -> Result<(), ThawError> {
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
            };
        self.context
            .read_resource(|doc| restore(root, doc))
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
            .read_resource(|doc| crate::component_lifecycle::unsaved_documents(root, doc))
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
                // The window is closing: its view goes to the workspace now, and the close waits
                // for the workspace to take it ([`Self::close_settling`]).
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
        if let Err(error) = self.context.records.try_flush_written() {
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

    /// Whether what [`Self::checkpoint_for_close`] sent is still on its way to the workspace:
    /// the view and the records' writes of this component and those it holds, asked each pass
    /// of a close without waiting.
    pub(crate) fn close_settling(&mut self) -> crate::close_admission::Settling {
        let mut settling = crate::close_admission::Settling::default();
        if self.context.closing.get() {
            return settling;
        }
        let _entry = self.context.wake_scope.enter();
        settling.take(
            self.context
                .views
                .settling()
                .map_err(|error| error.to_string()),
        );
        settling.take(self.context.records.settling());
        if let Some(root) = self.root.as_deref_mut() {
            settling.join(crate::component_lifecycle::close_settling(root));
        }
        settling
    }

    /// Finalization after a successful host checkpoint.
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
            resource_error: None,
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
        CloseComponentOnExit(self).0.close_owned()
    }
    fn close_owned(&mut self) -> ComponentCloseReport {
        drop(self.host_owner_wakes.take());
        self.context.retire_semantic_mount();
        let mut report = ComponentCloseReport {
            recovery: None,
            view_error: None,
            view_retry: None,
            resource_error: None,
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
            && let Err(error) = self
                .context
                .views
                .save(saved)
                .and_then(|()| self.context.views.settling().map(drop))
        {
            report.view_error = Some(error.to_string());
            report.view_retry = Some(self.context.views.clone());
        }
        if let Err(error) = self.context.records.try_flush() {
            report.resource_error = Some(error);
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
            let result = if self.context.records.is_opening(&self.context.record.id) {
                Ok(())
            } else {
                self.context
                    .records
                    .with_checked(&self.context.record.id, |doc| {
                        for invocation in &owned.2 {
                            tilo_ui_base::widget::poll_resource_operation(
                                &mut *root,
                                *invocation,
                                true,
                                doc,
                                &automerge::ROOT,
                            );
                        }
                    })
                    .map_err(|error| error.to_string())
            };
            tilo_ui_base::widget::unmount_native_views(root);
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

    fn action_directory_invocation_domain(
        &self,
    ) -> tilo_ui_base::widget::ActionDirectoryInvocationDomain {
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
    fn open_action(&self) -> Option<tilo_actions::ActionAddress> {
        self.invoke(|provider| provider.open_action()).flatten()
    }
    fn search_scopes(&self) -> Vec<crate::widget::search_request::ScopeDecl> {
        self.invoke(|provider| provider.search_scopes())
            .unwrap_or_default()
    }
    fn resolve_action_resource(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::ResourceSpec> {
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
    pub resource_error: Option<String>,
    pub record_retry: Option<ResourceStore>,
    /// Exact legacy owner for scoped close/retry; None keeps component close behavior.
    pub record_retry_target: Option<crate::DocumentId>,
    /// Reserved for fallible lifecycle results. Arbitrary app panics propagate.
    pub teardown_error: Option<String>,
}
impl ComponentCloseReport {
    /// Retry the same dirty canonical owner, retaining it until durability succeeds.
    pub fn retry_resource(&mut self) -> Result<(), String> {
        if let Some(records) = &self.record_retry {
            match &self.record_retry_target {
                Some(id) => records.try_flush_resource(id),
                None => records.try_flush(),
            }
            .inspect_err(|error| self.resource_error = Some(error.clone()))?;
            self.record_retry = None;
            self.record_retry_target = None;
            self.resource_error = None;
        }
        Ok(())
    }

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
    pub fn register(
        &mut self,
        definition: &'static ApplicationDefinition,
    ) -> Result<(), LaunchError> {
        if self.definitions.contains_key(definition.app_id()) {
            return Err(LaunchError::DuplicateApplication(
                definition.app_id().into(),
            ));
        }
        self.definitions.insert(definition.app_id(), definition);
        Ok(())
    }
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
/// A launch refused because its parts are bound to different things, with
/// the check that refused it named in the log.
fn mismatch(why: &'static str) -> LaunchError {
    tracing::warn!(why, "application binding does not match");
    LaunchError::BindingMismatch
}

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
        {
            assert_closed_cleanup("root");
        }

        {
            assert_closed_cleanup("registry");
        }
    }
}

#[cfg(test)]
mod bound_lifetime_tests {
    use super::*;
    use crate::{widget::AppKind, widget::Column};
    use tilo_ui_base::widget::Widget;

    use taffy::{NodeId, TaffyTree};
    struct Root(Column);
    impl Widget for Root {
        fn sync(
            &mut self,
            doc: &tilo_ui_base::widget::AppModel,
            path: &tilo_ui_base::widget::ModelPath,
        ) {
            self.0.sync(doc, path);
        }
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            theme: &tilo_ui_base::Theme,
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
        fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
            #[allow(
                unused_variables,
                unused_mut,
                reason = "a draw binds only the parts it uses"
            )]
            let (scene, transform, text, theme, mut env) = ctx.parts();
            self.0.draw(&mut tilo_ui_base::draw::DrawCtx::new(
                scene,
                transform,
                text,
                theme,
                env.clone(),
            ));
        }
        fn handle_event(
            &mut self,
            event: &winit::event::WindowEvent,
            ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
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
    }
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the factory trait fixes this signature, and `RestoreInput` is unused for a new resource"
    )]
    fn factory(
        mut context: ProcessContext,
        target: LaunchTarget,
        _: RestoreInput,
    ) -> Result<AppComponent, LaunchError> {
        if matches!(target, LaunchTarget::New) {
            context
                .initialize_new_resource(|_| Ok(()))
                .map_err(LaunchError::Record)?;
        }
        Ok(AppComponent::new(
            context,
            Box::new(Root(Column::new())),
            tilo_actions::ActionRegistry::new(),
        ))
    }

    tilo_test_env::scenario! {
        fn bound_component_lifetimes_keep_identity_and_refuse_stale_entries;

        phase host_named_new_and_package_primary_keep_distinct_identity_contracts;
        {
            let mut owners = crate::resource::ResourceOwners::in_memory();
            let records = owners.open("dev.tilo.mount-probe", "A").unwrap();
            let views = Rc::new(InstanceViewStore::in_memory("dev.tilo.mount-probe", "D", "A").unwrap());
            let definition = ApplicationDefinition {
                app_id: "dev.tilo.mount-probe".into(),
                title: "Mount probe".into(),
                icon: IconDescriptor::Glyph("P"),
                resource_type: ResourceType::new("probe"),
                required: vec![],
                optional: vec![],
                opens: &[],
                workspace_resource: false,
                opens_before_resource: false,
                create: factory,
            };
            let person = "ab".repeat(32);
            let supplied = DocumentId::for_tab(55);
            let prepare = |target, supplied| {
                ProcessContext::prepare_scoped(
                    &definition,
                    ProcessContextInputs {
                        records: records.clone(),
                        views: views.clone(),
                        device_id: "D".into(),
                        instance_id: 7,
                        permissions: tilo_ui_base::permissions::SharedPermissions::new(
                            PermissionContext::permissive(),
                        ),
                        author: MemberId::person(&person),
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
                prepare(&LaunchTarget::PrimaryNew, Some(DocumentId::for_tab(56))),
                Err(LaunchError::BindingMismatch)
            ));
            let primary = prepare(&LaunchTarget::PrimaryNew, None).unwrap();
            assert!(primary.record.id.is_package_primary());

        }

        phase existing_package_primary_is_local_while_invalid_ordinary_records_are_rejected;
        {
            let mut owners = crate::resource::ResourceOwners::in_memory();
            let records = owners.open("dev.tilo.mount-probe", "A").unwrap();
            let views = Rc::new(InstanceViewStore::in_memory("dev.tilo.mount-probe", "D", "A").unwrap());
            let definition = ApplicationDefinition {
                app_id: "dev.tilo.mount-probe".into(),
                title: "Mount probe".into(),
                icon: IconDescriptor::Glyph("P"),
                resource_type: ResourceType::new("probe"),
                required: vec![],
                optional: vec![],
                opens: &[],
                workspace_resource: false,
                opens_before_resource: false,
                create: factory,
            };
            let prepare = |checkout| {
                ProcessContext::prepare(
                    &definition,
                    ProcessContextInputs {
                        records: records.clone(),
                        views: views.clone(),
                        device_id: "D".into(),
                        instance_id: 7,
                        permissions: tilo_ui_base::permissions::SharedPermissions::new(
                            PermissionContext::permissive(),
                        ),
                        author: MemberId::person("probe-author"),
                        action_context: tilo_actions::ActionContext::default(),
                    },
                    &LaunchTarget::Record {
                        checkout,
                        reveal: None,
                    },
                )
            };

            let malformed = DocumentCheckout::new(
                DocumentId::derived("invalid role", &DocumentId::for_tab(55)),
                definition.resource_type.clone(),
            );
            assert!(matches!(
                prepare(malformed),
                Err(LaunchError::BindingMismatch)
            ));
            let foreign = DocumentCheckout::new(DocumentId::for_tab(56), definition.resource_type.clone());
            assert!(matches!(
                prepare(foreign),
                Err(LaunchError::Record(
                    crate::resource::ResourceOpenError::MissingResource
                ))
            ));

            let primary = DocumentCheckout::package_primary(definition.resource_type.clone());
            records.create_resource(&primary).unwrap();
            let context = prepare(primary).expect("an existing local package primary must reopen");
            assert!(context.record.id.is_package_primary());
        }

        phase component_lifetimes_refuse_stale_wakes_loans_and_busy_entries;
        {
            {
                let mut owners = crate::resource::ResourceOwners::in_memory();
                let records = owners.open("so.tilo.mount-probe", "A").unwrap();
                let views =
                    Rc::new(InstanceViewStore::in_memory("so.tilo.mount-probe", "D", "A").unwrap());
                let definition = ApplicationDefinition {
                    app_id: "so.tilo.mount-probe".into(),
                    title: "Mount probe".into(),
                    icon: IconDescriptor::Glyph("P"),
                    resource_type: ResourceType::new("probe"),
                    required: vec![],
                    optional: vec![],
                    opens: &[],
                    workspace_resource: false,
                    opens_before_resource: false,
                    create: factory,
                };
                let prepare = |target: &LaunchTarget| {
                    ProcessContext::prepare(
                        &definition,
                        ProcessContextInputs {
                            records: records.clone(),
                            views: views.clone(),
                            device_id: "D".into(),
                            instance_id: 7,
                            permissions: tilo_ui_base::permissions::SharedPermissions::new(
                                PermissionContext::permissive(),
                            ),
                            author: MemberId::person("probe-author"),
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
                    .read_document(&AutoCommit::new(), |_, _| tilo_ui_base::LocalState::new(0))
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
                                ResourceOpenError::Busy
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

            {
                let mut owners = crate::resource::ResourceOwners::in_memory();
                let records = owners.open("so.tilo.mount-probe", "A").unwrap();
                let views =
                    Rc::new(InstanceViewStore::in_memory("so.tilo.mount-probe", "D", "A").unwrap());
                let definition = ApplicationDefinition {
                    app_id: "so.tilo.mount-probe".into(),
                    title: "Mount probe".into(),
                    icon: IconDescriptor::Glyph("P"),
                    resource_type: ResourceType::new("probe"),
                    required: vec![],
                    optional: vec![],
                    opens: &[],
                    workspace_resource: false,
                    opens_before_resource: false,
                    create: factory,
                };
                let context = ProcessContext::prepare(
                    &definition,
                    ProcessContextInputs {
                        records,
                        views,
                        device_id: "D".into(),
                        instance_id: 7,
                        permissions: tilo_ui_base::permissions::SharedPermissions::new(
                            PermissionContext::permissive(),
                        ),
                        author: MemberId::person("probe-author"),
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
}

#[cfg(test)]
mod host_tests {
    use super::*;
    use crate::application_test_support as probe;

    tilo_test_env::scenario! {
        fn a_portable_host_binds_models_identity_and_storage_explicitly;

        phase models_are_explicitly_bound_to_each_host_context;
        {
            struct Backend;
            impl crate::models::ModelBackend for Backend {
                fn complete<'a>(
                    &'a self,
                    _: &'a crate::models::ModelRequest,
                ) -> crate::models::CompletionFuture<'a> {
                    Box::pin(async {
                        Err(crate::models::ModelError {
                            kind: crate::models::ErrorKind::PermissionDenied,
                            message: "fixture session".into(),
                        })
                    })
                }
            }
            let records = crate::resource::ResourceOwners::in_memory()
                .open(probe::APP, probe::WORKSPACE)
                .unwrap();
            let views = Rc::new(
                InstanceViewStore::in_memory(probe::APP, probe::DEVICE, probe::WORKSPACE).unwrap(),
            );
            let definition = probe::definition(probe::probe_factory);
            let prepare = |instance_id| {
                ProcessContext::prepare(
                    &definition,
                    ProcessContextInputs {
                        records: records.clone(),
                        views: views.clone(),
                        device_id: probe::DEVICE.into(),
                        instance_id,
                        permissions: probe::permissions(),
                        author: probe::author(),
                        action_context: probe::provenance(),
                    },
                    &LaunchTarget::New,
                )
                .unwrap()
            };
            let mut first = prepare(41);
            assert!(!first.models().is_available());
            first.provide_models(crate::models::Models::new(Rc::new(Backend)));
            first.provide_models(crate::models::Models::new(Rc::new(Backend)));
            let mut second = prepare(42);
            assert!(!first.provider_admin().is_available());
            first.provide_provider_admin(crate::models::ProviderAdministration::default());
            first.provide_provider_admin(crate::models::ProviderAdministration::default());
            let mut loans = first.take_lent();
            let admin = loans.remove(
                loans
                    .iter()
                    .position(|loan| loan.is::<crate::models::ProviderAdministration>())
                    .unwrap(),
            );
            assert!(
                !admin
                    .downcast::<crate::models::ProviderAdministration>()
                    .unwrap()
                    .is_available()
            );
            assert!(!second.provider_admin().is_available());
            assert_eq!(loans.len(), 1, "replacing a capability replaces its loan");
            let loaned = loans
                .into_iter()
                .next()
                .unwrap()
                .downcast::<crate::models::Models>()
                .unwrap();
            assert!(loaned.is_available());
            assert!(second.take_lent().is_empty());
            assert!(first.models().is_available());
            assert!(!second.models().is_available());
            let retained = first.models();
            drop(first);
            assert!(retained.is_available());
        }

        phase trusted_local_person_survives_author_changes_and_reopening;
        {
            let records = crate::resource::ResourceOwners::in_memory()
                .open(probe::APP, probe::WORKSPACE)
                .unwrap();
            let definition = probe::definition(probe::probe_factory);
            let mut context = ProcessContext::prepare(
                &definition,
                ProcessContextInputs {
                    records,
                    views: Rc::new(
                        InstanceViewStore::in_memory(probe::APP, probe::DEVICE, probe::WORKSPACE)
                            .unwrap(),
                    ),
                    device_id: probe::DEVICE.into(),
                    instance_id: 7,
                    permissions: probe::permissions(),
                    author: probe::author(),
                    action_context: probe::provenance(),
                },
                &LaunchTarget::New,
            )
            .unwrap();
            context.author = MemberId::person("agent");
            assert_eq!(context.local_person(), &probe::author());
            assert_eq!(context.callback_binding().local_person, probe::author());
            let (reopened, _) = context
                .reopener()
                .prepare(&definition, DocumentId::minted())
                .unwrap();
            assert_eq!(reopened.author(), &MemberId::person("agent"));
            assert_eq!(reopened.local_person(), &probe::author());
            assert_eq!(reopened.callback_binding().local_person, probe::author());
        }

        // The compositor runs apps on its frame loop: checkpointing and closing
        // one there must never wait for a store's writer thread, and the record
        // and View still reach the workspace, for the next start to find.
        phase checkpointing_and_closing_on_the_frame_loop_never_wait_on_storage;
        {
            let host = probe::ProbeHarness::new();
            let mut component =
                probe::instantiate_probe(&host, 7, LaunchTarget::New, RestoreInput::None).unwrap();
            let checkout = {
                let _loop = crate::storage::waiting::forbid_strictly_on_this_thread();
                let checkout = component.record().clone();
                assert_eq!(component.checkpoint_for_close(), Vec::<String>::new());
                let report = component.close();
                assert_eq!(report.resource_error, None);
                assert_eq!(report.view_error, None);
                drop(component);
                checkout
            };
            let probe::ProbeHarness {
                owners,
                records,
                views,
                workspace,
                ..
            } = host;
            drop((records, owners, views));
            let mut owners = crate::resource::ResourceOwners::writing(workspace.clone());
            let records = owners.open(probe::APP, probe::WORKSPACE).unwrap();
            assert!(records.open_resource(&checkout), "the record was saved");
            assert_eq!(probe::body(&records, &checkout), "initialized");
            let saved = InstanceViewStore::new(
                Some(workspace),
                probe::APP,
                probe::DEVICE,
                probe::WORKSPACE,
            )
            .unwrap()
            .load(7)
            .unwrap()
            .expect("the View was saved");
            assert_eq!(saved.document_id, checkout.id.as_str());
        }

        phase a_host_checks_the_view_binding_its_authority_and_persists_a_real_component;
        {
            let workspace = Rc::new(crate::resource::MemoryWorkspace::default());
            let mut owners = crate::resource::ResourceOwners::writing(workspace.clone());
            let records = owners.open(probe::APP, probe::WORKSPACE).unwrap();
            let views = Rc::new(
                InstanceViewStore::new(
                    Some(workspace.clone()),
                    probe::APP,
                    probe::DEVICE,
                    probe::WORKSPACE,
                )
                .unwrap(),
            );
            let definition = probe::definition(probe::probe_factory);
            let prepare = |views, permissions| {
                ProcessContext::prepare(
                    &definition,
                    ProcessContextInputs {
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
            // A host's views of another app's windows are not this window's to restore.
            let foreign = InstanceViewStore::in_memory("so.tilo.other", probe::DEVICE, probe::WORKSPACE)
                .unwrap();
            assert!(matches!(
                prepare(Rc::new(foreign), probe::permissions()),
                Err(LaunchError::BindingMismatch)
            ));
            let context = prepare(views.clone(), probe::permissions()).unwrap();
            let mut component = definition
                .instantiate(context, LaunchTarget::New, RestoreInput::None)
                .unwrap();
            let checkout = component.record().clone();
            assert!(records.is_ready(&checkout.id));
            let report = component.close();
            assert_eq!(report.resource_error, None);
            assert_eq!(report.view_error, None);
            // What the window closed on is the workspace's, for the next start.
            let saved = views.load(7).unwrap().unwrap();
            assert_eq!(saved.document_id, checkout.id.as_str());
            let mut reopened = crate::resource::ResourceOwners::writing(workspace);
            let records = reopened.open(probe::APP, probe::WORKSPACE).unwrap();
            assert!(
                records
                    .open_existing_resource_checked(&checkout)
                    .is_ok(),
                "the record the window closed on is in the workspace"
            );
            let mut denied = probe::definition(probe::probe_factory);
            denied.required.push(probe::denied_scope());
            assert!(matches!(
                ProcessContext::prepare(
                    &denied,
                    ProcessContextInputs {
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
}
