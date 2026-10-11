//! Ebook as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for the library.
//!
//! Document mutations (library prefs, remove-from-library, annotations,
//! reading settings / fonts / schemes) live on this resource via
//! [`Resource::bound_actions`] / [`Resource::children`]; edits bound to where
//! the reader is are the reader's bound declarations. Opening a book is the
//! reader's [`BooksEffect`]. The reader publishes only where it is in the
//! open book (`ebook.selection`), which changes no document.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, Effected, Fixture, LaunchInput, LiveRow, ResourceApp, ResourceSurface,
    SeedCx, SurfaceCx, ViewCx,
};
use ui::prelude::Effect;
use ui::tilo_actions::{ActionSpec, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{self, ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    EBOOK_ACTION_TYPE, EbookAction, bound_library_document_actions, publish_document_children,
    register,
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

    fn child(state: &EbookState, id: &str) -> Option<PublishedResource> {
        crate::actions::document_child(&state.entries, &state.annotations, id)
    }

    fn search(state: &EbookState) -> Vec<SearchEntry> {
        state
            .entries
            .iter()
            .map(|entry| {
                SearchEntry::new(entry.title.clone(), entry.author.clone())
                    .anchored(format!("ebook.book:{}", entry.id))
            })
            .collect()
    }
}

/// Books: the reader widget is the surface over the [`Ebook`] library.
pub struct EbookShell;

impl ResourceApp for EbookShell {
    type Resource = Ebook;
    const ID: &'static str = "so.tilo.ebook";
    const TITLE: &'static str = "Books";
    const ICON: Option<&'static str> = Some("ebook");
    const OPENS: &'static [&'static str] = &[EPUB_MEDIA_TYPE];
    /// A book to open (`ebook book.epub`).
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Ebook>) -> Element {
        cx.surface(BooksApp::new(cx.dispatcher(), bundled_library()))
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
        Ok(content) => Some(Invoke::effect(&BooksEffect::OpenStored {
            content,
            name: file.name().to_owned(),
        })),
        Err(error) => {
            tracing::warn!(%error, name = file.name(), "ebook: a handed book could not be kept");
            None
        }
    }
}

/// What Books does that changes no document by itself: what this reader
/// shows, opening books on this device, and sharing or fetching a book's
/// bytes through the workspace. Effects on one book, chapter or annotation
/// ride its row; going to or selecting a passage asks for the place in the
/// book whose row it rides.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "ebook.effect")]
pub enum BooksEffect {
    /// Open book
    ///
    /// Choose an EPUB through the system picker and open it.
    #[effect(job, cancellable, device_only)]
    OpenBook,
    /// Open a book the content store holds, putting it on the shelf.
    #[effect(job, device_only)]
    OpenStored {
        #[param("Content hash of the book's bytes")]
        content: String,
        #[param("The book's file name, for messages")]
        name: String,
    },
    /// Previous page
    #[effect(view, row)]
    PreviousPage,
    /// Next page
    #[effect(view, row)]
    NextPage,
    /// Toggle table of contents
    #[effect(view, row)]
    Contents,
    /// Toggle notes
    #[effect(view, row)]
    Notes,
    /// Open reading settings
    #[effect(view, row)]
    Settings,
    /// Open
    ///
    /// Shows a book at a chapter, page and block. A place that the book no
    /// longer has is refused rather than opened somewhere near it.
    #[effect(view, row)]
    Open {
        book: String,
        chapter: usize,
        page: usize,
        block: Option<usize>,
    },
    /// Go to
    ///
    /// Opens the book at a chapter and paragraph without selecting
    /// anything.
    #[effect(view, row)]
    GoTo {
        #[param("The book's id")]
        book: String,
        #[param("Chapter, counted from 0; the open chapter when omitted")]
        chapter: Option<usize>,
        #[param(
            "Block (paragraph or heading) to open at, counted from 0 in the chapter; its start when omitted"
        )]
        block: Option<usize>,
    },
    /// Select passage
    ///
    /// Selects a passage by paragraph and character, as a drag would, and
    /// shows its page.
    #[effect(view, row)]
    SelectPassage {
        #[param("The book's id")]
        book: String,
        #[param("Chapter, counted from 0; the open chapter when omitted")]
        chapter: Option<usize>,
        #[param(
            "Block (paragraph or heading) the selection starts in, counted from 0 in the chapter"
        )]
        start_block: usize,
        #[param("Character offset into the start block; 0 when omitted")]
        start_char: Option<usize>,
        #[param("Block the selection ends in; the start block when omitted")]
        end_block: Option<usize>,
        #[param("Exclusive character offset into the end block; the block's end when omitted")]
        end_char: Option<usize>,
    },
    /// Share with workspace
    ///
    /// Puts this device's copy of the book in the shared workspace, so
    /// collaborators can open it.
    #[effect(row, job, cancellable, idempotent)]
    Share { book: String },
    /// Download and open
    ///
    /// Fetches a book a collaborator shared from the workspace and opens it.
    #[effect(row, job, cancellable, idempotent)]
    Download { book: String },
}

impl ResourceSurface<Ebook, BooksEffect> for BooksApp {
    fn present(&mut self, state: &Rc<EbookState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Ebook>) {
        self.attach_host(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or("read"));
    }

    fn effect(&mut self, effect: BooksEffect, cx: &mut EffectCx<Ebook, Self>) -> EffectAnswer {
        match effect {
            BooksEffect::OpenBook => Ok(Self::choose_book(cx)),
            BooksEffect::OpenStored { content, name } => self.open_stored(content, name, cx),
            effect => {
                let invocation = cx.invocation().unwrap_or_default();
                let entries = cx.live().with(|state| state.entries.clone());
                let answer = self.begin_effect(invocation, effect, &entries)?;
                if matches!(answer, Effected::Pending) {
                    self.await_effect(invocation, cx.reply());
                }
                Ok(answer)
            }
        }
    }

    /// Where the reader is in the open book: its page chrome, a row beside
    /// the library.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.reading_row().into_iter().collect()
    }

    /// The reading position, or the book, chapter, annotation or location a
    /// search hit names, when the published page leaves it out.
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.reading_row()
            .filter(|row| row.id() == id)
            .or_else(|| self.search_row(id))
    }

    /// The library's edits bound to where the reader is: the page's
    /// bookmark and notes, and the selection's highlight and note.
    fn bound_declarations(&self) -> Vec<ActionDecl<Ebook>> {
        self.reading_declarations()
    }

    /// The books with their chapters' prose, their annotations and the
    /// library's remote books: what the reader holds, not the document.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.book_search_entries()
    }
}

/// A dispatcher of the library for tests that drive the reader (or one of
/// its editors) on a document of their own: its edits go through the same
/// permission check and executor a hosted reader's do.
#[cfg(test)]
pub(crate) fn library_dispatcher_for_test() -> ui::framework::Dispatcher<Ebook> {
    ui::framework::testing::dispatcher::<Ebook>(EbookShell::ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn the_library_publishes_its_door_and_the_reader_only_where_it_is() {
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

        let mut app = BooksApp::with_library(
            library_dispatcher_for_test(),
            crate::content::load_sample_library(),
        );
        let seeded = doc.clone();
        app.sync(&seeded, &ui::automerge::ROOT);
        let extras: Vec<_> = <BooksApp as ResourceSurface<Ebook, BooksEffect>>::live_rows(&app)
            .into_iter()
            .map(LiveRow::into_resource)
            .collect();
        assert_eq!(
            extras.len(),
            1,
            "the reader publishes where it is and nothing of the library: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        let book_id = app
            .open_book()
            .expect("sample library opens a book")
            .id
            .clone();
        assert_eq!(extras[0].id.as_str(), format!("ebook.selection:{book_id}"));
        for id in [
            "previous-page",
            "next-page",
            "contents",
            "notes",
            "settings",
            "select-passage",
            "go-to",
        ] {
            assert!(
                extras[0].actions.iter().any(|action| action.id == id),
                "the reader offers `{id}`"
            );
        }

        assert!(
            extras[0]
                .actions
                .iter()
                .all(|action| action.kind != OperationKind::Document),
            "where the reader is changes no document"
        );
        assert!(
            <BooksApp as ResourceSurface<Ebook, BooksEffect>>::live_row(&app, "ebook.settings")
                .is_none(),
            "the settings are the library's, published once"
        );

        let bound = |app: &BooksApp| -> Vec<&'static str> {
            app.reading_declarations()
                .iter()
                .map(|declaration| declaration.id)
                .collect()
        };
        assert_eq!(bound(&app), ["toggle-bookmark", "add-note"]);
        app.select_passage_for_test(0, 0, 0, 4);
        assert_eq!(
            bound(&app),
            [
                "toggle-bookmark",
                "add-note",
                "highlight-selection",
                "note-selection"
            ],
            "a selection binds its highlight and note"
        );

        let door = ui::tilo_resource::publish::<Ebook>(LIBRARY_RESOURCE_ID, "Books", &state);
        assert!(
            door.children
                .iter()
                .any(|child| child.id.as_str() == format!("ebook.book:{}", entry.id)),
            "publish children carry per-book document ops"
        );

        // Empty present must not wipe a local library that has not been published.
        let mut local = BooksApp::with_library(
            library_dispatcher_for_test(),
            crate::content::load_sample_library(),
        );
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

        let mut app = ui::framework::testing::TestApp::<EbookShell>::new().unwrap();
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
