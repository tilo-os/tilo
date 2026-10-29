//! The desktop's own surface apps, prepared in memory and private.

use std::rc::Rc;

use super::{
    AppComponent, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use crate::application_view::InstanceViewStore;
use crate::launch::LaunchError;
use crate::permissions::{PermissionContext, SharedPermissions, UserId};

/// Prepare `definition` as one of the desktop's own surface apps (the
/// bar, the banners, Quick Settings, a sheet): as `person` on `device`, its
/// record and view kept in memory under `room`, its window private, so
/// nothing of it is written to disk or offered beyond this device. `lend`
/// hands it what its host lends it.
///
/// # Errors
/// The app refuses the context.
pub fn prepare_private_surface(
    definition: &'static ApplicationDefinition,
    device: &str,
    person: &str,
    room: &str,
    instance: u64,
    lend: impl FnOnce(&mut HostContext),
) -> Result<AppComponent, LaunchError> {
    let store = Rc::new(durable_store::MemoryStore::new());
    let records = crate::record::RecordOwners::with_store(store.clone())
        .open(definition.app_id(), room)
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let views = Rc::new(
        InstanceViewStore::with_store(store, device, room)
            .map_err(|error| LaunchError::Operation(error.to_string()))?,
    );
    let target = LaunchTarget::New;
    let mut context = HostContext::prepare(
        definition,
        HostContextInputs {
            records,
            views,
            device_id: device.to_owned(),
            instance_id: instance,
            permissions: SharedPermissions::new(PermissionContext::member(
                person,
                crate::permissions::Role::Owner,
            )),
            author: UserId(person.to_owned()),
            action_context: tilo_actions::ActionContext::default(),
        },
        &target,
    )?;
    context.provide_private();
    lend(&mut context);
    definition.instantiate(context, target, RestoreInput::None)
}
