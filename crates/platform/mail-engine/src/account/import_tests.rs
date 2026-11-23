use super::*;

#[test]
fn a_pending_import_shows_its_metadata_and_stays_pending_once_mail_keeps_it() {
    use accounts::person::PersonAccounts;
    let id = AccountId::from("person@example.org");
    let config = AccountConfig {
        display_name: "Imported Reader".into(),
        email: id.as_str().into(),
        username: "mailbox-login".into(),
        imap_host: "imap.example.org".into(),
        smtp_host: "smtp.example.org".into(),
        import_pending: true,
        ..Default::default()
    };
    // The record keeps the account without a name of its own: Mail saves it.
    let mut stored = StoredAccount::new(id.clone(), accounts::thunderbird::PENDING_TYPE);
    stored.set_config(&config);
    let mut record = tilo_resource::Replica::<PersonAccounts>::new();
    record.apply(PersonAccounts::saving(&stored)).unwrap();
    let person = record.state().unwrap();

    let native = accounts_of(&person);
    assert_eq!(native.len(), 1);
    assert!(native.first().unwrap().config.password.is_empty());
    assert!(native.first().unwrap().config.import_pending);

    // Mail saves the record's label for the pending account, and removes nothing.
    let (edits, removed) = edits_for(&native, &person);
    assert!(removed.is_empty());
    assert_eq!(edits.len(), 1, "the name is what the record lacks");
    for edit in edits {
        record.apply(edit).unwrap();
    }
    let kept = record
        .state()
        .unwrap()
        .account(id.as_str())
        .expect("the record keeps the account");
    assert_eq!(
        kept.display_name, "Imported Reader",
        "Mail saves the record's label for pending accounts"
    );
}
