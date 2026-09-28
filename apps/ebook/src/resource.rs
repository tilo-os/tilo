//! Ebook as a [`tilo_resource::Resource`]: the shared shelf is the state,
//! annotations and membership are document actions, reading position presentation
//! is View.

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{EBOOK_ACTION_TYPE, EbookAction, register};
use crate::app::BooksApp;
use crate::doc::BooksDoc;
use crate::model::{LibraryEntry, LibraryPrefs, Settings};

/// Shared bookshelf projection.
#[derive(Clone, Debug, Default)]
pub struct EbookState {
    pub entries: Vec<LibraryEntry>,
    pub settings: Settings,
    pub prefs: LibraryPrefs,
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
        })
    }

    fn children(state: &EbookState) -> Vec<tilo_actions::Resource> {
        state
            .entries
            .iter()
            .take(64)
            .map(|entry| {
                tilo_actions::Resource::new(
                    format!("ebook.book:{}", entry.id),
                    "ebook.book",
                    entry.title.clone(),
                )
            })
            .collect()
    }

    fn local_operations() -> Vec<ActionSpec> {
        crate::actions::provide_resources(&[], &AutoCommit::new())
            .into_iter()
            .flat_map(|resource| resource.actions)
            .filter(|spec| spec.kind != OperationKind::Document)
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
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Ebook>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Ebook>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Ebook>) {
        self.framework = Some(dispatcher);
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

    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}

pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<EbookShell>()
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
