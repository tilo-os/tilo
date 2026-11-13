//! The word processor as a framework app: the document resource is rich text, its
//! view is the editor's window, and images, tables and headings are its children. The
//! framework adds storage and sync, permission-checked edits, per-author undo, history
//! and presence; the editor reads the document through its incremental state.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LaunchInput, Publishes, ResourceApp, RunMode,
    ViewCx,
};
use ui::record::RecordId;
use ui::tilo_resource::Invoke;
use ui::widget::Element;

use crate::WordProcessorWidget;
use crate::effects::WordProcessorEffect;
pub use crate::model::Document;
use crate::widget::Editor;

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
        Ok(action) => Invoke::edit(&crate::model::edit(&action))
            .map(|import| vec![Invoke::group([import, Document::ADOPT_COMMENTS.invoke()])])
            .unwrap_or_default(),
        Err(error) => {
            eprintln!("word-processor: could not open {}: {error}", file.name());
            Vec::new()
        }
    }
}

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

const SCENARIOS: &[&str] = &[
    "styles",
    "comments",
    "threaded-comments",
    "find",
    "error",
    "image-object",
    "reactions",
];

pub struct WordProcessorApp;

impl ResourceApp for WordProcessorApp {
    type Resource = Document;
    const ID: &'static str = "so.tilo.word-processor";
    // Each window shows the document it opened, never another window's.
    const WINDOW_RESOURCE: bool = true;
    const TITLE: &'static str = "Word Processor";
    const TITLE_KEY: &'static str = "word-processor-app-title";
    const ICON: Option<&'static str> = Some("word-processor");
    const OPENS: &'static [&'static str] = OPENS;
    const ARGUMENTS: usize = 1;

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1000, 700)
    }

    fn view(cx: &mut ViewCx<Document>) -> Element {
        let window = WordProcessorWidget::new();
        let editor = window.editor();
        attach(&mut editor.borrow_mut(), cx);
        let window = window.following(cx.live().clone());
        {
            let editor = Rc::clone(&editor);
            let status = Rc::clone(&editor);
            cx.effects(move |effect: WordProcessorEffect, cx| answer(&editor, effect, cx))
                .status(move || share_image_status(&status.borrow()));
        }
        {
            let editor = Rc::clone(&editor);
            cx.on_stage(move |scenario| editor.borrow_mut().stage_scenario(scenario));
        }
        cx.publishing(
            Publishes::new().live_rows(move || editor.borrow().search_rows()),
            Box::new(window),
        )
    }

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

    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        input.argument().map(RecordId::for_file)
    }

    fn genesis(input: &LaunchInput<'_>) -> Vec<Invoke<Document>> {
        import(input)
    }

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

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::i18n::CATALOG)
    }

    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        vec![crate::semantic_read::action::spec()]
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Document>> {
        let texts: &[&str] = match name {
            Some("comments" | "threaded-comments" | "reactions") => &["Quarterly planning summary"],
            Some("find") => &[
                "Quarterly report",
                "The report covers revenue, costs and headcount for the quarter.",
                "A second report follows in the spring.",
            ],
            Some("image-object") => &["Product direction image"],
            _ => &[],
        };
        let reactions: &[&str] = if name == Some("reactions") {
            &["🎉", "👍"]
        } else {
            &[]
        };
        texts
            .iter()
            .map(|text| Document::APPEND_PARAGRAPH.with(*text))
            .chain(
                reactions.iter().filter_map(|emoji| {
                    Invoke::edit(&Document::paths().reactions.react(*emoji)).ok()
                }),
            )
            .collect()
    }
}

fn attach(editor: &mut Editor, cx: &ViewCx<Document>) {
    let content = cx
        .content()
        .unwrap_or_else(tilo_content::ContentStore::memory);
    editor.model_mut().set_content_store(content);
    if let Some(path) = cx.launch_arguments().first() {
        editor.show_record_of(std::path::PathBuf::from(path));
    }
    editor.follow_personal_dictionary();
    if cx.mode() == RunMode::Live {
        editor.follow_preferences(cx.settings(), cx.settings());
    }
    editor.attach_framework(cx.live().clone(), cx.dispatcher());
}

fn answer(
    editor: &Rc<RefCell<Editor>>,
    effect: WordProcessorEffect,
    cx: &mut EffectCx<Document>,
) -> EffectAnswer {
    if effect == WordProcessorEffect::ShareImage {
        let hash = editor
            .borrow()
            .shared_image()
            .ok_or("there is no one picture to share: place the caret on one")?;
        return Effected::content(&hash);
    }
    match effect {
        WordProcessorEffect::ReadFile { file, from } => Ok(Effected::value(
            crate::file_convert::read_file(&file, from)?,
        )),
        WordProcessorEffect::ReadContent {
            content,
            name,
            from,
        } => Ok(Effected::value(crate::file_convert::read_content(
            editor.borrow().model().content_store(),
            (&content, &name),
            from,
        )?)),
        WordProcessorEffect::ConvertFile { file, to } => {
            let written = crate::file_convert::convert_file(&file, &to)?;
            Ok(Effected::value(
                serde_json::json!({ "file": written.display().to_string() }),
            ))
        }
        effect => editor.borrow_mut().answer_effect(editor, effect, cx),
    }
}

fn share_image_status(editor: &Editor) -> Vec<EffectStatus> {
    if editor.shared_image().is_some() {
        return Vec::new();
    }
    vec![EffectStatus::unavailable(
        WordProcessorEffect::SHARE_IMAGE,
        "no_image",
        "there is no one picture to share: place the caret on one",
    )]
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
                file: ui::tilo_resource::AbsolutePath::new(notes).unwrap(),
                to: "odt".into(),
            })
            .unwrap()
            .value
            .unwrap();
        let odt = root.path().join("notes.odt");
        assert_eq!(converted["file"], odt.display().to_string());
        let read = app
            .effect(&WordProcessorEffect::ReadFile {
                file: ui::tilo_resource::AbsolutePath::new(odt).unwrap(),
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

    /// A relative path is refused before it reaches the app: it would name
    /// a file wherever the process runs.
    #[test]
    fn file_effects_refuse_a_relative_path() {
        use super::WordProcessorEffect;
        use ui::tilo_resource::Effect as _;
        for (id, param) in [
            ("open-document", "path"),
            ("save", "path"),
            ("save-as", "path"),
            ("read-file", "file"),
            ("convert-file", "file"),
        ] {
            let mut arguments = serde_json::Map::new();
            arguments.insert(param.into(), "notes.md".into());
            arguments.insert("to".into(), "odt".into());
            let error = WordProcessorEffect::decode(id, &arguments).unwrap_err();
            assert!(
                matches!(
                    &error,
                    ui::tilo_resource::EffectDecodeError::Argument { name, source }
                        if name == param && source.to_string().contains("absolute")
                ),
                "{id}: {error}"
            );
        }
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

ui::reach_tests!(WordProcessorApp);
