use mail::model::{Addr, Draft, Folder};
use mail::transport::{build_mime, parse_addr_list, parse_eml};

#[test]
fn quoted_names_keep_their_punctuation_and_recipient_count() {
    let recipients = "\"Doe, Jane; PhD\" <jane@example.test>; bob@example.test";
    let parsed = parse_addr_list(recipients);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].name.as_deref(), Some("Doe, Jane; PhD"));
    assert_eq!(parsed[0].email, "jane@example.test");
    assert_eq!(parsed[1].email, "bob@example.test");

    let draft = Draft {
        to: recipients.into(),
        cc: "\"Li, Mei\" <mei@example.test>".into(),
        ..Default::default()
    };
    let bytes = build_mime(
        &tilo_content::ContentStore::memory(),
        &Addr::bare("sender@example.test"),
        &draft,
    )
    .unwrap();
    let message = parse_eml(&bytes, 0, Folder::Sent).unwrap();
    assert_eq!(message.to.len(), 2);
    assert_eq!(message.to[0].name.as_deref(), Some("Doe, Jane; PhD"));
    assert_eq!(message.to[0].email, "jane@example.test");
    assert_eq!(message.to[1].email, "bob@example.test");
    assert_eq!(message.cc.len(), 1);
    assert_eq!(message.cc[0].name.as_deref(), Some("Li, Mei"));
}

#[test]
fn escaped_quotes_and_unicode_survive_recipient_parsing() {
    let parsed = parse_addr_list("\"Méi \\\"MJ\\\", Li\" <mei@example.test>, next@example.test");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].name.as_deref(), Some("Méi \"MJ\", Li"));
    assert_eq!(parsed[0].email, "mei@example.test");
    assert_eq!(parsed[1].email, "next@example.test");
}
