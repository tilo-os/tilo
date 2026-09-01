use ui::tokens::TextRole;
use ui::widget::{Text, TextInput, Widget, labeled_field};

/// A durable document with two synchronized views and built-in undo/redo.
///
/// The inputs write directly to the Automerge model. The preview reads the
/// same fields during normal synchronization, so persistence, collaboration,
/// inspection, and Ctrl+Z need no app-owned notification or binding layer.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let title = TextInput::from_document_string("document-title")
        .with_id("document-title-input")
        .with_placeholder("document-title-placeholder");
    let body = TextInput::from_document_string("document-body")
        .with_id("document-body-input")
        .with_placeholder("document-body-placeholder")
        .multiline(true);

    Box::new(ui::column![
        Text::new("document-heading").with_role(TextRole::Title),
        labeled_field("document-title-label", title),
        labeled_field("document-body-label", body),
        Text::new("document-preview-heading").with_role(TextRole::Heading),
        Text::from_document_string("document-title")
            .with_role(TextRole::Heading)
            .with_id("document-title-preview"),
        Text::from_document_string("document-body").with_id("document-body-preview"),
    ])
}
