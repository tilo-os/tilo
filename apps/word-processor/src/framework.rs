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
//! maintained [`RichTextState`]: a keystroke refreshes its paragraph, and
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
    "template-picker",
    "toolbar-overflow",
    "find",
    "error",
    "image-object",
];

/// The word processor on the app framework.
pub struct WordProcessorApp;

impl ResourceApp for WordProcessorApp {
    type Resource = Document;
    const ID: &'static str = "so.tilo.word-processor";
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
        self.answer_effect(effect, cx)
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
    fn a_docx_exported_as_content_carries_its_text_and_placed_picture() {
        use ui::tilo_resource::{Effect, Resource};
        let spec = super::WordProcessorEffect::specs()
            .into_iter()
            .find(|spec| spec.id == "export-content")
            .expect("exporting as content is a published effect");
        assert_eq!(
            spec.contract.result,
            tilo_actions::ResultSpec::Resource {
                resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.to_owned(),
            }
        );
        assert!(
            !spec
                .contract
                .tags
                .iter()
                .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY),
            "it needs no device of its own"
        );

        let mut app = ui::framework::testing::TestApp::<WordProcessorApp>::new().unwrap();
        app.invoke(super::Document::APPEND_PARAGRAPH.with("Exported without a file"))
            .unwrap();
        app.settle();
        let png = ui::png::encode_rgba8(2, 1, &[255, 160, 40, 255, 30, 70, 180, 255]).unwrap();
        let hash = app.content().put(&png).unwrap();
        let resource = <super::Document as Resource>::KIND;
        let not_picture = app.content().put(b"not a picture").unwrap();
        let error = app
            .call_on(
                resource,
                "place-image",
                serde_json::json!({"content": not_picture}),
            )
            .unwrap_err();
        assert!(error.to_string().contains("not a picture"), "{error}");
        app.call_on(
            resource,
            "place-image",
            serde_json::json!({"content": hash}),
        )
        .expect("the picture is placed from content");
        app.settle();

        let output = app
            .effect(&super::WordProcessorEffect::ExportContent {
                format: "docx".into(),
            })
            .unwrap();
        let [address] = output.resources.as_slice() else {
            panic!("one content resource: {output:?}");
        };
        let docx = app
            .content()
            .read(
                address
                    .strip_prefix("tilo.content:")
                    .expect("a content address"),
            )
            .expect("the bytes are kept");
        assert!(docx.starts_with(b"PK"), "a .docx is a zip package");
        let parts =
            tilo_doc_codecs::safety::read_zip_parts(&docx, &tilo_doc_codecs::ZipLimits::default())
                .unwrap();
        let part = |name: &str| {
            parts
                .iter()
                .find(|(part, _)| part == name)
                .map(|(_, bytes)| bytes.as_slice())
        };
        let document = String::from_utf8_lossy(part("word/document.xml").expect("a body"));
        assert!(document.contains("Exported without a file"), "{document}");
        let media: Vec<&[u8]> = parts
            .iter()
            .filter(|(name, _)| {
                name.starts_with("word/media/")
                    && std::path::Path::new(name)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
            })
            .map(|(_, bytes)| bytes.as_slice())
            .collect();
        assert_eq!(
            media,
            [png.as_slice()],
            "the placed PNG travels byte for byte"
        );

        for format in ["odt", "md", "html", "rtf", "txt"] {
            let output = app
                .effect(&super::WordProcessorEffect::ExportContent {
                    format: format.into(),
                })
                .unwrap_or_else(|error| panic!("{format}: {error}"));
            assert_eq!(output.resources.len(), 1, "{format}");
        }
        assert!(
            app.effect(&super::WordProcessorEffect::ExportContent {
                format: "exe".into(),
            })
            .is_err()
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

    #[test]
    fn read_text_is_published_with_the_document() {
        let mut app = ui::framework::testing::TestApp::<WordProcessorApp>::new().unwrap();
        let resources = app.harness().resources();
        let document = resources
            .iter()
            .find(|resource| {
                resource.id.as_str() == <super::Document as ui::tilo_resource::Resource>::KIND
            })
            .expect("the host publishes the document");
        let read = document
            .actions
            .iter()
            .find(|action| action.id == "read-text")
            .expect("read-text is on the published document");
        assert_eq!(read.kind, tilo_actions::OperationKind::Effect);
    }

    /// An agent invokes the same surface-backed edit as a person, with no
    /// caret or selection supplied. The action finds its place from the body.
    fn image_document() -> (
        ui::framework::testing::TestApp<WordProcessorApp>,
        Vec<u8>,
        String,
    ) {
        let mut app = ui::framework::testing::TestApp::<WordProcessorApp>::new().unwrap();
        for text in ["Intro", "The lamp in use", "Closing"] {
            app.invoke(super::Document::APPEND_PARAGRAPH.with(text))
                .unwrap();
        }
        app.settle();
        let png = ui::png::encode_rgba8(2, 1, &[255, 160, 40, 255, 30, 70, 180, 255]).unwrap();
        let hash = app.content().put(&png).unwrap();
        (app, png, hash)
    }

    /// The invoker the host delivers for an admitted remote execution, not
    /// a participant id supplied in the action's arguments.
    fn image_invoker() -> tilo_actions::ActionInvoker {
        tilo_actions::ActionInvoker::actor("carrier")
            .with_participant(tilo_actions::ParticipantId::new("carrier-execution").unwrap())
    }

    fn insert_image(
        app: &mut ui::framework::testing::TestApp<WordProcessorApp>,
        hash: &str,
        after: Option<&str>,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        use ui::tilo_resource::{Effect, Resource};
        let effect = super::WordProcessorEffect::InsertImage {
            content: hash.into(),
            after: after.map(str::to_owned),
        };
        app.call_by(
            image_invoker(),
            <super::Document as Resource>::KIND,
            effect.id(),
            serde_json::Value::Object(effect.arguments()),
        )
    }

    /// Each body block's kind, its text, and the picture it shows.
    fn body(doc: &automerge::AutoCommit) -> Vec<(String, String, Option<String>)> {
        let containers = crate::model::open(doc).unwrap();
        tilo_doc::model::materialize(doc, &containers)
            .blocks
            .into_iter()
            .map(|block| {
                let blob = match block.attrs.get("blob") {
                    Some(tilo_doc::AttrVal::Str(hash)) => Some(hash.clone()),
                    _ => None,
                };
                (block.kind, block.text, blob)
            })
            .collect()
    }

    #[test]
    fn an_agent_inserts_produced_content_after_a_paragraph_or_at_the_end() {
        use ui::tilo_resource::Effect;
        let (mut app, _png, hash) = image_document();
        let operation = super::WordProcessorEffect::specs()
            .into_iter()
            .find(|operation| operation.id == "insert-image")
            .expect("the surface publishes insert-image");
        assert!(
            operation
                .params()
                .iter()
                .any(|param| param.name == "content" && param.is_content()),
            "the image is carried"
        );
        assert!(
            operation
                .params()
                .iter()
                .any(|param| param.name == "after" && !param.required)
        );
        assert_eq!(operation.contract.kind, tilo_actions::ActionKind::Undoable);
        assert_eq!(operation.kind, tilo_actions::OperationKind::View);
        assert!(operation.contract.is_participant_view());

        let unknown = tilo_content::hash_bytes(b"never produced");
        let error = insert_image(&mut app, &unknown, None).unwrap_err();
        assert!(error.to_string().contains("not available"), "{error}");
        let error = insert_image(&mut app, &hash, Some("no such words")).unwrap_err();
        assert!(
            error.to_string().contains("no paragraph contains"),
            "{error}"
        );
        let document = app.content().put(b"not a picture").unwrap();
        let error = insert_image(&mut app, &document, None).unwrap_err();
        assert!(error.to_string().contains("not a picture"), "{error}");
        assert!(
            app.read_document(body)
                .iter()
                .all(|(kind, ..)| kind != "image"),
            "a refusal writes nothing"
        );

        insert_image(&mut app, &hash, Some("lamp in"))
            .expect("the picture is placed after the paragraph");
        let blocks = app.read_document(body);
        let named = blocks
            .iter()
            .position(|(_, text, _)| text == "The lamp in use")
            .unwrap();
        assert_eq!(
            blocks.get(named + 1),
            Some(&("image".to_owned(), String::new(), Some(hash.clone()))),
            "{blocks:?}"
        );
        assert_eq!(
            blocks.get(named + 3).map(|(_, text, _)| text.as_str()),
            Some("Closing"),
            "the paragraph after it is untouched: {blocks:?}"
        );

        insert_image(&mut app, &format!("tilo.content:{hash}"), None)
            .expect("without `after` the picture is appended");
        let blocks = app.read_document(body);
        let closing = blocks
            .iter()
            .position(|(_, text, _)| text == "Closing")
            .unwrap();
        assert_eq!(
            blocks.get(closing + 1),
            Some(&("image".to_owned(), String::new(), Some(hash.clone()))),
            "{blocks:?}"
        );
        assert_eq!(
            blocks.iter().filter(|(kind, ..)| kind == "image").count(),
            2
        );
    }

    #[test]
    fn image_insertion_keeps_permissions_and_private_view_confinement() {
        use ui::tilo_resource::Resource;
        let (mut app, _png, hash) = image_document();
        let resource = <super::Document as Resource>::KIND;
        let before = app.read_document(body);
        let heads = app.read_document(|doc| doc.clone().get_heads());
        let local_view = app
            .with_widget(|editor: &mut super::WordProcessorWidget| editor.local_participant_view())
            .expect("the editor is present");

        let error = app
            .effect_by(
                "carrier",
                &super::WordProcessorEffect::InsertImage {
                    content: hash.clone(),
                    after: None,
                },
            )
            .unwrap_err();
        assert!(matches!(
            error,
            tilo_actions::DoorError::Unavailable { code, .. } if code == "private_view"
        ));
        // An admitted execution has its own caret, not authority over the
        // window's private viewport.
        let error = app
            .call_by(image_invoker(), resource, "zoom-in", serde_json::json!({}))
            .unwrap_err();
        assert!(matches!(
            error,
            tilo_actions::DoorError::Unavailable { code, .. } if code == "private_view"
        ));

        app.set_role(ui::permissions::Role::Viewer);
        // The viewer's resource tree does not offer document edits; the
        // door resolves it again rather than trusting the owner's surface.
        let error = insert_image(&mut app, &hash, Some("Intro")).unwrap_err();
        assert!(
            matches!(
                &error,
                tilo_actions::DoorError::Unknown(address)
                    if address.resource.as_str() == resource && address.action == "insert-image"
            ),
            "{error:?}"
        );
        assert_eq!(app.read_document(body), before, "refusals write nothing");
        assert_eq!(app.read_document(|doc| doc.clone().get_heads()), heads);

        app.set_role(ui::permissions::Role::Owner);
        insert_image(&mut app, &hash, Some("Intro"))
            .expect("the admitted remote participant can insert a picture");
        app.settle();
        assert!(
            app.read_document(body).iter().any(|(kind, _, blob)| {
                kind == "image" && blob.as_deref() == Some(hash.as_str())
            })
        );
        assert_eq!(
            app.with_widget(
                |editor: &mut super::WordProcessorWidget| editor.local_participant_view()
            )
            .expect("the editor is present"),
            local_view,
            "the remote edit never moves the window's private caret or selection"
        );
    }

    #[test]
    fn an_inserted_image_is_exported_in_the_docx() {
        let (mut app, png, hash) = image_document();
        insert_image(&mut app, &hash, Some("Intro")).expect("the picture is placed");
        app.settle();
        let output = app
            .effect(&super::WordProcessorEffect::ExportContent {
                format: "docx".into(),
            })
            .unwrap();
        let [address] = output.resources.as_slice() else {
            panic!("one content resource: {output:?}");
        };
        let docx = app
            .content()
            .read(tilo_actions::content_hash(address).expect("a content address"))
            .expect("the bytes are kept");
        let parts =
            tilo_doc_codecs::safety::read_zip_parts(&docx, &tilo_doc_codecs::ZipLimits::default())
                .unwrap();
        let media: Vec<&[u8]> = parts
            .iter()
            .filter(|(name, _)| name.starts_with("word/media/"))
            .map(|(_, bytes)| bytes.as_slice())
            .collect();
        assert_eq!(media, [png.as_slice()], "the inserted PNG is in word/media");
    }
}
