//! End-to-end persistence proofs — the real "control → shared JSON store" path,
//! driven through each pane's production `sync` and read back from disk. The
//! panes resolve the shared settings from the device data home (`XDG_DATA_HOME`), so the environment
//! is the interface here: each test points it at a fresh tree through
//! `tilo_test_env::TestHome`, which serialises the tests on one lock and
//! restores the prior environment afterwards, also when an assertion fails.

use settings::accounts::{AccountDirectory, AccountKind, AccountRoot};

struct AcceptAccount;

impl settings::account_probe::AccountTester for AcceptAccount {
    fn test(
        &self,
        _account: &accounts::Account,
        _secret: &accounts::Secret,
    ) -> Result<String, String> {
        Ok("signed in".into())
    }
}

#[test]
fn local_prefs_and_accounts_persist_to_disk() {
    // The External Accounts pane's Add flow writes a real account into the shared
    // canonical accounts store (the same store Mail reads), and Remove deletes it
    // — the full load/edit/save path through the pane, not just the store unit
    // test.
    {
        let config = tempfile::tempdir().unwrap();
        let config_root = config.path().join("tilo");
        let mut window = ui::framework::testing::TestApp::<settings::SettingsShell>::new()
            .expect("settings opens headlessly");
        let record = window
            .shared_workspace::<accounts::person::AccountList>(accounts::person::ACCOUNTS_OWNER)
            .expect("Settings keeps the workspace's accounts");
        let account_root = AccountRoot::at(&config_root).with_person(record.clone());

        assert!(AccountDirectory::load_in(&account_root).is_empty());

        let mut pane = settings::panes::accounts::AccountsPane::in_root(account_root.clone())
            .with_account_tester(std::sync::Arc::new(AcceptAccount));

        pane.command(settings::panes::accounts::AccountViewCommand::Add(
            AccountKind::Mail,
        ));
        pane.set_form_fields_for_test("Test User", "test@gmail.com", "test@gmail.com", "");
        pane.set_form_password_for_test("s3cret-pw");
        pane.submit_form_for_test();
        assert!(
            pane.finish_form_submission_for_test(tilo_test_env::hang_bound()),
            "bounded account probe and disk transaction complete"
        );

        let dir = AccountDirectory::load_in(&account_root);
        let views = dir.views_of(AccountKind::Mail);
        assert_eq!(
            views.len(),
            1,
            "Add persisted a real account to the shared store"
        );
        assert_eq!(views[0].id, "test@gmail.com");
        assert_eq!(
            dir.mail_config("test@gmail.com")
                .map(|config| config.imap_host),
            Some("imap.gmail.com".into()),
            "IMAP host was auto-configured from the address"
        );
        let id = views[0].id.clone();

        // The password persisted to the encrypted vault (not the plaintext store)…
        let account_id = accounts::AccountId::from(id.clone());
        assert_eq!(
            account_root.vault().get(&account_id).map(|s| s.password),
            Some("s3cret-pw".to_string()),
            "the password was sealed in the vault, keyed by account id"
        );
        // …and never reached the workspace's accounts record.
        let store_json = serde_json::to_string(&account_root.store().all()).unwrap();
        assert!(
            !store_json.contains("s3cret-pw"),
            "the password must not appear in the workspace's accounts"
        );
        let synced = record
            .get()
            .reporting(
                &id,
                "this-device",
                accounts::SyncHealth::Healthy,
                1,
                false,
                "Up to date",
            )
            .expect("the account is the workspace's")
            .expect("its first sync is news");
        record.invoke(&synced).expect("seed sync health");

        pane.remove(&id).unwrap();

        assert!(
            AccountDirectory::load_in(&account_root)
                .views_of(AccountKind::Mail)
                .is_empty(),
            "Remove deleted the account from the shared store on disk"
        );
        assert!(
            account_root.vault().get(&account_id).is_none(),
            "Remove also purged the account's vault secret"
        );
        assert!(
            record.get().sync_of(&id).is_none(),
            "Remove also dropped how the account synced"
        );
    }
}
