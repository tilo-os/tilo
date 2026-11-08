//! What a running app may still capture.
//! The broker hands out a stream when the person allows one; taking the permission back ends the
//! stream it handed out and stops answering the app's next ask. Neither closes the app: it loses
//! the stream, not its window.

use tilo_desktop_model::DesktopTree;
use tilo_portal::capture_broker::CaptureKind;

/// The device a workspace capability names for a capture kind, or `None` for a kind no workspace
/// grants: a screen share is the person's answer to a portal question, and it ends with the
/// session, the lock, or the person stopping it, never with a workspace's capability list.
#[must_use]
pub(crate) fn device_scope(kind: CaptureKind) -> Option<&'static str> {
    match kind {
        CaptureKind::Camera => Some("camera"),
        CaptureKind::Microphone => Some("microphone"),
        CaptureKind::Screen | CaptureKind::SystemAudio => None,
    }
}

/// Whether app `app_id` , running in workspace `workspace` (named by its room id, as the portal
/// names it), may still capture `kind` at `now` .
#[must_use]
pub(crate) fn still_allowed(
    tree: &DesktopTree,
    app_id: &str,
    workspace: &str,
    kind: CaptureKind,
    now: u64,
) -> bool {
    let Some(device) = device_scope(kind) else {
        return true;
    };
    tree.workspaces
        .iter()
        .find(|held| held.id().to_string() == workspace)
        .is_some_and(|held| held.grants_device_at(app_id, device, now))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]
mod tests {
    use super::*;
    use collab_types::caps::{Access, CapOverride, Scope, Subject};
    use tilo_desktop_model::Workspace;

    fn desk(grant: CapOverride) -> DesktopTree {
        let mut workspace = Workspace::create("Studio");
        workspace.sandbox.push(grant);
        DesktopTree {
            workspaces: vec![workspace],
            ..DesktopTree::default()
        }
    }

    fn camera(app: &str) -> CapOverride {
        CapOverride::grant(
            Subject::app(app),
            Scope::Device("camera".into()),
            Access::Read,
        )
    }

    #[test]
    fn taking_the_camera_away_stops_the_next_ask_but_not_any_other_device() {
        let mut tree = desk(camera("so.tilo.meet"));
        let room = tree.workspaces[0].id().to_string();
        assert!(still_allowed(
            &tree,
            "so.tilo.meet",
            &room,
            CaptureKind::Camera,
            10
        ));
        assert!(
            !still_allowed(&tree, "so.tilo.meet", &room, CaptureKind::Microphone, 10),
            "the camera grant is not a microphone grant"
        );
        let id = tree.workspaces[0].sandbox[0].id.clone();
        assert!(tree.workspaces[0].revoke_sandbox(&id));
        assert!(
            !still_allowed(&tree, "so.tilo.meet", &room, CaptureKind::Camera, 10),
            "the revoked camera is refused at once"
        );
        assert!(
            still_allowed(&tree, "so.tilo.meet", &room, CaptureKind::Screen, 10),
            "a screen share is the person's answer, not a workspace grant"
        );
    }

    #[test]
    fn an_expired_grant_stops_capturing_and_another_workspace_does_not_count() {
        let tree = desk(camera("so.tilo.meet").expiring(collab_types::caps::Expiry::AtUnix(20)));
        let room = tree.workspaces[0].id().to_string();
        assert!(still_allowed(
            &tree,
            "so.tilo.meet",
            &room,
            CaptureKind::Camera,
            19
        ));
        assert!(
            !still_allowed(&tree, "so.tilo.meet", &room, CaptureKind::Camera, 21),
            "a lapsed grant stops capturing without anyone revoking it"
        );
        assert!(
            !still_allowed(
                &tree,
                "so.tilo.meet",
                "another-room",
                CaptureKind::Camera,
                19
            ),
            "a grant in one workspace says nothing about another"
        );
    }
}
