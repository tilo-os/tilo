use super::*;

#[test]
fn compose_stands_in_the_window_and_sends_only_with_an_account_and_a_recipient() {
    // No account yet: the composer is the window's page, never a panel
    // floating over the empty state, and it cannot send.
    let mut first = MailApp::new_first_run();
    let mut doc = crate::resource::TestMail::doc();
    first.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    first.sync(&doc, &automerge::ROOT);
    first.layout_for_test(1100.0, 780.0);
    first.shared.borrow_mut().push_intent(Intent::Compose);
    first.process_intents();
    first.layout_for_test(1100.0, 780.0);
    assert!(first.compose.open);
    assert!(
        !first.compose.is_modal(),
        "no floating panel over a scrim, so nothing blurred shows through"
    );
    assert_eq!(
        first.compose.window_rect(),
        first.rect,
        "the page is the window"
    );
    first.compose.type_recipient("tomas@example.org");
    first.sync(&doc, &automerge::ROOT);
    assert!(!first.compose.can_send(), "no account to send from");

    // With an account: a reply stands in the message column, beside the
    // list that holds the message it answers.
    let (mut app, doc) = app();
    app.layout_for_test(1280.0, 800.0);
    app.stage_reply();
    app.sync(&doc, &automerge::ROOT);
    app.layout_for_test(1280.0, 800.0);
    assert!(
        app.compose.open && app.compose.in_pane(),
        "the reply is inline"
    );
    assert!(!app.compose.is_modal());
    let geometry = app.nav.geometry();
    let column = app.compose.window_rect();
    assert!(
        column.intersect(geometry.overview_column).is_zero_area(),
        "the reply {column:?} leaves the list {:?} in view",
        geometry.overview_column
    );
    assert!(
        app.compose.can_send(),
        "a reply has its recipient and account"
    );

    app.compose.open_blank();
    app.sync(&doc, &automerge::ROOT);
    app.layout_for_test(1280.0, 800.0);
    assert!(!app.compose.can_send(), "no recipient yet");
    let send = app
        .compose
        .accessibility_node()
        .children
        .into_iter()
        .find(|node| node.name.as_deref() == Some("Send"))
        .expect("Send is published");
    assert!(send.state.disabled, "Send is announced as unavailable");
    app.compose.type_recipient("tomas@example.org");
    assert!(app.compose.can_send());
}
#[test]
fn compose_send_accepts_recipients_in_any_header_field() {
    for (to, cc, bcc) in [
        ("dest@example.test", "", ""),
        ("", "dest@example.test", ""),
        ("", "", "hidden@example.test"),
        ("", "", ""),
    ] {
        let (mut app, mut doc) = app();
        // Replace the engine with an in-memory backend: no SMTP or other network IO.
        let (requests, outgoing) = std::sync::mpsc::channel();
        let (_events, incoming) = std::sync::mpsc::channel();
        app.state().engine = Some(EngineHandle {
            tx: requests,
            rx: incoming,
        });
        let sent_before = app.shared.borrow().mailbox.count(Folder::Sent);
        let draft = Draft {
            to: to.into(),
            cc: cc.into(),
            bcc: bcc.into(),
            subject: "Recipient regression".into(),
            body: "Hello".into(),
            ..Default::default()
        };
        app.compose.open_with_draft(&draft);
        app.send_compose(&mut ui::widget::Edits::lend(&mut doc));
        assert!(
            outgoing.try_recv().is_err(),
            "nothing reaches the server before the undo time is up"
        );
        if to.is_empty() && cc.is_empty() && bcc.is_empty() {
            assert!(app.compose.open, "an empty recipient list keeps the draft");
            assert_eq!(
                app.shared.borrow().status.as_deref(),
                Some("Add a recipient before sending")
            );
            continue;
        }
        assert!(!app.compose.open, "To={to:?}, Cc={cc:?}, Bcc={bcc:?}");
        let dispatcher = app.shared.borrow().dispatcher.clone().unwrap();
        TestMail::confirm(&mut doc, &dispatcher);
        let outbox = TestMail::outbox(&doc);
        let [(queued, None)] = outbox.as_slice() else {
            panic!("one send action must queue a message: {outbox:?}");
        };
        assert_eq!(queued.draft.to, to);
        assert_eq!(queued.draft.cc, cc);
        assert_eq!(queued.draft.bcc, bcc);
        app.server_outbox()
            .borrow_mut()
            .deliver(queued.clone(), Box::new(|_| {}));
        app.flush_server_outbox();
        app.undo_time_is_up(&mut doc);
        let EngineRequest::Send { account, draft, .. } = outgoing.try_recv().unwrap() else {
            panic!("the compose action must reach the send backend");
        };
        assert_eq!(draft.to, to);
        assert_eq!(draft.cc, cc);
        assert_eq!(draft.bcc, bcc);
        let wire = crate::transport::build_mime(
            &app.content,
            &Addr::new(account.display_name, account.email),
            &draft,
        )
        .unwrap();
        let wire = String::from_utf8(wire).unwrap();
        assert!(!wire.to_ascii_lowercase().contains("bcc:"));
        assert!(!wire.contains("hidden@example.test"));
        assert!(outgoing.try_recv().is_err(), "only one send is submitted");
        assert_eq!(app.shared.borrow().mailbox.count(Folder::Sent), sent_before);
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn calendar_invitation_handoff_creates_reviewable_drafts_and_refuses_bad_payloads() {
    {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:event@example\r\nDTSTART:20260915T120000Z\r\nDTEND:20260915T130000Z\r\nSUMMARY:Review\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let draft = prepare_calendar_invitation_draft(
            "guest@example.com",
            "Invitation: Review",
            "Please review this invitation.",
            ics,
        )
        .unwrap();
        let mut doc = crate::resource::TestMail::doc();
        let mut state = MailState::new_sample(crate::avatars::Avatars::new());
        state.save_draft(&mut ui::widget::Edits::lend(&mut doc), &draft);
        let stored = TestMail::load_draft(&doc);
        assert_eq!(stored.to, "guest@example.com");
        assert_eq!(stored.subject, "Invitation: Review");
        assert_eq!(stored.calendar_ics.as_deref(), Some(ics));
        assert!(
            stored.attachments.is_empty(),
            "no host path crosses the action"
        );
        let steps = ui::tilo_resource::history::versions(&mut doc.clone())
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|context| context.change.is_step())
            .count();
        assert_eq!(steps, 1, "saving the draft is one undo step");
    }

    {
        let valid = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert!(prepare_calendar_invitation_draft("bad", "Invite", "", valid).is_err());
        assert!(
            prepare_calendar_invitation_draft(
                "a@example.com",
                "Invite",
                "",
                &"x".repeat(256 * 1024 + 1),
            )
            .is_err()
        );
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn composer_opens_privately_seeds_replies_and_saves_drafts_in_place() {
    {
        let (mut app, mut doc) = app();
        let message = {
            let mail = app.shared.borrow();
            mail.mailbox
                .messages_in(Folder::Inbox)
                .first()
                .copied()
                .cloned()
                .expect("sample contains an inbox message")
        };
        let heads_before = doc.get_heads();

        let compose = crate::actions::effect_payload(&crate::actions::MailEffect::Compose);
        assert!(matches!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &compose)
                .unwrap(),
            ui::framework::Effected::Done(_)
        ));
        assert!(
            app.compose.open,
            "mailbox Compose opens the normal composer"
        );
        app.compose.close();

        let reply = crate::actions::effect_payload(&crate::actions::MailEffect::Reply {
            message: message.id.to_string(),
        });
        assert!(matches!(
            app.start_effect(ui::tilo_actions::InvocationId::new(), &reply)
                .unwrap(),
            ui::framework::Effected::Done(_)
        ));
        assert!(app.compose.open, "message Reply opens the normal composer");
        assert_eq!(
            app.compose
                .to_draft(&mut ui::widget::Edits::lend(&mut doc.clone()))
                .to,
            message.from.rfc5322()
        );
        assert_eq!(
            doc.get_heads(),
            heads_before,
            "an unsaved compose buffer remains local presentation state"
        );
    }

    {
        let (mut app, mut doc) = app();
        let before = app.shared.borrow().mailbox.count(Folder::Sent);
        app.compose.open_blank();
        app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        let draft = crate::model::Draft {
            to: "dest@there.example".into(),
            subject: "Hi".into(),
            body: "Hello".into(),
            html_body: Some("<p>Hello</p>".into()),
            ..Default::default()
        };
        app.compose.reattach(&draft);
        app.send_compose(&mut ui::widget::Edits::lend(&mut doc));
        let dispatcher = app
            .shared
            .borrow()
            .dispatcher
            .clone()
            .expect("a dispatcher");
        assert!(
            dispatcher.take_asked().expect("builds").is_empty(),
            "the person's own Send asks nothing: Undo is its way back"
        );
        let outbox = TestMail::outbox(&doc);
        let [(outgoing, None)] = outbox.as_slice() else {
            panic!("one message waits in the outbox: {outbox:?}");
        };
        assert_eq!(outgoing.draft.to, "dest@there.example");
        assert_eq!(outgoing.draft.subject, "Hi");
        assert!(
            !app.compose.open,
            "the message left the composer for the outbox"
        );
        assert_eq!(
            app.shared.borrow().mailbox.count(Folder::Sent),
            before,
            "a queued message is not delivered"
        );

        // The server refusing it leaves it in the outbox with the reason,
        // and brings it back to the composer.
        let (outgoing_tx, _outgoing_rx) = std::sync::mpsc::channel();
        let (incoming, incoming_rx) = std::sync::mpsc::channel();
        app.state().engine = Some(EngineHandle {
            tx: outgoing_tx,
            rx: incoming_rx,
        });
        let heard = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&heard);
        app.server_outbox().borrow_mut().deliver(
            outgoing.clone(),
            Box::new(move |result| *answer.borrow_mut() = Some(result)),
        );
        app.flush_server_outbox();
        app.undo_time_is_up(&mut doc);
        let (&request_id, _) = app
            .state()
            .pending_send
            .iter()
            .next()
            .expect("handed to the engine");
        incoming
            .send(EngineEvent::Error {
                request_id,
                kind: EngineRequestKind::Send,
                context: "send".into(),
                message: "relay denied".into(),
            })
            .unwrap();
        app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
        let settled = heard
            .borrow_mut()
            .take()
            .expect("the delivery answered")
            .expect("refused for good, not tried again");
        assert!(!settled.is_done());
        TestMail::settle_delivery(&mut doc, &outgoing.id, &settled);
        let outbox = TestMail::outbox(&doc);
        assert!(
            matches!(outbox.as_slice(), [(_, Some(reason))] if reason.contains("relay denied")),
            "{outbox:?}"
        );
        assert!(
            app.compose.open,
            "a refused message comes back to the composer"
        );

        app.send_compose(&mut ui::widget::Edits::lend(&mut doc));
        TestMail::confirm(&mut doc, &dispatcher);
        let outbox = TestMail::outbox(&doc);
        assert!(
            matches!(outbox.as_slice(), [(again, None)] if again.id != outgoing.id),
            "{outbox:?}"
        );
    }

    {
        let (mut app, mut doc) = app();
        let id = {
            let st = app.shared.borrow();
            st.mailbox
                .messages
                .values()
                .find(|m| m.folder == Folder::Inbox && m.to.len() > 1)
                .expect("a multi-recipient message")
                .id
        };
        app.shared.borrow_mut().apply_message_action(
            &mut ui::widget::Edits::lend(&mut doc),
            id,
            CtxAction::ReplyAll,
        );
        app.process_intents();
        assert!(app.compose.open, "reply-all opens the composer");
        let d = app.compose.to_draft(&mut ui::widget::Edits::lend(&mut doc));
        let sender = app
            .shared
            .borrow()
            .mailbox
            .get(id)
            .unwrap()
            .from
            .email
            .clone();
        assert!(d.to.contains(&sender), "sender goes in To: {}", d.to);
        assert!(!d.cc.is_empty(), "other recipients populate Cc: {}", d.cc);
        assert!(
            !d.cc.contains("you@example.test"),
            "our own address is excluded from Cc: {}",
            d.cc
        );
        assert!(
            !d.cc.contains(&sender),
            "the sender is not duplicated into Cc: {}",
            d.cc
        );
    }

    {
        let (mut app, mut doc) = app();
        let before = app.shared.borrow().mailbox.count(Folder::Drafts);
        app.compose.open_blank();
        app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        let draft = crate::model::Draft {
            to: "dest@there.example".into(),
            subject: "WIP proposal".into(),
            body: "half written".into(),
            html_body: Some("<p>half written</p>".into()),
            ..Default::default()
        };
        app.compose.reattach(&draft);
        let attachment =
            crate::attachment_store::stage_bytes(&app.content, b"draft agenda", "agenda.txt")
                .expect("stage portable attachment");
        app.compose
            .add_attachment(attachment.clone())
            .expect("attach to draft");
        app.save_compose_draft(&mut ui::widget::Edits::lend(&mut doc));
        assert!(!app.compose.open, "save closes the composer");
        assert_eq!(
            app.shared.borrow().mailbox.count(Folder::Drafts),
            before + 1,
            "the draft appears in Drafts"
        );

        let id = {
            let st = app.shared.borrow();
            st.mailbox
                .messages_in(Folder::Drafts)
                .iter()
                .find(|m| m.subject == "WIP proposal")
                .unwrap()
                .id
        };
        app.open_draft(id);
        assert!(app.compose.open, "the draft reopens in the composer");
        assert_eq!(
            app.compose
                .to_draft(&mut ui::widget::Edits::lend(&mut doc))
                .subject,
            "WIP proposal"
        );
        assert_eq!(
            app.compose
                .to_draft(&mut ui::widget::Edits::lend(&mut doc))
                .attachments,
            vec![attachment]
        );
        assert_eq!(app.editing_draft, Some(id));
    }

    {
        let (mut app, mut doc) = app();
        app.compose.open_blank();
        app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        app.compose.reattach(&crate::model::Draft {
            subject: "First".into(),
            body: "one".into(),
            ..Default::default()
        });
        app.save_compose_draft(&mut ui::widget::Edits::lend(&mut doc));
        let after_first = app.shared.borrow().mailbox.count(Folder::Drafts);
        let id = app.editing_draft.expect("editing the saved draft");
        app.open_draft(id);
        app.save_compose_draft(&mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(
            app.shared.borrow().mailbox.count(Folder::Drafts),
            after_first,
            "re-saving updates in place"
        );
    }

    {
        let (mut app, mut doc) = app();
        app.layout_for_test(1280.0, 800.0);
        assert!(!app.compose.open);
        assert!(
            dispatch(&mut app, &char_key("c"), &mut doc),
            "c shortcut consumed"
        );
        assert!(app.compose.open, "'c' opened the composer");
    }

    {
        for (outcome, expected) in [
            (
                Some(OpenFileOutcome::Cancelled),
                "Attachment selection cancelled",
            ),
            (
                Some(OpenFileOutcome::Failed("fixture denial".into())),
                "Couldn’t open attachment picker: fixture denial",
            ),
            (
                None,
                "Attachment picker disconnected before selecting a file",
            ),
        ] {
            let (mut app, mut doc) = app();
            app.compose.open_blank();
            let (sender, receiver) = std::sync::mpsc::channel();
            if let Some(outcome) = outcome {
                sender.send(outcome).unwrap();
            }
            drop(sender);
            app.compose_attachment_rx = Some(receiver);
            let deadline = app
                .next_wake_deadline()
                .expect("picker completion must wake");
            assert_eq!(app.next_wake_deadline(), Some(deadline));
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            assert!(app.compose_attachment_rx.is_none());
            assert_eq!(app.nav.toast_text(), Some(expected));
            app.nav.dismiss_toast();
            assert!(app.next_wake_deadline().is_none());
        }
    }

    // Phase `a_file_chosen_by_contents_is_attached`: where files have no
    // paths (a browser page) the chooser hands over contents, which are
    // staged like an opened file's.
    {
        let (mut app, mut doc) = app();
        app.compose.open_blank();
        let (sender, receiver) = std::sync::mpsc::channel();
        sender
            .send(ui::portal::ContentsOutcome::Picked(vec![
                ui::portal::PickedContents {
                    name: "notes.txt".into(),
                    media_type: None,
                    bytes: b"attached by contents".to_vec(),
                    location: None,
                },
            ]))
            .unwrap();
        app.compose_attachment_contents = Some(receiver);
        assert!(app.next_wake_deadline().is_some(), "the answer wakes");
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        assert!(app.compose_attachment_contents.is_none());
        assert_eq!(app.nav.toast_text(), Some("Attached notes.txt"));
        let attachments = app
            .compose
            .to_draft(&mut ui::widget::Edits::lend(&mut doc))
            .attachments;
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].filename, "notes.txt");
    }
}
