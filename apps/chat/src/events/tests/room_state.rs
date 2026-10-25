//! What the room shows of its channels, members, reactions and reading
//! position, driven through the same input and accessibility paths a person uses.

use super::*;

// --- the room's channels, one place -------------------------------

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one labelled phase per original scenario, each with its own fresh fixture"
)]
fn visible_controls_publish_unique_accessible_semantics() {
    // Phase: accessibility_names_contextual_navigation_and_conversation_content.
    {
        let (mut app, _doc) = app_with_doc();
        render_frame(&mut app, 1280.0, 720.0);
        let root = ui::Widget::accessibility(&app).expect("Chat has a semantic root");
        assert_eq!(root.role, ui::a11y::Role::Group);
        assert_eq!(root.name.as_deref(), Some("Chat"));
        assert!(
            root.children.iter().all(|node| {
                node.name
                    .as_deref()
                    .is_none_or(|name| !name.starts_with("Inbox"))
            }),
            "one room is one place: no inbox across chat workspaces"
        );
        assert!(
            root.children
                .iter()
                .all(|node| node.name.as_deref() != Some("New Workspace")),
            "Chat never exposes a second workspace creation concept"
        );
        let messages = root
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Messages"))
            .expect("conversation content is exposed as a list");
        assert!(!messages.children.is_empty());
        let channel = app.channel(&app.active_channel).expect("active channel");
        let heading = root
            .children
            .iter()
            .find(|node| node.role == ui::a11y::Role::Heading { level: 1 })
            .expect("the conversation header remains semantic when paint elides it");
        assert!(
            heading
                .name
                .as_deref()
                .is_some_and(|name| name.contains(&channel.name))
        );
        assert_eq!(heading.description.as_deref(), Some(channel.topic.as_str()));
    }

    // Phase: narrow_window_names_the_channel_and_offers_navigation_in_the_bar.
    {
        let (mut app, _doc) = app_with_doc();
        app.stage_view("narrow");
        render_frame(&mut app, 390.0, 844.0);
        let channel = app.channel(&app.active_channel).expect("active channel");
        let toolbar = ui::window_toolbar::declared().expect("the window declares its slots");
        assert_eq!(
            toolbar
                .location
                .as_ref()
                .map(|location| location.label.as_str()),
            Some(channel.name.as_str()),
            "the window's bar names the channel"
        );
    }

    // Phase: semantic_text_inputs_accept_focus_value_and_selection_without_sending.
    {
        let (mut app, mut doc) = app_with_doc();
        render_frame(&mut app, 1280.0, 720.0);
        let target = app
            .semantic_hits()
            .into_iter()
            .find_map(|(_, hit, ..)| matches!(hit, Hit::FocusMain).then(|| hit.semantic_id()))
            .expect("the painted main composer has a semantic target");
        let root = ui::Widget::accessibility(&app).expect("Chat has a semantic root");
        let composer = root
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some(target.as_str()))
            .expect("the composer is retained");
        assert_eq!(composer.role, ui::a11y::Role::MultilineTextInput);
        assert_eq!(composer.value.as_deref(), Some(""));

        assert!(ui::Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::Focus,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(ui::Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::SetValue("🦀 Accessible draft".into()),
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(
            app.composer_main.value(),
            "🦀 Accessible draft",
            "semantic editing updates the person's unsent draft"
        );
        assert!(ui::Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::SetSelection {
                anchor: 0,
                focus: 1,
            },
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        let (anchor, caret) = app.composer_main.selection();
        assert_eq!(
            (anchor.min(caret), anchor.max(caret)),
            (0, "🦀".len()),
            "grapheme-indexed semantic endpoints become UTF-8 offsets only inside the editor"
        );
        assert!(
            app.last_dispatched().is_none(),
            "draft remains unsent until Send"
        );
    }

    // Phase: every_visible_scenario_has_unique_semantic_targets.
    {
        for (scenario, width, height) in [
            ("", 1280.0, 720.0),
            ("narrow", 390.0, 844.0),
            ("thread-narrow", 390.0, 844.0),
            ("context", 1280.0, 720.0),
            ("attach", 1280.0, 720.0),
            ("attach-loading", 1280.0, 720.0),
            ("attach-error", 1280.0, 720.0),
            ("edit", 1280.0, 720.0),
            ("composer", 1280.0, 720.0),
            ("collaboration", 1280.0, 720.0),
        ] {
            let (mut app, _doc) = app_with_doc();
            app.stage_view(scenario);
            render_frame(&mut app, width, height);
            let targets: Vec<_> = app
                .semantic_hits()
                .into_iter()
                .map(|(_, hit, ..)| hit.semantic_id())
                .collect();
            let unique: std::collections::HashSet<_> = targets.iter().collect();
            assert_eq!(
                unique.len(),
                targets.len(),
                "scenario {scenario:?} publishes stable unique semantic targets"
            );
        }
    }
}

#[test]
fn a_mention_completed_with_tab_is_sent_as_a_marked_reference() {
    let (mut app, mut doc) = app_with_doc();
    app.focus = Focus::Main;
    app.set_composer_focus();
    // The editor learns who the room offers at `@` as it is drawn.
    render_frame(&mut app, 1280.0, 720.0);
    let who = app
        .participants()
        .into_iter()
        .find(|participant| !app.is_me(&participant.id) && participant.name.len() > 2)
        .expect("the demo room has someone to mention");
    let prefix: String = who.name.chars().take(2).collect();
    type_str(&mut app, &mut doc, &format!("ping @{prefix}"));
    feed(&mut app, &mut doc, &named_key(NamedKey::Tab));
    let label = format!("@{}", who.name);
    assert_eq!(app.composer_main.value(), format!("ping {label} "));
    type_str(&mut app, &mut doc, "now");
    feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
    let sent = crate::testing::messages(&doc, &app.active_channel)
        .into_iter()
        .last()
        .unwrap();
    assert_eq!(sent.text, format!("ping {label} now"));
    assert_eq!(sent.mentions.len(), 1, "{:?}", sent.mentions);
    assert_eq!(sent.mentions[0].user, who.id);
    assert_eq!(sent.mentions[0].slice(&sent.text), label);
    assert!(
        sent.blocks()[0]
            .marks
            .iter()
            .any(|span| span.mark == ui::widget::Mark::Mention(who.id.clone())),
        "the body keeps the mention as written"
    );
    // Rendered through the read-only view, it lays out at its width.
    render_frame(&mut app, 1280.0, 720.0);
    assert!(!app.body_views.borrow().is_empty());
}

#[test]
fn editing_a_formatted_message_keeps_its_formatting() {
    let (mut app, mut doc) = app_with_doc();
    app.focus = Focus::Main;
    app.set_composer_focus();
    type_str(&mut app, &mut doc, "plain ");
    feed_mods(&mut app, &mut doc, &char_key("b"), mods(true, false));
    type_str(&mut app, &mut doc, "bold");
    feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
    let sent = crate::testing::messages(&doc, &app.active_channel)
        .into_iter()
        .last()
        .unwrap();
    assert_eq!(sent.text, "plain bold");
    assert!(
        sent.blocks()[0]
            .marks
            .iter()
            .any(|span| span.mark == ui::widget::Mark::Bold && span.range == (6..10))
    );
    app.reload(&doc);
    app.begin_edit(sent.id.clone());
    assert_eq!(app.edit_input.value(), "plain bold");
    type_str(&mut app, &mut doc, "!");
    feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
    let edited = app.message(&sent.id).unwrap();
    assert_eq!(edited.text, "plain bold!");
    assert!(
        edited.blocks()[0]
            .marks
            .iter()
            .any(|span| span.mark == ui::widget::Mark::Bold),
        "the edit keeps the bold run"
    );
    assert_eq!(edited.history.last().unwrap().text, "plain bold");
}

#[test]
fn composer_accessibility_keeps_a_backward_browser_selection_direction() {
    let (mut app, mut doc) = app_with_doc();
    crate::testing::write_draft(&mut app, &mut doc, "Ae\u{301}👋B");
    app.sync_composers(&doc);
    render_frame(&mut app, 1280.0, 720.0);
    assert!(app.composer_main.perform_action(
        &ui::a11y::Action::SetSelection {
            anchor: 4,
            focus: 2,
        },
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));

    let root = ui::Widget::accessibility(&app).expect("Chat has a semantic root");
    let composer = root
        .children
        .iter()
        .find(|node| node.action_target.as_deref() == Some("chat.composer"))
        .expect("the main composer remains semantic");
    assert_eq!(
        composer.text_selection,
        Some(ui::a11y::AccessibleTextSelection {
            anchor: 9,
            focus: 4
        }),
        "the semantic bridge must retain the anchor at the right edge while Shift+Left moves focus backward"
    );
}

#[test]
fn reactions_toggle_through_the_picker_and_chips_on_the_shared_route() {
    // Phase: reaction_via_the_emoji_picker_round_trips_through_the_ui.
    {
        let (mut app, mut doc) = app_with_doc();
        let reacted = |app: &ChatApp| {
            app.message("m_me1")
                .unwrap()
                .reactions
                .iter()
                .any(|r| r.emoji == "🔥" && r.includes(model::ME))
        };
        assert!(!reacted(&app), "m_me1 starts without my 🔥");
        // Open the add-reaction picker on the message, then pick an emoji.
        app.on_hit(
            Hit::AddReaction {
                msg: "m_me1".into(),
                anchor: Rect::ZERO,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.emoji_open && app.emoji_target.as_deref() == Some("m_me1"));
        app.on_hit(
            Hit::EmojiChoice("🔥".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(!app.emoji_open, "picking closes the picker");
        assert!(reacted(&app), "the picked emoji became my reaction");
        // Re-opening and picking the same emoji toggles it back off.
        app.on_hit(
            Hit::AddReaction {
                msg: "m_me1".into(),
                anchor: Rect::ZERO,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.on_hit(
            Hit::EmojiChoice("🔥".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(!reacted(&app), "picking it again removed the reaction");
    }

    // Phase: reaction_chips_have_unique_toggle_semantics_and_use_the_shared_action_route.
    {
        let (mut app, mut doc) = app_with_doc();
        render_frame(&mut app, 1280.0, 720.0);

        let (_, reaction, _, role, checked) = app
            .semantic_hits()
            .into_iter()
            .find(|(_, hit, ..)| matches!(hit, Hit::Reaction { .. }))
            .expect("a painted reaction chip is a semantic control");
        assert_eq!(role, ui::a11y::Role::ToggleButton);
        let target = reaction.semantic_id();
        let root = ui::Widget::accessibility(&app).expect("Chat has a semantic root");
        let targets: std::collections::HashSet<_> = root
            .children
            .iter()
            .filter_map(|node| node.action_target.as_deref())
            .collect();
        assert_eq!(
            targets.len(),
            root.children
                .iter()
                .filter(|node| node.action_target.is_some())
                .count(),
            "visible controls do not publish duplicate semantic targets"
        );
        let reaction_node = root
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some(target.as_str()))
            .expect("the reaction target is retained in the accessibility tree");
        assert_eq!(reaction_node.role, ui::a11y::Role::ToggleButton);
        assert_eq!(reaction_node.state.checked, Some(checked));

        assert!(ui::Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        let (_, _, _, _, after) = app
            .semantic_hits()
            .into_iter()
            .find(|(_, hit, ..)| hit.semantic_id() == target)
            .expect("the reaction remains visible after toggling");
        assert_ne!(after, checked, "semantic activation toggles my reaction");
        let action = serde_json::to_value(
            app.dispatched_parts
                .borrow()
                .last()
                .cloned()
                .expect("the semantic control dispatches through the door"),
        )
        .unwrap();
        assert_eq!(action["action"], ui::tilo_resource::FIELD_EDIT, "{action}");
        assert!(
            ["react", "unreact"].contains(&action["args"]["op"].as_str().unwrap_or_default())
                && action["args"]["path"][2]["field"] == "reactions",
            "a reaction is the field edit of the message's reactions: {action}"
        );
    }
}

// --- members roster (right flap "members" half was unimplemented) ---

#[test]
fn the_roster_shows_who_the_room_says_is_here_not_who_once_introduced_themselves() {
    use crate::model::Presence;
    let (mut app, _) = app_with_doc();
    let others: Vec<String> = app
        .users
        .iter()
        .filter(|u| !app.is_me(&u.id) && !u.is_agent)
        .map(|u| u.id.clone())
        .collect();
    assert!(others.len() >= 2, "the sample room has other people");
    for user in &mut app.users {
        user.status = Presence::Online;
    }
    let status = |app: &crate::app::ChatApp, id: &str| {
        app.roster_rows()
            .into_iter()
            .find(|(u, _)| u.id == id)
            .map(|(u, _)| u.status)
    };
    // Without a room the record's word stands.
    assert_eq!(status(&app, &others[1]), Some(Presence::Online));
    // The room knows the first person is here; the second has left.
    app.room_names.insert(others[0].clone(), "Here".into());
    assert_eq!(status(&app, &others[0]), Some(Presence::Online));
    assert_eq!(
        status(&app, &others[1]),
        Some(Presence::Offline),
        "a visitor who closed the tab is offline"
    );
    let mut me = app.users[0].clone();
    me.id = app.me.id.clone();
    me.status = Presence::Offline;
    assert_eq!(app.live_status(&me), Presence::Online, "I am here");
}

#[test]
fn the_rooms_call_places_its_people_in_the_voice_channel_by_person() {
    let (mut app, _) = app_with_doc();
    let people = app.users.len();
    service_streams(&mut app, "c_huddle", 3);
    assert_eq!(
        app.roster_rows().len(),
        people,
        "the roster lists the room's people"
    );
    let in_call = app.voice_people("c_huddle");
    for index in 1..=3 {
        let person = format!("{index:064x}");
        assert!(in_call.contains(&person), "{person} is in the call");
        assert!(
            !app.user_name(&person).contains(&person),
            "nobody is shown by their raw id"
        );
    }
}

#[test]
fn a_private_room_keeps_collaboration_controls_out_of_the_header() {
    let mut doc = AutoCommit::new();
    crate::testing::starter(&mut doc, model::ME, "You", "QD");
    let mut app = ChatApp::new_ephemeral();
    app.active_channel = crate::record::CHAT_CHANNEL.into();
    app.thread_open = None;
    app.reload(&doc);
    render_frame(&mut app, 1280.0, 720.0);

    assert!(
        app.users.iter().all(|user| app.is_me(&user.id)),
        "only the local identity is evidenced in a fresh room"
    );
    assert_eq!(
        count_hits(&app, |h| matches!(h, Hit::StartCall | Hit::ToggleRightFlap)),
        0,
        "a private room has no People or call controls in its resting header"
    );
}

// --- unread badges (were seed-only fake data) -----------------------

/// Lay the app out once in a real viewport, as every presenting runner
/// (native window, browser canvas, offscreen shot) does before drawing.
fn present(app: &mut ChatApp) {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
        std::collections::HashMap::new();
    let theme = ui::Theme::default();
    let node = app.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(1200.0),
                height: taffy::AvailableSpace::Definite(800.0),
            },
        )
        .unwrap();
    app.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);
}

#[test]
fn headless_host_worker_sync_never_authors_read_receipts() {
    // A hosted/headless worker only pumps the model (update + sync) and
    // never lays out: read state belongs to the person's device, so the
    // host identity must not append "Mark channel read" changes.
    let (mut app, mut doc) = app_with_doc();
    let channel = app.active_channel.clone();
    crate::testing::apply_part_at(
        &mut doc,
        "u_aria",
        (crate::demo::NOW + 1) * 1000,
        crate::testing::post(&channel, "arrived while nobody was looking", None),
    )
    .unwrap();
    let watermark = crate::testing::read_mark(&doc, &channel, &app.me.id);
    let heads = doc.get_heads();
    for _ in 0..5 {
        assert!(!app.update(&doc.chat, &automerge::ROOT));
        app.sync(&doc.chat, &automerge::ROOT);
    }
    assert_eq!(
        crate::testing::read_mark(&doc, &channel, &app.me.id),
        watermark,
        "an unpresented worker must not advance the read watermark"
    );
    assert_eq!(
        doc.get_heads(),
        heads,
        "an unpresented worker authored a change"
    );

    // The same instance once presented to a person marks the channel in
    // view read on the next model frame, without waiting for input.
    present(&mut app);
    assert!(
        app.needs_animation_frame(),
        "first presentation requests a model frame"
    );
    assert!(
        app.update(&doc.chat, &automerge::ROOT),
        "first presentation re-runs sync"
    );
    app.sync(&doc.chat, &automerge::ROOT);
    app.land(&mut doc);
    assert!(
        crate::testing::read_mark(&doc, &channel, &app.me.id) > watermark,
        "the presented channel is marked read"
    );
    assert!(
        !app.update(&doc.chat, &automerge::ROOT),
        "the extra frame is one-shot"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one labelled phase per original scenario, each with its own fresh fixture"
)]
fn reading_position_drives_unread_state_and_the_return_to_latest_control() {
    // Phase: opening_a_channel_marks_it_read_and_clears_unread.
    {
        let (mut app, mut doc) = app_with_doc();
        // Simulate having viewed the initial channel (as each sync frame does).
        app.mark_active_read(&doc);
        app.land(&mut doc);
        app.reload(&doc);
        let unread =
            |app: &ChatApp, id: &str| app.channels.iter().find(|c| c.id == id).unwrap().unread;
        assert_eq!(
            unread(&app, "c_general"),
            1,
            "an unseen welcome message shows as unread"
        );
        app.on_hit(
            Hit::Channel("c_general".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        assert_eq!(app.active_channel, "c_general");
        assert_eq!(
            unread(&app, "c_general"),
            0,
            "opening the channel clears its unread"
        );
        // Leaving it keeps it read — the watermark persisted through the mutation.
        app.on_hit(
            Hit::Channel("c_crit".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        assert_eq!(
            unread(&app, "c_general"),
            0,
            "a read channel stays read after leaving"
        );
    }

    // Phase: a_message_in_another_channel_shows_as_unread.
    {
        let (mut app, mut doc) = app_with_doc();
        app.mark_active_read(&doc);
        app.land(&mut doc);
        app.reload(&doc);
        let unread =
            |app: &ChatApp, id: &str| app.channels.iter().find(|c| c.id == id).unwrap().unread;
        assert_eq!(
            unread(&app, "c_random"),
            0,
            "an empty channel has nothing unread"
        );
        // A peer posts to a channel we're not viewing (arrives via the CRDT).
        crate::testing::apply_part_at(
            &mut doc,
            "u_aria",
            crate::demo::NOW * 1000,
            crate::testing::post("c_random", "anyone around?", None),
        )
        .unwrap();
        app.reload(&doc);
        assert_eq!(
            unread(&app, "c_random"),
            1,
            "the unseen message is counted unread"
        );
    }

    // Phase: mouse_wheel_scrolls_the_timeline.
    {
        let (mut app, mut doc) = app_with_doc();
        assert_eq!(app.scroll, 0.0);
        feed(&mut app, &mut doc, &wheel(3.0));
        assert!(app.scroll > 0.0, "scrolling moves the timeline");
        let up = app.scroll;
        feed(&mut app, &mut doc, &wheel(-5.0));
        assert!(app.scroll < up, "scrolling the other way moves it back");
    }

    // Phase: reading_history_does_not_mark_new_messages_read_until_returning_to_latest.
    {
        let (mut app, mut doc) = app_with_doc();
        app.mark_active_read(&doc);
        app.land(&mut doc);
        let watermark = crate::testing::read_mark(&doc, &app.active_channel, &app.me.id);

        // A remote message arrives after the current read boundary. Being in
        // the room is not enough to acknowledge it once the reader has moved
        // back through history.
        crate::testing::apply_part_at(
            &mut doc,
            "u_aria",
            (crate::demo::NOW + 1) * 1000,
            crate::testing::post(
                &app.active_channel,
                "A newer update while you are reading history",
                None,
            ),
        )
        .unwrap();
        app.timeline_at_end = false;
        app.mark_active_read(&doc);
        app.land(&mut doc);
        assert_eq!(
            crate::testing::read_mark(&doc, &app.active_channel, &app.me.id),
            watermark,
            "a reader away from the live edge does not send a false read receipt"
        );

        app.jump_to_latest();
        app.mark_active_read(&doc);
        app.land(&mut doc);
        assert!(
            crate::testing::read_mark(&doc, &app.active_channel, &app.me.id) > watermark,
            "the same message is acknowledged once the reader returns to it"
        );
    }

    // Phase: history_scroll_exposes_an_accessible_return_to_latest_control.
    {
        let (mut app, mut doc) = app_with_doc();
        // The ephemeral fixture opens a thread by default. On a compact
        // viewport that is now an honest pushed detail view, so navigate
        // back before testing the main timeline's history controls.
        app.on_hit(Hit::CloseThread, &mut ui::widget::Edits::lend(&mut doc));
        assert!(app.thread_open.is_none());
        for n in 0..24 {
            crate::testing::apply_part_at(
        &mut doc,
        "u_aria",
        (crate::demo::NOW + i64::from(n)) * 1000,
        crate::testing::post(&app.active_channel, &format!(
                        "Longer history line {n}: enough text to wrap inside a narrow conversation column."
                    ), None),
    )
    .unwrap();
        }
        app.reload(&doc);
        render_frame(&mut app, 390.0, 620.0);
        feed(&mut app, &mut doc, &wheel(12.0));
        render_frame(&mut app, 390.0, 620.0);

        assert!(
            !app.timeline_at_end,
            "history-directed wheel input leaves the live edge"
        );
        assert!(
            app.semantic_hits().iter().any(|(_, hit, name, role, _)| {
                matches!(hit, Hit::JumpToLatest)
                    && name == "Jump to latest message"
                    && *role == ui::a11y::Role::Button
            }),
            "the return route is a labelled semantic button, not pointer-only chrome"
        );

        app.on_hit(Hit::JumpToLatest, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.scroll, 0.0);
        assert!(app.timeline_at_end);
    }

    // Phase: wheeling_back_to_the_latest_message_asks_for_its_read_receipt_once.
    {
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(Hit::CloseThread, &mut ui::widget::Edits::lend(&mut doc));
        for n in 0..24 {
            crate::testing::apply_part_at(
                &mut doc,
                "u_aria",
                (crate::demo::NOW + i64::from(n)) * 1000,
                crate::testing::post(
                    &app.active_channel,
                    &format!("History line {n}, long enough to wrap in a narrow column of text."),
                    None,
                ),
            )
            .unwrap();
        }
        app.reload(&doc);
        render_frame(&mut app, 390.0, 620.0);
        feed(&mut app, &mut doc, &wheel(12.0));
        render_frame(&mut app, 390.0, 620.0);
        assert!(!app.timeline_at_end, "the wheel went back through history");
        app.read_on_present = false;
        feed(&mut app, &mut doc, &wheel(-12.0));
        assert!(
            !app.read_on_present,
            "a notch only pans: it asks nothing of the sync itself"
        );
        render_frame(&mut app, 390.0, 620.0);
        assert!(
            app.timeline_at_end,
            "wheeling back reaches the newest message"
        );
        assert!(
            app.read_on_present,
            "arriving at the newest message asks the sync for its read receipt"
        );
        app.read_on_present = false;
        render_frame(&mut app, 390.0, 620.0);
        assert!(
            !app.read_on_present,
            "staying at the newest message asks nothing again"
        );
    }

    // Phase: ctrl_end_returns_to_the_live_edge_without_stealing_plain_end.
    {
        let (mut app, mut doc) = app_with_doc();
        app.scroll = -180.0;
        app.timeline_at_end = false;
        feed_mods(
            &mut app,
            &mut doc,
            &named_key(NamedKey::End),
            mods(true, false),
        );
        assert_eq!(app.scroll, 0.0);
        assert!(app.timeline_at_end);
    }
}

/// A category with no channel in it is no heading to a member: the
/// channels read as one list. Whoever keeps the layout still sees it, to
/// add its first channel there.
#[test]
fn an_empty_category_is_listed_only_to_whoever_keeps_the_layout() {
    let (mut app, mut doc) = app_with_doc();
    crate::testing::insert_category(&mut doc, "k_empty", "chat");
    app.reload(&doc);
    let listed = |app: &mut ChatApp, category: &str| {
        let place = crate::sidebar::category_place(category);
        app.sidebar_sections()
            .iter()
            .any(|section| section.id == place)
    };
    assert!(app.keeps_layout());
    assert!(
        listed(&mut app, "k_empty"),
        "a moderator sees where to add a channel"
    );
    app.perms = crate::permissions::ChatPermissions::from_context(
        ui::permissions::PermissionContext::member("guest", ui::permissions::Role::Viewer),
    );
    assert!(!app.keeps_layout());
    assert!(
        !listed(&mut app, "k_empty"),
        "a member sees no empty heading"
    );
    let filled = app
        .categories
        .iter()
        .find(|category| !app.category_channels(&category.id).is_empty())
        .map(|category| category.id.clone())
        .expect("the fixture files a channel under a category");
    assert!(
        listed(&mut app, &filled),
        "a category with channels stays listed"
    );
}

#[test]
fn the_sidebar_names_the_channels_and_categories_of_a_chat_kept_flat() {
    let mut doc = crate::testing::chat_kept_flat(5);
    assert!(ui::tilo_resource::evolution::migrate::<crate::record::Chat>(&mut doc).unwrap());
    let mut app = ChatApp::new_ephemeral();
    app.reload(&doc);
    let sections = app.sidebar_sections();
    let titled: Vec<(&str, &str)> = sections
        .iter()
        .filter(|section| !section.title.is_empty())
        .map(|section| (section.id.as_str(), section.title.as_str()))
        .collect();
    assert_eq!(
        titled,
        [
            ("category:cat_home_text", "Channels"),
            ("category:cat_home_voice", "Voice")
        ]
    );
    let names: Vec<&str> = sections
        .iter()
        .flat_map(|section| &section.items)
        .map(|item| item.label.as_str())
        .collect();
    for shown in ["general", "Lounge"] {
        assert!(names.contains(&shown), "{shown} in {names:?}");
    }
}

#[test]
fn messages_the_chat_record_kept_show_in_their_channel_and_none_is_unread() {
    let mut doc = crate::testing::chat_with_kept_messages(2, false);
    assert!(ui::tilo_resource::evolution::migrate::<crate::record::Chat>(&mut doc).unwrap());
    let mut app = ChatApp::new_ephemeral();
    app.active_channel = "c_general".into();
    app.reload(&doc);
    let shown: Vec<(&str, &str, &str)> = app
        .messages
        .iter()
        .map(|message| {
            (
                message.id.as_str(),
                message.author.as_str(),
                message.text.as_str(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [("m1", "ada", "first"), ("m2", crate::testing::BO, "second")]
    );
    assert_eq!(
        app.channel("c_random").map(|channel| channel.unread),
        Some(0)
    );
    assert!(
        app.search_messages
            .iter()
            .any(|message| message.text == "elsewhere")
    );
}

#[test]
fn a_kept_message_names_its_author_as_the_record_kept_them_and_never_by_their_id() {
    let mut doc = crate::testing::chat_with_kept_messages(1, true);
    assert!(ui::tilo_resource::evolution::migrate::<crate::record::Chat>(&mut doc).unwrap());
    let mut app = ChatApp::new_ephemeral();
    app.active_channel = "c_general".into();
    app.reload(&doc);
    assert_eq!(app.user_name("ada"), "Ada Lovelace");
    let unnamed = app.user_name(crate::testing::BO);
    assert_eq!(unnamed, crate::identity::unnamed_label(crate::testing::BO));
    assert!(!unnamed.contains(crate::testing::BO));
    let read = crate::effects::read_messages(
        &app.held_messages(),
        ("c_general", &|id| app.display_name(id)),
        None,
        None,
    )
    .unwrap();
    assert!(read.to_string().contains("first") && read.to_string().contains("second"));
}

#[test]
fn a_read_receipt_the_door_refused_is_not_sent_again_until_the_person_reads_further() {
    let (mut app, mut doc) = app_with_doc();
    // Only receipts are submitted here.
    app.dispatched.borrow_mut().clear();
    let marks = |app: &ChatApp| app.dispatched.borrow().len();
    app.mark_active_read(&doc);
    assert_eq!(marks(&app), 1, "the channel in view is marked read");
    // The door does not offer this person the receipt (a viewer): it says so
    // and nothing lands.
    app.dispatch
        .as_ref()
        .expect("the window has its dispatcher")
        .refused()
        .set(Some(ui::framework::Refused {
            action: crate::resource::Chat::MARK_READ.id.into(),
            item: None,
            reason: "That didn't run. It isn't available to you here.".into(),
            by_room: false,
        }));
    for _ in 0..3 {
        app.mark_active_read(&doc);
    }
    assert_eq!(
        marks(&app),
        1,
        "a refused receipt is not sent again on every frame"
    );
    crate::testing::apply_part_at(
        &mut doc,
        "u_aria",
        (crate::demo::NOW + 1) * 1000,
        crate::testing::post(&app.active_channel, "Something new to read", None),
    )
    .unwrap();
    app.reload(&doc);
    app.mark_active_read(&doc);
    assert_eq!(marks(&app), 2, "reading further asks the door again");
}
