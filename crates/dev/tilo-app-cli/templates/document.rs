use serde::{Deserialize, Serialize};
use ui::prelude::{
    ActionError, ActionRegistry, Doc, ExecutionLocus, Resource, Text, TextInput, TextRole,
    Transactable, Widget, labeled_field, tilo_actions,
};

const DOCUMENT_ACTION_TYPE: &str = "generated.document.action";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DocumentAction {
    SetTitle(String),
    SetBody(String),
    SetContent { title: String, body: String },
    Clear,
}

impl tilo_actions::Action for DocumentAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::SetTitle(title) => {
                doc.put(ui::automerge::ROOT, "document-title", title)?;
            }
            Self::SetBody(body) => {
                doc.put(ui::automerge::ROOT, "document-body", body)?;
            }
            Self::SetContent { title, body } => {
                doc.put(ui::automerge::ROOT, "document-title", title)?;
                doc.put(ui::automerge::ROOT, "document-body", body)?;
            }
            Self::Clear => {
                doc.put(ui::automerge::ROOT, "document-title", "")?;
                doc.put(ui::automerge::ROOT, "document-body", "")?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

pub fn register_actions(registry: &mut ActionRegistry) {
    registry.register::<DocumentAction>(DOCUMENT_ACTION_TYPE);
}

/// A durable document with two synchronized views and built-in undo/redo.
///
/// The inputs write directly to the Automerge model. The preview reads the
/// same fields during normal synchronization, so persistence, collaboration,
/// inspection, and Ctrl+Z need no app-owned notification or binding layer.
///
/// # Panics
///
/// Panics only if serializing the generated `DocumentAction` enum fails.
#[expect(
    clippy::expect_used,
    reason = "the generated action enum contains only serializable strings and unit variants"
)]
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let resource = document_resource();
    let text_binding = |action_id: &str| {
        tilo_actions::ActionBinding::new(
            &resource,
            resource
                .actions
                .iter()
                .find(|action| action.id == action_id)
                .expect("generated document resource exports its field action"),
            |value| serde_json::Map::from_iter([("value".into(), serde_json::json!(value))]),
        )
        .expect("document field produces valid action arguments")
    };
    let title = TextInput::from_document_string("document-title")
        .with_id("document-title-input")
        .with_placeholder("document-title-placeholder")
        .on_action(text_binding("set-title"));
    let body = TextInput::from_document_string("document-body")
        .with_id("document-body-input")
        .with_placeholder("document-body-placeholder")
        .multiline(true)
        .on_action(text_binding("set-body"));
    let root = Box::new(ui::column![
        Text::new("document-heading").with_role(TextRole::Title),
        labeled_field("document-title-label", title),
        labeled_field("document-body-label", body),
        Text::new("document-preview-heading").with_role(TextRole::Heading),
        Text::from_document_string("document-title")
            .with_role(TextRole::Heading)
            .with_id("document-title-preview"),
        Text::from_document_string("document-body").with_id("document-body-preview"),
    ]);
    ui::widget::resource_actions::immediate(root, resource, |_| {
        Ok(tilo_actions::ActionOutput::default())
    })
}
#[expect(
    clippy::expect_used,
    reason = "the generated action enum contains only serializable strings and unit variants"
)]
fn document_resource() -> Resource {
    let set_content = tilo_actions::ActionSpec::typed(
        "set-content",
        "Set document content",
        "Enter the document title",
        ExecutionLocus::ClientLocal,
        vec![
            tilo_actions::ActionParam::required_owned(
                "title",
                tilo_actions::ParamType::Text,
                "Document title",
            ),
            tilo_actions::ActionParam::required_owned(
                "body",
                tilo_actions::ParamType::Text,
                "Document body",
            ),
        ],
        |args| {
            tilo_actions::EncodedAction::encode(
                DOCUMENT_ACTION_TYPE,
                &DocumentAction::SetContent {
                    title: args.text("title").unwrap_or_default().to_owned(),
                    body: args.text("body").unwrap_or_default().to_owned(),
                },
            )
            .expect("bound document content is serializable")
        },
    );
    let set_title = tilo_actions::ActionSpec::typed(
        "set-title",
        "Set document title",
        "Enter the document title",
        ExecutionLocus::ClientLocal,
        vec![tilo_actions::ActionParam::required_owned(
            "value",
            tilo_actions::ParamType::Text,
            "Document title",
        )],
        |args| {
            tilo_actions::EncodedAction::encode(
                DOCUMENT_ACTION_TYPE,
                &DocumentAction::SetTitle(args.text("value").unwrap_or_default().to_owned()),
            )
            .expect("bound document title is serializable")
        },
    );
    let set_body = tilo_actions::ActionSpec::typed(
        "set-body",
        "Set document body",
        "Enter the document body",
        ExecutionLocus::ClientLocal,
        vec![tilo_actions::ActionParam::required_owned(
            "value",
            tilo_actions::ParamType::Text,
            "Document body",
        )],
        |args| {
            tilo_actions::EncodedAction::encode(
                DOCUMENT_ACTION_TYPE,
                &DocumentAction::SetBody(args.text("value").unwrap_or_default().to_owned()),
            )
            .expect("bound document body is serializable")
        },
    );
    Resource::new("generated.document:root", "generated.document", "Document")
        .with_action(set_title)
        .with_action(set_body)
        .with_action(set_content)
        .with_action(
            tilo_actions::ActionSpec::immediate(
                "clear",
                "Clear document",
                DOCUMENT_ACTION_TYPE,
                &DocumentAction::Clear,
            )
            .expect("the generated document action is serializable"),
        )
}
