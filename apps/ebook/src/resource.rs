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
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind, Resource as PublishedResource};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    EBOOK_ACTION_TYPE, EbookAction, bound_library_document_actions, library_effect_specs,
    publish_document_children, register,
};
use crate::app::BooksApp;
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
            })
            .collect()
    }
}

/// Framework app: the reader widget is the surface over [`Ebook`].
pub struct EbookShell;

impl ResourceApp for EbookShell {
    type Resource = Ebook;
    const ID: &'static str = "so.tilo.ebook";
    const TITLE: &'static str = "Books";
    const ICON: Option<&'static str> = Some("ebook");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Ebook>) -> Element {
        cx.surface(BooksApp::new())
    }

    fn label(state: &EbookState) -> String {
        if state.entries.is_empty() {
            "Books".into()
        } else {
            format!("{} books", state.entries.len())
        }
    }
}

impl ResourceSurface<Ebook> for BooksApp {
    fn present(&mut self, state: &Rc<EbookState>) {
        // Adopt shared shelf / settings only — never wipe a local library that
        // has not been published yet.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Ebook>) {
        self.attach_framework(cx.dispatcher());
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

pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<EbookShell>()
}

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
pub fn provide_resources(state: &EbookState) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Ebook>(
        LIBRARY_RESOURCE_ID,
        "Books",
        state,
    )]
}

pub fn standalone_launch() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" => return true,
            "--size" => {
                let _ = args.next();
                return true;
            }
            file if !file.starts_with('-') => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    #[test]
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
