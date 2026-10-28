//! A person's pointer reaches whoever shares the workspace with them,
//! through the room's host, as the desktop draws it: each shell's pointer
//! presence publishes its own pointer in the room on screen and places the
//! pointers it hears over its own tiles. Once with two of one person's
//! devices, the room hosted on the first; once with two people, the room
//! hosted on the inviter's device.

use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;
use ui::presence::PresenceCtx;

use super::{Device, Person, Wakes, hosted_workspace, scratch, until};
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
