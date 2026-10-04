//! Notes as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. Nested page bodies are rich-text child resources; the surface presents
//! the framework's workspace state and submits edits through the dispatcher.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::{ActionSpec, EncodedAction, OperationKind};
use tilo_document::{RichText, RichTextAction};
use ui::application::OpenFile;
use ui::framework::{Dispatcher, LaunchInput, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::headless::FrameHarness;
use ui::permissions::UserId;
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
// `Resource` is in scope for both `Notebook` and nested `RichText`.
use crate::actions::{
    NOTES_ACTION_TYPE, NOTES_OPERATION_TYPE, NotesAction, NotesOperation, encode,
};
use crate::app::NotesApp;
use crate::doc::NotesDoc;
use crate::model::{PageId, Workspace};
use ui::widget::Element;

/// The notebook resource — pages and their nested rich-text bodies.
pub struct Notebook;

impl Resource for Notebook {
    const KIND: &'static str = "notes.notebook";
    const LABEL: &'static str = "Notebook";
    const ACTION_TYPE: &'static str = NOTES_ACTION_TYPE;

    type State = Workspace;
    type Action = NotesAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        crate::actions::register(registry);
        RichText::register(registry);
    }

    fn read(doc: &Doc) -> Result<Workspace, ModelError> {
        let mut workspace = Workspace::default();
        workspace.data.pages = NotesDoc::list_pages(doc);
        workspace.data.tags = NotesDoc::list_tags(doc);
        workspace.data.tag_assignments = NotesDoc::list_tag_assignments(doc)
            .into_iter()
            .map(|(page, tag)| crate::model::TagAssignment { page, tag })
            .collect();
        workspace.bodies = std::sync::Arc::new(crate::actions::read_bodies(doc, workspace.pages()));
        Ok(workspace)
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        crate::notebook::initialize_empty(doc).map_err(ModelError::invalid)?;
        Ok(())
    }

    // The notebook's own edits never continue one another; typing in a
    // note continues through its body's rich text.
    fn continuable(_action: &NotesAction) -> bool {
        false
    }

    fn bound_declarations(state: &Workspace) -> Vec<ActionDecl<Self>> {
        crate::actions::notebook_bound_declarations(state)
    }

    fn bound_actions(state: &Workspace) -> Vec<ActionSpec> {
        crate::actions::notebook_bound_actions(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        [
            ("back", "Back", NotesOperation::NavigateBack),
            ("forward", "Forward", NotesOperation::NavigateForward),
        ]
        .into_iter()
        .filter_map(|(id, label, operation)| {
            ActionSpec::operation(
                id,
                label,
                OperationKind::View,
                NOTES_OPERATION_TYPE,
                &operation,
            )
            .ok()
        })
        .chain([
            crate::actions::file_operation("import-markdown", "Import Markdown", None),
            crate::actions::markdown_content_operation(),
        ])
        .collect()
    }

    fn children(state: &Workspace) -> Vec<tilo_actions::Resource> {
        crate::actions::notebook_children(state)
    }

    fn child(state: &Workspace, id: &str) -> Option<tilo_actions::Resource> {
        crate::actions::notebook_child(state, id)
    }

    fn search(state: &Workspace) -> Vec<SearchEntry> {
        state
            .pages()
            .iter()
            .map(|page| SearchEntry {
                anchor: Some(format!("notes.page:{}", page.id)),
                title: page.title.clone(),
                text: String::new(),
                action: None,
            })
            .collect()
    }
}

/// Framework app: the notes widget is the surface over [`Notebook`].
pub struct NotesShell;

impl ResourceApp for NotesShell {
    type Resource = Notebook;
    const ID: &'static str = "so.tilo.notes";
    const TITLE: &'static str = "Notes";
    const ICON: Option<&'static str> = Some("notes");
    const OPENS: &'static [&'static str] = &["text/markdown", "text/x-markdown"];
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Notebook>) -> Element {
        // `present` installs the framework's workspace before the first frame.
        let author = UserId(cx.me().unwrap_or("local").to_owned());
        let mut app = NotesApp::empty_surface(&author, cx.save_status());
        if let Some((page, _)) = launched_note(&cx.launch()) {
            app.open_when_present(page);
        }
        cx.surface(app)
    }

    /// A Markdown file the app is launched on becomes a note.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Notebook>> {
        let Some((page, file)) = launched_note(input) else {
            return Vec::new();
        };
        let Ok(markdown) = std::str::from_utf8(file.bytes()) else {
            tracing::warn!(
                file = file.name(),
                "notes: the launched file is not UTF-8 Markdown"
            );
            return Vec::new();
        };
        let title = std::path::Path::new(file.name())
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or("Imported note");
        // Images next to a file named on the command line come with it.
        let (markdown, assets) = input
            .argument()
            .map(std::path::Path::new)
            .and_then(|path| {
                crate::app::adopt_canonical_markdown_assets(path, markdown, None)
                    .map_err(|error| tracing::warn!(%error, "notes: launched images stay behind"))
                    .ok()
            })
            .unwrap_or_else(|| (markdown.to_owned(), Vec::new()));
        Invoke::apply(&crate::actions::import_markdown_action(
            page, title, &markdown, assets,
        ))
        .into_iter()
        .collect()
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenario::SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            // The drawer only exists below the narrow breakpoint.
            Some("drawer") => (430, 860),
            _ => crate::scenario::VIEWPORT,
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Notebook>> {
        crate::scenario::actions(name)
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }

    fn scenario_input(name: Option<&str>, input: &mut FrameHarness) -> Result<(), String> {
        crate::scenario::input(name, input)
    }
}

/// The note a launch on a Markdown file imports. The same file always names
/// the same note, so launching on it again opens the note it became rather
/// than importing it twice.
fn launched_note<'a>(input: &LaunchInput<'a>) -> Option<(PageId, &'a OpenFile)> {
    let file = input.file()?;
    let digest = blake3::Hasher::new()
        .update(file.name().as_bytes())
        .update(&[0])
        .update(file.bytes())
        .finalize();
    let bytes = *digest.as_bytes().first_chunk::<16>()?;
    Some((uuid::Builder::from_random_bytes(bytes).into_uuid(), file))
}

/// Framework dispatcher when hosted; otherwise the local action host, for
/// hosts that mount notes without the framework.
#[derive(Default)]
pub(crate) struct ActionSink {
    local: LocalSink,
    framework: Option<Dispatcher<Notebook>>,
    last: Option<EncodedAction>,
}

/// The local action host, shared with the page body editor.
pub(crate) type LocalSink = Rc<RefCell<LocalActions>>;

/// Notes' own action host and how body typing groups into undo steps on it.
#[derive(Default)]
pub(crate) struct LocalActions {
    host: ui::ActionHost,
    body: BodySteps,
}

/// How the body edits applied through the local host group into undo steps:
/// typing a word is one step, as through the framework's dispatcher.
#[derive(Default)]
struct BodySteps {
    /// The last body edit, to tell whether the next continues it.
    last: Option<RichTextAction>,
    /// Every body edit since the last boundary continued the one before it.
    continuing: bool,
    /// A body edit was applied since the last boundary was taken.
    applied: bool,
    /// The last boundary opened a step that stays open.
    opened: bool,
}

impl LocalActions {
    /// Apply `edit` to `page`'s body as the notebook's action.
    pub(crate) fn apply_body_edit(
        &mut self,
        doc: &mut AutoCommit,
        page: PageId,
        edit: &RichTextAction,
    ) -> Result<(), tilo_actions::ActionError> {
        let action = crate::actions::edit_body_action(page, edit.clone());
        let context = crate::actions::history_context(&action);
        self.host
            .try_dispatch_with_context(doc, NOTES_ACTION_TYPE, &action, context)?;
        if RichText::annotates(edit) {
            // Part of the edit that wrote the text, not a step of its own.
            return Ok(());
        }
        let steps = &mut self.body;
        let continues = steps
            .last
            .as_ref()
            .is_some_and(|previous| RichText::coalesces(previous, edit));
        steps.last = Some(edit.clone());
        let first_of_event = !std::mem::replace(&mut steps.applied, true);
        steps.continuing = continues && (first_of_event || steps.continuing);
        Ok(())
    }

    /// The undo boundary before this event's first action; `None` while
    /// typing continues the open step.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<ChangeHash>> {
        let bracket = self.host.take_committed();
        let steps = &mut self.body;
        if !std::mem::take(&mut steps.applied) {
            // Anything else ends the word being typed.
            steps.last = None;
            steps.continuing = false;
            steps.opened = false;
            return bracket;
        }
        let continuing = std::mem::take(&mut steps.continuing);
        // Only typing continues into the next event; a split, a move or a
        // format is a step of its own, closed now.
        let continuable = matches!(steps.last, Some(RichTextAction::SpliceText { .. }));
        steps.opened = bracket.is_some() && !continuing && continuable;
        if continuing { None } else { bracket }
    }

    /// Whether the step typing opened stays open for the next letters.
    pub(crate) fn keeps_step_open(&self) -> bool {
        self.body.opened
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Notebook>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    /// The local host, for the page body editor of a notebook mounted
    /// without the framework.
    pub(crate) fn local(&self) -> LocalSink {
        Rc::clone(&self.local)
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &NotesAction) {
        let context = crate::actions::history_context(action);
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => {
                self.local.borrow_mut().host.dispatch_with_context(
                    doc,
                    NOTES_ACTION_TYPE,
                    action,
                    context,
                );
            }
        }
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &NotesAction,
        context: tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(encode(action));
                dispatcher.apply_now_with(doc, action, context)
            }
            None => self
                .local
                .borrow_mut()
                .host
                .try_dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&self) -> Option<Vec<ChangeHash>> {
        self.local.borrow_mut().take_committed()
    }

    /// Whether the step typing opened stays open for the next letters.
    pub(crate) fn keeps_step_open(&self) -> bool {
        self.local.borrow().keeps_step_open()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<EncodedAction> {
        if self.framework.is_some() {
            self.last.clone()
        } else {
            self.local.borrow().host.last_dispatched().cloned()
        }
    }
}

impl ResourceSurface<Notebook> for NotesApp {
    fn present(&mut self, state: &Rc<Workspace>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Notebook>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }
}
