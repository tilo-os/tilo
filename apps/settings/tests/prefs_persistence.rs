//! End-to-end persistence proofs — the real "control → shared JSON store"
//! path, driven through each pane's production `sync` and read back from
//! disk.
//!
//! The panes resolve the shared `settings.json` from `XDG_CONFIG_HOME`, so the
//! environment is the interface here: each test points it at a fresh tree
//! through `tilo_test_env::TestHome`, which serialises the tests on one lock
//! and restores the prior environment afterwards, also when an assertion
//! fails. The accounts test needs no environment at all; it hands the pane an
//! explicit config root.

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

/// Accounts persist.
#[test]
fn local_prefs_and_accounts_persist_to_disk() {
    // Case `accounts_add_and_remove_round_trip_through_the_pane`.
    // The External Accounts pane's Add flow writes a real account into the shared
    // canonical accounts store (the same store Mail reads), and Remove deletes it
    // — the full load/edit/save path through the pane, not just the store unit
    // test.
    {
        let config = tempfile::tempdir().unwrap();
        let config_root = config.path().join("tilo");
        let account_root = AccountRoot::at(&config_root);

        // Fresh install → empty stores.
        assert!(AccountDirectory::load_in(&account_root).is_empty());

        let mut pane = settings::panes::accounts::AccountsPane::in_root(account_root.clone())
            .with_account_tester(std::sync::Arc::new(AcceptAccount));

        // Open the Add-email form, fill it, and Save.
        pane.command(settings::panes::accounts::AccountViewCommand::Add(
            AccountKind::Mail,
        ));
        pane.set_form_fields_for_test("Test User", "test@gmail.com", "test@gmail.com", "");
        pane.set_form_password_for_test("s3cret-pw");
        pane.submit_form_for_test();
        assert!(
            pane.finish_form_submission_for_test(),
            "bounded account probe and disk transaction complete"
        );

        // The account is now in the shared on-disk store Mail loads, auto-configured.
        let dir = AccountDirectory::load_in(&account_root);
        let views = dir.views_of(AccountKind::Mail);
        assert_eq!(
            views.len(),
            1,
            "Add persisted a real account to the shared store"
        );
        assert_eq!(views[0].id, "test@gmail.com");
        assert_eq!(
            dir.mail.accounts[0].config.imap_host, "imap.gmail.com",
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
        // …and never leaked into the plaintext account store on disk.
        let store_json = std::fs::read_to_string(accounts::store_path_in(&config_root))
            .expect("the shared store was written");
        assert!(
            !store_json.contains("s3cret-pw"),
            "the password must not appear in the plaintext store JSON"
        );
        account_root
            .sync_reporter(account_id.clone())
            .succeeded()
            .expect("seed sync health");

        // Remove it, as the confirmed Remove effect does.
        pane.remove(&id).unwrap();

        // The directory now reports empty for the Email kind — the account is gone
        // from the shared store on disk.
        assert!(
            AccountDirectory::load_in(&account_root)
                .views_of(AccountKind::Mail)
                .is_empty(),
            "Remove deleted the account from the shared store on disk"
        );
        // …and its sealed credential was dropped from the vault too.
        assert!(
            account_root.vault().get(&account_id).is_none(),
            "Remove also purged the account's vault secret"
        );
        assert!(
            account_root.statuses().get(&account_id).is_none(),
            "Remove also purged the account's local sync-health record"
        );
    }
}
