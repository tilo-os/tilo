//! Where someone is, when no window here shows it.
use super::*;

/// In a room whose layout is personal nobody's window is on this desktop:
/// a member's row still says which app they are in, as their live presence
/// focuses it, and stops saying it once they focus none.
#[test]
fn a_members_row_names_the_app_their_presence_focuses() {
    use tilo_relay_protocol::live::{LiveFrame, LiveRecord, PresenceFrame};
    let ada = peer_id(81);
    let (mut host_tree, room) = host_naming(&[(&ada, "Ada")]);
    looking_at_its_room(&mut host_tree);
    let host_handle = host_tree.shared();
    let dir = std::env::temp_dir().join(format!("tilo-roster-in-app-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let (host, host_net, _host_cmds) = session(&dir, "i.sock", &host_handle, &peer_id(80));
    let key: iroh::EndpointId = ada.parse().expect("an endpoint id");
    let say = |focus: Option<LiveRecord>| {
        let frame = LiveFrame::Presence(PresenceFrame {
            seq: 1,
            focus,
            ..PresenceFrame::default()
        });
        host_net.take_live_frame(room, *key.as_bytes(), &frame.encode().expect("a frame"));
    };
    let in_app = || {
        host_handle
            .read()
            .desktop
            .active_workspace()
            .and_then(|workspace| workspace.collaborators.first().cloned())
            .and_then(|ada| ada.in_app)
    };
    say(Some(LiveRecord {
        app: "so.tilo.notes".into(),
        record: "welcome".into(),
    }));
    assert!(host.refresh_roster(&PresenceCtx::default()));
    assert_eq!(in_app().as_deref(), Some("so.tilo.notes"));
    say(None);
    assert!(host.refresh_roster_at(&PresenceCtx::default(), Instant::now() + ROSTER_RECHECK));
    assert_eq!(in_app(), None, "focused on nothing, in no app");
    let _ = std::fs::remove_dir_all(&dir);
}
