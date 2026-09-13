//! Checked application construction shared by hosts, independent of any runner.
use crate::application_view::{InstanceViewStore, SavedInstanceView, validate_identity};
use crate::launch::LaunchError;
use crate::permissions::{PermissionContext, PermissionScope, UserId};
use crate::record::{
    RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordReservation, RecordStore,
};
use crate::view::{FrozenApp, ThawError, View};
use crate::wake::{WakeEvent, WakeHandle, WakeScope};
use crate::widget::{AppWidget, IconDescriptor};
use automerge::AutoCommit;
use std::cell::RefCell;
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
        reveal: Option<qos_actions::ActionAddress>,
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
    action_context: qos_actions::ActionContext,
    wake_scope: WakeScope,
    construction: Rc<()>,
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
        action_context: qos_actions::ActionContext,
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
    pub fn action_context(&self) -> &qos_actions::ActionContext {
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
    fn envelope(&self, view: View) -> SavedInstanceView {
        SavedInstanceView {
            version: 1,
            app_id: self.app_id.clone(),
            device_id: self.device_id.clone(),
            workspace_id: self.views.workspace_id().into(),
            instance_id: self.instance_id,
            record_id: self.record.id.as_str().into(),
            record_kind: self.record.kind.as_str().into(),
            view,
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
            RestoreInput::Local(saved) => Some(saved.view.clone()),
            _ => None,
        };
        let entry = context.wake_scope.enter();
        let result = (self.create)(context, target, restore).and_then(|mut component| {
            if !Rc::ptr_eq(&construction, &component.context.construction)
                || !records.is_ready(&record.id)
            {
                return Err(LaunchError::BindingMismatch);
            }
            if let Some(view) = local {
                component
                    .restore_local_view(&view)
                    .map_err(LaunchError::Restore)?;
            }
            // Also validate widget contributions from a fresh construction.
            component
                .capture_local_view()
                .map_err(LaunchError::Restore)?;
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
    root: Option<Box<dyn AppWidget>>,
    registry: Option<qos_actions::ActionRegistry>,
    local_view: View,
    close_report: Option<ComponentCloseReport>,
    prepared: bool,
}
impl AppComponent {
    pub fn new(
        context: HostContext,
        root: Box<dyn AppWidget>,
        registry: qos_actions::ActionRegistry,
    ) -> Self {
        Self {
            context,
            root: Some(root),
            registry: Some(registry),
            local_view: View::default(),
            close_report: None,
            prepared: false,
        }
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
        let _entry = self.context.wake_scope.enter();
        let root = self
            .root
            .as_deref()
            .ok_or_else(|| ThawError::InvalidView("component is closed".into()))?;
        let mut view = crate::view::capture_widget_view(
            root,
            crate::view::capture_tree_focus(root),
            self.local_view.viewport,
        );
        if view.open_resource.is_none() {
            view.open_resource
                .clone_from(&self.local_view.open_resource);
        }
        view.validate()?;
        Ok(view)
    }
    /// Local hooks cover scroll, mode and focused ephemeral state. App-specific fields
    /// absent from `View` are unsupported; they must not be smuggled in as document bytes.
    /// # Errors
    /// Rejects malformed geometry or closed components before invoking widget hooks.
    pub fn restore_local_view(&mut self, view: &View) -> Result<(), ThawError> {
        view.validate()?;
        let _entry = self.context.wake_scope.enter();
        let root = self
            .root
            .as_deref_mut()
            .ok_or_else(|| ThawError::InvalidView("component is closed".into()))?;
        crate::view::restore_widget_view(root, view);
        if let Some(focus) = &view.focus {
            crate::view::restore_tree_focus(root, focus);
        }
        self.local_view = view.clone();
        Ok(())
    }
    pub fn suspend(&mut self) -> ComponentCloseReport {
        self.close()
    }
    /// Save through the original store, retain failures, and permanently close this lifetime.
    pub fn close(&mut self) -> ComponentCloseReport {
        if let Some(report) = &self.close_report {
            return report.clone();
        }
        let mut report = ComponentCloseReport {
            recovery: None,
            view_error: None,
            view_retry: None,
            record_error: None,
            record_retry: None,
            teardown_error: None,
        };
        match self.capture_local_view() {
            Ok(view) => {
                let saved = self.context.envelope(view);
                if let Err(error) = self.context.views.save(&saved) {
                    report.view_error = Some(error.to_string());
                    report.view_retry = Some(self.context.views.clone());
                }
                report.recovery = Some(saved);
            }
            Err(error) => report.view_error = Some(error.to_string()),
        }
        self.context.wake_scope.close();
        if let Err(error) = self.context.records.try_flush() {
            report.record_error = Some(error);
            report.record_retry = Some(self.context.records.clone());
        }
        self.teardown();
        self.close_report = Some(report.clone());
        report
    }
    pub fn close_report(&self) -> Option<&ComponentCloseReport> {
        self.close_report.as_ref()
    }
    fn teardown(&mut self) {
        self.context.wake_scope.close();
        let _entry = self.context.wake_scope.enter();
        let root = self.root.take();
        let registry = self.registry.take();
        drop_component_owned(root, registry);
    }
}
impl Drop for AppComponent {
    fn drop(&mut self) {
        // Failed setup must not save a supplied bad View over the persisted envelope.
        self.teardown();
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
