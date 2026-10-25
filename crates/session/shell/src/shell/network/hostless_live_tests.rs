//! Live presence in a room no workspace host reaches: its frames travel on
//! the room's own topic and are read as the live plane's are, so a wave
//! needs no host.

use super::{NeighbourMsg, NetCmd, NetEvent, room_topic_id, shell_network_for_test};
use crate::shell::live_waves::roster_id;
use crate::shell::state::{PeerOrigin, WorkspaceTree};

const OWNER: &str = "quinten";

/// Content keys of `channel`, as a channel's member holds them.
fn keys_of(channel: collab_types::ChannelId) -> channel_crypto::ChannelContentKeys {
    let wire = (1_u8, channel, 1_u64, vec![(1_u64, [7_u8; 32])]);
    channel_crypto::ChannelContentKeys::from_bytes(&postcard::to_stdvec(&wire).expect("encodes"))
        .expect("channel keys")
}

/// One desktop waves at another in a room with no workspace host: the wave
/// leaves on the room's topic as live frames, and the other desktop, reading
/// them as frames off a live plane, hears who waved, once.
#[test]
fn a_wave_crosses_a_room_with_no_workspace_host() {
    let mut host_tree = WorkspaceTree::default();
    let ws = host_tree.append_empty_workspace("Design review");
    let room = host_tree
        .room_of(ws)
        .expect("a fresh workspace mints a room");
    host_tree.adopt_ownerless_workspaces(&OWNER.to_string());
    let host_handle = host_tree.shared();
    let (mut host_net, _host_cmds) = shell_network_for_test(host_handle.clone(), 1);
    let host_key = *host_net.keypair.public().as_bytes();
    assert!(host_handle.write().name_peer(
        room,
        &OWNER.to_string(),
        &host_net.keypair.public().to_string(),
        "Quinten",
    ));

    // The guest joined the room and knows its people from its document.
    let mut guest_tree = WorkspaceTree::default();
    let mine = guest_tree.append_empty_workspace("Mine");
    assert!(guest_tree.stand_in_room(mine, room));
    assert!(guest_tree.pin_room_authority(room, PeerOrigin::hub(1)));
    let changes = host_handle
        .write()
        .export_room_changes_since(room, &[])
        .expect("the host is in its room");
    guest_tree
        .apply_room_changes(room, &changes, &PeerOrigin::hub(1))
        .expect("the room's document merges");
    let (mut guest_net, mut guest_cmds) = shell_network_for_test(guest_tree.shared(), 2);
    let guest = guest_net.keypair.public();

    // Both hold the keys of the room's desktop record, which a frame naming
    // no window is sealed with.
    let channel = collab_types::document_channel_id(
        tilo_desktop_model::ownership::DESKTOP_APP,
        &room.to_string(),
        tilo_desktop_model::ownership::DESKTOP_RECORD,
    );
    let keys = keys_of(channel);
    let held = keys.clone();
    host_net.live_keys = Box::new(move |_, _| Some(held.clone()));
    guest_net.live_keys = Box::new(move |_, _| Some(keys.clone()));

    assert!(guest_net.workspace_hosts(room_topic_id(room)).is_empty());
    assert!(
        guest_net.publish_wave(&room.to_string(), &roster_id(&host_key)),
        "a room with no host still carries a wave"
    );
    let mut frames = 0;
    while let Ok(cmd) = guest_cmds.try_recv() {
        match cmd {
            NetCmd::Publish {
                ws,
                msg: msg @ NeighbourMsg::LivePresence { .. },
                ..
            } => {
                assert_eq!(ws, room_topic_id(room), "on the room's own topic");
                frames += 1;
                host_net
                    .event_tx
                    .try_send(NetEvent::Msg {
                        ws,
                        peer: guest,
                        membership_epoch: None,
                        replayed: false,
                        msg,
                    })
                    .expect("delivered");
                host_net.tick();
            }
            NetCmd::PublishLive { .. } => panic!("no live plane to publish on"),
            _ => {}
        }
    }
    assert_eq!(frames, 2, "first it says it is here, then it waves");
    assert_eq!(
        host_net.take_live_waves(),
        [roster_id(guest.as_bytes())],
        "the other desktop hears who waved"
    );
    assert!(host_net.take_live_waves().is_empty(), "once");
}
