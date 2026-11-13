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
                && node.name.as_deref() == Some("Open External accounts")
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
        app.shared.borrow_mut().status = Some(Notice::info("Synced 3 messages"));
        app.show_status();
        assert_eq!(app.nav.toast_text(), Some("Synced 3 messages"));
        assert!(
            app.shared.borrow().status.is_none(),
            "the shell owns it now"
        );
    }
}

/// A window with no account is the person's whole way into Mail: it must name
/// the account route, in the language the person reads, and activating that
/// named control must reach Settings' External Accounts — the same door the
/// pill and the window bar's Add account take.
#[test]
fn a_fresh_window_offers_adding_an_account() {
    fn route_named(node: &ui::a11y::AccessNode, action: &str) -> Option<String> {
        if node.action_target.as_deref() == Some(action) && node.role == ui::a11y::Role::Button {
            return node.name.clone();
        }
        node.children
            .iter()
            .find_map(|child| route_named(child, action))
    }

    let mut mail = MailApp::new_first_run();
    let mut doc = crate::resource::TestMail::doc();
    mail.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
    mail.sync(&doc, &automerge::ROOT);
    mail.layout_for_test(1100.0, 780.0);
    let window = mail
        .accessibility()
        .expect("a fresh Mail window has semantics");
    assert_eq!(
        route_named(&window, ACCOUNT_ROUTE).as_deref(),
        Some("Open External accounts"),
        "the window's own tree names the account route, not only its page"
    );
    assert!(
        mail.perform_semantic_action(
            ACCOUNT_ROUTE,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ),
        "the named control activates"
    );
    assert_eq!(
        mail.opened_links,
        ["settings://external_accounts"],
        "and takes the person where an account is added"
    );

    // The same window read in German names the route in German.
    crate::i18n::CATALOG.set_locale("de-DE");
    let mut german = MailApp::new_first_run();
    german.layout_for_test(1100.0, 780.0);
    let window = german
        .accessibility()
        .expect("a fresh Mail window has semantics");
    assert_eq!(
        route_named(&window, ACCOUNT_ROUTE).as_deref(),
        Some("Externe Konten öffnen"),
    );
    crate::i18n::CATALOG.set_locale("en-US");
}

/// A window that speaks German for the length of one test, then returns to
/// the language the rest of the suite pins.
struct GermanWindow {
    before: String,
}

impl GermanWindow {
    fn new() -> Self {
        let before = CATALOG.current_locale();
        CATALOG.set_locale("de-DE");
        Self { before }
    }
}

impl Drop for GermanWindow {
    fn drop(&mut self) {
        CATALOG.set_locale(&self.before);
    }
}

/// A German Mail says its notices in German, and every notice still reads the
/// way it was chosen: translating the words never turns a failure into a
/// success.
#[test]
fn a_german_window_says_its_notices_in_german() {
    let _german = GermanWindow::new();

    let failure = engine_error_text("fetch", "authentication failed", "anna@example.test");
    assert_eq!(failure.kind, ToastKind::Error, "{}", failure.text);
    assert!(
        failure.text.contains("Anmeldung bei anna@example.test"),
        "the account that failed is named in German: {}",
        failure.text
    );

    let offline = engine_error_text("fetch", "network unreachable", "anna@example.test");
    assert_eq!(offline.kind, ToastKind::Warning, "{}", offline.text);
    assert!(
        offline.text.starts_with("Du bist offline"),
        "{}",
        offline.text
    );

    let (mut app, _doc) = app();
    app.shared.borrow_mut().status = Some(Notice::warning(CATALOG.t("mail-status-message-gone")));
    app.show_status();
    assert_eq!(
        app.nav.toast_text(),
        Some("Diese Nachricht ist nicht mehr da")
    );

    // The words a person moves a message with follow the same language.
    assert_eq!(
        crate::state::moved_to(Folder::Inbox),
        "Eine Nachricht nach Posteingang verschoben"
    );
}
