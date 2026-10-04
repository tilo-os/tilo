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

/// A durable document. Text fields are collaborative text: two people typing
/// in the same field merge character by character.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Document {
    pub title: String,
    pub body: String,
}

#[resource(kind = "so.tilo.hot-reload-demo.document", label = "Document")]
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
        Box::new(ui::column![
            Text::new("document-heading").with_role(TextRole::Title),
            labeled_field("document-title-label", title),
            labeled_field("document-body-label", body),
            Text::new("document-preview-heading").with_role(TextRole::Heading),
            cx.text(|document| document.title.clone())
                .with_role(TextRole::Heading)
                .with_id("document-title-preview"),
            cx.text(|document| document.body.clone())
                .with_id("document-body-preview"),
        ])
    }
}

ui::dev_reload_entry!(App);
