//! A field edit's published schema says every argument each operation
//! reads, so an agent building an edit from the schema alone sends what the
//! edit needs.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use serde_json::{Value, json};
use tilo_resource::{Collection, Edit, Invoke, Model, OP_ARGUMENTS, Replica, resource};

/// A value of the argument `argument`, as the operation's JSON form reads it.
fn value_of(op: &str, argument: &str) -> Value {
    match (op, argument) {
        ("splice" | "insert-row" | "insert-column", "at") | (_, "delete" | "by") => json!(0),
        (_, "at") => json!("end"),
        (_, "match" | "outcome" | "rich") => json!({}),
        _ => json!("x"),
    }
}

fn edit(op: &str, arguments: &[&str]) -> Value {
    let mut edit = json!({ "path": [{ "field": "f" }], "op": op });
    for argument in arguments {
        edit[*argument] = value_of(op, argument);
    }
    edit
}

#[test]
fn op_arguments_match_what_an_edit_reads() {
    for (op, required, optional) in OP_ARGUMENTS {
        Edit::<()>::from_json(edit(op, required))
            .unwrap_or_else(|error| panic!("{op} with {required:?} is refused: {error}"));
        let all: Vec<&str> = required.iter().chain(optional.iter()).copied().collect();
        Edit::<()>::from_json(edit(op, &all))
            .unwrap_or_else(|error| panic!("{op} with {all:?} is refused: {error}"));
        for missing in *required {
            let fewer: Vec<&str> = required
                .iter()
                .copied()
                .filter(|argument| argument != missing)
                .collect();
            let error = Edit::<()>::from_json(edit(op, &fewer))
                .err()
                .unwrap_or_else(|| panic!("{op} without {missing} is read"));
            assert!(error.contains("missing field"), "{op}: {error}");
        }
    }
}

#[test]
fn the_published_schema_requires_each_operations_arguments() {
    let schema = <Edit<()> as tilo_resource::JsonSchema>::json_schema();
    for (op, required, _) in OP_ARGUMENTS {
        let edit = edit(op, required);
        tilo_actions::validate_json_schema(&edit, &schema)
            .unwrap_or_else(|reason| panic!("{edit} is refused: {reason}"));
        for missing in *required {
            let mut fewer = edit.clone();
            fewer.as_object_mut().unwrap().remove(*missing);
            assert!(
                tilo_actions::validate_json_schema(&fewer, &schema).is_err(),
                "the schema takes {op} without {missing}"
            );
        }
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Row {
    #[model(label)]
    pub name: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Ledger {
    pub title: String,
    pub rows: Collection<Row>,
}

#[resource(kind = "test.ledger", catalogue = "tests/locales")]
impl Ledger {
    /// Name the ledger.
    #[action]
    pub fn name(&mut self, title: String) {
        self.title = title;
    }
}

#[test]
fn every_published_action_says_what_it_reads() {
    let replica = Replica::<Ledger>::new();
    let unstated =
        tilo_resource::schema_check::unstated_arguments::<Ledger>(&replica.state().unwrap());
    assert!(unstated.is_empty(), "{unstated:#?}");
}

#[test]
fn removing_an_item_by_the_published_listing_works() {
    let mut replica = Replica::<Ledger>::new();
    let published = replica.published().unwrap();
    let edit_spec = published
        .actions
        .iter()
        .find(|spec| spec.id == tilo_resource::FIELD_EDIT)
        .unwrap();
    let listing = edit_spec.params()[0]
        .schema
        .as_deref()
        .unwrap()
        .get("description")
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    assert!(listing.contains("remove-item {id}"), "{listing}");
    replica
        .apply(
            Invoke::edit(
                &Edit::from_json(json!({
                    "path": [{ "field": "rows" }],
                    "op": "insert-item",
                    "id": "r1",
                    "value": { "name": "One" },
                }))
                .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    replica
        .apply(
            Invoke::edit(
                &Edit::from_json(json!({
                    "path": [{ "field": "rows" }],
                    "op": "remove-item",
                    "id": "r1",
                }))
                .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    assert!(replica.state().unwrap().rows.is_empty());
}
