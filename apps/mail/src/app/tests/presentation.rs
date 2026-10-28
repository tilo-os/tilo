use super::*;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn presentation_state_stays_local() {
    {
        let mut doc = crate::resource::TestMail::doc();
        {
            let mut app = MailApp::new_sample();
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            app.sync(&doc, &automerge::ROOT);
            app.shared
                .borrow_mut()
                .select_folder(&mut ui::widget::Edits::lend(&mut doc), Folder::Sent);
            app.shared.borrow_mut().set_active_account(1);
            app.compose.set_format(ComposeFormat::Text);
        }
        // A second app sharing the document receives only domain state.
        let mut app2 = MailApp::new_sample();
        app2.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app2.sync(&doc, &automerge::ROOT);
        app2.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app2.sync(&doc, &automerge::ROOT);
        assert_eq!(
            app2.shared.borrow().folder,
            Folder::Inbox,
            "folder is local"
        );
        assert_eq!(
            app2.shared.borrow().accounts.active,
            0,
            "the device-local active account is never restored from shared state"
        );
        assert_eq!(
            app2.compose.format(),
            ComposeFormat::Html,
            "compose format is local"
        );
        assert!(
            doc.get(&automerge::ROOT, "ui")
                .expect("document remains readable")
                .is_none(),
            "window presentation never enters collaborative state"
        );
    }

    {
        let (mut app, mut doc) = app();
        let selected_id = app.shared.borrow().accounts.accounts[1].id.clone();
        app.shared.borrow_mut().set_active_account(1);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);
        app.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        app.sync(&doc, &automerge::ROOT);

        let root = app
            .accounts_persist_root()
            .expect("sample app has a sandbox");
        let reloaded = AccountStore::load_in(&root);
        assert_eq!(
            reloaded.active_account().map(|account| account.id.as_str()),
            Some(selected_id.as_str()),
            "the shared device-local account store remembers the selected account"
        );
        assert!(
            doc.get(&automerge::ROOT, "account")
                .expect("document remains readable")
                .is_none(),
            "account configuration never crosses the collaborative boundary"
        );
    }

    {
        let mut first = MailApp::new_first_run();
        let mut doc = crate::resource::TestMail::doc();
        first.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        first.sync(&doc, &automerge::ROOT);
        first.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
        first.sync(&doc, &automerge::ROOT);
        first.layout_for_test(1100.0, 780.0);
        assert!(
            first.is_first_run(),
            "no configured account → first-run onboarding"
        );
        let semantics = first
            .first_run
            .accessibility()
            .expect("first-run semantics");
        assert_eq!(semantics.name.as_deref(), Some("No email account yet"));
        assert!(semantics.children.iter().any(|node| {
            node.role == ui::a11y::Role::Button
                && node.name.as_deref() == Some("Open External Accounts")
        }));
        assert!(
            first.first_run.perform_action(
                &ui::a11y::Action::Activate,
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ),
            "assistive activation reaches the link"
        );
        first.sync(&doc, &automerge::ROOT);
        assert_eq!(
            first.opened_links,
            ["settings://external_accounts"],
            "the empty state takes the person to Settings' External Accounts"
        );
        assert!(first.is_first_run(), "Mail opened no editor of its own");

        // With accounts, the sidebar has no "+" of its own, and an
        // account's own action goes to the same place.
        let (mut app, mut doc) = app();
        app.layout_for_test(1100.0, 780.0);
        assert!(
            find_access(&app, &|node| node.name.as_deref() == Some("Add account")).is_none(),
            "the sidebar adds no account itself"
        );
        app.shared
            .borrow_mut()
            .push_command(Command::AccountSettings);
        app.apply_commands(&mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.opened_links, ["settings://external_accounts"]);
    }

    {
        let (mut app, _doc) = app();
        app.shared.borrow_mut().status = Some("Synced 3 messages".into());
        app.show_status();
        assert_eq!(app.nav.toast_text(), Some("Synced 3 messages"));
        assert!(
            app.shared.borrow().status.is_none(),
            "the shell owns it now"
        );
    }
}
