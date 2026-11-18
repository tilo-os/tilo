//! What a running app may still capture.
//! The broker hands out a stream when the person allows one; taking the permission back ends the
//! stream it handed out and stops answering the app's next ask. Neither closes the app: it loses
//! the stream, not its window.

use collab_types::caps::{DeviceCap, Effect, Scope};
use tilo_action_policy::Answer;
use tilo_desktop_model::Layout;
use tilo_peer_state::DeviceGrants;
use tilo_portal::capture_broker::CaptureKind;

/// Whether app `app_id` in workspace `workspace` (named by its id, as the portal names it) may
/// still capture `kind` at `now` (in milliseconds): the person's answer for this device still
/// allows it, and the workspace does not refuse that device to the app.
///
/// A screen share or the sound the machine plays is the person's answer to a portal question, and
/// ends with the session, the lock, or the person stopping it.
#[must_use]
pub(crate) fn still_allowed(
    tree: &Layout,
    grants: &DeviceGrants,
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
    let refused = tree
        .workspaces
        .iter()
        .filter(|held| held.id().to_string() == workspace)
        .flat_map(|held| &held.sandbox)
        .any(|cap| {
            cap.effect == Effect::Restrict
                && cap.scope == Scope::Device(device)
                && cap.names_app(app_id)
                && cap.is_live_at(Some(now))
        });
    let ceiling = if refused { Answer::Deny } else { Answer::Allow };
    grants.decide(app_id, device, ceiling) == Answer::Allow
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
    use collab_types::caps::{Access, CapOverride};
    use tilo_action_policy::{Grantee, Lifetime};
    use tilo_desktop_model::Workspace;

    fn shell(caps: Vec<CapOverride>) -> Layout {
        let mut workspace = Workspace::new(tilo_desktop_model::WorkspaceId::new(), "Studio");
        workspace.sandbox = caps;
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
        fn a_capture_is_allowed_only_while_the_persons_answer_stands;

        phase taking_the_answer_back_stops_the_next_ask_but_not_any_other_device;
        {
            let tree = shell(Vec::new());
            let workspace = tree.workspaces[0].id().to_string();
            let grants = DeviceGrants::unkept();
            grants.answer("so.tilo.meet", DeviceCap::Camera, Answer::Allow, Lifetime::Session).unwrap();
            assert!(still_allowed(&tree, &grants, "so.tilo.meet", &workspace, CaptureKind::Camera, 10));
            assert!(
                !still_allowed(&tree, &grants, "so.tilo.meet", &workspace, CaptureKind::Microphone, 10),
                "the camera answer is not a microphone answer"
            );
            assert!(grants.revoke("", "so.tilo.meet", DeviceCap::Camera).unwrap());
            assert!(
                !still_allowed(&tree, &grants, "so.tilo.meet", &workspace, CaptureKind::Camera, 10),
                "the revoked camera is refused at once"
            );
            assert!(
                still_allowed(&tree, &grants, "so.tilo.meet", &workspace, CaptureKind::Screen, 10),
                "a screen share is the person's answer to its own question"
            );
        }

        phase a_workspace_cannot_allow_the_camera_but_can_refuse_it;
        {
            let tree = shell(vec![camera("so.tilo.meet")]);
            let workspace = tree.workspaces[0].id().to_string();
            let grants = DeviceGrants::unkept();
            assert!(
                !still_allowed(&tree, &grants, "so.tilo.meet", &workspace, CaptureKind::Camera, 10),
                "a member's grant in the workspace never opens this device's camera"
            );
            grants.answer("so.tilo.meet", DeviceCap::Camera, Answer::Allow, Lifetime::Session).unwrap();
            let refusing = shell(vec![CapOverride::new(
                Grantee::person("ada"),
                Scope::Device(DeviceCap::Camera),
                Effect::Restrict,
                Access::Read,
            )
            .through("so.tilo.meet")]);
            let workspace = refusing.workspaces[0].id().to_string();
            assert!(
                !still_allowed(&refusing, &grants, "so.tilo.meet", &workspace, CaptureKind::Camera, 10),
                "the workspace refuses its apps the camera the person allowed"
            );
        }
    }
}
