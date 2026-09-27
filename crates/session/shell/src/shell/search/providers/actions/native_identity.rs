//! Custody selection is not a native governance mutation.
use crate::shell::{native_workspace_owner::NativeWorkspaceOwner, state::RoomId};
use tilo_user_identity::DeviceCert;

pub(super) fn fence(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    selected: Option<&DeviceCert>,
) -> Result<(), String> {
    let same = owner
        .with_current(room, |view| Ok(selected == Some(view.device())))
        .unwrap_or(false);
    if !same && owner.contains_room(room) {
        owner.invalidate_identity(room);
    }
    if let Some(pending) = owner.pending_enrollment(room)? {
        let intent = crate::network::invite::VerifiedNativeEnrollmentIntent::restore(
            pending.intent_bytes(),
        )?;
        if selected != Some(intent.local_device()) {
            owner.invalidate_identity(room);
            owner.cancel_enrollment(room, pending.id())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_selection_changes_custody_without_rewriting_governance() {
        let (directory, owner, room, _, _) =
            crate::shell::native_workspace_owner::test_enrolled_owner();
        let current = owner
            .with_current(room, |view| Ok(view.device().clone()))
            .unwrap();
        let bytes = std::fs::read(directory.path().join("state.json")).unwrap();
        fence(&owner, room, Some(&current)).unwrap();
        assert!(
            owner.with_current(room, |_| Ok(())).is_ok(),
            "same identity must not tear down authority"
        );
        let other = owner
            .with_current(room, |view| Ok(view.governor().clone()))
            .unwrap();
        assert_ne!(other, current);
        fence(&owner, room, Some(&other)).unwrap();
        assert!(
            owner.with_current(room, |_| Ok(())).is_err(),
            "unavailable selected custody fences old authority"
        );
        assert_eq!(
            std::fs::read(directory.path().join("state.json")).unwrap(),
            bytes,
            "selection never grants native policy"
        );
    }
}
