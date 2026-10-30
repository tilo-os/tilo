//! The workspace settings' controls on the device that hosts the workspace:
//! the role a person picks for a member and their removal are written by
//! this device itself, as far as its person's grants reach; a sandbox grant
//! or an app's action given and taken back is asked of the room's governor.
//! Either way the change shows on every device of the room.

use std::sync::Arc;

use taffy::TaffyTree;
use tilo_user_identity::DeviceCapabilities;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Affine, Rect};

use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::chrome_state::{Chrome, WsTab};
use crate::shell::overlay_host::ShellOverlayHost;
use crate::shell::state::{
    Access, CapOverride, GovernanceRequest, ResourceRef, RoomId, Scope, Subject,
    WorkspaceAuthority as _,
};
use crate::shell::test_support::lay_out_as_root_with_text;

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
    let mut text = ui::text_render::TextRenderer::new();
    lay_out_as_root_with_text(host, 1600.0, 900.0, &Theme::dark(), &mut text);
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
        .map_or_else(
            || panic!("{id} has a row"),
            |(_, role, remove)| (*role, *remove),
        )
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
fn a_role_or_removal_picked_in_the_members_tab_reaches_every_device() {
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

    // Ada makes Grace a viewer: the chooser opens first, and this device
    // writes the choice itself.
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

    // Ada removes Grace: Remove asks to confirm, then her removal is
    // written here and reaches every device.
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

    drop((host, chrome));

    // A sandbox capability given in the Permissions tab is the governor's
    // too: once committed, the app may use the device.
    give_firefox_the_microphone(&first, room, &ada);
    assert!(
        !microphone(&first, room),
        "nothing changes before the commit"
    );
    until(&[&first, &guest], "the microphone grant in force", || {
        microphone(&first, room).then_some(())
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

/// Whether Firefox may use the microphone in `room`, as `device` sees it.
fn microphone(device: &Device, room: RoomId) -> bool {
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
}

/// Have `owner` give Firefox the microphone in `room` from `device`, as the
/// Permissions tab asks it of the room's governor.
fn give_firefox_the_microphone(device: &Device, room: RoomId, owner: &Person) {
    let request = {
        let tree = device.tree.read();
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
                &owner.id(),
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
    device.tree.write().request_governance(room, request);
}

#[test]
#[serial_test::serial]
fn quick_membership_changes_and_the_hosts_next_commit_reach_every_device() {
    // The owner's device hosts its own room: it writes membership changes
    // into its replica and its host commits the room's governance, two
    // copies of the room signed with one key. Changes made one right after
    // the other, then one the host commits, all arrive everywhere.
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(73), Person::new(74));
    home.env().set("TILO_USER_ID", ada.id());
    let first = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A73,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let guest = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B73,
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

    first.set_role(room, &grace, "viewer");
    first.set_role(room, &grace, "commenter");
    give_firefox_the_microphone(&first, room, &ada);
    let commenter = Some(tilo_action_policy::Role::Commenter);
    until(
        &[&first, &guest],
        "both roles and the microphone grant on both devices",
        || {
            [&first, &guest]
                .iter()
                .all(|device| device.role_of(room, &grace) == commenter && microphone(device, room))
                .then_some(())
        },
    );
    assert!(first.tree.read().governance_refusals.is_empty());
    drop((first, guest));
}
