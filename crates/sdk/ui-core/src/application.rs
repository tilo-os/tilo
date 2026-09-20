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
use crate::view::{FrozenApp, ThawError, View};
use crate::wake::{WakeEvent, WakeHandle, WakeScope};
use crate::widget::{AppWidget, IconDescriptor};
use automerge::AutoCommit;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

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
    pub create: ComponentFactory,
}
#[non_exhaustive]
pub enum LaunchTarget {
    New,
    Record {
        checkout: RecordCheckout,
        reveal: Option<tilo_actions::ActionAddress>,
    },
}
#[derive(Clone)]
pub enum RestoreInput {
    None,
    Local(SavedInstanceView),
    LegacyRecovery(FrozenApp),
}

/// Explicit identity, authority and storage. Apps receive no ambient profile resolver.
pub struct HostContext {
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
    wake_scope: WakeScope,
    construction: Rc<()>,
    lifetime_id: uuid::Uuid,
    queued_controls: Rc<RefCell<std::collections::VecDeque<tilo_actions::ControlInvocation>>>,
    closing: Rc<Cell<bool>>,
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

/// Explicit host preparation completed before invoking the application factory.
pub struct PreparedComponentInputs {
    pub context: HostContext,
    pub target: LaunchTarget,
    pub restore: RestoreInput,
    pub runner_inputs: ComponentRunnerInputs,
}

impl HostContext {
    /// Reserve New or open Existing before the factory, using only supplied identities.
    ///
    /// # Errors
    /// Rejects denied authority, invalid/foreign namespaces and unavailable records.
    #[allow(
        clippy::too_many_arguments,
        reason = "host inputs are explicit rather than resolved from ambient state"
    )]
    pub fn prepare(
        definition: &ApplicationDefinition,
        records: RecordStore,
        views: Rc<InstanceViewStore>,
        device_id: String,
        instance_id: InstanceId,
        permissions: Rc<RefCell<PermissionContext>>,
        author: UserId,
        action_context: tilo_actions::ActionContext,
        target: &LaunchTarget,
    ) -> Result<Self, LaunchError> {
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
                let checkout =
                    RecordCheckout::new(RecordId::minted(), definition.record_kind.clone());
                let reservation = records
                    .reserve_record(&checkout)
                    .map_err(LaunchError::Record)?;
                (checkout, Some(reservation))
            }
            LaunchTarget::Record { checkout, .. } => {
                if checkout.kind != definition.record_kind {
                    return Err(LaunchError::BindingMismatch);
                }
                records
                    .try_open_record(checkout)
                    .map_err(LaunchError::Record)?;
                (checkout.clone(), None)
            }
        };
        Ok(Self {
            app_id: definition.app_id.into(),
            instance_id,
            device_id,
            records,
            views,
            record,
            reservation,
            permissions,
            author,
            action_context,
            wake_scope: WakeScope::new(),
            construction: Rc::new(()),
            lifetime_id: uuid::Uuid::new_v4(),
            queued_controls: Rc::default(),
            closing: Rc::new(Cell::new(false)),
        })
    }
    pub fn record(&self) -> &RecordCheckout {
        &self.record
    }
    pub fn author(&self) -> &UserId {
        &self.author
    }
    pub fn permissions(&self) -> PermissionContext {
        self.permissions.borrow().clone()
    }
    pub fn action_context(&self) -> &tilo_actions::ActionContext {
        &self.action_context
    }
    pub fn wake_handle(&self) -> WakeHandle {
        self.wake_scope.handle()
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
            instance_id: self.instance_id,
            app_id: self.app_id.clone(),
            records: self.records.clone(),
            checkout: self.record.clone(),
            permissions: self.permissions.clone(),
            author: self.author.clone(),
            operation_context: self.action_context.clone(),
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
                    || saved.record_kind != self.record.kind.as_str()
                {
                    return Err(LaunchError::BindingMismatch);
                }
                Ok(())
            }
            RestoreInput::LegacyRecovery(frozen) => {
                if frozen.version != crate::view::FROZEN_VERSION {
                    return Err(LaunchError::Restore(ThawError::UnsupportedVersion {
                        found: frozen.version,
                        supported: crate::view::FROZEN_VERSION,
                    }));
                }
                AutoCommit::load(&frozen.state)
                    .map_err(|e| LaunchError::Restore(ThawError::State(e.to_string())))?;
                frozen.view.validate().map_err(LaunchError::Restore)?;
                Err(LaunchError::RecoveryRequired)
            }
        }
    }
}
impl Drop for HostContext {
    fn drop(&mut self) {
        self.wake_scope.close();
    }
}
impl ApplicationDefinition {
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
                    record,
                    records,
                    reports: Vec::new(),
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
        match &target {
            LaunchTarget::New if context.reservation.is_none() => {
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

/// Real app content and its lifetime; hosts cannot extract an unbound root.
pub struct AppComponent {
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
    /// Preserve every recovery owner while a runner still has custody of this component.
    pub(crate) fn setup_error(&mut self, source: LaunchError) -> LaunchError {
        let binding = self.callback_binding();
        self.close();
        LaunchError::ComponentSetup {
            source: Box::new(source),
            record: binding.checkout,
            records: binding.records,
            reports: self.take_close_reports(),
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
        root: Box<dyn AppWidget>,
        registry: tilo_actions::ActionRegistry,
    ) -> Self {
        let registry_owner = context.construction.clone();
        Self {
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

    /// Return only weak proxies; payload ownership remains with this component.
    pub(crate) fn search_providers(&self) -> Vec<Box<dyn crate::search::SearchProvider>> {
        let binding = self.callback_binding();
        let Ok(_entry) = binding.enter() else {
            return vec![];
        };
        let Some(root) = self.root.as_deref() else {
            return vec![];
        };
        root.search_providers()
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
            .read_record(|doc| root.unsaved_documents(doc))
            .unwrap_or_default()
    }

    /// Persist the current document and view through their original owners.
    /// Unlike close(), refusal leaves callbacks, publication and the live tree
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
            errors.extend(root.checkpoint_components());
            self.pending_reports
                .extend(root.take_component_close_reports());
        }
        errors.extend(crate::close_admission::retry_reports(
            &mut self.pending_reports,
        ));
        errors
    }

    /// Finalization after a successful host checkpoint. No second fallible
    /// write is introduced after the host has committed to destroying the tree.
    pub(crate) fn finish_admitted_close(&mut self) {
        if self.close_report.is_some() {
            return;
        }
        self.context.closing.set(true);
        self.context.wake_scope.close();
        let _entry = self.context.wake_scope.enter();
        if let Some(root) = self.root.as_deref_mut() {
            self.pending_reports.extend(root.finish_admitted_close());
            self.pending_reports
                .extend(root.take_component_close_reports());
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
        let mut report = ComponentCloseReport {
            recovery: None,
            view_error: None,
            view_retry: None,
            record_error: None,
            record_retry: None,
            teardown_error: None,
        };
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
            self.pending_reports.extend(root.close_components());
            self.pending_reports
                .extend(root.take_component_close_reports());
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
            reports.extend(root.take_component_close_reports());
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
    /// Reserved for fallible lifecycle results. Arbitrary app panics propagate.
    pub teardown_error: Option<String>,
}
impl ComponentCloseReport {
    /// Retry the same dirty canonical owner, retaining it until durability succeeds.
    /// # Errors
    /// Returns the original owner's current persistence failure.
    pub fn retry_record(&mut self) -> Result<(), String> {
        if let Some(records) = &self.record_retry {
            records
                .try_flush()
                .inspect_err(|error| self.record_error = Some(error.clone()))?;
            self.record_retry = None;
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
    fn root_panic_still_drops_registry_under_closed_scope_and_propagates() {
        assert_closed_cleanup("root");
    }
    #[test]
    fn registry_panic_propagates_after_both_closed_scope_drops() {
        assert_closed_cleanup("registry");
    }
}

#[cfg(test)]
mod bound_lifetime_tests {
    use super::*;
    use crate::widget::{AppKind, Column, Widget};
    use automerge::ObjId;
    use taffy::{NodeId, TaffyTree};
    struct Root(Column);
    impl Widget for Root {
        fn sync(&mut self, doc: &mut AutoCommit, path: &ObjId) {
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
            scale: f64,
            modifiers: &winit::event::Modifiers,
            doc: &mut AutoCommit,
            path: &ObjId,
            handled: &mut bool,
            focus: &mut Option<NodeId>,
            node: NodeId,
            tree: &TaffyTree<()>,
        ) {
            self.0.handle_event(
                event, scale, modifiers, doc, path, handled, focus, node, tree,
            );
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
    fn actual_component_reopen_refuses_queued_old_wakes_and_busy_store_entry() {
        let temp = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(temp.path().into());
        let records = owners.open("dev.tilo.mount-probe", "A").unwrap();
        let views = Rc::new(InstanceViewStore::new(temp.path().to_path_buf(), "D", "A").unwrap());
        let definition = ApplicationDefinition {
            app_id: "dev.tilo.mount-probe",
            title: "Mount probe",
            icon: IconDescriptor::Glyph("P"),
            record_kind: RecordKind::new("probe"),
            required: vec![],
            optional: vec![],
            create: factory,
        };
        let prepare = |target: &LaunchTarget| {
            HostContext::prepare(
                &definition,
                records.clone(),
                views.clone(),
                "D".into(),
                7,
                Rc::new(RefCell::new(PermissionContext::permissive())),
                UserId("probe-author".into()),
                tilo_actions::ActionContext::default(),
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
                    binding.with_document(&mut AutoCommit::new(), |_, _| panic!("busy entry ran")),
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

    #[test]
    fn task4_component_loan_checks_captured_registry_and_lifetime_before_delivery() {
        let temp = tempfile::tempdir().unwrap();
        let mut owners = crate::record::RecordOwners::new(temp.path().into());
        let records = owners.open("dev.tilo.mount-probe", "A").unwrap();
        let views = Rc::new(InstanceViewStore::new(temp.path().to_path_buf(), "D", "A").unwrap());
        let definition = ApplicationDefinition {
            app_id: "dev.tilo.mount-probe",
            title: "Mount probe",
            icon: IconDescriptor::Glyph("P"),
            record_kind: RecordKind::new("probe"),
            required: vec![],
            optional: vec![],
            create: factory,
        };
        let context = HostContext::prepare(
            &definition,
            records,
            views,
            "D".into(),
            7,
            Rc::new(RefCell::new(PermissionContext::permissive())),
            UserId("probe-author".into()),
            tilo_actions::ActionContext::default(),
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
            component.with_live_component(&foreign, |_, _, _| panic!("foreign registry delivered")),
            Err(crate::bound_dispatch::BoundDispatchError::ClosedLifetime)
        ));
        component.close();
        assert!(matches!(
            component.with_live_component(&binding, |_, _, _| panic!("closed component delivered")),
            Err(crate::bound_dispatch::BoundDispatchError::ClosedLifetime)
        ));
    }
}
