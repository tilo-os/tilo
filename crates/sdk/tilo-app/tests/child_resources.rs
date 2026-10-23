#![allow(clippy::unwrap_used, reason = "test assertions")]

//! A resource nested in another — here a saved query's SQL, plain text in a
//! text object of the query's document — is edited through a child
//! dispatcher: each keystroke a splice of the child, applied as the parent's
//! action, so the parent's permissions, history and undo hold, and the
//! palette, agents and scripts address the child like any resource.

use tilo_actions::{Action, ActionError};
use tilo_app::Role;
use tilo_app::automerge::transaction::Transactable;
use tilo_app::automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use tilo_app::prelude::*;
use tilo_resource::{Doc, Invoke, ModelError, child_resource_id, publish_child};

const ACTION_TYPE: &str = "child-resources.query";

#[derive(
    Clone,
    Debug,
    tilo_resource::__private::serde::Serialize,
    tilo_resource::__private::serde::Deserialize,
)]
#[serde(crate = "tilo_resource::__private::serde")]
enum QueryAction {
    EditSql(TextEdit),
    Rename(String),
}

fn sql(doc: &Doc) -> Option<ObjId> {
    match doc.get(ROOT, "sql") {
        Ok(Some((Value::Object(ObjType::Text), obj))) => Some(obj),
        _ => None,
    }
}

impl Action for QueryAction {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::EditSql(edit) => {
                let obj = sql(doc).ok_or_else(|| ActionError::Refused("no SQL".into()))?;
                edit.apply_under(doc, &obj)
            }
            Self::Rename(name) => {
                doc.put(ROOT, "name", name.as_str())?;
                Ok(())
            }
        }
    }
}

/// A saved query: a name and its SQL.
struct Query;

impl Resource for Query {
    const KIND: &'static str = "child-resources.query";
    const LABEL: &'static str = "Query";
    const ACTION_TYPE: &'static str = ACTION_TYPE;
    type State = String;
    type Action = QueryAction;

    fn read(doc: &Doc) -> Result<String, ModelError> {
        Ok(match doc.get(ROOT, "name") {
            Ok(Some((Value::Scalar(name), _))) => name.as_str().unwrap_or_default().to_owned(),
            _ => String::new(),
        })
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        doc.put_object(ROOT, "sql", ObjType::Text)?;
        doc.put(ROOT, "name", "untitled")?;
        Ok(())
    }

    fn children(_state: &String) -> Vec<tilo_actions::Resource> {
        vec![publish_child::<PlainText, Self>(
            &sql_id(),
            "SQL",
            None,
            QueryAction::EditSql,
        )]
    }
}

fn sql_id() -> String {
    child_resource_id::<Query>("sql")
}

struct QueryApp;

impl ResourceApp for QueryApp {
    type Resource = Query;
    const ID: &'static str = "so.tilo.test-child-resources";
    const TITLE: &'static str = "Query";

    fn view(cx: &mut ViewCx<Query>) -> Element {
        let text = cx.child::<PlainText>(&sql_id(), QueryAction::EditSql);
        Box::new(TextControl::new(
            TextInput::local().multiline(true).with_id("sql"),
            text,
            sql,
        ))
    }
}

fn typed_sql(app: &TestApp<QueryApp>) -> String {
    app.read_document(|doc| doc.text(sql(doc).unwrap()).unwrap())
}

fn open(role: Role) -> TestApp<QueryApp> {
    let mut app = TestApp::<QueryApp>::with_role(role).unwrap();
    app.harness().click_named("sql").unwrap();
    app
}

#[test]
fn typing_splices_the_child_through_the_parents_action() {
    let mut app = open(Role::Owner);
    app.harness().type_text("select 1");
    assert_eq!(typed_sql(&app), "select 1");
    assert_eq!(
        app.state(),
        "untitled",
        "the rest of the query is untouched"
    );
    assert!(
        app.history()
            .into_iter()
            .any(|context| context.resource.as_ref().is_some_and(|r| r.id == sql_id())),
        "history names the SQL as the resource edited"
    );
}

#[test]
fn undo_takes_back_the_child_a_word_at_a_time() {
    let mut app = open(Role::Owner);
    app.harness().type_text("select name");
    assert!(app.undo());
    assert_eq!(typed_sql(&app), "select ");
    assert!(app.undo());
    assert_eq!(typed_sql(&app), "");
}

#[test]
fn a_parent_action_between_keystrokes_ends_the_word() {
    let mut app = open(Role::Owner);
    app.harness().type_text("sel");
    app.invoke(Invoke::<Query>::apply(&QueryAction::Rename("Totals".into())).unwrap())
        .unwrap();
    app.harness().click_named("sql").unwrap();
    app.harness().key("End");
    app.harness().type_text("ect");
    assert_eq!(typed_sql(&app), "select");
    assert!(app.undo());
    assert_eq!(
        typed_sql(&app),
        "sel",
        "the letters after the rename are a step of their own"
    );
}

#[test]
fn a_viewers_keystrokes_change_nothing_and_the_field_shows_the_stored_text() {
    let mut app = open(Role::Viewer);
    let before = app.versions().len();
    app.harness().type_text("drop table");
    assert_eq!(typed_sql(&app), "");
    assert_eq!(app.versions().len(), before);
    let inspection = app.harness().inspection_json().unwrap();
    assert!(!inspection.contains("drop table"), "{inspection}");
}

#[test]
fn an_agent_addresses_the_child_by_its_own_actions() {
    let mut app = open(Role::Owner);
    let surface = app.agent_surface();
    let rows = surface.as_array().unwrap();
    assert!(
        rows.iter().any(|row| row["resource"] == sql_id().as_str()
            && row["action"] == "replace-text"
            && row["resource_kind"] == PlainText::KIND),
        "the SQL is published with the text's own actions: {surface}"
    );
    app.invoke_child::<PlainText>(
        &sql_id(),
        Invoke::<PlainText>::apply(&TextEdit::ReplaceText {
            text: "select 2".into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(typed_sql(&app), "select 2");
    let inspection = app.harness().inspection_json().unwrap();
    assert!(
        inspection.contains("select 2"),
        "the field shows it: {inspection}"
    );
    assert!(app.undo());
    assert_eq!(typed_sql(&app), "");
}

#[test]
fn a_collaborators_typing_merges_and_shows() {
    let mut app = open(Role::Owner);
    app.harness().type_text("select 1");
    app.merge_peer_edit(|doc| {
        let obj = sql(doc).unwrap();
        doc.splice_text(&obj, 8, 0, " from t").unwrap();
    });
    app.harness().key("End");
    app.harness().type_text(";");
    assert_eq!(typed_sql(&app), "select 1 from t;");
}
