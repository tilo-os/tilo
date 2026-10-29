//! Mounting the panel inside the desktop's own process.

use std::rc::Rc;

use ui::application::{AppComponent, HostContext, HostContextInputs, LaunchTarget, RestoreInput};
use ui::application_view::InstanceViewStore;
use ui::framework::ResourceApp;
use ui::launch::LaunchError;
use ui::permissions::{PermissionContext, Role, SharedPermissions, UserId};
use ui::record::RecordOwners;

use crate::host::{Lent, PanelHost};
use crate::resource::QuickSettingsApp;

/// The panel as a component the desktop draws, its readings and effects
/// reaching `host`. Its record is kept in memory and its window is private:
/// the panel is this device's and is gone with the session, so nothing of it
/// is written to disk or offered beyond this device.
///
/// # Errors
/// The component could not be prepared.
pub fn prepare(
    host: Rc<dyn PanelHost>,
    device: &str,
    person: &str,
    instance: u64,
) -> Result<AppComponent, LaunchError> {
    let definition = ui::framework::definition::<QuickSettingsApp>();
    let store = Rc::new(durable_store::MemoryStore::new());
    let room = "quick-settings";
    let records = RecordOwners::with_store(store.clone())
        .open(QuickSettingsApp::ID, room)
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
            permissions: SharedPermissions::new(PermissionContext::member(person, Role::Owner)),
            author: UserId(person.to_owned()),
            action_context: ui::tilo_actions::ActionContext::default(),
        },
        &target,
    )?;
    context.provide_private();
    context.lend(Lent(host));
    definition.instantiate(context, target, RestoreInput::None)
}
