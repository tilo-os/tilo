//! Hot reload application.

/// The application id.
pub const APP_ID: &str = "so.tilo.hot-reload-demo";
/// The window and launcher title.
pub const APP_TITLE: &str = "Hot reload";

pub mod locales;

fn install_catalog() {
    locales::install();
}

use ui::prelude::*;
use ui::widget::{Column, Field, Form, FormPage, FormSection, LabelPlacement};

/// A durable document. Text fields are collaborative text: two people typing
/// in the same field merge character by character.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Document {
    pub title: String,
    pub body: String,
}

#[resource(kind = "generated.document", label = "Document")]
impl Document {
    /// Set the document title.
    #[action]
    pub fn set_title(&mut self, #[param("Document title")] title: String) {
        self.title = title;
    }

    /// Set the document body.
    #[action]
    pub fn set_body(&mut self, #[param("Document body")] body: String) {
        self.body = body;
    }

    /// Set the document content.
    #[action]
    pub fn set_content(
        &mut self,
        #[param("Document title")] title: String,
        #[param("Document body")] body: String,
    ) {
        self.title = title;
        self.body = body;
    }

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
            Some("filled") => vec![
                Document::SET_CONTENT.with((
                    "Quarterly planning notes".into(),
                    "Three priorities for the next quarter: ship the offline editor, \
                 move the sync service to the new relay, and cut cold-start time \
                 in half. Each has an owner and a review date."
                        .into(),
                )),
            ],
            _ => Vec::new(),
        }
    }

    fn view(cx: &mut ViewCx<Document>) -> Element {
        crate::install_catalog();
        let title = cx
            .input(|document| document.title.clone(), Document::SET_TITLE)
            .with_id("document-title-input")
            .with_placeholder("document-title-placeholder");
        let body = cx
            .input(|document| document.body.clone(), Document::SET_BODY)
            .with_id("document-body-input")
            .with_placeholder("document-body-placeholder")
            .multiline(true);
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
                Box::new(
                    Text::new("document-preview-empty")
                        .with_role(TextRole::Body)
                        .dimmed(),
                )
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
