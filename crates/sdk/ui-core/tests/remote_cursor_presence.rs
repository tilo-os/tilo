//! Integration test proving that real peer cursor motion (delivered
//! through a `CollabEvent::RemoteCursorMoved` event) lands in
//! `PresenceCtx::remote_cursors` — the source the shell's
//! `RemoteCursorOverlay` actually renders from.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::app::App;
use tilo_ui::collaboration::{CollabEvent, Collaborator};
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
    // Case `remote_cursor_move_populates_presence_ctx`.
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

    // Case `remote_cursor_move_updates_existing_entry_in_place`.
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

    // Case `authenticated_context_marks_agent_presence_before_it_moves`.
    {
        let mut app = new_headless_app();
        app.apply_collab_event_for_test(CollabEvent::Context {
            collaborators: vec![Collaborator {
                id: "agent@release".into(),
                name: "Release assistant".into(),
                colour: None,
                is_agent: true,
                is_self: false,
            }],
            permissions: vec![],
        });
        app.apply_collab_event_for_test(CollabEvent::RemoteCursorMoved {
            user_id: "agent@release".into(),
            x: 24.0,
            y: 36.0,
        });

        let agent = UserId("agent@release".into());
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
