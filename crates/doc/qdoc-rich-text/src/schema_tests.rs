//! The edit vocabulary as agents read it: the `apply` descriptor carries the
//! schema of every [`RichTextAction`], and that schema is what serde reads
//! and writes.

use serde_json::{Map, Value, json};
use tilo_actions::{ActionArgs, ActionDescriptor, ArgError};
use tilo_resource::{APPLY, APPLY_PARAM, GROUP, GROUP_PARAM, JsonSchema};

use super::*;

/// What the host's directory accepts for one action's parameters.
const DIRECTORY_PARAMS_LIMIT: usize = 32 * 1024;

fn schema() -> Value {
    RichTextAction::json_schema()
}

fn variants(schema: &Value) -> Vec<(String, Value)> {
    schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|alternative| {
            let name = alternative["required"][0].as_str().unwrap().to_owned();
            (name.clone(), alternative["properties"][&name].clone())
        })
        .collect()
}

/// A value of `schema`: every field present and every option taken when
/// `full`, only what is required (options `null`) otherwise.
fn example(schema: &Value, full: bool) -> Value {
    if let Some(constant) = schema.get("const") {
        return constant.clone();
    }
    if let Some(values) = schema.get("enum") {
        return values[0].clone();
    }
    if let Some(alternatives) = schema.get("oneOf").and_then(Value::as_array) {
        let non_null = alternatives
            .iter()
            .find(|alternative| alternative["type"] != "null")
            .unwrap();
        if !full
            && alternatives
                .iter()
                .any(|alternative| alternative["type"] == "null")
        {
            return Value::Null;
        }
        return example(non_null, full);
    }
    let types: Vec<&str> = match &schema["type"] {
        Value::String(name) => vec![name.as_str()],
        Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
        _ => return Value::Null,
    };
    if !full && types.contains(&"null") {
        return Value::Null;
    }
    match types.iter().find(|name| **name != "null").copied() {
        Some("object") => {
            let required: Vec<&str> = schema["required"]
                .as_array()
                .map(|names| names.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let mut object = Map::new();
            for (name, property) in schema["properties"].as_object().into_iter().flatten() {
                if full || required.contains(&name.as_str()) {
                    object.insert(name.clone(), example(property, full));
                }
            }
            Value::Object(object)
        }
        Some("array") => json!([example(&schema["items"], full)]),
        Some("string") => json!("x"),
        Some("integer") => json!(2),
        Some("number") => json!(1.5),
        Some("boolean") => json!(true),
        _ => Value::Null,
    }
}

fn published() -> tilo_actions::Resource {
    let replica = tilo_resource::Replica::<RichText>::new();
    tilo_resource::publish::<RichText>("doc", "Doc", &replica.state().unwrap())
}

fn descriptor(action: &str) -> ActionDescriptor {
    tilo_actions::door::survey(&[published()])
        .into_iter()
        .find(|descriptor| descriptor.address.action == action)
        .unwrap()
}

#[test]
fn the_apply_descriptor_names_every_edit_with_its_fields() {
    let apply = descriptor(APPLY);
    let [param] = apply.params.as_slice() else {
        panic!("apply takes one parameter");
    };
    assert_eq!(param.name, APPLY_PARAM);
    assert_eq!(param.schema.as_ref(), Some(&schema()));

    let described: std::collections::BTreeMap<String, Value> =
        variants(&schema()).into_iter().collect();
    for name in [
        "SpliceText",
        "ReplaceSelection",
        "SetMark",
        "SetParagraphStyle",
        "ApplyNamedStyle",
        "SetBlockAttr",
        "SetPageMargins",
        "SetStyleProperty",
        "DeleteStyle",
        "SetImageCrop",
        "SetImageSize",
        "SetImagePlacement",
        "InsertTable",
        "EditTable",
        "InsertImage",
        "InsertPageBreak",
        "SplitBlock",
        "InsertField",
        "GenerateToc",
        "PasteFragment",
        "InsertParagraph",
        "RemoveBlocks",
        "MoveBlocks",
        "ReplaceBody",
        "AcceptChanges",
        "RejectChanges",
        "ResolveComment",
        "AddComment",
        "ReplyToComment",
    ] {
        assert!(described.contains_key(name), "{name} is not described");
    }

    let splice = &described["SpliceText"];
    assert_eq!(
        splice["required"],
        json!(["region", "block", "at", "del", "ins"])
    );
    assert_eq!(splice["additionalProperties"], false);
    assert_eq!(
        splice["properties"]["region"]["enum"],
        json!(["Body", "Header", "Footer"])
    );
    assert_eq!(splice["properties"]["at"]["type"], "integer");
    assert!(
        splice.get("description").is_none(),
        "descriptions sit on the variant alternative"
    );
    assert!(
        schema()["oneOf"][0]["description"]
            .as_str()
            .unwrap()
            .contains("Replace `[at, at+del)` of `block`")
    );
    // Optional fields are not required, and a serde default is not either.
    let set_mark = &described["SetMark"];
    assert_eq!(set_mark["required"], json!(["region", "spans", "name"]));
    assert_eq!(set_mark["properties"]["value"]["oneOf"][0]["type"], "null");
    let paste = &described["PasteFragment"];
    assert_eq!(
        paste["properties"]["blocks"]["items"]["properties"]["marks"]["items"]["required"],
        json!(["start", "end", "name", "value"])
    );

    // The schema reaches callers as part of the descriptor they list.
    let listed = serde_json::to_value(&apply).unwrap();
    assert_eq!(listed["params"][0]["schema"], schema());
    assert!(listed.to_string().contains("\"InsertParagraph\""));
}

#[test]
fn every_edit_serde_writes_is_one_the_schema_accepts() {
    let schema = schema();
    for (name, content) in variants(&schema) {
        for full in [true, false] {
            let written = json!({ name.clone(): example(&content, full) });
            let action: RichTextAction = serde_json::from_value(written.clone())
                .unwrap_or_else(|error| panic!("{written} does not read: {error}"));
            let serialized = serde_json::to_value(&action).unwrap();
            if full {
                assert_eq!(serialized, written, "{name} does not round-trip");
            }
            tilo_actions::validate_json_schema(&serialized, &schema)
                .unwrap_or_else(|reason| panic!("{serialized} is refused: {reason}"));
        }
    }
}

#[test]
fn what_the_editor_writes_is_accepted() {
    let edits = [
        RichTextAction::SpliceText {
            region: action::RegionKind::Header,
            block: "b1".into(),
            at: 3,
            del: 0,
            ins: "héllo".into(),
        },
        RichTextAction::SetMark {
            region: action::RegionKind::Body,
            spans: vec![action::Span {
                block: "b1".into(),
                start: 0,
                end: 4,
            }],
            name: "size".into(),
            value: Some(action::MarkVal::Int(14)),
            also_clear: Vec::new(),
        },
        RichTextAction::SetBlockAttr {
            region: action::RegionKind::Body,
            blocks: vec!["b1".into()],
            key: "indent".into(),
            value: action::AttrVal::Float(0.5),
        },
        RichTextAction::EditTable {
            table: "t".into(),
            edit: action::TableEdit::Delete,
        },
        RichTextAction::SplitBlock {
            region: action::RegionKind::Body,
            block: "b1".into(),
            at: 2,
            kind: "paragraph".into(),
            ordered: None,
            next: None,
        },
        RichTextAction::AcceptChanges { id: None },
    ];
    for edit in edits {
        let value = serde_json::to_value(&edit).unwrap();
        tilo_actions::validate_json_schema(&value, &schema())
            .unwrap_or_else(|reason| panic!("{value} is refused: {reason}"));
    }
}

#[test]
fn a_malformed_edit_is_refused_by_name_before_it_is_built() {
    let apply = descriptor(APPLY);
    let bind = |action: Value| {
        let arguments = json!({ APPLY_PARAM: action });
        ActionArgs::bind_json(&apply.params, arguments.as_object().unwrap())
    };
    assert!(bind(json!({"InsertParagraph": {"after": "b1", "text": "New"}})).is_ok());

    let Err(ArgError::Constraint { reason, .. }) =
        bind(json!({"SpliceText": {"region": "Body", "block": "b1", "at": 0, "ins": "x"}}))
    else {
        panic!("a splice without `del` binds");
    };
    assert!(
        reason.contains("SpliceText") && reason.contains("`del`"),
        "{reason}"
    );

    let Err(ArgError::Constraint { reason, .. }) = bind(json!({"SplitText": {}})) else {
        panic!("an unknown edit binds");
    };
    assert!(
        reason.contains("SpliceText") && reason.contains("SplitBlock"),
        "{reason}"
    );

    let Err(ArgError::Constraint { reason, .. }) =
        bind(json!({"InsertParagraph": {"after": "b1", "text": "New", "style": "quote"}}))
    else {
        panic!("an unknown field binds");
    };
    assert!(reason.contains("`style`"), "{reason}");
}

#[test]
fn group_members_are_described_and_checked_like_apply() {
    let group = descriptor(GROUP);
    let [param] = group.params.as_slice() else {
        panic!("group takes one parameter");
    };
    assert_eq!(param.name, GROUP_PARAM);
    let bind = |actions: Value| {
        let arguments = json!({ GROUP_PARAM: actions });
        ActionArgs::bind_json(&group.params, arguments.as_object().unwrap())
    };
    let insert = json!({"InsertParagraph": {"after": "b1", "text": "New"}});
    assert!(bind(json!([{"action": APPLY, "args": {"action": insert}}])).is_ok());
    let Err(ArgError::Constraint { reason, .. }) = bind(json!([
        {"action": APPLY, "args": {"action": {"RemoveBlocks": {"blocks": "b1"}}}}
    ])) else {
        panic!("a malformed member binds");
    };
    assert!(reason.contains("RemoveBlocks"), "{reason}");
}

#[test]
fn the_described_operations_can_be_published() {
    for descriptor in [descriptor(APPLY), descriptor(GROUP)] {
        for param in &descriptor.params {
            assert_eq!(param.validate_declaration(), Vec::<String>::new());
        }
        let bytes = serde_json::to_string(&descriptor.params).unwrap().len();
        assert!(
            bytes <= DIRECTORY_PARAMS_LIMIT,
            "`{}` parameters are {bytes} bytes",
            descriptor.address.action
        );
    }
    let mut registry = tilo_actions::ActionRegistry::new();
    tilo_resource::group::register::<RichText>(&mut registry);
    assert_eq!(
        tilo_actions::validate_surface_contracts(&[published()], &registry),
        Vec::new()
    );
}
