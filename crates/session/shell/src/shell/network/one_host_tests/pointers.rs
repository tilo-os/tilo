//! A person's pointer reaches whoever shares the workspace with them,
//! through the room's host, as the desktop draws it: each shell's pointer
//! presence publishes its own pointer in the room on screen and places the
//! pointers it hears over its own tiles. Once with two of one person's
//! devices, the room hosted on the first; once with two people, the room
//! hosted on the inviter's device; and once with two people in a room whose
//! layout is personal, where only a window shared on purpose is on both
//! desktops.

use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;
use ui::presence::PresenceCtx;

use super::{Device, NOTES, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::peer_presence::{Desk, PeerPresence};
use crate::shell::state::RoomId;

/// The tab both desktops show, where the pointer rests.
const SHARED_TAB: u64 = 7;

/// One desktop's pointer presence, as its compositor runs it each frame.
struct Desktop<'a> {
    device: &'a Device,
    presence: PeerPresence,
    ctx: PresenceCtx,
    desk: Desk,
}

impl<'a> Desktop<'a> {
    /// `device` with `room`'s workspace in front and its person showing
    /// where they are there, as the desktop says on start.
    fn new(device: &'a Device, room: RoomId) -> Self {
        {
            let mut tree = device.tree.write();
            let workspace = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == room)
                .map(|workspace| workspace.id)
                .expect("the device holds the workspace");
            tree.desktop.active = workspace;
        }
        device
            .net()
            .set_presence_visibility(&room.to_string(), ui::PresenceVisibility::WhereIAm);
        Self {
            device,
            presence: PeerPresence::for_network(device.net(), Vec::new()),
            ctx: PresenceCtx::default(),
            desk: Desk {
                tiles: vec![(SHARED_TAB, vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0))],
                pointer: None,
                strip: None,
            },
        }
    }

    fn frame(&mut self) {
        let _ = self
            .presence
            .frame(&mut self.ctx, &self.desk, &self.device.tree);
    }
}

/// `from`'s person points at the shared tab and `to` draws that pointer.
fn pointer_crosses(devices: &[&Device], from: &mut Desktop<'_>, to: &mut Desktop<'_>) {
    from.desk.pointer = Some((120.0, 80.0));
    until(devices, "the peer's pointer to be drawn", || {
        from.frame();
        to.frame();
        (!to.ctx.remote_cursors.is_empty()).then_some(())
    });
    from.desk.pointer = None;
}

#[test]
#[serial_test::serial]
fn a_pointer_crosses_between_a_persons_two_devices_through_the_room_one_of_them_hosts() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(51);
    home.env().set("TILO_USER_ID", ada.id());
    let first = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A51,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let second = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A52,
        DeviceCapabilities::member(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&first, &[&second], "Projects");
    second.reach(&first);
    until(
        &[&first, &second],
        "the laptop to hold the desktop's workspace",
        || second.holds(room).then_some(()),
    );

    let mut on_first = Desktop::new(&first, room);
    let mut on_second = Desktop::new(&second, room);
    pointer_crosses(&[&first, &second], &mut on_first, &mut on_second);
    pointer_crosses(&[&first, &second], &mut on_second, &mut on_first);
    drop((on_first, on_second));
    drop((first, second));
}

#[test]
#[serial_test::serial]
fn a_pointer_crosses_between_two_people_through_the_room_the_inviter_hosts() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(53), Person::new(54));
    let p = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("ada"),
        0x0A53,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let q = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B53,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (local, room) = hosted_workspace(&p, &[&q], "Review");
    q.join(&p.invite(local, "Grace", Some(grace.user())))
        .unwrap();
    until(&[&p, &q], "Grace to hold the workspace", || {
        q.holds(room).then_some(())
    });

    let mut on_p = Desktop::new(&p, room);
    let mut on_q = Desktop::new(&q, room);
    pointer_crosses(&[&p, &q], &mut on_p, &mut on_q);
    pointer_crosses(&[&p, &q], &mut on_q, &mut on_p);
    drop((on_p, on_q));
    drop((p, q));
}

/// Whether `desktop` draws a peer's pointer at `at`, give or take a pixel.
fn draws_at(desktop: &Desktop<'_>, at: (f32, f32)) -> bool {
    desktop.ctx.remote_cursors.iter().any(|cursor| {
        (cursor.position[0] - at.0).abs() <= 1.0 && (cursor.position[1] - at.1).abs() <= 1.0
    })
}

/// In a room whose layout is personal, as a community's public room is,
/// each desktop keeps its own windows under its own tab ids: a pointer
/// reaches the other desktop only inside a window its person shared with
/// the room on purpose, which the other desktop shows under the sharer's
/// tab, wherever it lays it out.
#[test]
#[serial_test::serial]
fn in_a_personal_room_a_pointer_crosses_only_inside_a_window_shared_on_purpose() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(55), Person::new(56));
    let p = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("ada"),
        0x0A55,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let q = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B55,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (local, room) = hosted_workspace(&p, &[&q], "Lounge");
    q.join(&p.invite(local, "Grace", Some(grace.user())))
        .unwrap();
    until(&[&p, &q], "Grace to hold the workspace", || {
        q.holds(room).then_some(())
    });
    // Listed publicly, the room keeps each member's windows on their
    // device.
    let mut listing = serde_json::Map::new();
    listing.insert("default_role".into(), "editor".into());
    p.govern(room, "publish", listing)
        .expect("the room's host lists the room");
    until(
        &[&p, &q],
        "both desktops to hold the room as personal",
        || {
            (p.tree().read().room_layout(room).is_personal()
                && q.tree().read().room_layout(room).is_personal())
            .then_some(())
        },
    );

    // Ada's windows carry the salt her device key derives, as a session
    // mints them: by it the room's host knows the window runs on her device.
    p.tree()
        .write()
        .set_peer_salt(tilo_desktop_model::ids::device_salt_of_key(
            &p.cert().device_public_key,
        ));
    let own = p.open_window(room, "Agenda");
    let shared = p.open_window(room, "Notes");
    p.net()
        .share_window(room, shared, NOTES, true, None)
        .expect("the room's host keeps the share");
    until(
        &[&p, &q],
        "Grace's desktop to show the shared window under Ada's tab",
        || q.shows_tab(room, shared).then_some(()),
    );
    assert!(
        !q.shows_tab(room, own),
        "Ada's other window stays on Ada's desktop"
    );

    let mut on_p = Desktop::new(&p, room);
    let mut on_q = Desktop::new(&q, room);
    on_p.desk.tiles = vec![
        (own, vello::kurbo::Rect::new(0.0, 0.0, 400.0, 600.0)),
        (shared, vello::kurbo::Rect::new(400.0, 0.0, 800.0, 600.0)),
    ];
    // Grace's desktop lays the shared window out elsewhere.
    on_q.desk.tiles = vec![(shared, vello::kurbo::Rect::new(100.0, 50.0, 500.0, 650.0))];

    on_p.desk.pointer = Some((520.0, 80.0));
    until(
        &[&p, &q],
        "Ada's pointer on Grace's tile of the shared window",
        || {
            on_p.frame();
            on_q.frame();
            draws_at(&on_q, (220.0, 130.0)).then_some(())
        },
    );
    drop((on_p, on_q));
    drop((p, q));
}
