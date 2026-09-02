//! Notes' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation Notes performs on its shared page tree is a
//! [`NotesAction`]: a small serializable value that deterministically mutates the
//! CRDT. Because the tree already lives in the shared document (`root["pages"]`,
//! keyed by page uuid — see [`crate::doc`]), an action replicates and replays for
//! free, and feeds straight back into the sidebar's page list. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the sidebar's "+" button,
//! an inline rename, a row's Delete) dispatch and what the Ctrl+K palette
//! dispatches — one source of truth, one replay path.
//!
//! The resource tree mirrors the data model: a notebook owns its pages, and a
//! page owns its child pages (the tree nests arbitrarily deep). Each resource
//! exports the [`ActionSpec`]s Ctrl+K offers for it (rename, delete, new child).
//!
//! Presentation — which page is selected, the scroll position, whether the
//! sidebar drawer is open, an expand/collapse toggle, the search query — stays
//! out of the document and never becomes an action.
//!
//! ## Determinism contract
//!
//! Page ids are minted at *authoring* time and carried in the payload;
//! [`Action::apply`] never touches the RNG or the wall-clock. A malformed payload
//! (an unparseable id) is a **no-op** rather than an error, so a bad replay can't
//! wedge a peer. Deletes and sibling renumbering are recomputed from the
//! document at the replay's base state, so two replicas at the same heads land on
//! the byte-identical result. Page *body* text is edited by splicing directly
//! into the per-block Automerge `Text` (the toolkit's CRDT-native text path); it
//! is not an action.

use std::collections::HashSet;

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    OperationKind, Resource, ResourceProvider, RetryPolicy,
};

use crate::doc::NotesDoc;
use crate::model::{PageId, PageMeta};
use crate::num;

/// Registry `type_id` for every Notes action.
pub const NOTES_ACTION_TYPE: &str = "notes.action";
pub const NOTES_OPERATION_TYPE: &str = "notes.operation";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotesOperation {
    OpenPage { page: PageId },
}

pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<NotesOperation>, String> {
    if type_id != NOTES_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// A serializable snapshot of a page's metadata, in exactly the representation
/// the document stores (ids as strings, an absent parent as `""`). Carrying the
/// document shape keeps [`Action::apply`] a pure write and makes
/// [`Self::from_meta`]/[`Self::to_meta`] lossless round-trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageData {
    /// Page uuid (string form), minted at authoring time.
    pub id: String,
    pub title: String,
    /// Parent page uuid (string form), or `""` for a root page.
    pub parent: String,
    pub order: u32,
}

impl PageData {
    pub fn from_meta(m: &PageMeta) -> Self {
        Self {
            id: m.id.to_string(),
            title: m.title.clone(),
            parent: m.parent.map(|p| p.to_string()).unwrap_or_default(),
            order: m.order,
        }
    }

    /// Reconstruct the model meta, or `None` when the id is malformed so `apply`
    /// can no-op instead of writing a garbage page.
    fn to_meta(&self) -> Option<PageMeta> {
        let id = self.id.parse::<PageId>().ok()?;
        let parent = if self.parent.is_empty() {
            None
        } else {
            self.parent.parse::<PageId>().ok()
        };
        Some(PageMeta {
            id,
            title: self.title.clone(),
            parent,
            order: self.order,
        })
    }
}

/// Every collaborative operation Notes exports on its shared page tree.
///
/// The set is deliberately small: a create, a rename, a re-parent/reorder, and
/// a single field edit are all one whole-page write ([`Self::UpsertPage`]); a
/// delete removes a page and its whole subtree. Page *body* text is not here —
/// it splices directly into the per-block CRDT `Text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotesAction {
    /// Create or replace a page's metadata (title / parent / order). One variant
    /// covers create, rename, re-parent, and reorder — all are a metadata write.
    /// The title is diff-spliced so a rename doesn't tombstone the whole title.
    UpsertPage(PageData),
    /// Delete a page and every descendant, then re-densify the `order` of each
    /// affected parent's surviving children so the tree stays contiguous. The
    /// descendant set and the renumbering are recomputed from the document at
    /// the replay's base state, so replicas converge.
    DeletePage { id: String },
}

/// Durable, browseable history metadata for a Notes mutation.
pub fn history_context(action: &NotesAction) -> qos_actions::ActionContext {
    let mut context = qos_actions::ActionContext::for_action("org.qos.Notes", "Notes", action);
    let (label, id, title) = match action {
        NotesAction::UpsertPage(page) => ("Update page", page.id.as_str(), page.title.as_str()),
        NotesAction::DeletePage { id } => ("Delete page", id.as_str(), "Page"),
    };
    context.action_label = label.to_string();
    context.resource = Some(qos_actions::ActionResource {
        id: format!("notes.page:{id}"),
        kind: "notes.page".to_string(),
        label: title.to_string(),
    });
    context
}

impl Action for NotesAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            NotesAction::UpsertPage(data) => {
                if let Some(meta) = data.to_meta() {
                    NotesDoc::upsert_page_meta(doc, &meta);
                }
            }
            NotesAction::DeletePage { id } => {
                let Ok(root) = id.parse::<PageId>() else {
                    return Ok(());
                };
                let all = NotesDoc::list_pages(doc);
                // Collect `root` and every descendant into the removal set,
                // walking the document's parent links (its base state).
                let mut removed: HashSet<PageId> = HashSet::new();
                let mut stack = vec![root];
                while let Some(cur) = stack.pop() {
                    if !removed.insert(cur) {
                        continue;
                    }
                    for p in all.iter().filter(|p| p.parent == Some(cur)) {
                        stack.push(p.id);
                    }
                }
                // Parents whose surviving children need re-densifying: the
                // parent of each removed page that is not itself removed.
                let mut affected: HashSet<Option<PageId>> = HashSet::new();
                for p in all.iter().filter(|p| removed.contains(&p.id)) {
                    if p.parent.is_none_or(|pp| !removed.contains(&pp)) {
                        affected.insert(p.parent);
                    }
                }
                // Delete in a deterministic order (by page id). A `HashSet`'s
                // iteration order is not stable across replicas, so emitting the
                // delete ops in set order would make the change diverge; sorting
                // pins one identical op sequence on every replica.
                let mut removed_sorted: Vec<PageId> = removed.iter().copied().collect();
                removed_sorted.sort();
                for id in &removed_sorted {
                    NotesDoc::remove_page(doc, *id);
                }
                // Re-densify each affected sibling group, visiting parents in a
                // stable order (root group first, then by parent id) for the same
                // reason. Within a group, siblings sort by their existing order
                // and are rewritten to 0,1,2,…; only the pages whose order
                // actually moves are re-written (a diff-spliced title + guarded
                // scalars make an unchanged upsert a no-op).
                let mut affected_sorted: Vec<Option<PageId>> = affected.into_iter().collect();
                affected_sorted.sort();
                for parent in affected_sorted {
                    let mut siblings: Vec<PageMeta> = all
                        .iter()
                        .filter(|p| p.parent == parent && !removed.contains(&p.id))
                        .cloned()
                        .collect();
                    siblings.sort_by_key(|p| p.order);
                    for (i, mut m) in siblings.into_iter().enumerate() {
                        let new_order = num::u32_of_usize(i);
                        if m.order != new_order {
                            m.order = new_order;
                            NotesDoc::upsert_page_meta(doc, &m);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Notes' action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<NotesAction>(NOTES_ACTION_TYPE);
}

/// Encode any [`NotesAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Notes action into a
/// replayable envelope.
///
/// # Panics
///
/// Panics if `action` fails to serialize. [`NotesAction`] is a plain,
/// non-recursive enum of owned strings and integers, so this cannot happen
/// for any value this module constructs.
#[allow(
    clippy::expect_used,
    reason = "NotesAction serializes with plain serde_json over owned strings/ints; encoding cannot fail"
)]
pub fn encode(action: &NotesAction) -> EncodedAction {
    EncodedAction::encode(NOTES_ACTION_TYPE, action).expect("NotesAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Create or replace a page (a new page, a rename, a re-parent, or a reorder).
pub fn upsert_page_action(meta: &PageMeta) -> NotesAction {
    NotesAction::UpsertPage(PageData::from_meta(meta))
}

/// Rename `meta`'s page, carrying the rest of its metadata unchanged so the
/// write only diff-splices the title.
pub fn rename_page_action(meta: &PageMeta, new_title: &str) -> NotesAction {
    let mut d = PageData::from_meta(meta);
    d.title = new_title.to_string();
    NotesAction::UpsertPage(d)
}

/// Create a child page under `parent` (or a root page when `parent` is `None`)
/// with a freshly-minted `id`, positioned at `order`.
pub fn new_page_action(id: PageId, parent: Option<PageId>, order: u32, title: &str) -> NotesAction {
    NotesAction::UpsertPage(PageData {
        id: id.to_string(),
        title: title.to_string(),
        parent: parent.map(|p| p.to_string()).unwrap_or_default(),
        order,
    })
}

/// Delete a page and its whole subtree.
pub fn delete_page_action(id: PageId) -> NotesAction {
    NotesAction::DeletePage { id: id.to_string() }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context Notes hands [`provide_resources`]: the page tree it
/// currently has open. Everything here is derived from state the document
/// already holds (the workspace mirrors the shared `pages` map); nothing is
/// fabricated.
#[derive(Debug, Clone)]
pub struct NotesResourceCtx {
    pub pages: Vec<PageMeta>,
}

/// A one-line label for a page in the palette.
fn page_label(m: &PageMeta) -> String {
    let t = m.title.trim();
    if t.is_empty() {
        "Untitled".to_string()
    } else {
        t.to_string()
    }
}

#[allow(
    clippy::expect_used,
    reason = "wraps encode(), which cannot fail for NotesAction — see its doc comment"
)]
fn immediate_spec(id: &str, label: &str, action: &NotesAction) -> ActionSpec {
    let spec = ActionSpec::immediate(id, label, NOTES_ACTION_TYPE, action)
        .expect("encode notes action")
        .reversible();
    if id.contains("delete") {
        spec.destructive()
    } else {
        spec
    }
}

/// The next `order` among `parent`'s children (append position).
fn next_order(pages: &[PageMeta], parent: Option<PageId>) -> u32 {
    pages
        .iter()
        .filter(|p| p.parent == parent)
        .map(|p| p.order)
        .max()
        .map_or(0, |o| o + 1)
}

/// Build one page resource and, recursively, its child pages.
fn page_resource(m: &PageMeta, pages: &[PageMeta]) -> Resource {
    let mut res = Resource::new(format!("notes.page:{}", m.id), "notes.page", page_label(m));

    res = res.with_action(
        ActionSpec::operation(
            "open",
            "Open page",
            OperationKind::View,
            NOTES_OPERATION_TYPE,
            &NotesOperation::OpenPage { page: m.id },
        )
        .expect("NotesOperation is always serializable")
        .retry_policy(RetryPolicy::Idempotent),
    );

    // Rename — a prompted action carrying the rest of the page's metadata
    // unchanged so only the title diff-splices.
    let meta = m.clone();
    res = res.with_action(ActionSpec::prompted(
        "rename",
        "Rename page",
        "Title",
        "page title…",
        ExecutionLocus::ClientLocal,
        move |title: &str| encode(&rename_page_action(&meta, title.trim())),
    ));

    // Delete the whole page (and its subtree).
    res = res.with_action(immediate_spec(
        "delete",
        "Delete page",
        &delete_page_action(m.id),
    ));

    // New child — prompted; the child id is minted once at commit time and the
    // append position is resolved from the current tree.
    let parent = m.id;
    let order = next_order(pages, Some(m.id));
    res = res.with_action(ActionSpec::prompted(
        "new-child",
        "New child page",
        "Title",
        "page title…",
        ExecutionLocus::ClientLocal,
        move |title: &str| {
            let t = title.trim();
            let title = if t.is_empty() { "Untitled" } else { t };
            encode(&new_page_action(Uuid::new_v4(), Some(parent), order, title))
        },
    ));

    // Child pages, in tree order.
    let mut children: Vec<&PageMeta> = pages.iter().filter(|p| p.parent == Some(m.id)).collect();
    children.sort_by_key(|p| p.order);
    for child in children {
        res = res.with_child(page_resource(child, pages));
    }
    res
}

/// Build the resource tree Notes currently has open: the notebook (always
/// present, exporting a "New page" action) and its pages, nested by the page
/// tree, each exporting rename / delete / new-child. Every document-mutating
/// operation is exported as the same action the on-screen affordance dispatches.
pub fn provide_resources(ctx: &NotesResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut notebook = Resource::new("notes.notebook", "notes.notebook", "Notes");

    // Notebook-level: create a new root page. The uuid is minted once when the
    // title argument is committed, then baked into the envelope, so replay stays
    // deterministic.
    let order = next_order(&ctx.pages, None);
    notebook = notebook.with_action(ActionSpec::prompted(
        "new-page",
        "New page",
        "Title",
        "page title…",
        ExecutionLocus::ClientLocal,
        move |title: &str| {
            let t = title.trim();
            let title = if t.is_empty() { "Untitled" } else { t };
            encode(&new_page_action(Uuid::new_v4(), None, order, title))
        },
    ));

    let mut roots: Vec<&PageMeta> = ctx.pages.iter().filter(|p| p.parent.is_none()).collect();
    roots.sort_by_key(|p| p.order);
    for root in roots {
        notebook = notebook.with_child(page_resource(root, &ctx.pages));
    }

    vec![notebook]
}

impl ResourceProvider for crate::app::NotesApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.notes_resource_ctx(), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Workspace;
    use qos_actions::{ActionInput, LocalTransport};
    use uuid::Uuid;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    /// A document seeded with a small page tree: a root "Projects" with two
    /// children, plus a second root "Inbox".
    fn seeded() -> (AutoCommit, PageId, PageId, PageId, PageId) {
        let mut doc = AutoCommit::new();
        let projects = Uuid::from_u128(0x1111);
        let a = Uuid::from_u128(0x2222);
        let b = Uuid::from_u128(0x3333);
        let inbox = Uuid::from_u128(0x4444);
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: projects,
                title: "Projects".into(),
                parent: None,
                order: 0,
            },
        );
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: a,
                title: "Launch".into(),
                parent: Some(projects),
                order: 0,
            },
        );
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: b,
                title: "Notes".into(),
                parent: Some(projects),
                order: 1,
            },
        );
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: inbox,
                title: "Inbox".into(),
                parent: None,
                order: 1,
            },
        );
        (doc, projects, a, b, inbox)
    }

    fn ctx_from(doc: &AutoCommit) -> NotesResourceCtx {
        NotesResourceCtx {
            pages: NotesDoc::list_pages(doc),
        }
    }

    /// Ctrl+K on Notes yields the nested tree: the notebook with a "New page"
    /// action, a `notes.page` child per root page exporting rename / delete /
    /// new-child, and grandchildren nested underneath. Drives the real
    /// resource-builder path.
    #[test]
    fn ctrl_k_publishes_the_nested_page_tree() {
        let (doc, _projects, _a, _b, _inbox) = seeded();
        let resources = provide_resources(&ctx_from(&doc), &doc);
        assert_eq!(resources.len(), 1, "one notebook root");
        let notebook = &resources[0];
        assert_eq!(notebook.kind, "notes.notebook");
        let nb_actions: Vec<&str> = notebook.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(nb_actions.contains(&"new-page"), "{nb_actions:?}");

        // Two root pages, in order.
        assert_eq!(notebook.children.len(), 2, "Projects + Inbox");
        let projects = notebook
            .children
            .iter()
            .find(|r| r.label == "Projects")
            .expect("a Projects page resource");
        assert_eq!(projects.kind, "notes.page");
        let p_actions: Vec<&str> = projects.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(p_actions.contains(&"rename"), "{p_actions:?}");
        assert!(p_actions.contains(&"delete"), "{p_actions:?}");
        assert!(p_actions.contains(&"new-child"), "{p_actions:?}");

        // Its two children nest underneath, in order.
        assert_eq!(
            projects.children.len(),
            2,
            "Launch + Notes nest under Projects"
        );
        assert_eq!(projects.children[0].label, "Launch");
        assert_eq!(projects.children[1].label, "Notes");

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("New page")), "{labels:?}");
        assert!(
            labels.iter().any(|l| l.contains("Rename page")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Delete page")),
            "{labels:?}"
        );
    }

    /// Deleting a page through its action removes the page *and* its whole
    /// subtree, and re-densifies the surviving root siblings.
    #[test]
    fn delete_page_removes_the_subtree_and_renumbers() {
        let (mut doc, projects, _a, _b, inbox) = seeded();
        assert_eq!(NotesDoc::list_pages(&doc).len(), 4);

        let action = delete_page_action(projects);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let left = NotesDoc::list_pages(&doc);
        assert_eq!(
            left.len(),
            1,
            "only Inbox survives (Projects + 2 children gone)"
        );
        assert_eq!(left[0].id, inbox);
        // Inbox was order 1; as the sole surviving root it re-densifies to 0.
        assert_eq!(left[0].order, 0, "surviving root sibling renumbers to 0");
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let (seeded_doc, _projects, _a, _b, _inbox) = seeded();
        let mut state = ui::State::new();
        state.doc = seeded_doc;
        let before_heads = state.doc.get_heads();
        let before = NotesDoc::list_pages(&state.doc).len();

        // Create a new root page via the action + host undo recording.
        let new_id = Uuid::from_u128(0x9999);
        let action = new_page_action(new_id, None, 2, "Fresh");
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            NotesDoc::list_pages(&state.doc).len(),
            before + 1,
            "the action created a page"
        );
        assert!(
            NotesDoc::list_pages(&state.doc)
                .iter()
                .any(|p| p.id == new_id),
            "the created page is present"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        // Ctrl+Z reverts exactly this action.
        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            NotesDoc::list_pages(&state.doc).len(),
            before,
            "undo reverted the created page and nothing else"
        );
        assert!(
            !NotesDoc::list_pages(&state.doc)
                .iter()
                .any(|p| p.id == new_id),
            "the created page is gone after undo"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let (seeded_doc, _projects, a, _b, _inbox) = seeded();
        let mut doc_a = seeded_doc;
        doc_a.commit();
        let bytes = doc_a.save();
        let mut doc_b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            doc_a.get_heads(),
            doc_b.get_heads(),
            "precondition: same base state"
        );

        // Rename a page: a whole-page upsert that diff-splices the title.
        let meta = NotesDoc::list_pages(&doc_a)
            .into_iter()
            .find(|p| p.id == a)
            .unwrap();
        let action = rename_page_action(&meta, "Renamed via action");
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut doc_a, &action, encoded).unwrap();
        qos_actions::replay(&mut doc_b, &env, &registry()).unwrap();

        assert_eq!(
            doc_a.get_heads(),
            doc_b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(
            NotesDoc::list_pages(&doc_b)
                .into_iter()
                .find(|p| p.id == a)
                .unwrap()
                .title,
            "Renamed via action",
            "the rename landed on the peer"
        );
    }

    /// Deleting a subtree also replays byte-identically — the recursive-delete +
    /// renumber path, not just the upsert.
    #[test]
    fn delete_replays_byte_identical() {
        let (seeded_doc, projects, _a, _b, _inbox) = seeded();
        let mut doc_a = seeded_doc;
        doc_a.commit();
        let bytes = doc_a.save();
        let mut doc_b = AutoCommit::load(&bytes).unwrap();

        let action = delete_page_action(projects);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut doc_a, &action, encoded).unwrap();
        qos_actions::replay(&mut doc_b, &env, &registry()).unwrap();

        assert_eq!(
            doc_a.get_heads(),
            doc_b.get_heads(),
            "delete replayed identically"
        );
        assert_eq!(NotesDoc::list_pages(&doc_b).len(), 1);
    }

    /// Single source of truth: the palette's rename / delete / new-child specs
    /// encode byte-identically to the builders the on-screen affordances call.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (doc, projects, _a, _b, _inbox) = seeded();
        let resources = provide_resources(&ctx_from(&doc), &doc);
        let page = resources[0]
            .children
            .iter()
            .find(|r| r.label == "Projects")
            .unwrap();

        // Delete spec == delete builder.
        let delete_spec = page.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        let from_builder = encode(&delete_page_action(projects));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for delete"
        );

        // Rename spec == rename builder for the same title.
        let meta = NotesDoc::list_pages(&doc)
            .into_iter()
            .find(|p| p.id == projects)
            .unwrap();
        let rename_spec = page.actions.iter().find(|a| a.id == "rename").unwrap();
        let from_spec = prompted_encode(rename_spec, "Renamed");
        let from_builder = encode(&rename_page_action(&meta, "Renamed"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );

        // New-child spec builds an UpsertPage under the right parent at the right
        // order (the minted id differs each call, so compare the stable fields).
        let new_child_spec = page.actions.iter().find(|a| a.id == "new-child").unwrap();
        let encoded = prompted_encode(new_child_spec, "Child");
        let decoded: NotesAction =
            serde_json::from_slice(&encoded.payload).expect("payload decodes to NotesAction");
        match decoded {
            NotesAction::UpsertPage(d) => {
                assert_eq!(
                    d.parent,
                    projects.to_string(),
                    "child parented under Projects"
                );
                assert_eq!(d.title, "Child");
                assert_eq!(d.order, 2, "appended after the two existing children");
            }
            NotesAction::DeletePage { id } => {
                panic!("new-child built the wrong action: DeletePage {{ id: {id:?} }}")
            }
        }
    }

    /// A move (re-parent + reorder) is expressed as `UpsertPage`s; replaying them
    /// converges. Mirrors what the app dispatches for a drag-reorder.
    #[test]
    fn reparent_via_upsert_replays_identically() {
        let (seeded_doc, projects, a, _b, inbox) = seeded();
        let mut doc_a = seeded_doc;
        doc_a.commit();
        let bytes = doc_a.save();
        let mut doc_b = AutoCommit::load(&bytes).unwrap();

        // Move child `a` out from under Projects to a root sibling after Inbox.
        let mut w = Workspace::new();
        w.data.pages = NotesDoc::list_pages(&doc_a);
        assert!(w.move_to(a, None, None));
        let moved = w.get(a).unwrap().clone();
        assert_eq!(moved.parent, None);

        let action = upsert_page_action(&moved);
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut doc_a, &action, encoded).unwrap();
        qos_actions::replay(&mut doc_b, &env, &registry()).unwrap();

        assert_eq!(
            doc_a.get_heads(),
            doc_b.get_heads(),
            "move replayed identically"
        );
        let pb = NotesDoc::list_pages(&doc_b);
        assert_eq!(pb.iter().find(|p| p.id == a).unwrap().parent, None);
        // Untouched pages are still there.
        assert!(pb.iter().any(|p| p.id == projects));
        assert!(pb.iter().any(|p| p.id == inbox));
    }

    /// The retained accessibility tree surfaces the notebook's pages as stable
    /// nodes, and a page node's exposed `delete` affordance is the byte-identical
    /// `EncodedAction` the on-screen page control dispatches.
    #[test]
    fn a11y_tree_surfaces_page_delete_as_the_affordance_action() {
        let (doc, projects, _a, _b, _inbox) = seeded();
        let resources = provide_resources(&ctx_from(&doc), &doc);

        // An inert root; the operable nodes come from the published resources.
        // `HashMap::new()`'s type is inferred from `refresh`'s signature.
        let root = ui::widget::Column::new();
        let node_map = std::collections::HashMap::new();
        let mut tree = ui::SemanticTree::new();

        // First refresh populates the tree; a second, unchanged refresh is a
        // no-op with identical node identity (retained, not rebuilt each frame).
        let d1 = tree.refresh(&root, &node_map, None, &resources);
        assert!(!d1.added.is_empty(), "the first refresh populates the tree");
        let notebook = tree
            .node(&ui::SemanticId("r:notes.notebook".into()))
            .expect("notebook node present");
        assert_eq!(notebook.name.as_deref(), Some("Notes"));
        assert!(
            notebook.action("new-page").is_some(),
            "the notebook affords new-page"
        );
        let ids1: Vec<ui::SemanticId> = tree.iter().map(|n| n.id.clone()).collect();

        let d2 = tree.refresh(&root, &node_map, None, &resources);
        assert!(d2.is_empty(), "an unchanged frame yields no delta: {d2:?}");
        let ids2: Vec<ui::SemanticId> = tree.iter().map(|n| n.id.clone()).collect();
        assert_eq!(ids1, ids2, "node identity survives across frames");

        // The Projects page node exposes rename (prompted) + delete (immediate);
        // the delete affordance carries the on-screen control's exact action.
        let page = tree
            .node(&ui::SemanticId(format!("r:notes.page:{projects}")))
            .expect("projects page node");
        assert!(page.action("rename").is_some(), "page affords rename");
        let exposed = match &page.action("delete").expect("delete affordance").kind {
            ui::SemanticActionKind::App(enc) => enc.clone(),
            other => panic!("expected an app action, got {other:?}"),
        };
        assert_eq!(
            exposed,
            encode(&delete_page_action(projects)),
            "the a11y affordance and the on-screen builder are one action"
        );

        // Invoking the exposed action mutates the doc exactly as the on-screen
        // control does: two identical docs, deleted through each path, compared.
        let dispatch_encoded = |doc: &mut AutoCommit, enc: EncodedAction| {
            let decoded = registry().decode(&enc).unwrap();
            let mut tx = LocalTransport::new();
            qos_actions::dispatch(doc, decoded.as_ref(), enc, &mut tx).unwrap();
        };
        let (mut via_a11y, ..) = seeded();
        let (mut via_affordance, ..) = seeded();
        dispatch_encoded(&mut via_a11y, exposed);
        dispatch_encoded(&mut via_affordance, encode(&delete_page_action(projects)));

        let a: Vec<_> = NotesDoc::list_pages(&via_a11y)
            .iter()
            .map(|p| p.id)
            .collect();
        let b: Vec<_> = NotesDoc::list_pages(&via_affordance)
            .iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(
            a, b,
            "invoking the a11y affordance deletes the same page + subtree"
        );
        assert_eq!(
            NotesDoc::list_pages(&via_a11y).len(),
            1,
            "only Inbox survives"
        );
    }

    #[test]
    fn page_action_history_is_addressable() {
        let context = history_context(&NotesAction::UpsertPage(PageData {
            id: "page-7".into(),
            title: "Launch brief".into(),
            parent: String::new(),
            order: 0,
        }));
        assert_eq!(context.action_label, "Update page");
        let resource = context.resource.expect("page resource");
        assert_eq!(resource.id, "notes.page:page-7");
        assert_eq!(resource.label, "Launch brief");
    }
}
