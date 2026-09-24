//! Ordinary application workers shared by durable native and hosted owners.
//! The host supplies current authority and committed bytes; this module owns
//! only the real application runner and its original recovery custody.
use crate::{
    application::{ApplicationDefinition, ComponentCloseReport, PreparedComponentInputs},
    collaboration::CollaborationManager,
    launch::LaunchError,
    permissions::PermissionContext,
    portable_storage::{PendingWrite, PortableStorage},
};
use std::{cell::RefCell, rc::Rc};

#[cfg(unix)]
pub mod private_actions;

#[cfg(unix)]
pub mod managed_sender;

pub use crate::invocation_scope::{
    PublishedInvocationAuthorizer, PublishedInvocationLease, PublishedInvocationRequest,
};

pub struct HostWorker {
    app: crate::App,
    permissions: Rc<RefCell<PermissionContext>>,
}
impl HostWorker {
    pub fn mount(
        definition: &ApplicationDefinition,
        inputs: PreparedComponentInputs,
        collaboration: CollaborationManager,
        permissions: Rc<RefCell<PermissionContext>>,
    ) -> Result<Self, LaunchError> {
        Self::mount_inner(definition, inputs, collaboration, permissions)
    }
    /// Bind funded negative custody before the actual factory. This does not
    /// activate a reader, authenticate a Source, or install disclosure authority.
    pub fn mount_with_retirement(
        definition: &ApplicationDefinition,
        mut inputs: PreparedComponentInputs,
        collaboration: CollaborationManager,
        permissions: Rc<RefCell<PermissionContext>>,
        retirement: crate::semantic_document_read::retirement::MountRetirement,
    ) -> Result<Self, LaunchError> {
        inputs.context.bind_retirement(retirement)?;
        Self::mount_inner(definition, inputs, collaboration, permissions)
    }
    fn mount_inner(
        definition: &ApplicationDefinition,
        inputs: PreparedComponentInputs,
        mut collaboration: CollaborationManager,
        permissions: Rc<RefCell<PermissionContext>>,
    ) -> Result<Self, LaunchError> {
        // The actual admitted record exists before the factory. Defaults or
        // migrations performed by the factory cannot become a trusted baseline.
        inputs.context.require_invocation_provenance()?;
        collaboration.require_document_authorization();
        let mut component =
            definition.instantiate(inputs.context, inputs.target, inputs.restore)?;
        let binding = component.callback_binding();
        if let Err(error) = binding
            .records
            .require_invocation_provenance(&binding.checkout)
        {
            return Err(component.setup_error(LaunchError::Operation(error)));
        }
        let mut app = crate::App::from_component_with_collaboration(
            component,
            inputs.runner_inputs,
            collaboration,
        )?;
        if let Err(error) = binding
            .records
            .require_invocation_provenance(&binding.checkout)
        {
            app.close_components();
            return Err(LaunchError::ComponentSetup {
                source: Box::new(LaunchError::Operation(error)),
                record: binding.checkout,
                records: binding.records,
                reports: app.take_component_close_reports(),
            });
        }
        app.require_published_invocation_authorizer();
        Ok(Self { app, permissions })
    }
    /// Install once for the exact host worker connection. Required dispatch
    /// derives explicit per-job scopes and carries original contributors into
    /// document publication; the concrete final owner must evaluate them again.
    pub fn install_published_invocation_authorizer(
        &mut self,
        authorizer: PublishedInvocationAuthorizer,
    ) -> Result<(), String> {
        self.app.install_published_invocation_authorizer(authorizer)
    }
    /// Recompute time-sensitive permissions in the host before every pump.
    pub fn pump(&mut self, current: PermissionContext) -> Result<(), String> {
        self.app.published_invocation_health()?;
        *self.permissions.borrow_mut() = current;
        self.app.pump_hosted_model();
        self.app.published_invocation_health()?;
        if !self.app.collaboration_is_connected() {
            return Err("application collaboration transport failed; outcome unknown".into());
        }
        Ok(())
    }
    pub fn close(&mut self) -> Vec<ComponentCloseReport> {
        self.app.close_components();
        self.app.take_component_close_reports()
    }
    pub fn app_mut(&mut self) -> &mut crate::App {
        &mut self.app
    }
    pub fn permissions(&self) -> &Rc<RefCell<PermissionContext>> {
        &self.permissions
    }
}

/// Preserve the original failed component's record/view retry owners.
/// Hosts retain these reports until durable acknowledgement or recovery export.
pub fn retain_setup_error(error: LaunchError, reports: &mut Vec<ComponentCloseReport>) {
    if let LaunchError::ComponentSetup {
        source,
        record,
        records,
        reports: nested,
    } = error
    {
        reports.extend(nested);
        reports.push(ComponentCloseReport {
            recovery: None,
            view_error: None,
            view_retry: None,
            record_error: None,
            record_retry: Some(records),
            record_retry_target: Some(record.id),
            teardown_error: None,
        });
        retain_setup_error(*source, reports);
    }
}

/// `committed` must come from the host's actual durable owner, in the same
/// namespace as `write`. Enqueue, a signed envelope or a network ACK is not
/// committed state. Cache revision checks still protect later pending writes.
pub fn acknowledge_document(
    cache: &PortableStorage,
    write: &PendingWrite,
    committed: &[u8],
    publication_permitted: bool,
) -> Result<bool, String> {
    let mut document = crate::automerge::AutoCommit::load(committed).map_err(|e| e.to_string())?;
    let mut pending =
        crate::automerge::AutoCommit::load(&write.value.bytes).map_err(|e| e.to_string())?;
    let heads = pending.get_heads();
    if heads
        .iter()
        .all(|head| document.get_change_by_hash(head).is_some())
        && publication_permitted
    {
        cache.acknowledge(write, Ok(()))?;
        Ok(true)
    } else {
        Ok(false)
    }
}
