use super::*;
use ui::tilo_resource::Replica;

#[test]
fn a_pending_import_shows_its_metadata_and_stays_pending_once_mail_keeps_it() {
    let id = AccountId::from("person@example.org");
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
    let native = AccountStore::from_shared(&shared);
    assert_eq!(native.accounts.len(), 1);
    assert!(native.accounts.first().unwrap().config.password.is_empty());
    assert!(native.accounts.first().unwrap().config.import_pending);

    let mut record = Replica::<PersonAccounts>::new();
    let (edits, _) = native.edits_for(&record.state().unwrap());
    for edit in edits {
        record.apply(edit).unwrap();
    }
    let kept = record.state().unwrap().store();
    let reopened = AccountStore::from_shared(&kept);
    assert_eq!(reopened.accounts.len(), 1);
    assert!(reopened.accounts.first().unwrap().config.import_pending);
    assert_eq!(
        kept.get(&id).unwrap().display_name,
        "Imported Reader",
        "Mail keeps the pending account's label"
    );
    assert!(
        kept.list(&TYPE_ID.into()).is_empty(),
        "a pending import is not a mail account until its person signs it in"
    );
}
