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
    /// hands it a file ([`HostContext::take_open_file`]).
    pub opens: &'static [&'static str],
    pub create: ComponentFactory,
}
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
    permissions: Rc<RefCell<PermissionContext>>,
    author: UserId,
    action_context: tilo_actions::ActionContext,
    /// See [`Self::write_as_author`].
    writes_as_author: bool,
    wake_scope: WakeScope,
    construction: Rc<()>,
    lifetime_id: uuid::Uuid,
    queued_controls: Rc<RefCell<std::collections::VecDeque<tilo_actions::ControlInvocation>>>,
    control_domain: crate::ControlDomain,
    closing: Rc<Cell<bool>>,
    services: crate::host::HostServices,
    retirement: Option<crate::semantic_document_read::retirement::MountRetirement>,
    content: Option<tilo_content::ContentStore>,
    open_file: Option<OpenFile>,
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

impl HostContext {
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
        Ok(Self {
            app_id: package,
            instance_id,
            device_id,
            records,
            views,
            record,
            reservation,
            permissions,
            author,
            action_context,
            writes_as_author: false,
            wake_scope: WakeScope::new(),
            construction: Rc::new(()),
            lifetime_id: uuid::Uuid::new_v4(),
            queued_controls: Rc::default(),
            control_domain: crate::control_custody::ControlDomain::new(),
            closing: Rc::new(Cell::new(false)),
            services: crate::host::current(),
            retirement: None,
            content: None,
            open_file: None,
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
    /// Explicit owner-bound producer capability, available before factory work.
    pub fn control_domain(&self) -> crate::ControlDomain {
        self.control_domain
            .set_owner(crate::control_custody::ControlOwner::Component(
                self.callback_binding(),
            ));
        self.control_domain.clone()
    }
    /// Establish presentation scope before constructing a nested component.
    pub fn inherit_control_custody(
        &self,
        parent: &crate::ControlDomain,
    ) -> Result<(), crate::control_custody::CustodyError> {
        self.control_domain().attach_to(parent)
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
        crate::storage::AppFiles::for_app(&self.app_id)
    }
    /// This app's preferences: its own section of the desktop settings
    /// document, named by the app id.
    pub fn settings(&self) -> crate::storage::Settings {
        crate::storage::Settings::for_app(&self.app_id)
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
    pub(crate) fn callback_binding(&self) -> crate::bound_dispatch::CallbackBinding {
        crate::bound_dispatch::CallbackBinding {
            lifetime: self.wake_scope.handle(),
            closing: self.closing.clone(),
            registry_owner: Rc::downgrade(&self.construction),
            lifetime_id: self.lifetime_id,
            queued_controls: self.queued_controls.clone(),
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
    /// Whether the app keeps one record per room, its package primary
    /// (Chat's conversation), rather than a record per window. Definitions
    /// do not declare this; Chat's record kind names it.
    pub fn keeps_room_record(&self) -> bool {
        self.record_kind.as_str() == "chat.workspace"
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
    pub fn opens_media_type(&self, media_type: &str) -> bool {
        open_file::media_type_essence(media_type)
            .is_some_and(|essence| self.opens.contains(&essence.as_str()))
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
}

impl ComponentBehaviour {
    /// Every behaviour off; a component spells out only what it turns on,
    /// `ComponentBehaviour { capture_protected: true, ..ComponentBehaviour::DEFAULT }`.
    pub const DEFAULT: Self = Self {
        autofocus_on_structure_change: false,
        appearance_reconcile: false,
        capture_protected: false,
    };
}

impl Default for ComponentBehaviour {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Real app content and its lifetime; hosts cannot extract an unbound root.
pub struct AppComponent {
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
}
impl AppComponent {
    /// Set how the hosting window treats this component's tree.
    pub const fn set_behaviour(&mut self, behaviour: ComponentBehaviour) {
        self.behaviour = behaviour;
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
                HistoryPresentation::CheckedFiles { .. } => {
                    Err(crate::history_mode::HistoryRegistrationError::UnsupportedOwner)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.history = Some(HistoryRegistration::new(definition, sources)?);
        Ok(())
    }
    /// Install an explicit multi-owner bundle after the factory has established
    /// its owners. The destination runner validates the complete settled tree.
    /// # Errors
    /// Refuses invalid registrations or a bundle belonging to another application.
    pub fn register_history(
        &mut self,
        registration: crate::history_mode::HistoryRegistration,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        registration.validate()?;
        if registration.definition.app_id != self.context.app_id {
            return Err(crate::history_mode::HistoryRegistrationError::ForeignSource);
        }
        self.history = Some(registration);
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
        let _ = self.with_live_component(&captured, |binding, root, _registry| {
            if let Ok(status) = binding.records.save_status(&binding.checkout.id) {
                root.record_save_status(status);
            }
        });
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
        query: &str,
        cursor: Option<&crate::search::SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> crate::search::SearchPage {
        self.invoke(|provider| provider.search_page(query, cursor, work_budget, result_limit))
            .unwrap_or_else(|| crate::search::SearchPage {
                results: vec![],
                next: None,
                inspected: 0,
            })
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
        fn sync(&mut self, doc: &mut crate::widget::AppModel, path: &crate::widget::ModelPath) {
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
