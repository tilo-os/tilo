//! Notes as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. Nested page bodies are rich-text child resources; the surface presents
//! the framework's workspace state and submits edits through the dispatcher.

use std::rc::Rc;

use automerge::{ObjType, ROOT, ReadDoc, Value, transaction::Transactable};

use tilo_actions::ActionSpec;
use tilo_document::RichText;
use ui::application::OpenFile;
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, LaunchInput, ResourceApp, ResourceSurface, SurfaceCx,
    ViewCx,
};
use ui::headless::FrameHarness;
use ui::permissions::UserId;
use ui::tilo_resource::{ActionDecl, Delta, Doc, Invoke, ModelError, Resource, SearchEntry};
// `Resource` is in scope for both `Notebook` and nested `RichText`.
use crate::actions::{NOTES_ACTION_TYPE, NotesAction};
use crate::app::NotesApp;
use crate::doc::NotesDoc;
use crate::effect::NotesEffect;
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
        let mut workspace = crate::notebook::project_without_asset_rehash(doc, None)
            .map_err(ModelError::invalid)?;
        workspace.canonical_projection_heads = committed_projection_heads(doc);
        workspace.bodies = {
            let _profile = ui::tilo_resource::profiling::Phase::start("projection_bodies");
            std::sync::Arc::new(crate::actions::read_bodies(doc, workspace.pages()))
        };
        workspace.body_states = {
            let _profile = ui::tilo_resource::profiling::Phase::start("projection_rich_states");
            Rc::new(crate::actions::read_body_states(doc, workspace.pages()))
        };
        Ok(workspace)
    }

    fn refresh(state: &mut Workspace, doc: &Doc, delta: &Delta) -> Result<(), ModelError> {
        if delta.is_empty() {
            return Ok(());
        }
        let Some(updates) = changed_page_bodies(state, doc, delta)? else {
            *state = Self::read(doc)?;
            return Ok(());
        };
        let refreshed_pages = updates.iter().map(|update| update.page).collect();
        let before = state.canonical_projection_heads.clone();
        {
            let _profile = ui::tilo_resource::profiling::Phase::start("projection_bodies");
            let bodies = std::sync::Arc::make_mut(&mut state.bodies);
            for update in &updates {
                bodies.insert(update.page, update.blocks.clone());
            }
        }
        {
            let _profile = ui::tilo_resource::profiling::Phase::start("projection_rich_states");
            let bodies = Rc::make_mut(&mut state.body_states);
            for update in updates {
                bodies.insert(update.page, update.rich);
            }
        }
        let after = committed_projection_heads(doc);
        state.body_projection_refresh =
            before
                .zip(after.clone())
                .map(|(before, after)| crate::model::BodyProjectionRefresh {
                    before,
                    after,
                    pages: refreshed_pages,
                });
        state.canonical_projection_heads = after;
        Ok(())
    }

    fn publication_cache_epoch(_state: &Workspace) -> Option<String> {
        // Daily-note actions must change at local midnight even when nobody
        // edited the notebook. All other canonical publication inputs are state.
        Some(crate::actions::today_title())
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

    fn children(state: &Workspace) -> Vec<tilo_actions::Resource> {
        crate::actions::notebook_children(state)
    }

    fn child(state: &Workspace, id: &str) -> Option<tilo_actions::Resource> {
        crate::actions::notebook_child(state, id)
    }

    const READABLE: bool = true;

    /// The notebook as its pages and tags; one page (`notes.page:<id>`) as
    /// its title and blocks, each with the id the body's edit actions take.
    fn read_content(
        _state: &Workspace,
        doc: &Doc,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let pages = NotesDoc::list_pages(doc);
        let Some(item) = item else {
            return Ok(serde_json::json!({
                "kind": Self::KIND,
                "pages": pages.iter().map(|page| serde_json::json!({
                    "item": crate::actions::page_resource_id(page.id),
                    "title": page.title,
                    "parent": page.parent.map(crate::actions::page_resource_id),
                })).collect::<Vec<_>>(),
                "tags": NotesDoc::list_tags(doc).iter().map(|tag| serde_json::json!({
                    "id": tag.id.to_string(),
                    "name": tag.name,
                })).collect::<Vec<_>>(),
            }));
        };
        let page = item
            .strip_prefix("notes.page:")
            .and_then(|id| id.parse::<PageId>().ok())
            .and_then(|id| pages.iter().find(|page| page.id == id))
            .ok_or_else(|| format!("no note `{item}`; read the notebook for the pages it holds"))?;
        let blocks = NotesDoc::page_blocks(doc, page.id);
        Ok(serde_json::json!({
            "kind": "notes.page",
            "item": crate::actions::page_resource_id(page.id),
            "title": page.title,
            "parent": page.parent.map(crate::actions::page_resource_id),
            "text": blocks.iter().map(|block| block.text.as_str()).collect::<Vec<_>>().join("\n"),
            "blocks": blocks.iter().map(|block| serde_json::json!({
                "id": block.id,
                "kind": block_kind(&block.kind),
                "text": block.text,
            })).collect::<Vec<_>>(),
        }))
    }

    fn search(state: &Workspace) -> Vec<SearchEntry> {
        crate::search::entries(state)
    }
}

fn committed_projection_heads(doc: &Doc) -> Option<Vec<automerge::ChangeHash>> {
    (doc.pending_ops() == 0).then(|| doc.base_heads())
}

struct PageBodyRefresh {
    page: PageId,
    blocks: Vec<crate::actions::StoredBlock>,
    rich: tilo_document::RichTextState,
}

/// Body edits preserve the already validated page tree. Structural changes,
/// unknown deltas and edits outside those bodies take the full schema reader.
fn changed_page_bodies(
    state: &Workspace,
    doc: &Doc,
    delta: &Delta,
) -> Result<Option<Vec<PageBodyRefresh>>, ModelError> {
    let root = delta.under(&ROOT);
    if root.is_all()
        || root.key("pages") != ui::tilo_resource::delta::Touch::Below
        || root.keys().any(|key| key != "pages")
    {
        return Ok(None);
    }
    let Ok(Some((Value::Object(ObjType::Map), pages))) = doc.get(ROOT, "pages") else {
        return Ok(None);
    };
    let changed = delta.under(&pages);
    if changed.is_all() || changed.is_empty() {
        return Ok(None);
    }
    let mut updates = Vec::new();
    for key in changed.keys() {
        if changed.key(key) == ui::tilo_resource::delta::Touch::Written {
            return Ok(None);
        }
        let Ok(page) = key.parse::<PageId>() else {
            return Ok(None);
        };
        if state.get(page).is_none() {
            return Ok(None);
        }
        let Ok(Some((Value::Object(ObjType::Map), object))) = doc.get(&pages, key) else {
            return Ok(None);
        };
        let fields = delta.under(&object);
        if fields.is_all()
            || fields.key("body") == ui::tilo_resource::delta::Touch::Untouched
            || fields.keys().any(|field| field != "body")
        {
            return Ok(None);
        }
        let body = crate::notebook::validated_page_body(doc, page).map_err(ModelError::invalid)?;
        let rich = RichText::read_under(doc, &body)?;
        updates.push(PageBodyRefresh {
            page,
            blocks: NotesDoc::page_stored_blocks(doc, page),
            rich,
        });
    }
    Ok(Some(updates))
}

/// A block's kind as a reader names it.
fn block_kind(kind: &ui::widget::BlockKind) -> serde_json::Value {
    use ui::widget::BlockKind;
    match kind {
        BlockKind::Paragraph => "paragraph".into(),
        BlockKind::Quote => "quote".into(),
        BlockKind::Heading(level) => serde_json::json!({ "heading": level }),
        BlockKind::Bullet { indent } => serde_json::json!({ "bullet": indent }),
        BlockKind::Numbered { indent } => serde_json::json!({ "numbered": indent }),
        BlockKind::Checklist { indent, done } => {
            serde_json::json!({ "checklist": indent, "done": done })
        }
        BlockKind::CodeBlock { lang } => serde_json::json!({ "code": lang }),
        BlockKind::Table { .. } => "table".into(),
        BlockKind::Divider => "divider".into(),
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
        let (markdown, assets) = input.argument().map_or_else(
            || (markdown.to_owned(), Vec::new()),
            |path| {
                crate::app::adopt_canonical_markdown_assets(std::path::Path::new(path), markdown)
            },
        );
        Invoke::apply(&crate::actions::import_markdown_action(
            page, title, &markdown, assets,
        ))
        .into_iter()
        .collect()
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::metadata::CATALOG)
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

impl ResourceSurface<Notebook, NotesEffect> for NotesApp {
    fn take_save_retry(&mut self) -> bool {
        self.take_save_retry_request()
    }

    fn effect(&mut self, effect: NotesEffect, cx: &mut EffectCx<Notebook, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    /// Back and forward are offered only where they lead.
    fn effects(&self) -> Vec<EffectStatus> {
        self.navigation_statuses()
    }

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

#[cfg(test)]
#[path = "resource/refresh_tests.rs"]
mod refresh_tests;
