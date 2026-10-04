//! Ebook as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for the library.
//!
//! Document mutations (library prefs, remove-from-library, annotations,
//! reading settings / fonts / schemes) live on this resource via
//! [`Resource::bound_actions`] / [`Resource::children`]. Open-book is an Effect
//! in [`Resource::local_operations`]. Hosted [`PublishesResources::resources`]
//! returns only the peer's live reading selection (`ebook.selection`) — never a
//! second hand-built `ebook.library` / `ebook.settings` root beside
//! [`tilo_resource::publish`]. Selection-bound highlight / note / bookmark and
//! page-chrome Views need the open book and stay on that live extra.

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, Fixture, LaunchInput, ResourceApp, ResourceSurface, SeedCx,
    SurfaceCx, ViewCx,
};
use ui::prelude::Effect;
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{self, ActionRef, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    EBOOK_ACTION_TYPE, EbookAction, bound_library_document_actions, library_effect_specs,
    publish_document_children, register,
};
use crate::app::BooksApp;
use crate::component::EPUB_MEDIA_TYPE;
use crate::doc::BooksDoc;
use crate::model::{Annotation, ColorScheme, FontEntry, LibraryEntry, LibraryPrefs, Settings};

/// Stable id for the published library root.
pub const LIBRARY_RESOURCE_ID: &str = "ebook.library";

/// Shared bookshelf projection: membership, reading settings, and annotations.
#[derive(Clone, Debug, Default)]
pub struct EbookState {
    pub entries: Vec<LibraryEntry>,
    pub settings: Settings,
    pub prefs: LibraryPrefs,
    pub fonts: Vec<FontEntry>,
    pub schemes: Vec<ColorScheme>,
    pub annotations: Vec<Annotation>,
}

/// The books library resource.
pub struct Ebook;

impl Resource for Ebook {
    const KIND: &'static str = "ebook.library";
    const LABEL: &'static str = "Books";
    const ACTION_TYPE: &'static str = EBOOK_ACTION_TYPE;

    type State = EbookState;
    type Action = EbookAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<EbookState, ModelError> {
        Ok(EbookState {
            entries: BooksDoc::read_library_entries(doc),
            settings: BooksDoc::read_settings(doc),
            prefs: BooksDoc::read_library_prefs(doc),
            fonts: BooksDoc::read_fonts(doc),
            schemes: BooksDoc::read_schemes(doc),
            annotations: BooksDoc::list_annotations(doc),
        })
    }

    fn bound_actions(state: &EbookState) -> Vec<ActionSpec> {
        bound_library_document_actions(state.prefs)
            .into_iter()
            .filter(|spec| spec.kind == OperationKind::Document)
            .collect()
    }

    fn children(state: &EbookState) -> Vec<PublishedResource> {
        publish_document_children(
            &state.entries,
            &state.annotations,
            &state.settings,
            &state.fonts,
            &state.schemes,
        )
    }

    fn local_operations() -> Vec<ActionSpec> {
        library_effect_specs()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &EbookState) -> Vec<SearchEntry> {
        state
            .entries
            .iter()
            .map(|entry| SearchEntry {
                anchor: Some(format!("ebook.book:{}", entry.id)),
                title: entry.title.clone(),
                text: entry.author.clone(),
                action: None,
            })
            .collect()
    }
}

/// Books: the reader widget is the surface over the [`Ebook`] library.
pub struct EbookShell;

/// Open the book whose bytes the content store holds as `content`, shelving
/// it first: what a launch on a book takes.
const OPEN_STORED: ActionRef<Ebook, (String, String)> =
    ActionRef::new("open-stored", &["content", "name"]);

impl ResourceApp for EbookShell {
    type Resource = Ebook;
    const ID: &'static str = "so.tilo.ebook";
    const TITLE: &'static str = "Books";
    const ICON: Option<&'static str> = Some("ebook");
    const OPENS: &'static [&'static str] = &[EPUB_MEDIA_TYPE];
    /// A book to open (`ebook book.epub`).
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Ebook>) -> Element {
        cx.surface(BooksApp::new(bundled_library()))
    }

    fn label(state: &EbookState) -> String {
        if state.entries.is_empty() {
            "Books".into()
        } else {
            format!("{} books", state.entries.len())
        }
    }

    /// A book handed over (opened with Books, named on the command line)
    /// is kept in the content store and opened from there, so the shelf row
    /// names its bytes and every mount of the library reads them.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Ebook>> {
        launch_file(input).into_iter().collect()
    }

    /// Where files have no paths (the browser), a new library holds the
    /// book compiled into the app: the one book there is to read.
    fn seed(_cx: &SeedCx<'_>) -> Vec<Invoke<Ebook>> {
        bundled_seed(bundled_library())
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn fixture(fixture: &mut Fixture<Ebook>) {
        crate::scenarios::fixture(fixture);
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Ebook>> {
        crate::scenarios::actions(name)
    }
}

/// Whether the host has no native files, so the one book there is to read is
/// the one compiled into the app: the browser.
pub(crate) fn bundled_library() -> bool {
    !crate::content::native_paths()
}

/// What a new library starts with: the bundled book where the library is
/// bundled, else nothing.
pub(crate) fn bundled_seed(bundled: bool) -> Vec<Invoke<Ebook>> {
    if !bundled {
        return Vec::new();
    }
    crate::content::bundled_book_entry()
        .and_then(|entry| Invoke::apply(&crate::actions::restore_library_book_action(&entry)).ok())
        .into_iter()
        .collect()
}

/// The launch's book, kept in the launch's content store, as the effect that
/// opens it.
pub(crate) fn launch_file(input: &LaunchInput<'_>) -> Option<Invoke<Ebook>> {
    let file = input.file()?;
    let Some(content) = input.content() else {
        tracing::warn!(
            name = file.name(),
            "ebook: no content store to keep a handed book in"
        );
        return None;
    };
    match content.put(file.bytes()) {
        Ok(hash) => Some(OPEN_STORED.with((hash, file.name().to_owned()))),
        Err(error) => {
            tracing::warn!(%error, name = file.name(), "ebook: a handed book could not be kept");
            None
        }
    }
}

/// What Books does that changes no document by itself.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "ebook.effect")]
pub enum BooksEffect {
    /// Open a book the content store holds, putting it on the shelf.
    #[effect(job)]
    OpenStored {
        #[param("Content hash of the book's bytes")]
        content: String,
        #[param("The book's file name, for messages")]
        name: String,
    },
}

impl ResourceSurface<Ebook, BooksEffect> for BooksApp {
    fn present(&mut self, state: &Rc<EbookState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Ebook>) {
        self.attach_host(&cx);
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or("read"));
    }

    fn effect(&mut self, effect: BooksEffect, cx: &mut EffectCx<Ebook, Self>) -> EffectAnswer {
        match effect {
            BooksEffect::OpenStored { content, name } => self.open_stored(content, name, cx),
        }
    }

    // Edits bound to where the user is publish as hand-built rows of the
    // widget's own rather than as `bound_declarations`.
    fn publishes_document_actions(&self) -> bool {
        true
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Ebook>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Ebook>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &EbookAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(
        dead_code,
        reason = "BooksApp tests read the last dispatch; the hosted path does not"
    )]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

/// The library's dispatcher as the framework hands it to the reader, for
/// tests that drive the reader (or one of its editors) on a document of
/// their own: its edits go through the same permission check and executor.
/// The headless app it came from is returned too, to outlive the test.
#[cfg(test)]
pub(crate) fn library_dispatcher_for_test() -> (
    ui::framework::testing::TestApp<impl ResourceApp<Resource = Ebook>>,
    Dispatcher<Ebook>,
) {
    use std::cell::RefCell;

    thread_local! {
        static ATTACHED: RefCell<Option<Dispatcher<Ebook>>> = const { RefCell::new(None) };
    }

    struct Probe;

    impl ResourceApp for Probe {
        type Resource = Ebook;
        const ID: &'static str = EbookShell::ID;
        const TITLE: &'static str = EbookShell::TITLE;

        fn view(cx: &mut ViewCx<Ebook>) -> Element {
            ATTACHED.with(|attached| *attached.borrow_mut() = Some(cx.dispatcher()));
            Box::new(ui::widget::Column::new())
        }
    }

    let app = ui::framework::testing::TestApp::<Probe>::new().expect("the library opens");
    let dispatcher = ATTACHED
        .with(|attached| attached.borrow_mut().take())
        .expect("the library's view was built");
    (app, dispatcher)
}

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
pub fn provide_resources(state: &EbookState) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Ebook>(
        LIBRARY_RESOURCE_ID,
        "Books",
        state,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn resource_publish_owns_library_door_and_hosted_keeps_selection_extras() {
        let mut doc = AutoCommit::new();
        let entry = LibraryEntry {
            id: "book:0123456789abcdef0123456789abcdef".into(),
            shared: false,
            title: "Published Door".into(),
            author: "Tester".into(),
            subject: String::new(),
            added: 1,
            content: None,
        };
        restore_and_annotate(&mut doc, &entry);

        let state = Ebook::read(&doc).expect("seeded ebook state");
        let published = ui::tilo_resource::publish::<Ebook>(LIBRARY_RESOURCE_ID, "Books", &state);
        assert_eq!(published.kind, "ebook.library");
        for id in [
            "open-book",
            "toggle-hide-read",
            "sort-title",
            "sort-author",
            "sort-recent",
        ] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework library resource"
            );
        }
        let book = published
            .children
            .iter()
            .find(|child| child.kind == "ebook.book")
            .expect("book child on the published door");
        assert!(
            book.actions
                .iter()
                .any(|action| action.id == "remove-from-library"),
            "remove-from-library Document op must ride the book child"
        );
        assert!(
            book.children.iter().any(|child| {
                child.kind == "ebook.highlight"
                    && child
                        .actions
                        .iter()
                        .any(|action| action.id == "delete-highlight")
            }),
            "highlight Document ops nest under the book"
        );
        let settings = published
            .children
            .iter()
            .find(|child| child.kind == "ebook.settings")
            .expect("settings child on the published door");
        assert!(
            settings
                .actions
                .iter()
                .any(|action| action.id.starts_with("theme-")),
            "reading-theme Document ops must be on settings"
        );

        let mut app = BooksApp::with_library(crate::content::load_sample_library());
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert!(
            standalone.iter().any(|r| r.id.as_str() == "ebook.library"),
            "standalone keeps the library root"
        );
        assert!(
            standalone
                .iter()
                .find(|r| r.id.as_str() == "ebook.library")
                .is_some_and(|r| r.actions.iter().any(|a| a.id == "toggle-hide-read")),
            "standalone library keeps Document prefs"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &seeded, "ebook.library").is_none());
        let hosted_settings = ui::resources::resource(&app, &seeded, "ebook.settings")
            .expect("hosted settings stay the published row");
        assert!(
            hosted_settings
                .actions
                .iter()
                .any(|action| action.id.starts_with("theme-")),
            "hosted settings keep the published theme actions"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "ebook.library"
                    && resource.kind != "ebook.settings"),
            "hosted must not republish ebook.library/settings beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(
            extras.len(),
            1,
            "hosted returns the reading-selection slice alone"
        );
        assert_eq!(extras[0].kind, "ebook.selection");
        let book_id = app
            .open_book()
            .expect("sample library opens a book")
            .id
            .clone();
        assert_eq!(extras[0].id.as_str(), format!("ebook.selection:{book_id}"));
        for id in [
            "toggle-bookmark",
            "add-note",
            "previous-page",
            "next-page",
            "contents",
            "notes",
            "settings",
        ] {
            assert!(
                extras[0].actions.iter().any(|action| action.id == id),
                "hosted keeps open-book chrome `{id}`: {:?}",
                extras[0]
                    .actions
                    .iter()
                    .map(|a| a.id.as_str())
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            !extras[0]
                .actions
                .iter()
                .any(|action| action.id == "remove-from-library" || action.id == "toggle-hide-read"),
            "hosted selection must not copy shelf Document ops already on publish"
        );

        app.select_passage_for_test(0, 0, 0, 4);
        let with_sel = ui::resources::PublishesResources::resources(&app, &seeded);
        assert_eq!(with_sel.len(), 1);
        assert!(
            with_sel[0]
                .actions
                .iter()
                .any(|action| action.id == "note-selection"),
            "hosted selection binds note-selection when text is selected"
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "ebook.library");
        assert!(
            door[0]
                .actions
                .iter()
                .any(|action| action.id == "toggle-hide-read")
        );
        assert!(
            door[0]
                .children
                .iter()
                .any(|child| child.id.as_str() == format!("ebook.book:{}", entry.id)),
            "publish children carry per-book document ops"
        );

        // Empty present must not wipe a local library that has not been published.
        let mut local = BooksApp::with_library(crate::content::load_sample_library());
        let before = local.library().len();
        assert!(before > 0, "sample app starts with a local library");
        local.adopt_resource_state(&EbookState::default());
        assert_eq!(
            local.library().len(),
            before,
            "adopting an empty projection must not clear unpublished books"
        );
    }

    /// A note's text is a rich-text resource beneath the note: the palette,
    /// agents and scripts edit it there, and each edit is the library's
    /// action, undone as one.
    #[test]
    fn a_notes_text_is_published_beneath_it_and_edits_as_the_librarys_action() {
        use tilo_document::{RichText, RichTextAction};
        use ui::tilo_resource::Invoke;

        let (mut app, _) = library_dispatcher_for_test();
        let entry = LibraryEntry {
            id: "book:0123456789abcdef0123456789abcdef".into(),
            shared: false,
            title: "Annotated".into(),
            author: "Tester".into(),
            subject: String::new(),
            added: 1,
            content: None,
        };
        let id = uuid::Uuid::from_u128(0xE800_0001_0002_0003_0004_0005_0006_0008);
        let note = crate::actions::NoteDraft {
            book_id: entry.id.clone(),
            id,
            anchor: crate::model::AnnotationAnchor {
                chapter: 0,
                block: 0,
                start: 0,
                end: 4,
                excerpt: "door".into(),
            },
            color: crate::model::HiColor::Yellow,
            text: "first".into(),
        };
        app.invoke(Invoke::apply(&crate::actions::restore_library_book_action(&entry)).unwrap())
            .unwrap();
        app.invoke(Invoke::apply(&note.into_action()).unwrap())
            .unwrap();
        let text = |state: EbookState| {
            state
                .annotations
                .into_iter()
                .find(|annotation| annotation.id == id)
                .map(|annotation| annotation.note)
        };
        assert_eq!(text(app.state()).as_deref(), Some("first"));

        let state = app.state();
        let published = ui::tilo_resource::publish::<Ebook>(LIBRARY_RESOURCE_ID, "Books", &state);
        let note_row = published
            .children
            .iter()
            .flat_map(|book| &book.children)
            .find(|child| child.id.as_str() == format!("ebook.note:{id}"))
            .expect("the note is published under its book");
        let body = note_row
            .children
            .iter()
            .find(|child| child.id.as_str() == crate::actions::note_text_resource_id(id))
            .expect("the note's text is published beneath it");
        assert_eq!(body.kind, RichText::KIND);

        app.invoke_child::<RichText>(
            &crate::actions::note_text_resource_id(id),
            Invoke::<RichText>::apply(&RichTextAction::ReplaceBody {
                markdown: "from an agent".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(text(app.state()).as_deref(), Some("from an agent"));
        assert!(app.undo());
        assert_eq!(text(app.state()).as_deref(), Some("first"));
    }

    /// The open reader shows its page to assistive technology as a document
    /// named after the open chapter.
    fn reads_chapter(app: &mut ui::framework::testing::TestApp<EbookShell>, title: &str) -> bool {
        app.harness()
            .wait_until(ui::time::Duration::from_secs(10), |harness| {
                harness.a11y_node_named(title).is_some()
            })
    }

    #[test]
    fn launching_on_an_epub_keeps_it_in_the_content_store_shelves_and_opens_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("handed-off.epub");
        std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();
        let mut app =
            ui::framework::testing::TestApp::<EbookShell>::launched(&[path.to_str().unwrap()])
                .unwrap();
        let id = crate::epub::content_id(crate::content::SAMPLE_EPUB);
        let book = crate::component::book_resource(&id);
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(10), |harness| harness
                    .bound_resources(Some(&book))
                    .is_ok_and(|resources| !resources.is_empty())),
            "the handed-off book joins the library"
        );
        let first_chapter = crate::content::bundled_sample().unwrap().chapters[0]
            .title
            .clone();
        assert!(
            reads_chapter(&mut app, &first_chapter),
            "the handed-off book opens in the reader"
        );
        let shelf = app.state().entries;
        assert_eq!(shelf.len(), 1, "and is now on the shelf: {shelf:?}");
        assert_eq!(
            shelf[0].content.as_deref(),
            Some(tilo_content::hash_bytes(crate::content::SAMPLE_EPUB).as_str()),
            "its row names the bytes the content store keeps, not a path"
        );

        // Reopened with nothing named, the shelf still reads it.
        let mut again = app.reopen().unwrap();
        assert_eq!(again.state().entries.len(), 1);
        assert!(
            again
                .harness()
                .wait_until(ui::time::Duration::from_secs(10), |harness| harness
                    .bound_resources(Some(&book))
                    .is_ok_and(|resources| !resources.is_empty())),
            "the reopened library reads the book from the content store"
        );
    }

    #[test]
    fn every_scenario_stages_the_sample_shelf_through_the_librarys_actions() {
        for scenario in
            std::iter::once(None).chain(crate::scenarios::SCENARIOS.iter().copied().map(Some))
        {
            let app = ui::framework::testing::TestApp::<EbookShell>::capture(scenario)
                .unwrap_or_else(|error| panic!("{scenario:?}: {error}"));
            let shelf = app.state().entries.len();
            if scenario == Some("onboarding") {
                assert_eq!(shelf, 0, "the first start is an empty shelf");
            } else {
                assert_eq!(shelf, 3, "{scenario:?} shows the sample shelf");
            }
        }
        let mut comments =
            ui::framework::testing::TestApp::<EbookShell>::capture(Some("comments")).unwrap();
        assert!(
            comments.harness().a11y_nodes().iter().any(|(_, node)| node
                .label()
                .is_some_and(|label| label.contains("colour that keeps the time of day"))),
            "the comments scenario has the highlight's comments open"
        );
    }

    fn restore_and_annotate(doc: &mut AutoCommit, entry: &LibraryEntry) {
        BooksDoc::upsert_library_entry(doc, entry);
        let group = uuid::Uuid::from_u128(0xE800_0001_0002_0003_0004_0005_0006_0007);
        let highlight = Annotation {
            id: group,
            doc_id: entry.id.clone(),
            kind: crate::model::AnnoKind::Highlight,
            chapter: 0,
            block: 0,
            start: 0,
            end: 4,
            excerpt: "door".into(),
            note: String::new(),
            color: crate::model::HiColor::Yellow,
            created: 1,
            group,
            blocks: Vec::new(),
        };
        BooksDoc::upsert_annotation(doc, &highlight);
        doc.commit();
    }
}
