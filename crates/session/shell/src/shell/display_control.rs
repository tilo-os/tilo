//! Display intent admitted through the exact private Settings connection.
//! KMS stays in the compositor; this service writes its existing bounded
//! preview transaction through the session-owned settings source.
use super::state::{RoomId, WeakWorkspaceTreeHandle};
use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_actor, role_caps};
use settings_store::{
    SettingsSource,
    display_control::{DisplayCommand, MAX_DISPLAY_CONTROL_BYTES},
};
use std::sync::Arc;
use tilo_action_broker::services::{DisplayControlContext, DisplayControlServices};

pub(crate) struct NativeDisplayControl {
    pub tree: WeakWorkspaceTreeHandle,
    pub source: Arc<dyn SettingsSource>,
}

fn device_owner(root: [u8; 32], device: [u8; 32]) -> Result<bool, String> {
    let registry = crate::identity_registry::IdentityRegistryStore::from_env()
        .map_err(|error| error.to_string())?;
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    let certificate = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .map_err(|error| error.to_string())?;
    if certificate.user_public_key != root || certificate.device_public_key != device {
        return Ok(false);
    }
    tilo_identity_host::provisioning::active_identity_owns_device(registry.identity_dir(), &paths)
        .map_err(|error| error.to_string())
}

fn authorize(
    view: &super::native_workspace_owner::NativeCommittedView,
    root_key: [u8; 32],
    device: [u8; 32],
    room: &str,
) -> Result<(), String> {
    if view.device().user_public_key != root_key || view.device().device_public_key != device {
        return Err("The Settings worker is not on this accepted owner device.".into());
    }
    let now = std::time::SystemTime::now();
    let (role, capabilities, person) = view.peer().permissions(device, now)?;
    let actor = Actor::person(Some(role), person).in_room(room);
    let seconds = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "The owner policy clock is unavailable.")?
        .as_secs();
    if decide_actor(
        role_caps(role).is_some_and(|verb| verb.implies(Verb::Admin)),
        Verb::Admin,
        &actor,
        &ResourceRef::App("so.tilo.settings".into()),
        &capabilities,
        Some(seconds),
    ) != CapDecision::Allow
    {
        return Err("The current owner policy does not permit display changes.".into());
    }
    Ok(())
}

impl DisplayControlServices for NativeDisplayControl {
    fn command(&self, context: &DisplayControlContext, bytes: &[u8]) -> Result<Vec<u8>, String> {
        if bytes.len() > MAX_DISPLAY_CONTROL_BYTES {
            return Err("The display request exceeds its wire bound.".into());
        }
        let command: DisplayCommand =
            serde_json::from_slice(bytes).map_err(|_| "The display request is invalid.")?;
        let mutates = command.mutates();
        let execute = || {
            let observed = settings_store::hardware::DisplayHardwareSnapshot::load()
                .ok_or("The current compositor display state is unavailable.")?;
            let reply =
                settings_store::display_control::execute(self.source.as_ref(), observed, command)?;
            let reply = serde_json::to_vec(&reply)
                .map_err(|_| "The display reply could not be encoded.")?;
            if reply.len() > MAX_DISPLAY_CONTROL_BYTES {
                return Err("The display reply exceeds its wire bound.".into());
            }
            Ok(reply)
        };
        if !mutates {
            return execute();
        }
        if !device_owner(context.root(), context.device())? {
            return Err("This session does not own the device.".into());
        }
        let room = RoomId::parse(context.room()).ok_or("The admitted Settings room is invalid.")?;
        let tree = self
            .tree
            .upgrade()
            .ok_or("The current desktop owner has retired.")?;
        if !tree.read().requires_native_owner(room) {
            let person =
                tilo_action_broker::collab_broker_capnp::principal_id_for_root(context.root());
            return super::launcher::action_authority::with_ordinary_person_admin(
                &tree, room, &person, execute,
            );
        }
        let owner = tree
            .read()
            .installed_native_owner()
            .ok_or("The current native owner is unavailable.")?;
        owner.with_current(room, |view| {
            authorize(view, context.root(), context.device(), context.room())?;
            execute()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_mutation_requires_the_actual_current_native_device_and_admin_policy() {
        let (_directory, owner, room, _now, _key) =
            super::super::native_workspace_owner::test_enrolled_owner();
        owner
            .with_current(room, |view| {
                let root_key = view.device().user_public_key;
                let device = view.device().device_public_key;
                let room = room.to_string();
                // The fixture is a joined guest, not a device owner. Exact native
                // certificate equality alone must never promote its accepted role.
                let role = view
                    .peer()
                    .permissions(device, std::time::SystemTime::now())?
                    .0;
                assert_eq!(role, collab_types::Role::Editor);
                assert!(
                    authorize(view, root_key, device, &room).is_err(),
                    "a real accepted Editor has no display Admin authority"
                );
                assert!(authorize(view, [0; 32], device, &room).is_err());
                assert!(authorize(view, root_key, [0; 32], &room).is_err());
                Ok(())
            })
            .unwrap();
        owner.fence_all();
        assert!(
            owner
                .with_current(room, |view| authorize(
                    view,
                    view.device().user_public_key,
                    view.device().device_public_key,
                    &room.to_string()
                ))
                .is_err()
        );
    }
}
