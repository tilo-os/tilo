use serde_json::json;

use super::*;
use crate::{ActionAddress, ActionContract, ActionParam, ExecutionLocus, InputSpec, OperationKind};

fn descriptor(action_label: &str, params: Vec<ActionParam>) -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        state: crate::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new("inbox", "do"),
        resource_kind: "test.kind".into(),
        resource_label: "Inbox".into(),
        action_label: action_label.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Document,
        contract: ActionContract::default(),
        input: InputSpec::None,
        params,
        view: None,
    }
}

fn send() -> ActionDescriptor {
    descriptor(
        "Send message",
        vec![
            ActionParam::required("account", ParamType::Text, "Sending account"),
            ActionParam::required("message", ParamType::Object, "The message").with_schema(json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string"}, "cc": {"type": "string"},
                    "subject": {"type": "string"}, "body": {"type": "string"},
                    "attachments": {"type": "array"}
                }
            })),
        ],
    )
}

#[allow(clippy::needless_pass_by_value, reason = "tests pass json! literals")]
fn args(value: serde_json::Value) -> Map<String, Value> {
    value.as_object().cloned().expect("an object")
}

fn mail_arguments() -> Map<String, Value> {
    args(json!({
        "account": "me@example.org",
        "message": {
            "to": "bob@example.org", "cc": "", "subject": "Note X",
            "body": "Hello Bob,\nhere is the note.",
            "attachments": [{"name": "a.pdf"}, {"name": "b.md"}, {"path": "/notes/c.png"}]
        }
    }))
}

tilo_test_env::scenario! {
    fn a_message_is_previewed_as_a_message;

    phase a_message_parameter_is_previewed_as_a_message;
    {
        let draft = Draft::render(&send(), &mail_arguments());
        assert_eq!(draft.layout, DraftLayout::Message);
        assert_eq!(draft.action_label, "Send message");
        let to = draft.role(MessageRole::To).expect("recipients");
        assert_eq!(to.value.line(), "bob@example.org");
        assert!(to.editable);
        assert_eq!(to.path, ["message", "to"]);
        let body = draft.role(MessageRole::Body).expect("body");
        assert!(body.multiline && body.editable);
        assert_eq!(
            draft
                .role(MessageRole::Attachments)
                .map(|field| &field.value),
            Some(&DraftValue::List {
                count: 3,
                labels: vec!["a.pdf".into(), "b.md".into(), "c.png".into()],
            })
        );
        // An empty value is not shown.
        assert!(draft.role(MessageRole::Cc).is_none());
        assert_eq!(draft.gist().as_deref(), Some("bob@example.org · Note X"));
    }

    phase a_tag_opts_an_action_into_the_message_preview;
    {
        let mut post = descriptor(
            "Post",
            vec![
                ActionParam::required("channel", ParamType::Text, "Channel"),
                ActionParam::required("text", ParamType::Text, "Text"),
            ],
        );
        let arguments = args(json!({"channel": "general", "text": "Hi all"}));
        assert_eq!(Draft::render(&post, &arguments).layout, DraftLayout::Fields);
        post.contract.tags.push(MESSAGE_PREVIEW_TAG.into());
        let draft = Draft::render(&post, &arguments);
        assert_eq!(draft.layout, DraftLayout::Message);
        assert_eq!(
            draft
                .role(MessageRole::Body)
                .map(|field| field.value.line()),
            Some("Hi all".into())
        );
    }
}

tilo_test_env::scenario! {
    fn a_draft_shows_and_returns_an_actions_arguments;

    phase any_action_is_drafted_from_its_parameters;
    {
        let delete = descriptor(
            "Delete permanently",
            vec![
                ActionParam::required("paths", ParamType::Array, "Files"),
                ActionParam::optional("keep_versions", ParamType::Boolean, "Keep versions"),
                ActionParam::required("target", ParamType::Resource, "Folder"),
                ActionParam::optional("token", ParamType::Secret, "Token"),
            ],
        );
        let draft = Draft::render(
            &delete,
            &args(json!({
                "paths": ["/home/a/report.pdf", "/home/a/old.md"],
                "keep_versions": false,
                "target": {"label": "Projects", "id": "f1"},
                "token": "s3cret"
            })),
        );
        assert_eq!(draft.layout, DraftLayout::Fields);
        let labels: Vec<&str> = draft
            .fields
            .iter()
            .map(|field| field.label.as_str())
            .collect();
        assert_eq!(labels, ["Paths", "Keep versions", "Target", "Token"]);
        assert_eq!(draft.fields[0].value.line(), "report.pdf, old.md");
        assert_eq!(
            draft.fields[2].value,
            DraftValue::Resource {
                label: "Projects".into()
            }
        );
        assert!(!draft.fields[2].editable);
        assert_eq!(draft.fields[3].value, DraftValue::Withheld);
        assert!(
            !serde_json::to_string(&draft).unwrap().contains("s3cret"),
            "a secret never enters the draft"
        );
        assert!(draft.fields.iter().all(|field| field.role.is_none()));
    }

    phase a_summary_names_each_argument_in_a_line_and_withholds_secrets;
    // A reviewer reads each argument in one line by its label: a long value
    // cut short, a secret withheld.
    {
        let submit = descriptor(
            "Submit the application",
            vec![
                ActionParam::required("name", ParamType::Text, "Applicant"),
                ActionParam::required("passport", ParamType::Text, "Passport number"),
                ActionParam::required("purpose", ParamType::Text, "Why they travel"),
                ActionParam::required("portal_password", ParamType::Secret, "Consulate login"),
            ],
        );
        let summary = Draft::render(
            &submit,
            &args(json!({
                "name": "Jane Doe",
                "passport": "X1234567",
                "purpose": "Attending the\nannual conference of European visa officers in Lisbon, three days",
                "portal_password": "hunter2"
            })),
        )
        .summary();
        assert_eq!(
            summary[..2],
            [
                ("Name".to_owned(), "Jane Doe".to_owned()),
                ("Passport".to_owned(), "X1234567".to_owned()),
            ]
        );
        assert_eq!(summary[2].0, "Purpose");
        assert!(
            summary[2].1.starts_with("Attending the annual"),
            "{summary:?}"
        );
        assert!(summary[2].1.ends_with('…'));
        assert!(summary[2].1.chars().count() <= SUMMARY_CHARS + 1);
        assert_eq!(summary[3], ("Portal password".to_owned(), "•••".to_owned()));
        assert!(!format!("{summary:?}").contains("hunter2"));
    }

    phase password_fields_in_object_parameters_are_withheld;
    {
        let action = descriptor(
            "Connect",
            vec![
                ActionParam::required("account", ParamType::Object, "Account").with_schema(json!({
                    "type": "object",
                    "properties": {"token": {"type": "string", "format": "password"}}
                })),
            ],
        );
        let arguments = args(json!({"account": {"token": "private-token"}}));
        let draft = Draft::render(&action, &arguments);
        assert_eq!(draft.fields[0].value, DraftValue::Withheld);
        assert!(!draft.fields[0].editable);
        assert!(
            !serde_json::to_string(&draft)
                .unwrap()
                .contains("private-token")
        );
        assert!(
            draft
                .apply_edits(
                    &arguments,
                    &[DraftEdit {
                        path: vec!["account".into(), "token".into()],
                        text: "replacement".into(),
                    }]
                )
                .is_err()
        );
    }

    phase edits_change_only_editable_fields_and_keep_their_types;
    {
        let arguments = mail_arguments();
        let draft = Draft::render(&send(), &arguments);
        let edited = draft
            .apply_edits(
                &arguments,
                &[
                    DraftEdit {
                        path: vec!["message".into(), "to".into()],
                        text: "carol@example.org".into(),
                    },
                    DraftEdit {
                        path: vec!["message".into(), "body".into()],
                        text: "Hi Carol".into(),
                    },
                ],
            )
            .expect("recipients and body are editable");
        assert_eq!(edited["message"]["to"], "carol@example.org");
        assert_eq!(edited["message"]["body"], "Hi Carol");
        assert_eq!(
            edited["message"]["attachments"],
            arguments["message"]["attachments"]
        );
        assert_ne!(arguments_digest(&edited), arguments_digest(&arguments));

        let refused = draft.apply_edits(
            &arguments,
            &[DraftEdit {
                path: vec!["message".into(), "attachments".into()],
                text: "x".into(),
            }],
        );
        assert!(refused.is_err(), "a list is not edited as text");
        let unknown = draft.apply_edits(
            &arguments,
            &[DraftEdit {
                path: vec!["message".into(), "bcc".into()],
                text: "eve@example.org".into(),
            }],
        );
        assert!(
            unknown.is_err(),
            "an edit cannot add a field the draft does not show"
        );
    }

    phase the_digest_ignores_key_order_but_not_values;
    {
        let one = args(json!({"a": 1, "b": {"x": "1", "y": [1, 2]}}));
        let two = args(json!({"b": {"y": [1, 2], "x": "1"}, "a": 1}));
        let three = args(json!({"b": {"y": [2, 1], "x": "1"}, "a": 1}));
        assert_eq!(arguments_digest(&one), arguments_digest(&two));
        assert_ne!(arguments_digest(&one), arguments_digest(&three));
    }

    phase a_draft_gives_back_the_arguments_it_shows_and_no_others;
    {
        let rename = descriptor(
            "Rename an entry here",
            vec![
                ActionParam::required("entry", ParamType::Text, "Entry"),
                ActionParam::required("name", ParamType::Text, "New name"),
                ActionParam::optional("keep", ParamType::Boolean, "Keep a copy"),
                ActionParam::optional("count", ParamType::Integer, "Count"),
            ],
        );
        let arguments = args(json!({
            "entry": "doc (3).txt", "name": "Meeting notes.txt", "keep": false, "count": 3
        }));
        let rebuilt = Draft::render(&rename, &arguments)
            .arguments()
            .expect("all shown");
        assert_eq!(arguments_digest(&rebuilt), arguments_digest(&arguments));

        // A null the draft leaves out rebuilds to other arguments: the digest
        // tells them apart.
        let with_null = args(json!({"entry": "a", "name": "b", "keep": null}));
        let rebuilt = Draft::render(&rename, &with_null)
            .arguments()
            .expect("shown");
        assert_ne!(arguments_digest(&rebuilt), arguments_digest(&with_null));

        // What a draft shows only in part gives back nothing.
        assert!(
            Draft::render(&send(), &mail_arguments())
                .arguments()
                .is_none()
        );
    }

    phase a_choice_value_reads_by_its_published_label;
    // A value that names one of its parameter's choices reads by the label the
    // surface publishes for it: a field id reads as the field's name ("Body"),
    // never the raw `body` the action binds.
    {
        let edit = descriptor(
            "Edit a text field",
            vec![
                ActionParam::required("field", ParamType::Text, "The field id")
                    .with_labelled_choices([("title", "Title"), ("body", "Body")]),
                ActionParam::required("value", ParamType::Text, "The new value"),
            ],
        );
        let draft = Draft::render(&edit, &args(json!({"field": "body", "value": "milk"})));
        let field = draft
            .fields
            .iter()
            .find(|field| field.path == ["field"])
            .expect("the field row");
        assert_eq!(
            field.shows.as_deref(),
            Some("Body"),
            "a field id reads as the field's name, not the id"
        );
        // What runs stays the value the action binds: the draft gives its
        // arguments back exactly, the field id included.
        let rebuilt = Draft::render(&edit, &args(json!({"field": "body", "value": "milk"})))
            .arguments()
            .expect("all shown");
        assert_eq!(
            rebuilt.get("field").and_then(|value| value.as_str()),
            Some("body"),
            "the label is a display, the id is what runs"
        );
    }
}

/// A secret named by reference is reviewed as a secret is: withheld, not
/// editable, and its address appears nowhere in what the reviewer is sent.
#[test]
fn a_secret_named_by_reference_is_withheld_like_a_secret() {
    let adding = descriptor(
        "Add mail account",
        vec![
            ActionParam::required("address", ParamType::Text, "Address"),
            crate::remote::person_vault::item_param(
                "password",
                "The account's password, by reference",
            ),
        ],
    );
    let draft = Draft::render(
        &adding,
        &args(json!({
            "address": "ada@example.test",
            "password": "vault.item:0123456789abcdef0123456789abcdef",
        })),
    );
    assert_eq!(draft.fields.len(), 2);
    assert_eq!(draft.fields[0].value.line(), "ada@example.test");
    assert_eq!(draft.fields[1].label, "Password");
    assert_eq!(draft.fields[1].value, DraftValue::Withheld);
    assert!(!draft.fields[1].editable);
    let sent = serde_json::to_string(&draft).expect("a draft is sent as JSON");
    assert!(!sent.contains("0123456789abcdef"), "{sent}");
    assert!(
        draft
            .summary()
            .iter()
            .all(|(_, value)| !value.contains("vault.item")),
        "{:?}",
        draft.summary()
    );
}

#[test]
fn arguments_digest_pins_canonical_bytes_for_shared_corpus() {
    const CORPUS: &[(&str, &str, &[u8])] = &[
        (
            "nested_unsorted",
            r#"{"z":{"b":2,"a":1},"a":{"d":{"y":true,"x":false},"c":[3,2,1]},"m":0}"#,
            br#"{"a":{"c":[3,2,1],"d":{"x":false,"y":true}},"m":0,"z":{"a":1,"b":2}}"#,
        ),
        (
            "utf8_vs_utf16_key_order",
            r#"{"\uD83D\uDE00":4,"\uE000":3,"é":2,"z":1}"#,
            b"{\"z\":1,\"\xc3\xa9\":2,\"\xee\x80\x80\":3,\"\xf0\x9f\x98\x80\":4}",
        ),
        ("empty_object", "{}", b"{}"),
        ("empty_array", "[]", b"[]"),
        (
            "escaped_and_unicode_string",
            r#""q\"b\\c\u0000\u0001\u0007\u0008\u0009\u000a\u000b\u000c\u000d\u000e\u001f\u007f café 日本語 🚀""#,
            b"\"q\\\"b\\\\c\\u0000\\u0001\\u0007\\b\\t\\n\\u000b\\f\\r\\u000e\\u001f\x7f caf\xc3\xa9 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e \xf0\x9f\x9a\x80\"",
        ),
        (
            "escaped_keys_object",
            r#"{"q\"b\\c\u0000\b\t\n\f\r\u001f\u007f":2,"a":1}"#,
            b"{\"a\":1,\"q\\\"b\\\\c\\u0000\\b\\t\\n\\f\\r\\u001f\x7f\":2}",
        ),
        ("i64_min", "-9223372036854775808", b"-9223372036854775808"),
        ("i64_max", "9223372036854775807", b"9223372036854775807"),
        (
            "u64_above_i64_max",
            "9223372036854775808",
            b"9223372036854775808",
        ),
        ("u64_max", "18446744073709551615", b"18446744073709551615"),
        ("zero", "0", b"0"),
        ("negative_zero", "-0.0", b"-0.0"),
        ("float_whole", "1.0", b"1.0"),
        ("float_tenth", "0.1", b"0.1"),
        ("float_1e20", "1e20", b"1e+20"),
        ("float_1e_neg5", "1e-5", b"0.00001"),
        ("bool_true", "true", b"true"),
        ("bool_false", "false", b"false"),
        ("null", "null", b"null"),
        (
            "mixed_array",
            r#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"b":2,"a":1}]"#,
            br#"[null,true,false,0,-1,1.0,0.1,-0.0,"a\"b",[],{},{"a":1,"b":2}]"#,
        ),
    ];
    const PINNED_NESTED_DIGEST: [u8; 32] = [
        0xbc, 0xc6, 0xf1, 0xbb, 0x4c, 0xe5, 0xa2, 0xb3, 0x58, 0x07, 0x05, 0x0e, 0xdb, 0x5f, 0x5b,
        0x78, 0x6e, 0x3f, 0xd5, 0xe1, 0x18, 0xb0, 0x6b, 0xd9, 0x09, 0xed, 0xb0, 0x87, 0xfd, 0x6e,
        0xf8, 0xd9,
    ];

    for &(name, input, expected) in CORPUS {
        let value: Value = serde_json::from_str(input).unwrap();
        let (map, expected_bytes) = match value {
            Value::Object(map) => (map, expected.to_vec()),
            other => {
                let mut map = Map::new();
                map.insert("v".to_owned(), other);
                let mut bytes = Vec::with_capacity(expected.len().saturating_add(6));
                bytes.extend_from_slice(br#"{"v":"#);
                bytes.extend_from_slice(expected);
                bytes.push(b'}');
                (map, bytes)
            }
        };
        let expected_digest = *blake3::Hasher::new_derive_key("Tilo draft arguments v1")
            .update(&expected_bytes)
            .finalize()
            .as_bytes();
        assert_eq!(arguments_digest(&map), expected_digest, "row {name}");
    }
    let nested = args(serde_json::from_str(CORPUS[0].1).unwrap());
    assert_eq!(arguments_digest(&nested), PINNED_NESTED_DIGEST);
}
