//! Integration test proving that real peer cursor motion (delivered
//! through a `CollabEvent::RemoteCursorMoved` event) lands in
//! `PresenceCtx::remote_cursors` — which is the source the shell's
//! `RemoteCursorOverlay` actually renders from. Closes the
//! "Presence over real socket" gap where the overlay was wired to a
//! vec nothing was writing to.

use quick_ui::app::App;
use quick_ui::collaboration::CollabEvent;
use quick_ui::presence::user_color;
use quick_ui::state::State;
use quick_ui::widget::Container;

fn new_headless_app() -> App {
    // A trivial leaf is enough — this test never runs a frame, it just
    // drives the collab-event entry point and inspects PresenceCtx.
    App::new(State::new(), Box::new(Container::new()))
}

#[test]
fn remote_cursor_move_populates_presence_ctx() {
    let mut app = new_headless_app();
    app.apply_collab_event_for_test(CollabEvent::RemoteCursorMoved {
        user_id: "alice".to_string(),
        x: 120.0,
        y: 80.0,
    });

    let presence = app.presence_ctx();
    assert_eq!(
        presence.remote_cursors.len(),
        1,
        "one RemoteCursorMoved must produce exactly one overlay cursor",
    );
    let c = &presence.remote_cursors[0];
    assert_eq!(c.user_id, "alice");
    assert_eq!(c.position, [120.0, 80.0]);
    let (r, g, b) = user_color("alice");
    assert!((c.color[0] - r as f32 / 255.0).abs() < 1e-3);
    assert!((c.color[1] - g as f32 / 255.0).abs() < 1e-3);
    assert!((c.color[2] - b as f32 / 255.0).abs() < 1e-3);
    assert_eq!(c.color[3], 1.0);
}

#[test]
fn remote_cursor_move_updates_existing_entry_in_place() {
    let mut app = new_headless_app();
    app.apply_collab_event_for_test(CollabEvent::RemoteCursorMoved {
        user_id: "bob".to_string(),
        x: 10.0,
        y: 10.0,
    });
    app.apply_collab_event_for_test(CollabEvent::RemoteCursorMoved {
        user_id: "bob".to_string(),
        x: 200.0,
        y: 300.0,
    });

    let presence = app.presence_ctx();
    assert_eq!(
        presence.remote_cursors.len(),
        1,
        "repeated events for the same peer must update in place, not append",
    );
    assert_eq!(presence.remote_cursors[0].position, [200.0, 300.0]);
}
