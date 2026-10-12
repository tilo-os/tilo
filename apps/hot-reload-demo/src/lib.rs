//! Hot reload application.

/// The application id.
pub const APP_ID: &str = "so.tilo.hot-reload-demo";
/// The window and launcher title.
pub const APP_TITLE: &str = "Hot reload";

pub mod locales;

use ui::prelude::*;
use ui::widget::{Column, EmptyState, Field, Form, FormPage, FormSection, LabelPlacement};

/// A durable document. Text fields are collaborative text: two people typing
/// in the same field merge character by character.
///
/// Each field is edited at its path (`Document::paths().title`); the one
/// verb is clearing it, which is more than setting a field.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Document {
    /// Title
    pub title: String,
    /// Body
    pub body: String,
}

#[resource(kind = "hot-reload-demo.document", label = "Document")]
impl Document {
    /// Clear the document.
    #[action(destructive)]
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The app: two editors and a live preview of the same document.
///
/// The preview reads the fields the inputs edit, so it follows every edit —
/// local, a collaborator's, an undo — with no notification code.
pub struct App;

impl ResourceApp for App {
    type Resource = Document;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn label(document: &Document) -> String {
        if document.title.is_empty() {
            String::from("Document")
        } else {
            document.title.clone()
        }
    }

    fn scenarios() -> &'static [&'static str] {
        &["filled"]
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Document>> {
        match name {
            Some("filled") => {
                let paths = Document::paths();
                [
                    paths.title.replace("Quarterly planning notes"),
                    paths.body.replace(
                        "Three priorities for the next quarter: ship the offline editor, \
                         move the sync service to the new relay, and cut cold-start time \
                         in half. Each has an owner and a review date.",
                    ),
                ]
                .iter()
                .filter_map(|edit| Invoke::edit(edit).ok())
                .collect()
            }
            _ => Vec::new(),
        }
    }

    fn view(cx: &mut ViewCx<Document>) -> Element {
        let paths = cx.paths();
        let title = cx
            .input(paths.title)
            .with_id("document-title-input")
            .with_placeholder("document-title-placeholder");
        let body = cx
            .input(paths.body)
            .with_id("document-body-input")
            .with_placeholder("document-body-placeholder")
            .with_min_lines(6);
        let has_content =
            cx.value(|document| !document.title.is_empty() || !document.body.is_empty());
        let preview = cx.switch(&has_content, |cx, has_content| -> Element {
            if *has_content {
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(Box::new(
                            cx.text(|document| document.title.clone())
                                .with_role(TextRole::Subheading)
                                .with_id("document-title-preview"),
                        ))
                        .push(Box::new(
                            cx.text(|document| document.body.clone())
                                .with_id("document-body-preview"),
                        )),
                )
            } else {
                // An empty section of the page: a heading-sized empty state
                // that hugs its content.
                Box::new(EmptyState::new("document-preview-empty").inline())
            }
        });
        Box::new(FormPage::new(
            "document-heading",
            Form::new()
                .with_label_placement(Some(LabelPlacement::Above))
                .section(
                    FormSection::new("document-edit-heading")
                        .field(Field::new("document-title-label", title))
                        .field(Field::new("document-body-label", body)),
                )
                .section(FormSection::new("document-preview-heading").content(preview)),
        ))
    }
}

ui::dev_reload_entry!(App);
