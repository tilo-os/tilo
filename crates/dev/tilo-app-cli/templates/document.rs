use ui::i18n::I18nManager;
use ui::prelude::*;

/// One document. Its text fields are collaborative text: two people typing in
/// the same field merge character by character.
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

    /// Replace the title and body in one undo step.
    #[action]
    pub fn set_content(
        &mut self,
        #[param("Document title")] title: String,
        #[param("Document body")] body: String,
    ) {
        self.title = title;
        self.body = body;
    }

    /// Erase the document. Declared destructive, so every surface that
    /// invokes it asks for confirmation first.
    #[action(destructive)]
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

pub struct App;

impl ResourceApp for App {
    type Resource = Document;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    /// The name the palette, search and history show for the open document.
    fn label(document: &Document) -> String {
        if document.title.trim().is_empty() {
            I18nManager::lookup_plain("document-untitled")
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
        // The preview reads the fields the inputs edit, so it follows every
        // edit, local or remote, with no notification code.
        let preview_title = cx
            .text(|document| document.title.clone())
            .with_role(TextRole::Heading)
            .with_id("document-title-preview");
        let preview_body = cx
            .text(|document| document.body.clone())
            .with_id("document-body-preview");
        Box::new(ui::column![
            labeled_field("document-title-label", title),
            labeled_field("document-body-label", body),
            Text::new("document-preview-heading").with_role(TextRole::Caption),
            preview_title,
            preview_body,
        ])
    }
}
