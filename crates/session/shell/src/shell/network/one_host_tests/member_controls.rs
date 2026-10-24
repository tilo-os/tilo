//! The workspace settings' controls on the device that hosts the workspace:
//! the role a person picks for a member, their removal, a sandbox grant or
//! an app's action given and taken back, is asked of the room's governor,
//! and nothing changes here until the governor committed it. Then it shows
//! on every device of the room.

use std::collections::HashMap;
use std::sync::Arc;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_user_identity::DeviceCapabilities;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Affine, Point, Rect};

use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::chrome_state::{Chrome, WsTab};
use crate::shell::overlay_host::ShellOverlayHost;
use crate::shell::state::{
    Access, CapOverride, GovernanceRequest, ResourceRef, RoomId, Scope, Subject,
    WorkspaceAuthority as _,
};

/// The settings' members tab of `device`'s active workspace, laid out and
/// drawn, over the device's own tree.
fn members_tab(device: &Device) -> (ShellOverlayHost, Chrome) {
    let (mut host, chrome) =
        crate::shell::build_tilo_overlay_host_with_handle(device.tree.clone(), false);
    {
        let mut state = chrome.borrow_mut();
        state.overlays.ws_settings = true;
        state.ws_settings_tab = WsTab::Members;
    }
    draw(&mut host);
    (host, chrome)
}

/// Lay `host` out on a 1600 by 900 screen and draw it, as a frame does.
fn draw(host: &mut ShellOverlayHost) {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = host.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
    let mut text = ui::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(1600.0),
                height: AvailableSpace::Definite(900.0),
            },
            |known, available, node_id, _, _| {
                node_map.get(&node_id).map_or(Size::ZERO, |&widget| {
                    // SAFETY: every pointer in the map is a widget of `host`,
                    // which outlives this layout pass.
                    unsafe { (*widget).measure(known, available, &mut text) }
                })
            },
        )
        .expect("the settings lay out");
    host.apply_layout(&taffy, node, Point::ZERO);
    host.draw(
        &mut vello::Scene::new(),
        Affine::IDENTITY,
        &mut text,
        &Theme::dark(),
        &ui::presence::PresenceCtx::default(),
    );
}

/// A pointer click in the middle of `rect`.
fn click(host: &mut ShellOverlayHost, rect: Rect) {
    let mut taffy = TaffyTree::<()>::new();
    let node = taffy.new_leaf(taffy::Style::default()).unwrap();
    let at = rect.center();
    for event in [
        ui::synthetic_input::cursor_moved(at.x, at.y),
        ui::synthetic_input::mouse_press(),
        ui::synthetic_input::mouse_release(),
    ] {
        ui::testing::Dispatch::new()
            .layout(&taffy, node)
            .send(host, &event);
    }
    draw(host);
}

/// The role and Remove controls of `person`'s row.
fn row_of(host: &ShellOverlayHost, person: &Person) -> (Rect, Rect) {
    let id = person.id();
    host.member_control_rects()
        .iter()
        .find(|(user, _, _)| *user == id)
        .map(|(_, role, remove)| (*role, *remove))
        .unwrap_or_else(|| panic!("{id} has a row"))
}

/// Whether `person` may change `room`'s contents outright, as `device`
/// sees the room.
fn may_change(device: &Device, room: RoomId, person: &Person) -> bool {
    device
        .tree
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .is_some_and(|workspace| {
            workspace.person_allowed(
                &person.id(),
                &tilo_action_policy::TreePath::root(),
                tilo_action_policy::ActionKind::Undoable,
            )
        })
}

#[test]
#[serial_test::serial]
#[allow(
    clippy::too_many_lines,
    reason = "one journey: a member's role changed, then the member removed"
)]
fn a_role_or_removal_picked_in_the_members_tab_is_the_governors_commit() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(71), Person::new(72));
    home.env().set("TILO_USER_ID", ada.id());
    let first = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A71,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let guest = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B71,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (local, room) = hosted_workspace(&first, &[&guest], "Projects");
    guest
        .join(&first.invite(local, "Grace", Some(grace.user())))
        .unwrap();
    let editor = Some(tilo_action_policy::Role::Editor);
    until(&[&first, &guest], "Grace in the workspace", || {
        (guest.holds(room)
            && first.role_of(room, &grace) == editor
            && guest.role_of(room, &grace) == editor)
            .then_some(())
    });
    first.tree.write().set_active(local);
    let (mut host, chrome) = members_tab(&first);

    // The owner's own row offers nothing to press.
    let (own_role, own_remove) = row_of(&host, &ada);
    assert_eq!(own_role.area(), 0.0, "the owner's role is not a choice");
    assert_eq!(own_remove.area(), 0.0, "no one removes themselves here");

    // Ada makes Grace a viewer: the chooser opens first, and the choice is
    // asked of the governor, not applied here.
    let (role, _) = row_of(&host, &grace);
    click(&mut host, role);
    assert!(host.member_sheet_is_open_for_tests());
    assert_eq!(
        host.member_sheet_option_rects_for_tests().len(),
        3,
        "the chooser offers Viewer, Commenter and Editor at once"
    );
    let viewer = host.member_sheet_option_rects_for_tests()[0];
    click(&mut host, viewer);
    assert_eq!(
        first.role_of(room, &grace),
        editor,
        "nothing changes here before the governor commits it"
    );
    let viewer = Some(tilo_action_policy::Role::Viewer);
    until(&[&first, &guest], "Grace a viewer on both devices", || {
        (first.role_of(room, &grace) == viewer && guest.role_of(room, &grace) == viewer)
            .then_some(())
    });
    assert!(
        !may_change(&first, room, &grace),
        "a viewer may no longer change the workspace"
    );
    assert!(first.tree.read().governance_refusals.is_empty());

    // Ada removes Grace: Remove asks to confirm, then the governor takes
    // her out.
    draw(&mut host);
    let (_, remove) = row_of(&host, &grace);
    click(&mut host, remove);
    assert!(host.member_sheet_is_open_for_tests());
    assert_eq!(
        first.role_of(room, &grace),
        viewer,
        "the first click only opens the confirmation"
    );
    let confirm = host.member_sheet_option_rects_for_tests()[2];
    click(&mut host, confirm);
    until(&[&first, &guest], "Grace removed", || {
        first.role_of(room, &grace).is_none().then_some(())
    });
    assert!(first.tree.read().governance_refusals.is_empty());

    // An agent admitted at the door is a member once the governor committed
    // its admission grants, not before.
    let agents = |device: &Device| {
        use crate::shell::state::WorkspaceAuthority as _;
        device
            .tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .map(|workspace| {
                workspace
                    .agent_members()
                    .into_iter()
                    .map(|(agent, _)| agent)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    crate::shell::agent_admission::ensure_agent_member(
        &chrome.borrow(),
        &room.to_string(),
        "claude-code",
        tilo_action_policy::roles::AgentPreset::Observer,
    )
    .expect("the owner admits the agent");
    assert!(
        agents(&first).is_empty(),
        "nothing changes before the commit"
    );
    until(&[&first, &guest], "the agent a member", || {
        (agents(&first) == ["claude-code"]).then_some(())
    });
    drop((host, chrome));

    // A sandbox capability given in the Permissions tab is the governor's
    // too: once committed, the app may use the device.
    let microphone = |device: &Device| {
        device
            .tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .is_some_and(|workspace| {
                workspace.allows(
                    &workspace.actor_for_app("firefox"),
                    &ResourceRef::Device("microphone".into()),
                    Access::Read,
                )
            })
    };
    let request = {
        let tree = first.tree.read();
        let mut edited = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .cloned()
            .expect("the workspace");
        drop(tree);
        let id = edited
            .grant_as(
                &ada.id(),
                CapOverride::grant(
                    Subject::app("firefox"),
                    Scope::Device("microphone".into()),
                    Access::Read,
                ),
            )
            .expect("the owner may grant it");
        GovernanceRequest::GrantSandbox(
            edited
                .sandbox
                .iter()
                .find(|cap| cap.id == id)
                .cloned()
                .expect("the granted capability"),
        )
    };
    first.tree.write().request_governance(room, request);
    assert!(!microphone(&first), "nothing changes before the commit");
    until(&[&first, &guest], "the microphone grant in force", || {
        microphone(&first).then_some(())
    });

    // An app allowed another app's action, then not: both are the
    // governor's commits.
    let app_action = |device: &Device| {
        device.policy(room).is_some_and(|policy| {
            policy.grants.iter().any(|grant| {
                grant.subject == tilo_action_policy::Subject::App("so.tilo.shortcuts".into())
            })
        })
    };
    let ask = |operation| {
        let request = crate::shell::workspace_governance::governance_request(room, operation)
            .expect("a request the governor answers");
        first.tree.write().request_governance(room, request);
    };
    ask(
        tilo_desktop::workspace_settings::GovernanceOperation::GrantAppAction {
            app: "so.tilo.shortcuts".into(),
            target_app: "so.tilo.agent".into(),
            resource: "agent.transform:freeform".into(),
            action: "freeform".into(),
            lifetime: "always".into(),
        },
    );
    until(&[&first, &guest], "the app's action allowed", || {
        app_action(&first).then_some(())
    });
    let granted = first
        .policy(room)
        .and_then(|policy| {
            policy
                .grants
                .iter()
                .find(|grant| {
                    grant.subject == tilo_action_policy::Subject::App("so.tilo.shortcuts".into())
                })
                .map(|grant| grant.id.as_str().to_owned())
        })
        .expect("the app action's grant");
    // As the Permissions tab's Revoke asks it.
    ask(tilo_desktop::workspace_settings::GovernanceOperation::Revoke { grant: granted });
    until(&[&first, &guest], "the app's action taken back", || {
        (!app_action(&first)).then_some(())
    });
    assert!(first.tree.read().governance_refusals.is_empty());
    drop((first, guest));
}
