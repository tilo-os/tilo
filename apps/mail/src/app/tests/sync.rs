use super::*;

#[test]
fn settings_mail_account_connects_after_an_empty_windows_first_sync() {
    let mut host =
        ui::framework::testing::TestApp::<settings::resource::SettingsShell>::new().unwrap();
    let record = host.shared_person::<PersonAccounts>().unwrap();
    let mut mail = MailApp::new_first_run();
    let (requests, outgoing) = std::sync::mpsc::channel();
    let (_events, incoming) = std::sync::mpsc::channel();
    mail.state().engine = Some(EngineHandle {
        tx: requests,
        rx: incoming,
    });
    mail.follow_person_accounts(record.clone());
    let doc = TestMail::doc();
    mail.sync(&doc, &automerge::ROOT);
    assert!(mail.shared.borrow().seeded);
    assert!(!mail.connect_requested);
    assert!(outgoing.try_recv().is_err());

    let config = AccountConfig {
        email: "ada@example.org".into(),
        username: "ada@example.org".into(),
        imap_host: "imap.gmail.com".into(),
        smtp_host: "smtp.gmail.com".into(),
        auth: accounts::email::AuthMethod::OAuth,
        ..Default::default()
    };
    let mut stored = accounts::Account::new("gmail-ada", crate::account::TYPE_ID);
    stored.set_config(&config);
    record.invoke(&PersonAccounts::saving(&stored)).unwrap();
    mail.sync(&doc, &automerge::ROOT);
    assert_eq!(mail.shared.borrow().accounts.active_config(), config);
    assert!(!mail.is_first_run());
    assert!(mail.connect_requested);
    assert!(
        matches!(outgoing.try_recv().unwrap(), EngineRequest::Connect { account, .. } if account == config)
    );
    assert!(
        matches!(outgoing.try_recv().unwrap(), EngineRequest::Fetch { account, folder: Folder::Inbox, .. } if account == config)
    );
    assert!(outgoing.try_recv().is_err());

    mail.shared.borrow_mut().accounts.toggle_collapsed(0);
    mail.shared.borrow_mut().folder = Folder::Sent;
    let mut other = accounts::Account::new("gmail-other", crate::account::TYPE_ID);
    let mut other_config = config.clone();
    other_config.email = "other@example.org".into();
    other.set_config(&other_config);
    record.invoke(&PersonAccounts::saving(&other)).unwrap();
    mail.sync(&doc, &automerge::ROOT);
    assert_eq!(mail.shared.borrow().accounts.len(), 2);
    assert_eq!(mail.shared.borrow().accounts.active_config(), config);
    assert!(
        mail.shared
            .borrow()
            .accounts
            .active_account()
            .unwrap()
            .collapsed
    );
    assert_eq!(mail.shared.borrow().folder, Folder::Sent);
    assert!(
        outgoing.try_recv().is_err(),
        "an inactive addition must not refetch the active mailbox"
    );
}
#[cfg(feature = "live")]
#[test]
fn already_open_mail_adopts_settings_google_consent_on_sync() {
    use account_custody::{ClientTable, OAuthIssuer};
    use settings::accounts::{AccountKind, AccountRoot};
    use settings::panes::accounts::{AccountViewCommand, AccountsPane, BrowserOpener};
    use test_http::{Reply, Responder};
    use tilo_oauth::fake::{FakeAuthorizationServer, browse};

    struct Browser;
    impl BrowserOpener for Browser {
        fn open(&self, url: &str) -> ui::portal::LaunchChannel {
            let url = url.to_string();
            std::thread::spawn(move || browse(&url).unwrap());
            let (sender, receiver) = std::sync::mpsc::channel();
            sender.send(ui::portal::LaunchOutcome::Opened).unwrap();
            receiver
        }
    }

    let mut host =
        ui::framework::testing::TestApp::<settings::resource::SettingsShell>::new().unwrap();
    let record = host.shared_person::<PersonAccounts>().unwrap();
    // No mail engine or remote avatars; the account projection and vault
    // are confined to this window's private temporary profile.
    let mut mail = MailApp::new_first_run();
    let root = AccountRoot::at(mail.accounts_persist_root().unwrap()).with_person(record.clone());
    mail.follow_person_accounts(record.clone());
    let doc = TestMail::doc();
    mail.sync(&doc, &automerge::ROOT);
    assert!(mail.shared.borrow().accounts.is_empty());
    assert!(mail.is_first_run());
    assert!(mail.state().engine.is_none());

    let oauth = FakeAuthorizationServer::start();
    let registration = ClientTable::defaults()
        .client_registration(OAuthIssuer::Google)
        .unwrap();
    oauth.require_client_secret(registration.secret().unwrap());
    let gmail = Responder::start(vec![
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(r#"{"emailAddress":"ada@example.org"}"#),
    ]);
    let custody = root
        .custodian()
        .with_issuer_endpoints(OAuthIssuer::Google, oauth.provider())
        .with_gmail_profile_endpoint(format!("{}/profile", gmail.url()));
    let mut pane = AccountsPane::in_root(root.clone())
        .with_custodian(custody)
        .with_browser_opener(std::sync::Arc::new(Browser));
    pane.command(AccountViewCommand::Add(AccountKind::GoogleDrive));
    pane.command(AccountViewCommand::SubmitGoogleMail);
    assert!(pane.finish_sign_in_step_for_test(ui::testing::hang_bound()));
    assert!(
        !pane.sign_in_open(),
        "{:?}",
        pane.sign_in_message_for_test()
    );
    assert_eq!(gmail.next_request().target, "/profile");
    let published = AccountStore::load_in(root.explicit().unwrap());
    assert_eq!(published.accounts.len(), 1);
    assert_eq!(record.get().store().len(), 2);
    assert!(mail.shared.borrow().accounts.is_empty());

    // The same already-open widget observes Settings' shared-record edit
    // through the production sync path, without reopening or re-following.
    mail.sync(&doc, &automerge::ROOT);
    let shown = mail.shared.borrow();
    assert_eq!(shown.accounts.accounts.len(), 1);
    let account = shown.accounts.active_account().unwrap();
    assert_eq!(account.id, published.accounts[0].id);
    assert_eq!(account.config.email, "ada@example.org");
    assert_eq!(account.config.imap_host, "imap.gmail.com");
    assert_eq!(account.config.smtp_host, "smtp.gmail.com");
    assert_eq!(account.config.smtp_port, 587);
    assert_eq!(account.config.auth, accounts::email::AuthMethod::OAuth);
    assert!(account.config.password.is_empty());
    drop(shown);
    assert!(!mail.is_first_run());
    assert!(mail.state().engine.is_none());
}
/// A folder sync brings what is new, takes what another client changed
/// or removed, keeps the result on the device, and asks only for what
/// is new the next time.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one sync after another, each depending on what the last one left"
)]
fn folder_syncs_apply_new_mail_remote_flags_and_removals_and_are_kept() {
    let (mut app, mut doc) = app();
    let kept = tempfile::tempdir().unwrap();
    app.state().store = crate::store::Store::at(kept.path().to_path_buf());
    let (outgoing, requests) = std::sync::mpsc::channel();
    let (incoming, incoming_rx) = std::sync::mpsc::channel();
    app.state().engine = Some(EngineHandle {
        tx: outgoing,
        rx: incoming_rx,
    });
    let account = app.shared.borrow().accounts.active_config().email;
    let sync = |app: &mut MailApp, doc: &mut AutoCommit, synced: crate::sync::Synced| {
        let config = app.shared.borrow().accounts.active_config();
        let request = app
            .state()
            .post_engine(EngineRequest::fetch(config, Folder::Inbox))
            .unwrap();
        let posted = requests.try_recv().unwrap();
        incoming
            .send(EngineEvent::Fetched {
                request_id: request,
                account_id: account.clone(),
                folder: Folder::Inbox,
                synced,
            })
            .unwrap();
        incoming
            .send(EngineEvent::Completed {
                request_id: request,
                kind: EngineRequestKind::Fetch,
            })
            .unwrap();
        app.drain_engine(&mut ui::widget::Edits::lend(doc));
        app.hydrate_domain_state(&mut ui::widget::Edits::lend(doc));
        posted
    };

    // The first sync knows nothing and starts the folder from its
    // newest page.
    let posted = sync(
        &mut app,
        &mut doc,
        crate::sync::Synced {
            uid_validity: 9,
            reset: true,
            messages: vec![
                fetched(&account, 1, 9, 100),
                fetched(&account, 2, 9, 200),
                fetched(&account, 3, 9, 300),
            ],
            has_older: Some(false),
            ..Default::default()
        },
    );
    assert!(matches!(
        posted,
        EngineRequest::Fetch {
            want: crate::sync::Want::Latest { known: None },
            ..
        }
    ));
    let held = |app: &MailApp| -> Vec<u32> {
        let mut uids: Vec<u32> = app
            .shared
            .borrow()
            .mailbox
            .of_account(&account)
            .filter_map(|message| message.server.map(|at| at.uid))
            .collect();
        uids.sort_unstable();
        uids
    };
    assert_eq!(held(&app), [1, 2, 3]);

    // The next asks only for what is new to the window it holds; the
    // server says 2 was removed and 3 was read elsewhere, and 4 arrived.
    let posted = sync(
        &mut app,
        &mut doc,
        crate::sync::Synced {
            uid_validity: 9,
            messages: vec![fetched(&account, 4, 9, 400)],
            flags: vec![(1, false, false), (3, true, false)],
            window: Some((1, 3)),
            ..Default::default()
        },
    );
    assert!(matches!(
        posted,
        EngineRequest::Fetch {
            want: crate::sync::Want::Latest {
                known: Some(crate::sync::Window {
                    uid_validity: 9,
                    newest: 3,
                    oldest: 1
                })
            },
            ..
        }
    ));
    assert_eq!(held(&app), [1, 3, 4], "what the server removed goes");
    let read = app
        .shared
        .borrow()
        .mailbox
        .by_uid(&account, Folder::Inbox, 3)
        .is_some_and(|message| message.read);
    assert!(read, "read on another client reads as read here");

    // What was fetched is kept: a new window opens on it.
    let deadline = ui::testing::hang_deadline();
    let kept_uids = loop {
        let mut uids: Vec<u32> = app
            .state()
            .store
            .load(&account)
            .iter()
            .filter_map(|message| message.server.map(|at| at.uid))
            .collect();
        uids.sort_unstable();
        if uids == [1, 3, 4] || ui::time::now() > deadline {
            break uids;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(kept_uids, [1, 3, 4]);
}
/// A watched inbox that changed is fetched at once, for the account it
/// belongs to; a watch for an account no longer held is ignored.
#[test]
fn a_watched_inbox_that_changed_is_fetched_at_once() {
    let (mut app, mut doc) = app();
    let (outgoing, requests) = std::sync::mpsc::channel();
    let (incoming, incoming_rx) = std::sync::mpsc::channel();
    app.state().engine = Some(EngineHandle {
        tx: outgoing,
        rx: incoming_rx,
    });
    let account = app.shared.borrow().accounts.accounts[1].config.clone();
    for account_id in [account.email.clone(), "gone@nowhere.example".to_owned()] {
        incoming
            .send(EngineEvent::Arrived {
                request_id: uuid::Uuid::new_v4(),
                account_id,
            })
            .unwrap();
    }
    app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
    let posted: Vec<EngineRequest> = requests.try_iter().collect();
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert!(matches!(
        &posted[0],
        EngineRequest::Fetch { account: fetched, folder: Folder::Inbox, .. }
            if fetched.email == account.email
    ));
}
/// Every inbox together lists each account's mail, and a reply to a
/// message goes from the account it came to.
#[test]
fn all_inboxes_list_every_account_and_replies_go_from_the_owner() {
    let (mut app, _doc) = app();
    let (first, second) = {
        let st = app.shared.borrow();
        (
            st.accounts.accounts[0].config.email.clone(),
            st.accounts.accounts[1].config.email.clone(),
        )
    };
    let to_first = fetched(&first, 1, 1, 100);
    let to_second = fetched(&second, 2, 1, 200);
    let (first_id, second_id) = (to_first.id, to_second.id);
    {
        let mut st = app.shared.borrow_mut();
        st.mailbox.insert(to_first);
        st.mailbox.insert(to_second);
    }
    let listed = |app: &MailApp| -> Vec<MessageId> {
        app.shared
            .borrow()
            .list_rows()
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    };
    assert!(
        !listed(&app).contains(&second_id),
        "the open account lists its own mail"
    );
    app.shared
        .borrow_mut()
        .push_command(Command::OpenAllInboxes);
    app.apply_commands(&mut ui::widget::Edits::lend(
        &mut crate::resource::TestMail::doc(),
    ));
    app.sync(&crate::resource::TestMail::doc(), &automerge::ROOT);
    let rows = listed(&app);
    assert!(
        rows.contains(&first_id) && rows.contains(&second_id),
        "{rows:?}"
    );
    assert_eq!(
        crate::sidebar::selected_row(&app.shared.borrow()).as_deref(),
        Some(crate::sidebar::ALL_INBOXES)
    );
    app.open_reply(second_id);
    assert_eq!(app.compose.sending_from(), Some(second.as_str()));
    assert_eq!(app.sending_account().email, second);
}
/// Searching asks every account's server once the text stops changing,
/// and what the servers find joins the mail the search reads.
#[test]
fn a_search_asks_the_servers_and_keeps_what_they_find() {
    let (mut app, mut doc) = app();
    let (outgoing, requests) = std::sync::mpsc::channel();
    let (incoming, incoming_rx) = std::sync::mpsc::channel();
    app.state().engine = Some(EngineHandle {
        tx: outgoing,
        rx: incoming_rx,
    });
    app.state().search_servers("in");
    assert!(
        app.state().server_search.is_none(),
        "two letters search only here"
    );
    assert!(
        !app.state().searching_servers(),
        "nothing for a search to wait on"
    );
    app.state().search_servers("invoice");
    assert!(
        app.state().searching_servers(),
        "a search waits for the servers"
    );
    app.state().post_server_search();
    assert!(
        requests.try_recv().is_err(),
        "not while the text may change"
    );
    {
        let mut room = app.state();
        room.server_search = room
            .server_search
            .take()
            .map(|(text, _)| (text, ui::time::now()));
        room.post_server_search();
    }
    let posted: Vec<EngineRequest> = requests.try_iter().collect();
    assert_eq!(
        posted.len(),
        app.shared.borrow().accounts.accounts.len(),
        "every account is asked"
    );
    let EngineRequest::Search {
        request_id,
        account,
        text,
    } = posted[0].clone()
    else {
        panic!("a search: {posted:?}");
    };
    assert_eq!(text, "invoice");
    assert!(app.state().searching_servers(), "the servers are looking");
    app.state().search_servers("invoice");
    assert!(
        app.state().server_search.is_none(),
        "the same text is not asked again"
    );

    let mut old = fetched(&account.email, 9, 1, 10);
    old.server = None;
    let old_id = old.id;
    incoming
        .send(EngineEvent::Found {
            request_id,
            account_id: account.email,
            messages: vec![old],
        })
        .unwrap();
    incoming
        .send(EngineEvent::Completed {
            request_id,
            kind: EngineRequestKind::Search,
        })
        .unwrap();
    app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(
        app.state().searching_servers(),
        posted.len() > 1,
        "the other accounts are still looking"
    );
    for other in &posted[1..] {
        incoming
            .send(EngineEvent::Completed {
                request_id: other.request_id(),
                kind: EngineRequestKind::Search,
            })
            .unwrap();
    }
    app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
    assert!(!app.state().searching_servers(), "every server answered");
    assert!(
            crate::search::message_entries(&app.shared.borrow().mailbox)
                .iter()
                .any(|entry| entry.anchor.as_deref()
                    == Some(format!("mail.message:{old_id}").as_str())),
            "what the server found is searched"
        );
}
/// A sent message waits out the undo time before it reaches the server;
/// "Undo" on the sending notice takes it back to the composer unsent.
#[test]
fn a_sent_message_waits_so_undo_takes_it_back_unsent() {
    let send = |app: &mut MailApp, doc: &mut AutoCommit| -> Outgoing {
        app.compose.open_blank();
        app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        app.compose.reattach(&crate::model::Draft {
            to: "dest@there.example".into(),
            subject: "Hi".into(),
            body: "Hello".into(),
            ..Default::default()
        });
        app.send_compose(&mut ui::widget::Edits::lend(doc));
        let dispatcher = app
            .shared
            .borrow()
            .dispatcher
            .clone()
            .expect("a dispatcher");
        TestMail::confirm(doc, &dispatcher);
        let outbox = TestMail::outbox(doc);
        let [(outgoing, None)] = outbox.as_slice() else {
            panic!("one message waits in the outbox: {outbox:?}");
        };
        outgoing.clone()
    };
    let (mut app, mut doc) = app();
    let (outgoing_tx, requests) = std::sync::mpsc::channel();
    let (_incoming, incoming_rx) = std::sync::mpsc::channel();
    app.state().engine = Some(EngineHandle {
        tx: outgoing_tx,
        rx: incoming_rx,
    });
    let outgoing = send(&mut app, &mut doc);
    app.show_status();
    assert!(app.send_notice, "the sending notice offers Undo");
    // The notice outlives its own four-second lifetime while a send can
    // still be taken back for ten (`APP-mail-01`): it goes, and comes
    // back on the next sync until the message is on its way.
    app.nav.dismiss_toast();
    app.show_status();
    assert!(
        app.nav
            .toast_text()
            .is_some_and(|text| text.contains("Sending")),
        "the undo notice is still up while the message is held"
    );
    app.server_outbox()
        .borrow_mut()
        .deliver(outgoing, Box::new(|_| {}));
    app.flush_server_outbox();
    app.release_held_sends(&ui::widget::Edits::lend(&mut doc));
    assert!(requests.try_recv().is_err(), "it waits before it goes");
    assert!(
        app.next_wake_deadline().is_some(),
        "the window wakes to send it"
    );

    app.unsend = true;
    app.take_back_sends(&mut ui::widget::Edits::lend(&mut doc));
    let dispatcher = app
        .shared
        .borrow()
        .dispatcher
        .clone()
        .expect("a dispatcher");
    TestMail::confirm(&mut doc, &dispatcher);
    assert!(TestMail::outbox(&doc).is_empty(), "taken back unsent");
    assert!(app.compose.open, "back in the composer");
    app.flush_server_outbox();
    assert!(requests.try_recv().is_err(), "never sent");

    app.compose.close();
    let outgoing = send(&mut app, &mut doc);
    app.server_outbox()
        .borrow_mut()
        .deliver(outgoing, Box::new(|_| {}));
    app.flush_server_outbox();
    app.undo_time_is_up(&mut doc);
    assert!(
        matches!(requests.try_recv(), Ok(EngineRequest::Send { .. })),
        "after the undo time it goes"
    );
}
/// A forward carries the forwarded message's files: one on this device
/// goes with it, and one not downloaded yet is named, not dropped
/// unsaid.
#[test]
fn a_forward_carries_the_messages_files() {
    let (mut app, mut doc) = app();
    let mut message = app
        .shared
        .borrow()
        .mailbox
        .messages
        .values()
        .next()
        .expect("a seeded message")
        .clone();
    message.id = uuid::Uuid::new_v4();
    message.folder = Folder::Inbox;
    message.subject = "Figures".into();
    message.attachments = vec![
        Attachment::hydrated("q3.csv", "text/csv", b"quarter,total\n3,42\n".to_vec()),
        Attachment::projected("deck.pdf", "application/pdf", 2048),
    ];
    let id = message.id;
    app.shared.borrow_mut().mailbox.insert(message);

    app.open_forward(id);
    assert!(app.compose.open, "Forward opens the composer");
    let draft = app.compose.to_draft(&mut ui::widget::Edits::lend(&mut doc));
    let [file] = draft.attachments.as_slice() else {
        panic!(
            "the downloaded file goes with the forward: {:?}",
            draft.attachments
        );
    };
    assert_eq!(file.filename, "q3.csv");
    assert_eq!(
        crate::attachment_store::materialize(&app.content, file).unwrap(),
        b"quarter,total\n3,42\n"
    );
    let status = app.shared.borrow().status.clone().unwrap_or_default();
    assert!(
        status.contains("deck.pdf") && status.contains("not downloaded"),
        "the file left behind is named: {status}"
    );
    assert!(
        !app.compose.touched(),
        "a forward nobody changed leaves no draft when closed"
    );
}
/// A forward says truly why a file stays behind: a downloaded file past
/// what one message carries is too large, not "not downloaded", and a
/// file whose bytes another one already carries is not left behind.
#[test]
fn a_forward_names_a_file_too_large_to_carry_as_too_large() {
    let (mut app, mut doc) = app();
    let mut message = app
        .shared
        .borrow()
        .mailbox
        .messages
        .values()
        .next()
        .expect("a seeded message")
        .clone();
    message.id = uuid::Uuid::new_v4();
    message.folder = Folder::Inbox;
    message.subject = "Footage".into();
    let past_one_message = vec![0_u8; crate::attachment_store::MAX_ATTACHMENT_BYTES + 1];
    message.attachments = vec![
        Attachment::hydrated("notes.txt", "text/plain", b"take two".to_vec()),
        Attachment::hydrated("copy of notes.txt", "text/plain", b"take two".to_vec()),
        Attachment::hydrated("footage.mp4", "video/mp4", past_one_message),
    ];
    let id = message.id;
    app.shared.borrow_mut().mailbox.insert(message);

    app.open_forward(id);
    let draft = app.compose.to_draft(&mut ui::widget::Edits::lend(&mut doc));
    let [file] = draft.attachments.as_slice() else {
        panic!(
            "the notes go with the forward once: {:?}",
            draft.attachments
        );
    };
    assert_eq!(file.filename, "notes.txt");
    let status = app.shared.borrow().status.clone().unwrap_or_default();
    assert!(
        status.contains("footage.mp4") && status.contains("more than one message can carry"),
        "the downloaded file too large to carry is named as too large: {status}"
    );
    assert!(
        !status.contains("not downloaded"),
        "nothing here waits to be downloaded: {status}"
    );
    assert!(
        !status.contains("copy of notes.txt"),
        "the notes' bytes go with the forward: {status}"
    );
}
/// A saved draft, opened again and sent, is done with: it leaves
/// Drafts, and the mailbox's draft is empty for the next message.
#[test]
fn a_sent_draft_leaves_drafts() {
    let (mut app, mut doc) = app();
    TestMail::store_draft(
        &mut doc,
        &crate::model::Draft {
            to: "dest@there.example".into(),
            subject: "Ready to go".into(),
            body: "See you at noon".into(),
            ..Default::default()
        },
    );
    app.hydrate_domain_state_now(&mut ui::widget::Edits::lend(&mut doc));
    let in_drafts = |app: &MailApp| -> Vec<MessageId> {
        app.shared
            .borrow()
            .mailbox
            .messages_in(Folder::Drafts)
            .iter()
            .filter(|message| message.subject == "Ready to go")
            .map(|message| message.id)
            .collect()
    };
    let [id] = in_drafts(&app)[..] else {
        panic!("the saved draft shows in Drafts, once");
    };

    app.open_draft(id);
    app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
    app.send_compose(&mut ui::widget::Edits::lend(&mut doc));
    let outbox = TestMail::outbox(&doc);
    assert!(
        matches!(outbox.as_slice(), [(sent, None)] if sent.draft.subject == "Ready to go"),
        "the draft went to the outbox: {outbox:?}"
    );
    assert!(
        TestMail::load_draft(&doc).is_empty(),
        "sending the draft empties it: {:?}",
        TestMail::load_draft(&doc)
    );
    app.hydrate_domain_state_now(&mut ui::widget::Edits::lend(&mut doc));
    assert!(
        in_drafts(&app).is_empty(),
        "the sent message no longer shows in Drafts"
    );
}
/// Select all acts on the whole folder: without a server to read the
/// rest from, what the device holds is the whole of it, and the status
/// says how many it took rather than leaving the person to count.
#[test]
fn select_all_takes_the_whole_folder_and_says_how_many() {
    let (mut app, mut doc) = app();
    app.shared.borrow_mut().set_bulk_selecting(true);
    app.shared
        .borrow_mut()
        .push_command(Command::SelectAllVisible);
    app.apply_commands(&mut ui::widget::Edits::lend(&mut doc));
    let (rows, selected, status) = {
        let st = app.shared.borrow();
        (
            st.list_rows().len(),
            st.selected_bulk_ids().len(),
            st.status.clone(),
        )
    };
    assert!(rows > 0, "the sample inbox holds rows");
    assert!(
        !app.shared.borrow().selecting_all,
        "there is nothing more to read"
    );
    assert_eq!(selected, rows, "every row is chosen");
    assert_eq!(
        status.as_deref(),
        Some(format!("Selected all {rows} messages in Inbox").as_str())
    );
}
/// Select all on a folder whose server holds pages this device has not
/// read asks for them, and acts on none of them until the folder is
/// whole — the old "Marked 50 read" over a wall of unread mail was a
/// whole-folder command applied to one page.
#[test]
fn select_all_reads_the_pages_the_server_still_holds() {
    let (mut app, mut doc) = app();
    let (outgoing, requests) = std::sync::mpsc::channel();
    let (incoming, incoming_rx) = std::sync::mpsc::channel();
    app.state().engine = Some(EngineHandle {
        tx: outgoing,
        rx: incoming_rx,
    });
    let account = app.shared.borrow().accounts.active_config().email;
    let first_page = |app: &mut MailApp, doc: &mut AutoCommit| {
        let config = app.shared.borrow().accounts.active_config();
        let request = app
            .state()
            .post_engine(EngineRequest::fetch(config, Folder::Inbox))
            .unwrap();
        let _ = requests.try_recv().unwrap();
        incoming
            .send(EngineEvent::Fetched {
                request_id: request,
                account_id: account.clone(),
                folder: Folder::Inbox,
                synced: crate::sync::Synced {
                    uid_validity: 9,
                    reset: true,
                    messages: vec![fetched(&account, 1, 9, 100), fetched(&account, 2, 9, 200)],
                    has_older: Some(true),
                    ..Default::default()
                },
            })
            .unwrap();
        incoming
            .send(EngineEvent::Completed {
                request_id: request,
                kind: EngineRequestKind::Fetch,
            })
            .unwrap();
        app.drain_engine(&mut ui::widget::Edits::lend(doc));
        app.hydrate_domain_state(&mut ui::widget::Edits::lend(doc));
    };
    first_page(&mut app, &mut doc);
    assert_eq!(
        app.has_older.get(&(account.clone(), Folder::Inbox)),
        Some(&true)
    );

    // Select all: the page held is chosen, and the page before is asked
    // for rather than "all" being reported over what this device has.
    app.shared.borrow_mut().set_bulk_selecting(true);
    app.shared
        .borrow_mut()
        .push_command(Command::SelectAllVisible);
    app.apply_commands(&mut ui::widget::Edits::lend(&mut doc));
    let posted: Vec<EngineRequest> = requests.try_iter().collect();
    assert!(
        posted.iter().any(|request| matches!(
            request,
            EngineRequest::Fetch {
                want: crate::sync::Want::Older { .. },
                ..
            }
        )),
        "the page before is asked for: {posted:?}"
    );
    assert!(app.shared.borrow().selecting_all);
    let chose = app.shared.borrow().selected_bulk_ids().len();
    assert_eq!(
        app.shared.borrow().status.as_deref(),
        Some("Reading the rest of this folder…")
    );

    // The last page arrives: the folder is whole, and the selection is
    // every message in it.
    let fetch = EngineRequest::fetch(app.shared.borrow().accounts.active_config(), Folder::Inbox);
    let request = app.state().post_engine(fetch).unwrap();
    let _ = requests.try_recv().unwrap();
    incoming
        .send(EngineEvent::Fetched {
            request_id: request,
            account_id: account.clone(),
            folder: Folder::Inbox,
            synced: crate::sync::Synced {
                uid_validity: 9,
                messages: vec![fetched(&account, 3, 9, 300)],
                has_older: Some(false),
                ..Default::default()
            },
        })
        .unwrap();
    incoming
        .send(EngineEvent::Completed {
            request_id: request,
            kind: EngineRequestKind::Fetch,
        })
        .unwrap();
    app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
    app.hydrate_domain_state(&mut ui::widget::Edits::lend(&mut doc));
    let st = app.shared.borrow();
    assert!(!st.selecting_all, "there is nothing left to read");
    let held = st.list_rows().len();
    assert!(held > chose, "the page that arrived joined it");
    assert_eq!(st.selected_bulk_ids().len(), held);
    assert_eq!(
        st.status.as_deref(),
        Some(format!("Selected all {held} messages in Inbox").as_str())
    );
}
/// The outbox is a place: a row in the sidebar, a page of what waits to
/// leave with the reason a server refused it, and the verb that takes a
/// message back unsent.
#[test]
fn the_outbox_shows_what_waits_and_why_and_takes_a_message_back() {
    let (mut app, mut doc) = app();
    TestMail::store_draft(
        &mut doc,
        &crate::model::Draft {
            to: "dest@there.example".into(),
            subject: "Ready to go".into(),
            body: "See you at noon".into(),
            ..Default::default()
        },
    );
    app.hydrate_domain_state_now(&mut ui::widget::Edits::lend(&mut doc));
    let id = app
        .shared
        .borrow()
        .mailbox
        .messages_in(Folder::Drafts)
        .first()
        .map(|message| message.id)
        .expect("the draft shows in Drafts");
    app.open_draft(id);
    app.compose.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
    app.send_compose(&mut ui::widget::Edits::lend(&mut doc));
    let waiting = TestMail::outbox(&doc);
    let [(queued, None)] = waiting.as_slice() else {
        panic!("one message waits: {waiting:?}");
    };
    let queued_id = queued.id.clone();

    // A device that holds the account refuses it, in its own words.
    TestMail::settle_delivery(
        &mut doc,
        &queued_id,
        &ui::tilo_resource::Settled::failed("550 mailbox unavailable".to_owned()),
    );
    app.sync(&doc, &automerge::ROOT);
    app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    app.sync(&doc, &automerge::ROOT);

    let account = app.shared.borrow().accounts.active;
    app.shared.borrow_mut().open_outbox(account);
    app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    app.sync(&doc, &automerge::ROOT);
    {
        let state = app.shared.borrow();
        assert_eq!(state.queued_count(account), 1, "the sidebar counts it");
        let [row] = state.queued()[..] else {
            panic!("the outbox holds the message that never left");
        };
        assert_eq!(row.to, "dest@there.example");
        assert_eq!(row.subject, "Ready to go");
        assert_eq!(
            row.state_line(),
            "550 mailbox unavailable",
            "the server's refusal is the row's own words"
        );
        assert_eq!(
            crate::sidebar::selected_row(&state).as_deref(),
            Some("0/outbox"),
            "the sidebar shows where the person is"
        );
    }
    assert_eq!(
        crate::window_bar::location(&app.shared.borrow(), None),
        "Outbox"
    );
    // The page says the same: what waits, and the verb that takes it back.
    let tree = MailApp::overview_accessibility_node(&app.shared.borrow());
    let said = format!("{tree:?}");
    assert!(
        said.contains("To dest@there.example: Ready to go"),
        "{said}"
    );
    assert!(said.contains("550 mailbox unavailable"), "{said}");
    app.perform_semantic_action(
        &format!("mail.outbox-cancel:{queued_id}"),
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    );
    assert!(
        TestMail::outbox(&doc).is_empty(),
        "taken out of the outbox unsent: {:?}",
        TestMail::outbox(&doc)
    );
    assert_eq!(
        app.shared.borrow().status.as_deref(),
        Some("Taken out of the outbox; nothing was sent"),
        "the page says what it did"
    );
    app.sync(&doc, &automerge::ROOT);
    assert_eq!(app.shared.borrow().queued_count(account), 0, "and gone");
}
/// A failure that arrives after the person switched accounts is said of
/// the account that asked, not of the one selected now.
#[test]
fn a_late_engine_error_stays_with_the_account_that_asked() {
    let (mut app, mut doc) = app();
    let (outgoing, outgoing_rx) = std::sync::mpsc::channel();
    let (incoming, incoming_rx) = std::sync::mpsc::channel();
    app.state().engine = Some(EngineHandle {
        tx: outgoing,
        rx: incoming_rx,
    });
    let asking = crate::model::AccountConfig {
        email: "asking@example.test".into(),
        ..Default::default()
    };
    let request = app
        .state()
        .post_engine(EngineRequest::fetch(asking, Folder::Inbox))
        .expect("the fetch went to the engine");
    assert_eq!(
        outgoing_rx.try_recv().unwrap().account(),
        "asking@example.test"
    );
    app.shared.borrow_mut().accounts =
        crate::account::AccountStore::single(crate::model::AccountConfig {
            email: "selected@example.test".into(),
            ..Default::default()
        });
    incoming
        .send(EngineEvent::Error {
            request_id: request,
            kind: EngineRequestKind::Fetch,
            context: "fetch".into(),
            message: "authentication failed".into(),
        })
        .unwrap();
    app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
    let status = app.shared.borrow().status.clone().unwrap_or_default();
    assert!(status.contains("asking@example.test"), "{status}");
    assert!(!status.contains("selected@example.test"), "{status}");
    // Its sync health is kept for that account, in this app's profile.
    let root = app.accounts_persist_root().expect("sandboxed profile");
    let health = accounts::AccountSyncStatusStore::load_in(&root);
    assert_eq!(
        health
            .get(&accounts::AccountId::from("asking@example.test"))
            .map(|status| status.health),
        Some(accounts::AccountSyncHealth::NeedsAuthentication)
    );
    assert!(
        health
            .get(&accounts::AccountId::from("selected@example.test"))
            .is_none()
    );
}
/// What the document still has to tell the server goes to the engine on
/// the account it belongs to, and the engine's acknowledgement records it
/// as applied; a refusal leaves it to tell, and an update for an account
/// this device does not have is left to a device that has it.
#[test]
fn server_updates_go_to_the_engine_and_its_acknowledgement_settles_them() {
    type Outcome = Rc<RefCell<Option<Result<(), String>>>>;
    fn push(app: &MailApp, update: ServerUpdate) -> Outcome {
        let outcome: Outcome = Rc::default();
        let seen = Rc::clone(&outcome);
        app.server_outbox().borrow_mut().push(
            update,
            Box::new(move |result| *seen.borrow_mut() = Some(result)),
        );
        outcome
    }

    let (mut app, mut doc) = app();
    let configured_email = app.shared.borrow().accounts.active_config().email;
    let (outgoing, outgoing_rx) = std::sync::mpsc::channel();
    let (incoming, incoming_rx) = std::sync::mpsc::channel();
    app.state().engine = Some(EngineHandle {
        tx: outgoing,
        rx: incoming_rx,
    });
    let id = app.shared.borrow().mailbox.messages_in(Folder::Inbox)[0].id;
    let message = {
        let mut state = app.shared.borrow_mut();
        let message = state.mailbox.get_mut(id).unwrap();
        message.rfc_message_id = Some("launch@example.test".into());
        message.clone()
    };
    app.shared.borrow().ingest_fetched(
        &mut ui::widget::Edits::lend(&mut doc),
        &configured_email,
        std::slice::from_ref(&message),
    );
    app.shared
        .borrow_mut()
        .set_read(&mut ui::widget::Edits::lend(&mut doc), id, !message.read);
    let [update] = TestMail::server_updates(&doc).try_into().unwrap();

    let outcome = push(&app, update.clone());
    app.flush_server_outbox();
    let request = outgoing_rx
        .try_recv()
        .expect("the update went to the engine");
    let EngineRequest::StoreFlag {
        account,
        message: located,
        ..
    } = &request
    else {
        panic!("a flag change is a flag store: {request:?}");
    };
    assert_eq!(account.email, configured_email);
    assert_eq!(
        located.rfc_message_id.as_deref(),
        Some("launch@example.test")
    );
    assert_eq!(located.read, !message.read);
    assert_eq!(located.folder, Folder::Inbox);

    incoming
        .send(EngineEvent::Error {
            request_id: request.request_id(),
            kind: EngineRequestKind::StoreFlag,
            context: "flag".into(),
            message: "connection reset".into(),
        })
        .unwrap();
    app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
    assert!(matches!(outcome.borrow().as_ref(), Some(Err(_))));
    assert_eq!(
        TestMail::server_updates(&doc),
        std::slice::from_ref(&update),
        "still to tell"
    );

    let outcome = push(&app, update);
    app.flush_server_outbox();
    let request = outgoing_rx.try_recv().expect("retried");
    incoming
        .send(EngineEvent::Completed {
            request_id: request.request_id(),
            kind: EngineRequestKind::StoreFlag,
        })
        .unwrap();
    app.drain_engine(&mut ui::widget::Edits::lend(&mut doc));
    assert!(matches!(outcome.borrow().as_ref(), Some(Ok(()))));
    assert!(
        TestMail::server_updates(&doc).is_empty(),
        "the server has it"
    );

    let elsewhere = push(
        &app,
        ServerUpdate::Remove {
            id: id.to_string(),
            account: "nobody@example.test".into(),
            locator: "launch@example.test".into(),
            folder: Folder::Trash,
        },
    );
    app.flush_server_outbox();
    assert!(
        outgoing_rx.try_recv().is_err(),
        "nothing went to the engine"
    );
    assert!(matches!(elsewhere.borrow().as_ref(), Some(Err(_))));
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the regression test spells out a complete interleaving so ownership mistakes remain diagnosable"
)]
fn engine_events_settle_only_their_own_request() {
    {
        let mut fixture = InterleavedEngineFixture::new();
        let deadline = fixture
            .app
            .next_wake_deadline()
            .expect("worker results wake independently of animation");
        assert_eq!(fixture.app.next_wake_deadline(), Some(deadline));

        fixture.send(EngineEvent::Error {
            request_id: fixture.unrelated_id,
            kind: EngineRequestKind::StoreFlag,
            context: "flag".into(),
            message: "rejected".into(),
        });
        fixture.send(EngineEvent::Sent {
            request_id: fixture.unrelated_id,
            wire_bytes: 9,
        });
        fixture.send(EngineEvent::Completed {
            request_id: fixture.unrelated_id,
            kind: EngineRequestKind::StoreFlag,
        });
        fixture.drain();
        assert!(fixture.app.state().resource_refresh_invocation.is_some());
        assert!(fixture.app.state().resource_refresh_result.is_none());
        assert!(!fixture.app.state().pending_send.is_empty());
        assert!(
            fixture
                .app
                .state()
                .pending_engine_requests
                .contains_key(&fixture.refresh_id)
        );
        assert!(
            fixture
                .app
                .state()
                .pending_engine_requests
                .contains_key(&fixture.send_id)
        );
        assert!(
            !fixture
                .app
                .state()
                .pending_engine_requests
                .contains_key(&fixture.unrelated_id)
        );
        assert!(
            fixture
                .app
                .shared
                .borrow()
                .status
                .as_deref()
                .is_none_or(|status| !status.contains("sent")),
            "an unmatched Sent event must not claim delivery"
        );

        fixture.send(EngineEvent::Fetched {
            request_id: fixture.send_id,
            account_id: "me@example.test".into(),
            folder: Folder::Inbox,
            synced: crate::sync::Synced::default(),
        });
        fixture.drain();
        assert!(
            !fixture.app.state().pending_send.is_empty(),
            "wrong-kind results are inert"
        );
        assert!(
            fixture
                .app
                .state()
                .pending_engine_requests
                .contains_key(&fixture.send_id)
        );

        fixture.send(EngineEvent::Fetched {
            request_id: fixture.refresh_id,
            account_id: "me@example.test".into(),
            folder: Folder::Inbox,
            synced: crate::sync::Synced::default(),
        });
        fixture.send(EngineEvent::Completed {
            request_id: fixture.refresh_id,
            kind: EngineRequestKind::Fetch,
        });
        fixture.drain();
        assert!(fixture.app.state().resource_refresh_invocation.is_some());
        assert!(matches!(
            fixture.app.state().resource_refresh_result.as_ref(),
            Some(tilo_actions::InvocationUpdate::Succeeded { invocation: got, .. }) if *got == fixture.invocation
        ));
        assert!(matches!(
            fixture.app.state().take_effect_result(fixture.invocation),
            Some(tilo_actions::InvocationUpdate::Succeeded { invocation: got, .. }) if got == fixture.invocation
        ));
        assert!(fixture.app.state().resource_refresh_invocation.is_none());
        assert!(!fixture.app.state().pending_send.is_empty());

        fixture.send(EngineEvent::Sent {
            request_id: fixture.send_id,
            wire_bytes: 42,
        });
        fixture.send(EngineEvent::Completed {
            request_id: fixture.send_id,
            kind: EngineRequestKind::Send,
        });
        fixture.drain();
        assert!(fixture.app.state().pending_send.is_empty());
        assert!(!fixture.app.state().awaiting_engine);
        assert_eq!(
            fixture.app.next_wake_deadline(),
            fixture.app.nav.next_wake_deadline()
        );
    }

    {
        let (mut app, _doc) = app();
        let (outgoing, _outgoing_rx) = std::sync::mpsc::channel();
        let (_incoming_tx, incoming) = std::sync::mpsc::channel();
        app.state().engine = Some(EngineHandle {
            tx: outgoing,
            rx: incoming,
        });
        let request_id = uuid::Uuid::new_v4();
        let owner = tilo_actions::InvocationId::new();
        app.state().resource_refresh_invocation = Some((request_id, owner));
        app.state().resource_refresh_result = Some(tilo_actions::InvocationUpdate::Succeeded {
            invocation: owner,
            output: tilo_actions::ActionOutput::default(),
        });

        let payload = crate::actions::effect_payload(&crate::actions::MailEffect::Refresh);
        assert!(
            app.start_effect(tilo_actions::InvocationId::new(), &payload)
                .is_err()
        );
        assert!(
            app.state()
                .take_effect_result(tilo_actions::InvocationId::new())
                .is_none()
        );
        assert!(matches!(
            app.state().take_effect_result(owner),
            Some(tilo_actions::InvocationUpdate::Succeeded { invocation, .. }) if invocation == owner
        ));
        assert!(app.state().resource_refresh_invocation.is_none());
    }
}
/// Sample mode keeps its accounts in a sandbox, never the real store; a
/// live window keeps them in its host's profile.
#[test]
fn account_roots_follow_each_mode() {
    {
        // Regression guard for the leak where the bundled demo accounts
        // (you@example.test, you@hooli.example, new@host.example) ended up in the
        // user's real ~/.config/tilo/accounts/store.json: the sample / shots /
        // test app must resolve a throwaway sandbox as its persist target, never
        // the real per-user config root.
        let root = MailApp::new_sample()
            .accounts_persist_root()
            .expect("sample mode has a persist root");
        let real = accounts::config_root().expect("a resolvable real config root");
        assert_ne!(
            root, real,
            "sample mode must NOT target the real config root"
        );
        assert!(
            root.starts_with(std::env::temp_dir()),
            "sample mode must persist under a throwaway temp dir, got {}",
            root.display()
        );
    }

    {
        // A session window keeps the person's accounts; a test, capture or
        // `--data-dir` profile keeps its own beside its settings document.
        let session = ui::storage::AppStorage::for_app("so.tilo.mail");
        assert_eq!(session.config_root(), accounts::config_root());
        let profile = tempfile::tempdir().unwrap();
        let storage = ui::storage::AppStorage::new(
            "so.tilo.mail",
            session.secrets(),
            ui::storage::SettingsDocument::Profile(profile.path().to_path_buf()),
        );
        assert_eq!(storage.config_root().as_deref(), Some(profile.path()));
        let app = MailApp::for_window(&storage, Some(ContentStore::memory()));
        assert_eq!(
            app.accounts_persist_root().as_deref(),
            Some(profile.path()),
            "the live app persists into its host's profile"
        );
        assert!(
            app.shared.borrow().accounts.is_empty(),
            "a fresh profile has no accounts"
        );
        assert!(
            app.shared.borrow().mailbox.messages.is_empty(),
            "no fabricated startup mail"
        );
    }
}

#[test]
fn trusted_senders_are_a_record_both_ways() {
    use crate::trusted_senders::TrustedSenders;

    let mut host = ui::framework::testing::TestApp::<crate::resource::MailShell>::new().unwrap();
    let record = host.person::<TrustedSenders>().unwrap();
    let mut mail = MailApp::new_first_run();
    mail.follow_trusted_senders(record.clone());
    let doc = TestMail::doc();

    // Another window or device trusts a sender: this window adopts it.
    record
        .invoke(&TrustedSenders::TRUST.with("lena@fischer.example".to_owned()))
        .unwrap();
    mail.sync(&doc, &automerge::ROOT);
    assert!(
        mail.shared
            .borrow()
            .remote_senders
            .contains("lena@fischer.example")
    );

    // This window trusts one and stops trusting the other: both are the
    // person's edits of the record.
    {
        let mut shared = mail.shared.borrow_mut();
        shared.remote_senders.remove("lena@fischer.example");
        shared
            .remote_senders
            .insert("hello@fieldnotes.example".to_owned());
    }
    mail.sync(&doc, &automerge::ROOT);
    assert_eq!(
        record.with(TrustedSenders::addresses),
        std::collections::BTreeSet::from(["hello@fieldnotes.example".to_owned()])
    );
    assert!(record.undo(), "trusting is an undo step of the person's");
}
