//! A person's own devices share their workspaces without invitations: the
//! person's workspace directory lists each workspace with the device that
//! hosts it, and another of their devices enters the room at that host and
//! holds it as a member. What either device does reaches the other, and a
//! device that was away catches up from the host's retained log.

use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;

use super::{Device, Person, Wakes, hosted_workspace, scratch, until};

#[test]
#[serial_test::serial]
#[allow(
    clippy::too_many_lines,
    reason = "one journey across two devices of a person, an absence included"
)]
fn a_persons_second_device_holds_the_first_devices_workspace_and_catches_up_after_an_absence() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(41);
    home.env().set("TILO_USER_ID", ada.id());
    let first = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A11,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let mut second = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A12,
        DeviceCapabilities::member(),
        &wakes,
    );

    // The desktop makes a workspace, hosted on it.
    let (_, room) = hosted_workspace(&first, &[&second], "Projects");

    // The laptop finds it in Ada's workspace directory and enters it at the
    // desktop's host: no invitation is asked for or redeemed.
    second.reach(&first);
    until(
        &[&first, &second],
        "the laptop to hold the desktop's workspace",
        || second.holds(room).then_some(()),
    );
    assert_eq!(second.workspace_rooms(), vec![room]);
    assert_eq!(second.workspaces_of(room), 1);
    assert!(
        first.net().pending_invitations(room).is_empty(),
        "nobody was invited"
    );
    assert!(
        second.hosted_rooms().is_empty(),
        "the laptop is a member, its own host stays off"
    );
    assert_eq!(first.hosted_rooms(), vec![room]);

    // Windows, in both directions.
    let on_desktop = first.open_window(room, "Notes");
    until(
        &[&first, &second],
        "the desktop's window on the laptop",
        || second.shows_tab(room, on_desktop).then_some(()),
    );
    let on_laptop = second.open_window(room, "Notes, laptop");
    until(
        &[&first, &second],
        "the laptop's window on the desktop",
        || first.shows_tab(room, on_laptop).then_some(()),
    );

    // Layout: closing a window takes it off the other device too.
    first.close_window(room, on_desktop);
    until(
        &[&first, &second],
        "the closed window to leave the laptop",
        || (!second.shows_tab(room, on_desktop) && second.shows_tab(room, on_laptop)).then_some(()),
    );

    // The app's document, in both directions.
    first.open_notes_first(room);
    until(
        &[&first, &second],
        "the note's record on both devices",
        || (first.holds_note(room) && second.holds_note(room)).then_some(()),
    );
    first.write_note(room, "Draft the roadmap");
    until(
        &[&first, &second],
        "the desktop's note on the laptop",
        || (second.note(room).as_deref() == Some("Draft the roadmap")).then_some(()),
    );
    second.write_note(room, "Draft the roadmap, then review it");
    until(
        &[&first, &second],
        "the laptop's note on the desktop",
        || (first.note(room).as_deref() == Some("Draft the roadmap, then review it")).then_some(()),
    );

    // The laptop goes away; the desktop keeps working.
    second.go_offline();
    first.write_note(room, "Roadmap reviewed while the laptop was away");
    let while_away = first.open_window(room, "Notes, while away");
    until(&[&first], "the desktop to hold its own edits", || {
        (first.note(room).as_deref() == Some("Roadmap reviewed while the laptop was away")
            && first.shows_tab(room, while_away))
        .then_some(())
    });

    // It comes back and catches up from the desktop host's retained log.
    second.come_back();
    until(&[&first, &second], "the laptop to catch up", || {
        (second.holds(room)
            && second.note(room).as_deref() == Some("Roadmap reviewed while the laptop was away")
            && second.shows_tab(room, while_away)
            && second.shows_tab(room, on_laptop)
            && !second.shows_tab(room, on_desktop))
        .then_some(())
    });
    assert_eq!(second.workspaces_of(room), 1, "still one workspace");
    drop((first, second));
}

/// Governance a person does on a device that does not host their
/// workspace goes to the room's host, and their other device sees it: the
/// app authorship opening an app there grants, and a member's new role.
#[test]
#[serial_test::serial]
fn governance_from_a_persons_other_device_reaches_the_host_and_the_first_device() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(51), Person::new(52));
    home.env().set("TILO_USER_ID", ada.id());
    let first = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A21,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let second = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A22,
        DeviceCapabilities::member(),
        &wakes,
    );
    let guest = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B21,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (local, room) = hosted_workspace(&first, &[&second, &guest], "Projects");
    second.reach(&first);
    until(
        &[&first, &second],
        "the laptop to hold the workspace",
        || second.holds(room).then_some(()),
    );
    guest
        .join(&first.invite(local, "Grace", Some(grace.user())))
        .unwrap();
    until(
        &[&first, &second, &guest],
        "Grace to hold the workspace",
        || guest.holds(room).then_some(()),
    );
    until(
        &[&first, &second, &guest],
        "Grace's role on every device",
        || {
            let editor = Some(tilo_action_policy::Role::Editor);
            (first.role_of(room, &grace) == editor
                && second.role_of(room, &grace) == editor
                && guest.role_of(room, &grace) == editor)
                .then_some(())
        },
    );

    // Opening the notes app first on the laptop lets it author the room's
    // records: the desktop, whose host decided it, and Grace see that.
    assert!(!first.app_may_author(room, super::NOTES));
    second.open_notes_first(room);
    until(
        &[&first, &second, &guest],
        "the app's authorship everywhere",
        || {
            (first.app_may_author(room, super::NOTES)
                && second.app_may_author(room, super::NOTES)
                && guest.app_may_author(room, super::NOTES))
            .then_some(())
        },
    );

    // Ada makes Grace a viewer from the laptop.
    second.set_role(room, &grace, "viewer");
    until(
        &[&first, &second, &guest],
        "Grace's new role everywhere",
        || {
            let viewer = Some(tilo_action_policy::Role::Viewer);
            (first.role_of(room, &grace) == viewer
                && second.role_of(room, &grace) == viewer
                && guest.role_of(room, &grace) == viewer)
                .then_some(())
        },
    );
    assert_eq!(
        first.role_of(room, &ada),
        Some(tilo_action_policy::Role::Owner),
        "Ada still owns it"
    );
    drop((first, second, guest));
}

/// A governance change a person makes on a device that does not host their
/// workspace, while the host is away, waits: it is never shown as refused,
/// and once the host's session is back it is asked and applies on both
/// devices.
#[test]
#[serial_test::serial]
fn governance_made_while_the_host_is_away_applies_once_it_is_back() {
    use crate::shell::state::GovernanceRequest;
    use tilo_desktop_model::commands::DesktopCommand;

    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(53);
    home.env().set("TILO_USER_ID", ada.id());
    let mut first = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A31,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let second = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A32,
        DeviceCapabilities::member(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&first, &[&second], "Projects");
    second.reach(&first);
    until(
        &[&first, &second],
        "the laptop to hold the workspace",
        || second.holds(room).then_some(()),
    );
    let name_on = |device: &Device| {
        device
            .tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .map(|workspace| workspace.name.clone())
    };
    let refused_on = |device: &Device| !device.tree.read().governance_refusals.is_empty();
    let rename = |name: &str| {
        second.tree.write().request_governance(
            room,
            GovernanceRequest::Command(DesktopCommand::RenameWorkspace { name: name.into() }),
        );
    };

    // With the host there, a rename on the laptop applies on both devices.
    rename("Plans");
    until(
        &[&first, &second],
        "the first rename on both devices",
        || {
            (name_on(&first).as_deref() == Some("Plans")
                && name_on(&second).as_deref() == Some("Plans"))
            .then_some(())
        },
    );

    // The desktop, the workspace's host, goes away; Ada renames the
    // workspace on the laptop.
    first.go_offline();
    rename("Roadmap");
    until(&[&second], "the rename to wait for the host", || {
        let waiting = second.net().outbox_waiting.lock().unwrap().is_some();
        (waiting && second.tree.read().room_outbox.len() == 1).then_some(())
    });
    assert!(!refused_on(&second), "a host out of reach refused nothing");

    // The desktop comes back: the laptop asks again, and the rename applies.
    first.come_back();
    second.reach(&first);
    until(
        &[&first, &second],
        "the desktop to host the workspace again",
        || first.hosted_rooms().contains(&room).then_some(()),
    );
    until(
        &[&first, &second],
        "the laptop to ask the host again",
        || {
            assert!(!refused_on(&second), "a host out of reach refused nothing");
            second.tree.read().room_outbox.is_empty().then_some(())
        },
    );
    until(&[&first, &second], "the rename on both devices", || {
        assert!(!refused_on(&second), "a host out of reach refused nothing");
        (name_on(&first).as_deref() == Some("Roadmap")
            && name_on(&second).as_deref() == Some("Roadmap"))
        .then_some(())
    });
    assert!(second.tree.read().room_outbox.is_empty());
    drop((first, second));
}

/// An effect a member device records in its own person's workspace while
/// the workspace's host is away waits on that device, shown and never
/// refused, and reaches the room's history once the host's session is
/// back: once, on both devices.
#[test]
#[serial_test::serial]
fn an_effect_recorded_while_the_host_is_away_reaches_the_room_once_it_is_back() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(54);
    home.env().set("TILO_USER_ID", ada.id());
    let mut first = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A41,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let second = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A42,
        DeviceCapabilities::member(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&first, &[&second], "Projects");
    second.reach(&first);
    until(
        &[&first, &second],
        "the laptop to hold the workspace",
        || second.holds(room).then_some(()),
    );
    let invocation = tilo_actions::InvocationId::new();
    let held_on = |device: &Device| {
        device
            .tree
            .read()
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .map_or(0, |workspace| {
                workspace
                    .effect_receipts
                    .iter()
                    .filter(|receipt| receipt.invocation == invocation)
                    .count()
            })
    };
    let refused_on = |device: &Device| !device.tree.read().governance_refusals.is_empty();

    // The desktop, the workspace's host, goes away; the laptop records an
    // effect in the workspace.
    first.go_offline();
    let recorded = second.tree.write().record_in_room(
        room,
        tilo_desktop_model::WorkspaceEffectReceipt::recorded(
            invocation,
            &ada.id(),
            tilo_actions::ActionContext::default(),
            tilo_actions::EffectReceiptStatus::Succeeded,
            1,
        ),
    );
    assert!(recorded);
    until(&[&second], "the effect to wait for the host", || {
        let waiting = second.net().outbox_waiting.lock().unwrap().is_some();
        (waiting && second.tree.read().room_outbox.len() == 1).then_some(())
    });
    assert_eq!(held_on(&second), 1, "the laptop shows it while it waits");
    assert!(!refused_on(&second), "a host out of reach refused nothing");

    // The desktop comes back: the laptop asks again, and the room's history
    // holds the effect on both devices.
    first.come_back();
    second.reach(&first);
    until(
        &[&first, &second],
        "the room's history to hold the effect on both devices",
        || {
            assert!(!refused_on(&second), "a host out of reach refused nothing");
            (second.tree.read().room_outbox.is_empty()
                && held_on(&first) == 1
                && held_on(&second) == 1)
                .then_some(())
        },
    );
    drop((first, second));
}
