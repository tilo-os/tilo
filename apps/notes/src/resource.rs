//! Notes as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. Nested page bodies are rich-text child resources; the surface presents
//! the framework's workspace state and submits edits through the dispatcher.

use std::rc::Rc;

use tilo_actions::{ActionSpec, OperationKind};
use tilo_document::RichText;
use ui::application::OpenFile;
use ui::framework::{LaunchInput, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::headless::FrameHarness;
use ui::permissions::UserId;
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
// `Resource` is in scope for both `Notebook` and nested `RichText`.
use crate::actions::{NOTES_ACTION_TYPE, NOTES_OPERATION_TYPE, NotesAction, NotesOperation};
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
        workspace.body_states = Rc::new(crate::actions::read_body_states(doc, workspace.pages()));
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
