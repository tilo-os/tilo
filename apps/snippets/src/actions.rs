//! Snippets' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every mutation of the shared snippet library is a [`SnippetsAction`]: a small
//! serializable value that deterministically mutates the CRDT. Because the
//! library already lives in the shared document (`root["snippets"]`, keyed by
//! uuid — see [`crate::doc`]), an action replicates and replays for free, and
//! feeds straight back into the list + detail panes. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the `+ New` button, the
//! Star / Duplicate / Delete toolbar, the language pill, Copy) dispatch and what
//! the Ctrl+K palette dispatches — one source of truth, one replay path.
//!
//! The resource tree mirrors the model: the library owns its snippets, and each
//! snippet exports the operations Ctrl+K offers for it (rename, set language,
//! move, tag, describe, star, delete).
//!
//! Presentation — which snippet is selected, the search text, the scroll offset,
//! the open expand-modal, copying text to the clipboard — stays out of the
//! document and never becomes an action.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use uuid::Uuid;

use tilo_actions::{
    Action, ActionAddress, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, OperationKind, Resource, ResourceProvider, RetryPolicy,
};

use crate::doc::SnippetsDoc;
use crate::model::{Language, Snippet, SnippetId};

/// Registry `type_id` for every Snippets action.
pub const SNIPPETS_ACTION_TYPE: &str = "snippets.action";
pub const SNIPPETS_OPERATION_TYPE: &str = "snippets.operation";

/// Stable resource id for one snippet. Search and every other caller use this
/// exact identity instead of maintaining a second navigation namespace.
pub fn snippet_resource_id(id: SnippetId) -> String {
    format!("snippets.snippet:{id}")
}

/// Canonical view action that opens one snippet.
pub fn open_snippet_address(id: SnippetId) -> ActionAddress {
    ActionAddress::new(snippet_resource_id(id), "open")
}

/// Stable resource id for a tag. Hex encoding preserves arbitrary Unicode tag
/// names without letting punctuation become resource-address syntax.
pub fn tag_resource_id(tag: &str) -> String {
    format!("snippets.tag:{}", stable_key(tag))
}

/// Stable, punctuation-safe component for custom-painted semantic controls.
pub fn stable_key(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .fold(String::new(), |mut encoded, byte| {
            // Writing formatted integers to a String is infallible.
            let _ = write!(encoded, "{byte:02x}");
            encoded
        })
}

/// Canonical view action that reveals all snippets carrying `tag`.
pub fn reveal_tag_address(tag: &str) -> ActionAddress {
    ActionAddress::new(tag_resource_id(tag), "reveal")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnippetsOperation {
    OpenSnippet { id: SnippetId },
    RevealTag { tag: String },
    FocusSearch,
}

/// Decode a Snippets view operation when the type matches.
///
/// # Errors
/// Returns an error for malformed operation payloads.
pub fn decode_operation(
    type_id: &str,
    payload: &[u8],
) -> Result<Option<SnippetsOperation>, String> {
    if type_id != SNIPPETS_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[allow(
    clippy::expect_used,
    reason = "SnippetsOperation contains only serializable strings and UUIDs"
)]
fn view_operation(id: &str, label: &str, operation: &SnippetsOperation) -> ActionSpec {
    ActionSpec::operation(
        id,
        label,
        OperationKind::View,
        SNIPPETS_OPERATION_TYPE,
        operation,
    )
    .expect("SnippetsOperation is always serializable")
    .retry_policy(RetryPolicy::Idempotent)
}

/// Every collaborative operation Snippets exports on its library.
///
/// Determinism contract: ids are minted at *authoring* time and carried in the
/// payload, never inside [`Action::apply`], which must not touch the RNG or the
/// wall-clock. A malformed payload (an id that doesn't parse, a row that no
/// longer exists) is a **no-op** rather than an error, so a bad replay can't
/// wedge a peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SnippetsAction {
    /// Insert a whole snippet — the create path shared by `+ New` and Duplicate.
    /// The uuid is minted at authoring time and carried inside `snippet`, so
    /// replay reproduces the byte-identical row.
    CreateSnippet { snippet: Snippet },
    /// Remove a snippet by id.
    DeleteSnippet { id: String },
    /// Replace a snippet's title.
    Rename { id: String, title: String },
    /// Replace body text, preserving indentation and trailing newlines.
    SetBody { id: String, body: String },
    /// Set the body language, carried as the stable slug (`"rust"`, `"shell"`,
    /// …) so the wire form is independent of the enum's memory layout.
    SetLanguage { id: String, language: String },
    /// Star / unstar. Carries the target value (not a toggle) so replay is
    /// deterministic regardless of the replica's current state.
    SetFavorite { id: String, favorite: bool },
    /// Replace the free-text description shown above the body.
    SetDescription { id: String, description: String },
    /// Move the snippet to a folder (`""` == the implicit "All Snippets" root).
    SetFolder { id: String, folder: String },
    /// Replace the whole tag set.
    SetTags { id: String, tags: Vec<String> },
    /// Record one use — bumps the counter that drives the "Most Used" smart
    /// folder and the per-row usage hint. Deterministic against the replay's
    /// base state: it reads the current count and adds one.
    RecordUse { id: String },
}

impl SnippetsAction {
    fn parse_id(id: &str) -> Option<SnippetId> {
        id.parse::<SnippetId>().ok()
    }

    /// Read-modify-write one field on the authoritative document, reusing the
    /// churn-safe [`SnippetsDoc::upsert`] (diff-spliced Text + change-guarded
    /// scalars). A missing row is a no-op. The read is of the *base* document,
    /// so two replicas at the same heads produce the identical ops.
    fn edit<F: FnOnce(&mut Snippet)>(doc: &mut Doc, id: &str, f: F) {
        if let Some(mut s) = Self::parse_id(id).and_then(|i| SnippetsDoc::get(doc, i)) {
            f(&mut s);
            SnippetsDoc::upsert(doc, &s);
        }
    }
}

impl Action for SnippetsAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            SnippetsAction::CreateSnippet { snippet } => {
                SnippetsDoc::upsert(doc, snippet);
            }
            SnippetsAction::DeleteSnippet { id } => {
                if let Some(uuid) = Self::parse_id(id) {
                    SnippetsDoc::remove(doc, uuid);
                }
            }
            SnippetsAction::Rename { id, title } => {
                Self::edit(doc, id, |s| s.title.clone_from(title));
            }
            SnippetsAction::SetBody { id, body } => {
                Self::edit(doc, id, |s| s.body.clone_from(body));
            }
            SnippetsAction::SetLanguage { id, language } => {
                let lang = Language::from_slug(language);
                Self::edit(doc, id, |s| s.language = lang);
            }
            SnippetsAction::SetFavorite { id, favorite } => {
                Self::edit(doc, id, |s| s.favorite = *favorite);
            }
            SnippetsAction::SetDescription { id, description } => {
                Self::edit(doc, id, |s| s.description.clone_from(description));
            }
            SnippetsAction::SetFolder { id, folder } => {
                Self::edit(doc, id, |s| s.folder.clone_from(folder));
            }
            SnippetsAction::SetTags { id, tags } => {
                Self::edit(doc, id, |s| s.tags.clone_from(tags));
            }
            SnippetsAction::RecordUse { id } => {
                Self::edit(doc, id, |s| s.uses = s.uses.saturating_add(1));
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Snippets' action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<SnippetsAction>(SNIPPETS_ACTION_TYPE);
}

/// Encode any [`SnippetsAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a Snippets action into
/// a replayable envelope.
///
/// # Panics
/// Never in practice: every field of [`SnippetsAction`] is a plain `String`,
/// `bool` or `Vec<String>` (or a [`Snippet`], itself built from the same),
/// all of which always serialize.
#[allow(
    clippy::expect_used,
    reason = "SnippetsAction's fields are plain strings/bools/vecs, which always serialize — a failure here would mean serde itself is broken"
)]
pub fn encode(action: &SnippetsAction) -> EncodedAction {
    EncodedAction::encode(SNIPPETS_ACTION_TYPE, action)
        .expect("SnippetsAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

pub fn create_snippet_action(snippet: Snippet) -> SnippetsAction {
    SnippetsAction::CreateSnippet { snippet }
}

pub fn delete_snippet_action(id: SnippetId) -> SnippetsAction {
    SnippetsAction::DeleteSnippet { id: id.to_string() }
}

pub fn set_body_action(id: SnippetId, body: &str) -> SnippetsAction {
    SnippetsAction::SetBody {
        id: id.to_string(),
        body: body.to_owned(),
    }
}

pub fn rename_action(id: SnippetId, title: &str) -> SnippetsAction {
    SnippetsAction::Rename {
        id: id.to_string(),
        title: title.to_string(),
    }
}

pub fn set_language_action(id: SnippetId, language: Language) -> SnippetsAction {
    SnippetsAction::SetLanguage {
        id: id.to_string(),
        language: language.slug().to_string(),
    }
}

pub fn set_favorite_action(id: SnippetId, favorite: bool) -> SnippetsAction {
    SnippetsAction::SetFavorite {
        id: id.to_string(),
        favorite,
    }
}

pub fn set_description_action(id: SnippetId, description: &str) -> SnippetsAction {
    SnippetsAction::SetDescription {
        id: id.to_string(),
        description: description.to_string(),
    }
}

pub fn set_folder_action(id: SnippetId, folder: &str) -> SnippetsAction {
    SnippetsAction::SetFolder {
        id: id.to_string(),
        folder: folder.to_string(),
    }
}

pub fn set_tags_action(id: SnippetId, tags: &[String]) -> SnippetsAction {
    SnippetsAction::SetTags {
        id: id.to_string(),
        tags: tags.to_vec(),
    }
}

pub fn record_use_action(id: SnippetId) -> SnippetsAction {
    SnippetsAction::RecordUse { id: id.to_string() }
}

/// Parse the free-text argument of the "Set tags" palette action into a clean,
/// de-duplicated set: comma-separated, trimmed, empties dropped, order kept.
pub fn parse_tags(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in raw.split(',') {
        let t = t.trim();
        if !t.is_empty() && !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context the app hands [`provide_resources`] so the
/// library-level "New snippet" action can drop the snippet into whatever folder
/// the sidebar currently has selected — state that lives in the app, not the
/// document.
#[derive(Debug, Clone, Default)]
pub struct SnippetsResourceCtx {
    /// Folder a new snippet is filed under (`""` == the "All Snippets" root).
    pub current_folder: String,
}

#[allow(
    clippy::expect_used,
    reason = "SnippetsAction's fields are plain strings/bools/vecs, which always serialize — a failure here would mean serde itself is broken"
)]
fn encode_spec(id: &str, label: &str, action: &SnippetsAction) -> ActionSpec {
    let spec = ActionSpec::immediate(id, label, SNIPPETS_ACTION_TYPE, action)
        .expect("encode snippets action")
        .reversible();
    if id.contains("delete") {
        spec.destructive()
    } else {
        spec
    }
}

/// One-line label for a snippet resource in the palette.
fn snippet_label(s: &Snippet) -> String {
    let t = s.title.trim();
    let base = if t.is_empty() { "Untitled snippet" } else { t };
    format!("{} · {}", base, s.language.label())
}

/// Build the small contextual root shown immediately by Ctrl+K.
pub fn provide_contextual_resources(ctx: &SnippetsResourceCtx) -> Vec<Resource> {
    let mut lib = Resource::new("snippets.library", "snippets.library", "Snippets");
    lib = lib.with_action(view_operation(
        "search",
        "Search snippets",
        &SnippetsOperation::FocusSearch,
    ));

    // Library-level: create a new snippet at the current folder. The uuid is
    // minted once when the title argument is committed, then baked into the
    // envelope, so replay stays deterministic.
    let folder = ctx.current_folder.clone();
    lib = lib.with_action(ActionSpec::prompted(
        "new-snippet",
        "New snippet",
        "Title",
        "snippet title…",
        ExecutionLocus::ClientLocal,
        move |title: &str| {
            let mut s = Snippet::blank(&folder);
            s.id = Uuid::new_v4();
            let t = title.trim();
            if !t.is_empty() {
                s.title = t.to_string();
            }
            encode(&create_snippet_action(s))
        },
    ));

    vec![lib]
}

pub fn provide_tag_resource(tag: String) -> Resource {
    Resource::new(tag_resource_id(&tag), "snippets.tag", &tag).with_action(view_operation(
        "reveal",
        "Reveal tagged snippets",
        &SnippetsOperation::RevealTag { tag },
    ))
}

/// Materialise one snippet only after a canonical search result names it.
pub fn provide_snippet_resource(s: &Snippet) -> Resource {
    let id = s.id;
    let mut r = Resource::new(
        snippet_resource_id(id),
        "snippets.snippet",
        snippet_label(s),
    );
    r = r.with_action(view_operation(
        "open",
        "Open snippet",
        &SnippetsOperation::OpenSnippet { id },
    ));

    r = r.with_action(ActionSpec::prompted(
        "edit-body",
        ui::i18n!("snippets-edit-body"),
        ui::i18n!("snippets-body-placeholder"),
        ui::i18n!("snippets-body-placeholder"),
        ExecutionLocus::ClientLocal,
        move |text: &str| encode(&set_body_action(id, text)),
    ));
    r = r.with_action(ActionSpec::prompted(
        "rename",
        "Rename",
        "Title",
        "new title…",
        ExecutionLocus::ClientLocal,
        move |t: &str| encode(&rename_action(id, t.trim())),
    ));
    r = r.with_action(ActionSpec::prompted(
        "set-language",
        "Set language",
        "Language",
        "rust, shell, python, sql…",
        ExecutionLocus::ClientLocal,
        move |t: &str| {
            encode(&set_language_action(
                id,
                Language::from_slug(&t.trim().to_lowercase()),
            ))
        },
    ));
    r = r.with_action(ActionSpec::prompted(
        "move-to-folder",
        "Move to folder",
        "Folder",
        "folder name (blank = All)",
        ExecutionLocus::ClientLocal,
        move |t: &str| encode(&set_folder_action(id, t.trim())),
    ));
    r = r.with_action(ActionSpec::prompted(
        "set-tags",
        "Set tags",
        "Tags",
        "comma, separated, tags",
        ExecutionLocus::ClientLocal,
        move |t: &str| encode(&set_tags_action(id, &parse_tags(t))),
    ));
    r = r.with_action(ActionSpec::prompted(
        "edit-description",
        "Edit description",
        "Description",
        "describe this snippet…",
        ExecutionLocus::ClientLocal,
        move |t: &str| encode(&set_description_action(id, t)),
    ));
    r = r.with_action(encode_spec(
        "toggle-favorite",
        if s.favorite { "Unstar" } else { "Star" },
        &set_favorite_action(id, !s.favorite),
    ));
    r.with_action(encode_spec(
        "delete",
        "Delete snippet",
        &delete_snippet_action(id),
    ))
}

/// Build the complete resource tree for audits and non-interactive callers.
pub fn provide_resources(ctx: &SnippetsResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let mut lib = provide_contextual_resources(ctx)
        .pop()
        .unwrap_or_else(|| Resource::new("snippets.library", "snippets.library", "Snippets"));
    let snippets = SnippetsDoc::list(doc);
    let tags = snippets
        .iter()
        .flat_map(|snippet| snippet.tags.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>();
    for tag in tags {
        lib = lib.with_child(provide_tag_resource(tag));
    }

    for s in snippets {
        lib = lib.with_child(provide_snippet_resource(&s));
    }

    vec![lib]
}

impl ResourceProvider for crate::app::SnippetsApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.snippets_resource_ctx(), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SnippetsApp;
    use tilo_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// Build an [`EncodedAction`] from a prompted spec's closure (the same path
    /// the palette takes on Enter).
    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } | ActionInput::Deferred { .. } => {
                panic!("expected an immediate action: {}", spec.id)
            }
        }
    }

    /// A demo app with its library mirrored into a shared doc — the state the
    /// live app is always in by the time an event arrives (sync runs first).
    fn seeded() -> (SnippetsApp, AutoCommit) {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    fn first_snippet(doc: &AutoCommit) -> Snippet {
        SnippetsDoc::list(doc)
            .into_iter()
            .next()
            .expect("a seeded snippet")
    }

    /// Ctrl+K publishes a bounded contextual root; a stable id lazily resolves
    /// one snippet with every action the model implies.
    #[test]
    fn ctrl_k_publishes_the_library_and_snippet_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one library root");
        let lib = &resources[0];
        assert_eq!(lib.kind, "snippets.library");
        let lib_actions: Vec<&str> = lib.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(lib_actions.contains(&"new-snippet"), "{lib_actions:?}");
        assert!(lib.children.is_empty(), "the contextual root stays bounded");
        let snippet = first_snippet(&doc);
        let resource_id = snippet_resource_id(snippet.id);
        let snip = Widget::resource(&app, &doc, &resource_id).expect("lazy snippet resource");
        assert_eq!(snip.kind, "snippets.snippet");
        let ids: Vec<&str> = snip.actions.iter().map(|a| a.id.as_str()).collect();
        for want in [
            "rename",
            "set-language",
            "move-to-folder",
            "set-tags",
            "edit-description",
            "toggle-favorite",
            "delete",
        ] {
            assert!(
                ids.contains(&want),
                "snippet resource missing {want}: {ids:?}"
            );
        }

        let tag_name = snippet
            .tags
            .first()
            .expect("seeded snippet has a tag")
            .clone();
        let tag = Widget::resource(&app, &doc, &tag_resource_id(&tag_name))
            .expect("seeded snippets lazily publish tag resources");
        let reveal = tag
            .actions
            .iter()
            .find(|action| action.id == "reveal")
            .expect("tag exports its reveal action");
        assert_eq!(reveal.kind, OperationKind::View);
        let decoded_tag_name =
            match decode_operation(SNIPPETS_OPERATION_TYPE, &immediate_encode(reveal).payload) {
                Ok(Some(SnippetsOperation::RevealTag { tag })) => tag,
                other => panic!("unexpected tag operation: {other:?}"),
            };
        assert_eq!(decoded_tag_name, tag_name);
        assert_eq!(
            reveal_tag_address(&tag_name),
            ActionAddress::new(&tag.id.0, &reveal.id)
        );

        let open = snip
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("snippet exports open");
        assert_eq!(open.kind, OperationKind::View);
        assert!(
            snip.actions
                .iter()
                .filter(|action| action.id != "open")
                .all(|action| action.kind == OperationKind::Document),
            "snippet edits and deletes remain document actions"
        );

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        let mut palette_resources = resources.clone();
        palette_resources.push(snip.clone());
        palette.load_open_resources(&palette_resources);
        palette.open();
        palette.set_query("New snippet");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("New snippet")),
            "{labels:?}"
        );
        palette.set_query("Delete snippet");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Delete snippet")),
            "{labels:?}"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the doc and records exactly one undo boundary, and Ctrl+Z reverts
    /// precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let (_app, seeded_doc) = seeded();
        let mut state = ui::State::new();
        state.doc = seeded_doc;
        let target = first_snippet(&state.doc);
        let before_heads = state.doc.get_heads();
        assert!(
            !target.favorite,
            "seed picks an unstarred snippet as the target"
        );

        let action = set_favorite_action(target.id, true);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert!(
            SnippetsDoc::get(&state.doc, target.id).unwrap().favorite,
            "the action starred the snippet"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert!(
            !SnippetsDoc::get(&state.doc, target.id).unwrap().favorite,
            "undo reverted the star and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let target = first_snippet(&a);
        let action = rename_action(target.id, "Converged Title");
        let encoded = encode(&action);

        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(
            SnippetsDoc::get(&a, target.id).unwrap().title,
            "Converged Title"
        );
        assert_eq!(
            SnippetsDoc::get(&a, target.id).unwrap().title,
            SnippetsDoc::get(&b, target.id).unwrap().title,
        );
    }

    /// Deleting a snippet through its action removes exactly that row from the
    /// authoritative document.
    #[test]
    fn delete_action_removes_the_snippet() {
        let (_app, mut doc) = seeded();
        let before = SnippetsDoc::list(&doc).len();
        let target = first_snippet(&doc);

        let action = delete_snippet_action(target.id);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let left = SnippetsDoc::list(&doc);
        assert_eq!(left.len(), before - 1, "exactly one snippet removed");
        assert!(
            left.iter().all(|s| s.id != target.id),
            "the target row is gone"
        );
    }

    /// A malformed id is a no-op, not an error — a bad replay can't wedge a peer.
    #[test]
    fn malformed_id_is_a_no_op() {
        let mut doc = AutoCommit::new();
        let bogus = SnippetsAction::DeleteSnippet {
            id: "not-a-uuid".into(),
        };
        assert!(bogus.apply(&mut doc).is_ok());
        let rename = SnippetsAction::Rename {
            id: "also-bad".into(),
            title: "x".into(),
        };
        assert!(rename.apply(&mut doc).is_ok());
        assert!(
            SnippetsDoc::list(&doc).is_empty(),
            "nothing was created by malformed actions"
        );
    }

    /// Single source of truth: the palette's immediate specs for a snippet build
    /// the byte-identical [`EncodedAction`] the direct builders do.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (app, doc) = seeded();
        let snippet = first_snippet(&doc);
        let snip = Widget::resource(&app, &doc, &snippet_resource_id(snippet.id))
            .expect("a snippet resource");
        let id_str = snip.id.as_str().trim_start_matches("snippets.snippet:");
        let id: SnippetId = id_str.parse().unwrap();
        let seeded_fav = SnippetsDoc::get(&doc, id).unwrap().favorite;

        // Delete is immediate on both sides.
        let del_spec = snip.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(del_spec);
        let from_builder = encode(&delete_snippet_action(id));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "delete is one source of truth"
        );

        // Toggle-favorite carries the flipped value, matching the toolbar Star.
        let fav_spec = snip
            .actions
            .iter()
            .find(|a| a.id == "toggle-favorite")
            .unwrap();
        let from_spec = immediate_encode(fav_spec);
        let from_builder = encode(&set_favorite_action(id, !seeded_fav));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "star is one source of truth"
        );

        // A prompted rename matches the rename builder for the same argument.
        let rename_spec = snip.actions.iter().find(|a| a.id == "rename").unwrap();
        let from_spec = prompted_encode(rename_spec, "Renamed");
        let from_builder = encode(&rename_action(id, "Renamed"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );
    }

    #[test]
    fn parse_tags_trims_dedups_and_drops_empties() {
        assert_eq!(parse_tags("  k8s , deploy ,, k8s "), vec!["k8s", "deploy"]);
        assert!(parse_tags("  ,  , ").is_empty());
    }

    #[test]
    fn set_language_round_trips_through_the_slug() {
        let mut doc = AutoCommit::new();
        let mut s = Snippet::blank("Ops");
        s.language = Language::PlainText;
        SnippetsDoc::upsert(&mut doc, &s);
        set_language_action(s.id, Language::Rust)
            .apply(&mut doc)
            .unwrap();
        assert_eq!(
            SnippetsDoc::get(&doc, s.id).unwrap().language,
            Language::Rust
        );
    }
}
