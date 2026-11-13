//! Live repository backend, built on `git2`. It reads an on-disk repository into
//! the render-friendly [`RepoModel`] (working-tree status, per-file diffs, history
//! with ref decorations, and the branch, tag, stash and remote panels), and this is
//! the only place that touches libgit2 — the rest of the app works off
//! [`RepoModel`], so the screenshot path can substitute the bundled sample. Reads
//! are best-effort: a missing piece (no upstream, an unreadable object) is skipped
//! rather than fatal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail};
use git2::{
    ApplyLocation, ApplyOptions, BranchType, Delta, DiffFormat, DiffOptions, IndexAddOption,
    ObjectType, Repository, ResetType, Sort, Status, StatusOptions,
};

use crate::diff::{language_for, parse_unified};
use crate::model::{
    Branch, ChangeArea, ChangeKind, Commit, DiffKey, DiffMap, FileChange, FileDiff, RefKind,
    RefLabel, Remote, RepoModel, Stash, Tag,
};

/// How many commits the history view walks, so a huge repo does not stall the first frame.
const HISTORY_LIMIT: usize = 400;

/// Open the repository discovered from `path` (walking up to find `.git`) and read it into a model
/// plus a map of per-file diffs keyed by path; `None` when no repository can be opened.
pub fn load(path: &Path) -> Option<(RepoModel, DiffMap)> {
    let repo = Repository::discover(path).ok()?;
    let mut model = RepoModel::default();
    let mut diffs: DiffMap = DiffMap::new();

    if let Some(wd) = repo.workdir() {
        model.workdir = wd.to_string_lossy().to_string();
        model.name = wd
            .file_name()
            .map_or_else(|| "repository".into(), |s| s.to_string_lossy().to_string());
    }

    read_identity(&repo, &mut model);
    read_head(&repo, &mut model);
    read_status(&repo, &mut model, &mut diffs);
    read_history(&repo, &mut model, &mut diffs);
    read_branches(&repo, &mut model);
    read_tags(&repo, &mut model);
    read_stashes(&repo, &mut model);
    read_remotes(&repo, &mut model);
    match list_worktrees(path) {
        Ok(worktrees) => model.worktrees = worktrees,
        Err(error) => model.worktree_error = Some(format!("{error:#}")),
    }

    let layout = crate::graph::layout(&mut model.commits);
    model.graph_width = layout.lanes;

    Some((model, diffs))
}

fn read_identity(repo: &Repository, model: &mut RepoModel) {
    if let Ok(cfg) = repo.config() {
        model.author_name = cfg.get_string("user.name").unwrap_or_default();
        model.author_email = cfg.get_string("user.email").unwrap_or_default();
    }
}

fn read_head(repo: &Repository, model: &mut RepoModel) {
    match repo.head() {
        Ok(head) => {
            if head.is_branch() {
                model.head = head.shorthand().unwrap_or("HEAD").to_string();
            } else {
                let short = head
                    .target()
                    .map_or_else(|| "HEAD".into(), |o| short_id(&o.to_string()));
                model.head = format!("detached @ {short}");
            }
        }
        Err(_) => model.head = "(no commits yet)".into(),
    }
}

fn classify(status: Status) -> Option<(ChangeArea, ChangeKind)> {
    if status.contains(Status::CONFLICTED) {
        return Some((ChangeArea::Unstaged, ChangeKind::Conflicted));
    }
    if status.contains(Status::INDEX_NEW) {
        return Some((ChangeArea::Staged, ChangeKind::Added));
    }
    if status.contains(Status::INDEX_MODIFIED) {
        return Some((ChangeArea::Staged, ChangeKind::Modified));
    }
    if status.contains(Status::INDEX_DELETED) {
        return Some((ChangeArea::Staged, ChangeKind::Deleted));
    }
    if status.contains(Status::INDEX_RENAMED) {
        return Some((ChangeArea::Staged, ChangeKind::Renamed));
    }
    None
}

fn classify_worktree(status: Status) -> Option<(ChangeArea, ChangeKind)> {
    if status.contains(Status::WT_NEW) {
        return Some((ChangeArea::Untracked, ChangeKind::Untracked));
    }
    if status.contains(Status::WT_MODIFIED) {
        return Some((ChangeArea::Unstaged, ChangeKind::Modified));
    }
    if status.contains(Status::WT_DELETED) {
        return Some((ChangeArea::Unstaged, ChangeKind::Deleted));
    }
    if status.contains(Status::WT_RENAMED) {
        return Some((ChangeArea::Unstaged, ChangeKind::Renamed));
    }
    None
}

fn read_status(repo: &Repository, model: &mut RepoModel, diffs: &mut DiffMap) {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);
    let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
        return;
    };

    for entry in statuses.iter() {
        let st = entry.status();
        let path = entry.path().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }

        if let Some((area, kind)) = classify(st) {
            model.staged.push(FileChange {
                path: path.clone(),
                old_path: None,
                area,
                kind,
                additions: 0,
                deletions: 0,
            });
        }
        if let Some((area, kind)) = classify_worktree(st) {
            let fc = FileChange {
                path: path.clone(),
                old_path: None,
                area,
                kind,
                additions: 0,
                deletions: 0,
            };
            match area {
                ChangeArea::Untracked => model.untracked.push(fc),
                _ => model.unstaged.push(fc),
            }
        }
    }

    collect_worktree_diffs(repo, model, diffs);
    collect_index_diffs(repo, model, diffs);
}

fn collect_worktree_diffs(repo: &Repository, model: &mut RepoModel, diffs: &mut DiffMap) {
    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .context_lines(3);
    let Ok(diff) = repo.diff_index_to_workdir(None, Some(&mut opts)) else {
        return;
    };
    fold_diff(&diff, model, diffs, false);
}

fn collect_index_diffs(repo: &Repository, model: &mut RepoModel, diffs: &mut DiffMap) {
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    let Ok(diff) = repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts)) else {
        return;
    };
    fold_diff(&diff, model, diffs, true);
}

fn fold_diff(diff: &git2::Diff, model: &mut RepoModel, diffs: &mut DiffMap, staged: bool) {
    let mut raw_by_path: HashMap<String, String> = HashMap::new();
    let _ = diff.print(DiffFormat::Patch, |delta, _hunk, line| {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if path.is_empty() {
            return true;
        }
        let buf = raw_by_path.entry(path).or_default();
        let origin = line.origin();
        if matches!(origin, '+' | '-' | ' ') {
            buf.push(origin);
        }
        buf.push_str(std::str::from_utf8(line.content()).unwrap_or(""));
        true
    });

    for (path, raw) in raw_by_path {
        let mut fd = parse_unified(&path, &language_for(&path), &raw);
        if let Some(delta) = find_delta(diff, &path) {
            match delta {
                Delta::Added | Delta::Untracked => fd.is_new = true,
                Delta::Deleted => fd.is_deleted = true,
                _ => {}
            }
        }
        let (adds, dels) = (fd.additions(), fd.deletions());
        // Fold counts onto the matching side only, so a file present on both sides keeps each
        // side's own counts.
        let fold_into = |list: &mut Vec<FileChange>| -> bool {
            if let Some(fc) = list.iter_mut().find(|f| f.path == path) {
                fc.additions = adds;
                fc.deletions = dels;
                true
            } else {
                false
            }
        };
        if staged {
            fold_into(&mut model.staged);
        } else if !fold_into(&mut model.unstaged) {
            fold_into(&mut model.untracked);
        }
        // Store the diff under its side-specific key, so the staged and worktree diffs of the same
        // path never collide.
        diffs.insert(
            DiffKey {
                staged,
                path: path.clone(),
            },
            fd,
        );
    }
}

fn find_delta(diff: &git2::Diff, path: &str) -> Option<Delta> {
    for d in diff.deltas() {
        let p = d
            .new_file()
            .path()
            .or_else(|| d.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if p == path {
            return Some(d.status());
        }
    }
    None
}

fn read_history(repo: &Repository, model: &mut RepoModel, _diffs: &mut DiffMap) {
    let ref_map = ref_decorations(repo);

    let Ok(mut walk) = repo.revwalk() else { return };
    let _ = walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME);
    if walk.push_head().is_err() {
        // No HEAD (empty repo): also try every local branch tip.
        if let Ok(branches) = repo.branches(Some(BranchType::Local)) {
            for b in branches.flatten() {
                if let Some(oid) = b.0.get().target() {
                    let _ = walk.push(oid);
                }
            }
        }
    }
    // Include side branches, so the graph shows lanes other than HEAD's.
    if let Ok(branches) = repo.branches(None) {
        for b in branches.flatten() {
            if let Some(oid) = b.0.get().target() {
                let _ = walk.push(oid);
            }
        }
    }

    for (i, oid) in walk.flatten().enumerate() {
        if i >= HISTORY_LIMIT {
            break;
        }
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let id = oid.to_string();
        let author = commit.author();
        let summary = commit.summary().ok().flatten().unwrap_or("").to_string();
        let full = commit.message().unwrap_or("");
        let body = full
            .split_once('\n')
            .map(|x| x.1)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let parents: Vec<String> = commit.parent_ids().map(|p| p.to_string()).collect();
        let time = commit.time().seconds();

        model.commits.push(Commit {
            id: id.clone(),
            summary,
            body,
            author_name: author.name().unwrap_or("?").to_string(),
            author_email: author.email().unwrap_or("").to_string(),
            when: relative_time(time),
            time,
            parents,
            refs: ref_map.get(&id).cloned().unwrap_or_default(),
            lane: 0,
            files: vec![],
        });
    }
}

fn ref_decorations(repo: &Repository) -> HashMap<String, Vec<RefLabel>> {
    let mut map: HashMap<String, Vec<RefLabel>> = HashMap::new();

    if let Ok(head) = repo.head()
        && let Some(oid) = head.target()
    {
        map.entry(oid.to_string()).or_default().push(RefLabel {
            name: "HEAD".into(),
            kind: RefKind::Head,
        });
    }

    if let Ok(refs) = repo.references() {
        for r in refs.flatten() {
            let Some(oid) = r.target() else { continue };
            let name = r.shorthand().unwrap_or("").to_string();
            if name.is_empty() || name == "HEAD" {
                continue;
            }
            let kind = if r.is_tag() {
                RefKind::Tag
            } else if r.is_remote() {
                RefKind::RemoteBranch
            } else {
                RefKind::LocalBranch
            };
            map.entry(oid.to_string())
                .or_default()
                .push(RefLabel { name, kind });
        }
    }
    map
}

fn read_branches(repo: &Repository, model: &mut RepoModel) {
    let head_name = model.head.clone();
    let Ok(branches) = repo.branches(None) else {
        return;
    };
    for (branch, btype) in branches.flatten() {
        let Some(name) = branch.name().ok().flatten() else {
            continue;
        };
        let is_remote = btype == BranchType::Remote;
        let tip = branch
            .get()
            .target()
            .map(|o| short_id(&o.to_string()))
            .unwrap_or_default();
        let upstream = branch.upstream().ok().and_then(|u| {
            u.name()
                .ok()
                .flatten()
                .map(std::string::ToString::to_string)
        });
        let (ahead, behind) = upstream_delta(repo, &branch);
        model.branches.push(Branch {
            name: name.to_string(),
            is_head: !is_remote && name == head_name,
            is_remote,
            upstream,
            ahead,
            behind,
            tip,
        });
    }
}

fn upstream_delta(repo: &Repository, branch: &git2::Branch) -> (u32, u32) {
    let local = branch.get().target();
    let upstream = branch.upstream().ok().and_then(|u| u.get().target());
    if let (Some(l), Some(u)) = (local, upstream)
        && let Ok((ahead, behind)) = repo.graph_ahead_behind(l, u)
    {
        return (ahead as u32, behind as u32);
    }
    (0, 0)
}

fn read_tags(repo: &Repository, model: &mut RepoModel) {
    let Ok(names) = repo.tag_names(None) else {
        return;
    };
    for name in names.iter().filter_map(|name| name.ok().flatten()) {
        let full = format!("refs/tags/{name}");
        let target = repo
            .revparse_single(&full)
            .ok()
            .map(|o| short_id(&o.id().to_string()))
            .unwrap_or_default();
        let message = repo
            .revparse_single(&full)
            .ok()
            .and_then(|o| o.into_tag().ok())
            .and_then(|t| t.message().ok().flatten().map(|m| m.trim().to_string()));
        model.tags.push(Tag {
            name: name.to_string(),
            target,
            message,
        });
    }
}

fn read_stashes(repo: &Repository, model: &mut RepoModel) {
    // `stash_foreach` needs a mutable repo; re-open to avoid threading mutability through the read
    // pipeline.
    if let Ok(mut owned) = Repository::open(repo.path()) {
        let mut out: Vec<Stash> = Vec::new();
        let _ = owned.stash_foreach(|index, message, _oid| {
            let branch = message
                .strip_prefix("WIP on ")
                .and_then(|s| s.split(':').next())
                .unwrap_or("")
                .to_string();
            out.push(Stash {
                index,
                message: message.to_string(),
                branch,
            });
            true
        });
        model.stashes = out;
    }
}

fn read_remotes(repo: &Repository, model: &mut RepoModel) {
    let Ok(names) = repo.remotes() else { return };
    for name in names.iter().filter_map(|name| name.ok().flatten()) {
        if let Ok(remote) = repo.find_remote(name) {
            model.remotes.push(Remote {
                name: name.to_string(),
                url: remote.url().unwrap_or("").to_string(),
            });
        }
    }
}

fn short_id(full: &str) -> String {
    full.chars().take(7).collect()
}

fn relative_time(seconds: i64) -> String {
    ui::format::relative(ui::clock::local_now().timestamp().saturating_sub(seconds))
}

// ----------------------------------- write actions ----------------------------------- These
// mutate the on-disk repository: each opens the repository fresh, performs one git operation and
// returns, and the app re-runs [`load`] afterwards, so the model reflects the repository rather
// than a hand-patched copy.

fn open(path: &Path) -> anyhow::Result<Repository> {
    Repository::discover(path)
        .with_context(|| format!("no git repository at or above {}", path.display()))
}

pub fn is_repo(path: &Path) -> bool {
    Repository::discover(path).is_ok()
}

/// Stage a single path, or its deletion when it is gone from the working tree (`git add <path>`).
pub fn stage_file(path: &Path, file: &str) -> anyhow::Result<()> {
    let repo = open(path)?;
    let mut index = repo.index()?;
    let rel = Path::new(file);
    // A dangling symlink is still a Git entry; checking its target would mistake it for a deletion.
    let wt_exists = repo
        .workdir()
        .is_some_and(|w| w.join(rel).symlink_metadata().is_ok());
    if wt_exists {
        index
            .add_path(rel)
            .with_context(|| format!("git add {file}"))?;
    } else {
        index
            .remove_path(rel)
            .with_context(|| format!("git add (deletion) {file}"))?;
    }
    index.write()?;
    Ok(())
}

/// Unstage a single path back to its HEAD state in the index, leaving the working tree untouched
/// (`git reset HEAD -- <path>`).
pub fn unstage_file(path: &Path, file: &str) -> anyhow::Result<()> {
    let repo = open(path)?;
    let rel = Path::new(file);
    if let Some(head_commit) = repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
        repo.reset_default(Some(head_commit.as_object()), [rel])
            .with_context(|| format!("git reset HEAD -- {file}"))?;
    } else {
        let mut index = repo.index()?;
        let _ = index.remove_path(rel);
        index.write()?;
    }
    Ok(())
}

/// Stage every change: tracked modifications, deletions and untracked files (`git add -A`).
pub fn stage_all(path: &Path) -> anyhow::Result<()> {
    let repo = open(path)?;
    let mut index = repo.index()?;
    index.add_all(["*"], IndexAddOption::DEFAULT, None)?;
    // `add_all` does not record deletions of tracked files; `update_all` does.
    index.update_all(["*"], None)?;
    index.write()?;
    Ok(())
}

/// Unstage every staged change back to HEAD (`git reset HEAD`).
pub fn unstage_all(path: &Path) -> anyhow::Result<()> {
    let repo = open(path)?;
    if let Some(head_commit) = repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
        repo.reset(head_commit.as_object(), ResetType::Mixed, None)
            .context("git reset HEAD")?;
    } else {
        let mut index = repo.index()?;
        index.clear()?;
        index.write()?;
    }
    Ok(())
}

/// Stage or unstage a single hunk (`git add -p` / `git restore --staged -p`). `hunk_index` is the
/// 0-based position within the file's diff in the relevant direction, the same ordering the UI
/// assigns to its hunk buttons.
pub fn apply_hunk(path: &Path, file: &str, hunk_index: usize, unstage: bool) -> anyhow::Result<()> {
    use std::cell::Cell;
    let repo = open(path)?;
    let rel = Path::new(file);

    // An untracked file has no index base to patch against, and a brand-new file is one whole-file
    // addition anyway, so staging any of its hunks stages the file.
    if !unstage {
        let st = repo.status_file(rel).unwrap_or_else(|_| Status::empty());
        if st.contains(Status::WT_NEW) && !st.contains(Status::INDEX_NEW) {
            let mut index = repo.index()?;
            index
                .add_path(rel)
                .with_context(|| format!("git add {file}"))?;
            index.write()?;
            return Ok(());
        }
    }

    let mut opts = DiffOptions::new();
    opts.pathspec(file).context_lines(3);

    let (diff, location, reverse) = if unstage {
        // Index against HEAD; reverse the hunk so applying it to the index undoes that hunk's
        // staged change only.
        let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        opts.reverse(true);
        let diff = repo
            .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))
            .context("diff HEAD..index for unstage")?;
        (diff, ApplyLocation::Index, true)
    } else {
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let diff = repo
            .diff_index_to_workdir(None, Some(&mut opts))
            .context("diff index..workdir for stage")?;
        (diff, ApplyLocation::Index, false)
    };

    // Count this file's hunks in delta order to match the UI index, then apply only the selected
    // one. `Cell`s let the callbacks share borrows, so the outcome is readable after `apply`
    // returns.
    let seen = Cell::new(0usize);
    let applied = Cell::new(false);
    let mut apply_opts = ApplyOptions::new();
    apply_opts.hunk_callback(|hunk| {
        // git2 invokes this per hunk of every already-filtered delta, in file order; only the
        // target hunk is let through.
        let n = seen.get();
        let is_target = n == hunk_index;
        if is_target {
            applied.set(true);
        }
        seen.set(n + 1);
        let _ = hunk;
        is_target
    });
    // Restrict to our file, so hunk ordering matches the single-file diff.
    apply_opts.delta_callback(move |delta| {
        delta.is_some_and(|d| d.new_file().path() == Some(rel) || d.old_file().path() == Some(rel))
    });

    repo.apply(&diff, location, Some(&mut apply_opts))
        .with_context(|| {
            format!(
                "git apply {} hunk {hunk_index} of {file}",
                if reverse { "(reverse)" } else { "" }
            )
        })?;

    if !applied.get() {
        bail!("hunk {hunk_index} not found for {file}");
    }
    Ok(())
}

/// Discard a file's worktree changes: a tracked file is restored from the index (`git restore
/// <file>`), dropping unstaged edits while staged ones stay, and an untracked file is deleted from
/// disk. Destructive; the caller confirms intent.
///
/// # Errors
///
/// When the repository cannot be opened or the file cannot be discarded.
pub fn discard_file(path: &Path, file: &str) -> anyhow::Result<()> {
    let repo = open(path)?;
    let rel = Path::new(file);
    let st = repo.status_file(rel).unwrap_or_else(|_| Status::empty());
    if st.contains(Status::WT_NEW) && !st.contains(Status::INDEX_NEW) {
        if let Some(wd) = repo.workdir() {
            let full = wd.join(rel);
            if full.symlink_metadata().is_ok() {
                std::fs::remove_file(&full).with_context(|| format!("remove untracked {file}"))?;
            }
        }
        return Ok(());
    }
    let mut co = git2::build::CheckoutBuilder::new();
    co.path(rel).force();
    repo.checkout_index(None, Some(&mut co))
        .with_context(|| format!("git restore {file}"))?;
    Ok(())
}

/// Discard a single hunk of a file's worktree changes (`git checkout -p`, discard direction),
/// `hunk_index` counting in the file's worktree diff. Destructive.
pub fn discard_hunk(path: &Path, file: &str, hunk_index: usize) -> anyhow::Result<()> {
    use std::cell::Cell;
    let repo = open(path)?;
    let rel = Path::new(file);

    // Reverse the index→workdir diff so applying a hunk to the working tree undoes that hunk's
    // edit; hunks keep top-to-bottom order, matching the diff the UI shows.
    let mut opts = DiffOptions::new();
    opts.pathspec(file).context_lines(3).reverse(true);
    let diff = repo
        .diff_index_to_workdir(None, Some(&mut opts))
        .context("diff index..workdir for discard")?;
    let seen = Cell::new(0usize);
    let applied = Cell::new(false);
    let mut apply_opts = ApplyOptions::new();
    apply_opts.hunk_callback(|_hunk| {
        let n = seen.get();
        let is_target = n == hunk_index;
        if is_target {
            applied.set(true);
        }
        seen.set(n + 1);
        is_target
    });
    apply_opts.delta_callback(move |delta| {
        delta.is_some_and(|d| d.new_file().path() == Some(rel) || d.old_file().path() == Some(rel))
    });

    repo.apply(&diff, ApplyLocation::WorkDir, Some(&mut apply_opts))
        .with_context(|| format!("git discard hunk {hunk_index} of {file}"))?;
    if !applied.get() {
        bail!("hunk {hunk_index} not found for {file}");
    }
    Ok(())
}

/// Commit the current index with `message`, parented on HEAD or as the root commit, with the author
/// from the repository's signature (a missing identity is an error, as in `git commit`). Returns
/// the short id.
pub fn commit(path: &Path, message: &str) -> anyhow::Result<String> {
    if message.trim().is_empty() {
        bail!("empty commit message");
    }
    let repo = open(path)?;
    let sig = repo
        .signature()
        .context("no committer identity — set user.name and user.email")?;

    let mut index = repo.index()?;
    let tree_oid = index.write_tree().context("write index tree")?;
    let tree = repo.find_tree(tree_oid)?;

    let parents: Vec<git2::Commit> = match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
        Some(c) => vec![c],
        None => vec![],
    };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)
        .context("create commit")?;
    Ok(short_id(&oid.to_string()))
}

/// Check out an existing local branch and point HEAD at it, updating the tree and index (safe
/// checkout: it fails on conflicting local changes).
pub fn checkout_branch(path: &Path, name: &str) -> anyhow::Result<()> {
    let repo = open(path)?;
    let (object, reference) = repo
        .revparse_ext(name)
        .with_context(|| format!("no such branch {name}"))?;
    let mut co = git2::build::CheckoutBuilder::new();
    co.safe();
    repo.checkout_tree(&object, Some(&mut co))
        .with_context(|| format!("checkout tree for {name}"))?;
    match reference {
        Some(r) => {
            let refname = r.name().context("branch ref name is not valid UTF-8")?;
            repo.set_head(refname)?;
        }
        None => {
            repo.set_head_detached(object.id())?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckoutPlan {
    Carry,
    Stash,
}

/// Switch branches, deciding what becomes of uncommitted work before the tree is touched.
/// [`CheckoutPlan::Stash`] parks the changes and reports the stash's short id; if the checkout then
/// fails they are put back.
pub fn checkout_branch_planned(
    path: &Path,
    name: &str,
    plan: CheckoutPlan,
) -> anyhow::Result<Option<String>> {
    match plan {
        CheckoutPlan::Carry => {
            checkout_branch(path, name)?;
            Ok(None)
        }
        CheckoutPlan::Stash => {
            let stashed = stash_save(path, &format!("before switching to {name}"))?;
            if let Err(error) = checkout_branch(path, name) {
                // Put the work back where it was: a switch that failed must not also have hidden
                // the edits.
                let _ = stash_apply(path, 0);
                return Err(error.context("the uncommitted changes were put back"));
            }
            Ok(Some(stashed))
        }
    }
}

/// Create a local branch at HEAD or at `start_point`, and switch to it when `checkout` is set.
pub fn create_branch(
    path: &Path,
    name: &str,
    start_point: Option<&str>,
    checkout: bool,
) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        bail!("empty branch name");
    }
    let repo = open(path)?;
    let target = match start_point {
        Some(sp) => repo
            .revparse_single(sp)
            .with_context(|| format!("bad start point {sp}"))?
            .peel_to_commit()?,
        None => repo
            .head()
            .context("no HEAD to branch from")?
            .peel_to_commit()?,
    };
    repo.branch(name, &target, false)
        .with_context(|| format!("create branch {name}"))?;
    if checkout {
        checkout_branch(path, name)?;
    }
    Ok(())
}

/// Delete a local branch, refusing the checked-out one; the ref is deleted even when its commits
/// are unmerged, since the GUI confirmed intent.
pub fn delete_branch(path: &Path, name: &str) -> anyhow::Result<()> {
    let repo = open(path)?;
    if let Ok(head) = repo.head()
        && head.shorthand().is_ok_and(|branch| branch == name)
    {
        bail!("cannot delete the checked-out branch {name}");
    }
    let mut branch = repo
        .find_branch(name, BranchType::Local)
        .with_context(|| format!("no such local branch {name}"))?;
    branch
        .delete()
        .with_context(|| format!("delete branch {name}"))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictSide {
    Ours,
    Theirs,
}

/// What each side of a conflicted file brings, so the choice can name what it costs before it is
/// made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConflictCost {
    pub ours: usize,
    pub theirs: usize,
}

impl ConflictCost {
    #[must_use]
    pub const fn discards(self, side: ConflictSide) -> usize {
        match side {
            ConflictSide::Ours => self.theirs,
            ConflictSide::Theirs => self.ours,
        }
    }
}

/// What keeping each side of `file` would discard: the lines of the side that loses.
pub fn conflict_cost(path: &Path, file: &str) -> anyhow::Result<ConflictCost> {
    let repo = open(path)?;
    let index = repo.index()?;
    let blob_of = |stage: i32| -> anyhow::Result<Vec<u8>> {
        let entry = index.get_path(Path::new(file), stage).ok_or_else(|| {
            anyhow!(
                "{file} has no {} side — it is not a conflict in both directions",
                if stage == 2 { "our" } else { "their" }
            )
        })?;
        Ok(repo.find_blob(entry.id)?.content().to_vec())
    };
    let ours = blob_of(2)?;
    let theirs = blob_of(3)?;
    cost_between(&repo, file, &ours, &theirs)
}

/// The lines each of two versions has that the other does not, as git's own diff counts them.
fn cost_between(
    repo: &Repository,
    file: &str,
    ours: &[u8],
    theirs: &[u8],
) -> anyhow::Result<ConflictCost> {
    let old = repo.find_blob(repo.blob(ours)?)?;
    let new = repo.find_blob(repo.blob(theirs)?)?;
    let mut opts = DiffOptions::new();
    opts.context_lines(0);
    // A line the diff adds is one only their side has; a line it removes is one only ours has.
    let mut cost = ConflictCost::default();
    let mut count = |_delta: git2::DiffDelta<'_>,
                     _hunk: Option<git2::DiffHunk<'_>>,
                     line: git2::DiffLine<'_>| {
        match line.origin() {
            '+' => cost.theirs = cost.theirs.saturating_add(1),
            '-' => cost.ours = cost.ours.saturating_add(1),
            _ => {}
        }
        true
    };
    repo.diff_blobs(
        Some(&old),
        Some(file),
        Some(&new),
        Some(file),
        Some(&mut opts),
        None,
        None,
        None,
        Some(&mut count),
    )?;
    Ok(cost)
}

/// Resolve a conflicted file by keeping one side: the chosen stage is written to the working tree
/// and staged, which clears the conflict. The other side is discarded, and `MERGE_HEAD` keeps the
/// history that produced both.
pub fn resolve_conflict(path: &Path, file: &str, side: ConflictSide) -> anyhow::Result<()> {
    let repo = open(path)?;
    let mut index = repo.index()?;
    let stage = match side {
        ConflictSide::Ours => 2,
        ConflictSide::Theirs => 3,
    };
    let entry = index.get_path(Path::new(file), stage).ok_or_else(|| {
        anyhow!(
            "{file} has no {} side to keep",
            match side {
                ConflictSide::Ours => "our",
                ConflictSide::Theirs => "their",
            }
        )
    })?;
    let blob = repo.find_blob(entry.id)?;
    let workdir = repo.workdir().context("bare repository")?;
    let full = workdir.join(file);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&full, blob.content()).with_context(|| format!("write {file}"))?;
    // Adding the path stages the resolved content and drops the conflict's stage entries, which is
    // what "resolved" means to git.
    index
        .add_path(Path::new(file))
        .with_context(|| format!("stage the resolved {file}"))?;
    index.write()?;
    Ok(())
}

/// The files `commit` changed against its first parent (the empty tree for a root commit), with
/// their line counts.
pub fn commit_files(path: &Path, commit: &str) -> anyhow::Result<Vec<FileChange>> {
    let repo = open(path)?;
    let commit = repo
        .revparse_single(commit)
        .with_context(|| format!("no such commit {commit}"))?
        .peel_to_commit()?;
    let new_tree = commit.tree()?;
    let parent_tree = match commit.parent(0) {
        Ok(parent) => Some(parent.tree()?),
        Err(_) => None,
    };
    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&new_tree), None)
        .context("diff the commit against its parent")?;
    Ok(files_of(&diff, &repo))
}

pub fn commit_file_diff(path: &Path, commit: &str, file: &str) -> anyhow::Result<FileDiff> {
    let repo = open(path)?;
    let commit = repo
        .revparse_single(commit)
        .with_context(|| format!("no such commit {commit}"))?
        .peel_to_commit()?;
    let new_tree = commit.tree()?;
    let parent_tree = match commit.parent(0) {
        Ok(parent) => Some(parent.tree()?),
        Err(_) => None,
    };
    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    opts.pathspec(file);
    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&new_tree), Some(&mut opts))
        .context("diff the commit's file against its parent")?;
    let mut fd;
    let mut raw = String::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        let origin = line.origin();
        if matches!(origin, '+' | '-' | ' ') {
            raw.push(origin);
        }
        raw.push_str(std::str::from_utf8(line.content()).unwrap_or(""));
        true
    })
    .context("read the commit's diff")?;
    fd = parse_unified(file, &language_for(file), &raw);
    if let Some(delta) = diff.deltas().next() {
        match delta.status() {
            Delta::Added | Delta::Untracked => fd.is_new = true,
            Delta::Deleted => fd.is_deleted = true,
            _ => {}
        }
    }
    Ok(fd)
}

fn files_of(diff: &git2::Diff, repo: &Repository) -> Vec<FileChange> {
    let mut files = Vec::new();
    for (index, delta) in diff.deltas().enumerate() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        let kind = match delta.status() {
            Delta::Added | Delta::Untracked => ChangeKind::Added,
            Delta::Deleted => ChangeKind::Deleted,
            Delta::Renamed => ChangeKind::Renamed,
            _ => ChangeKind::Modified,
        };
        let (additions, deletions) = git2::Patch::from_diff(diff, index)
            .ok()
            .flatten()
            .and_then(|patch| patch.line_stats().ok())
            .map_or((0, 0), |(_, added, removed)| (added, removed));
        let old_path = delta
            .old_file()
            .path()
            .map(|p| p.to_string_lossy().to_string())
            .filter(|old| *old != path);
        files.push(FileChange {
            path,
            old_path,
            area: ChangeArea::Staged,
            kind,
            additions: u32::try_from(additions).unwrap_or(u32::MAX),
            deletions: u32::try_from(deletions).unwrap_or(u32::MAX),
        });
    }
    let _ = repo;
    files
}

/// Fetch refs from a remote (default `origin`, else the first configured), best-effort through the
/// SSH agent and default credential helpers. Returns the remote name fetched.
pub fn fetch(path: &Path, remote_name: Option<&str>) -> anyhow::Result<String> {
    let repo = open(path)?;
    let name = resolve_remote(&repo, remote_name)?;
    let mut remote = repo.find_remote(&name)?;
    let mut cb = git2::RemoteCallbacks::new();
    cb.credentials(credentials(repo.config()?));
    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(cb);
    let refspecs: Vec<String> = remote
        .fetch_refspecs()?
        .iter()
        .filter_map(|refspec| refspec.ok().flatten())
        .map(std::string::ToString::to_string)
        .collect();
    crate::transport::with_context(repo.config()?, || {
        remote.fetch(&refspecs, Some(&mut fo), None)
    })
    .with_context(|| format!("git fetch {name}"))?;
    Ok(name)
}

/// Fetch, then fast-forward the current branch to its upstream when possible; a divergence is
/// reported rather than silently merged (`git pull --ff-only`).
pub fn pull(path: &Path, remote_name: Option<&str>) -> anyhow::Result<()> {
    let name = fetch(path, remote_name)?;
    let repo = open(path)?;
    let head = repo.head().context("no HEAD to pull into")?;
    if !head.is_branch() {
        bail!("not on a branch — cannot pull");
    }
    let branch_shorthand = head.shorthand().unwrap_or_default().to_string();
    let local_branch = repo.find_branch(&branch_shorthand, BranchType::Local)?;
    let upstream = local_branch
        .upstream()
        .with_context(|| format!("branch {branch_shorthand} has no upstream"))?;
    let upstream_oid = upstream
        .get()
        .target()
        .ok_or_else(|| anyhow!("upstream has no target"))?;
    let upstream_commit = repo.find_annotated_commit(upstream_oid)?;

    let (analysis, _) = repo.merge_analysis(&[&upstream_commit])?;
    if analysis.is_up_to_date() {
        return Ok(());
    }
    if analysis.is_fast_forward() {
        let refname = format!("refs/heads/{branch_shorthand}");
        let mut reference = repo.find_reference(&refname)?;
        // Check out against the old HEAD before advancing it: a conflict must leave both the edits
        // and the branch tip untouched.
        let target = repo.find_object(upstream_oid, None)?;
        repo.checkout_tree(&target, Some(git2::build::CheckoutBuilder::new().safe()))
            .context("cannot pull over local changes")?;
        reference.set_target(upstream_oid, "pull: fast-forward")?;
        let _ = name;
        Ok(())
    } else {
        bail!("non-fast-forward: {branch_shorthand} has diverged from its upstream")
    }
}

/// Push the current branch to a like-named branch on the resolved remote.
pub fn push(path: &Path, remote_name: Option<&str>) -> anyhow::Result<()> {
    let repo = open(path)?;
    let head = repo.head().context("no HEAD to push")?;
    if !head.is_branch() {
        bail!("not on a branch — cannot push");
    }
    let branch = head.shorthand().unwrap_or_default().to_string();
    let name = resolve_remote(&repo, remote_name)?;
    let mut remote = repo.find_remote(&name)?;
    let mut cb = git2::RemoteCallbacks::new();
    cb.credentials(credentials(repo.config()?));
    cb.push_update_reference(|reference, status| {
        if let Some(reason) = status {
            return Err(git2::Error::from_str(&format!(
                "remote rejected {reference}: {reason}"
            )));
        }
        Ok(())
    });
    let mut po = git2::PushOptions::new();
    po.remote_callbacks(cb);
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    crate::transport::with_context(repo.config()?, || remote.push(&[&refspec], Some(&mut po)))
        .with_context(|| format!("git push {name} {branch}"))?;
    Ok(())
}

/// Apply a stash entry without dropping it (`git stash apply stash@{index}`).
pub fn stash_apply(path: &Path, index: usize) -> anyhow::Result<()> {
    let mut repo = open(path)?;
    repo.stash_apply(index, None)
        .with_context(|| format!("git stash apply stash@{{{index}}}"))?;
    Ok(())
}

/// Push the working-tree and index changes onto the stash (`git stash push`); returns the new
/// stash's short id.
pub fn stash_save(path: &Path, message: &str) -> anyhow::Result<String> {
    let mut repo = open(path)?;
    let sig = repo
        .signature()
        .context("no identity — set user.name and user.email")?;
    let msg = if message.trim().is_empty() {
        None
    } else {
        Some(message)
    };
    let oid = repo
        .stash_save(&sig, msg.unwrap_or("WIP"), None)
        .context("git stash push")?;
    Ok(short_id(&oid.to_string()))
}

/// The caller's remote, else `origin`, else the first configured remote, else an error.
fn resolve_remote(repo: &Repository, want: Option<&str>) -> anyhow::Result<String> {
    let names = repo.remotes()?;
    let all: Vec<String> = names
        .iter()
        .filter_map(|name| name.ok().flatten())
        .map(std::string::ToString::to_string)
        .collect();
    if let Some(w) = want {
        if all.iter().any(|n| n == w) {
            return Ok(w.to_string());
        }
        bail!("no remote named {w}");
    }
    if all.iter().any(|n| n == "origin") {
        return Ok("origin".to_string());
    }
    all.into_iter()
        .next()
        .ok_or_else(|| anyhow!("no remotes configured"))
}

/// Try each supported credential source once per connection. A rejected
/// password or agent key must not cause an endless authentication loop.
fn credentials(
    config: git2::Config,
) -> impl FnMut(&str, Option<&str>, git2::CredentialType) -> Result<git2::Cred, git2::Error> {
    let mut helper_tried = false;
    let mut agent_tried = false;
    let mut default_tried = false;
    let mut username_tried = false;
    move |url, username, allowed| {
        if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) && !helper_tried {
            helper_tried = true;
            if let Ok(credential) = git2::Cred::credential_helper(&config, url, username) {
                return Ok(credential);
            }
        }
        if allowed.contains(git2::CredentialType::SSH_KEY) && !agent_tried {
            agent_tried = true;
            if let Some(user) = username {
                return git2::Cred::ssh_key_from_agent(user);
            }
        }
        if allowed.contains(git2::CredentialType::USERNAME) && !username_tried {
            username_tried = true;
            let configured = config.get_string("credential.username").ok();
            if let Some(user) = username.or(configured.as_deref()) {
                return git2::Cred::username(user);
            }
        }
        if allowed.contains(git2::CredentialType::DEFAULT) && !default_tried {
            default_tried = true;
            return git2::Cred::default();
        }
        Err(git2::Error::from_str(
            "authentication failed: configure a Git credential helper or SSH agent",
        ))
    }
}

/// Tests and callers that hold a path: the repository's workdir, or the discovered git dir's parent
/// when bare; `None` when no repository is reachable.
pub fn workdir(path: &Path) -> Option<PathBuf> {
    let repo = Repository::discover(path).ok()?;
    repo.workdir().map(std::path::Path::to_path_buf)
}

pub fn tree_has_path(path: &Path, rev: &str, file: &str) -> anyhow::Result<bool> {
    let repo = open(path)?;
    let obj = repo.revparse_single(rev)?;
    let tree = obj.peel_to_tree()?;
    let found = tree.get_path(Path::new(file)).is_ok();
    if found && let Ok(entry) = tree.get_path(Path::new(file)) {
        return Ok(entry.kind() == Some(ObjectType::Blob));
    }
    Ok(false)
}

/// Every working directory, failing when a registered one cannot be read; the main checkout is
/// included even when called from a linked worktree.
pub fn list_worktrees(path: &Path) -> anyhow::Result<Vec<crate::model::Worktree>> {
    let current = Repository::discover(path)?;
    let repo = Repository::open(current.commondir())?;
    let current_path = current
        .workdir()
        .context("repository has no working directory")?
        .canonicalize()?;
    let describe = |repo: &Repository, name: String| -> anyhow::Result<crate::model::Worktree> {
        let path = repo
            .workdir()
            .context("worktree has no working directory")?
            .canonicalize()?;
        let head = repo.head()?;
        Ok(crate::model::Worktree {
            name,
            current: path == current_path,
            path: path.to_string_lossy().into_owned(),
            head: head.shorthand()?.to_owned(),
        })
    };
    // An unborn main checkout still has a valid working directory.
    let mut rows = if matches!(repo.head(), Err(error) if error.code() == git2::ErrorCode::UnbornBranch)
    {
        let path = repo
            .workdir()
            .context("repository has no working directory")?
            .canonicalize()?;
        vec![crate::model::Worktree {
            name: "main".into(),
            current: path == current_path,
            path: path.to_string_lossy().into_owned(),
            head: repo
                .find_reference("HEAD")?
                .symbolic_target()?
                .unwrap_or("HEAD")
                .trim_start_matches("refs/heads/")
                .to_owned(),
        }]
    } else {
        vec![describe(&repo, "main".into())?]
    };
    for name in &repo.worktrees()? {
        let name = name?.context("worktree name is not UTF-8")?;
        let worktree = repo.find_worktree(name)?;
        worktree.validate()?;
        rows.push(describe(
            &Repository::open_from_worktree(&worktree)?,
            name.into(),
        )?);
    }
    rows.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(rows)
}

/// Create a branch and a linked checkout at a chosen revision: never overwriting a branch or an
/// existing path, and never switching the caller's HEAD.
pub fn create_worktree(
    path: &Path,
    destination: &ui::tilo_resource::AbsolutePath,
    branch: &str,
    start: &str,
) -> anyhow::Result<()> {
    if destination.symlink_metadata().is_ok() {
        bail!("worktree destination already exists");
    }
    let parent = destination
        .parent()
        .context("worktree destination has no parent")?;
    if !parent.is_dir() {
        bail!("worktree destination parent must exist");
    }
    if branch.trim().is_empty() || start.trim().is_empty() {
        bail!("branch and start point must not be empty");
    }
    let current = Repository::discover(path)?;
    let repo = Repository::open(current.commondir())?;
    let commit_id = current.revparse_single(start)?.peel_to_commit()?.id();
    let commit = repo.find_commit(commit_id)?;
    let new_branch = repo.branch(branch, &commit, false)?;
    // A safe administrative name independent of slashes in the branch name.
    let worktree_name = format!("tilo-{}", hex::encode(branch.as_bytes()));
    let mut options = git2::WorktreeAddOptions::new();
    options.reference(Some(new_branch.get()));
    if let Err(error) = repo.worktree(&worktree_name, destination, Some(&options)) {
        bail!("worktree creation failed: {error}; newly created branch {branch} was retained");
    }
    Ok(())
}
