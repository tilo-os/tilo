//! A document app for the suites, and nothing a person installs.
//!
//! A test that types into a window, edits a record through its published actions or reads lent
//! content needs a document app to do it with. It opens this one: the shared editor over a
//! rich-text record, with the actions that record publishes, an effect that reads a document the
//! workspace holds as content, and an effect that writes a file and answers nothing. Its id is
//! never a product's, so no suite's assertion moves when a product changes, and the app stays
//! when a product that once stood in its place goes.

use automerge::AutoCommit;
use tilo_rich_text::RichTextViews;
use ui::framework::{EffectAnswer, ResourceApp, ViewCx};
use ui::tilo_resource;
use ui::widget::Element;

/// The record every window shows: the document model's rich text, which the shared editor
/// draws and edits, its blocks addressed by their own ids.
pub use tilo_rich_text::RichText as Document;

/// The app's id.
pub const APP_ID: &str = "so.tilo.test-document";

/// What the app does beside its record: reads a document the workspace holds as content, and
/// writes one to a file it is given.
#[derive(Clone, Debug, PartialEq, Eq, ui::tilo_resource::Effect)]
#[effect(operation = "test-document-app.effects")]
pub enum TestEffect {
    /// Read a document kept as content.
    ///
    /// The document the content address names, as its text: what an agent reads a file the
    /// workspace lent it through.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    ReadContent {
        #[param(content, "The document's content address, `tilo.content:<hash>`")]
        content: String,
        #[param("Its file name, e.g. notes.md")]
        name: String,
        #[param("The character to read from; the start when none")]
        from: Option<usize>,
    },
    /// Open a document.
    ///
    /// Reads a file into the record; it answers nothing, so an agent is never offered it.
    #[effect(kind = "outside")]
    #[effect(job, destructive)]
    OpenDocument {
        #[param("Absolute filesystem path; omit to choose one")]
        path: Option<ui::tilo_resource::AbsolutePath>,
    },
}

pub struct TestDocumentApp;

impl ResourceApp for TestDocumentApp {
    type Resource = Document;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Document";

    fn view(cx: &mut ViewCx<Document>) -> Element {
        let content = cx.content();
        cx.effects(move |effect: TestEffect, _cx| answer(effect, content.as_ref()));
        cx.rich_text()
    }
}

/// The answer to `effect`, from what this device holds.
fn answer(effect: TestEffect, content: Option<&tilo_content::ContentStore>) -> EffectAnswer {
    match effect {
        TestEffect::ReadContent {
            content: address,
            name,
            from,
        } => {
            let hash = ui::tilo_actions::content_hash(address.trim())
                .ok_or_else(|| format!("`{address}` is not a `tilo.content:<hash>` address"))?;
            let store = content.ok_or("this device holds no content")?;
            let bytes = store
                .read(hash)
                .ok_or_else(|| format!("{name} is not on this device"))?;
            let text =
                String::from_utf8(bytes).map_err(|_| format!("{name} is not a text document"))?;
            let text = match from.unwrap_or(0) {
                0 => text,
                at => text.chars().skip(at).collect(),
            };
            Ok(ui::framework::Effected::value(serde_json::json!({
                "text": text,
                "name": name,
            })))
        }
        TestEffect::OpenDocument { .. } => Ok(ui::framework::Effected::done()),
    }
}

/// The app as a host registers it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<TestDocumentApp>()
}

/// The empty document a new record starts from.
pub fn initialize_empty_document(doc: &mut AutoCommit) -> Result<(), String> {
    <Document as ui::tilo_resource::Resource>::initialize(doc).map_err(|error| error.to_string())
}

/// What the app publishes, as its tests read it: it declares effects, so the vocabulary is
/// named here for a compile-time check that the derive saw them.
#[cfg(test)]
mod tests {
    use super::TestEffect;
    use ui::tilo_resource::Effect as _;

    #[test]
    fn the_app_publishes_a_read_and_a_job_effect() {
        let specs = TestEffect::specs();
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        assert!(ids.contains(&"read-content"), "{ids:?}");
        assert!(ids.contains(&"open-document"), "{ids:?}");
    }
}
