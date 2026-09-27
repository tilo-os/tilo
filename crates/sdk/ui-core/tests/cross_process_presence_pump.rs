//! Two-App proof that typed cross-process presence reaches the App pump.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::thread;
use std::time::Duration;

use crate::support::typed_collab::TypedBroker;
use tilo_ui::app::App;
use tilo_ui::collaboration::PresencePayload;
use tilo_ui::presence::PresenceKey;
use tilo_ui::state::State;
use tilo_ui::widget::Container;

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

    let key = PresenceKey::Workspace { id: "home".into() };
    tilo_test_env::wait_until("App B to process the typed presence", || {
        // A manager can return just before the peer's subscription RPC is
        // registered. Re-publishing this stateful position makes the test
        // wait on the protocol outcome rather than a scheduler delay.
        app_a.broadcast_presence(key.clone(), PresencePayload::Pointer { x: 320.0, y: 240.0 });
        thread::sleep(Duration::from_millis(10));
        app_b.pump_collab_presence();
        app_b
            .presence_ctx()
            .pointer_for(&key, &tilo_ui::permissions::UserId("peer-a".into()))
            == Some((320.0, 240.0))
    });

    let presence = app_b.presence_ctx();
    let alice = presence
        .remote_cursors
        .iter()
        .find(|c| c.user_id == "peer-a")
        .expect("peer-a's pointer populates App B's remote cursors");
    assert_eq!(alice.position, [320.0, 240.0]);
    assert_eq!(
        presence.pointer_for(&key, &tilo_ui::permissions::UserId("peer-a".into())),
        Some((320.0, 240.0)),
    );

    app_a.broadcast_presence(key, PresencePayload::Left);
    tilo_test_env::wait_until("App B to process peer-a's departure", || {
        app_b.pump_collab_presence();
        app_b
            .presence_ctx()
            .remote_cursors
            .iter()
            .all(|c| c.user_id != "peer-a")
    });
}
