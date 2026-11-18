//! What a running app may still capture.
//! The broker hands out a stream when the person allows one; taking the permission back ends the
//! stream it handed out and stops answering the app's next ask. Neither closes the app: it loses
//! the stream, not its window.

use collab_types::caps::{CapDecision, DeviceCap, ResourceRef};
use tilo_action_policy::{Acting, MemberId};
use tilo_desktop_model::Layout;
use tilo_portal::capture_broker::CaptureKind;

/// Whether app `app_id` , running for `person` in workspace `workspace` (named by its id, as the
/// portal names it), may still capture `kind` at `now` : the workspace's sandbox still grants that
/// person the device through that app, and no restriction takes it away.
///
/// A screen share or the sound the machine plays is the person's answer to a portal question, and
/// ends with the session, the lock, or the person stopping it, never with a workspace's list.
#[must_use]
pub(crate) fn still_allowed(
    tree: &Layout,
    person: &str,
    app_id: &str,
    workspace: &str,
    kind: CaptureKind,
    now: u64,
) -> bool {
    let device = match kind {
        CaptureKind::Camera => DeviceCap::Camera,
        CaptureKind::Microphone => DeviceCap::Microphone,
        CaptureKind::Screen | CaptureKind::SystemAudio => return true,
    };
    let Some(member) = MemberId::parsed_person(person) else {
        return false;
    };
    let acting = Acting::member(member).in_app(app_id);
    tree.workspaces
        .iter()
        .find(|held| held.id().to_string() == workspace)
        .is_some_and(|held| {
            held.sandbox_decide_as_at(&acting, &ResourceRef::Device(device), device.access(), now)
                == CapDecision::Allow
        })
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
    use collab_types::caps::{Access, CapOverride, Scope};
    use tilo_action_policy::Grantee;
    use tilo_desktop_model::Workspace;

    fn shell(grant: CapOverride) -> Layout {
        let mut workspace = Workspace::new(tilo_desktop_model::WorkspaceId::new(), "Studio");
        workspace.sandbox.push(grant);
        Layout {
            workspaces: vec![workspace],
            ..Layout::default()
        }
    }

    fn camera(app: &str) -> CapOverride {
        CapOverride::grant(
            Grantee::person("ada"),
            Scope::Device(DeviceCap::Camera),
            Access::Read,
        )
        .through(app)
    }

    tilo_test_env::scenario! {
        fn a_capture_is_allowed_only_while_its_workspaces_grant_stands;

        phase taking_the_camera_away_stops_the_next_ask_but_not_any_other_device;
        {
            let mut tree = shell(camera("so.tilo.meet"));
            let workspace = tree.workspaces[0].id().to_string();
            assert!(still_allowed(
                &tree,
                "ada",
                "so.tilo.meet",
                &workspace,
                CaptureKind::Camera,
                10
            ));
            assert!(
                !still_allowed(
                    &tree,
                    "ada",
                    "so.tilo.meet",
                    &workspace,
                    CaptureKind::Microphone,
                    10
                ),
                "the camera grant is not a microphone grant"
            );
            let id = tree.workspaces[0].sandbox[0].id.clone();
            assert!(tree.workspaces[0].revoke_sandbox(&id));
            assert!(
                !still_allowed(&tree, "ada", "so.tilo.meet", &workspace, CaptureKind::Camera, 10),
                "the revoked camera is refused at once"
            );
            assert!(
                still_allowed(&tree, "ada", "so.tilo.meet", &workspace, CaptureKind::Screen, 10),
                "a screen share is the person's answer, not a workspace grant"
            );
        }

        phase an_expired_grant_stops_capturing_and_another_workspace_does_not_count;
        {
            let tree = shell(camera("so.tilo.meet").lasting(tilo_action_policy::Lifetime::Until(20_000)));
            let workspace = tree.workspaces[0].id().to_string();
            assert!(still_allowed(
                &tree,
                "ada",
                "so.tilo.meet",
                &workspace,
                CaptureKind::Camera,
                19
            ));
            assert!(
                !still_allowed(&tree, "ada", "so.tilo.meet", &workspace, CaptureKind::Camera, 21),
                "a lapsed grant stops capturing without anyone revoking it"
            );
            assert!(
                !still_allowed(
                    &tree,
                    "ada",
                    "so.tilo.meet",
                    "another-workspace",
                    CaptureKind::Camera,
                    19
                ),
                "a grant in one workspace says nothing about another"
            );
        }

        phase another_persons_grant_does_not_let_the_app_capture;
        {
            let tree = shell(camera("so.tilo.meet"));
            let workspace = tree.workspaces[0].id().to_string();
            assert!(
                !still_allowed(&tree, "bob", "so.tilo.meet", &workspace, CaptureKind::Camera, 10),
                "ada's grant is not bob's"
            );
        }
    }
}
