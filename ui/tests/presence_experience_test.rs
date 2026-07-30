//! Real-path tests for the rich presence layer: they drive the exact
//! `App` collab pump a live app runs each frame (`apply_collab_event_for_test`
//! routes through the same `apply_remote_presence` the socket path uses),
//! and assert on the production `PresenceCtx` surface the widgets render
//! from — not a bespoke helper.
//!
//! Covered:
//! - a peer's `Typing` event records a fading textmarker under the right scope,
//! - a peer arriving on a *different* nav scope flags that nav item,
//! - an off-screen peer can be followed by clicking their edge mist.

use std::time::Instant;

use ui::app::App;
use ui::collaboration::{CollabEvent, PresencePayload};
use ui::permissions::UserId;
use ui::presence::PresenceKey;
use ui::state::State;
use ui::widget::Container;
use vello::kurbo::{Point, Rect};

fn app() -> App {
    App::new(State::new(), Box::new(Container::new()))
}

/// A peer's `Typing` presence event lands as a fading highlighter span in
/// `PresenceCtx`, scoped to the block they typed in — exactly what the
/// block editor / text input read to paint the marker.
#[test]
fn remote_typing_event_records_fading_tint() {
    let mut a = app();
    let key = PresenceKey::Widget {
        path: vec![ui::presence::PathSegment::Index(2)],
    };
    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Typing { start: 4, end: 11 },
    });

    let now = Instant::now();
    let marks = a
        .presence_ctx()
        .active_typing(&UserId("alice".into()), &key, now);
    assert_eq!(marks.len(), 1, "the typing event must record one span");
    assert_eq!((marks[0].0.start, marks[0].0.end), (4, 11));
    assert!(marks[0].1 > 0.5, "a just-typed span is near full opacity");
    assert!(
        a.presence_ctx().has_active_typing(now),
        "the app should report an active fade so it keeps animating"
    );
}

/// A peer arriving on a Tab scope that is NOT the one we're looking at
/// flags that nav item: `peers_on(that tab)` returns them, which is the
/// exact query the notes sidebar / sheets tab strip use to paint the
/// "Alice is on the Contacts screen" indicator.
#[test]
fn remote_arrival_on_tab_flags_the_right_nav_item() {
    let mut a = app();
    let contacts = PresenceKey::Tab { id: "contacts".into() };
    let calendar = PresenceKey::Tab { id: "calendar".into() };

    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: contacts.clone(),
        payload: PresencePayload::Arrived,
    });

    assert_eq!(
        a.presence_ctx().peers_on(&contacts),
        &[UserId("alice".into())],
        "the nav item Alice is on must be flagged"
    );
    assert!(
        a.presence_ctx().peers_on(&calendar).is_empty(),
        "an unrelated nav item must NOT be flagged"
    );

    // Alice switches screens: the flag moves, it doesn't accumulate.
    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: calendar.clone(),
        payload: PresencePayload::Arrived,
    });
    assert!(
        a.presence_ctx().peers_on(&contacts).is_empty(),
        "Alice's old nav flag must clear when she moves"
    );
    assert_eq!(a.presence_ctx().peers_on(&calendar), &[UserId("alice".into())]);
}

/// An off-screen peer's cursor produces a clickable edge-mist band;
/// clicking it latches the follow target to that peer and resolves to
/// their live pointer.
#[test]
fn follow_offscreen_peer_by_clicking_their_mist() {
    let mut a = app();
    // A 100x100 visible area; the peer's cursor is far to the right.
    let viewport = Rect::new(0.0, 0.0, 100.0, 100.0);
    a.apply_collab_event_for_test(CollabEvent::RemoteCursorMoved {
        user_id: "bob".into(),
        x: 260.0,
        y: 50.0,
    });

    assert!(a.followed_peer().is_none(), "not following anyone yet");

    // Click well inside the right-edge band.
    let hit = a.try_follow_click(viewport, Point::new(98.0, 50.0));
    assert!(hit, "clicking the mist band should select the peer");
    assert_eq!(a.followed_peer(), Some("bob"));
    assert_eq!(
        a.followed_peer_target(),
        Some([260.0, 50.0]),
        "following resolves to the peer's live pointer",
    );

    // A click in open canvas doesn't change the follow target.
    let miss = a.try_follow_click(viewport, Point::new(40.0, 40.0));
    assert!(!miss);
    assert_eq!(a.followed_peer(), Some("bob"), "a miss must not drop the follow");
}
