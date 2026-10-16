//! Rich text as a child resource of another: the editor edits it through
//! the owning resource's own action, so undo, history, permissions and
//! replay are that resource's, and the palette, agents and scripts address
//! it like any resource.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde::{Deserialize, Serialize};
use tilo_document::{RichText, RichTextAction};
use tilo_rich_text::RichTextFieldViews;
use ui::automerge::transaction::Transactable;
use ui::automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::framework::testing::TestApp;
use ui::framework::{ResourceApp, ViewCx};
use ui::permissions::Role;
use ui::tilo_actions::{Action, ActionError, Doc};
use ui::tilo_resource::{Invoke, ModelError, Resource, child_resource_id, publish_child};
use ui::widget::Element;

const ACTION_TYPE: &str = "test.message";

/// The owning resource's action: an edit of the message body.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct EditBody(RichTextAction);

fn body(doc: &Doc) -> Option<ObjId> {
    match doc.get(ROOT, "body") {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

impl Action for EditBody {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let root = body(doc).ok_or_else(|| ActionError::Refused("no message body".into()))?;
        self.0.apply_under(doc, &root)
    }
}

/// A message: a subject and a rich-text body, as a hand-written resource
/// whose one action edits the body.
struct Message;

impl Resource for Message {
    const KIND: &'static str = "test.message";
    const LABEL: &'static str = "Message";
    const ACTION_TYPE: &'static str = ACTION_TYPE;
    type State = String;
    type Action = EditBody;

    fn read(doc: &Doc) -> Result<String, ModelError> {
        Ok(match doc.get(ROOT, "subject") {
            Ok(Some((Value::Scalar(value), _))) => value.as_str().unwrap_or_default().to_owned(),
            _ => String::new(),
        })
    }

    /// The body, edited as a rich-text resource of its own.
    fn children(_state: &String) -> Vec<ui::tilo_actions::Resource> {
        vec![publish_child::<RichText, Self>(
            &body_id(),
            "Body",
            None,
            EditBody,
        )]
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        let root = doc.put_object(ROOT, "body", ObjType::Map)?;
        doc.put(ROOT, "subject", "untouched")?;
        RichText::initialize_under(doc, &root)
    }
}

fn body_id() -> String {
    child_resource_id::<Message>("body")
}

struct Mail;

impl ResourceApp for Mail {
    type Resource = Message;
    const ID: &'static str = "so.tilo.rich-text-field-example";
    const TITLE: &'static str = "Message";

    fn view(cx: &mut ViewCx<Message>) -> Element {
        cx.rich_text_field(&body_id(), body, EditBody)
    }
}

fn text(app: &TestApp<Mail>) -> Vec<String> {
    app.read_document(|doc| {
        let root = body(doc).unwrap();
        let c = tilo_doc::model::open(doc, &root).unwrap();
        tilo_doc::model::materialize(doc, &c)
            .blocks
            .into_iter()
            .map(|block| block.text)
            .collect()
    })
}

fn open(role: Role) -> TestApp<Mail> {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<Mail>::with_role(role).unwrap();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .unwrap();
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    app
}

#[test]
fn typing_edits_the_field_through_the_owning_action() {
    let mut app = open(Role::Owner);
    app.harness().type_text("hi there");
    app.harness().key("Enter");
    app.harness().type_text("second");
    assert_eq!(text(&app), ["hi there", "second"]);
    assert_eq!(
        app.state(),
        "untouched",
        "the rest of the resource is untouched"
    );
}

#[test]
fn undo_takes_back_a_word_at_a_time() {
    let mut app = open(Role::Owner);
    app.harness().type_text("hello world");
    assert!(app.undo());
    assert_eq!(text(&app), ["hello "]);
    assert!(app.undo());
    assert_eq!(text(&app), [""]);
}

#[test]
fn a_viewer_cannot_edit_the_field() {
    let mut app = open(Role::Viewer);
    let before = app.versions().len();
    app.harness().type_text("nope");
    assert_eq!(text(&app), [""]);
    assert_eq!(app.versions().len(), before);
}

#[test]
fn an_agent_edits_the_body_as_a_resource_of_its_own() {
    let mut app = open(Role::Owner);
    let declared = app.agent_surface();
    assert!(
        declared.as_array().unwrap().iter().any(|row| {
            row["resource"] == body_id().as_str() && row["resource_kind"] == RichText::KIND
        }),
        "the body is published as a rich-text resource: {declared}"
    );
    app.invoke_child::<RichText>(
        &body_id(),
        Invoke::<RichText>::apply(&RichTextAction::ReplaceBody {
            markdown: "from an agent".into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(text(&app), ["from an agent"]);
    assert_eq!(app.state(), "untouched");
    assert!(app.undo());
    assert_eq!(text(&app), [""]);
}

#[test]
fn a_viewer_cannot_edit_the_body_through_its_address() {
    let mut app = open(Role::Viewer);
    let refused = app.invoke_child::<RichText>(
        &body_id(),
        Invoke::<RichText>::apply(&RichTextAction::ReplaceBody {
            markdown: "nope".into(),
        })
        .unwrap(),
    );
    assert!(refused.is_err());
    assert_eq!(text(&app), [""]);
}
