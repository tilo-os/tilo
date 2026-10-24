use super::*;
use crate::model;
use taffy::{NodeId, TaffyTree};
use tilo_automerge::ApplyIncoming;
use ui::Widget;

fn service_streams(app: &mut ChatApp, channel: &str, count: usize) {
    app.native_voice
        .submit(ui::native_media::Command::Join {
            channel_id: channel.into(),
        })
        .unwrap();
    let requests = app.native_voice.take_requests(ui::time::now());
    let id = requests
        .iter()
        .rev()
        .find_map(|request| match request {
            ui::collaboration::CollabRequest::NativeMediaCommand {
                request_id,
                command,
            } if matches!(command.command, ui::native_media::Command::Join { .. }) => {
                Some(*request_id)
            }
            _ => None,
        })
        .unwrap();
    let mut snapshot = ui::native_media::Snapshot::unavailable();
    snapshot.state = ui::native_media::State::Live;
    snapshot.issue = None;
    snapshot.inventory_revision = Some(1);
    snapshot.publishers = (1..=count)
        .map(|index| format!("{}/{index:064x}", "11".repeat(32)))
        .collect();
    if count > 0 {
        snapshot.rosters = vec![ui::native_media::Roster {
            channel_id: channel.into(),
            people: (1..=count).map(|index| format!("{index:064x}")).collect(),
            ..Default::default()
        }];
    }
    assert!(snapshot.valid());
    app.native_voice.complete(id, &Ok(snapshot));
    app.project_native_voice();
}

fn app_with_doc() -> (ChatApp, crate::testing::Room) {
    let doc = crate::testing::demo_doc();
    let mut app = ChatApp::new_ephemeral();
    app.attach_room(&doc);
    app.reload(&doc);
    (app, doc)
}

/// Exercise a domain action through the retained menu's semantic
/// activation channel—the same selection channel used by Enter/click.
fn activate_context_menu_action(
    app: &mut ChatApp,
    doc: &mut AutoCommit,
    target: MenuTarget,
    action: MenuAction,
) {
    let items = match &target {
        MenuTarget::Channel(_) => MenuAction::for_channel().to_vec(),
        MenuTarget::Category(_) => MenuAction::for_category().to_vec(),
        MenuTarget::Message(_) => MenuAction::for_message().to_vec(),
        MenuTarget::Room => MenuAction::for_room().to_vec(),
        MenuTarget::CallParticipant(_) => MenuAction::for_call_participant().to_vec(),
    };
    let index = items
        .iter()
        .position(|candidate| *candidate == action)
        .expect("action belongs to this menu");
    app.rect = vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0);
    app.menu = Some(app.context_menu(target, Point::new(120.0, 120.0), items));
    assert!(ui::Widget::perform_semantic_action(
        app,
        &format!("context-menu:item:{index}"),
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(doc),
        &automerge::ROOT,
    ));
    assert!(app.menu.is_none(), "selection closes the modal menu");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one labelled phase per original scenario, each with its own fresh fixture"
)]
fn channel_context_menu_delete_through_the_palette_action() {
    // Phase: delete_channel_affordance_matches_palette_spec.
    {
        let (mut app, mut doc) = app_with_doc();

        // The palette's delete spec for c_crit (built before the mutation).
        let state = <crate::resource::Chat as ui::tilo_resource::Resource>::read(&doc).unwrap();
        let crit = <crate::resource::Chat as ui::tilo_resource::Resource>::child(
            &state,
            &crate::testing::item("channels", "c_crit"),
        )
        .expect("c_crit is a channel resource");
        let offered = tilo_actions::door::survey(std::slice::from_ref(&crit))
            .into_iter()
            .find(|offered| offered.address.action == "delete-channel")
            .expect("the channel offers delete-channel");
        let from_spec = tilo_actions::door::encode_json(
            std::slice::from_ref(&crit),
            &offered.address,
            &serde_json::Map::new(),
        )
        .expect("the channel offers delete-channel with itself bound");
        let from_spec: serde_json::Value = serde_json::from_slice(&from_spec.payload).unwrap();
        assert_eq!(
            from_spec["op"],
            crate::testing::operation(
                &crate::resource::Chat::DELETE_CHANNEL
                    .with(ui::tilo_resource::Id::from_raw("c_crit"))
            ),
            "the palette's delete-channel is the record's verb on that channel"
        );

        // Drive the real on-screen affordance (right-click menu → Delete).
        activate_context_menu_action(
            &mut app,
            &mut doc,
            MenuTarget::Channel("c_crit".into()),
            MenuAction::Delete,
        );
        let dispatched = app
            .last_dispatched()
            .expect("the affordance dispatched an action");

        assert_eq!(
            serde_json::to_value(&dispatched).unwrap(),
            from_spec["op"],
            "the delete-channel affordance and its palette action build the identical action"
        );
        // And it really deleted the channel once the person confirmed (the
        // affordance is on the real path).
        app.confirm_asked(&mut doc);
        assert!(
            crate::testing::channels(&doc)
                .into_iter()
                .all(|c| c.id != "c_crit")
        );
    }

    // Phase: context_menu_delete_removes_a_channel.
    {
        let (mut app, mut doc) = app_with_doc();
        assert!(app.channels.iter().any(|c| c.id == "c_random"));
        activate_context_menu_action(
            &mut app,
            &mut doc,
            MenuTarget::Channel("c_random".into()),
            MenuAction::Delete,
        );
        app.confirm_asked(&mut doc);
        assert!(
            !app.channels.iter().any(|c| c.id == "c_random"),
            "channel deleted via menu"
        );
    }

    // Phase: context_menu_is_modal_keyboard_operable_and_accessible.
    {
        let (mut app, mut doc) = app_with_doc();
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0);
        app.menu = Some(app.context_menu(
            MenuTarget::Channel("c_random".into()),
            Point::new(850.0, 680.0),
            MenuAction::for_channel().to_vec(),
        ));

        let root = ui::Widget::accessibility(&app).expect("Chat semantic root");
        let menu = root
            .children
            .iter()
            .find(|node| node.role == ui::a11y::Role::Menu)
            .expect("open shared menu is in Chat's semantic tree");
        assert_eq!(menu.children.len(), MenuAction::for_channel().len());
        assert_eq!(menu.children[0].name.as_deref(), Some("Edit channel"));
        assert_eq!(menu.children[1].name.as_deref(), Some("Rename channel"));
        assert_eq!(menu.children[2].name.as_deref(), Some("Archive channel"));
        assert_eq!(
            menu.children[5].name.as_deref(),
            Some("Delete channel"),
            "the destructive action comes last"
        );
        assert!(ui::Widget::traps_focus(&app));
        let capture = ui::Widget::modal_capture(&app).expect("menu captures input");
        assert!(app.rect.contains(capture.center()));

        // The first row is the safe action the keyboard lands on.
        for key in [NamedKey::ArrowDown, NamedKey::Enter] {
            let mut handled = false;
            app.dispatch_event(
                &named_key(key),
                1.0,
                Modifiers::default(),
                &mut ui::widget::Edits::lend(&mut doc),
                &mut handled,
            );
            assert!(handled);
        }
        assert!(app.menu.is_none());
        assert!(
            app.channel_sheet.is_some(),
            "Enter activates the safely focused non-destructive action"
        );
    }
}

#[test]
fn with_no_channel_open_the_composer_takes_no_text() {
    // A chat with no channels yet, as a room opens one its operator named
    // none for.
    let mut doc = AutoCommit::new();
    let mut app = ChatApp::new_ephemeral();
    app.reload(&doc);
    assert!(
        app.channel(&app.active_channel).is_none(),
        "no channel is open"
    );
    app.focus = Focus::Main;
    app.set_composer_focus();
    type_str(&mut app, &mut doc, "nowhere to go");
    assert_eq!(app.composer_main.value(), "", "no channel, no draft");
    feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
    assert!(
        !app.dispatched_parts
            .borrow()
            .iter()
            .map(|action| serde_json::to_value(action).unwrap())
            .any(|action| action["action"] == "post"),
        "nothing was posted"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one labelled phase per original scenario, each with its own fresh fixture"
)]
fn composer_edits_the_draft_and_sends_through_the_action_route() {
    // Phase: send_affordance_dispatches_a_send_message_action.
    {
        let (mut app, mut doc) = app_with_doc();
        app.active_channel = "c_general".into();
        app.focus = Focus::Main;
        app.thread_open = None;
        crate::testing::write_draft(&mut app, &mut doc, "hi from the composer");
        app.on_hit(Hit::Send, &mut ui::widget::Edits::lend(&mut doc));
        let action = app
            .dispatched_parts
            .borrow()
            .iter()
            .map(|action| serde_json::to_value(action).unwrap())
            .find(|action| action["action"] == "post")
            .expect("send dispatched a post");
        assert_eq!(action["args"]["channel"], "c_general");
        assert_eq!(action["args"]["text"], "hi from the composer");
    }

    // Phase: typing_then_send_appends_a_message.
    {
        let (mut app, mut doc) = app_with_doc();
        let before = crate::testing::messages(&doc, "c_crit").len();
        app.focus = Focus::Main;
        app.set_composer_focus();
        // Type through the real composer TextInput, then Enter to send
        // (multiline + enter-submits: plain Enter sends).
        type_str(&mut app, &mut doc, "hello team");
        assert_eq!(
            app.composer_main.value(),
            "hello team",
            "composer holds the typed text"
        );
        assert_eq!(
            app.draft_main, "hello team",
            "the draft is mirrored into the CRDT"
        );
        feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
        let after = crate::testing::messages(&doc, "c_crit");
        assert_eq!(after.len(), before + 1);
        assert_eq!(after.last().unwrap().text, "hello team");
        assert_eq!(app.composer_main.value(), "", "composer cleared after send");
        assert_eq!(app.draft_main, "", "draft cleared after send");
    }

    // Phase: backspace_edits_the_draft.
    {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        app.set_composer_focus();
        type_str(&mut app, &mut doc, "abc");
        feed(&mut app, &mut doc, &named_key(NamedKey::Backspace));
        assert_eq!(
            app.composer_main.value(),
            "ab",
            "backspace removed the last char"
        );
        assert_eq!(app.composer_main.caret(), 2, "caret follows the deletion");
        assert_eq!(app.draft_main, "ab", "the draft mirrors the edit");
    }

    // Phase: ctrl_v_pastes_clipboard_into_the_composer.
    {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        app.set_composer_focus();

        let sentinel = "pasted from the clipboard";
        ui::clipboard_copy(sentinel);
        assert_eq!(
            ui::clipboard_paste().as_deref(),
            Some(sentinel),
            "clipboard round-trips here (precondition for the paste path)"
        );

        feed_mods(&mut app, &mut doc, &char_key("v"), mods(true, false));

        assert_eq!(
            app.composer_main.value(),
            sentinel,
            "Ctrl+V landed the clipboard text in the composer"
        );
        assert_eq!(
            app.draft_main, sentinel,
            "the pasted draft is mirrored into the CRDT"
        );
    }

    // Phase: drag_select_then_ctrl_c_copies_the_selection.
    {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        app.set_composer_focus();
        type_str(&mut app, &mut doc, "copy me please");
        // A frame lays the composer out + populates its glyph metrics so a
        // pointer hit-test resolves a caret position.
        render_frame(&mut app, 1280.0, 720.0);

        // Select the whole value (Ctrl+A) then copy it (Ctrl+C).
        feed_mods(&mut app, &mut doc, &char_key("a"), mods(true, false));
        ui::clipboard_copy("stale"); // ensure the copy overwrites something
        feed_mods(&mut app, &mut doc, &char_key("c"), mods(true, false));
        assert_eq!(
            ui::clipboard_paste().as_deref(),
            Some("copy me please"),
            "Ctrl+C copied the selected composer text to the clipboard"
        );
    }

    // Phase: composer_emoji_pick_inserts_into_the_draft.
    {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        assert!(app.draft_main.is_empty());
        app.on_hit(
            Hit::ComposerEmoji(Point::ZERO),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(
            app.emoji_open && app.emoji_target.is_none(),
            "the composer picker has no reaction target"
        );
        app.on_hit(
            Hit::EmojiChoice("🎉".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(
            app.draft_main, "🎉",
            "the emoji was typed into the composer draft"
        );
        assert_eq!(app.emoji_recents.items(), ["🎉"], "picks are remembered");
    }

    // Phase: reply_in_thread_sets_parent.
    {
        let (mut app, mut doc) = app_with_doc();
        app.thread_open = Some("m_concept".into());
        app.focus = Focus::Thread;
        app.insert_text(&mut ui::widget::Edits::lend(&mut doc), "nice");
        app.send_focused(&mut ui::widget::Edits::lend(&mut doc));
        let replies = model::reply_count(&crate::testing::messages(&doc, "c_crit"), "m_concept");
        assert_eq!(
            replies, 4,
            "thread gained one reply on top of the seeded three"
        );
    }
}

/// Deleting a message of your own from its menu asks nothing: it goes
/// at once, and the Undo under the composer brings it back through the
/// history of the part it was in.
#[test]
fn deleting_your_own_message_asks_nothing_and_undo_brings_it_back() {
    let (mut app, mut doc) = app_with_doc();
    app.active_channel = "c_general".into();
    app.focus = Focus::Main;
    app.thread_open = None;
    crate::testing::write_draft(&mut app, &mut doc, "a slip of the keys");
    app.on_hit(Hit::Send, &mut ui::widget::Edits::lend(&mut doc));
    app.reload(&doc);
    let mine = |doc: &crate::testing::Room| {
        crate::testing::messages(doc, "c_general")
            .into_iter()
            .find(|message| message.text == "a slip of the keys")
            .map(|message| message.id)
    };
    let id = mine(&doc).expect("the message was sent");

    activate_context_menu_action(
        &mut app,
        &mut doc,
        MenuTarget::Message(id.clone()),
        MenuAction::DeleteMessage,
    );
    let chain = app.chain.clone().expect("the window holds the messages");
    assert_eq!(
        chain.dispatcher().take_asked().map(|asked| asked.len()),
        Ok(0),
        "deleting your own message asks nothing"
    );
    assert_eq!(mine(&doc), None, "the message goes at once");
    app.reload(&doc);
    render_frame(&mut app, 1280.0, 720.0);
    assert!(
        app.semantic_hits()
            .into_iter()
            .any(|(_, hit, ..)| matches!(hit, Hit::UndoDelete)),
        "Undo is offered under the composer"
    );

    app.on_hit(Hit::UndoDelete, &mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(mine(&doc), Some(id), "Undo brings the message back");
    assert!(app.deleted.borrow().is_none(), "and the offer is gone");
}

#[test]
fn a_refused_post_keeps_its_text_and_says_it_was_not_sent() {
    let (mut app, mut doc) = app_with_doc();
    app.active_channel = "c_general".into();
    app.focus = Focus::Main;
    app.thread_open = None;
    let before = crate::testing::top_level(&doc, "c_general");
    let too_long = "x".repeat(crate::record::MAX_MESSAGE_TEXT_BYTES + 1);
    crate::testing::write_draft(&mut app, &mut doc, &too_long);
    app.on_hit(Hit::Send, &mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(crate::testing::top_level(&doc, "c_general"), before);
    assert_eq!(
        app.composer_main.value(),
        too_long,
        "the text waits to be sent again"
    );
    assert!(
        app.not_sent.is_some(),
        "the refusal is shown, never swallowed"
    );
    render_frame(&mut app, 1280.0, 720.0);
    let retries = app
        .hits
        .iter()
        .filter(|(rect, hit)| matches!(hit, Hit::Send) && rect.y0 >= app.composer_main.rect().y1)
        .count();
    assert_eq!(retries, 1, "Retry stands under the composer");
}

/// A post the room refuses after this window showed it as sent comes
/// back to the composer, marked as not sent.
#[test]
fn a_post_the_room_refuses_comes_back_to_the_composer() {
    let (mut app, mut doc) = app_with_doc();
    app.active_channel = "c_general".into();
    app.focus = Focus::Main;
    app.thread_open = None;
    crate::testing::write_draft(&mut app, &mut doc, "over the budget");
    app.on_hit(Hit::Send, &mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(
        app.composer_main.value(),
        "",
        "sent, as far as this window knows"
    );
    let refused = app
        .chain
        .as_ref()
        .expect("the window holds the messages")
        .dispatcher()
        .refused();
    refused.set(Some(ui::framework::Refused {
        action: "post".into(),
        item: None,
        reason: "the room is receiving too many changes from you".into(),
        by_room: true,
    }));
    app.settle_refused_post();
    assert_eq!(app.composer_main.value(), "over the budget");
    assert_eq!(
        app.not_sent.as_deref(),
        Some("the room is receiving too many changes from you")
    );
}

#[test]
fn semantic_send_reports_acceptance_only_when_it_appends_a_message() {
    let (mut app, mut doc) = app_with_doc();
    app.active_channel = "c_general".into();
    app.focus = Focus::Main;
    app.thread_open = None;
    render_frame(&mut app, 1280.0, 720.0);
    let target = app
        .semantic_hits()
        .into_iter()
        .find_map(|(_, hit, ..)| matches!(hit, Hit::Send).then(|| hit.semantic_id()))
        .expect("the painted Send button has a semantic target");

    assert!(!ui::Widget::perform_semantic_action(
        &mut app,
        &target,
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    let before = crate::testing::top_level(&doc, "c_general");
    crate::testing::write_draft(&mut app, &mut doc, "accepted semantic send");
    render_frame(&mut app, 1280.0, 720.0);
    assert!(ui::Widget::perform_semantic_action(
        &mut app,
        &target,
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert_eq!(crate::testing::top_level(&doc, "c_general"), before + 1);
    assert_eq!(app.composer_main.value(), "");
    assert!(
        !crate::projection::has_draft(&doc, "c_general"),
        "a draft never enters the room's record"
    );
}

#[test]
fn live_start_selects_the_rooms_first_real_channel() {
    // Against a real (minimal) starter document whose ids differ from the
    // demo defaults the app boots with, the reload must re-home the
    // active channel and drop the stale demo thread.
    let mut doc = AutoCommit::new();
    crate::testing::starter(&mut doc, model::ME, "You", "Y");
    let mut app = ChatApp::new_ephemeral(); // bare view state → demo ids
    app.reload(&doc);
    if app.ensure_selection() {
        app.reload(&doc);
    }
    assert_eq!(
        app.active_channel,
        crate::record::CHAT_CHANNEL,
        "picked the first text channel"
    );
    assert!(app.thread_open.is_none(), "stale demo thread dropped");
    // And nothing fake leaked in.
    assert!(app.users.iter().all(|u| u.id == model::ME));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one labelled phase per original scenario, each with its own fresh fixture"
)]
fn attachments_are_picked_imported_and_posted_as_real_portable_cards() {
    // Phase: attachment_image_decodes_through_the_image_store.
    {
        // The concept message in #design-crit carries an image attachment;
        // prewarming must drive it through the real fetch → decode → cache
        // path so a frame paints a decoded thumbnail (not a placeholder).
        let (app, _doc) = app_with_doc();
        assert!(
            app.messages.iter().any(|m| matches!(
                &m.attachment,
                Some(crate::model::Attachment::Image { blob, .. }) if blob == crate::images::DEMO_CALL_GRID
            )),
            "seeded conversation has an image attachment"
        );
        app.request_visible_images();
        app.images.block_until_idle(tilo_test_env::hang_bound());
        assert!(
            app.images.get(crate::images::DEMO_CALL_GRID).is_some(),
            "the shared ImageStore decoded the attachment thumbnail"
        );
    }

    // Phase: attach_shares_a_real_file_with_true_dimensions_and_no_caption.
    {
        // The picker imports the selected file into the content store, with
        // true header dimensions and no invented caption.
        let (mut app, mut doc) = app_with_doc();
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("diagram.png");
        write_real_png(&png, 240, 160);

        let before = crate::testing::messages(&doc, "c_crit").len();
        app.on_hit(
            Hit::AttachPick(png.to_string_lossy().into_owned()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(
            crate::testing::messages(&doc, "c_crit").len(),
            before,
            "selecting only accepts a bounded local import; sync authors the message"
        );
        settle_attachment_until(&mut app, &mut doc, "c_crit", before + 1);
        let msgs = crate::testing::messages(&doc, "c_crit");
        assert_eq!(msgs.len(), before + 1, "one real attachment posted");
        let last = msgs.last().unwrap();
        assert_eq!(last.text, "", "no invented caption");
        match &last.attachment {
            Some(model::Attachment::Image { blob, w, h, .. }) => {
                assert_eq!(blob.len(), 64, "posts a content hash");
                assert!(app.content.has(blob));
                assert_eq!(
                    (*w, *h),
                    (240, 160),
                    "reads the file's true header dimensions"
                );
            }
            other => panic!("expected a real image attachment, got {other:?}"),
        }
    }

    // Phase: attach_button_opens_picker_listing_real_workspace_files.
    {
        let (mut app, mut doc) = app_with_doc();
        let dir = tempfile::tempdir().unwrap();
        write_real_png(&dir.path().join("shot.png"), 64, 64);
        // Point the workspace picker at the temp dir and open it via the real
        // event path (the composer "+").
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_CHAT_ATTACH_DIR", dir.path());
        app.on_hit(Hit::Attach, &mut ui::widget::Edits::lend(&mut doc));
        drop(env);
        assert!(app.attach_open, "the picker opens");
        assert!(
            app.attachment_picker_pending.is_some(),
            "directory work is off-thread"
        );
        settle_picker_until(&mut app);
        assert!(
            app.attach_entries.iter().any(|e| e.name == "shot.png"),
            "a real workspace image is listed, not a synthesized stand-in"
        );
    }

    // Phase: picker_result_settlement_is_generation_checked_and_cancelled_when_dismissed.
    {
        let (mut app, doc) = app_with_doc();
        app.attach_open = true;
        app.attachment_picker_pending = Some(7);
        app.attachment_picker_result_tx
            .try_send(AttachmentPickerResult {
                generation: 6,
                entries: Vec::new(),
            })
            .unwrap();
        app.settle_attachment_picker();
        assert_eq!(app.attachment_picker_pending, Some(7));

        // Reopening while a result is pending does not create a second scan.
        app.attachment_picker_generation = 12;
        app.open_attach_picker();
        assert_eq!(app.attachment_picker_generation, 12);
        assert_eq!(app.attachment_picker_pending, Some(7));

        // A dismissed picker consumes its matching worker answer as a
        // cancellation, rather than leaking the bounded worker slot.
        app.attach_open = false;
        app.attachment_picker_result_tx
            .try_send(AttachmentPickerResult {
                generation: 7,
                entries: Vec::new(),
            })
            .unwrap();
        app.settle_attachment_picker();
        assert!(app.attachment_picker_pending.is_none());
        assert_eq!(
            app.attachment_status.as_deref(),
            Some("Attachment picker cancelled")
        );

        // Keep the test on the same real event/sync surface as other picker
        // tests: a cancellation never authors a document operation.
        assert!(!crate::testing::messages(&doc, "c_crit").is_empty());
    }

    // Phase: attaching_a_video_posts_an_inline_video_card.
    {
        let (mut app, mut doc) = app_with_doc();
        let dir = tempfile::tempdir().unwrap();
        let mp4 = dir.path().join("clip.mp4");
        std::fs::write(&mp4, b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00moov").unwrap();
        let before = crate::testing::messages(&doc, "c_crit").len();
        app.on_hit(
            Hit::AttachPick(mp4.to_string_lossy().into_owned()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        settle_attachment_until(&mut app, &mut doc, "c_crit", before + 1);
        let msgs = crate::testing::messages(&doc, "c_crit");
        assert_eq!(msgs.len(), before + 1, "the video posted as one message");
        match &msgs.last().unwrap().attachment {
            Some(model::Attachment::Video {
                name, blob, size, ..
            }) => {
                assert_eq!(name, "clip.mp4");
                assert_eq!(blob.len(), 64, "references a content hash");
                assert!(app.content.has(blob));
                assert!(*size > 0, "reads the real file size");
            }
            other => panic!("expected a Video attachment, got {other:?}"),
        }
    }

    // Phase: attaching_a_generic_file_posts_a_portable_file_card.
    {
        let (mut app, mut doc) = app_with_doc();
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("notes.md");
        std::fs::write(&note, b"this is an ordinary attachment").unwrap();
        let before = crate::testing::messages(&doc, "c_crit").len();

        app.on_hit(
            Hit::AttachPick(note.to_string_lossy().into_owned()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        settle_attachment_until(&mut app, &mut doc, "c_crit", before + 1);
        match &crate::testing::messages(&doc, "c_crit")
            .last()
            .unwrap()
            .attachment
        {
            Some(model::Attachment::File { name, blob, size }) => {
                assert_eq!(name, "notes.md");
                assert_eq!(*size, 30);
                assert!(app.content.has(blob));
            }
            other => panic!("expected a generic File attachment, got {other:?}"),
        }
    }

    // Phase: stale_import_result_does_not_consume_the_accepted_request.
    {
        let (mut app, mut doc) = app_with_doc();
        app.attachment_pending = Some(AttachmentImportPending {
            token: 7,
            channel: app.active_channel.clone(),
        });
        app.attachment_result_tx
            .try_send(AttachmentImportResult::Failed {
                token: 6,
                reason: "old worker".into(),
            })
            .unwrap();

        app.settle_attachment_import();
        app.land(&mut doc);
        assert_eq!(
            app.attachment_pending.as_ref().map(|pending| pending.token),
            Some(7)
        );
        assert!(
            app.attachment_status
                .as_deref()
                .is_some_and(|status| status.contains("stale"))
        );
    }
}

#[test]
fn switching_to_voice_channel_starts_a_muted_service_join_without_membership_write() {
    let (mut app, mut doc) = app_with_doc();
    let before = doc.save();
    app.on_hit(
        Hit::Channel("c_standup".into()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    app.land(&mut doc);
    app.reload(&doc);
    assert!(app.call.active && app.call.muted && app.call.audio_only);
    assert_eq!(app.call.channel, "c_standup");
    assert_eq!(
        app.native_voice.snapshot.state,
        ui::native_media::State::Preparing
    );
    assert_eq!(doc.save(), before);
}

#[test]
fn expanding_a_call_preserves_verified_streams_without_rewriting_legacy_history() {
    let (mut app, mut doc) = app_with_doc();
    app.on_hit(
        Hit::Channel("c_huddle".into()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    app.land(&mut doc);
    app.reload(&doc);
    service_streams(&mut app, "c_huddle", 3);
    let before = doc.save();
    app.on_hit(Hit::CallExpand, &mut ui::widget::Edits::lend(&mut doc));
    assert!(app.call.expanded);
    assert_eq!(app.voice_in("c_huddle").len(), 3);
    assert_eq!(doc.save(), before);
}

#[test]
fn call_overlay_keeps_the_roster_and_fills_its_surface_at_every_size() {
    // Phase: expanded_overlay_fills_the_surface_at_every_width.
    {
        // The narrow/portrait void regression, exercised through the real app:
        // join the demo huddle, expand, and at each target width the host +
        // remote tiles must cover the whole content band — no large empty
        // region beneath or beside the host.
        use vello::kurbo::Rect;
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(
            Hit::Channel("c_huddle".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        service_streams(&mut app, "c_huddle", 3);
        app.on_hit(Hit::CallExpand, &mut ui::widget::Edits::lend(&mut doc));
        assert!(app.call.active && app.call.expanded);

        for &(w, h) in &[(1280.0_f64, 720.0_f64), (800.0, 600.0), (560.0, 720.0)] {
            app.rect = Rect::new(0.0, 0.0, w, h);
            let region = app.overlay_content_region();
            let tiles = app.overlay_tiles();
            assert_eq!(tiles.len(), 4, "w={w}: host + three remotes → four tiles");
            for t in &tiles {
                assert!(
                    t.x0 >= region.x0 - 0.5
                        && t.x1 <= region.x1 + 0.5
                        && t.y0 >= region.y0 - 0.5
                        && t.y1 <= region.y1 + 0.5,
                    "w={w}: a tile escaped the content region"
                );
            }
            // Bounding box covers the region (within one gap) in both axes.
            let bbox =
                tiles
                    .iter()
                    .fold(Rect::new(f64::MAX, f64::MAX, f64::MIN, f64::MIN), |b, r| {
                        Rect::new(
                            b.x0.min(r.x0),
                            b.y0.min(r.y0),
                            b.x1.max(r.x1),
                            b.y1.max(r.y1),
                        )
                    });
            let slack = region.width().min(region.height()) * 0.05 + 17.0;
            assert!(
                region.x1 - bbox.x1 <= slack && bbox.x0 - region.x0 <= slack,
                "w={w}: horizontal void — bbox x {:?} vs region x [{:.0},{:.0}]",
                (bbox.x0, bbox.x1),
                region.x0,
                region.x1
            );
            assert!(
                region.y1 - bbox.y1 <= slack && bbox.y0 - region.y0 <= slack,
                "w={w}: vertical void below the tiles — bbox y1 {:.0} vs region y1 {:.0}",
                bbox.y1,
                region.y1
            );
            // The host is a genuine cell, not a pinned sliver.
            let host = app.overlay_host_rect();
            assert_eq!(host, tiles[0], "host is the first grid cell");
            assert!(
                host.width() > region.width() * 0.25 && host.height() > region.height() * 0.25,
                "w={w}: host tile is a real share of the surface, not a corner sliver"
            );
        }
    }

    // Phase: call_resize_samples_keep_content_and_native_chrome_in_one_geometry.
    {
        let (mut app, _doc) = app_with_doc();
        app.call.join("c_huddle");
        render_frame(&mut app, 1200.0, 800.0);
        app.stage_floating_call("floating-call-collapse-000");

        let stable_content = app.call_video_rect();
        let mut prior_height = f64::INFINITY;
        for reveal in [1.0_f32, 0.75, 0.5, 0.25, 0.0] {
            app.call_chrome_motion.pin_for_visual_review(reveal);
            render_frame(&mut app, 1200.0, 800.0);
            let presentation =
                ui::widget::FloatingSurfaceWidgetExt::floating_surface_presentations(&app)
                    .into_iter()
                    .next()
                    .expect("the detached call publishes its animated native bounds");
            assert_eq!(presentation.content_rect, stable_content);
            assert_eq!(
                presentation.window_rect,
                app.huddle_panel().inflate(8.0, 8.0)
            );
            assert!(presentation.window_rect.height() <= prior_height);
            prior_height = presentation.window_rect.height();
        }
        assert_eq!(prior_height, app.huddle_panel().inflate(8.0, 8.0).height());
    }
}

#[test]
fn leaving_a_call_clears_local_service_state_without_rewriting_history() {
    let (mut app, mut doc) = app_with_doc();
    app.on_hit(
        Hit::Channel("c_standup".into()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    app.land(&mut doc);
    app.reload(&doc);
    service_streams(&mut app, "c_standup", 2);
    let before = doc.save();
    assert_eq!(app.voice_in("c_standup").len(), 2);
    app.on_hit(Hit::CallLeave, &mut ui::widget::Edits::lend(&mut doc));
    assert!(!app.call.active && app.call.muted);
    assert!(!app.voice_people("c_standup").contains(&app.me.id));
    assert_eq!(
        app.voice_in("c_standup").len(),
        2,
        "the others stay in the room's call"
    );
    assert_eq!(doc.save(), before);
}

fn add_channel_asks_then_creates_and_opens_it() {
    let (mut app, mut doc) = app_with_doc();
    let before = app.channels.len();
    app.on_hit(
        Hit::AddChannel {
            category: "cat_acme_main".into(),
        },
        &mut ui::widget::Edits::lend(&mut doc),
    );
    assert_eq!(
        app.channels.len(),
        before,
        "nothing is made before it is named"
    );
    assert!(app.channel_sheet.is_some(), "the channel sheet asks");
    type_str(&mut app, &mut doc, "Launch Plans");
    feed(&mut app, &mut doc, &named_key(NamedKey::Tab));
    type_str(&mut app, &mut doc, "Dates");
    feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
    assert!(app.channel_sheet.is_none(), "Enter creates and closes it");
    let made = app
        .channels
        .iter()
        .find(|c| c.name == "launch-plans")
        .expect("the channel is made under its kept name");
    assert_eq!(made.category, "cat_acme_main");
    assert_eq!(made.topic, "Dates");
    assert_eq!(made.kind, ChannelKind::Text);
    assert_eq!(app.active_channel, made.id, "a new text channel opens");
}

#[test]
fn channels_and_categories_are_created_renamed_and_reordered_from_the_sidebar() {
    add_channel_asks_then_creates_and_opens_it();

    // Phase: a_menus_rename_names_the_row_in_the_sidebar.
    {
        let (mut app, mut doc) = app_with_doc();
        app.apply_menu_action(
            MenuTarget::Channel("c_random".into()),
            MenuAction::Rename,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(
            app.sidebar_rename.as_deref(),
            Some("channel:c_random"),
            "the sidebar is asked to open the name in place"
        );
        app.rename_place(
            &MenuTarget::Channel("c_random".into()),
            " watercooler ",
            &mut ui::widget::Edits::lend(&mut doc),
        );
        let c = app.channels.iter().find(|c| c.id == "c_random").unwrap();
        assert_eq!(c.name, "watercooler");
        app.rename_place(
            &MenuTarget::Channel("c_random".into()),
            "  ",
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(
            app.channel("c_random").unwrap().name,
            "watercooler",
            "an empty name changes nothing"
        );
    }

    // Phase: category_create_and_collapse_via_the_hit_path.
    {
        let (mut app, mut doc) = app_with_doc();
        let acme_cats = |app: &ChatApp| app.categories.len();
        let before = acme_cats(&app);
        app.open_channel_sheet(crate::channel_sheet::ChannelSheet::new_category());
        type_str(&mut app, &mut doc, "Research");
        feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
        assert_eq!(acme_cats(&app), before + 1, "a category was created");
        assert!(app.categories.iter().any(|c| c.name == "Research"));
        let collapsed = |app: &ChatApp| {
            app.categories
                .iter()
                .any(|category| category.id == "cat_acme_main" && category.collapsed)
        };
        let collapsed_before = collapsed(&app);
        app.apply_menu_action(
            MenuTarget::Category("cat_acme_main".into()),
            MenuAction::ToggleCollapse,
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_ne!(
            collapsed(&app),
            collapsed_before,
            "the menu's Collapse toggles the category collapsed state"
        );
    }

    // Phase: reorder_channel_via_menu_action.
    {
        let (mut app, mut doc) = app_with_doc();
        let order =
            |app: &ChatApp, id: &str| app.channels.iter().find(|c| c.id == id).unwrap().order;
        assert!(order(&app, "c_crit") > order(&app, "c_general"));
        activate_context_menu_action(
            &mut app,
            &mut doc,
            MenuTarget::Channel("c_crit".into()),
            MenuAction::MoveUp,
        );
        assert!(
            order(&app, "c_crit") < order(&app, "c_general"),
            "MoveUp swapped order with the sibling above it"
        );
    }
}

#[test]
fn message_lines_keep_a_readable_length_on_a_wide_screen() {
    use ui::theme::Theme;
    use vello::kurbo::Rect;
    let (app, _doc) = app_with_doc();
    let theme = Theme::dark();
    let readable = theme.tokens.metrics.readable_width;
    let narrow = Rect::new(0.0, 0.0, 600.0, 800.0);
    let wide = Rect::new(0.0, 0.0, 2200.0, 1400.0);
    assert!(
        app.message_text_width(narrow, &theme) < readable,
        "a narrow column uses the room it has"
    );
    assert!(
        (app.message_text_width(wide, &theme) - readable).abs() < f32::EPSILON,
        "a wide one stops at the readable measure"
    );
}

#[test]
fn an_empty_channel_invites_the_first_message() {
    let (mut app, _doc) = app_with_doc();
    let id = app.active_channel.clone();
    let name = app.channel(&id).expect("an active channel").name.clone();
    app.channels
        .iter_mut()
        .filter(|c| c.id == id)
        .for_each(|c| c.topic.clear());
    let invite = app.empty_channel_message().expect("an invitation");
    assert!(invite.contains(&name), "{invite}");

    app.channels
        .iter_mut()
        .filter(|c| c.id == id)
        .for_each(|c| c.topic = "Release planning".into());
    assert_eq!(
        app.empty_channel_message().as_deref(),
        Some("Release planning"),
        "a topic says what the channel is for"
    );

    app.channels
        .iter_mut()
        .filter(|c| c.id == id)
        .for_each(|c| c.topic.clear());
    app.perms = crate::permissions::ChatPermissions::from_context(
        ui::permissions::PermissionContext::member("reader", ui::permissions::Role::Viewer),
    );
    assert_eq!(app.empty_channel_message(), None, "no invitation to write");
}

#[test]
fn the_message_menu_adds_a_first_reaction_and_starts_a_thread() {
    // The reaction row's "+" and the thread bar exist only once a
    // message has reactions or replies, so the menu is how anyone
    // reacts first or replies first.
    assert!(MenuAction::for_others_message().contains(&MenuAction::AddReaction));
    assert!(MenuAction::for_others_message().contains(&MenuAction::ReplyInThread));
    let (mut app, mut doc) = app_with_doc();
    let id = app
        .messages
        .iter()
        .find(|m| m.reactions.is_empty())
        .map(|m| m.id.clone())
        .expect("a message without reactions");
    activate_context_menu_action(
        &mut app,
        &mut doc,
        MenuTarget::Message(id.clone()),
        MenuAction::AddReaction,
    );
    assert!(app.emoji_open, "the picker opens");
    app.on_hit(
        Hit::EmojiChoice("🔥".into()),
        &mut ui::widget::Edits::lend(&mut doc),
    );
    let reacted = app.message(&id).expect("the message");
    assert!(
        reacted.reactions.iter().any(|r| r.emoji == "🔥"),
        "the first reaction lands: {:?}",
        reacted.reactions
    );

    activate_context_menu_action(
        &mut app,
        &mut doc,
        MenuTarget::Message(id.clone()),
        MenuAction::ReplyInThread,
    );
    assert_eq!(app.thread_open.as_deref(), Some(id.as_str()));
    assert_eq!(
        app.focus,
        Focus::Thread,
        "the reply field takes the keyboard"
    );
    activate_context_menu_action(
        &mut app,
        &mut doc,
        MenuTarget::Message(id.clone()),
        MenuAction::ReplyInThread,
    );
    assert_eq!(
        app.thread_open.as_deref(),
        Some(id.as_str()),
        "replying again keeps the thread open"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one labelled phase per original scenario, each with its own fresh fixture"
)]
fn flaps_and_detail_views_open_reflow_and_unwind() {
    // Phase: members_pill_reveals_the_roster_not_a_blank_column.
    {
        let (mut app, mut doc) = app_with_doc();
        app.thread_open = None; // no thread → the flap's other half is members
        app.on_hit(Hit::ToggleRightFlap, &mut ui::widget::Edits::lend(&mut doc));
        render_frame(&mut app, 1280.0, 720.0);
        assert!(
            app.right_flap_open(),
            "the members pill opened the right flap"
        );
        // The members panel painted its own close control (the pill that
        // opened it is the window bar's People slot, shown on), and no
        // thread panel did — i.e. the previously-blank column now has content.
        assert_eq!(
            count_hits(&app, |h| matches!(h, Hit::ToggleRightFlap)),
            1,
            "the members panel's close is painted (roster shown)"
        );
        assert!(
            ui::window_toolbar::declared().is_some_and(|toolbar| toolbar
                .actions
                .iter()
                .any(|action| action.id == "people" && action.on)),
            "the bar's People slot shows the roster open"
        );
        assert_eq!(
            count_hits(&app, |h| matches!(h, Hit::CloseThread)),
            0,
            "no thread panel is shown when no thread is open"
        );
    }

    // Phase: narrow_thread_is_a_full_detail_view_with_back_navigation.
    {
        let (mut app, _doc) = app_with_doc();
        app.thread_open = Some("m_concept".into());
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, 390.0, 844.0);
        app.reconcile_flaps();
        app.right_flap.snap(true);

        assert_eq!(
            app.thread_rect(),
            app.rect,
            "a phone-width thread fills the content view rather than floating at desktop width"
        );
        render_frame(&mut app, 390.0, 844.0);
        assert_eq!(
            count_hits(&app, |h| matches!(h, Hit::CloseThread)),
            1,
            "the detail header supplies one Back control"
        );
    }

    // Phase: open_thread_affordance_opens_thread_and_right_flap.
    {
        let (mut app, mut doc) = app_with_doc();
        app.thread_open = None;
        app.right_flap.close();
        app.on_hit(
            Hit::OpenThread("m_concept".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(app.thread_open.as_deref(), Some("m_concept"));
        assert_eq!(app.focus, Focus::Thread);
        assert!(
            app.right_flap_open(),
            "opening a thread reveals the right flap"
        );
        // Re-hitting the same affordance toggles the thread closed.
        app.on_hit(
            Hit::OpenThread("m_concept".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(
            app.thread_open.is_none(),
            "re-hitting the affordance closes the thread"
        );
        assert_eq!(app.focus, Focus::Main);
    }

    // Phase: escape_unwinds_the_layer_stack.
    {
        let (mut app, mut doc) = app_with_doc();
        app.emoji_open = true;
        app.thread_open = Some("m_concept".into());
        let mods = Modifiers::default();
        // Top layer first: the emoji picker.
        assert!(app.on_key(
            &Key::Named(NamedKey::Escape),
            mods,
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        assert!(!app.emoji_open, "Escape closes the emoji picker first");
        assert!(
            app.thread_open.is_some(),
            "the thread underneath stays open"
        );
        // Next Escape peels the thread.
        assert!(app.on_key(
            &Key::Named(NamedKey::Escape),
            mods,
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        assert!(
            app.thread_open.is_none(),
            "a second Escape closes the thread"
        );
        app.pins_open = true;
        app.right_flap.open();
        app.right_flap.snap(true);
        assert!(app.on_key(
            &Key::Named(NamedKey::Escape),
            mods,
            &mut ui::widget::Edits::lend(&mut doc)
        ));
        assert!(
            !app.pins_open && !app.right_flap_open(),
            "Escape closes the pins"
        );
    }
}

fn write_real_png(path: &std::path::Path, w: u32, h: u32) {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([80, 160, 220, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    std::fs::write(path, bytes.into_inner()).unwrap();
}

fn settle_attachment_until(
    app: &mut ChatApp,
    doc: &mut crate::testing::Room,
    channel: &str,
    expected_count: usize,
) {
    tilo_test_env::wait_until("the attachment worker", || {
        app.settle_attachment_import();
        app.land(doc);
        crate::testing::messages(doc, channel).len() == expected_count
    });
}

/// Where this device has no workspace (a browser page), Share opens the
/// platform's chooser at once; what it, a paste or a drop hands over is
/// shared from its bytes, one file at a time.
#[test]
fn files_from_this_device_are_shared_from_their_bytes() {
    let (mut app, mut doc) = app_with_doc();
    app.active_channel = "c_crit".into();
    app.external_services = false;
    app.on_hit(Hit::Attach, &mut ui::widget::Edits::lend(&mut doc));
    assert!(app.device_pick.is_some(), "the platform's chooser is asked");
    assert!(!app.attach_open, "no workspace listing where there is none");
    app.device_pick = None;

    let before = crate::testing::messages(&doc, "c_crit").len();
    app.device_files
        .push_back(("first.txt".into(), b"one".to_vec()));
    app.device_files
        .push_back(("second.txt".into(), b"two".to_vec()));
    tilo_test_env::wait_until("both chosen files", || {
        app.settle_device_files();
        app.settle_attachment_import();
        app.land(&mut doc);
        crate::testing::messages(&doc, "c_crit").len() == before + 2
    });
    let names: Vec<String> = crate::testing::messages(&doc, "c_crit")
        .iter()
        .rev()
        .take(2)
        .filter_map(|message| message.attachment.as_ref().map(|a| a.name().to_owned()))
        .collect();
    assert_eq!(names, ["second.txt", "first.txt"]);

    // A file dropped on the page comes with its bytes, not a path.
    let dropped = ui::portal::PickedContents {
        name: "dropped.txt".into(),
        media_type: Some("text/plain".into()),
        bytes: b"three".to_vec(),
        location: None,
    };
    ui::portal::dropping(ui::portal::Dropped::Contents(dropped), || {
        feed_mods(
            &mut app,
            &mut doc,
            &WindowEvent::DroppedFile("dropped.txt".into()),
            Modifiers::default(),
        );
    });
    tilo_test_env::wait_until("the dropped file", || {
        app.settle_device_files();
        app.settle_attachment_import();
        app.land(&mut doc);
        crate::testing::messages(&doc, "c_crit").len() == before + 3
    });
    let last = crate::testing::messages(&doc, "c_crit");
    assert_eq!(
        last.last()
            .and_then(|message| message.attachment.as_ref())
            .map(|attachment| attachment.name().to_owned())
            .as_deref(),
        Some("dropped.txt")
    );

    // One too large for the page to read is refused by its size alone.
    let unread = ui::portal::Dropped::NotRead {
        name: "film.mov".into(),
        size: 5 << 30,
        why: ui::portal::NotRead::TooLarge,
    };
    ui::portal::dropping(unread, || {
        feed_mods(
            &mut app,
            &mut doc,
            &WindowEvent::DroppedFile("film.mov".into()),
            Modifiers::default(),
        );
    });
    assert_eq!(
        app.attachment_status.as_deref(),
        Some("Not shared: film.mov: files up to 32 MiB can be shared")
    );

    app.device_files.push_back(("empty.txt".into(), Vec::new()));
    tilo_test_env::wait_until("the refused file", || {
        app.settle_device_files();
        app.settle_attachment_import();
        app.attachment_pending.is_none() && app.device_files.is_empty()
    });
    assert_eq!(
        app.attachment_status.as_deref(),
        Some("Not shared: the file is empty"),
        "a failure says what happened, never that a workspace is unavailable"
    );
}

/// A file the room's host would not take is not posted: the person
/// reads why (too big, or past what the room takes of their files in
/// an hour), never a post whose file reaches nobody.
#[test]
fn a_file_the_room_would_not_take_is_refused_in_plain_words() {
    use ui::tilo_resource::Fields as _;
    const MIB: usize = 1024 * 1024;
    let (mut app, mut doc) = app_with_doc();
    app.active_channel = "c_crit".into();
    app.external_services = false;
    let before = crate::record::Chat::read_fields(&doc.chat, &automerge::ROOT).unwrap();
    let mut after = before.clone();
    after.files = crate::record::RoomFiles {
        largest: 4 << 20,
        member_hourly: 6 << 20,
    };
    after
        .write_changed_fields(
            &before,
            &mut doc.chat,
            &automerge::ROOT,
            &ui::tilo_resource::Delta::default(),
        )
        .unwrap();
    app.reload(&doc);
    let posted = crate::testing::messages(&doc, "c_crit").len();
    let offer = |app: &mut ChatApp, doc: &mut crate::testing::Room, name: &str, size| {
        app.device_files
            .push_back((name.to_owned(), vec![7_u8; size]));
        tilo_test_env::wait_until(name, || {
            app.settle_device_files();
            app.settle_attachment_import();
            app.land(doc);
            app.attachment_pending.is_none() && app.device_files.is_empty()
        });
    };

    offer(&mut app, &mut doc, "big.bin", 4 * MIB + 1);
    assert_eq!(
        app.attachment_status.as_deref(),
        Some("Not shared: the file is too big; this room takes files up to 4 MiB")
    );
    assert_eq!(crate::testing::messages(&doc, "c_crit").len(), posted);

    offer(&mut app, &mut doc, "first.bin", 4 * MIB);
    assert_eq!(app.attachment_status.as_deref(), Some("Attachment shared"));
    assert_eq!(crate::testing::messages(&doc, "c_crit").len(), posted + 1);

    offer(&mut app, &mut doc, "second.bin", 3 * MIB);
    assert_eq!(
        app.attachment_status.as_deref(),
        Some(
            "Not shared: you've reached this room's limit of 6 MiB of files an hour; try again later"
        )
    );
    assert_eq!(crate::testing::messages(&doc, "c_crit").len(), posted + 1);
}

fn settle_picker_until(app: &mut ChatApp) {
    tilo_test_env::wait_until("the attachment picker worker", || {
        app.settle_attachment_picker();
        app.attachment_picker_pending.is_none()
    });
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one labelled phase per original scenario, each with its own fresh fixture"
)]
fn own_messages_are_edited_in_place_and_commit_only_on_save() {
    // Phase: edit_via_menu_writes_through_edit_message.
    {
        let (mut app, mut doc) = app_with_doc();
        let original = app.message("m_me1").unwrap().text.clone();
        let history_before = app.message("m_me1").unwrap().history.len();
        assert!(!app.message("m_me1").unwrap().edited);

        // The right-click menu resolves to this action; invoke it directly.
        activate_context_menu_action(
            &mut app,
            &mut doc,
            MenuTarget::Message("m_me1".into()),
            MenuAction::EditMessage,
        );
        assert!(
            app.editing.is_some(),
            "Edit loads the message into the inline editor"
        );
        assert_eq!(
            app.edit_input.value(),
            original,
            "editor seeded with the current body"
        );

        app.edit_input.select_all();
        type_str(
            &mut app,
            &mut doc,
            "Revised: host tile carries the follow control.",
        );
        feed(&mut app, &mut doc, &named_key(NamedKey::Enter));

        assert!(app.editing.is_none(), "saving closes the editor");
        let m = app.message("m_me1").unwrap();
        assert_eq!(m.text, "Revised: host tile carries the follow control.");
        assert!(m.edited, "the message is flagged edited");
        assert_eq!(
            m.history.len(),
            history_before + 1,
            "history grew by one revision"
        );
        assert_eq!(
            m.history.last().unwrap().text,
            original,
            "history holds the original body"
        );
    }

    // Phase: message_edit_selection_and_ime_commit_only_on_save.
    {
        let (mut app, mut doc) = app_with_doc();
        let original = app.message("m_me1").unwrap().text.clone();
        app.begin_edit("m_me1".into());

        feed_mods(&mut app, &mut doc, &char_key("a"), mods(true, false));
        let (anchor, caret) = app.edit_input.selection();
        assert_eq!((anchor.min(caret), anchor.max(caret)), (0, original.len()));
        feed(
            &mut app,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Preedit("東".into(), Some((0, 3)))),
        );
        assert_eq!(
            app.edit_input.value(),
            original,
            "preedit does not alter the committed input value"
        );
        feed(
            &mut app,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
        );
        assert_eq!(app.edit_input.value(), "東京");
        assert_eq!(
            crate::testing::messages(&doc, "c_crit")
                .into_iter()
                .find(|message| message.id == "m_me1")
                .unwrap()
                .text,
            original,
            "partial edits stay local until Save"
        );

        feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
        assert_eq!(app.message("m_me1").unwrap().text, "東京");
    }

    // Phase: clicking_message_editor_places_the_shared_input_caret.
    {
        let (mut app, mut doc) = app_with_doc();
        app.begin_edit("m_me1".into());
        render_frame(&mut app, 1280.0, 720.0);
        let editor = app
            .hits
            .iter()
            .find_map(|(rect, hit)| matches!(hit, Hit::FocusEdit).then_some(*rect))
            .expect("the open editor records its input hit region");

        let x = editor.x0 + 18.0;
        let y = editor.y0 + 14.0;
        feed(&mut app, &mut doc, &cursor_moved(x, y));
        feed(
            &mut app,
            &mut doc,
            &mouse(ElementState::Pressed, MouseButton::Left),
        );
        feed(
            &mut app,
            &mut doc,
            &mouse(ElementState::Released, MouseButton::Left),
        );
        let cursor = app.edit_input.caret();
        assert!(
            cursor < app.edit_input.value().len(),
            "clicking near the left edge moves the caret away from the end"
        );

        type_str(&mut app, &mut doc, "Z");
        assert_eq!(app.edit_input.caret(), cursor + 1);
        assert!(!app.edit_input.value().ends_with('Z'));
    }

    // Phase: arrow_up_in_empty_composer_edits_last_own_message.
    {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        app.set_composer_focus();
        assert!(
            app.composer_main.value().is_empty(),
            "composer starts empty"
        );
        feed(&mut app, &mut doc, &named_key(NamedKey::ArrowUp));
        let id = app.editing.clone().expect("ArrowUp opened the editor");
        assert_eq!(id, "m_me2", "loaded the last own top-level message");
        assert_eq!(
            app.edit_input.value(),
            app.message("m_me2").unwrap().text,
            "seeded with its current body"
        );
    }

    // Phase: arrow_up_with_a_draft_does_not_start_editing.
    {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        app.set_composer_focus();
        type_str(&mut app, &mut doc, "half a thought");
        feed(&mut app, &mut doc, &named_key(NamedKey::ArrowUp));
        assert!(
            app.editing.is_none(),
            "a non-empty composer suppresses ArrowUp-to-edit"
        );
    }

    // Phase: edit_escape_discards_without_writing.
    {
        let (mut app, mut doc) = app_with_doc();
        let original = app.message("m_me1").unwrap().text.clone();
        app.begin_edit("m_me1".into());
        type_str(&mut app, &mut doc, "Z");
        feed(&mut app, &mut doc, &named_key(NamedKey::Escape));
        assert!(app.editing.is_none(), "Escape closes the editor");
        let m = app.message("m_me1").unwrap();
        assert_eq!(m.text, original, "cancel leaves the message untouched");
        assert!(!m.edited, "cancel does not flag the message edited");
    }

    // Phase: cannot_edit_another_authors_message.
    {
        let (mut app, _doc) = app_with_doc();
        assert_eq!(app.message("m_open").unwrap().author, "u_aria");
        app.begin_edit("m_open".into());
        assert!(
            app.editing.is_none(),
            "a foreign message never enters the editor"
        );
    }

    // Phase: edit_history_popover_opens_and_dismisses.
    {
        let (mut app, mut doc) = app_with_doc();
        assert!(
            app.message("m_sora").unwrap().edited,
            "m_sora is an edited message"
        );
        app.on_hit(
            Hit::EditHistory {
                msg: "m_sora".into(),
                anchor: Point::ZERO,
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(
            app.history_open.as_deref(),
            Some("m_sora"),
            "the history popover opened"
        );
        app.on_hit(Hit::Dismiss, &mut ui::widget::Edits::lend(&mut doc));
        assert!(app.history_open.is_none(), "the dismiss scrim closes it");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the floating-surface regression deliberately follows the complete app-to-shell presentation lifecycle"
)]
fn call_tiles_and_controls_stay_honest_and_live_when_detached() {
    // Phase: host_call_tile_has_no_fabricated_video_frame.
    {
        // The host video surface must be empty until a real capture feeds it —
        // never a synthesized gradient "face".
        let app = ChatApp::new_ephemeral();
        assert!(
            app.video.latest_frame().is_none(),
            "no frame is fabricated at construction"
        );
    }

    // Phase: detached_call_keeps_live_controls_in_the_shell_presentation_pass.
    {
        use ui::Widget;
        use ui::floating_surface::Attachment;
        use ui::presence::PresenceCtx;
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;
        use vello::Scene;
        use vello::kurbo::{Affine, Rect};

        let (mut app, mut doc) = app_with_doc();
        app.call.join("c_huddle");
        render_frame(&mut app, 1200.0, 800.0);
        ui::widget::FloatingSurfaceWidgetExt::set_floating_surface_bounds(
            &mut app,
            Rect::new(0.0, 0.0, 1600.0, 900.0),
        );

        let handle = app
            .hits
            .iter()
            .find_map(|(rect, hit)| matches!(hit, Hit::CallDragHandle).then_some(*rect))
            .expect("the attached live call exposes its drag handle");
        let pointer_origin = handle.center();
        let surface_origin = app.call_video_rect();
        feed(
            &mut app,
            &mut doc,
            &cursor_moved(pointer_origin.x, pointer_origin.y),
        );
        feed(
            &mut app,
            &mut doc,
            &mouse(ElementState::Pressed, MouseButton::Left),
        );
        assert!(Widget::wants_pointer_capture(&app));

        let outside = Point::new(1300.0, pointer_origin.y + 40.0);
        feed(&mut app, &mut doc, &cursor_moved(outside.x, outside.y));
        assert!(matches!(
            app.call.placement.attachment(),
            Attachment::Desktop { .. }
        ));
        assert_eq!(
            app.call_video_rect(),
            surface_origin + (outside - pointer_origin),
            "the transfer frame preserves the live surface geometry"
        );
        assert!(
            ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut app).is_some(),
            "the compositor receives the one-shot adoption request"
        );

        // Preparing a native window is not yet proof of pixels. Keep the
        // clipped app copy alive until the compositor acknowledges the first
        // committed floating buffer, so there is no transparent transfer
        // frame even when GPU/window setup takes a beat.
        render_frame(&mut app, 1200.0, 800.0);
        assert_eq!(
            count_hits(&app, |hit| matches!(hit, Hit::CallDragHandle)),
            1
        );
        let prepared_content = app.call_video_rect();
        let prepared_window = app.huddle_panel().inflate(8.0, 8.0);
        ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
            &mut app,
            &ui::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                id: "chat-call:c_huddle".into(),
                token: "0123456789abcdef0123456789abcdef".into(),
                rect: ui::floating_surface::protocol_rect_array(prepared_window),
                content_rect: ui::floating_surface::protocol_rect_array(prepared_content),
                transfer_covered: true,
                coordinate_offset: [0.0, 0.0],
            },
        );
        assert!(!app.call.placement.is_desktop_adopted());
        let mut scene = Scene::new();
        let mut text_renderer = TextRenderer::new();
        ui::widget::FloatingSurfaceWidgetExt::draw_floating_surfaces(
            &mut app,
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        assert!(!scene.encoding().is_empty());

        ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
            &mut app,
            &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                id: "chat-call:c_huddle".into(),
                rect: ui::floating_surface::protocol_rect_array(prepared_content),
            },
        );
        assert!(app.call.placement.is_desktop_adopted());
        render_frame(&mut app, 1200.0, 800.0);
        assert_eq!(
            count_hits(&app, |hit| matches!(hit, Hit::CallDragHandle)),
            0
        );
        let mut scene = Scene::new();
        ui::widget::FloatingSurfaceWidgetExt::draw_floating_surfaces(
            &mut app,
            &mut scene,
            Affine::IDENTITY,
            &mut text_renderer,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        assert_eq!(
            count_hits(&app, |hit| matches!(hit, Hit::CallDragHandle)),
            1
        );

        // Drag back well inside Chat. The runtime sees `returning_to_app` in
        // the presentation and asks the shell to suspend its native surface;
        // until that acknowledgement arrives only the shell pass may paint.
        let returned_pointer = Point::new(980.0, pointer_origin.y + 84.0);
        feed(
            &mut app,
            &mut doc,
            &cursor_moved(returned_pointer.x, returned_pointer.y),
        );
        assert!(app.call.placement.is_return_pending());
        let final_shell_rect = app.call_video_rect();
        let presentations =
            ui::widget::FloatingSurfaceWidgetExt::floating_surface_presentations(&app);
        assert_eq!(presentations.len(), 1);
        assert!(presentations[0].returning_to_app);

        ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
            &mut app,
            &ui::floating_surface::FloatingSurfaceHostMessage::Suspended {
                id: "chat-call:c_huddle".into(),
            },
        );
        assert!(matches!(
            app.call.placement.attachment(),
            Attachment::App { .. }
        ));
        assert_eq!(
            app.call_video_rect(),
            final_shell_rect,
            "the first app-owned call frame must exactly cover the final shell frame"
        );
        assert!(
            Widget::wants_pointer_capture(&app),
            "the held drag survives shell suspension"
        );

        let continued_pointer = returned_pointer + vello::kurbo::Vec2::new(-18.0, 11.0);
        feed(
            &mut app,
            &mut doc,
            &cursor_moved(continued_pointer.x, continued_pointer.y),
        );
        assert_eq!(
            app.call_video_rect(),
            final_shell_rect + (continued_pointer - returned_pointer),
            "the first primary-window motion continues the same drag"
        );

        // Release is still delivered after both ownership boundaries.
        feed(
            &mut app,
            &mut doc,
            &mouse(ElementState::Released, MouseButton::Left),
        );
        assert!(!Widget::wants_pointer_capture(&app));
    }

    // Phase: call_controls_track_hover_and_clear_it_on_departure.
    {
        let (mut app, mut doc) = app_with_doc();
        app.call.join("c_huddle");
        render_frame(&mut app, 1200.0, 800.0);
        let camera = app
            .hits
            .iter()
            .find_map(|(rect, hit)| matches!(hit, Hit::CallCamera).then_some(*rect))
            .expect("the live huddle exposes its camera");

        feed(
            &mut app,
            &mut doc,
            &cursor_moved(camera.center().x, camera.center().y),
        );
        assert!(matches!(app.call_hovered, Some(Hit::CallCamera)));

        feed(&mut app, &mut doc, &cursor_moved(2.0, 2.0));
        assert!(app.call_hovered.is_none());
    }
}

// --- draw + input harness -------------------------------------------
//
// The chat surface computes its regions from `self.rect` and records every
// clickable area into `self.hits` during `draw`, so a single draw pass at a
// known size fully populates the hit list a real click would resolve
// against — no GPU or window needed (identical to the offscreen `--shots`
// path). These helpers drive that and the raw winit input path.

fn render_frame(app: &mut ChatApp, w: f64, h: f64) {
    use ui::Widget;
    use ui::presence::PresenceCtx;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::Scene;
    use vello::kurbo::Affine;
    app.rect = vello::kurbo::Rect::new(0.0, 0.0, w, h);
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
}

fn stage_agent_work(app: &mut ChatApp, doc: &mut crate::testing::Room) {
    for (author, invoke) in crate::resource::agent_work_steps() {
        crate::testing::apply_as(doc, author, invoke).unwrap();
    }
    let mut head = crate::parts::ChatPart::clone(&doc.messages().parts()[0].1);
    let (summons, message) = crate::resource::agent_summons();
    head.messages.push(summons, message);
    doc.write_part(0, &head).unwrap();
    for (author, invoke) in crate::resource::agent_work_part_steps() {
        crate::testing::apply_part_as(doc, author, invoke).unwrap();
    }
    app.land(doc);
    app.stage_scenario("agent-work");
}

/// Chat paints its regions by hand, and reports as damage the regions whose
/// drawing changed: a typed character repaints the composer, not the
/// window; a resize repaints all of it.
#[test]
fn a_typed_character_damages_the_composer_and_a_resize_the_window() {
    use vello::kurbo::Rect;
    let (mut app, mut doc) = app_with_doc();
    app.focus = Focus::Main;
    app.set_composer_focus();
    type_str(&mut app, &mut doc, "hello");
    let (w, h) = (1200.0, 800.0);
    // One text renderer across frames, as a host keeps: its fonts are the
    // same fonts from frame to frame.
    let mut tr = ui::text_render::TextRenderer::new();
    let theme = ui::theme::Theme::dark();
    let presence = ui::presence::PresenceCtx::empty();
    let mut frame = |app: &mut ChatApp, w: f64, h: f64| {
        app.rect = Rect::new(0.0, 0.0, w, h);
        let mut scene = vello::Scene::new();
        app.draw(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut tr,
            &theme,
            &presence,
        );
        let mut damage = Vec::new();
        app.collect_damage(&mut damage);
        app.mark_drawn();
        damage
    };
    // The room's avatars and attachments decode off the frame and repaint
    // the timeline when they land: the frames measured come after every
    // picture asked for has been drawn.
    for _ in 0..3 {
        frame(&mut app, w, h);
        app.images
            .block_until_idle(std::time::Duration::from_secs(30));
    }
    assert!(app.images.is_idle(), "the room's pictures settled");
    let still = frame(&mut app, w, h);
    assert!(
        still.is_empty(),
        "an unchanged frame damages nothing: {still:?}"
    );

    type_str(&mut app, &mut doc, "!");
    let damage = frame(&mut app, w, h);
    let composer = Rect::new(
        app.center_left(),
        app.messages_rect().y1,
        app.center_right(),
        h,
    );
    assert!(!damage.is_empty(), "the typed character is repainted");
    for rect in &damage {
        assert_eq!(
            composer.union(*rect),
            composer,
            "{rect:?} is outside the composer {composer:?}"
        );
    }

    let window = Rect::new(0.0, 0.0, w + 100.0, h);
    let damage = frame(&mut app, w + 100.0, h);
    assert!(
        damage.iter().any(|rect| rect.union(window) == *rect),
        "a resize repaints the window: {damage:?}"
    );
}

#[test]
fn inspector_accessibility_focus_retires_the_previous_keyboard_owner() {
    use ui::Widget;
    use ui::a11y::Action;
    use ui::headless::FrameHarness;
    let (app, doc) = app_with_doc();
    let mut state = ui::State::new();
    assert!(state.adopt_doc(doc.chat));
    let mut driver = FrameHarness::for_root(state, app, (1800, 900), ui::Theme::dark());
    // People is the window bar's slot; the desktop hands its press to the app.
    assert!(
        ui::window_toolbar::declared()
            .is_some_and(|toolbar| toolbar.actions.iter().any(|action| action.id == "people")),
        "the window's bar offers People"
    );
    ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action {
        id: "people".into(),
    });
    assert!(driver.wait_until(tilo_test_env::hang_bound(), |driver| {
        driver
            .root::<ChatApp>()
            .right_flap
            .resize_accessibility()
            .is_some()
    }));
    // One of the app's own controls takes the keyboard first.
    let attach = driver
        .a11y_node_named("Share a file")
        .expect("the composer's Share a file control");
    driver.perform_a11y_action(attach, &Action::Focus).unwrap();
    assert!(driver.root::<ChatApp>().keyboard_target.is_some());
    let inspector = driver.a11y_node_named("Resize inspector").unwrap();
    driver
        .perform_a11y_action(inspector, &Action::Focus)
        .unwrap();
    assert!(driver.root::<ChatApp>().keyboard_target.is_none());
    let root = driver.root::<ChatApp>();
    assert!(
        root.right_flap
            .resize_accessibility()
            .unwrap()
            .state
            .focused
    );

    let children = root.accessibility().unwrap().children;
    let attach = children
        .iter()
        .position(|node| node.name.as_deref() == Some("Share a file"))
        .unwrap();
    let right = children
        .iter()
        .position(|node| node.action_target.as_deref() == Some("inspector-resize"))
        .unwrap();
    let root = driver.root_mut::<ChatApp>();
    assert!(root.focus_synthetic_accessibility(&[attach]));
    assert!(root.focus_synthetic_accessibility(&[right]));
    assert!(root.keyboard_target.is_none());
    assert!(
        root.right_flap
            .resize_accessibility()
            .unwrap()
            .state
            .focused
    );
}

/// When no window holds the keyboard, a desktop offers what is typed to
/// each window in turn, so a Chat that does not have focus must leave it.
#[test]
fn chat_without_the_keyboard_leaves_typed_text_to_the_focused_window() {
    use ui::headless::FrameHarness;
    let (app, doc) = app_with_doc();
    let mut state = ui::State::new();
    assert!(state.adopt_doc(doc.chat));
    let mut driver = FrameHarness::for_root(state, app, (960, 720), ui::Theme::dark());
    let commit = |text: &str| WindowEvent::Ime(winit::event::Ime::Commit(text.into()));

    driver.root_mut::<ChatApp>().set_focus(false);
    driver.send_host_event(&commit("lost"));
    driver.send_host_event(&char_key("x"));
    assert_eq!(
        driver.root::<ChatApp>().composer_main.value(),
        "",
        "an unfocused Chat takes no keys and no composed text"
    );

    driver.root_mut::<ChatApp>().set_focus(true);
    driver.send_host_event(&commit("hi"));
    assert_eq!(driver.root::<ChatApp>().composer_main.value(), "hi");
}

/// The shell keeps the channels docked while the conversation beside them
/// keeps a reading column; at that narrowest, every composer control still
/// fits.
#[test]
fn the_composer_fits_the_narrowest_conversation_beside_the_channels() {
    let (mut app, _doc) = app_with_doc();
    let narrowest =
        f64::from(ui::widget::ContentKind::Reading.min_width(&ui::tokens::Metrics::BASE));
    render_frame(&mut app, narrowest, 720.0);
    let content = app.content_rect();
    for control in [Hit::FocusMain, Hit::Send, Hit::Attach] {
        let rect = app
            .hits
            .iter()
            .find_map(|(rect, hit)| (hit.semantic_id() == control.semantic_id()).then_some(*rect))
            .expect("the actual conversation paints every composer control");
        assert!(
            content.contains_rect(rect),
            "composer controls fit beside navigation"
        );
    }
}

#[test]
fn the_attached_call_window_rests_below_the_header_row() {
    for (w, h) in [(1280.0, 800.0), (800.0, 600.0)] {
        let (mut app, _doc) = app_with_doc();
        app.thread_open = Some("m_concept".into());
        app.right_flap.open();
        app.call.join("c_huddle");
        render_frame(&mut app, w, h);
        render_frame(&mut app, w, h);
        let panel = app.huddle_panel();
        let header = app.rect.y0 + app.chrome.header_h;
        assert!(
            panel.y0 >= header,
            "{w}x{h}: the call window ({:.1}) covers the header row ({header:.1})",
            panel.y0
        );
        assert!(
            !app.hits.iter().any(|(rect, hit)| {
                matches!(hit, Hit::Send | Hit::FocusMain) && rect.overlaps(panel)
            }),
            "{w}x{h}: the call window covers the composer"
        );
    }
}

#[test]
fn agent_work_thread_survives_pointer_navigation_and_projects_updates() {
    use crate::model::{AgentStepOutcome, AgentWork, AgentWorkState};
    use ui::Widget;

    for (width, height) in [(1600.0, 900.0), (800.0, 600.0)] {
        let (mut app, mut doc) = app_with_doc();
        stage_agent_work(&mut app, &mut doc);
        app.land(&mut doc);
        app.sync(&doc.chat, &automerge::ROOT);
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, width, height);
        app.reconcile_flaps();
        app.right_flap.snap(true);
        render_frame(&mut app, width, height);

        let description = |app: &ChatApp| {
            app.accessibility()
                .unwrap()
                .children
                .into_iter()
                .find(|node| node.name.as_deref() == Some("Messages"))
                .unwrap()
                .children
                .into_iter()
                .find(|node| node.name.as_deref() == Some("Tide (agent): "))
                .expect("the open thread exposes its agent reply")
                .description
                .unwrap()
        };
        assert!(description(&app).contains("Latest intent: Add function"));
        assert!(description(&app).contains("Requested: Add function"));

        let close = app
            .hits
            .iter()
            .find(|(_, hit)| matches!(hit, Hit::CloseThread))
            .expect("painted thread has a close/back control")
            .0
            .center();
        click_point(&mut app, &mut doc, close);
        assert!(app.thread_open.is_none());

        // Render the settled closed flap, not the outgoing overlay frame
        // that deliberately continues intercepting input during its exit.
        app.right_flap.snap(false);

        render_frame(&mut app, width, height);
        let open = app
            .hits
            .iter()
            .find(|(_, hit)| matches!(hit, Hit::OpenThread(id) if id == "m_agent_request"))
            .expect("the original request retains its reply affordance")
            .0
            .center();
        click_point(&mut app, &mut doc, open);
        assert_eq!(app.thread_open.as_deref(), Some("m_agent_request"));

        let mut applied_step = app
            .messages
            .iter()
            .find(|message| message.id == crate::testing::agent_reply())
            .unwrap()
            .work
            .as_ref()
            .unwrap()
            .trace[0]
            .clone();
        applied_step.outcome = AgentStepOutcome::Applied;
        crate::testing::apply_part_as(
            &mut doc,
            crate::resource::DEMO_AGENT,
            crate::testing::record_work(
                crate::resource::DEMO_SUMMONS,
                &AgentWork::new(
                    AgentWorkState::Done {
                        did: "Revenue curve added".into(),
                    },
                    crate::demo::NOW,
                )
                .with_trace(vec![applied_step]),
            ),
        )
        .unwrap();
        app.land(&mut doc);
        app.sync(&doc.chat, &automerge::ROOT);
        render_frame(&mut app, width, height);
        assert_eq!(
            description(&app),
            "Completed: Revenue curve added; Applied: Add function",
            "the state update retains the previously recorded dispatch evidence"
        );
        let reply = app
            .messages
            .iter()
            .find(|message| message.id == crate::testing::agent_reply())
            .unwrap();
        assert_eq!(reply.parent.as_deref(), Some("m_agent_request"));
        assert!(
            reply.text.is_empty(),
            "work updates do not overwrite authored text"
        );
    }
}

fn count_hits(app: &ChatApp, pred: impl Fn(&Hit) -> bool) -> usize {
    app.hits.iter().filter(|(_, h)| pred(h)).count()
}

fn click_point(app: &mut ChatApp, doc: &mut AutoCommit, point: Point) {
    feed(app, doc, &cursor_moved(point.x, point.y));
    feed(app, doc, &mouse(ElementState::Pressed, MouseButton::Left));
    feed(app, doc, &mouse(ElementState::Released, MouseButton::Left));
}

#[test]
fn agent_work_compact_thread_scrolls_all_evidence_without_moving_timeline() {
    use crate::model::{AgentStep, AgentStepOutcome, AgentWork, AgentWorkState};
    use ui::Widget;
    let (mut app, mut doc) = app_with_doc();
    stage_agent_work(&mut app, &mut doc);
    app.sync(&doc.chat, &automerge::ROOT);
    let work = AgentWork::new(AgentWorkState::Done { did: "Reviewed all operations".into() }, crate::demo::NOW)
        .with_trace((0..60).map(|index| AgentStep::authored("calculator.workspace",
            format!("Operation {index}: preserve graph bounds and add the requested revenue curve"),
            AgentStepOutcome::Applied, index)).collect());
    crate::testing::apply_part_as(
        &mut doc,
        crate::resource::DEMO_AGENT,
        crate::testing::record_work(crate::resource::DEMO_SUMMONS, &work),
    )
    .unwrap();
    app.sync(&doc.chat, &automerge::ROOT);
    render_frame(&mut app, 800.0, 600.0);
    let region = app.primary_vertical_scroll().unwrap();
    assert_eq!(region.key, "thread");
    assert!(
        region.metrics.max_scroll() > 600.0,
        "production wrapping creates real overflow"
    );
    let timeline_offset = app.scroll;
    assert!(matches!(
        app.apply_primary_vertical_scroll(ui::widget::PrimaryScrollAction::End),
        ui::widget::PrimaryScrollResult::Changed(_)
    ));
    render_frame(&mut app, 800.0, 600.0);
    let region = app.primary_vertical_scroll().unwrap();
    assert!((region.metrics.offset - region.metrics.max_scroll()).abs() < 0.01);
    assert!((app.scroll - timeline_offset).abs() < 0.01);
    let tree = app.accessibility().unwrap();
    let messages = tree
        .children
        .iter()
        .find(|node| node.name.as_deref() == Some("Messages"))
        .unwrap();
    assert_eq!(
        messages.children.len(),
        2,
        "pushed detail exposes only parent and reply"
    );
    let description = messages
        .children
        .iter()
        .find_map(|node| node.description.as_ref())
        .unwrap();
    assert!(
        description.contains("Operation 59:"),
        "the final recorded dispatch is accessible"
    );
    let point = app.thread_body.center();
    feed(&mut app, &mut doc, &cursor_moved(point.x, point.y));
    feed(&mut app, &mut doc, &wheel(2.0));
    assert!(
        f64::from(app.thread_scroll.offset_y()) < region.metrics.max_scroll(),
        "pointer wheel reaches earlier evidence"
    );
    assert!((app.scroll - timeline_offset).abs() < 0.01);
}

use ui::synthetic_input::cursor_moved;
fn mouse(state: ElementState, button: MouseButton) -> WindowEvent {
    ui::synthetic_input::mouse_input(state, button)
}
fn wheel(y: f64) -> WindowEvent {
    ui::synthetic_input::mouse_wheel(0.0, y as f32)
}
fn feed(app: &mut ChatApp, doc: &mut AutoCommit, ev: &WindowEvent) {
    let mods = Modifiers::default();
    let mut handled = false;
    app.dispatch_event(
        ev,
        1.0,
        mods,
        &mut ui::widget::Edits::lend(doc),
        &mut handled,
    );
}

use ui::synthetic_input::named_key;

use ui::synthetic_input::char_key;

/// Type each character of `s` into the app through the real keyboard path
/// (space via the named key, like winit reports it).
fn type_str(app: &mut ChatApp, doc: &mut AutoCommit, s: &str) {
    for ch in s.chars() {
        let ev = if ch == ' ' {
            named_key(NamedKey::Space)
        } else {
            char_key(&ch.to_string())
        };
        feed(app, doc, &ev);
    }
}

fn mods(ctrl: bool, shift: bool) -> Modifiers {
    use winit::keyboard::ModifiersState;
    let mut s = ModifiersState::empty();
    if ctrl {
        s |= ModifiersState::CONTROL;
    }
    if shift {
        s |= ModifiersState::SHIFT;
    }
    Modifiers::from(s)
}

fn feed_mods(app: &mut ChatApp, doc: &mut AutoCommit, ev: &WindowEvent, m: Modifiers) {
    let mut handled = false;
    app.dispatch_event(ev, 1.0, m, &mut ui::widget::Edits::lend(doc), &mut handled);
}

/// Pins opened from the window bar's button close on Escape: the bar's
/// press leaves the keyboard with the app, and Escape there peels the panel.
#[test]
fn escape_closes_the_pins_opened_from_the_window_bar() {
    use ui::headless::FrameHarness;
    let (app, doc) = app_with_doc();
    let mut state = ui::State::new();
    assert!(state.adopt_doc(doc.chat));
    let mut driver = FrameHarness::for_root(state, app, (1200, 800), ui::Theme::dark());
    driver.press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: "pins".into() });
    assert!(
        driver.root::<ChatApp>().pins_open,
        "the bar's button opens the pins"
    );
    driver.key("Escape");
    let app = driver.root::<ChatApp>();
    assert!(
        !app.pins_open && !app.right_flap_open(),
        "Escape closes the pins"
    );
}

/// On a window too narrow to dock it, the pins float over the conversation
/// and end above the composer: its emoji and Send stay uncovered and take
/// their presses, so a message can still be written and sent beside them.
#[test]
fn floating_pins_leave_the_composer_whole() {
    use ui::headless::FrameHarness;
    let (app, doc) = app_with_doc();
    let mut state = ui::State::new();
    assert!(state.adopt_doc(doc.chat));
    let mut driver = FrameHarness::for_root(state, app, (640, 700), ui::Theme::dark());
    driver.press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: "pins".into() });
    assert!(driver.wait_until(tilo_test_env::hang_bound(), |driver| {
        !driver.root::<ChatApp>().right_flap.is_animating()
    }));
    let app = driver.root::<ChatApp>();
    assert!(
        app.pins_open && app.right_overlay(),
        "the pins float on a narrow window"
    );
    let panel = app.right_flap.sidebar_rect();
    let scrim = app.right_flap.scrim_rect();
    for wanted in ["chat.composer-emoji", "chat.send"] {
        let (rect, hit) = app
            .hits
            .iter()
            .find(|(_, hit)| hit.semantic_id() == wanted)
            .cloned()
            .unwrap_or_else(|| panic!("the composer's {wanted}"));
        assert!(
            rect.intersect(panel).area() <= 0.0 && rect.intersect(scrim).area() <= 0.0,
            "{wanted} at {rect:?} stays clear of the pins {panel:?} and their scrim {scrim:?}"
        );
        assert_eq!(
            app.hit_at(rect.center()).map(|hit| hit.semantic_id()),
            Some(hit.semantic_id()),
            "a press on {wanted} reaches it"
        );
    }
}

mod calls_and_peers;
mod room_state;

/// Each Discord guest in the expanded call is their own tile, named as
/// their bridged messages read, and opens how loud they play here.
#[test]
fn a_bridged_calls_guests_get_their_own_tiles_and_volume() {
    use ui::native_media::{Guest, Roster};
    let (mut app, mut doc) = app_with_doc();
    app.call.join("c_standup");
    app.call.expanded = true;
    app.native_voice.snapshot.rosters = vec![Roster {
        channel_id: "c_standup".into(),
        people: vec!["u_aria".into()],
        guests: vec![Guest {
            id: "discord:1".into(),
            name: "Dana".into(),
            avatar: None,
            service: "discord".into(),
            linked: None,
            muted: false,
        }],
        bridges: vec!["discord".into()],
        ..Default::default()
    }];
    app.refresh_native_streams();
    render_frame(&mut app, 1280.0, 720.0);
    let dana = app
        .semantic_hits()
        .into_iter()
        .find(|(_, hit, ..)| matches!(hit, Hit::CallParticipant(id) if id == "discord:1"))
        .expect("Dana has a tile of her own");
    assert_eq!(dana.2, "Dana (via Discord): volume and mute for you");
    assert!(
        app.semantic_hits()
            .iter()
            .all(|(_, hit, ..)| !matches!(hit, Hit::CallParticipant(id) if app.is_me(id))),
        "this person's own tile has no volume"
    );

    app.on_hit(dana.1, &mut ui::widget::Edits::lend(&mut doc));
    let menu = app.menu.as_ref().expect("her tile opens the volume menu");
    assert_eq!(menu.target, MenuTarget::CallParticipant("discord:1".into()));
    activate_context_menu_action(
        &mut app,
        &mut doc,
        MenuTarget::CallParticipant("discord:1".into()),
        MenuAction::Silence,
    );
    assert!(app.call_listening("discord:1").silenced);
    activate_context_menu_action(
        &mut app,
        &mut doc,
        MenuTarget::CallParticipant("discord:1".into()),
        MenuAction::Volume(50),
    );
    let listening = app.call_listening("discord:1");
    assert_eq!((listening.percent, listening.silenced), (50, false));
}
