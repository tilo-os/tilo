use super::*;
use accounts::SecretVault;

#[test]
fn pending_metadata_is_visible_without_hydrating_or_changing_an_existing_secret() {
    let temp = tempfile::tempdir().unwrap();
    let id = AccountId::from("person@example.org");
    let mut vault = SecretVault::open_in(temp.path());
    vault.put(&id, &accounts::Secret::password("fixture-existing-secret"));
    vault.try_save().unwrap();
    let mut shared = SharedStore::new();
    let mut stored = StoredAccount::new(id.clone(), accounts::thunderbird::PENDING_TYPE);
    stored.set_config(&AccountConfig {
        display_name: "Imported Reader".into(),
        email: id.as_str().into(),
        username: "mailbox-login".into(),
        imap_host: "imap.example.org".into(),
        smtp_host: "smtp.example.org".into(),
        import_pending: true,
        ..Default::default()
    });
    shared.upsert(stored);
    shared.save_in(temp.path()).unwrap();
    let native =
        AccountStore::from_store(&shared, &AccountCredentials::open_in(temp.path()).unwrap());
    assert_eq!(native.accounts.len(), 1);
    assert!(native.accounts.first().unwrap().config.password.is_empty());
    assert!(native.accounts.first().unwrap().config.import_pending);
    native.persist_in(temp.path()).unwrap();
    let reopened = AccountStore::load_in(temp.path());
    assert_eq!(reopened.accounts.len(), 1);
    assert!(reopened.accounts.first().unwrap().config.import_pending);
    assert_eq!(
        SharedStore::load_in(temp.path())
            .get(&id)
            .unwrap()
            .display_name,
        "Imported Reader",
        "Mail saves the shared AccountConfig label for pending accounts"
    );
    assert!(
        SharedStore::load_in(temp.path())
            .list(&TYPE_ID.into())
            .is_empty()
    );
    assert_eq!(
        SecretVault::open_in(temp.path()).get(&id).unwrap().password,
        "fixture-existing-secret"
    );
}
