//! The git client's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! ## The split — almost everything here is external
//!
//! The Tilo action-model recipe (`docs/action-model-recipe.md`) sorts every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`] (a serializable value that deterministically changes
//! the shared Automerge document, so undo and multi-peer replay come for free),
//! while presentation / view state stays plain app code. The git client is a
//! near-textbook case of a mostly-**external** app:
//!
//! - **External resource operations.** Every git operation — stage / unstage,
//!   commit, branch create / switch / delete, checkout, fetch / pull / push,
//!   stash, discard, per-hunk staging — mutates the *on-disk* repository through
//!   libgit2 (see [`crate::repo`]). They are exported as typed effects on the
//!   repository resource rather than disguised as document actions. The live
//!   repository is *projected* into the in-memory
//!   [`RepoModel`] and re-read after each op; that model
//!   is not persisted to the document either.
//!
//! - **Presentation — never actions.** Which screen is showing, the selected
//!   file or commit, scroll offsets, the sidebar flap, the armed-discard
//!   confirmation, keyboard focus — all local, per-instance view state.
//!
//! ## What genuinely lives in the shared document
//!
//! Two things live in the CRDT:
//!
//! - **Durable view preferences** under `root["settings"]` (see
//!   [`crate::settings`]) — diff layout, whitespace, syntax highlight, wrap.
//!   Each toggle is an absolute [`Action`] so replay is deterministic.
//! - **Shared repository projection** under `root["shared_repo"]` (see
//!   [`crate::doc`]) — branches, commits, remotes, tags, stashes and identity.
//!   Working-tree status is *not* projected; it stays a live hosted extra.
//!
//! Git mutations against the on-disk repository remain Effects. The resource
//! tree is owned by [`tilo_resource::publish`]: preferences are
//! [`bound_actions`], Effects/Views are [`local_operations`], and shared
//! children ride [`children`].

use automerge::AutoCommit;
use automerge::transaction::Transactable;
use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    OperationKind, Resource, RetryPolicy,
};

use crate::doc::{GitClientState, SharedRepo};
use crate::model::{ChangeArea, FileChange, RepoModel};
use crate::settings::{self, DiffLayout, Settings};

/// Registry `type_id` for every git-client action.
pub const GIT_CLIENT_ACTION_TYPE: &str = "git_client.action";
pub const GIT_CLIENT_OPERATION_TYPE: &str = "git_client.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitClientOperation {
    ShowChanges,
    ShowHistory,
    ShowBranches,
    OpenChange { area: String, path: String },
    OpenCommit { id: String },
    StageAll,
    UnstageAll,
    StageFile { path: String },
    UnstageFile { path: String },
    DiscardFile { path: String },
    Commit { message: String },
    CreateBranch { name: String },
    CheckoutBranch { name: String },
    DeleteBranch { name: String },
    SaveStash { message: String },
    ApplyStash { index: usize },
    Fetch { remote: String },
    Pull { remote: String },
    Push { remote: String },
}

fn operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    value: &GitClientOperation,
) -> Option<ActionSpec> {
    let spec = ActionSpec::operation(id, label, kind, GIT_CLIENT_OPERATION_TYPE, value).ok()?;
    match value {
        GitClientOperation::ShowChanges
        | GitClientOperation::ShowHistory
        | GitClientOperation::ShowBranches
        | GitClientOperation::OpenChange { .. }
        | GitClientOperation::OpenCommit { .. } => Some(spec.retry_policy(RetryPolicy::Idempotent)),
        GitClientOperation::StageAll
        | GitClientOperation::UnstageAll
        | GitClientOperation::StageFile { .. }
        | GitClientOperation::UnstageFile { .. } => {
            Some(spec.reversible().retry_policy(RetryPolicy::Idempotent))
        }
        GitClientOperation::CheckoutBranch { .. } | GitClientOperation::Fetch { .. } => {
            Some(spec.retry_policy(RetryPolicy::Idempotent))
        }
        GitClientOperation::DiscardFile { .. } | GitClientOperation::DeleteBranch { .. } => {
            Some(spec.destructive())
        }
        GitClientOperation::Commit { .. }
        | GitClientOperation::CreateBranch { .. }
        | GitClientOperation::SaveStash { .. }
        | GitClientOperation::ApplyStash { .. }
        | GitClientOperation::Pull { .. }
        | GitClientOperation::Push { .. } => Some(spec),
    }
}

/// Every collaborative operation the git client exports on the shared document.
///
/// Preference setters are absolute *sets* in `root["settings"]` — never
/// toggles — so the payload fully determines the resulting state. The prior
/// value is read (once, presentation-side) at *authoring* time to pick the
/// opposite value; the action itself carries only the target.
/// [`WriteSharedRepo`] replaces the machine-derived branch/commit/remote
/// projection wholesale (same grain as Disks' topology write).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitClientAction {
    /// Set the diff pane layout. `layout` is the document's string form
    /// (`"unified"` | `"split"`); any other value normalises to `"unified"`.
    SetDiffLayout { layout: String },
    /// Set whether the diff renders whitespace.
    SetShowWhitespace { on: bool },
    /// Set whether the diff is syntax-highlighted.
    SetSyntaxHighlight { on: bool },
    /// Set whether the diff soft-wraps long lines.
    SetWrapLines { on: bool },
    /// Replace the shared repository projection (no working-tree lists).
    WriteSharedRepo { json: String },
}

impl Action for GitClientAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            GitClientAction::SetDiffLayout { layout } => {
                let obj = settings::settings_obj(doc);
                // Normalise through the enum so an unknown payload lands on a
                // valid setting instead of wedging.
                let norm = layout
                    .parse::<DiffLayout>()
                    .unwrap_or(DiffLayout::Unified)
                    .as_str();
                doc.put(&obj, settings::keys::DIFF_LAYOUT, norm)?;
            }
            GitClientAction::SetShowWhitespace { on } => {
                let obj = settings::settings_obj(doc);
                doc.put(&obj, settings::keys::SHOW_WHITESPACE, *on)?;
            }
            GitClientAction::SetSyntaxHighlight { on } => {
                let obj = settings::settings_obj(doc);
                doc.put(&obj, settings::keys::SYNTAX_HIGHLIGHT, *on)?;
            }
            GitClientAction::SetWrapLines { on } => {
                let obj = settings::settings_obj(doc);
                doc.put(&obj, settings::keys::WRAP_LINES, *on)?;
            }
            GitClientAction::WriteSharedRepo { json } => {
                SharedRepo::write_json(doc, json);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // Preferences and the shared projection: this client applies +
        // replicates, peers replay.
        ExecutionLocus::ClientLocal
    }
}

/// Register the git client's action decoder for palette execution + remote
/// replay. Called once at app start.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<GitClientAction>(GIT_CLIENT_ACTION_TYPE);
}

/// Encode any [`GitClientAction`] into the wire form the palette and the
/// on-screen affordances both dispatch — the single point that turns a git
/// action into a replayable envelope.
///
/// # Errors
///
/// Returns an error if `action` fails to serialize into the wire envelope.
pub fn encode(action: &GitClientAction) -> Result<EncodedAction, ActionError> {
    EncodedAction::encode(GIT_CLIENT_ACTION_TYPE, action)
}

/// Build the "set the diff layout to `layout`" action — the single source of
/// truth shared by the app-bar segmented control (see [`crate::app`]) and the
/// Ctrl+K resource tree below.
pub fn set_diff_layout_action(layout: DiffLayout) -> GitClientAction {
    GitClientAction::SetDiffLayout {
        layout: layout.as_str().to_string(),
    }
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// Keep the palette's contextual tree cheap even for monorepos. Every omitted
/// item is still addressable through [`ResourceProvider::resource`].
pub const MAX_CONTEXT_CHANGES: usize = 48;
pub const MAX_CONTEXT_COMMITS: usize = 48;
pub const MAX_CONTEXT_REFS: usize = 32;
pub const MAX_CONTEXT_STASHES: usize = 24;
pub const MAX_CONTEXT_REMOTES: usize = 16;

fn stable_component(value: &str) -> String {
    hex::encode(value)
}

fn change_area_name(area: ChangeArea) -> &'static str {
    match area {
        ChangeArea::Staged => "staged",
        ChangeArea::Unstaged => "unstaged",
        ChangeArea::Untracked => "untracked",
    }
}

fn change_resource_id(area: ChangeArea, path: &str) -> String {
    format!(
        "git_client.diff:{}:{}",
        change_area_name(area),
        stable_component(path)
    )
}

fn commit_resource_id(id: &str) -> String {
    format!("git_client.commit:{id}")
}

fn branch_resource_id(name: &str) -> String {
    format!("git_client.branch:{}", stable_component(name))
}

fn tag_resource_id(name: &str) -> String {
    format!("git_client.tag:{}", stable_component(name))
}

fn remote_resource_id(name: &str) -> String {
    format!("git_client.remote:{}", stable_component(name))
}

fn change_resource(change: &FileChange) -> Resource {
    let area = change_area_name(change.area);
    let open = GitClientOperation::OpenChange {
        area: area.into(),
        path: change.path.clone(),
    };
    let mut resource = Resource::new(
        change_resource_id(change.area, &change.path),
        "git_client.diff",
        format!("{} — {area}", change.path),
    );
    if let Some(spec) = operation("open", "Open diff", OperationKind::View, &open) {
        resource = resource.with_action(spec);
    }
    let mutation = match change.area {
        ChangeArea::Staged => Some((
            "unstage",
            "Unstage file",
            GitClientOperation::UnstageFile {
                path: change.path.clone(),
            },
        )),
        ChangeArea::Unstaged | ChangeArea::Untracked => Some((
            "stage",
            "Stage file",
            GitClientOperation::StageFile {
                path: change.path.clone(),
            },
        )),
    };
    if let Some((id, label, value)) = mutation
        && let Some(spec) = operation(id, label, OperationKind::Effect, &value)
    {
        resource = resource.with_action(spec);
    }
    if change.area != ChangeArea::Staged
        && let Some(spec) = operation(
            "discard",
            "Discard file changes",
            OperationKind::Effect,
            &GitClientOperation::DiscardFile {
                path: change.path.clone(),
            },
        )
    {
        resource = resource.with_action(spec);
    }
    resource
}

fn commit_resource(commit: &crate::model::Commit) -> Resource {
    let mut resource = Resource::new(
        commit_resource_id(&commit.id),
        "git_client.commit",
        commit.summary.clone(),
    );
    if let Some(spec) = operation(
        "open",
        "Open commit",
        OperationKind::View,
        &GitClientOperation::OpenCommit {
            id: commit.id.clone(),
        },
    ) {
        resource = resource.with_action(spec);
    }
    resource.with_children(commit.files.iter().take(16).map(|file| {
        Resource::new(
            format!(
                "git_client.commit-file:{}:{}",
                commit.id,
                stable_component(&file.path)
            ),
            "git_client.commit-file",
            file.path.clone(),
        )
    }))
}

fn branch_resource(branch: &crate::model::Branch) -> Resource {
    let mut resource = Resource::new(
        branch_resource_id(&branch.name),
        "git_client.branch",
        branch.name.clone(),
    );
    if !branch.is_head
        && let Some(spec) = operation(
            "checkout",
            "Check out branch",
            OperationKind::Effect,
            &GitClientOperation::CheckoutBranch {
                name: branch.name.clone(),
            },
        )
    {
        resource = resource.with_action(spec);
    }
    if !branch.is_head
        && !branch.is_remote
        && let Some(spec) = operation(
            "delete",
            "Delete branch",
            OperationKind::Effect,
            &GitClientOperation::DeleteBranch {
                name: branch.name.clone(),
            },
        )
    {
        resource = resource.with_action(spec);
    }
    resource
}

fn remote_resource(remote: &crate::model::Remote) -> Resource {
    let mut resource = Resource::new(
        remote_resource_id(&remote.name),
        "git_client.remote",
        remote.name.clone(),
    );
    for (id, label, value) in [
        (
            "fetch",
            "Fetch remote",
            GitClientOperation::Fetch {
                remote: remote.name.clone(),
            },
        ),
        (
            "pull",
            "Pull from remote",
            GitClientOperation::Pull {
                remote: remote.name.clone(),
            },
        ),
        (
            "push",
            "Push to remote",
            GitClientOperation::Push {
                remote: remote.name.clone(),
            },
        ),
    ] {
        if let Some(spec) = operation(id, label, OperationKind::Effect, &value) {
            resource = resource.with_action(spec);
        }
    }
    resource
}

/// Preference flips bound from live settings for [`tilo_resource::publish`].
pub(crate) fn document_bound_actions(settings: Settings) -> Vec<ActionSpec> {
    let mut actions = Vec::new();
    let next_layout = match settings.diff_layout {
        DiffLayout::Unified => DiffLayout::SideBySide,
        DiffLayout::SideBySide => DiffLayout::Unified,
    };
    let layout_label = match next_layout {
        DiffLayout::SideBySide => "Diff layout: side-by-side",
        DiffLayout::Unified => "Diff layout: unified",
    };
    if let Ok(spec) = ActionSpec::immediate(
        "diff-layout",
        layout_label,
        GIT_CLIENT_ACTION_TYPE,
        &set_diff_layout_action(next_layout),
    ) {
        actions.push(spec);
    }
    if let Ok(spec) = ActionSpec::immediate(
        "syntax-highlight",
        if settings.syntax_highlight {
            "Turn off syntax highlighting"
        } else {
            "Turn on syntax highlighting"
        },
        GIT_CLIENT_ACTION_TYPE,
        &GitClientAction::SetSyntaxHighlight {
            on: !settings.syntax_highlight,
        },
    ) {
        actions.push(spec);
    }
    if let Ok(spec) = ActionSpec::immediate(
        "show-whitespace",
        if settings.show_whitespace {
            "Hide whitespace"
        } else {
            "Show whitespace"
        },
        GIT_CLIENT_ACTION_TYPE,
        &GitClientAction::SetShowWhitespace {
            on: !settings.show_whitespace,
        },
    ) {
        actions.push(spec);
    }
    if let Ok(spec) = ActionSpec::immediate(
        "wrap-lines",
        if settings.wrap_lines {
            "Stop wrapping long lines"
        } else {
            "Wrap long lines"
        },
        GIT_CLIENT_ACTION_TYPE,
        &GitClientAction::SetWrapLines {
            on: !settings.wrap_lines,
        },
    ) {
        actions.push(spec);
    }
    actions
}

/// View / Effect catalogue on the repository door (no document delta).
#[allow(
    clippy::too_many_lines,
    reason = "one catalogue lists every repository View and Effect"
)]
pub(crate) fn root_local_operations() -> Vec<ActionSpec> {
    let mut actions = Vec::new();
    for (id, label, value) in [
        (
            "show-changes",
            "Show changes",
            GitClientOperation::ShowChanges,
        ),
        (
            "show-history",
            "Show history",
            GitClientOperation::ShowHistory,
        ),
        (
            "show-branches",
            "Show branches and remotes",
            GitClientOperation::ShowBranches,
        ),
    ] {
        if let Some(spec) = operation(id, label, OperationKind::View, &value) {
            actions.push(spec);
        }
    }
    for (id, label, value) in [
        (
            "stage-all",
            "Stage all changes",
            GitClientOperation::StageAll,
        ),
        (
            "unstage-all",
            "Unstage all changes",
            GitClientOperation::UnstageAll,
        ),
    ] {
        if let Some(spec) = operation(id, label, OperationKind::Effect, &value) {
            actions.push(spec);
        }
    }
    actions.push(
        ActionSpec::prompted_operation(
            "commit",
            "Commit staged changes",
            "Commit message",
            "summary",
            OperationKind::Effect,
            GIT_CLIENT_OPERATION_TYPE,
            |message| {
                EncodedAction::encode_operation(
                    GIT_CLIENT_OPERATION_TYPE,
                    &GitClientOperation::Commit {
                        message: message.to_string(),
                    },
                )
                .unwrap_or_else(|error| EncodedAction {
                    type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                    payload: format!("git commit operation could not be serialized: {error}")
                        .into_bytes(),
                })
            },
        )
        .with_description("Create a commit from the currently staged changes"),
    );
    actions.push(ActionSpec::prompted_operation(
        "create-branch",
        "Create branch",
        "Branch name",
        "feature/name",
        OperationKind::Effect,
        GIT_CLIENT_OPERATION_TYPE,
        |name| {
            EncodedAction::encode_operation(
                GIT_CLIENT_OPERATION_TYPE,
                &GitClientOperation::CreateBranch {
                    name: name.to_string(),
                },
            )
            .unwrap_or_else(|error| EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: format!("git branch operation could not be serialized: {error}")
                    .into_bytes(),
            })
        },
    ));
    actions.push(ActionSpec::prompted_operation(
        "save-stash",
        "Stash working changes",
        "Stash message",
        "work in progress",
        OperationKind::Effect,
        GIT_CLIENT_OPERATION_TYPE,
        |message| {
            EncodedAction::encode_operation(
                GIT_CLIENT_OPERATION_TYPE,
                &GitClientOperation::SaveStash {
                    message: message.to_string(),
                },
            )
            .unwrap_or_else(|error| EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: format!("git stash operation could not be serialized: {error}")
                    .into_bytes(),
            })
        },
    ));
    actions
}

/// Working-tree status collection — live when hosted, included in standalone
/// publish when the model carries staged/unstaged/untracked rows.
pub(crate) fn changes_collection(model: &RepoModel) -> Resource {
    Resource::new("git_client.changes", "git_client.collection", "Changes").with_children(
        model
            .staged
            .iter()
            .chain(model.unstaged.iter())
            .chain(model.untracked.iter())
            .take(MAX_CONTEXT_CHANGES)
            .map(change_resource),
    )
}

/// Shared children for [`tilo_resource::publish`]: history, branches, tags,
/// stashes, remotes. Working-tree Changes stay off this list.
pub(crate) fn document_children(model: &RepoModel) -> Vec<Resource> {
    let mut children = Vec::new();
    children.push(
        Resource::new("git_client.history", "git_client.collection", "History").with_children(
            model
                .commits
                .iter()
                .take(MAX_CONTEXT_COMMITS)
                .map(commit_resource),
        ),
    );
    children.push(
        Resource::new("git_client.branches", "git_client.collection", "Branches").with_children(
            model
                .branches
                .iter()
                .take(MAX_CONTEXT_REFS)
                .map(branch_resource),
        ),
    );
    children.push(
        Resource::new("git_client.tags", "git_client.collection", "Tags").with_children(
            model.tags.iter().take(MAX_CONTEXT_REFS).map(|tag| {
                Resource::new(
                    tag_resource_id(&tag.name),
                    "git_client.tag",
                    tag.name.clone(),
                )
            }),
        ),
    );
    children.push(
        Resource::new("git_client.stashes", "git_client.collection", "Stashes").with_children(
            model.stashes.iter().take(MAX_CONTEXT_STASHES).map(|stash| {
                let mut resource = Resource::new(
                    format!("git_client.stash:{}", stash.index),
                    "git_client.stash",
                    stash.message.clone(),
                );
                if let Some(spec) = operation(
                    "apply",
                    "Apply stash",
                    OperationKind::Effect,
                    &GitClientOperation::ApplyStash { index: stash.index },
                ) {
                    resource = resource.with_action(spec);
                }
                resource
            }),
        ),
    );
    children.push(
        Resource::new("git_client.remotes", "git_client.collection", "Remotes").with_children(
            model
                .remotes
                .iter()
                .take(MAX_CONTEXT_REMOTES)
                .map(remote_resource),
        ),
    );
    children
}

/// Children on the framework resource: working-tree when present on the
/// projection used for publish, then shared history/branches/remotes.
pub(crate) fn publish_children(state: &GitClientState) -> Vec<Resource> {
    let mut children = Vec::new();
    if state.repo.change_count() > 0 {
        children.push(changes_collection(&state.repo));
    }
    children.extend(document_children(&state.repo));
    children
}

pub(crate) fn repo_label(model: &RepoModel) -> String {
    let repo_name = model.name.as_str();
    if repo_name.trim().is_empty() {
        "Repository".to_string()
    } else {
        repo_name.to_string()
    }
}

/// Build the resource tree via [`tilo_resource::publish`]. Standalone /
/// `--shots` pass the live model (including working-tree rows); the framework
/// door reads the shared projection from the document.
pub fn provide_resources(model: &RepoModel, doc: &AutoCommit) -> Vec<Resource> {
    // Branches, commits, and remotes come from the document once it holds the
    // shared projection. A caller that has not written one yet still publishes
    // the model it passed. Working-tree Changes stay a live extra on that model.
    let shared = SharedRepo::read(doc);
    let state = if shared.is_some() {
        GitClientState::from_doc(doc)
    } else {
        GitClientState {
            settings: Settings::read_only(doc),
            repo: model.clone(),
        }
    };
    let label = repo_label(if shared.is_some() { &state.repo } else { model });
    let mut root =
        ui::tilo_resource::publish::<crate::resource::GitClient>("git_client.repo", &label, &state);
    if shared.is_some()
        && model.change_count() > 0
        && !root
            .children
            .iter()
            .any(|child| child.id.as_str() == "git_client.changes")
    {
        root.children.insert(0, changes_collection(model));
    }
    vec![root]
}

/// Resolve an arbitrary current repository item without building the complete
/// hierarchy. This keeps search/history/agent addresses valid beyond the
/// contextual caps above while rejecting stale ids after a fetch or checkout.
fn find_git(resource: &Resource, id: &str) -> Option<Resource> {
    if resource.id.as_str() == id {
        return Some(resource.clone());
    }
    for child in &resource.children {
        if let Some(found) = find_git(child, id) {
            return Some(found);
        }
    }
    None
}

pub fn resolve_resource(model: &RepoModel, doc: &AutoCommit, id: &str) -> Option<Resource> {
    let root = provide_resources(model, doc).into_iter().next()?;
    if let Some(found) = find_git(&root, id) {
        return Some(found);
    }
    for change in model
        .staged
        .iter()
        .chain(model.unstaged.iter())
        .chain(model.untracked.iter())
    {
        if change_resource_id(change.area, &change.path) == id {
            return Some(change_resource(change));
        }
    }
    if let Some(commit) = model
        .commits
        .iter()
        .find(|commit| commit_resource_id(&commit.id) == id)
    {
        return Some(commit_resource(commit));
    }
    for commit in &model.commits {
        if let Some(file) = commit.files.iter().find(|file| {
            format!(
                "git_client.commit-file:{}:{}",
                commit.id,
                stable_component(&file.path)
            ) == id
        }) {
            return Some(Resource::new(
                id,
                "git_client.commit-file",
                file.path.clone(),
            ));
        }
    }
    if let Some(branch) = model
        .branches
        .iter()
        .find(|branch| branch_resource_id(&branch.name) == id)
    {
        return Some(branch_resource(branch));
    }
    if let Some(tag) = model
        .tags
        .iter()
        .find(|tag| tag_resource_id(&tag.name) == id)
    {
        return Some(Resource::new(id, "git_client.tag", tag.name.clone()));
    }
    if let Some(stash) = model
        .stashes
        .iter()
        .find(|stash| format!("git_client.stash:{}", stash.index) == id)
    {
        let mut resource = Resource::new(id, "git_client.stash", stash.message.clone());
        if let Some(spec) = operation(
            "apply",
            "Apply stash",
            OperationKind::Effect,
            &GitClientOperation::ApplyStash { index: stash.index },
        ) {
            resource = resource.with_action(spec);
        }
        return Some(resource);
    }
    model
        .remotes
        .iter()
        .find(|remote| remote_resource_id(&remote.name) == id)
        .map(remote_resource)
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::GitClientApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.resource_model() {
            Some(model) => provide_resources(model, doc),
            None => Vec::new(),
        }
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        resolve_resource(self.resource_model()?, doc, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::LocalTransport;
    use ui::testing::action_contract::assert_spec_encodes_as;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// Dispatch an action through the executor, exactly as the on-screen
    /// affordance path does (decode from the wire, apply through a local
    /// transport).
    fn dispatch(doc: &mut AutoCommit, action: &GitClientAction) {
        let encoded = encode(action).unwrap();
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();
    }

    /// A base document with the default settings seeded, as the app's first
    /// `sync` leaves it — so toggling a preference is a genuine change.
    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        Settings::default().write(&mut doc);
        doc.commit();
        doc
    }

    fn repo_model() -> RepoModel {
        RepoModel {
            name: "tilo".into(),
            ..RepoModel::default()
        }
    }

    /// Preference actions: each writes its absolute value into `root["settings"]`, a
    /// malformed diff layout normalises, and palette toggles target the opposite of
    /// the live value. Each phase is a former test with its own fresh fixture.
    #[test]
    fn preference_actions_write_normalised_values_that_track_live_state() {
        preference_actions_write_the_settings();
        malformed_diff_layout_normalises_instead_of_wedging();
        palette_actions_target_the_opposite_of_current_state();
    }

    /// Each preference action writes its absolute value into `root["settings"]`.
    fn preference_actions_write_the_settings() {
        let mut doc = seeded();
        assert_eq!(Settings::read_only(&doc).diff_layout, DiffLayout::Unified);
        assert!(Settings::read_only(&doc).syntax_highlight);

        dispatch(&mut doc, &set_diff_layout_action(DiffLayout::SideBySide));
        dispatch(&mut doc, &GitClientAction::SetSyntaxHighlight { on: false });
        dispatch(&mut doc, &GitClientAction::SetShowWhitespace { on: true });
        dispatch(&mut doc, &GitClientAction::SetWrapLines { on: true });

        let s = Settings::read_only(&doc);
        assert_eq!(s.diff_layout, DiffLayout::SideBySide);
        assert!(!s.syntax_highlight);
        assert!(s.show_whitespace);
        assert!(s.wrap_lines);
    }

    /// A malformed diff-layout payload normalises to the canonical form rather
    /// than erroring, so an out-of-range replay lands on a valid setting.
    fn malformed_diff_layout_normalises_instead_of_wedging() {
        let mut doc = seeded();
        dispatch(
            &mut doc,
            &GitClientAction::SetDiffLayout {
                layout: "nonsense".into(),
            },
        );
        assert_eq!(
            Settings::read_only(&doc).diff_layout,
            DiffLayout::Unified,
            "an unknown layout falls back to the canonical default"
        );
    }

    /// The repository resource tree: Ctrl+K publishes repo, preference and view
    /// actions in order; git objects carry confirmation contracts; contextual
    /// resources are bounded while omitted items still resolve. Each phase is a former test with its own fresh fixture.
    #[test]
    fn repository_resource_tree_exports_bounded_git_objects_and_contracts() {
        ctrl_k_publishes_the_repo_and_preference_actions();
        repository_resources_cover_git_objects_and_confirmation_contracts();
        contextual_resources_are_bounded_but_omitted_items_resolve();
    }

    /// Ctrl+K on an open repository publishes the repo resource exporting the
    /// preference, view and repository actions in order. Drives the real
    /// resource builder.
    fn ctrl_k_publishes_the_repo_and_preference_actions() {
        let doc = seeded();
        let resources = provide_resources(&repo_model(), &doc);
        assert_eq!(resources.len(), 1, "one repository root");
        let repo = &resources[0];
        assert_eq!(repo.kind, "git_client.repo");
        assert_eq!(repo.label, "tilo");
        assert_eq!(repo.id.as_str(), "git_client.repo");

        let ids: Vec<&str> = repo.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            "diff-layout",
            "syntax-highlight",
            "show-whitespace",
            "wrap-lines",
            "show-changes",
            "show-history",
            "show-branches",
            "stage-all",
            "unstage-all",
            "commit",
            "create-branch",
            "save-stash",
        ] {
            assert!(ids.contains(&expected), "missing {expected} in {ids:?}");
        }
    }

    /// The palette's exported actions track the live document: with a non-default
    /// state seeded, each toggle targets the opposite of the current value.
    fn palette_actions_target_the_opposite_of_current_state() {
        let mut doc = AutoCommit::new();
        let s = Settings {
            diff_layout: DiffLayout::SideBySide,
            syntax_highlight: false,
            ..Settings::default()
        };
        s.write(&mut doc);
        doc.commit();

        let resources = provide_resources(&repo_model(), &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "diff-layout")
            .unwrap();
        // Currently side-by-side, so the exported action flips back to unified.
        assert_spec_encodes_as(
            spec,
            None,
            &encode(&set_diff_layout_action(DiffLayout::Unified)).unwrap(),
        );
        let syn = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "syntax-highlight")
            .unwrap();
        assert_spec_encodes_as(
            syn,
            None,
            &encode(&GitClientAction::SetSyntaxHighlight { on: true }).unwrap(),
        );
    }

    ui::action_contract_tests! {
        seed: seeded(),
        registry: registry(),
        resources: |doc| provide_resources(&crate::sample::model().0, doc),
        palette: ["side-by-side", "syntax highlighting"],
        replay: [
            diff_layout => |_| encode(&set_diff_layout_action(DiffLayout::SideBySide)).unwrap(),
            syntax_highlight => |_| encode(&GitClientAction::SetSyntaxHighlight { on: false }).unwrap(),
        ],
        undo: [
            diff_layout => |_| encode(&set_diff_layout_action(DiffLayout::SideBySide)).unwrap(),
            wrap_lines => |_| encode(&GitClientAction::SetWrapLines { on: true }).unwrap(),
        ],
        encodes: [
            diff_layout => ("git_client.repo", "diff-layout", |_, _| {
                encode(&set_diff_layout_action(DiffLayout::SideBySide)).unwrap()
            }),
            syntax_highlight => ("git_client.repo", "syntax-highlight", |_, _| {
                encode(&GitClientAction::SetSyntaxHighlight { on: false }).unwrap()
            }),
        ],
    }

    fn repository_resources_cover_git_objects_and_confirmation_contracts() {
        let (model, _) = crate::sample::model();
        let doc = seeded();
        let root = provide_resources(&model, &doc).remove(0);
        let kinds: std::collections::HashSet<_> = root
            .children
            .iter()
            .flat_map(|collection| collection.children.iter())
            .map(|resource| resource.kind.as_str())
            .collect();
        for expected in [
            "git_client.diff",
            "git_client.commit",
            "git_client.branch",
            "git_client.tag",
            "git_client.stash",
            "git_client.remote",
        ] {
            assert!(kinds.contains(expected), "missing {expected}: {kinds:?}");
        }
        let discard = root
            .children
            .iter()
            .flat_map(|collection| &collection.children)
            .find(|resource| resource.kind == "git_client.diff")
            .and_then(|resource| {
                resource
                    .actions
                    .iter()
                    .find(|action| action.id == "discard")
            });
        if let Some(discard) = discard {
            assert_eq!(
                discard.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm,
                "agent/palette discard must pass the host confirmation gate"
            );
            assert_eq!(discard.contract.risk, tilo_actions::RiskLevel::Destructive);
        }
    }

    fn contextual_resources_are_bounded_but_omitted_items_resolve() {
        let (mut model, _) = crate::sample::model();
        let template = model.commits[0].clone();
        model.commits = (0..MAX_CONTEXT_COMMITS + 9)
            .map(|index| {
                let mut commit = template.clone();
                commit.id = format!("{index:040x}");
                commit.summary = format!("Commit {index}");
                commit
            })
            .collect();
        let omitted = commit_resource_id(&model.commits[MAX_CONTEXT_COMMITS + 3].id);
        let doc = seeded();
        let root = provide_resources(&model, &doc).remove(0);
        let history = root
            .children
            .iter()
            .find(|resource| resource.id.as_str() == "git_client.history")
            .unwrap();
        assert_eq!(history.children.len(), MAX_CONTEXT_COMMITS);
        assert!(
            history
                .children
                .iter()
                .all(|resource| resource.id.as_str() != omitted)
        );
        assert_eq!(
            resolve_resource(&model, &doc, &omitted)
                .expect("omitted commit resolves lazily")
                .id
                .as_str(),
            omitted
        );
    }
}
