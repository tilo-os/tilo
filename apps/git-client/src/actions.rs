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
//!   file or commit, scroll offsets, the list drawer, a pending discard
//!   confirmation, keyboard focus — all local, per-instance view state.
//!
//! ## What genuinely lives in the shared document
//!
//! Two things live in the CRDT:
//!
//! - **The repository the record opens** under `root["repository"]`.
//! - **Shared repository projection** under `root["shared_repo"]` (see
//!   [`crate::doc`]) — branches, commits, remotes, tags, stashes and identity.
//!   Working-tree status is *not* projected; it stays a live hosted extra.
//!
//! Git work against the on-disk repository is the [`GitClientEffect`]
//! vocabulary, answered by the surface. The resource tree is owned by
//! [`tilo_resource::publish`]: shared children ride the resource's children
//! with their item effects bound, and working-tree status is the surface's
//! live rows. The diff preferences are the person's settings
//! ([`crate::settings`]), switched by the surface's effects.

use crate::i18n::t;
use automerge::transaction::Transactable;
use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionError, ActionRegistry, Doc, EncodedAction, ExecutionLocus, Resource,
};

use ui::framework::LiveRow;
use ui::tilo_resource::{self, Effect};

use crate::doc::SharedRepo;
use crate::model::{ChangeArea, FileChange, RepoModel};

/// Registry `type_id` for every git-client action.
pub const GIT_CLIENT_ACTION_TYPE: &str = "git_client.action";
/// The git client's work on the repository on disk and what it shows of one,
/// which change no document. Navigation and whole-tree work are offered on
/// the repository; everything else rides the changed file, commit, branch,
/// stash or remote it acts on, named by path, id or name and resolved again
/// when invoked.
#[derive(Debug, Clone, PartialEq, Eq, Effect)]
#[effect(operation = "git_client.effect", device_only)]
pub enum GitClientEffect {
    /// Run approved project check
    #[effect(job, cancellable, returns, untrusted, device_only = false)]
    RunApprovedCheck {
        #[param("Check ID approved by this exact project host")]
        check_id: String,
    },
    /// Show changes
    #[effect(view)]
    ShowChanges,
    /// Show history
    #[effect(view)]
    ShowHistory,
    /// Show branches and remotes
    #[effect(view)]
    ShowBranches,
    /// Open diff
    #[effect(view, row)]
    OpenChange {
        #[param("Where the change is: staged, unstaged or untracked")]
        area: String,
        #[param("Path of the file in the repository")]
        path: String,
    },
    /// Open commit
    #[effect(view, row)]
    OpenCommit {
        #[param("The commit's id")]
        id: String,
    },
    /// Stage all changes
    #[effect(idempotent)]
    StageAll,
    /// Unstage all changes
    #[effect(idempotent)]
    UnstageAll,
    /// Stage file
    #[effect(row, idempotent)]
    StageFile {
        #[param("Path of the file in the repository")]
        path: String,
    },
    /// Unstage file
    #[effect(row, idempotent)]
    UnstageFile {
        #[param("Path of the file in the repository")]
        path: String,
    },
    /// Discard file changes
    #[effect(row, destructive)]
    DiscardFile {
        #[param("Path of the file in the repository")]
        path: String,
    },
    /// Commit staged changes
    ///
    /// Create a commit from the currently staged changes.
    Commit {
        #[param("Commit message")]
        message: String,
    },
    /// Create branch
    CreateBranch {
        #[param("Branch name")]
        name: String,
    },
    /// Create linked worktree
    ///
    /// Make a new branch at the selected commit and a separate working directory.
    #[effect(job)]
    CreateWorktree {
        #[param("Absolute path of the new working directory")]
        path: String,
        #[param("New branch name")]
        branch: String,
        #[param("Start point: branch, tag or commit")]
        start: String,
    },
    /// Check out branch
    #[effect(row, idempotent)]
    CheckoutBranch {
        #[param("Branch name")]
        name: String,
    },
    /// Delete branch
    #[effect(row, destructive)]
    DeleteBranch {
        #[param("Branch name")]
        name: String,
    },
    /// Stash working changes
    SaveStash {
        #[param("Stash message")]
        message: String,
    },
    /// Apply stash
    #[effect(row)]
    ApplyStash {
        #[param("The stash's index")]
        index: usize,
    },
    /// Fetch remote
    #[effect(row, job, idempotent)]
    Fetch {
        #[param("Remote name")]
        remote: String,
    },
    /// Pull from remote
    #[effect(row, job)]
    Pull {
        #[param("Remote name")]
        remote: String,
    },
    /// Push to remote
    #[effect(row, job)]
    Push {
        #[param("Remote name")]
        remote: String,
    },
    /// Switch the diff layout
    ///
    /// Between unified and side-by-side: a preference of this person, which
    /// follows them to their other devices.
    #[effect(permission = "read", device_only = false)]
    DiffLayout,
    /// Turn syntax highlighting on or off
    ///
    /// A preference of this person, which follows them to their other
    /// devices.
    #[effect(permission = "read", device_only = false)]
    SyntaxHighlight,
    /// Show or hide whitespace
    ///
    /// A preference of this person, which follows them to their other
    /// devices.
    #[effect(permission = "read", device_only = false)]
    ShowWhitespace,
    /// Wrap long lines or stop wrapping them
    ///
    /// A preference of this person, which follows them to their other
    /// devices.
    #[effect(permission = "read", device_only = false)]
    WrapLines,
}

/// Every collaborative operation the git client exports on the shared document.
///
/// [`WriteSharedRepo`] replaces the machine-derived branch/commit/remote
/// projection wholesale (same grain as Disks' topology write).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitClientAction {
    /// Replace the shared repository projection (no working-tree lists).
    WriteSharedRepo { json: String },
    /// Open the repository at `path` in this record.
    OpenRepository { path: String },
}

impl Action for GitClientAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            GitClientAction::WriteSharedRepo { json } => {
                SharedRepo::write_json(doc, json);
            }
            GitClientAction::OpenRepository { path } => {
                doc.put(
                    &automerge::ROOT,
                    crate::doc::keys::REPOSITORY,
                    path.as_str(),
                )?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // The repository and its shared projection: this client applies +
        // replicates, peers replay.
        ExecutionLocus::ClientLocal
    }

    /// The projection is read from the repository on disk, nobody's edit:
    /// it reaches every collaborator, and no undo takes it back.
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self {
            GitClientAction::WriteSharedRepo { .. } => tilo_actions::ChangeKind::External,
            GitClientAction::OpenRepository { .. } => tilo_actions::ChangeKind::Content,
        }
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

// ===========================================================================
// Resource tree
// ===========================================================================

/// Keep the palette's contextual tree cheap even for monorepos. Every omitted
/// item is still addressable by id ([`resolve_child`], [`working_tree_row`]).
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

pub(crate) fn change_resource_id(area: ChangeArea, path: &str) -> String {
    format!(
        "git_client.diff:{}:{}",
        change_area_name(area),
        stable_component(path)
    )
}

fn commit_resource_id(id: &str) -> String {
    format!("git_client.commit:{id}")
}

pub(crate) fn branch_resource_id(name: &str) -> String {
    format!("git_client.branch:{}", stable_component(name))
}

fn tag_resource_id(name: &str) -> String {
    format!("git_client.tag:{}", stable_component(name))
}

fn remote_resource_id(name: &str) -> String {
    format!("git_client.remote:{}", stable_component(name))
}

fn change_row(change: &FileChange) -> LiveRow {
    let area = change_area_name(change.area);
    let path = change.path.clone();
    let row = LiveRow::new(
        change_resource_id(change.area, &change.path),
        "git_client.diff",
        format!("{} — {area}", change.path),
    )
    .effect(&GitClientEffect::OpenChange {
        area: area.into(),
        path: path.clone(),
    });
    match change.area {
        ChangeArea::Staged => row.effect(&GitClientEffect::UnstageFile { path }),
        ChangeArea::Unstaged | ChangeArea::Untracked => row
            .effect(&GitClientEffect::StageFile { path: path.clone() })
            .effect(&GitClientEffect::DiscardFile { path }),
    }
}

fn commit_resource(commit: &crate::model::Commit) -> Resource {
    Resource::new(
        commit_resource_id(&commit.id),
        "git_client.commit",
        commit.summary.clone(),
    )
    .with_action(
        GitClientEffect::OpenCommit {
            id: commit.id.clone(),
        }
        .bound(),
    )
    .with_children(commit.files.iter().take(16).map(|file| {
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
    if !branch.is_head {
        resource = resource.with_action(
            GitClientEffect::CheckoutBranch {
                name: branch.name.clone(),
            }
            .bound(),
        );
    }
    if !branch.is_head && !branch.is_remote {
        resource = resource.with_action(
            GitClientEffect::DeleteBranch {
                name: branch.name.clone(),
            }
            .bound(),
        );
    }
    resource
}

fn remote_resource(remote: &crate::model::Remote) -> Resource {
    let name = || remote.name.clone();
    Resource::new(
        remote_resource_id(&remote.name),
        "git_client.remote",
        remote.name.clone(),
    )
    .with_action(GitClientEffect::Fetch { remote: name() }.bound())
    .with_action(GitClientEffect::Pull { remote: name() }.bound())
    .with_action(GitClientEffect::Push { remote: name() }.bound())
}

/// The working tree's changed files as live rows: the bounded page beneath
/// one `Changes` row, every file in `model`.
pub(crate) fn working_tree_rows(model: &RepoModel) -> LiveRow {
    model
        .staged
        .iter()
        .chain(model.unstaged.iter())
        .chain(model.untracked.iter())
        .take(MAX_CONTEXT_CHANGES)
        .map(change_row)
        .fold(
            LiveRow::new("git_client.changes", "git_client.collection", "Changes"),
            LiveRow::row,
        )
}

/// The changed file `id`, beyond the page [`working_tree_rows`] publishes.
pub(crate) fn working_tree_row(model: &RepoModel, id: &str) -> Option<LiveRow> {
    model
        .staged
        .iter()
        .chain(model.unstaged.iter())
        .chain(model.untracked.iter())
        .find(|change| change_resource_id(change.area, &change.path) == id)
        .map(change_row)
}

/// Shared children for [`tilo_resource::publish`]: history, branches, tags,
/// stashes, remotes. Working-tree Changes stay off this list.
pub(crate) fn document_children(model: &RepoModel) -> Vec<Resource> {
    let mut children = Vec::new();
    children.push(
        Resource::new(
            "git_client.history",
            "git_client.collection",
            t("git-screen-history"),
        )
        .with_children(
            model
                .commits
                .iter()
                .take(MAX_CONTEXT_COMMITS)
                .map(commit_resource),
        ),
    );
    children.push(
        Resource::new(
            "git_client.branches",
            "git_client.collection",
            t("git-section-branches"),
        )
        .with_children(
            model
                .branches
                .iter()
                .take(MAX_CONTEXT_REFS)
                .map(branch_resource),
        ),
    );
    children.push(
        Resource::new(
            "git_client.tags",
            "git_client.collection",
            t("git-section-tags"),
        )
        .with_children(model.tags.iter().take(MAX_CONTEXT_REFS).map(|tag| {
            Resource::new(
                tag_resource_id(&tag.name),
                "git_client.tag",
                tag.name.clone(),
            )
        })),
    );
    children.push(
        Resource::new("git_client.stashes", "git_client.collection", "Stashes").with_children(
            model
                .stashes
                .iter()
                .take(MAX_CONTEXT_STASHES)
                .map(stash_resource),
        ),
    );
    children.push(
        Resource::new(
            "git_client.remotes",
            "git_client.collection",
            t("git-section-remotes"),
        )
        .with_children(
            model
                .remotes
                .iter()
                .take(MAX_CONTEXT_REMOTES)
                .map(remote_resource),
        ),
    );
    children
}

pub(crate) fn repo_label(model: &RepoModel) -> String {
    let repo_name = model.name.as_str();
    if repo_name.trim().is_empty() {
        t("git-collection-repository")
    } else {
        repo_name.to_string()
    }
}

fn stash_resource(stash: &crate::model::Stash) -> Resource {
    Resource::new(
        format!("git_client.stash:{}", stash.index),
        "git_client.stash",
        stash.message.clone(),
    )
    .with_action(GitClientEffect::ApplyStash { index: stash.index }.bound())
}

/// Resolve one current repository item by id without building the whole
/// hierarchy, so search, history and agent addresses stay valid beyond the
/// published page while ids gone after a fetch or checkout resolve to `None`.
pub(crate) fn resolve_child(model: &RepoModel, id: &str) -> Option<Resource> {
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
        return Some(stash_resource(stash));
    }
    model
        .remotes
        .iter()
        .find(|remote| remote_resource_id(&remote.name) == id)
        .map(remote_resource)
}

/// Linked working directories are live device resources, not shared paths.
pub(crate) fn worktree_rows(model: &RepoModel) -> LiveRow {
    let mut collection = LiveRow::new(
        "git_client.worktrees",
        "git_client.worktrees",
        t("git-section-worktrees"),
    );
    if let Some(error) = &model.worktree_error {
        collection = collection.row(LiveRow::new(
            "git_client.worktrees:error",
            "git_client.error",
            crate::i18n::t_args(
                "git-worktree-read-error",
                &[("error", error.as_str().into())],
            ),
        ));
    }
    for worktree in &model.worktrees {
        collection = collection.row(LiveRow::new(
            format!("git_client.worktree:{}", stable_component(&worktree.path)),
            "git_client.worktree",
            format!("{} — {}", worktree.head, worktree.path),
        ));
    }
    collection
}

pub(crate) fn worktree_row(model: &RepoModel, id: &str) -> Option<LiveRow> {
    if id == "git_client.worktrees" {
        return Some(worktree_rows(model));
    }
    if id == "git_client.worktrees:error" {
        return model.worktree_error.as_ref().map(|error| {
            LiveRow::new(
                id,
                "git_client.error",
                crate::i18n::t_args(
                    "git-worktree-read-error",
                    &[("error", error.as_str().into())],
                ),
            )
        });
    }
    let worktree = model.worktrees.iter().find(|worktree| {
        id == format!("git_client.worktree:{}", stable_component(&worktree.path))
    })?;
    Some(LiveRow::new(
        id,
        "git_client.worktree",
        format!("{} — {}", worktree.head, worktree.path),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use tilo_actions::LocalTransport;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
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

    /// A base document with a repository opened in it.
    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        dispatch(
            &mut doc,
            &GitClientAction::OpenRepository {
                path: "/srv/repos/tilo".into(),
            },
        );
        doc.commit();
        doc
    }

    /// The repository door the framework publishes from `doc` once `model`'s
    /// projection is written into it.
    fn published(model: &RepoModel, doc: &AutoCommit) -> Vec<Resource> {
        let mut doc = doc.clone();
        SharedRepo::write(&mut doc, &SharedRepo::from_model(model));
        let state = crate::doc::GitClientState::from_doc(&doc);
        vec![ui::tilo_resource::publish::<crate::resource::GitClient>(
            "git_client.repo",
            &repo_label(&state.repo),
            &state,
        )]
    }

    fn repo_model() -> RepoModel {
        RepoModel {
            name: "tilo".into(),
            ..RepoModel::default()
        }
    }

    /// The repository resource tree: Ctrl+K publishes the repo; git objects carry confirmation contracts; contextual
    /// resources are bounded while omitted items still resolve. Each phase is a former test with its own fresh fixture.
    #[test]
    fn repository_resource_tree_exports_bounded_git_objects_and_contracts() {
        ctrl_k_publishes_the_repo();
        repository_resources_cover_git_objects_and_confirmation_contracts();
        contextual_resources_are_bounded_but_omitted_items_resolve();
    }

    /// Ctrl+K on an open repository publishes the repo resource. Drives the
    /// real resource builder.
    fn ctrl_k_publishes_the_repo() {
        let doc = seeded();
        let resources = published(&repo_model(), &doc);
        assert_eq!(resources.len(), 1, "one repository root");
        let repo = &resources[0];
        assert_eq!(repo.kind, "git_client.repo");
        assert_eq!(repo.label, "tilo");
        assert_eq!(repo.id.as_str(), "git_client.repo");
    }

    ui::action_contract_tests! {
        seed: seeded(),
        registry: registry(),
        resources: |doc| published(&crate::sample::model().0, doc),
        palette: ["Fetch remote", "Open commit"],
        replay: [
            open_repository => |_| encode(&GitClientAction::OpenRepository {
                path: "/srv/repos/other".into(),
            }).unwrap(),
        ],
        undo: [
            open_repository => |_| encode(&GitClientAction::OpenRepository {
                path: "/srv/repos/other".into(),
            }).unwrap(),
        ],
    }

    fn repository_resources_cover_git_objects_and_confirmation_contracts() {
        let (model, _) = crate::sample::model();
        let doc = seeded();
        let root = published(&model, &doc).remove(0);
        let kinds: std::collections::HashSet<_> = root
            .children
            .iter()
            .flat_map(|collection| collection.children.iter())
            .map(|resource| resource.kind.as_str())
            .collect();
        for expected in [
            "git_client.commit",
            "git_client.branch",
            "git_client.tag",
            "git_client.stash",
            "git_client.remote",
        ] {
            assert!(kinds.contains(expected), "missing {expected}: {kinds:?}");
        }
        let changes = working_tree_rows(&model).into_resource();
        assert!(
            changes.children.iter().all(|row| row
                .actions
                .iter()
                .all(|action| action.kind != tilo_actions::OperationKind::Document)),
            "working-tree rows only view and run Effects"
        );
        let discard = changes
            .children
            .iter()
            .find(|resource| {
                resource.kind == "git_client.diff"
                    && resource
                        .actions
                        .iter()
                        .any(|action| action.id == "discard-file")
            })
            .and_then(|resource| {
                resource
                    .actions
                    .iter()
                    .find(|action| action.id == "discard-file")
            });
        assert!(discard.is_some(), "a worktree change offers discard");
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
        let root = published(&model, &doc).remove(0);
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
            resolve_child(&model, &omitted)
                .expect("omitted commit resolves lazily")
                .id
                .as_str(),
            omitted
        );
    }
}
