//! Integration test proving that real peer cursor motion (delivered
//! through a `CollabEvent::RemoteCursorMoved` event) lands in
//! `PresenceCtx::remote_cursors` — the source the shell's
//! `RemoteCursorOverlay` actually renders from.

use tilo_ui::app::App;
use tilo_ui::collaboration::{CollabEvent, Member, MemberId};
use tilo_ui::permissions::UserId;
use tilo_ui::presence::{ParticipantKind, user_color};
use tilo_ui::state::State;
use tilo_ui::widget::Container;

fn new_headless_app() -> App {
    // A trivial leaf is enough — this test never runs a frame, it just
    // drives the collab-event entry point and inspects PresenceCtx.
    App::new(State::new(), Box::new(Container::new()))
}

#[test]
fn remote_cursor_moves_populate_and_update_presence() {
    {
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
        assert!((c.color[0] - f32::from(r) / 255.0).abs() < 1e-3);
        assert!((c.color[1] - f32::from(g) / 255.0).abs() < 1e-3);
        assert!((c.color[2] - f32::from(b) / 255.0).abs() < 1e-3);
        assert_eq!(c.color[3], 1.0);
    }

    {
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

    {
        let mut app = new_headless_app();
        app.apply_collab_event_for_test(CollabEvent::Context {
            collaborators: vec![Member::of_software(
                MemberId::agent("release"),
                "Release assistant",
                None,
            )],
            permissions: vec![],
            apps: Vec::new(),
        });
        app.apply_collab_event_for_test(CollabEvent::RemoteCursorMoved {
            user_id: "agent:release".into(),
            x: 24.0,
            y: 36.0,
        });

        let agent = UserId("agent:release".into());
        assert_eq!(
            app.presence_ctx().participant_kind(&agent),
            ParticipantKind::Agent,
        );
        assert_eq!(
            app.presence_ctx().remote_cursors[0].label,
            "Release assistant · Agent",
        );
    }
}
