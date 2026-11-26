use tilo_ui_base::presence::*;
use tilo_ui_base::collaboration::PresencePayload;
use crate::time::Duration;
use tilo_ui_base::permissions::MemberId;

fn workspace() -> PresenceKey {
    PresenceKey::Workspace {
        id: "workspace-1".into(),
    }
}

fn mine() -> PresenceCtx {
    let mut ctx = PresenceCtx::empty();
    ctx.set_own_identity([MemberId::person("ada")]);
    ctx
}

tilo_test_env::scenario! {
    fn a_session_knows_itself_and_its_pointers_ease_and_retire;

    phase a_session_suppresses_only_its_own_identity;
    {
        // phase: a_session_does_not_paint_a_pill_for_itself
        {
            let mut ctx = mine();
            ctx.apply_remote_presence(
                MemberId::person("ada"),
                workspace(),
                PresencePayload::Pointer { x: 120.0, y: 90.0 },
            );
            ctx.apply_remote_presence(MemberId::person("ada"), workspace(), PresencePayload::Caret { pos: 7 });

            assert!(
                ctx.remote_cursors.is_empty(),
                "painted a cursor pill for this session's own identity: {:?}",
                ctx.remote_cursors,
            );
            assert_eq!(ctx.pointer_for(&workspace(), &MemberId::person("ada")), None);
            assert_eq!(ctx.caret_for(&workspace(), &MemberId::person("ada")), None);
            assert!(ctx.peers_on(&workspace()).is_empty());
        }
        // phase: another_person_is_still_painted
        // The control. A second *person* is a second member, and the
        // self-rule may not touch them — hiding a real member is the
        // worse of the two failures by a distance.
        {
            let mut ctx = mine();
            ctx.apply_remote_presence(
                MemberId::person("peer@77"),
                workspace(),
                PresencePayload::Pointer { x: 12.0, y: 34.0 },
            );
            ctx.apply_remote_presence(
                MemberId::person("peer@77"),
                workspace(),
                PresencePayload::Caret { pos: 3 },
            );

            assert_eq!(
                ctx.pointer_for(&workspace(), &MemberId::person("peer@77")),
                Some((12.0, 34.0))
            );
            assert_eq!(
                ctx.caret_for(&workspace(), &MemberId::person("peer@77")),
                Some(3)
            );
            assert_eq!(ctx.remote_cursors.len(), 1);
            assert_eq!(ctx.remote_cursors[0].member, MemberId::person("peer@77"));
        }
        // phase: an_agent_in_the_same_document_is_still_painted
        // A document legitimately has more than one app member: the
        // agent joins as one and publishes a caret as it edits, with no
        // window anywhere. It announces an identity of its own, so the
        // rule leaves it alone — the test is the member, not the
        // process and not the presence of a window.
        {
            let mut ctx = mine();
            ctx.apply_remote_presence(
                MemberId::person("agent"),
                workspace(),
                PresencePayload::Caret { pos: 11 },
            );
            assert_eq!(
                ctx.caret_for(&workspace(), &MemberId::person("agent")),
                Some(11)
            );
        }
        // phase: a_sibling_arriving_still_gets_the_document
        // `Arrived` is not only a thing to draw — it is how a joiner asks
        // the workspace for the document, and a second window of the same app
        // depends on the answer. Suppressing the pill must not suppress
        // the handshake.
        {
            let mut ctx = mine();
            assert_eq!(
                ctx.apply_remote_presence(MemberId::person("ada"), workspace(), PresencePayload::Arrived),
                RemotePresenceEffect::PeerArrived,
                "a sibling that arrived must still be answered with state",
            );
            assert!(
                ctx.peers_on(&workspace()).is_empty(),
                "answering a sibling must not also list it as somebody else",
            );
        }
        // phase: an_unnamed_session_suppresses_nobody
        // A session that does not know what it is called suppresses
        // nobody. The rule fails open on purpose.
        {
            let mut ctx = PresenceCtx::empty();
            ctx.apply_remote_presence(
                MemberId::person("ada"),
                workspace(),
                PresencePayload::Pointer { x: 1.0, y: 2.0 },
            );
            assert_eq!(ctx.remote_cursors.len(), 1);
        }
        // phase: the_cursor_stream_honours_the_same_rule
        // The cursor stream reaches the same pill without passing through
        // `apply_remote_presence`. A guard on one API and not the other
        // leaves the bug reachable.
        {
            let mut ctx = mine();
            ctx.upsert_remote_cursor(MemberId::person("ada"), [5.0, 5.0], None);
            assert!(ctx.remote_cursors.is_empty());
            ctx.upsert_remote_cursor(MemberId::person("peer@77"), [5.0, 5.0], None);
            assert_eq!(ctx.remote_cursors.len(), 1);
        }
    }

    phase the_own_member_is_known_without_becoming_a_peer;
    {
        // phase: a_session_does_not_follow_its_own_view
        // Following yourself is not a thing. A `View` broadcast stamped
        // with this session's own identity is neither stored nor reported
        // as a reason to move the viewport.
        {
            let mut ctx = mine();
            let subset = tilo_presence::ViewSubset {
                scroll: vec![10.0],
                open_resource: None,
                viewport: crate::view::ViewRect {
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                },
                view_mode: None,
            };
            assert_eq!(
                ctx.apply_remote_presence(
                    MemberId::person("ada"),
                    PresenceKey::view(&subset),
                    PresencePayload::Arrived,
                ),
                RemotePresenceEffect::None,
            );
            assert!(ctx.peer_view(&MemberId::person("ada")).is_none());
        }
        // phase: knowing_which_member_you_are_does_not_make_you_a_peer
        // The workspace's answer to "which member am I" arrives without making
        // this session a member of its own presence store — a `names` entry
        // for it would paint a pill over its own document.
        {
            let mut ctx = PresenceCtx::empty();
            let me = MemberId::person("peer@9007495404254172154");
            ctx.set_self_member(me.clone(), "Bob");

            assert_eq!(ctx.self_id(), Some(&me));
            assert_eq!(ctx.self_name(), Some("Bob"));
            assert!(
                !ctx.names.contains_key(&me),
                "the row became a display name, which is what the roster, the \
         caret chips and the mention colours all read"
            );

            // And a frame stamped with that address is this session's own, so the
            // pill and the cursor stay suppressed.
            assert!(ctx.is_own_identity(&me));
            assert_eq!(
                ctx.apply_remote_presence(me.clone(), workspace(), PresencePayload::Caret { pos: 4 }),
                RemotePresenceEffect::None,
            );
            assert!(
                ctx.peers_on(&workspace()).is_empty(),
                "this session was listed as somebody else in its own workspace"
            );
        }
        // phase: an_unnamed_own_member_reports_no_name
        // A member the workspace has admitted but not named has an address and no
        // name. Handing the address back as a name is how a routing hash ends up
        // rendered where a name belongs.
        {
            let mut ctx = PresenceCtx::empty();
            let me = MemberId::person("peer@42");
            ctx.set_self_member(me.clone(), "   ");
            assert_eq!(ctx.self_id(), Some(&me));
            assert_eq!(ctx.self_name(), None);
        }
        // phase: restating_the_published_identities_keeps_the_workspace_address
        {
            let mut ctx = PresenceCtx::empty();
            let me = MemberId::person("peer@42");
            ctx.set_self_member(me.clone(), "Bob");
            ctx.set_own_identity([MemberId::person("so.tilo.chat")]);
            assert!(ctx.is_own_identity(&me));
            assert!(ctx.is_own_identity(&MemberId::person("so.tilo.chat")));
        }
    }

    phase pointer_position_eases_between_samples;
    // Peers publish at ~30 Hz; a pointer painted raw steps on a faster
    // display. Each new sample is glided to over the gap since the last.
    {
        let t0 = crate::time::now();
        let workspace = PresenceKey::Workspace { id: "r".into() };
        let peer = MemberId::person("peer@7");
        let gap = Duration::from_millis(33);
        let mut ctx = PresenceCtx::default();
        ctx.set_pointer_at(&workspace, peer.clone(), (0.0, 0.0), t0);
        assert_eq!(
            ctx.pointer_position_at(&workspace, &peer, t0),
            Some((0.0, 0.0)),
            "a first sample is shown where it is: nowhere to glide from",
        );
        assert!(!ctx.pointers_easing(t0));

        let t1 = t0 + gap;
        ctx.set_pointer_at(&workspace, peer.clone(), (33.0, 0.0), t1);
        assert_eq!(
            ctx.pointer_for(&workspace, &peer),
            Some((33.0, 0.0)),
            "the store holds what was heard; only the paint glides",
        );
        assert_eq!(
            ctx.pointer_position_at(&workspace, &peer, t1),
            Some((0.0, 0.0))
        );
        let (x, y) = ctx
            .pointer_position_at(&workspace, &peer, t1 + gap / 2)
            .expect("a pointer");
        assert!((x - 16.5).abs() < 0.01 && y.abs() < f32::EPSILON, "{x}");
        let (x, _) = ctx
            .pointer_position_at(&workspace, &peer, t1 + gap / 4)
            .expect("a pointer");
        assert!(
            (x - 33.0 * (0.25 * 0.25 * (3.0 - 2.0 * 0.25))).abs() < 0.05,
            "eased, not linear: {x}"
        );
        assert!(
            ctx.pointers_easing(t1 + gap / 2),
            "mid-glide asks for frames"
        );
        assert_eq!(
            ctx.pointer_position_at(&workspace, &peer, t1 + gap),
            Some((33.0, 0.0)),
            "and lands as the next sample is due",
        );
        assert!(!ctx.pointers_easing(t1 + gap), "a landed pointer is quiet");

        // A sample arriving mid-glide starts from where the pointer shows.
        let t2 = t1 + gap / 2;
        ctx.set_pointer_at(&workspace, peer.clone(), (66.0, 0.0), t2);
        let (x, _) = ctx
            .pointer_position_at(&workspace, &peer, t2)
            .expect("a pointer");
        assert!((x - 16.5).abs() < 0.01, "no jump back or ahead: {x}");

        // A restatement does not restart anything.
        let landed = t2 + POINTER_EASE_MAX;
        ctx.set_pointer_at(&workspace, peer.clone(), (66.0, 0.0), landed);
        assert_eq!(
            ctx.pointer_position_at(&workspace, &peer, landed),
            Some((66.0, 0.0))
        );
        assert!(!ctx.pointers_easing(landed));

        // After sitting still, the glide is capped rather than spread over
        // the whole pause.
        let later = landed + Duration::from_secs(2);
        ctx.set_pointer_at(&workspace, peer.clone(), (100.0, 0.0), later);
        assert!(ctx.pointers_easing(later + POINTER_EASE_MAX / 2));
        assert_eq!(
            ctx.pointer_position_at(&workspace, &peer, later + POINTER_EASE_MAX),
            Some((100.0, 0.0)),
        );
    }

    phase a_pointer_carried_with_its_tile_keeps_its_glide;
    // A pointer anchored to a tile is re-placed, under the instant it was
    // heard, every time that tile moves: it rides the tile, glide and all.
    {
        let t0 = crate::time::now();
        let workspace = PresenceKey::Workspace { id: "r".into() };
        let peer = MemberId::person("peer@7");
        let gap = Duration::from_millis(40);
        let mut ctx = PresenceCtx::default();
        ctx.set_pointer_at(&workspace, peer.clone(), (100.0, 0.0), t0);
        ctx.set_pointer_at(&workspace, peer.clone(), (140.0, 0.0), t0 + gap);
        // The tile scrolls 30 px left; nothing new was heard.
        ctx.set_pointer_at(&workspace, peer.clone(), (110.0, 0.0), t0 + gap);
        let (x, _) = ctx
            .pointer_position_at(&workspace, &peer, t0 + gap + gap / 2)
            .expect("a pointer");
        assert!(
            (x - 90.0).abs() < 0.01,
            "halfway from 70 to 110 in the scrolled frame: {x}"
        );

        // A pointer that never moved is carried with no glide at all.
        let still = MemberId::person("peer@8");
        ctx.set_pointer_at(&workspace, still.clone(), (10.0, 10.0), t0);
        ctx.set_pointer_at(&workspace, still.clone(), (40.0, 10.0), t0);
        assert_eq!(
            ctx.pointer_position_at(&workspace, &still, t0),
            Some((40.0, 10.0))
        );
    }

    phase quiet_pointer_is_retired_after_idle_and_returns_without_a_glide;
    // A pointer retired for going quiet takes its glide with it, so when it
    // is heard again it appears where it is rather than gliding from where
    // it was last seen seconds ago.
    {
        let t0 = crate::time::now();
        let workspace = PresenceKey::Workspace { id: "r".into() };
        let peer = MemberId::person("peer@7");
        let mut ctx = PresenceCtx::default();
        ctx.set_pointer_at(&workspace, peer.clone(), (0.0, 0.0), t0);
        ctx.set_pointer_at(
            &workspace,
            peer.clone(),
            (50.0, 0.0),
            t0 + Duration::from_millis(30),
        );
        let quiet = t0 + Duration::from_millis(30) + POINTER_IDLE;
        assert!(ctx.prune_pointers(quiet, POINTER_IDLE));
        assert_eq!(ctx.pointer_position_at(&workspace, &peer, quiet), None);
        assert!(!ctx.pointers_easing(quiet));

        ctx.set_pointer_at(&workspace, peer.clone(), (500.0, 0.0), quiet);
        assert_eq!(
            ctx.pointer_position_at(&workspace, &peer, quiet),
            Some((500.0, 0.0))
        );

        // A pointer that arrived as a frame is also on record as a remote
        // cursor; retiring the pointer retires that record, or a peer who
        // vanished keeps a window repainting for the rest of the session.
        let mut ctx = PresenceCtx::default();
        ctx.apply_remote_presence_at(
            peer.clone(),
            workspace.clone(),
            tilo_ui_base::collaboration::PresencePayload::Pointer { x: 1.0, y: 2.0 },
            t0,
        );
        assert_eq!(ctx.remote_cursors.len(), 1);
        assert!(ctx.prune_pointers(t0 + POINTER_IDLE, POINTER_IDLE));
        assert!(ctx.remote_cursors.is_empty(), "{:?}", ctx.remote_cursors);
    }

    phase a_quiet_pointer_fades_out_just_before_it_is_retired;
    {
        let t0 = crate::time::now();
        let workspace = PresenceKey::Workspace { id: "r".into() };
        let peer = MemberId::person("peer@7");
        let mut ctx = PresenceCtx::default();
        ctx.set_pointer_at(&workspace, peer.clone(), (10.0, 20.0), t0);
        let opacity = |ctx: &PresenceCtx, after: Duration, fade: Duration| {
            ctx.pointer_opacity(&workspace, &peer, t0 + after, POINTER_IDLE, fade)
        };

        let fade_starts = POINTER_IDLE.saturating_sub(POINTER_FADE);
        assert!((opacity(&ctx, fade_starts, POINTER_FADE) - 1.0).abs() < f32::EPSILON);
        assert!(!ctx.pointers_fading(t0 + fade_starts, POINTER_IDLE, POINTER_FADE));
        let midway = fade_starts + POINTER_FADE / 2;
        let half = opacity(&ctx, midway, POINTER_FADE);
        assert!((half - 0.5).abs() < 0.01, "half-faded midway: {half}");
        assert!(ctx.pointers_fading(t0 + midway, POINTER_IDLE, POINTER_FADE));
        assert!(
            (opacity(&ctx, midway, Duration::ZERO) - 1.0).abs() < f32::EPSILON,
            "reduced motion: opaque until retired",
        );
        assert!(ctx.prune_pointers(t0 + POINTER_IDLE, POINTER_IDLE));
        assert_eq!(ctx.pointer_for(&workspace, &peer), None);
        assert!(!ctx.pointers_fading(t0 + POINTER_IDLE, POINTER_IDLE, POINTER_FADE));

        // Heard from again: opaque once more.
        let t1 = t0 + POINTER_IDLE + Duration::from_secs(1);
        ctx.set_pointer_at(&workspace, peer.clone(), (10.0, 20.0), t1);
        assert!(
            (ctx.pointer_opacity(&workspace, &peer, t1, POINTER_IDLE, POINTER_FADE) - 1.0).abs()
                < f32::EPSILON
        );
    }
}
