use qos_actions::{ActionError, ActionRegistry, Doc, ExecutionLocus, Resource};
use serde::{Deserialize, Serialize};
use ui::automerge::transaction::Transactable;
use ui::tokens::TextRole;
use ui::widget::{Text, TextInput, Widget, labeled_field};

const DOCUMENT_ACTION_TYPE: &str = "generated.document.action";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DocumentAction {
    SetContent { title: String, body: String },
    Clear,
}

impl qos_actions::Action for DocumentAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
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
    let title = TextInput::from_document_string("document-title")
        .with_id("document-title-input")
        .with_placeholder("document-title-placeholder");
    let body = TextInput::from_document_string("document-body")
        .with_id("document-body-input")
        .with_placeholder("document-body-placeholder")
        .multiline(true);

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
    let set_content = qos_actions::ActionSpec::typed(
        "set-content",
        "Set document content",
        "Enter the document title",
        ExecutionLocus::ClientLocal,
        vec![
            qos_actions::ActionParam::required_owned(
                "title",
                qos_actions::ParamType::Text,
                "Document title",
            ),
            qos_actions::ActionParam::required_owned(
                "body",
                qos_actions::ParamType::Text,
                "Document body",
            ),
        ],
        |args| {
            qos_actions::EncodedAction::encode(
                DOCUMENT_ACTION_TYPE,
                &DocumentAction::SetContent {
                    title: args.text("title").unwrap_or_default().to_owned(),
                    body: args.text("body").unwrap_or_default().to_owned(),
                },
            )
            .expect("bound document content is serializable")
        },
    );
    let resource = Resource::new("generated.document:root", "generated.document", "Document")
        .with_action(set_content)
        .with_action(
            qos_actions::ActionSpec::immediate(
                "clear",
                "Clear document",
                DOCUMENT_ACTION_TYPE,
                &DocumentAction::Clear,
            )
            .expect("the generated document action is serializable"),
        );
    crate::resource_actions::immediate(root, resource, |_| Ok(qos_actions::ActionOutput::default()))
}
