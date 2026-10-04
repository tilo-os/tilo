//! Snippets' **actions** and the nested resources it publishes beneath the
//! library.
//!
//! Every mutation of the shared snippet library is a [`SnippetsAction`]: a small
//! serializable value that deterministically mutates the CRDT. Because the
//! library already lives in the shared document (`root["snippets"]`, keyed by
//! uuid — see [`crate::doc`]), an action replicates and replays for free, and
//! feeds straight back into the list + detail panes. The on-screen affordances
//! (the `+ New` button, the Star / Duplicate / Delete toolbar, the language
//! pill, Copy) submit the same values through the framework dispatcher that
//! the palette's published actions build — one source of truth, one replay
//! path.
//!
//! The published children mirror the model: each snippet exports the
//! operations offered for it (rename, set language, move, tag, describe, star,
//! delete) and its body as editable text, each tag reveals its snippets, and
//! each folder files a new snippet under itself.
//!
//! Presentation — which snippet is selected, the search text, the scroll offset,
//! the open expand-modal, copying text to the clipboard — stays out of the
//! document and never becomes an action.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{ActionDecl, PlainText, TextEdit, declaration_spec, publish_child};
use uuid::Uuid;

use tilo_actions::{
    Action, ActionAddress, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    ExecutionLocus, OperationKind, ParamType, Resource, RetryPolicy,
};

use crate::doc::SnippetsDoc;
use crate::model::{Language, Snippet, SnippetId};
use crate::resource::Snippets;

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
    hex::encode(value)
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
    /// Edit the body text: one splice of the snippet's body, so two people
    /// typing in the same body merge character by character.
    EditBody { id: String, edit: TextEdit },
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
            SnippetsAction::EditBody { id, edit } => {
                if let Some(text) =
                    Self::parse_id(id).and_then(|id| SnippetsDoc::body_text(doc, id))
                {
                    edit.apply_under(doc, &text)?;
                }
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

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the published resources below.
// ===========================================================================

pub fn create_snippet_action(snippet: Snippet) -> SnippetsAction {
    SnippetsAction::CreateSnippet { snippet }
}

pub fn delete_snippet_action(id: SnippetId) -> SnippetsAction {
    SnippetsAction::DeleteSnippet { id: id.to_string() }
}

/// Wrap one edit of snippet `id`'s body text as the library's action.
pub fn edit_body_action(id: SnippetId, edit: TextEdit) -> SnippetsAction {
    SnippetsAction::EditBody {
        id: id.to_string(),
        edit,
    }
}

/// Replace snippet `id`'s whole body with `body`, written as the smallest
/// splice of what the document holds.
pub fn set_body_action(id: SnippetId, body: &str) -> SnippetsAction {
    edit_body_action(
        id,
        TextEdit::ReplaceText {
            text: body.to_owned(),
        },
    )
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
// Published children
// ===========================================================================

/// One-line label for a snippet resource in the palette.
fn snippet_label(s: &Snippet) -> String {
    let t = s.title.trim();
    let base = if t.is_empty() { "Untitled snippet" } else { t };
    format!("{} · {}", base, s.language.label())
}

/// Library-level View ops (search focus). Document mutations live on
/// [`crate::resource::Snippets::declarations`] / [`crate::resource::Snippets::children`].
pub(crate) fn library_view_specs() -> Vec<ActionSpec> {
    vec![view_operation(
        "search",
        "Search snippets",
        &SnippetsOperation::FocusSearch,
    )]
}

/// Document-derived children: folders, tags and snippets.
pub(crate) fn publish_children(snippets: &[Snippet]) -> Vec<Resource> {
    let folders = snippets
        .iter()
        .map(|snippet| snippet.folder.clone())
        .filter(|folder| !folder.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    let tags = snippets
        .iter()
        .flat_map(|snippet| snippet.tags.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>();
    let mut children =
        Vec::with_capacity(folders.len() + tags.len().saturating_add(snippets.len()));
    for folder in folders {
        children.push(provide_folder_resource(folder));
    }
    for tag in tags {
        children.push(provide_tag_resource(tag));
    }
    for snippet in snippets {
        children.push(provide_snippet_resource(snippet));
    }
    children
}

/// Stable resource id for a folder.
pub fn folder_resource_id(folder: &str) -> String {
    format!("snippets.folder:{}", stable_key(folder))
}

/// A blank snippet titled `title` (or the default title when it is blank)
/// filed under `folder`. The uuid is minted here, when the action is built on
/// the origin, so replay reproduces the identical row.
pub fn titled_snippet(title: &str, folder: &str) -> Snippet {
    let mut snippet = Snippet::blank(folder);
    snippet.id = Uuid::new_v4();
    let title = title.trim();
    if !title.is_empty() {
        snippet.title = title.to_string();
    }
    snippet
}

/// A folder, which files a new snippet under itself.
pub fn provide_folder_resource(folder: String) -> Resource {
    let id = folder_resource_id(&folder);
    let label = folder.clone();
    let create = ActionDecl::<Snippets>::new(
        "new-snippet",
        format!("New snippet in {folder}"),
        vec![ActionParam::required("title", ParamType::Text, "Title")],
        move |args, _| {
            Ok(create_snippet_action(titled_snippet(
                args.text("title").unwrap_or_default(),
                &folder,
            )))
        },
    )
    .describe("Create a new snippet in this folder.");
    Resource::new(id, "snippets.folder", label).with_action(declaration_spec(&create))
}

pub fn provide_tag_resource(tag: String) -> Resource {
    Resource::new(tag_resource_id(&tag), "snippets.tag", &tag).with_action(view_operation(
        "reveal",
        "Reveal tagged snippets",
        &SnippetsOperation::RevealTag { tag },
    ))
}

/// The id a snippet's body text is published under, as a child of it.
pub fn body_resource_id(id: SnippetId) -> String {
    ui::tilo_resource::child_resource_id::<Snippets>(&format!("body:{id}"))
}

/// One edit of snippet `id` that takes a single text argument `param`.
fn text_decl(
    action: &'static str,
    label: &str,
    param: &'static str,
    prompt: &'static str,
    build: impl Fn(&str) -> SnippetsAction + Send + Sync + 'static,
) -> ActionSpec {
    declaration_spec(&ActionDecl::<Snippets>::new(
        action,
        label,
        vec![ActionParam::required(param, ParamType::Text, prompt)],
        move |args, _| Ok(build(args.text(param).unwrap_or_default())),
    ))
}

/// One snippet, with every edit it takes bound to it.
pub fn provide_snippet_resource(s: &Snippet) -> Resource {
    let id = s.id;
    let favorite = !s.favorite;
    let languages: Vec<&'static str> = Language::ALL.iter().map(|l| l.slug()).collect();
    let resource = Resource::new(
        snippet_resource_id(id),
        "snippets.snippet",
        snippet_label(s),
    )
    .with_action(view_operation(
        "open",
        "Open snippet",
        &SnippetsOperation::OpenSnippet { id },
    ))
    .with_action(text_decl(
        "edit-body",
        &ui::i18n!("snippets-edit-body"),
        "body",
        "The whole new body",
        move |body| set_body_action(id, body),
    ))
    .with_action(text_decl(
        "rename",
        "Rename",
        "title",
        "New title",
        move |t| rename_action(id, t.trim()),
    ))
    .with_action(declaration_spec(&ActionDecl::<Snippets>::new(
        "set-language",
        "Set language",
        vec![
            ActionParam::required("language", ParamType::Text, "Language").with_choices(languages),
        ],
        move |args, _| {
            Ok(set_language_action(
                id,
                Language::from_slug(&args.text("language").unwrap_or_default().to_lowercase()),
            ))
        },
    )))
    .with_action(text_decl(
        "move-to-folder",
        "Move to folder",
        "folder",
        "Folder name (blank = All Snippets)",
        move |t| set_folder_action(id, t.trim()),
    ))
    .with_action(text_decl(
        "set-tags",
        "Set tags",
        "tags",
        "Comma-separated tags",
        move |t| set_tags_action(id, &parse_tags(t)),
    ))
    .with_action(text_decl(
        "edit-description",
        "Edit description",
        "description",
        "Description",
        move |t| set_description_action(id, t),
    ))
    .with_action(declaration_spec(&ActionDecl::<Snippets>::new(
        "toggle-favorite",
        if s.favorite { "Unstar" } else { "Star" },
        Vec::new(),
        move |_, _| Ok(set_favorite_action(id, favorite)),
    )))
    .with_action(declaration_spec(
        &ActionDecl::<Snippets>::new("delete", "Delete snippet", Vec::new(), move |_, _| {
            Ok(delete_snippet_action(id))
        })
        .destructive(),
    ));
    resource.with_children(vec![publish_child::<PlainText, Snippets>(
        &body_resource_id(id),
        &format!("{} body", snippet_label(s)),
        Some(&s.body),
        move |edit| edit_body_action(id, edit),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        for snippet in crate::seed::starter_library() {
            create_snippet_action(snippet).apply(&mut doc).unwrap();
        }
        doc.commit();
        doc
    }

    fn first_snippet(doc: &AutoCommit) -> Snippet {
        SnippetsDoc::list(doc)
            .into_iter()
            .next()
            .expect("a seeded snippet")
    }

    /// Build the action `spec` publishes for `args`, as the door does.
    fn build(spec: &ActionSpec, args: serde_json::Value) -> SnippetsAction {
        let serde_json::Value::Object(args) = args else {
            panic!("arguments are an object");
        };
        let encoded = spec.build_json(&args).expect("arguments bind");
        assert_eq!(encoded.type_id, SNIPPETS_ACTION_TYPE, "{}", spec.id);
        serde_json::from_slice(&encoded.payload).expect("a snippets action")
    }

    fn action<'a>(resource: &'a Resource, id: &str) -> &'a ActionSpec {
        resource
            .actions
            .iter()
            .find(|spec| spec.id == id)
            .unwrap_or_else(|| panic!("{} offers {id}", resource.id.as_str()))
    }

    /// Every snippet, tag and folder is published with its edits bound to it,
    /// and an edit touches exactly the addressed row.
    #[test]
    fn children_bind_each_edit_to_their_snippet_tag_or_folder() {
        let mut doc = seeded();
        let snippets = SnippetsDoc::list(&doc);
        let children = publish_children(&snippets);
        let target = first_snippet(&doc);
        let snippet = children
            .iter()
            .find(|child| child.id.as_str() == snippet_resource_id(target.id))
            .expect("each snippet is a child");
        assert_eq!(action(snippet, "open").kind, OperationKind::View);
        for id in [
            "edit-body",
            "rename",
            "set-language",
            "move-to-folder",
            "set-tags",
            "edit-description",
            "toggle-favorite",
            "delete",
        ] {
            assert_eq!(action(snippet, id).kind, OperationKind::Document, "{id}");
        }
        assert_eq!(
            build(
                action(snippet, "rename"),
                serde_json::json!({"title": " Renamed "})
            ),
            rename_action(target.id, "Renamed")
        );
        assert_eq!(
            build(
                action(snippet, "set-tags"),
                serde_json::json!({"tags": "k8s , deploy,,k8s"})
            ),
            set_tags_action(target.id, &["k8s".into(), "deploy".into()])
        );
        assert_eq!(
            build(action(snippet, "toggle-favorite"), serde_json::json!({})),
            set_favorite_action(target.id, !target.favorite)
        );
        let body = snippet
            .children
            .iter()
            .find(|child| child.id.as_str() == body_resource_id(target.id))
            .expect("the body is a text child of its snippet");
        assert!(body.actions.iter().any(|spec| spec.id == "replace-text"));

        let others: Vec<Snippet> = snippets
            .iter()
            .filter(|s| s.id != target.id)
            .cloned()
            .collect();
        build(action(snippet, "delete"), serde_json::json!({}))
            .apply(&mut doc)
            .unwrap();
        assert!(SnippetsDoc::get(&doc, target.id).is_none());
        let mut left = SnippetsDoc::list(&doc);
        left.sort_by_key(|s| s.id);
        let mut expected = others;
        expected.sort_by_key(|s| s.id);
        assert_eq!(left, expected, "every other snippet is untouched");

        let tag_name = target.tags.first().expect("seeded snippets are tagged");
        let tag = children
            .iter()
            .find(|child| child.id.as_str() == tag_resource_id(tag_name))
            .expect("each tag is a child");
        let reveal = action(tag, "reveal");
        assert_eq!(reveal.kind, OperationKind::View);
        assert_eq!(
            reveal_tag_address(tag_name),
            ActionAddress::new(tag.id.as_str(), &reveal.id)
        );

        let folder_name = snippets
            .iter()
            .map(|s| s.folder.clone())
            .find(|f| !f.is_empty())
            .expect("seeded snippets are filed");
        let folder = children
            .iter()
            .find(|child| child.id.as_str() == folder_resource_id(&folder_name))
            .expect("each folder is a child");
        let SnippetsAction::CreateSnippet { snippet } = build(
            action(folder, "new-snippet"),
            serde_json::json!({"title": "Here"}),
        ) else {
            panic!("a folder creates snippets");
        };
        assert_eq!(
            (snippet.title.as_str(), snippet.folder.as_str()),
            ("Here", folder_name.as_str())
        );
    }

    /// Body edits splice the stored text, so concurrent typing in one body
    /// merges; malformed ids are no-ops.
    #[test]
    fn body_edits_merge_and_malformed_ids_are_no_ops() {
        use tilo_automerge::ApplyIncoming;
        let mut ours = AutoCommit::new();
        let mut snippet = Snippet::blank("");
        snippet.body = "select 1".into();
        create_snippet_action(snippet.clone())
            .apply(&mut ours)
            .unwrap();
        ours.commit();
        let mut theirs = ours.fork();
        edit_body_action(
            snippet.id,
            TextEdit::Splice {
                at: 8,
                delete: 0,
                insert: ";".into(),
            },
        )
        .apply(&mut theirs)
        .unwrap();
        theirs.commit();
        set_body_action(snippet.id, "SELECT 1")
            .apply(&mut ours)
            .unwrap();
        ours.commit();
        ours.merge_incoming(&mut theirs).unwrap();
        assert_eq!(
            SnippetsDoc::get(&ours, snippet.id).unwrap().body,
            "SELECT 1;"
        );

        let mut doc = AutoCommit::new();
        for bogus in [
            SnippetsAction::DeleteSnippet {
                id: "not-a-uuid".into(),
            },
            SnippetsAction::Rename {
                id: "also-bad".into(),
                title: "x".into(),
            },
            SnippetsAction::EditBody {
                id: "bad".into(),
                edit: TextEdit::ReplaceText { text: "x".into() },
            },
        ] {
            assert!(bogus.apply(&mut doc).is_ok());
        }
        assert!(SnippetsDoc::list(&doc).is_empty());
    }

    /// Tag parsing and language slugs.
    #[test]
    fn tag_and_language_inputs_normalise_and_round_trip() {
        assert_eq!(parse_tags("  k8s , deploy ,, k8s "), vec!["k8s", "deploy"]);
        assert!(parse_tags("  ,  , ").is_empty());

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
