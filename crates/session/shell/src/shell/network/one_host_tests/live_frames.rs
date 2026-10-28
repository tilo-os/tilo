//! Live frames (a wave, a pointer with what someone says beside it) cross
//! between two shells through the room's host: once with the room hosted
//! on a person's device, once with it hosted by their home relay while the
//! device that made it is off.

use std::sync::Arc;

use tilo_relay_protocol::live::CursorChat;
use tilo_user_identity::DeviceCapabilities;

use super::home_relay::HomeRelay;
use super::{Device, NOTES, Person, Wakes, hosted_workspace, scratch, until};
use crate::shell::live_waves::roster_id;
use crate::shell::state::RoomId;

/// The content keys of `channel`, the same on every member, as a channel's
/// membership hands them out.
fn keys_of(channel: collab_types::ChannelId) -> channel_crypto::ChannelContentKeys {
    let wire = (1_u8, channel, 1_u64, vec![(1_u64, [7_u8; 32])]);
    channel_crypto::ChannelContentKeys::from_bytes(&postcard::to_stdvec(&wire).unwrap()).unwrap()
}

/// `from` waves at `to` and says something beside its pointer in the notes
/// window; `to` hears both, from `from`.
fn live_frames_cross(devices: &[&Device], from: &Device, to: &Device, room: RoomId) {
    let wave = until(devices, "a wave to cross through the host", || {
        from.net()
            .publish_wave(&room.to_string(), &roster_id(&to.cert().device_public_key));
        to.net().take_live_waves().into_iter().next()
    });
    assert_eq!(
        wave,
        roster_id(&from.cert().device_public_key),
        "the wave is from the device that waved"
    );
    let said = until(
        devices,
        "a pointer's words to cross through the host",
        || {
            from.net().publish_cursor_chat(
                (&room.to_string(), NOTES, "plan"),
                (12.0, 34.0),
                Some(CursorChat {
                    text: "look here".into(),
                    sent: true,
                }),
            );
            to.net()
                .take_live_chat()
                .into_iter()
                .find(|chat| chat.chat.is_some())
        },
    );
    assert_eq!(said.room, room.to_string());
    assert_eq!(
        said.chat.map(|chat| chat.text).as_deref(),
        Some("look here")
    );
    assert_eq!(
        said.peer,
        neighbour_wire::peer_handle(&from.cert().device_public_key),
        "attributed to the device that said it"
    );
}

#[test]
#[serial_test::serial]
fn live_frames_cross_between_two_shells_through_a_device_hosted_room() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(91), Person::new(92));
    let mut p = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("ada"),
        0x0A61,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let mut q = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B61,
        DeviceCapabilities::primary(),
        &wakes,
    );
    p.hold_live_keys(keys_of);
    q.hold_live_keys(keys_of);
    // Each keeps its workspaces' layouts through its session's broker, as a
    // desktop session does: a window one shows is a tab on the other.
    p.carry_layouts();
    q.carry_layouts();
    let (local, room) = hosted_workspace(&p, &[&q], "Review");
    q.join(&p.invite(local, "Grace", Some(grace.user())))
        .unwrap();
    until(&[&p, &q], "Grace to hold the workspace", || {
        q.holds(room).then_some(())
    });
    live_frames_cross(&[&p, &q], &q, &p, room);
    live_frames_cross(&[&p, &q], &p, &q, room);
    drop((p, q));
}

#[test]
#[serial_test::serial]
fn live_frames_cross_between_two_shells_through_a_room_the_home_relay_hosts() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(93);
    home.env().set("TILO_USER_ID", ada.id());
    let mut env = tilo_test_env::ScopedEnv::new();
    let relay = HomeRelay::start(&ada, dir.path(), &mut env);
    let mut desktop = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A62,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let mut laptop = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A63,
        DeviceCapabilities::member(),
        &wakes,
    );
    let mut phone = Device::start(
        &ada,
        "Ada's phone",
        &dir.path().join("phone"),
        0x0A64,
        DeviceCapabilities::member(),
        &wakes,
    );
    laptop.hold_live_keys(keys_of);
    phone.hold_live_keys(keys_of);
    let (_, room) = hosted_workspace(&desktop, &[&laptop, &phone], "Personal");
    laptop.reach(&desktop);
    phone.reach(&desktop);
    until(
        &[&desktop, &laptop, &phone],
        "the other devices to hold it",
        || (laptop.holds(room) && phone.holds(room)).then_some(()),
    );
    desktop
        .net()
        .add_hosting_member(room)
        .unwrap_or_else(|error| panic!("the relay is added as a host: {error}"));
    until(
        &[&desktop, &laptop, &phone],
        "the relay to host the room",
        || relay.retained(room).is_some().then_some(()),
    );

    // With the desktop off, only the relay hosts the room.
    desktop.go_offline();
    live_frames_cross(&[&laptop, &phone], &laptop, &phone, room);
    live_frames_cross(&[&laptop, &phone], &phone, &laptop, room);
    drop((desktop, laptop, phone, relay, env));
}
