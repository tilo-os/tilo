//! The shell surface apps, prepared in memory and private.

use std::rc::Rc;

use super::{
    AppComponent, ApplicationDefinition, LaunchTarget, ProcessContext, ProcessContextInputs,
    RestoreInput,
};
use crate::application_view::InstanceViewStore;
use crate::launch_error::LaunchError;
use tilo_ui_base::{permissions::MemberId, permissions::SharedPermissions};

/// Prepare `definition` as one of the shell surface apps (the
/// bar, the banners, Quick Settings, a sheet): as `person` on `device`, its
/// record and view kept in memory under `workspace`, its window private, so
/// nothing of it is written to disk or offered beyond this device. `lend`
/// hands it what its host lends it.
///
/// # Errors
/// The app refuses the context.
pub fn prepare_private_surface(
    definition: &'static ApplicationDefinition,
    device: &str,
    person: &str,
    workspace: &str,
    instance: u64,
    lend: impl FnOnce(&mut ProcessContext),
) -> Result<AppComponent, LaunchError> {
    let records = crate::resource::ResourceOwners::in_memory()
        .open(definition.app_id(), workspace)
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let views = Rc::new(
        InstanceViewStore::in_memory(definition.app_id(), device, workspace)
            .map_err(|error| LaunchError::Operation(error.to_string()))?,
    );
    let target = LaunchTarget::New;
    let mut context = ProcessContext::prepare(
        definition,
        ProcessContextInputs {
            records,
            views,
            device_id: device.to_owned(),
            instance_id: instance,
            permissions: SharedPermissions::default(),
            author: MemberId::person(person),
            action_context: tilo_actions::ActionContext::default(),
        },
        &target,
    )?;
    context.provide_private();
    // Its record is kept in this process alone, in a workspace no daemon keeps: the person owns
    // it, and what they may do there is decided as their device decides an owner's actions.
    #[cfg(feature = "model")]
    context.provide_harness_device(
        crate::harness_device::HarnessDevice::member_of(
            workspace,
            &MemberId::person(person),
            tilo_ui_base::permissions::Role::Owner,
        )
        .shown(),
    );
    lend(&mut context);
    definition.instantiate(context, target, RestoreInput::None)
}
