//! Mounting the bar where the desktop has no in-process apps of its own
//! to mount it among (a test's or a capture's chrome).

use std::rc::Rc;

use ui::application::{AppComponent, HostContext, HostContextInputs, LaunchTarget, RestoreInput};
use ui::application_view::InstanceViewStore;
use ui::framework::ResourceApp as _;
use ui::launch::LaunchError;
use ui::permissions::{PermissionContext, Role, SharedPermissions, UserId};
use ui::record::RecordOwners;

use crate::link::{BarService, Link};
use crate::resource::TopBarApp;

/// The bar as a component the desktop draws, showing what it publishes
/// on `link`. Its record is kept in memory and its window is private:
/// nothing of it is written to disk or offered beyond this device.
///
/// # Errors
/// The component could not be prepared.
pub fn prepare(link: Link, device: &str, person: &str) -> Result<AppComponent, LaunchError> {
    let definition = crate::definition();
    let store = Rc::new(durable_store::MemoryStore::new());
    let room = "topbar";
    let records = RecordOwners::with_store(store.clone())
        .open(TopBarApp::ID, room)
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
            instance_id: 0,
            permissions: SharedPermissions::new(PermissionContext::member(person, Role::Owner)),
            author: UserId(person.to_owned()),
            action_context: ui::tilo_actions::ActionContext::default(),
        },
        &target,
    )?;
    context.provide_private();
    context.lend(BarService { link });
    definition.instantiate(context, target, RestoreInput::None)
}
