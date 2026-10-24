//! A room's governor names its other hosts in the room's governance, so
//! every member learns them as it learns the rest of it, a member that was
//! away included. While the governor is out of reach a member's session
//! goes to the next host the room names, and the room stays in step through
//! it. Governance only ever goes to the governor: a change asked meanwhile
//! waits and is made once the governor is back.

use std::sync::Arc;
use std::time::Instant;

use tilo_desktop_model::commands::DesktopCommand;
use tilo_user_identity::DeviceCapabilities;

use super::home_relay::HomeRelay;
use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
use crate::network::actor::{HEARTBEAT_INTERVAL, NetCmd};
use crate::shell::state::{GovernanceRequest, RoomId};

/// The hosts besides its governor that `room`'s governance names, as
/// `device` holds it.
fn hosts_on(device: &Device, room: RoomId) -> usize {
    device
        .net()
        .native_owner()
        .ok()
        .and_then(|owner| {
            owner
                .with_current(room, |view| Ok(view.peer().governance().room_hosts().len()))
                .ok()
        })
        .unwrap_or(0)
}

/// The dials of its rooms' governors and of their other hosts `device`
/// made so far.
fn dials_of(device: &Device) -> (u64, u64) {
    let (reply, answer) = tokio::sync::oneshot::channel();
    device
        .net()
        .cmd_tx
        .try_send(NetCmd::WireTally(reply))
        .unwrap();
    let counts = answer.blocking_recv().unwrap().snapshot();
    let count = |kind| counts.get(kind).copied().unwrap_or(0);
    (count("NativeGovernorDial"), count("NativeHostDial"))
}

/// Ada's desktop and laptop holding the desktop's workspace "Personal",
/// with her home relay running.
struct Ada {
    relay: HomeRelay,
    first: Device,
    second: Device,
    room: RoomId,
    _env: tilo_test_env::ScopedEnv,
    _home: tilo_test_env::TestHome,
    _dir: tempfile::TempDir,
}

fn ada(seed: u8) -> Ada {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(seed);
    home.env().set("TILO_USER_ID", ada.id());
    let mut env = tilo_test_env::ScopedEnv::new();
    let relay = HomeRelay::start(&ada, dir.path(), &mut env);
    let first = Device::start(
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
    let (_, room) = hosted_workspace(&first, &[&second], "Personal");
    second.reach(&first);
    until(
        &[&first, &second],
        "the laptop to hold the workspace",
        || second.holds(room).then_some(()),
    );
    first.open_notes_first(room);
    Ada {
        relay,
        first,
        second,
        room,
        _env: env,
        _home: home,
        _dir: dir,
    }
}

#[test]
#[serial_test::serial]
fn a_member_learns_a_host_added_while_it_was_away_and_stays_in_step_through_it() {
    let Ada {
        relay,
        mut first,
        mut second,
        room,
        _env,
        _home,
        _dir,
    } = ada(71);
    first.write_note(room, "before the relay");
    until(&[&first, &second], "the note on the laptop", || {
        (second.note(room).as_deref() == Some("before the relay")).then_some(())
    });
    assert_eq!(
        hosts_on(&second, room),
        0,
        "the room names no other host yet"
    );

    // The relay is added as a host while the laptop is off.
    second.go_offline();
    first
        .net()
        .add_hosting_member(room)
        .unwrap_or_else(|error| panic!("the relay is added as a host: {error}"));
    first.write_note(room, "kept on the relay too");
    until(&[&first], "the relay to host the workspace", || {
        relay
            .retained(room)
            .is_some_and(|bodies| bodies > 0)
            .then_some(())
    });

    // The laptop comes back and learns the host with the rest of the room.
    second.come_back();
    second.reach(&first);
    until(&[&first, &second], "the laptop to learn the relay", || {
        (hosts_on(&second, room) == 1
            && second.note(room).as_deref() == Some("kept on the relay too"))
        .then_some(())
    });

    // The desktop goes off: the laptop goes on through the relay.
    first.go_offline();
    let before = relay.retained(room).unwrap();
    second.write_note(room, "written while the desktop was off");
    until(&[&second], "the relay to take the laptop's edit", || {
        (relay.retained(room).unwrap_or(0) > before).then_some(())
    });

    // Back, the desktop catches up with what the laptop wrote meanwhile.
    first.come_back();
    second.reach(&first);
    until(&[&first, &second], "the desktop to catch up", || {
        (first.note(room).as_deref() == Some("written while the desktop was off")).then_some(())
    });
    drop((first, second, relay));
}

#[test]
#[serial_test::serial]
#[allow(
    clippy::too_many_lines,
    reason = "one journey: the host added, the governor away and back"
)]
fn a_governance_change_asked_while_the_governor_is_away_is_made_when_it_is_back() {
    let Ada {
        relay,
        mut first,
        second,
        room,
        _env,
        _home,
        _dir,
    } = ada(72);
    first
        .net()
        .add_hosting_member(room)
        .unwrap_or_else(|error| panic!("the relay is added as a host: {error}"));
    first.write_note(room, "kept on the relay too");
    until(&[&first, &second], "the laptop to learn the relay", || {
        (relay.retained(room).is_some_and(|bodies| bodies > 0)
            && hosts_on(&second, room) == 1
            && second.note(room).as_deref() == Some("kept on the relay too"))
        .then_some(())
    });
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

    // The desktop, the room's governor, goes off; the laptop stays in step
    // through the relay.
    first.go_offline();
    let before = relay.retained(room).unwrap();
    second.write_note(room, "written while the desktop was off");
    until(&[&second], "the relay to take the laptop's edit", || {
        (relay.retained(room).unwrap_or(0) > before).then_some(())
    });

    // Ada renames the workspace on the laptop: the relay is no governor, so
    // the change waits.
    second.tree.write().request_governance(
        room,
        GovernanceRequest::Command(DesktopCommand::RenameWorkspace {
            name: "Roadmap".into(),
        }),
    );
    until(&[&second], "the rename to wait for the governor", || {
        let waiting = second.net().outbox_waiting.lock().unwrap().is_some();
        (waiting && second.tree.read().room_outbox.len() == 1).then_some(())
    });
    assert!(
        !refused_on(&second),
        "a governor out of reach refused nothing"
    );
    assert_eq!(name_on(&second).as_deref(), Some("Personal"));

    // The desktop comes back: the laptop's session returns to it, and the
    // rename is made.
    first.come_back();
    second.reach(&first);
    until(&[&first, &second], "the rename on both devices", || {
        assert!(
            !refused_on(&second),
            "a governor out of reach refused nothing"
        );
        (second.tree.read().room_outbox.is_empty()
            && name_on(&first).as_deref() == Some("Roadmap")
            && name_on(&second).as_deref() == Some("Roadmap")
            && first.note(room).as_deref() == Some("written while the desktop was off"))
        .then_some(())
    });
    drop((first, second, relay));
}

#[test]
#[serial_test::serial]
fn with_every_host_out_of_reach_a_member_tries_each_once_a_heartbeat() {
    const HEARTBEATS: u32 = 4;
    let Ada {
        relay,
        mut first,
        second,
        room,
        _env,
        _home,
        _dir,
    } = ada(73);
    first
        .net()
        .add_hosting_member(room)
        .unwrap_or_else(|error| panic!("the relay is added as a host: {error}"));
    until(&[&first, &second], "the laptop to learn the relay", || {
        (relay.retained(room).is_some_and(|bodies| bodies > 0) && hosts_on(&second, room) == 1)
            .then_some(())
    });

    // The desktop and the relay both go off.
    first.go_offline();
    drop(relay);
    until(&[&second], "the laptop to try the relay", || {
        (dials_of(&second).1 > 0).then_some(())
    });
    let (governor_before, hosts_before) = dials_of(&second);
    let end = Instant::now() + HEARTBEAT_INTERVAL * HEARTBEATS;
    until(&[&second], "the window to pass", || {
        (Instant::now() >= end).then_some(())
    });
    let (governor_after, hosts_after) = dials_of(&second);
    let governor = governor_after - governor_before;
    let hosts = hosts_after - hosts_before;
    assert!(
        governor <= u64::from(HEARTBEATS) + 1,
        "the governor is dialled once a heartbeat at most: {governor} in {HEARTBEATS}"
    );
    assert!(
        hosts <= governor + 1,
        "the relay is dialled once for each failed dial of the governor at most: \
         {hosts} for {governor}"
    );
    assert!(governor > 0, "the laptop keeps dialling the governor");
    drop((first, second));
}
