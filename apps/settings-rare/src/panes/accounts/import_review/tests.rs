use super::*;
use accounts::thunderbird::{Endpoint, Transport};

fn candidate(email: &str) -> Candidate {
    Candidate {
        source_account: "account1".into(),
        source_identity: "id1".into(),
        email: email.into(),
        display_name: "Example".into(),
        incoming: Endpoint {
            host: "imap.example.org".into(),
            port: 993,
            username: "mailbox-login".into(),
            transport: Transport::Tls,
            authentication: None,
        },
        outgoing: Endpoint {
            host: "smtp.example.org".into(),
            port: 465,
            username: "mailbox-login".into(),
            transport: Transport::Tls,
            authentication: None,
        },
    }
}

fn review(root: &AccountRoot, source: Candidate) -> ImportReview {
    let mut review = ImportReview::default();
    let revision = review.begin(root).unwrap();
    review.receive(revision, Ok(vec![source])).unwrap();
    review
}

#[test]
fn reviewed_metadata_is_pending_idempotent_and_leaves_orphan_credentials_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let root = AccountRoot::at(temp.path());
    let id = accounts::AccountId::from("person@example.org");
    let mut vault = root.vault();
    vault.put(&id, &accounts::Secret::password("fixture-orphan-secret"));
    vault.try_save().unwrap();
    let mut review = review(&root, candidate(id.as_str()));
    assert!(review.choices.first().unwrap().authentication.is_none());
    assert!(review.commit(&root, review.revision).is_err());
    review
        .select("account1:id1", "password", true, review.revision)
        .unwrap();
    review.commit(&root, review.revision).unwrap();
    let store = root.store();
    assert!(
        store.list(&"email".into()).is_empty(),
        "old Mail readers cannot activate pending metadata"
    );
    let account = store.get(&id).unwrap();
    assert_eq!(
        account.type_id.as_str(),
        accounts::thunderbird::PENDING_TYPE
    );
    let config = account
        .config_as::<accounts::email::AccountConfig>()
        .unwrap();
    assert!(config.import_pending);
    assert_eq!(config.username, "mailbox-login");
    assert_eq!(config.smtp_host, "smtp.example.org");
    assert_eq!(
        root.vault().get(&id).unwrap().password,
        "fixture-orphan-secret"
    );
    let before = std::fs::read(root.path().unwrap().join("accounts/store.json")).unwrap();
    assert!(!String::from_utf8_lossy(&before).contains("fixture-orphan-secret"));
    review.commit(&root, review.revision).unwrap();
    assert_eq!(
        std::fs::read(root.path().unwrap().join("accounts/store.json")).unwrap(),
        before
    );
}

#[test]
fn existing_configured_accounts_are_not_overwritten_and_cancel_fences_discovery() {
    let temp = tempfile::tempdir().unwrap();
    let root = AccountRoot::at(temp.path());
    let mut original = accounts::Account::new("person@example.org", "email");
    original.display_name = "Existing owner label".into();
    original.config = serde_json::json!({"owner_extension": "preserve-me"});
    let mut store = root.store();
    store.upsert(original.clone());
    root.save_store(&store).unwrap();
    let mut review = review(&root, candidate("person@example.org"));
    let old = review.revision;
    review
        .select("account1:id1", "password", true, old)
        .unwrap();
    assert!(
        review.commit(&root, old).is_err(),
        "stale choice cannot commit"
    );
    review.commit(&root, review.revision).unwrap();
    assert_eq!(root.store().get(&original.id), Some(&original));
    let pending = review.begin(&root).unwrap();
    review.cancel().unwrap();
    assert!(
        review
            .receive(pending, Ok(vec![candidate("other@example.org")]))
            .is_err()
    );
    assert!(review.choices.is_empty());
    assert_eq!(root.store().len(), 1);
}

#[test]
fn an_account_change_during_review_refuses_the_import() {
    let temp = tempfile::tempdir().unwrap();
    let root = AccountRoot::at(temp.path());
    let mut review = review(&root, candidate("person@example.org"));
    review
        .select("account1:id1", "password", true, review.revision)
        .unwrap();
    let mut store = root.store();
    store.upsert(accounts::Account::new("newer", "caldav"));
    root.save_store(&store).unwrap();
    assert!(review.commit(&root, review.revision).is_err());
    assert_eq!(root.store().len(), 1);
    assert!(root.store().get(&"person@example.org".into()).is_none());
}

#[test]
fn pending_password_settings_activate_only_after_the_existing_secure_save() {
    #[derive(Default)]
    struct AcceptFresh(std::sync::Mutex<Vec<(accounts::Account, accounts::Secret)>>);
    impl crate::account_probe::AccountTester for AcceptFresh {
        fn test(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
        ) -> Result<String, String> {
            self.0
                .lock()
                .map_err(|_| "fixture record unavailable".to_owned())?
                .push((account.clone(), secret.clone()));
            Ok("fixture accepted".into())
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = AccountRoot::at(temp.path());
    let mut review = review(&root, candidate("person@example.org"));
    review
        .select("account1:id1", "password", true, review.revision)
        .unwrap();
    review.commit(&root, review.revision).unwrap();
    let tester = std::sync::Arc::new(AcceptFresh::default());
    let mut pane =
        super::super::AccountsPane::in_root(root.clone()).with_account_tester(tester.clone());
    pane.command(super::super::AccountViewCommand::Reauthenticate(
        "person@example.org".into(),
    ));
    assert!(pane.form_open());
    pane.submit_form_for_test();
    assert_eq!(
        root.store()
            .get(&"person@example.org".into())
            .unwrap()
            .type_id
            .as_str(),
        accounts::thunderbird::PENDING_TYPE
    );
    assert!(tester.0.lock().unwrap().is_empty());
    pane.set_form_password_for_test("fixture-fresh-password");
    pane.submit_form_for_test();
    assert!(pane.finish_form_submission_for_test());
    let calls = tester.0.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    let (account, secret) = &calls[0];
    assert_eq!(account.type_id.as_str(), "email");
    let config = account
        .config_as::<accounts::email::AccountConfig>()
        .unwrap();
    assert!(!config.import_pending);
    assert_eq!(config.username, "mailbox-login");
    assert_eq!(config.imap_host, "imap.example.org");
    assert_eq!(config.smtp_host, "smtp.example.org");
    assert_eq!(secret.password, "fixture-fresh-password");
    let store = root.store();
    let saved = store.get(&"person@example.org".into()).unwrap();
    assert_eq!(saved.type_id.as_str(), "email");
    assert!(
        !saved
            .config_as::<accounts::email::AccountConfig>()
            .unwrap()
            .import_pending
    );
    assert_eq!(
        root.vault().get(&saved.id).unwrap().password,
        "fixture-fresh-password"
    );
}
