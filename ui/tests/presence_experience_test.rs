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

/// A collaborator that publishes **only** a caret — never a `Typing`
/// payload — still gets a highlighter over the run it just wrote.
///
/// This is `apps/agent`'s exact stream, and the reason the derivation
/// lives on the receiving side rather than the sending one. `App`
/// synthesises `Typing` from its own focused widget's caret, which
/// covers peers running this toolkit and nobody else: an agent, a
/// headless bot, or any other SDK client publishes the one thing every
/// collaborator publishes — where its caret is — and would otherwise
/// show a cursor with nothing to say what it did.
///
/// The agent announces the caret at the block's *pre-edit* length and
/// then ships the CRDT delta, so the run between two consecutive carets
/// is exactly the text it inserted, and it is already in the document by
/// the time the second caret arrives.
#[test]
fn caret_only_peer_still_gets_a_highlight() {
    let mut a = app();
    let key = PresenceKey::Widget {
        path: vec![
            ui::presence::PathSegment::Field("wp_block".into()),
            ui::presence::PathSegment::Field("b1".into()),
        ],
    };
    // Caret before the edit, then after it. No `Typing` anywhere — and
    // between them the delta that carries the edit itself, which is what
    // tells the caret advance apart from somebody reading.
    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "agent".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 0 },
    });
    a.presence_ctx_mut().note_remote_text_edit(Instant::now());
    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "agent".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 11 },
    });

    let now = Instant::now();
    let marks = a
        .presence_ctx()
        .active_typing(&UserId("agent".into()), &key, now);
    assert_eq!(marks.len(), 1, "the advanced caret must record one span");
    assert_eq!(
        (marks[0].0.start, marks[0].0.end),
        (0, 11),
        "the span is the run between the two carets"
    );
    assert!(marks[0].1 > 0.5, "a just-written span is near full opacity");
}

/// A peer running this toolkit publishes `Typing` *and* an advanced
/// caret for the same run. That must read as one marker, not two tints
/// stacked into a darker, less legible band.
#[test]
fn toolkit_peer_sending_both_tints_once() {
    let mut a = app();
    let key = PresenceKey::Widget {
        path: vec![ui::presence::PathSegment::Index(7)],
    };
    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 4 },
    });
    // The order `App::broadcast_focus_presence` sends them in.
    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Typing { start: 4, end: 9 },
    });
    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 9 },
    });

    let marks = a
        .presence_ctx()
        .active_typing(&UserId("alice".into()), &key, Instant::now());
    assert_eq!(marks.len(), 1, "one run is one marker, however it was reported");
    assert_eq!((marks[0].0.start, marks[0].0.end), (4, 9));
}

/// Deleting is not writing. A caret that moves *back* — backspace, or
/// an arrow key — leaves no marker, or every backspace would paint the
/// text it just removed.
#[test]
fn retreating_caret_marks_nothing() {
    let mut a = app();
    let key = PresenceKey::Widget {
        path: vec![ui::presence::PathSegment::Index(1)],
    };
    for pos in [9usize, 4] {
        a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "bob".into(),
            key: key.clone(),
            payload: PresencePayload::Caret { pos },
        });
    }
    assert!(
        a.presence_ctx()
            .active_typing(&UserId("bob".into()), &key, Instant::now())
            .is_empty(),
        "a retreating caret is a deletion or a move, not a write"
    );
}

/// Reading is not writing.
///
/// A caret sweeping forward through a document nobody is editing — a peer
/// clicking into a paragraph, arrowing along a line, dragging to select —
/// must leave no marker at all. The highlight claims "here is what they
/// just changed", so painting it for movement makes it a lie, and in a
/// document two people are reading together that is the ordinary case
/// rather than the edge one.
///
/// The caret says *who* and *where*; only a document change says *that
/// anything happened*. This is the assertion that keeps the two joined.
#[test]
fn a_caret_moving_without_an_edit_marks_nothing() {
    let mut a = app();
    let key = PresenceKey::Widget {
        path: vec![ui::presence::PathSegment::Index(3)],
    };
    // A long forward sweep, exactly as reading produces it, and not one
    // byte of document change anywhere near it.
    for pos in [0usize, 12, 30, 64, 90] {
        a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "reader".into(),
            key: key.clone(),
            payload: PresencePayload::Caret { pos },
        });
    }
    assert!(
        a.presence_ctx()
            .active_typing(&UserId("reader".into()), &key, Instant::now())
            .is_empty(),
        "a caret moving through an unedited document must not paint a marker"
    );
    assert!(
        !a.presence_ctx().has_active_typing(Instant::now()),
        "and nothing anywhere should be animating for it"
    );
}

/// An explicit `Typing` payload is a peer *stating* it wrote something,
/// and is not gated: it is a report, not an inference.
#[test]
fn an_explicit_typing_report_needs_no_edit_pulse() {
    let mut a = app();
    let key = PresenceKey::Widget {
        path: vec![ui::presence::PathSegment::Index(5)],
    };
    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "alice".into(),
        key: key.clone(),
        payload: PresencePayload::Typing { start: 0, end: 6 },
    });
    assert_eq!(
        a.presence_ctx()
            .active_typing(&UserId("alice".into()), &key, Instant::now())
            .len(),
        1,
        "a peer that says it typed is believed"
    );
}

/// You do not get highlighted to yourself.
///
/// A marker following your own cursor is noise — you know what you just
/// typed — and on a busy document it would be a stripe under every word
/// you wrote. The frames a session publishes come back to it (the same
/// person may have the document open twice), so this is a live path and
/// not a hypothetical: even with an edit pulse armed and a caret running
/// forward, your own id paints nothing.
#[test]
fn your_own_edits_do_not_highlight_on_your_own_screen() {
    let mut a = app();
    a.presence_ctx_mut().set_own_identity(["me"]);
    let key = PresenceKey::Widget {
        path: vec![ui::presence::PathSegment::Index(9)],
    };

    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "me".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 0 },
    });
    // Arm the pulse — a real edit really did land — and advance anyway.
    a.presence_ctx_mut().note_remote_text_edit(Instant::now());
    a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
        user_id: "me".into(),
        key: key.clone(),
        payload: PresencePayload::Caret { pos: 14 },
    });

    assert!(
        a.presence_ctx()
            .active_typing(&UserId("me".into()), &key, Instant::now())
            .is_empty(),
        "your own writing must not be highlighted back at you"
    );
    assert!(
        !a.presence_ctx().has_active_typing(Instant::now()),
        "and it must not keep the frame loop awake either"
    );
}
