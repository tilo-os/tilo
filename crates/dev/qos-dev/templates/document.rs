use ui::widget::{Column, Text, TextInput, Widget};

/// A durable document with two synchronized views and built-in undo/redo.
///
/// The inputs write directly to the Automerge model. The preview reads the
/// same fields during normal synchronization, so persistence, collaboration,
/// inspection, and Ctrl+Z need no app-owned notification or binding layer.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let title = TextInput::from_document_string("document-title")
        .with_id("document-title-input")
        .with_accessible_name("document-title-label")
        .with_placeholder("document-title-placeholder");
    let body = TextInput::from_document_string("document-body")
        .with_id("document-body-input")
        .with_accessible_name("document-body-label")
        .with_placeholder("document-body-placeholder")
        .multiline(true);

    Box::new(
        Column::new()
            .push(Box::new(Text::new("document-heading").with_size(28.0)))
            .push(Box::new(Text::new("document-title-label")))
            .push(Box::new(title))
            .push(Box::new(Text::new("document-body-label")))
            .push(Box::new(body))
            .push(Box::new(
                Text::new("document-preview-heading").with_size(20.0),
            ))
            .push(Box::new(
                Text::from_document_string("document-title")
                    .with_size(18.0)
                    .with_id("document-title-preview"),
            ))
            .push(Box::new(
                Text::from_document_string("document-body").with_id("document-body-preview"),
            )),
    )
}
