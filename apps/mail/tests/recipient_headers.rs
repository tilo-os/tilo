use mail::model::{Addr, Draft, Folder};
use mail::transport::{build_mime, parse_addr_list, parse_eml};

tilo_test_env::scenario! {
    /// `parse_addr_list` over the recipient fields a person writes.
    fn recipient_fields_parse_names_quotes_and_counts;

    phase quoted_names_keep_their_punctuation_and_recipient_count;
    {
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

    phase escaped_quotes_and_unicode_survive_recipient_parsing;
    {
        let parsed = parse_addr_list("\"Méi \\\"MJ\\\", Li\" <mei@example.test>, next@example.test");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name.as_deref(), Some("Méi \"MJ\", Li"));
        assert_eq!(parsed[0].email, "mei@example.test");
        assert_eq!(parsed[1].email, "next@example.test");
    }

    // A recipient field as a person leaves it: empty, blank, one address, and
    // with the separator a completion leaves after the last one.
    phase a_field_names_as_many_recipients_as_it_holds_addresses;
    {
        let cases: [(&str, &[&str]); 5] = [
            ("", &[]),
            ("   ", &[]),
            ("solo@example.test", &["solo@example.test"]),
            ("Maya Chen <maya@example.test>, ", &["maya@example.test"]),
            (
                "maya@example.test, solo@example.test, ",
                &["maya@example.test", "solo@example.test"],
            ),
        ];
        for (field, expected) in cases {
            let emails: Vec<String> = parse_addr_list(field)
                .into_iter()
                .map(|recipient| recipient.email)
                .collect();
            assert_eq!(emails, expected, "{field:?}");
        }
    }
}
