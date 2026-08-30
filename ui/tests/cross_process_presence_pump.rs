//! Two-App proof that typed cross-process presence reaches the App pump.

use std::thread;
use std::time::Duration;

use crate::support::typed_collab::TypedBroker;
use ui::app::App;
use ui::collaboration::PresencePayload;
use ui::presence::PresenceKey;
use ui::state::State;
use ui::widget::Container;

#[test]
fn cross_process_pump_lands_remote_cursor_on_peer_b() {
    let broker = TypedBroker::new();
    let mut app_a = App::with_collaboration_manager(
        State::new(),
        Box::new(Container::new()),
        broker.manager("peer-a"),
    );
    let mut app_b = App::with_collaboration_manager(
        State::new(),
        Box::new(Container::new()),
        broker.manager("peer-b"),
    );
    thread::sleep(Duration::from_millis(100));

    let key = PresenceKey::Workspace { id: "home".into() };
    app_a.broadcast_presence(key.clone(), PresencePayload::Pointer { x: 320.0, y: 240.0 });
    thread::sleep(Duration::from_millis(100));
    assert!(
        app_b.pump_collab_presence(),
        "App B processes typed presence"
    );

    let presence = app_b.presence_ctx();
    let alice = presence
        .remote_cursors
        .iter()
        .find(|c| c.user_id == "peer-a")
        .expect("peer-a's pointer populates App B's remote cursors");
    assert_eq!(alice.position, [320.0, 240.0]);
    assert_eq!(
        presence.pointer_for(&key, &ui::permissions::UserId("peer-a".into())),
        Some((320.0, 240.0)),
    );

    app_a.broadcast_presence(key, PresencePayload::Left);
    thread::sleep(Duration::from_millis(100));
    app_b.pump_collab_presence();
    assert!(
        app_b
            .presence_ctx()
            .remote_cursors
            .iter()
            .all(|c| c.user_id != "peer-a")
    );
}
