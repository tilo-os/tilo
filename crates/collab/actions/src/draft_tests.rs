use serde_json::json;

use super::*;
use crate::{ActionAddress, ActionContract, ActionParam, ExecutionLocus, InputSpec, OperationKind};

fn descriptor(action_label: &str, params: Vec<ActionParam>) -> ActionDescriptor {
    ActionDescriptor {
        semantic_identity: None,
        address: ActionAddress::new("inbox", "do"),
        resource_kind: "test.kind".into(),
        resource_label: "Inbox".into(),
        action_label: action_label.into(),
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

#[test]
fn a_message_parameter_is_previewed_as_a_message() {
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

#[test]
fn any_action_is_drafted_from_its_parameters() {
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

#[test]
fn edits_change_only_editable_fields_and_keep_their_types() {
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

#[test]
fn the_digest_ignores_key_order_but_not_values() {
    let one = args(json!({"a": 1, "b": {"x": "1", "y": [1, 2]}}));
    let two = args(json!({"b": {"y": [1, 2], "x": "1"}, "a": 1}));
    let three = args(json!({"b": {"y": [2, 1], "x": "1"}, "a": 1}));
    assert_eq!(arguments_digest(&one), arguments_digest(&two));
    assert_ne!(arguments_digest(&one), arguments_digest(&three));
}

#[test]
fn a_tag_opts_an_action_into_the_message_preview() {
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
