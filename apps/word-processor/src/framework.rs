//! The word processor as a framework app: the document resource is rich text,
//! and the editor — its own page layout, input and chrome — is the surface.
//! Images, tables and headings are children of that resource, so the framework
//! publishes them on the same door as the document.
//!
//! What the framework adds without the editor writing it: record storage and
//! sync, permission-checked edits (a viewer's keystrokes are refused before
//! they reach the document), per-author undo with typing grouped by word,
//! attributed history and a History mode drawn by the editor's real page
//! layout, presence keyed to paragraphs, and search over the outline.
//!
//! The editor reads the document through the framework's incrementally
//! maintained [`RichTextState`](tilo_document::RichTextState): a keystroke refreshes its paragraph, and
//! only the paragraphs around the viewport are ever read.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::record::RecordId;
use ui::tilo_resource::Invoke;
use ui::widget::Element;

use crate::WordProcessorWidget;
use crate::effects::WordProcessorEffect;
pub use crate::model::Document;

/// The import of the file `input` names, as one action.
fn import(input: &LaunchInput<'_>) -> Vec<Invoke<Document>> {
    let Some(file) = input.file() else {
        return Vec::new();
    };
    let format = crate::WpFormat::from_path(std::path::Path::new(file.name()));
    let content = input
        .content()
        .cloned()
        .unwrap_or_else(tilo_content::ContentStore::memory);
    match crate::import_action(file.bytes(), format, &content) {
        // The file's text and the comments it brought, as one step.
        Ok(action) => Invoke::edit(&crate::model::edit(&action))
            .map(|import| vec![Invoke::group([import, Document::ADOPT_COMMENTS.invoke()])])
            .unwrap_or_default(),
        Err(error) => {
            eprintln!("word-processor: could not open {}: {error}", file.name());
            Vec::new()
        }
    }
}

/// The media types of the files the word processor opens: every format it
/// imports (PDF and EPUB are export-only).
const OPENS: &[&str] = &[
    "application/vnd.oasis.opendocument.text",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "text/markdown",
    "text/x-markdown",
    "text/plain",
    "text/html",
    "application/rtf",
    "text/rtf",
];

/// The screenshot scenarios: each stages the document through its actions
/// and the editor's view state where it lives.
const SCENARIOS: &[&str] = &[
    "styles",
    "comments",
    "threaded-comments",
    "find",
    "error",
    "image-object",
];

pub struct WordProcessorApp;

impl ResourceApp for WordProcessorApp {
    type Resource = Document;
    const ID: &'static str = "so.tilo.word-processor";
    // Each window shows the document it opened, never another window's.
    const WINDOW_RESOURCE: bool = true;
    const TITLE: &'static str = "Word Processor";
    const ICON: Option<&'static str> = Some("word-processor");
    const OPENS: &'static [&'static str] = OPENS;
    const ARGUMENTS: usize = 1;

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1000, 700)
    }

    fn view(cx: &mut ViewCx<Document>) -> Element {
        cx.surface(WordProcessorWidget::new())
    }

    /// The document's first heading, as the palette and history name it.
    fn label(state: &Document) -> String {
        state
            .text()
            .map(tilo_document::RichTextState::headings)
            .unwrap_or_default()
            .into_iter()
            .map(|heading| heading.text.trim().to_owned())
            .find(|text| !text.is_empty())
            .unwrap_or_else(|| "Document".to_owned())
    }

    /// A file is a record of its own: reopening it reopens its history.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        input.argument().map(RecordId::for_file)
    }

    /// A file's record starts as the file imported, in its genesis: every
    /// device that opens the same file before they have synced writes the
    /// same document.
    fn genesis(input: &LaunchInput<'_>) -> Vec<Invoke<Document>> {
        import(input)
    }

    /// Reopened, the record is the document and the file is where Save
    /// writes it, unless the file changed since the record last read or
    /// wrote it: then it is read again, an edit Undo takes back.
    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<Document>> {
        let (Some(file), Some(state)) = (input.file(), input.record::<Document>()) else {
            return Vec::new();
        };
        let digest = state
            .text()
            .and_then(tilo_document::RichTextState::file_digest);
        if created || digest == Some(tilo_content::hash_bytes(file.bytes()).as_str()) {
            return Vec::new();
        }
        import(input)
    }

    /// The app's messages, history's sentences among them.
    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    /// The document's text a page at a time, for agents, scripts and
    /// assistive tools.
    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        vec![crate::semantic_read::action::spec()]
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    /// The text each scenario shows; comments, images and the caret are
    /// staged on the editor, which knows the paragraphs these actions made.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Document>> {
        let texts: &[&str] = match name {
            Some("comments" | "threaded-comments") => &["Quarterly planning summary"],
            Some("find") => &[
                "Quarterly report",
                "The report covers revenue, costs and headcount for the quarter.",
                "A second report follows in the spring.",
            ],
            Some("image-object") => &["Product direction image"],
            _ => &[],
        };
        texts
            .iter()
            .map(|text| Document::APPEND_PARAGRAPH.with(*text))
            .collect()
    }
}

impl ResourceSurface<Document, WordProcessorEffect> for WordProcessorWidget {
    /// The editor reads the live state where it needs it; a new state only
    /// asks for a redraw.
    fn present(&mut self, state: &Rc<Document>) {
        // An editor that has not drawn yet still has a caret, at the start
        // of the body, so edits bound to it (a table at the caret) can be
        // invoked before the first frame.
        if self.model().caret().block.is_empty()
            && let Some(first) = state.text().and_then(|text| text.body().id_at(0))
        {
            self.model_mut().set_caret(crate::word_processor::Caret {
                block: first.to_owned(),
                offset: 0,
            });
        }
        self.mark_document_changed();
    }

    fn attach(&mut self, cx: SurfaceCx<Document>) {
        // The images are in the store the host lends the window, which
        // carries them on the document's route; without one they are kept
        // in memory.
        let content = cx
            .content()
            .unwrap_or_else(tilo_content::ContentStore::memory);
        self.model_mut().set_content_store(content);
        if let Some(path) = cx.launch_arguments().first() {
            self.show_record_of(std::path::PathBuf::from(path));
        }
        self.follow_personal_dictionary();
        // A past version and a capture show the layout they are given.
        if cx.mode() == RunMode::Live {
            self.follow_preferences(cx.settings(), cx.settings());
        }
        self.attach_framework(cx.live(), cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    /// The Find results of whoever invokes them, beneath the document.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.search_rows()
    }

    fn effect(
        &mut self,
        effect: WordProcessorEffect,
        cx: &mut EffectCx<Document, Self>,
    ) -> EffectAnswer {
        if effect == WordProcessorEffect::ShareImage {
            let hash = self
                .shared_image()
                .ok_or("there is no one picture to share: place the caret on one")?;
            return Effected::content(&hash);
        }
        // Files on disk, not the document open here.
        match effect {
            WordProcessorEffect::ReadFile { file, from } => Ok(Effected::value(
                crate::file_convert::read_file(&file, from)?,
            )),
            WordProcessorEffect::ReadContent {
                content,
                name,
                from,
            } => Ok(Effected::value(crate::file_convert::read_content(
                self.model().content_store(),
                (&content, &name),
                from,
            )?)),
            WordProcessorEffect::ConvertFile { file, to } => {
                let written = crate::file_convert::convert_file(&file, &to)?;
                Ok(Effected::value(
                    serde_json::json!({ "file": written.display().to_string() }),
                ))
            }
            effect => self.answer_effect(effect, cx),
        }
    }

    fn effects(&self) -> Vec<EffectStatus> {
        if self.shared_image().is_some() {
            return Vec::new();
        }
        vec![EffectStatus::unavailable(
            WordProcessorEffect::SHARE_IMAGE,
            "no_image",
            "there is no one picture to share: place the caret on one",
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::WordProcessorApp;

    #[test]
    fn the_document_publishes_file_effects_and_views() {
        let mut app = ui::framework::testing::TestApp::<WordProcessorApp>::new().unwrap();
        let surface = app.agent_surface();
        let ids: Vec<&str> = surface
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["resource"] == "word-processor.document")
            .filter_map(|row| row["action"].as_str())
            .collect();
        for id in [
            "find",
            "open-document",
            "save",
            "save-as",
            "read-file",
            "convert-file",
            "zoom-in",
            "move-caret",
            "type-text",
            "read-text",
            "append-paragraph",
            "field-edit",
        ] {
            assert!(ids.contains(&id), "{id}: {ids:?}");
        }
    }

    /// A caller with no screen converts a file on disk and reads one, and
    /// the document open here is untouched.
    #[test]
    fn files_on_disk_convert_and_read_without_opening_them() {
        use super::WordProcessorEffect;
        let root = tempfile::tempdir().unwrap();
        let notes = root.path().join("notes.md");
        std::fs::write(&notes, "# Notes\n\nBuy milk.").unwrap();
        let mut app = ui::framework::testing::TestApp::<WordProcessorApp>::new().unwrap();
        let before = app.peer_document().get_heads();
        let converted = app
            .effect(&WordProcessorEffect::ConvertFile {
                file: notes.display().to_string(),
                to: "odt".into(),
            })
            .unwrap()
            .value
            .unwrap();
        let odt = root.path().join("notes.odt");
        assert_eq!(converted["file"], odt.display().to_string());
        let read = app
            .effect(&WordProcessorEffect::ReadFile {
                file: odt.display().to_string(),
                from: None,
            })
            .unwrap()
            .value
            .unwrap();
        assert!(
            read["text"].as_str().unwrap().contains("Buy milk."),
            "{read}"
        );
        app.settle();
        assert_eq!(app.peer_document().get_heads(), before);
    }

    #[test]
    fn sharing_an_image_answers_content_another_app_takes() {
        use ui::tilo_resource::Effect;
        let spec = super::WordProcessorEffect::specs()
            .into_iter()
            .find(|spec| spec.id == "share-image")
            .expect("sharing an image is a published effect");
        assert_eq!(
            spec.contract.result,
            tilo_actions::ResultSpec::Resource {
                resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.to_owned(),
            },
            "its result is typed content, so a caller chains it into a content input"
        );
        let answer = ui::framework::Effected::content("abc123").expect("a content hash");
        assert!(
            matches!(
                answer,
                ui::framework::Effected::Done(ref output)
                    if output.resources == ["tilo.content:abc123"]
            ),
            "{answer:?}"
        );
    }

    #[test]
    fn page_margins_are_a_published_document_action() {
        let mut replica = ui::tilo_resource::Replica::<super::Document>::new();
        replica
            .apply(
                super::Document::SET_PAGE_MARGINS.with(crate::model::document::SetPageMargins {
                    top: 72.0,
                    right: 54.0,
                    bottom: 72.0,
                    left: 54.0,
                }),
            )
            .expect("margins apply");
        let containers = crate::model::open(replica.doc()).expect("document opens");
        assert_eq!(
            tilo_doc::model::meta_str(replica.doc(), &containers, "page_margin_top").as_deref(),
            Some("72")
        );
        assert_eq!(
            tilo_doc::model::meta_str(replica.doc(), &containers, "page_margin_right").as_deref(),
            Some("54")
        );
    }
}
