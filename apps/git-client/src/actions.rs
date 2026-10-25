//! The git client's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! ## The split — almost everything here is external
//!
//! The Tilo action-model recipe (`docs/action-model-recipe.md`) sorts every
//! interaction into exactly one of two buckets: a *document-mutating* operation
//! becomes an [`Action`](tilo_actions::Action) (a serializable value that deterministically changes
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
//! The record ([`crate::resource::GitClient`]) holds the repository it
//! opens and what collaborators see of it ([`crate::shared`]): branches,
//! commits, remotes, tags, stashes and identity. Working-tree status is
//! never in it.
//!
//! Git work against the on-disk repository is the [`GitClientEffect`]
//! vocabulary, answered by the surface. What the window shows of the
//! repository — its history, branches, tags, stashes and remotes, and the
//! working tree — is published as the surface's live rows, each carrying the
//! effects that act on it. The diff preferences are the person's settings
//! ([`crate::settings`]), switched by the surface's effects.

use crate::i18n::t;

use ui::framework::LiveRow;
use ui::tilo_resource::{self, Effect};

use crate::model::{ChangeArea, FileChange, RepoModel};

/// The git client's work on the repository on disk and what it shows of one,
/// which change no document. Navigation and whole-tree work are offered on
/// the repository; everything else rides the changed file, commit, branch,
/// stash or remote it acts on, named by path, id or name and resolved again
/// when invoked.
#[derive(Debug, Clone, PartialEq, Eq, Effect)]
#[effect(operation = "git_client.effect")]
pub enum GitClientEffect {
    /// Open repository
    ///
    /// Open the git repository at a path on this device.
    OpenRepository {
        #[param("Path of the repository's folder on this device")]
        path: String,
    },
    /// Choose repository
    ///
    /// Pick the repository's folder with the system's file chooser, then
    /// open it.
    #[effect(attended, job)]
    ChooseRepository,
    /// Run approved project check
    #[effect(job, cancellable, returns, untrusted)]
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
    #[effect(kind = "read")]
    DiffLayout,
    /// Turn syntax highlighting on or off
    ///
    /// A preference of this person, which follows them to their other
    /// devices.
    #[effect(kind = "read")]
    SyntaxHighlight,
    /// Show or hide whitespace
    ///
    /// A preference of this person, which follows them to their other
    /// devices.
    #[effect(kind = "read")]
    ShowWhitespace,
    /// Wrap long lines or stop wrapping them
    ///
    /// A preference of this person, which follows them to their other
    /// devices.
    #[effect(kind = "read")]
    WrapLines,
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// Keep the palette's contextual tree cheap even for monorepos. Every omitted
/// item is still addressable by id (`repository_row`, `working_tree_row`).
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

fn commit_file_resource_id(commit: &str, path: &str) -> String {
    format!("git_client.commit-file:{commit}:{}", stable_component(path))
}

fn commit_resource(commit: &crate::model::Commit) -> LiveRow {
    commit.files.iter().take(16).fold(
        LiveRow::new(
            commit_resource_id(&commit.id),
            "git_client.commit",
            commit.summary.clone(),
        )
        .effect(&GitClientEffect::OpenCommit {
            id: commit.id.clone(),
        }),
        |row, file| {
            row.row(LiveRow::new(
                commit_file_resource_id(&commit.id, &file.path),
                "git_client.commit-file",
                file.path.clone(),
            ))
        },
    )
}

fn branch_resource(branch: &crate::model::Branch) -> LiveRow {
    let mut row = LiveRow::new(
        branch_resource_id(&branch.name),
        "git_client.branch",
        branch.name.clone(),
    );
    if !branch.is_head {
        row = row.effect(&GitClientEffect::CheckoutBranch {
            name: branch.name.clone(),
        });
    }
    if !branch.is_head && !branch.is_remote {
        row = row.effect(&GitClientEffect::DeleteBranch {
            name: branch.name.clone(),
        });
    }
    row
}

fn tag_resource(tag: &crate::model::Tag) -> LiveRow {
    LiveRow::new(
        tag_resource_id(&tag.name),
        "git_client.tag",
        tag.name.clone(),
    )
}

fn remote_resource(remote: &crate::model::Remote) -> LiveRow {
    let name = || remote.name.clone();
    LiveRow::new(
        remote_resource_id(&remote.name),
        "git_client.remote",
        remote.name.clone(),
    )
    .effect(&GitClientEffect::Fetch { remote: name() })
    .effect(&GitClientEffect::Pull { remote: name() })
    .effect(&GitClientEffect::Push { remote: name() })
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

/// What the window shows of the repository, as live rows: its history,
/// branches, tags, stashes and remotes, each a bounded page.
pub(crate) fn repository_rows(model: &RepoModel) -> Vec<LiveRow> {
    fn collection<T>(
        id: &str,
        label: String,
        items: &[T],
        bound: usize,
        row: impl Fn(&T) -> LiveRow,
    ) -> LiveRow {
        items.iter().take(bound).map(row).fold(
            LiveRow::new(id, "git_client.collection", label),
            LiveRow::row,
        )
    }
    vec![
        collection(
            "git_client.history",
            t("git-screen-history"),
            &model.commits,
            MAX_CONTEXT_COMMITS,
            commit_resource,
        ),
        collection(
            "git_client.branches",
            t("git-section-branches"),
            &model.branches,
            MAX_CONTEXT_REFS,
            branch_resource,
        ),
        collection(
            "git_client.tags",
            t("git-section-tags"),
            &model.tags,
            MAX_CONTEXT_REFS,
            tag_resource,
        ),
        collection(
            "git_client.stashes",
            t("git-section-stashes"),
            &model.stashes,
            MAX_CONTEXT_STASHES,
            stash_resource,
        ),
        collection(
            "git_client.remotes",
            t("git-section-remotes"),
            &model.remotes,
            MAX_CONTEXT_REMOTES,
            remote_resource,
        ),
    ]
}

pub(crate) fn repo_label(model: &RepoModel) -> String {
    let repo_name = model.name.as_str();
    if repo_name.trim().is_empty() {
        t("git-collection-repository")
    } else {
        repo_name.to_string()
    }
}

fn stash_resource(stash: &crate::model::Stash) -> LiveRow {
    LiveRow::new(
        format!("git_client.stash:{}", stash.index),
        "git_client.stash",
        stash.message.clone(),
    )
    .effect(&GitClientEffect::ApplyStash { index: stash.index })
}

/// Resolve one current repository item by id without building the whole
/// hierarchy, so search, history and agent addresses stay valid beyond the
/// published page while ids gone after a fetch or checkout resolve to `None`.
pub(crate) fn repository_row(model: &RepoModel, id: &str) -> Option<LiveRow> {
    if let Some(row) = repository_rows(model)
        .into_iter()
        .find(|collection| collection.id() == id)
    {
        return Some(row);
    }
    if let Some(commit) = model
        .commits
        .iter()
        .find(|commit| commit_resource_id(&commit.id) == id)
    {
        return Some(commit_resource(commit));
    }
    for commit in &model.commits {
        if let Some(file) = commit
            .files
            .iter()
            .find(|file| commit_file_resource_id(&commit.id, &file.path) == id)
        {
            return Some(LiveRow::new(
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
        return Some(tag_resource(tag));
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
    use tilo_actions::Resource;

    /// The rows the window publishes for `model`: its repository and its
    /// working tree.
    fn published(model: &RepoModel) -> Vec<Resource> {
        repository_rows(model)
            .into_iter()
            .chain([working_tree_rows(model)])
            .map(LiveRow::into_resource)
            .collect()
    }

    /// The repository resource tree: git objects carry confirmation
    /// contracts; contextual resources are bounded while omitted items still
    /// resolve. Each phase is a former test with its own fresh fixture.
    #[test]
    fn repository_resource_tree_exports_bounded_git_objects_and_contracts() {
        repository_resources_cover_git_objects_and_confirmation_contracts();
        contextual_resources_are_bounded_but_omitted_items_resolve();
    }

    fn repository_resources_cover_git_objects_and_confirmation_contracts() {
        let (model, _) = crate::sample::model();
        let rows = published(&model);
        let kinds: std::collections::HashSet<_> = rows
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
        assert!(
            rows.iter().all(|collection| collection
                .children
                .iter()
                .flat_map(|row| row.actions.iter())
                .all(|action| action.kind != tilo_actions::OperationKind::Document)),
            "repository rows only view and run Effects"
        );
        let changes = working_tree_rows(&model).into_resource();
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
        let rows = published(&model);
        let history = rows
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
            repository_row(&model, &omitted)
                .expect("omitted commit resolves lazily")
                .id(),
            omitted
        );
    }
}
