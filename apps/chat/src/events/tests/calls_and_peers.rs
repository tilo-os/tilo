use super::*;

#[test]
fn calls_start_join_leave_and_mute_through_shared_voice_membership() {
    // Phase: start_call_creates_a_voice_channel_when_the_room_has_none.
    {
        let (mut app, mut doc) = text_only_room();
        assert!(
            !app.channels.iter().any(|c| c.kind == ChannelKind::Voice),
            "the room is text-only"
        );
        app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
        assert!(
            app.call.active,
            "the Call button starts a call even with no voice channel"
        );
        assert!(
            app.channels.iter().any(|c| c.kind == ChannelKind::Voice),
            "a voice channel was created to hold the call"
        );
        assert!(app.call.muted);
        assert_eq!(
            app.native_voice.snapshot.state,
            ui::native_media::State::Preparing
        );
    }

    // Phase: header_call_button_joins_an_existing_voice_channel.
    {
        let (mut app, mut doc) = app_with_doc();
        assert!(!app.call.active);
        let voice_before = app
            .channels
            .iter()
            .filter(|c| c.kind == ChannelKind::Voice)
            .count();
        app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
        assert!(app.call.active);
        assert_eq!(
            app.channel(&app.call.channel).unwrap().kind,
            ChannelKind::Voice,
            "joined an existing voice channel"
        );
        let voice_after = app
            .channels
            .iter()
            .filter(|c| c.kind == ChannelKind::Voice)
            .count();
        assert_eq!(
            voice_before, voice_after,
            "no spurious voice channel created when one exists"
        );
    }

    // Phase: no_mute_or_deafen_controls_outside_a_call.
    {
        let (mut app, mut doc) = app_with_doc();
        assert!(!app.call.active);
        render_frame(&mut app, 1280.0, 720.0);
        assert_eq!(
            count_hits(&app, |h| matches!(h, Hit::CallMute)),
            0,
            "no mute control outside a call"
        );
        // Join a call → the window's bar offers the microphone.
        app.on_hit(
            Hit::Channel("c_standup".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        assert!(app.call.active);
        render_frame(&mut app, 1280.0, 720.0);
        assert!(
            ui::window_toolbar::declared().is_some_and(|toolbar| toolbar
                .actions
                .iter()
                .any(|action| action.id == "call-mute")),
            "in a call the window's bar shows the microphone"
        );
    }

    // Phase: follow_host_control_no_longer_exists.
    {
        let (app, _doc) = app_with_doc();
        let (_, buttons) = app.overlay_controls_layout();
        assert!(
            !buttons.iter().any(|(label, ..)| *label == "Follow host"),
            "the non-functional Follow-host toggle was removed"
        );
        assert!(
            buttons
                .iter()
                .any(|(.., hit, _)| matches!(hit, Hit::CallCamera))
        );
        assert!(
            !buttons
                .iter()
                .any(|(.., hit, _)| matches!(hit, Hit::CallMute | Hit::CallLeave)),
            "the microphone and leaving are the window bar's, not the call's twice"
        );
    }
}

fn voice_channels(app: &ChatApp) -> usize {
    app.channels
        .iter()
        .filter(|c| c.kind == ChannelKind::Voice)
        .count()
}

/// A room whose only channel is `#chat`: its voice channel deleted.
fn text_only_room() -> (ChatApp, crate::testing::Room) {
    let mut doc = crate::testing::Room::default();
    crate::testing::starter(&mut doc, model::ME, "You", "Y");
    crate::testing::apply_as(
        &mut doc,
        model::ME,
        Chat::DELETE_CHANNEL.with(chat::DeleteChannel {
            channel: item(crate::record::VOICE_CHANNEL),
        }),
    )
    .unwrap();
    let mut app = ChatApp::new_ephemeral();
    app.attach_room(&doc);
    app.reload(&doc);
    (app, doc)
}

/// The joins sent to the native voice service (its watch aside).
fn voice_joins(app: &mut ChatApp) -> Vec<ui::native_media::Command> {
    use ui::widget::Collaborative as _;
    app.take_collab_requests()
        .into_iter()
        .filter_map(|request| match request {
            ui::collaboration::CollabRequest::NativeMediaCommand { command, .. }
                if matches!(command.command, ui::native_media::Command::Join { .. }) =>
            {
                Some(command.command)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn start_call_joins_the_rooms_voice_channel_creating_one_when_text_only() {
    use crate::effects::ChatEffect;
    // A text-only room gains a voice channel, and the call starts in
    // it once the channel is in the document.
    let (mut app, mut doc) = text_only_room();
    assert_eq!(voice_channels(&app), 0);
    app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(voice_channels(&app), 1);
    assert!(app.call.active);
    let channel = app.channel(&app.call.channel).expect("call channel");
    assert_eq!(
        (channel.kind, channel.name.as_str()),
        (ChannelKind::Voice, "Voice")
    );
    assert_eq!(
        app.native_voice.snapshot.state,
        ui::native_media::State::Preparing
    );

    // A room with a voice channel joins it without making another,
    // through the published effect.
    let (mut app, _doc) = app_with_doc();
    let before = voice_channels(&app);
    assert!(before > 0);
    app.answer_effect(ChatEffect::StartCall)
        .expect("start-call runs");
    assert!(app.call.active);
    assert_eq!(voice_channels(&app), before);
    assert_eq!(
        voice_joins(&mut app),
        vec![ui::native_media::Command::Join {
            channel_id: app.call.channel.clone()
        }]
    );

    // Invoking it again reveals the running call; it never closes it.
    app.call.expanded = false;
    for _ in 0..2 {
        app.answer_effect(ChatEffect::StartCall)
            .expect("start-call runs");
        assert!(app.call.active && app.call.expanded);
    }
    assert!(voice_joins(&mut app).is_empty(), "no second join");

    // The header button keeps its own collapse toggle.
    let (mut app, mut doc) = app_with_doc();
    app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
    app.call.expanded = true;
    app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
    assert!(app.call.active && !app.call.expanded);
    app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
    assert!(app.call.active && app.call.expanded);
}

#[test]
fn a_video_call_turns_the_camera_on_once_the_call_is_live() {
    let (mut app, mut doc) = app_with_doc();
    let before = doc.save();
    app.answer_effect(crate::effects::ChatEffect::StartVideoCall)
        .expect("a video call starts");
    assert!(app.call.active);
    let joins: Vec<u64> = {
        use ui::widget::Collaborative as _;
        app.take_collab_requests()
            .into_iter()
            .filter_map(|request| match request {
                ui::collaboration::CollabRequest::NativeMediaCommand {
                    request_id,
                    command,
                } if matches!(command.command, ui::native_media::Command::Join { .. }) => {
                    Some(request_id)
                }
                _ => None,
            })
            .collect()
    };
    assert_eq!(joins.len(), 1, "it joins the voice channel");
    assert!(app.camera_on_live, "the camera waits for the call");
    let mut live = ui::native_media::Snapshot::unavailable();
    live.state = ui::native_media::State::Live;
    live.issue = None;
    app.native_voice.complete(joins[0], &Ok(live));
    app.project_native_voice();
    assert!(!app.camera_on_live);
    let requests = app.native_voice.take_requests(ui::time::now());
    assert!(
        requests.iter().any(|request| matches!(
            request,
            ui::collaboration::CollabRequest::NativeMediaCommand {
                command: ui::native_media::Request {
                    command: ui::native_media::Command::SetCamera { on: true },
                    ..
                },
                ..
            }
        )),
        "the camera is asked for once the call is live"
    );
    assert_eq!(doc.save(), before, "a call writes nothing into the room");
}

/// Answer every voice request with the broker's reply when this app has
/// no native media lane; returns how many there were.
fn refuse_voice(app: &mut ChatApp) -> usize {
    use ui::widget::Collaborative as _;
    let mut sent = 0;
    for request in app.take_collab_requests() {
        if let ui::collaboration::CollabRequest::NativeMediaCommand { request_id, .. } = request {
            sent += 1;
            app.handle_collab_event(&ui::collaboration::CollabEvent::NativeMediaCommandResult {
                request_id,
                result: Err("native media requires the bounded control lane".into()),
            });
        }
    }
    sent
}

#[test]
fn a_refused_voice_service_is_not_asked_again_on_every_frame() {
    // Phase: not_in_a_call.
    {
        let (mut app, _doc) = app_with_doc();
        let sent: usize = (0..20).map(|_| refuse_voice(&mut app)).sum();
        assert_eq!(
            sent, 1,
            "one watch, left alone a while once refused, not one request per frame"
        );
    }
    // Phase: a_refused_join_says_why_and_asks_nothing_more.
    {
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(
            Hit::Channel("c_standup".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        assert!(
            app.native_voice.channel.is_some(),
            "opening a voice channel joins it"
        );
        let sent: usize = (0..20).map(|_| refuse_voice(&mut app)).sum();
        assert_eq!(sent, 1, "a refused join is not asked again, nor left");
        assert!(app.call.active, "the voice card stays and says why");
        assert_eq!(
            app.media_error.as_deref(),
            Some(crate::i18n::t("chat-call-issue-unavailable").as_str())
        );
    }
}

#[test]
fn microphone_intent_waits_for_local_service_and_never_writes_membership() {
    let (mut app, mut doc) = app_with_doc();
    app.on_hit(
        Hit::Channel("c_standup".into()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    app.land(&mut doc);
    app.reload(&doc);
    service_streams(&mut app, "c_standup", 0);
    let before = doc.save();
    assert!(app.call.muted);
    app.on_hit(Hit::CallMute, &mut ui::widget::Edits::lend(&mut doc));
    assert!(
        app.call.muted,
        "a microphone request is not local consent or active capture"
    );
    let requests = app.native_voice.take_requests(ui::time::now());
    assert!(requests.iter().any(|request| matches!(
        request,
        ui::collaboration::CollabRequest::NativeMediaCommand {
            command: ui::native_media::Request {
                command: ui::native_media::Command::SetMuted { muted: false },
                ..
            },
            ..
        }
    )));
    assert_eq!(doc.save(), before);
}

#[test]
fn leave_remains_available_after_downgrade() {
    use ui::permissions::{PermissionContext, Role};
    let (mut app, mut doc) = app_with_doc();
    app.on_hit(
        Hit::Channel("c_huddle".into()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    app.land(&mut doc);
    app.reload(&doc);
    app.on_hit(Hit::CallExpand, &mut ui::widget::Edits::lend(&mut doc));
    app.perms = crate::permissions::ChatPermissions::from_context(PermissionContext::member(
        model::ME,
        Role::Viewer,
    ));
    app.on_hit(Hit::CallLeave, &mut ui::widget::Edits::lend(&mut doc));
    assert!(!app.call.active);
}

// --- header Call button (was unreachable without a voice channel) ---

#[test]
fn emoji_search_enter_picks_the_best_match() {
    let (mut app, mut doc) = app_with_doc();
    app.focus = Focus::Main;
    app.on_hit(
        Hit::ComposerEmoji(Point::ZERO),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    app.emoji_search.set_query("pizza");
    app.pick_first_emoji_result(&mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(app.draft_main, "🍕");
    assert!(!app.emoji_open);
    assert_eq!(app.emoji_recents.items()[0], "🍕");
    // Reopening starts from an empty query.
    app.on_hit(
        Hit::ComposerEmoji(Point::ZERO),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    assert!(app.emoji_search.value().is_empty());
}

// --- category + reorder + thread + workspace via the hit path -------

// --- multi-participant collaboration (the real sync path) ------------
//
// These drive two independent `ChatApp` instances against two Automerge
// documents and shuttle the exact incremental deltas the compositor's
// collab broker ships (`save_incremental` → peer `load_incremental` — see
// `ui::App::broadcast_doc_delta` / `pump_collab_presence`). No mock: the
// production event handlers do the writing and the real CRDT does the
// merging, so this is a faithful test of live multi-peer chat.

/// A distinct participant: a fresh app bound to the shared room
/// starter, authoring under `id` — genuinely different from any other peer.
fn peer(id: &str, name: &str, initials: &str, color: usize) -> ChatApp {
    let mut app = ChatApp::new_ephemeral();
    app.me = crate::identity::LocalIdentity {
        id: id.into(),
        name: name.into(),
        initials: initials.into(),
        color_idx: color,
    };
    app.attach_framework(ui::framework::Dispatcher::detached("so.tilo.chat", id));
    app.active_channel = crate::record::CHAT_CHANNEL.into();
    app.thread_open = None;
    app.focus = Focus::Main;
    app
}

/// Ship each peer's pending changes to the other — the broker fan-out.
fn exchange(a: &mut AutoCommit, b: &mut AutoCommit) {
    let da = a.save_incremental();
    if !da.is_empty() {
        b.load_incoming(&da).unwrap();
    }
    let db = b.save_incremental();
    if !db.is_empty() {
        a.load_incoming(&db).unwrap();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one labelled phase per original scenario, each with its own fresh fixture"
)]
fn two_participants_converge_on_messages_and_edits() {
    // Phase: messages_channels_and_reactions_converge_across_two_participants.
    {
        // Alice opens the room's chat; Bob adopts her document (as a
        // late-joining peer does) and announces himself.
        let mut doc_a = AutoCommit::new();
        crate::testing::starter(&mut doc_a, "u_alice", "Alice", "AL");
        let snapshot = doc_a.save(); // resets alice's incremental cursor
        let mut doc_b = AutoCommit::load(&snapshot).unwrap();
        crate::testing::introduce(&mut doc_b, "u_bob", "Bob", "BO");
        exchange(&mut doc_a, &mut doc_b);

        let mut a = peer("u_alice", "Alice", "AL", 0);
        let mut b = peer("u_bob", "Bob", "BO", 1);
        let parts = crate::testing::Room::default();
        a.attach_room(&parts);
        b.attach_room(&parts);
        a.reload(&doc_a);
        b.reload(&doc_b);
        assert_eq!(a.me.id, "u_alice");
        assert_eq!(b.me.id, "u_bob");

        // Structure converges: a channel Alice creates shows up for Bob.
        crate::testing::apply_as(
            &mut doc_a,
            "u_alice",
            crate::resource::Chat::CREATE_CHANNEL.with(chat::CreateChannel {
                name: "planning".into(),
                category: None,
                kind: None,
                topic: None,
            }),
        )
        .unwrap();
        let chan = crate::testing::channels(&doc_a)
            .into_iter()
            .find(|channel| channel.name == "planning")
            .expect("the channel is made")
            .id;
        exchange(&mut doc_a, &mut doc_b);
        b.reload(&doc_b);
        assert!(
            b.channels
                .iter()
                .any(|c| c.id == chan && c.name == "planning"),
            "peer B sees the channel peer A created"
        );

        // Alice sends a message — it reaches Bob, attributed to Alice (not a
        // shared `u_me`).
        a.insert_text(&mut ui::widget::Edits::lend(&mut doc_a), "morning all");
        a.send_focused(&mut ui::widget::Edits::lend(&mut doc_a));
        exchange(&mut doc_a, &mut doc_b);
        b.reload(&doc_b);
        let alice_msg = b
            .messages
            .iter()
            .find(|m| m.text == "morning all")
            .expect("B sees A's message");
        assert_eq!(alice_msg.author, "u_alice");
        assert_eq!(
            b.user_name("u_alice"),
            "Alice",
            "attributed to the right person"
        );
        let msg_id = alice_msg.id.clone();

        // Bob replies — converges back to Alice, attributed to Bob.
        b.insert_text(&mut ui::widget::Edits::lend(&mut doc_b), "morning!");
        b.send_focused(&mut ui::widget::Edits::lend(&mut doc_b));
        exchange(&mut doc_a, &mut doc_b);
        a.reload(&doc_a);
        let bob_msg = a
            .messages
            .iter()
            .find(|m| m.text == "morning!")
            .expect("A sees B's reply");
        assert_eq!(bob_msg.author, "u_bob");
        assert_eq!(a.user_name("u_bob"), "Bob");

        // Both react with the SAME emoji to Alice's message. Distinct
        // identities mean the count is 2 — the exact thing a shared `u_me`
        // would have collapsed to 1.
        a.on_hit(
            Hit::Reaction {
                msg: msg_id.clone(),
                emoji: "👍".into(),
            },
            &mut ui::widget::Edits::lend(&mut doc_a),
        );
        b.on_hit(
            Hit::Reaction {
                msg: msg_id.clone(),
                emoji: "👍".into(),
            },
            &mut ui::widget::Edits::lend(&mut doc_b),
        );
        exchange(&mut doc_a, &mut doc_b);
        a.reload(&doc_a);
        b.reload(&doc_b);
        for (who, app) in [("A", &a), ("B", &b)] {
            let m = app.messages.iter().find(|m| m.id == msg_id).unwrap();
            let thumbs = m
                .reactions
                .iter()
                .find(|r| r.emoji == "👍")
                .expect("👍 present");
            assert_eq!(
                thumbs.count(),
                2,
                "peer {who}: both distinct reactors counted once each"
            );
            assert!(thumbs.includes("u_alice") && thumbs.includes("u_bob"));
        }

        // Full convergence: identical timelines on both peers.
        let ids = |app: &ChatApp| {
            app.messages
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&a), ids(&b), "both peers converge on the same timeline");
    }

    // Phase: an_edit_and_its_history_converge_across_two_participants.
    {
        // The author edits a message; the *new* body AND the archived prior
        // version both reach the other peer. Drives the real inline-editor path
        // (begin_edit → insert_text → commit_edit → edit-message), then
        // shuttles the incremental delta a peer would receive from the broker.
        let mut doc_a = AutoCommit::new();
        crate::testing::starter(&mut doc_a, "u_alice", "Alice", "AL");
        let snapshot = doc_a.save();
        let mut doc_b = AutoCommit::load(&snapshot).unwrap();
        crate::testing::introduce(&mut doc_b, "u_bob", "Bob", "BO");
        exchange(&mut doc_a, &mut doc_b);

        let mut a = peer("u_alice", "Alice", "AL", 0);
        let mut b = peer("u_bob", "Bob", "BO", 1);
        let parts = crate::testing::Room::default();
        a.attach_room(&parts);
        b.attach_room(&parts);
        a.reload(&doc_a);
        b.reload(&doc_b);

        a.insert_text(&mut ui::widget::Edits::lend(&mut doc_a), "shipping friday");
        a.send_focused(&mut ui::widget::Edits::lend(&mut doc_a));
        exchange(&mut doc_a, &mut doc_b);
        b.reload(&doc_b);
        let msg_id = b
            .messages
            .iter()
            .find(|m| m.text == "shipping friday")
            .expect("B sees it")
            .id
            .clone();
        assert!(
            b.message(&msg_id).is_some_and(|m| !m.edited),
            "not edited yet"
        );

        // Alice edits through the real inline editor: load her own message,
        // append to its working text, and commit.
        a.begin_edit(msg_id.clone());
        assert!(a.editing.is_some(), "the author can open the inline editor");
        a.insert_text(
            &mut ui::widget::Edits::lend(&mut doc_a),
            " → moved to monday",
        );
        a.commit_edit(&mut ui::widget::Edits::lend(&mut doc_a));

        // The edit — new body + archived prior version — converges to Bob.
        exchange(&mut doc_a, &mut doc_b);
        b.reload(&doc_b);
        let m = b.message(&msg_id).expect("edited message present for B");
        assert_eq!(
            m.text, "shipping friday → moved to monday",
            "B sees the edited body"
        );
        assert!(m.edited, "B sees the (edited) flag");
        assert_eq!(m.history.len(), 1, "B sees one archived prior version");
        assert_eq!(
            m.history[0].text, "shipping friday",
            "the prior body is preserved verbatim"
        );

        // A second edit accumulates history without losing the first — proving
        // the log grows rather than overwriting.
        a.begin_edit(msg_id.clone());
        a.insert_text(&mut ui::widget::Edits::lend(&mut doc_a), " (confirmed)");
        a.commit_edit(&mut ui::widget::Edits::lend(&mut doc_a));
        exchange(&mut doc_a, &mut doc_b);
        b.reload(&doc_b);
        let m = b.message(&msg_id).unwrap();
        assert_eq!(m.history.len(), 2, "both prior versions are kept");
        assert_eq!(m.history[0].text, "shipping friday");
        assert_eq!(m.history[1].text, "shipping friday → moved to monday");
        // And a non-author cannot edit someone else's message via the same path.
        b.begin_edit(msg_id.clone());
        assert!(
            b.editing.is_none(),
            "a non-author can't open the editor on A's message"
        );
    }
}

#[test]
fn a_view_only_peer_cannot_post_react_or_hydrate_embeds() {
    use ui::permissions::{PermissionContext, Role};
    let (mut app, mut doc) = app_with_doc();
    // Restrict this peer to a read-only member with no file capabilities —
    // exactly what an invite without write/file access produces.
    let ctx = PermissionContext::member(model::ME, Role::Viewer);
    app.perms = crate::permissions::ChatPermissions::from_context(ctx);

    assert!(
        !app.can_participate(),
        "a member without admin can't write to the workspace"
    );
    render_frame(&mut app, 1280.0, 720.0);
    assert_eq!(
        count_hits(&app, |h| matches!(
            h,
            Hit::FocusMain | Hit::Send | Hit::Attach
        )),
        0,
        "a viewer sees an honest non-interactive composer, not controls that fail silently"
    );
    assert!(
        !app.embed_allowed("/var/workspace/secret.png"),
        "an ungranted embed is gated"
    );

    // Sending is a no-op under the gate.
    let before = crate::testing::messages(&doc, "c_crit").len();
    app.insert_text(&mut ui::widget::Edits::lend(&mut doc), "should not post");
    app.send_focused(&mut ui::widget::Edits::lend(&mut doc));
    app.begin_edit("m_me1".into());
    assert!(
        app.editing.is_none(),
        "a viewer cannot open an editor for an existing message"
    );
    assert_eq!(
        crate::testing::messages(&doc, "c_crit").len(),
        before,
        "gated peer posts nothing"
    );

    // Attaching is a no-op too.
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("x.png");
    write_real_png(&png, 32, 32);
    assert!(
        !app.attach_file(&mut ui::widget::Edits::lend(&mut doc), &png),
        "a gated peer can't attach either"
    );

    // And reacting is gated.
    if let Some(id) = app
        .messages
        .iter()
        .find(|m| m.reactions.is_empty())
        .map(|m| m.id.clone())
    {
        app.on_hit(
            Hit::Reaction {
                msg: id.clone(),
                emoji: "👍".into(),
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        let after = app.messages.iter().find(|mm| mm.id == id).unwrap();
        assert!(
            after.reactions.iter().all(|r| r.emoji != "👍"),
            "gated: no reaction added"
        );
    }
}

#[test]
fn two_peers_join_through_native_service_without_publishing_legacy_tickets() {
    let mut doc_a = AutoCommit::new();
    crate::testing::starter(&mut doc_a, "u_alice", "Alice", "AL");
    let mut doc_b = AutoCommit::load(&doc_a.save()).unwrap();
    let mut a = peer("u_alice", "Alice", "AL", 0);
    let mut b = peer("u_bob", "Bob", "BO", 1);
    a.reload(&doc_a);
    b.reload(&doc_b);
    let before_a = doc_a.save();
    let before_b = doc_b.save();
    a.on_hit(
        Hit::Channel("c_voice".into()),
        &mut ui::widget::Edits::lend(&mut doc_a),
    );
    b.on_hit(
        Hit::Channel("c_voice".into()),
        &mut ui::widget::Edits::lend(&mut doc_b),
    );
    for app in [&mut a, &mut b] {
        assert!(app.call.muted);
        assert_eq!(
            app.native_voice.snapshot.state,
            ui::native_media::State::Preparing
        );
        assert_eq!(app.native_voice.take_requests(ui::time::now()).len(), 1);
        assert!(app.voice_in("c_voice").is_empty());
    }
    assert_eq!(doc_a.save(), before_a);
    assert_eq!(doc_b.save(), before_b);
}

#[test]
fn in_a_call_the_window_bar_names_the_call_and_offers_leave() {
    let (mut app, _doc) = app_with_doc();
    render_frame(&mut app, 1280.0, 800.0);
    let bar_ids = || -> Vec<String> {
        ui::window_toolbar::declared()
            .map(|toolbar| {
                toolbar
                    .actions
                    .into_iter()
                    .map(|action| action.id)
                    .collect()
            })
            .unwrap_or_default()
    };
    assert!(
        !bar_ids().iter().any(|id| id == "call"),
        "out of a call the bar has no call"
    );
    app.call.join("c_huddle");
    render_frame(&mut app, 1280.0, 800.0);
    let ids = bar_ids();
    let call = ids.iter().position(|id| id == "call").expect("the call");
    let leave = ids.iter().position(|id| id == "call-leave").expect("leave");
    assert!(leave > call, "leave follows the call it ends");
    let destructive = |id: &str| {
        ui::window_toolbar::declared()
            .and_then(|toolbar| toolbar.actions.into_iter().find(|action| action.id == id))
            .expect("in the bar")
            .destructive
    };
    assert!(destructive("call-leave"), "leaving ends the call: danger");
    assert!(!destructive("call-mute"), "muting is undone with a press");
}

#[test]
fn everyone_sees_who_is_muted_and_this_person_their_own_mute_at_once() {
    let (mut app, _doc) = app_with_doc();
    app.call.join("c_huddle");
    service_streams(&mut app, "c_huddle", 2);
    let (first, second) = (format!("{:064x}", 1), format!("{:064x}", 2));
    let me = app.me.id.clone();
    let roster = &mut app.native_voice.snapshot.rosters[0];
    roster.people.push(me.clone());
    roster.people.sort();
    roster.muted = vec![second.clone()];
    app.refresh_native_streams();
    let muted = |app: &ChatApp, person: &str| {
        app.voice_in("c_huddle")
            .iter()
            .find(|member| member.user == person)
            .expect("in the call")
            .muted
    };
    assert!(muted(&app, &second), "the room says they are muted");
    assert!(!muted(&app, &first));
    // The room has not heard this person's mute yet: their own word holds.
    app.call.muted = true;
    app.refresh_native_streams();
    assert!(muted(&app, &me));
    app.call.muted = false;
    app.refresh_native_streams();
    assert!(!muted(&app, &me));
    render_frame(&mut app, 1280.0, 800.0);
}

#[test]
fn deafen_is_in_the_bar_and_asks_the_service() {
    let (mut app, mut doc, mut live) = connected_call();
    render_frame(&mut app, 1280.0, 720.0);
    let deafen = || {
        ui::window_toolbar::declared()
            .and_then(|toolbar| {
                toolbar
                    .actions
                    .into_iter()
                    .find(|action| action.id == "call-deafen")
            })
            .expect("deafen is at hand while in a call")
    };
    assert_eq!(deafen().icon, "headphones");
    app.on_hit(Hit::CallDeafen, &mut ui::widget::Edits::lend(&mut doc));
    assert!(
        app.call.deafened && app.call.muted,
        "deafened and muted at once"
    );
    live.deafened = true;
    let sent = answer_voice(&mut app, &live);
    assert!(sent.contains(&ui::native_media::Command::SetDeafened { deafened: true }));
    assert!(app.call.deafened);
    render_frame(&mut app, 1280.0, 720.0);
    assert_eq!(
        deafen().icon,
        "headphone-off",
        "the bar says how to hear again"
    );
    app.on_hit(Hit::CallDeafen, &mut ui::widget::Edits::lend(&mut doc));
    live.deafened = false;
    let sent = answer_voice(&mut app, &live);
    assert!(sent.contains(&ui::native_media::Command::SetDeafened { deafened: false }));
    assert!(!app.call.deafened);
}

#[test]
fn everyone_sees_who_is_deafened() {
    let (mut app, _doc) = app_with_doc();
    app.call.join("c_huddle");
    service_streams(&mut app, "c_huddle", 2);
    let (first, second) = (format!("{:064x}", 1), format!("{:064x}", 2));
    let roster = &mut app.native_voice.snapshot.rosters[0];
    roster.muted = vec![first.clone(), second.clone()];
    roster.deafened = vec![second.clone()];
    app.refresh_native_streams();
    let member = |app: &ChatApp, person: &str| {
        let member = app
            .voice_in("c_huddle")
            .iter()
            .find(|member| member.user == person)
            .expect("in the call");
        (member.muted, member.deafened)
    };
    assert_eq!(
        member(&app, &second),
        (true, true),
        "the room says they hear nothing"
    );
    assert_eq!(member(&app, &first), (true, false));
    render_frame(&mut app, 1280.0, 800.0);
}

#[test]
fn whoever_the_call_hears_rings_and_a_quiet_face_settles() {
    let (mut app, _doc) = app_with_doc();
    service_streams(&mut app, "c_huddle", 2);
    let talker = format!("{:064x}", 1);
    app.native_voice.snapshot.speaking = vec![talker.clone()];
    app.refresh_native_streams();
    render_frame(&mut app, 1280.0, 800.0);
    let now = ui::time::now();
    assert_eq!(app.speaking.level(&talker, now), 1.0, "heard: ringed");
    assert_eq!(app.speaking.level(&format!("{:064x}", 2), now), 0.0);
    app.native_voice.snapshot.speaking.clear();
    app.refresh_native_streams();
    render_frame(&mut app, 1280.0, 800.0);
    assert!(app.speaking.animating, "a pause eases the ring out");
}

/// Answer every voice request this app sent with `snapshot`, as its voice
/// service would; returns the commands it sent.
fn answer_voice(
    app: &mut ChatApp,
    snapshot: &ui::native_media::Snapshot,
) -> Vec<ui::native_media::Command> {
    use ui::widget::Collaborative as _;
    let mut sent = Vec::new();
    for request in app.take_collab_requests() {
        if let ui::collaboration::CollabRequest::NativeMediaCommand {
            request_id,
            command,
        } = request
        {
            sent.push(command.command);
            app.handle_collab_event(&ui::collaboration::CollabEvent::NativeMediaCommandResult {
                request_id,
                result: Ok(snapshot.clone()),
            });
        }
    }
    sent
}

/// A connected call in the room's voice channel.
fn connected_call() -> (ChatApp, crate::testing::Room, ui::native_media::Snapshot) {
    let (mut app, mut doc) = app_with_doc();
    app.on_hit(
        Hit::Channel("c_standup".into()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    let mut live = ui::native_media::Snapshot::unavailable();
    live.state = ui::native_media::State::Live;
    live.issue = None;
    live.muted = true;
    let sent = answer_voice(&mut app, &live);
    assert!(
        sent.iter()
            .any(|command| matches!(command, ui::native_media::Command::Join { .. })),
        "opening the voice channel joins its call"
    );
    assert_eq!(
        app.native_voice.snapshot.state,
        ui::native_media::State::Live
    );
    (app, doc, live)
}

#[test]
fn opening_the_voice_channel_of_the_call_one_is_in_shows_it_without_joining_again() {
    let (mut app, mut doc, live) = connected_call();
    app.on_hit(
        Hit::Channel("c_standup".into()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    assert_eq!(
        app.native_voice.snapshot.state,
        ui::native_media::State::Live,
        "the call stays connected: no Joining"
    );
    let sent = answer_voice(&mut app, &live);
    assert!(
        !sent
            .iter()
            .any(|command| matches!(command, ui::native_media::Command::Join { .. })),
        "no second join: {sent:?}"
    );
    assert!(app.call.active && app.call.channel == "c_standup");
}

#[test]
fn a_muted_microphone_on_a_device_without_one_says_so() {
    let (mut app, mut doc, live) = connected_call();
    let mute_name = |app: &mut ChatApp| {
        render_frame(app, 1280.0, 720.0);
        ui::window_toolbar::declared().and_then(|toolbar| {
            toolbar
                .actions
                .into_iter()
                .find(|action| action.id == "call-mute")
                .map(|action| action.name)
        })
    };
    assert_ne!(
        mute_name(&mut app).as_deref(),
        Some("No microphone"),
        "a device that has not tried its microphone offers to unmute"
    );
    // Unmuting finds no microphone: the call goes on, muted.
    app.on_hit(Hit::CallMute, &mut ui::widget::Edits::lend(&mut doc));
    let mut missing = live;
    missing.issue = Some(ui::native_media::Issue::SourceLost);
    answer_voice(&mut app, &missing);
    assert!(app.call.active && app.call.muted);
    assert_eq!(
        mute_name(&mut app).as_deref(),
        Some("No microphone"),
        "the window bar's microphone names why"
    );
}

#[test]
fn someone_alone_in_a_room_can_still_unmute_and_leave_the_call() {
    let (mut app, _doc, _live) = connected_call();
    // A fresh room: nobody else has written, so Chat knows no one else.
    let me = app.me.id.clone();
    app.users.retain(|user| user.id == me);
    assert!(!app.has_other_collaborator());
    render_frame(&mut app, 1280.0, 720.0);
    let ids: Vec<String> = ui::window_toolbar::declared()
        .map(|toolbar| {
            toolbar
                .actions
                .into_iter()
                .map(|action| action.id)
                .collect()
        })
        .unwrap_or_default();
    assert!(
        ids.iter().any(|id| id == "call-mute") && ids.iter().any(|id| id == "call-leave"),
        "the call's microphone and Leave are at hand in any room: {ids:?}"
    );
}

#[test]
fn a_browser_that_cannot_join_calls_says_so_plainly() {
    let (mut app, mut doc) = app_with_doc();
    app.on_hit(
        Hit::Channel("c_standup".into()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    let mut refused = ui::native_media::Snapshot::unavailable();
    refused.state = ui::native_media::State::Ended;
    refused.issue = Some(ui::native_media::Issue::Unsupported);
    answer_voice(&mut app, &refused);
    render_frame(&mut app, 1280.0, 720.0);
    let said = crate::i18n::t("chat-call-issue-unsupported");
    assert!(said.contains("browser"), "{said}");
    assert_eq!(app.media_error.as_deref(), Some(said.as_str()));
    let root = app.accessibility().expect("chat semantics");
    let status = root
        .children
        .iter()
        .find(|node| {
            matches!(node.role, ui::a11y::Role::StaticText)
                && node
                    .name
                    .as_deref()
                    .is_some_and(|name| name.ends_with("in call"))
        })
        .and_then(|node| node.description.clone());
    assert_eq!(
        status.as_deref(),
        Some(said.as_str()),
        "the call panel says why, not a call that never connects"
    );
}

#[test]
fn the_docked_call_shows_the_other_person_large_and_follows_who_speaks() {
    let (mut app, _doc) = app_with_doc();
    app.call.join("c_huddle");
    render_frame(&mut app, 1280.0, 800.0);
    let me = app.me.id.clone();
    assert_eq!(app.huddle_people(), vec![me.clone()], "alone: yourself");
    assert!(app.huddle_features_me());

    service_streams(&mut app, "c_huddle", 2);
    render_frame(&mut app, 1280.0, 800.0);
    let (first, second) = (format!("{:064x}", 1), format!("{:064x}", 2));
    let people = app.huddle_people();
    assert_eq!(people[0], first, "with company: someone else is large");
    assert!(people[1..].contains(&me), "yourself goes small");
    assert!(!app.huddle_features_me());

    app.native_voice.snapshot.speaking = vec![second.clone()];
    app.refresh_native_streams();
    render_frame(&mut app, 1280.0, 800.0);
    assert_eq!(app.huddle_people()[0], second, "who speaks is large");

    app.native_voice.snapshot.speaking.clear();
    app.refresh_native_streams();
    render_frame(&mut app, 1280.0, 800.0);
    assert_eq!(app.huddle_people()[0], second, "a pause keeps them large");

    app.native_voice.snapshot.speaking = vec![me];
    app.refresh_native_streams();
    render_frame(&mut app, 1280.0, 800.0);
    assert_eq!(
        app.huddle_people()[0],
        second,
        "speaking yourself changes nothing"
    );
}

#[test]
fn the_call_panel_says_how_many_are_in_the_call_where_it_paints_it() {
    let (mut app, _doc) = app_with_doc();
    service_streams(&mut app, "c_huddle", 1);
    render_frame(&mut app, 1280.0, 800.0);
    let root = app.accessibility().expect("chat semantics");
    let count = root
        .children
        .iter()
        .find(|node| {
            matches!(node.role, ui::a11y::Role::StaticText)
                && node.name.as_deref() == Some("2 in call")
        })
        .expect("the call's count is in the semantics tree");
    let bounds = count.bounds.expect("the count has bounds").to_rect();
    let panel = app.huddle_panel();
    assert!(
        bounds.width() > 0.0 && panel.contains(bounds.center()),
        "read where the panel paints it: {bounds:?} in {panel:?}"
    );
}

#[test]
fn a_call_that_ended_for_a_reason_no_longer_counts_you_in_it() {
    use ui::presence::capability::PublishesPresence;
    let (mut app, _doc) = app_with_doc();
    service_streams(&mut app, "c_huddle", 1);
    let me = app.me.id.clone();
    assert!(app.in_call());
    assert!(app.voice_people("c_huddle").contains(&me));
    assert!(app.presence_payload().is_some(), "the others see you in it");

    // The room ended this device's part (a page that stood still past the
    // participation's lifetime): the panel stays to say why.
    app.native_voice.snapshot.state = ui::native_media::State::Ended;
    app.native_voice.snapshot.issue = Some(ui::native_media::Issue::SessionEnded);
    app.project_native_voice();
    assert!(app.call.active, "the panel says why the call ended");
    assert!(!app.in_call());
    assert_eq!(
        app.voice_people("c_huddle"),
        vec![format!("{:064x}", 1)],
        "only who is still in the call"
    );
    assert!(
        app.presence_payload().is_none(),
        "the others no longer see you in it"
    );
}

#[test]
fn the_sidebar_chooses_the_call_while_its_stage_fills_the_window() {
    let (mut app, mut doc) = app_with_doc();
    let chosen = |app: &ChatApp| app.places.shown().and_then(|(_, selected)| selected);
    app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
    app.call.expanded = true;
    app.show_sidebar();
    let call = crate::sidebar::channel_place(&app.call.channel);
    assert_eq!(chosen(&app), Some(call));
    let text = app
        .channels
        .iter()
        .find(|c| c.kind == ChannelKind::Text)
        .map(|c| c.id.clone())
        .expect("a text channel");
    app.on_hit(
        Hit::Channel(text.clone()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    app.show_sidebar();
    assert!(app.call.active && !app.call.expanded, "the call floats on");
    assert_eq!(chosen(&app), Some(crate::sidebar::channel_place(&text)));
}

#[test]
fn the_window_bar_names_the_call_while_its_stage_fills_the_window() {
    let (mut app, mut doc) = app_with_doc();
    let location = |app: &mut ChatApp| {
        render_frame(app, 1280.0, 720.0);
        ui::window_toolbar::declared()
            .and_then(|toolbar| toolbar.location)
            .map(|location| location.label)
    };
    app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
    let call = app
        .channel(&app.call.channel)
        .map(|channel| channel.name.clone());
    app.call.expanded = true;
    assert_eq!(location(&mut app), call);
    app.call.expanded = false;
    assert_ne!(
        location(&mut app),
        call,
        "collapsed, the conversation is the place"
    );
}
