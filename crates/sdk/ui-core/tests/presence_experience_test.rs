//! Real-path tests for the rich presence layer: they drive the exact `App`
//! collab pump a live app runs each frame (`apply_collab_event_for_test`
//! routes through the same `apply_remote_presence` the socket path uses),
//! and assert on the production `PresenceCtx` surface the widgets render
//! from — not a bespoke helper.

use std::time::{Instant, SystemTime};

use tilo_ui::app::App;
use tilo_ui::collaboration::{CollabEvent, PresencePayload};
use tilo_ui::permissions::UserId;
use tilo_ui::presence::PresenceKey;
use tilo_ui::state::State;
use tilo_ui::widget::Container;
use vello::kurbo::{Point, Rect};

fn app() -> App {
    App::new(State::new(), Box::new(Container::new()))
}

#[test]
fn room_arrivals_status_and_follow_reach_the_right_surface() {
    // Case `remote_arrival_on_tab_flags_the_right_nav_item`.
    // `peers_on(that tab)` returns them, the exact query the notes sidebar /
    // sheets tab strip use to paint "Alice is on the Contacts screen".
    {
        let mut a = app();
        let contacts = PresenceKey::Tab {
            id: "contacts".into(),
        };
        let calendar = PresenceKey::Tab {
            id: "calendar".into(),
        };

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
        assert_eq!(
            a.presence_ctx().peers_on(&calendar),
            &[UserId("alice".into())]
        );
    }

    // Case `follow_offscreen_peer_by_clicking_their_mist`.
    // Clicking the edge-mist band latches the follow target to that peer and
    // resolves to their live pointer.
    {
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
        assert_eq!(
            a.followed_peer(),
            Some("bob"),
            "a miss must not drop the follow"
        );
    }

    // Kept last: this case already fails at base in the local (macOS) run, and
    // running it last keeps the other cases observable.
    // Case `room_status_arrives_with_honest_provenance_and_can_be_cleared`.
    {
        let mut a = app();
        let room = PresenceKey::Workspace {
            id: "finance-room".into(),
        };
        let bob = UserId("bob".into());
        a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: bob.0.clone(),
            key: room.clone(),
            payload: PresencePayload::Status {
                text: "Reconciling forecast against actuals".into(),
                automatic: true,
            },
        });
        let status = a
            .presence_ctx()
            .status_for(&bob, SystemTime::now())
            .expect("status should be visible in the universal room model");
        assert_eq!(status.text, "Reconciling forecast against actuals");
        assert!(matches!(
            status.origin,
            tilo_ui::StatusOrigin::Inferred { .. }
        ));

        a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: bob.0.clone(),
            key: room,
            payload: PresencePayload::Status {
                text: String::new(),
                automatic: false,
            },
        });
        assert!(
            a.presence_ctx()
                .status_for(&bob, SystemTime::now())
                .is_none()
        );
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "merged scenario keeps its original cases together; each case builds a fresh fixture"
)]
#[test]
fn remote_writing_tints_once_per_run() {
    // Case `remote_typing_event_records_fading_tint`.
    // Lands as a fading highlighter span in `PresenceCtx`, scoped to the
    // block they typed in — exactly what the block editor / text input read
    // to paint the marker.
    {
        let mut a = app();
        let key = PresenceKey::Widget {
            path: vec![tilo_ui::presence::PathSegment::Index(2)],
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

    // Case `caret_only_peer_still_gets_a_highlight`.
    // A collaborator that publishes **only** a caret — never `Typing` — still
    // gets a highlighter over the run it just wrote.
    //
    // Why derivation lives on the receiving side, not the sending one: `App`
    // only synthesises `Typing` from its own focused widget's caret, which
    // covers peers running this toolkit and nobody else — an agent, headless
    // bot, or other SDK client publishes only where its caret is, so deriving
    // from that on receipt is what gives it a marker at all.
    {
        let mut a = app();
        let key = PresenceKey::Widget {
            path: vec![
                tilo_ui::presence::PathSegment::Field("wp_block".into()),
                tilo_ui::presence::PathSegment::Field("b1".into()),
            ],
        };
        // Caret before the edit, then after it; the delta between them is what
        // tells the advance apart from somebody reading.
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

    // Case `toolkit_peer_sending_both_tints_once`.
    // A peer publishing `Typing` *and* an advanced caret for the same run
    // must read as one marker, not two tints stacked into a darker, less
    // legible band.
    {
        let mut a = app();
        let key = PresenceKey::Widget {
            path: vec![tilo_ui::presence::PathSegment::Index(7)],
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
        assert_eq!(
            marks.len(),
            1,
            "one run is one marker, however it was reported"
        );
        assert_eq!((marks[0].0.start, marks[0].0.end), (4, 9));
    }

    // Case `an_explicit_typing_report_needs_no_edit_pulse`.
    // An explicit `Typing` payload is a peer *stating* it wrote something, and is
    // not gated: it is a report, not an inference.
    {
        let mut a = app();
        let key = PresenceKey::Widget {
            path: vec![tilo_ui::presence::PathSegment::Index(5)],
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
}

#[test]
fn caret_motion_that_is_not_writing_marks_nothing() {
    // Case `retreating_caret_marks_nothing`.
    // Deleting is not writing. A caret that moves *back* — backspace, or an arrow
    // key — leaves no marker, or every backspace would paint the text it just
    // removed.
    {
        let mut a = app();
        let key = PresenceKey::Widget {
            path: vec![tilo_ui::presence::PathSegment::Index(1)],
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

    // Case `a_caret_moving_without_an_edit_marks_nothing`.
    // Reading is not writing: a caret sweeping forward through a document
    // nobody is editing — clicking into a paragraph, arrowing along a line,
    // dragging to select — must leave no marker. The caret says *who* and
    // *where*; only a document change says *that anything happened*.
    {
        let mut a = app();
        let key = PresenceKey::Widget {
            path: vec![tilo_ui::presence::PathSegment::Index(3)],
        };
        // A long forward sweep, exactly as reading produces it, with no
        // document change anywhere near it.
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

    // Case `an_absurd_caret_jump_is_a_click_and_not_a_run`.
    // The cap exists for exactly one case: a pointer landing far down a long
    // paragraph while an edit happens to have landed a moment earlier, which
    // would otherwise paint the whole span it crossed.
    {
        let mut a = app();
        let key = PresenceKey::Widget {
            path: vec![tilo_ui::presence::PathSegment::Index(4)],
        };
        a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "bob".into(),
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 0 },
        });
        a.presence_ctx_mut().note_remote_text_edit(Instant::now());
        a.apply_collab_event_for_test(CollabEvent::RemotePresenceUpdate {
            user_id: "bob".into(),
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 4000 },
        });
        assert!(
            a.presence_ctx()
                .active_typing(&UserId("bob".into()), &key, Instant::now())
                .is_empty(),
            "nobody typed 4000 characters between two presence frames"
        );
    }

    // Case `your_own_edits_do_not_highlight_on_your_own_screen`.
    // A marker following your own cursor is noise, and on a busy document
    // would be a stripe under every word you wrote. The frames a session
    // publishes come back to it (the same person may have the document open
    // twice), so this is a live path, not a special case: your own id paints
    // nothing even with an edit pulse armed and a caret running forward.
    {
        let mut a = app();
        a.presence_ctx_mut().set_own_identity(["me"]);
        let key = PresenceKey::Widget {
            path: vec![tilo_ui::presence::PathSegment::Index(9)],
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
}
