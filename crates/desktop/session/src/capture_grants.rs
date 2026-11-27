//! What stops a capture under way: the person taking their answer back, on this device or
//! on another of theirs, and the app's workspace refusing it the device. Either ends the stream,
//! the input capture or the remote control the app holds; neither closes the app.

use std::collections::BTreeSet;

use tilo_action_policy::Answer;
use tilo_desktop_model::Layout;
use tilo_peer_state::{DeviceGrants, Refusal};
use tilo_permissions::caps::DeviceCap;
use tilo_permissions::caps::Effect;
use tilo_permissions::caps::Scope;

/// What the workspaces of `tree` refuse their apps of the person's devices at `now` (in
/// milliseconds), as `(workspace, app, device)`: workspaces are named by their id, as the portal
/// names them.
#[must_use]
pub(crate) fn refusals(tree: &Layout, now: u64) -> BTreeSet<Refusal> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| {
            let id = workspace.id().to_string();
            workspace.sandbox.iter().filter_map(move |cap| {
                let Scope::Device(device) = cap.scope else {
                    return None;
                };
                let app = cap.app.as_ref()?;
                (cap.effect == Effect::Restrict
                    && device.is_personal()
                    && cap.is_live_at(Some(now)))
                .then(|| (id.clone(), app.as_str().to_owned(), device))
            })
        })
        .collect()
}

/// Whether what app `app` captures of `device` in `workspace` ends: the person took back what it
/// was allowed (`taken`, from [`DeviceGrants::taken_back`]), or its workspace refuses it.
#[must_use]
pub(crate) fn ended(
    grants: &DeviceGrants,
    taken: &BTreeSet<(String, DeviceCap)>,
    app: &str,
    workspace: Option<&str>,
    device: DeviceCap,
) -> bool {
    taken.contains(&(app.to_owned(), device))
        || grants.decide(app, workspace, device) == Answer::Deny
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::Grantee;
    use tilo_action_types::Lifetime;
    use tilo_desktop_model::Workspace;
    use tilo_permissions::caps::Access;
    use tilo_permissions::caps::CapOverride;

    fn shell(caps: Vec<CapOverride>) -> Layout {
        let mut workspace = Workspace::new(tilo_desktop_model::WorkspaceId::new(), "Studio");
        workspace.sandbox = caps;
        Layout {
            workspaces: vec![workspace],
            ..Layout::default()
        }
    }

    fn device(effect: Effect, device: DeviceCap) -> CapOverride {
        CapOverride::new(
            Grantee::person("ada"),
            Scope::Device(device),
            effect,
            Access::Read,
        )
        .through("so.tilo.meet")
    }

    /// A workspace refuses its apps the person's devices, and never allows one: a grant of the
    /// camera in it opens nothing, a restriction of the screen ends a share and refuses the next.
    #[test]
    fn a_workspace_refuses_but_never_allows_a_device() {
        let tree = shell(vec![
            device(Effect::Grant, DeviceCap::Camera),
            device(Effect::Restrict, DeviceCap::Screen),
            device(Effect::Restrict, DeviceCap::Input).lasting(Lifetime::Until(5)),
        ]);
        let workspace = tree.workspaces[0].id().to_string();
        let refused = refusals(&tree, 10);
        assert_eq!(
            refused,
            [(
                workspace.clone(),
                "so.tilo.meet".to_owned(),
                DeviceCap::Screen
            )]
            .into(),
            "a lapsed restriction refuses nothing"
        );
        let grants = DeviceGrants::unkept();
        grants.refuse_in(refused);
        let none = BTreeSet::new();
        assert_eq!(
            grants.decide("so.tilo.meet", Some(&workspace), DeviceCap::Camera),
            Answer::Ask
        );
        assert!(ended(
            &grants,
            &none,
            "so.tilo.meet",
            Some(&workspace),
            DeviceCap::Screen
        ));
        assert!(!ended(
            &grants,
            &none,
            "so.tilo.meet",
            None,
            DeviceCap::Screen
        ));
        assert!(!ended(
            &grants,
            &none,
            "so.tilo.chat",
            Some(&workspace),
            DeviceCap::Screen
        ));
    }

    /// Taking an answer back stops the capture it allowed, and only for that app and device; a capture
    /// started on a one-time answer has nothing to take back.
    #[test]
    fn taking_an_answer_back_stops_the_capture_it_allowed() {
        let grants = DeviceGrants::unkept();
        for device in [DeviceCap::Camera, DeviceCap::Input] {
            grants.answer("so.tilo.meet", device, Answer::Allow, Lifetime::Session);
        }
        assert!(grants.taken_back().is_empty());
        assert!(grants.revoke_here("so.tilo.meet", DeviceCap::Input));
        let taken = grants.taken_back();
        assert!(ended(
            &grants,
            &taken,
            "so.tilo.meet",
            None,
            DeviceCap::Input
        ));
        assert!(!ended(
            &grants,
            &taken,
            "so.tilo.meet",
            None,
            DeviceCap::Camera
        ));
        assert!(!ended(
            &grants,
            &taken,
            "so.tilo.meet",
            None,
            DeviceCap::Screen
        ));
    }
}
