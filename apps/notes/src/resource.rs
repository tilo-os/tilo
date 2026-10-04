//! Notes as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. Nested page bodies are rich-text child resources; the surface presents
//! the framework's workspace state and submits edits through the dispatcher.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType};
use tilo_document::{RichText, RichTextAction};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::permissions::UserId;
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
// `Resource` is in scope for both `Notebook` and nested `RichText`.
use crate::actions::{
    NOTES_ACTION_TYPE, NOTES_OPERATION_TYPE, NotesAction, NotesOperation, encode, new_page_action,
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
        Ok(workspace)
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        crate::notebook::initialize_empty(doc).map_err(ModelError::invalid)?;
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "new-page",
                "New note",
                vec![ActionParam::required(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "Title",
                )],
                |args, _| {
                    let title = args
                        .text(tilo_actions::PROMPTED_PARAM)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    Ok(new_page_action(PageId::new_v4(), None, 0, &title))
                },
            )
            .describe("Create a new root note."),
        ]
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
        .collect()
    }

    fn children(state: &Workspace) -> Vec<tilo_actions::Resource> {
        // Page tree with each note's rich-text body nested underneath, plus
        // the tag catalogue.
        let mut children = crate::actions::notebook_page_children(state.pages(), None);
        let ctx = crate::actions::NotesResourceCtx {
            pages: state.pages().to_vec(),
            blocks: std::collections::HashMap::new(),
            tags: state.tags().to_vec(),
            tag_assignments: state
                .data
                .tag_assignments
                .iter()
                .map(|assignment| (assignment.page, assignment.tag))
                .collect(),
            can_go_back: false,
            can_go_forward: false,
        };
        children.push(crate::actions::tags_resource(&ctx));
        children
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
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1100, 720)
    }

    fn view(cx: &mut ViewCx<Notebook>) -> Element {
        // Empty shell; `present` installs the framework workspace. Do not open
        // a fresh AutoCommit here — that would ignore the hosted document.
        let author = UserId("local".into());
        let app = NotesApp::empty_surface(&author);
        cx.surface(app)
    }
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
}
