use super::*;
use crate::collaboration::{CollabEvent, PresencePayload};
use crate::state::State;

/// A replay's caret that no widget here can take is not retried every
/// frame: only once the semantic tree has changed since it missed.
#[test]
fn a_caret_no_widget_takes_is_retried_only_when_the_tree_changes() {
    let mut app = App::new(State::new(), crate::view::overlay_test_root(false, None));
    app.init_layout();
    app.follow_with("alice", crate::view::FollowMode::Replay);
    app.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: crate::presence::PresenceKey::Widget {
            path: vec![crate::presence::PathSegment::Field("nowhere".into())],
        },
        payload: PresencePayload::Caret { pos: 3 },
    });
    let missed = app
        .presence
        .stood_missed_at
        .expect("no widget here takes the caret");
    app.needs_sync = false;
    app.presence.follow_after_sync = false;
    for _ in 0..5 {
        app.follow_after_sync();
    }
    assert!(!app.needs_sync, "an unchanged tree asks for no sync");
    assert!(!app.presence.follow_after_sync);
    assert_eq!(app.presence.stood_missed_at, Some(missed));
    // Once the tree has changed it is tried again, and misses at the
    // revision it then has.
    app.presence.stood_missed_at = Some(missed.wrapping_add(1));
    app.follow_after_sync();
    assert_eq!(app.presence.stood_missed_at, Some(missed));
    assert!(!app.needs_sync);
}
