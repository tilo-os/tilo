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

use tilo_actions::{ActionParam, ParamType};
use tilo_document::{RichText, RichTextAction, RichTextState};
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::record::RecordId;
use ui::tilo_resource::{ActionDecl, Demand, Doc, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::WordProcessorWidget;
use crate::effects::WordProcessorEffect;

/// Rich text plus the images, tables and headings that document already holds.
///
/// The record kind stays `tilo.rich-text`. The editor's chrome (find, open,
/// save) is the editor's [`crate::effects::WordProcessorEffect`]. Structural objects,
/// notes, review, paragraph styles, and the page header and footer are
/// [`Resource::children`], so a hosted palette does not rebuild them beside
/// the document.
pub struct Document;

impl Resource for Document {
    const KIND: &'static str = <RichText as Resource>::KIND;
    const LABEL: &'static str = <RichText as Resource>::LABEL;
    const ACTION_TYPE: &'static str = <RichText as Resource>::ACTION_TYPE;

    type State = RichTextState;
    type Action = RichTextAction;

    /// A short document is read whole, so its document operations find a
    /// paragraph by its text without a view (`insert-image` after one); a
    /// view's own demand then keeps only what it shows.
    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        let mut state = <RichText as Resource>::read(doc)?;
        let len = state.body().len();
        if len <= crate::actions::MAX_FOUND_PARAGRAPHS {
            <RichText as Resource>::load(&mut state, doc, &Demand::range(0..len))?;
        }
        Ok(state)
    }

    fn locate(state: &Self::State, obj: &automerge::ObjId) -> Option<ui::tilo_resource::Path> {
        <RichText as Resource>::locate(state, obj)
    }

    fn lands(state: &Self::State, action: &Self::Action) -> Option<ui::tilo_resource::Place> {
        <RichText as Resource>::lands(state, action)
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        <RichText as Resource>::initialize(doc)
    }

    fn refresh(
        state: &mut Self::State,
        doc: &Doc,
        delta: &ui::tilo_resource::Delta,
    ) -> Result<(), ModelError> {
        <RichText as Resource>::refresh(state, doc, delta)
    }

    fn load(state: &mut Self::State, doc: &Doc, demand: &Demand) -> Result<(), ModelError> {
        <RichText as Resource>::load(state, doc, demand)
    }

    fn coalesces(previous: &Self::Action, next: &Self::Action) -> bool {
        <RichText as Resource>::coalesces(previous, next)
    }

    fn least_permission() -> ui::tilo_resource::PermissionRequirement {
        <RichText as Resource>::least_permission()
    }

    fn permission(action: &Self::Action) -> ui::tilo_resource::PermissionRequirement {
        <RichText as Resource>::permission(action)
    }

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        <RichText as Resource>::register(registry);
    }

    fn action_schema() -> Option<serde_json::Value> {
        <RichText as Resource>::action_schema()
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        let mut decls: Vec<ActionDecl<Self>> = <RichText as Resource>::declarations()
            .into_iter()
            .map(|decl| ActionDecl {
                id: decl.id,
                label: decl.label,
                description: decl.description,
                params: decl.params,
                permission: decl.permission,
                destructive: decl.destructive,
                result: decl.result,
                tags: decl.tags,
                content: decl.content,
                refines: decl.refines,
                build: decl.build,
            })
            .collect();
        decls.push(
            ActionDecl::new(
                "set-page-margins",
                "Set page margins",
                vec![
                    ActionParam::required_owned("top", ParamType::Number, "Top margin in px"),
                    ActionParam::required_owned("right", ParamType::Number, "Right margin in px"),
                    ActionParam::required_owned("bottom", ParamType::Number, "Bottom margin in px"),
                    ActionParam::required_owned("left", ParamType::Number, "Left margin in px"),
                ],
                |args, _| {
                    let margin = |name: &str| {
                        args.number(name)
                            .ok_or_else(|| format!("missing {name} margin"))
                    };
                    Ok(RichTextAction::SetPageMargins {
                        top: margin("top")?,
                        right: margin("right")?,
                        bottom: margin("bottom")?,
                        left: margin("left")?,
                    })
                },
            )
            .describe("Set the four page margins, in pixels."),
        );
        decls
    }

    fn views(state: &Self::State) -> Vec<tilo_actions::ResourceViewSpec> {
        <RichText as Resource>::views(state)
    }

    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        let mut specs = <RichText as Resource>::local_operations();
        specs.push(crate::semantic_read::action::spec());
        specs
    }

    fn search(state: &Self::State) -> Vec<ui::tilo_resource::SearchEntry> {
        <RichText as Resource>::search(state)
    }

    fn children(state: &Self::State) -> Vec<tilo_actions::Resource> {
        crate::actions::structural_children(state)
    }

    fn bound_actions(state: &Self::State) -> Vec<tilo_actions::ActionSpec> {
        crate::actions::toc_action(state)
            .into_iter()
            .chain([crate::actions::insert_image_spec(state)])
            .collect()
    }
}

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
        Ok(action) => Invoke::apply(&action).into_iter().collect(),
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
    fn label(state: &RichTextState) -> String {
        state
            .headings()
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
        let (Some(file), Some(state)) = (input.file(), input.record::<RichTextState>()) else {
            return Vec::new();
        };
        if created || state.file_digest() == Some(tilo_content::hash_bytes(file.bytes()).as_str()) {
            return Vec::new();
        }
        import(input)
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
            .filter_map(|text| {
                Invoke::apply(&RichTextAction::AppendParagraph {
                    text: (*text).to_owned(),
                })
                .ok()
            })
            .collect()
    }
}

impl ResourceSurface<Document, WordProcessorEffect> for WordProcessorWidget {
    /// The editor reads the live state where it needs it; a new state only
    /// asks for a redraw.
    fn present(&mut self, state: &Rc<RichTextState>) {
        // An editor that has not drawn yet still has a caret, at the start
        // of the body, so edits bound to it (a table at the caret) can be
        // invoked before the first frame.
        if self.model().caret().block.is_empty()
            && let Some(first) = state.body().id_at(0)
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
        self.follow_personal_dictionary(cx.settings());
        // A past version and a capture show the layout they are given.
        if cx.mode() == RunMode::Live {
            self.follow_preferences(cx.settings(), cx.settings());
        }
        self.attach_framework(cx.live(), cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    /// Edits bound to the caret and selection of whoever invokes them.
    fn bound_declarations(&self) -> Vec<ActionDecl<Document>> {
        self.bound_edits()
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
            .filter(|row| row["resource"] == "tilo.rich-text")
            .filter_map(|row| row["action"].as_str())
            .collect();
        for id in ["find", "open", "save", "save-as", "zoom-in", "move-caret"] {
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
        use ui::tilo_resource::{Effect, Invoke, Resource};
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
        app.invoke(
            Invoke::apply(&super::RichTextAction::AppendParagraph {
                text: "Exported without a file".into(),
            })
            .unwrap(),
        )
        .unwrap();
        app.settle();
        let png = ui::png::encode_rgba8(2, 1, &[255, 160, 40, 255, 30, 70, 180, 255]).unwrap();
        let hash = app.content().put(&png).unwrap();
        let resource = <super::Document as Resource>::KIND;
        app.call_on(
            resource,
            "place-image",
            serde_json::json!({tilo_actions::PROMPTED_PARAM: hash}),
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
        use ui::tilo_resource::Resource;
        let decl = super::Document::declarations()
            .into_iter()
            .find(|decl| decl.id == "set-page-margins")
            .expect("page margins are a document action");
        let supplied = serde_json::json!({
            "top": 72.0,
            "right": 54.0,
            "bottom": 72.0,
            "left": 54.0
        });
        let supplied = supplied.as_object().expect("margin object");
        let args = tilo_actions::ActionArgs::bind_json(decl.params.as_slice(), supplied)
            .expect("margin arguments");
        let action = (decl.build)(&args, 0).expect("the declaration builds an action");
        let mut doc = automerge::AutoCommit::new();
        <super::Document as Resource>::initialize(&mut doc).expect("document initializes");
        ui::tilo_actions::Action::apply(&action, &mut doc).expect("margins apply");
        let containers = tilo_doc::model::open(&doc, &automerge::ROOT).expect("document opens");
        assert_eq!(
            tilo_doc::model::meta_str(&doc, &containers, "page_margin_top").as_deref(),
            Some("72")
        );
        assert_eq!(
            tilo_doc::model::meta_str(&doc, &containers, "page_margin_right").as_deref(),
            Some("54")
        );
    }

    #[test]
    fn read_text_is_published_with_the_document() {
        use ui::tilo_resource::Resource;
        let mut doc = automerge::AutoCommit::new();
        <super::Document as Resource>::initialize(&mut doc).expect("document initializes");
        let state = <super::Document as Resource>::read(&doc).expect("document reads");
        let published =
            ui::tilo_resource::publish::<super::Document>("tilo.rich-text", "Document", &state);
        let read = published
            .actions
            .iter()
            .find(|action| action.id == "read-text")
            .expect("read-text is on the published document");
        assert_eq!(read.kind, tilo_actions::OperationKind::Effect);
    }

    /// The running Writer with three paragraphs, a picture in its content
    /// store, and an agent joined from its record with write access, as an
    /// agent's worker holds it: no view and no caret.
    fn joined() -> (
        ui::framework::testing::TestApp<WordProcessorApp>,
        ui::framework::Participant,
        Vec<u8>,
        String,
    ) {
        use ui::permissions::{Actor, CapOverride, PermissionContext, Scope, Subject, Verb};
        use ui::tilo_resource::Invoke;
        let mut app = ui::framework::testing::TestApp::<WordProcessorApp>::new().unwrap();
        for text in ["Intro", "The lamp in use", "Closing"] {
            app.invoke(
                Invoke::apply(&super::RichTextAction::AppendParagraph { text: text.into() })
                    .unwrap(),
            )
            .unwrap();
        }
        app.settle();
        let png = ui::png::encode_rgba8(2, 1, &[255, 160, 40, 255, 30, 70, 180, 255]).unwrap();
        let hash = app.content().put(&png).unwrap();
        let snapshot = app.read_document(|doc| doc.clone().save());
        let agent = PermissionContext::from_capabilities(
            Actor::agent("carrier"),
            vec![CapOverride::grant(
                Subject::agent("carrier"),
                Scope::App(<WordProcessorApp as ui::framework::ResourceApp>::ID.to_owned()),
                Verb::Write,
            )],
        );
        let participant = ui::framework::Participant::from_snapshot(
            ui::framework::resource_kind::<WordProcessorApp>(),
            &snapshot,
            "carrier",
            agent,
        )
        .unwrap();
        (app, participant, png, hash)
    }

    fn insert_image(
        participant: &mut ui::framework::Participant,
        hash: &str,
        after: Option<&str>,
    ) -> Result<Option<Vec<u8>>, tilo_actions::DoorError> {
        let mut arguments = serde_json::Map::new();
        arguments.insert(tilo_actions::PROMPTED_PARAM.into(), hash.into());
        if let Some(after) = after {
            arguments.insert("after".into(), after.into());
        }
        participant.invoke(
            &tilo_actions::ActionAddress::new(
                <super::Document as ui::tilo_resource::Resource>::KIND,
                "insert-image",
            ),
            &arguments,
        )
    }

    /// Each body block's kind, its text, and the picture it shows.
    fn body(doc: &automerge::AutoCommit) -> Vec<(String, String, Option<String>)> {
        let containers = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
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

    /// An agent places the picture the step before it produced through the
    /// record's own document operation: after the paragraph it names by its
    /// text, or at the end; content it does not hold and text no paragraph
    /// has are refused before anything is written.
    #[test]
    fn an_agent_inserts_produced_content_after_a_paragraph_or_at_the_end() {
        let (app, mut participant, _png, hash) = joined();
        let operation = participant
            .operations()
            .into_iter()
            .find(|operation| operation.address.action == "insert-image")
            .expect("the record publishes insert-image as a document operation");
        assert!(operation.input.wants_content(), "the image is carried");
        assert!(
            operation
                .params
                .iter()
                .any(|param| param.name == "after" && !param.required)
        );
        assert_eq!(
            operation.contract.permission,
            tilo_actions::PermissionRequirement::Write
        );

        let store = app.content();
        let window = ui::WakeScope::new();
        ui::tilo_resource::content::register(&window.handle(), store);
        let _entered = window.enter();

        let unknown = tilo_content::hash_bytes(b"never produced");
        let error = insert_image(&mut participant, &unknown, None).unwrap_err();
        assert!(error.to_string().contains("not available"), "{error}");
        let error = insert_image(&mut participant, &hash, Some("no such words")).unwrap_err();
        assert!(
            error.to_string().contains("no paragraph contains"),
            "{error}"
        );
        assert!(
            body(participant.doc())
                .iter()
                .all(|(kind, ..)| kind != "image"),
            "a refusal writes nothing"
        );

        insert_image(&mut participant, &hash, Some("lamp in"))
            .expect("the picture is placed after the paragraph");
        let blocks = body(participant.doc());
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

        insert_image(&mut participant, &format!("tilo.content:{hash}"), None)
            .expect("without `after` the picture is appended");
        let blocks = body(participant.doc());
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

    /// The picture an agent inserted travels into the .docx the record is
    /// exported as, byte for byte.
    #[test]
    fn an_inserted_image_is_exported_in_the_docx() {
        let (mut app, mut participant, png, hash) = joined();
        {
            let window = ui::WakeScope::new();
            ui::tilo_resource::content::register(&window.handle(), app.content());
            let _entered = window.enter();
            insert_image(&mut participant, &hash, Some("Intro")).expect("the picture is placed");
        }
        let mut peer = participant.doc().clone();
        app.merge_peer(&mut peer).unwrap();
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
