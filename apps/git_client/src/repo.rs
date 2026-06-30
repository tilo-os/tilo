//! Live repository backend, built on `git2`.
//!
//! Reads an on-disk repository into the render-friendly [`RepoModel`]:
//! the working-tree status (staged / unstaged / untracked), per-file
//! diffs, the commit history with ref decorations, and the branch / tag /
//! stash / remote panels. The functions here are the only place that
//! touch libgit2 — the rest of the app works purely off [`RepoModel`], so
//! the headless screenshot path can substitute the bundled sample without
//! a repository present.
//!
//! Everything is read-only and best-effort: a missing piece (no upstream,
//! an unreadable object) is skipped rather than fatal, so a partially
//! valid repo still renders.

use std::collections::HashMap;
use std::path::Path;

use git2::{
    BranchType, Delta, DiffFormat, DiffOptions, Repository, Sort, Status, StatusOptions,
};

use crate::diff::{language_for, parse_unified};
use crate::model::{
    Branch, ChangeArea, ChangeKind, Commit, FileChange, FileDiff, RefKind, RefLabel, Remote,
    RepoModel, Stash, Tag,
};

/// How many commits to walk for the history view. Bounded so a huge repo
/// doesn't stall the first frame.
const HISTORY_LIMIT: usize = 400;

/// Open the repository discovered from `path` (walks up to find `.git`)
/// and read it into a full model plus a map of per-file diffs keyed by
/// path. Returns `None` if no repository can be opened.
pub fn load(path: &Path) -> Option<(RepoModel, HashMap<String, FileDiff>)> {
    let repo = Repository::discover(path).ok()?;
    let mut model = RepoModel::default();
    let mut diffs: HashMap<String, FileDiff> = HashMap::new();

    if let Some(wd) = repo.workdir() {
        model.workdir = wd.to_string_lossy().to_string();
        model.name = wd
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "repository".into());
    }

    read_identity(&repo, &mut model);
    read_head(&repo, &mut model);
    read_status(&repo, &mut model, &mut diffs);
    read_history(&repo, &mut model, &mut diffs);
    read_branches(&repo, &mut model);
    read_tags(&repo, &mut model);
    read_stashes(&repo, &mut model);
    read_remotes(&repo, &mut model);

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
                    .map(|o| short_id(&o.to_string()))
                    .unwrap_or_else(|| "HEAD".into());
                model.head = format!("detached @ {short}");
            }
        }
        Err(_) => model.head = "(no commits yet)".into(),
    }
}

fn classify(status: Status) -> Option<(ChangeArea, ChangeKind)> {
    // Prefer the staged view when both index and worktree changed; the
    // unstaged delta surfaces separately via the worktree flags below.
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

fn read_status(
    repo: &Repository,
    model: &mut RepoModel,
    diffs: &mut HashMap<String, FileDiff>,
) {
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

        // A file can have both a staged and a worktree change; record each.
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

    // Build per-file diffs for the worktree (index→workdir) and the index
    // (HEAD→index), and fold +/- counts back onto the list entries.
    collect_worktree_diffs(repo, model, diffs);
    collect_index_diffs(repo, model, diffs);
}

fn collect_worktree_diffs(
    repo: &Repository,
    model: &mut RepoModel,
    diffs: &mut HashMap<String, FileDiff>,
) {
    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .context_lines(3);
    let Ok(diff) = repo.diff_index_to_workdir(None, Some(&mut opts)) else {
        return;
    };
    fold_diff(&diff, model, diffs, false);
}

fn collect_index_diffs(
    repo: &Repository,
    model: &mut RepoModel,
    diffs: &mut HashMap<String, FileDiff>,
) {
    let head_tree = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_tree().ok());
    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    let Ok(diff) = repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts)) else {
        return;
    };
    fold_diff(&diff, model, diffs, true);
}

/// Render a `git2::Diff` to unified text per-file, parse it, store it, and
/// fold +/- counts back onto the matching list entries.
fn fold_diff(
    diff: &git2::Diff,
    model: &mut RepoModel,
    diffs: &mut HashMap<String, FileDiff>,
    staged: bool,
) {
    // Accumulate raw unified text per new-path.
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
        // Mark new/deleted from delta status if the textual markers missed it.
        if let Some(delta) = find_delta(diff, &path) {
            match delta {
                Delta::Added | Delta::Untracked => fd.is_new = true,
                Delta::Deleted => fd.is_deleted = true,
                _ => {}
            }
        }
        let (adds, dels) = (fd.additions(), fd.deletions());
        // Fold counts into the matching list entry.
        let lists: [&mut Vec<FileChange>; 3] = if staged {
            [&mut model.staged, &mut model.unstaged, &mut model.untracked]
        } else {
            [&mut model.unstaged, &mut model.untracked, &mut model.staged]
        };
        for list in lists {
            if let Some(fc) = list.iter_mut().find(|f| f.path == path) {
                if fc.additions == 0 && fc.deletions == 0 {
                    fc.additions = adds;
                    fc.deletions = dels;
                }
            }
        }
        diffs.entry(path).or_insert(fd);
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

fn read_history(
    repo: &Repository,
    model: &mut RepoModel,
    _diffs: &mut HashMap<String, FileDiff>,
) {
    // Map oid -> ref labels so each commit row can show its decorations.
    let ref_map = ref_decorations(repo);

    let Ok(mut walk) = repo.revwalk() else { return };
    let _ = walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME);
    if walk.push_head().is_err() {
        // No HEAD (empty repo) — also try every local branch tip.
        if let Ok(branches) = repo.branches(Some(BranchType::Local)) {
            for b in branches.flatten() {
                if let Some(oid) = b.0.get().target() {
                    let _ = walk.push(oid);
                }
            }
        }
    }
    // Include side branches so the graph shows lanes other than HEAD's.
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
        let Ok(commit) = repo.find_commit(oid) else { continue };
        let id = oid.to_string();
        let author = commit.author();
        let summary = commit.summary().unwrap_or("").to_string();
        let full = commit.message().unwrap_or("");
        let body = full
            .splitn(2, '\n')
            .nth(1)
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

    // HEAD pointer.
    if let Ok(head) = repo.head() {
        if let Some(oid) = head.target() {
            map.entry(oid.to_string())
                .or_default()
                .push(RefLabel { name: "HEAD".into(), kind: RefKind::Head });
        }
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
    let Ok(branches) = repo.branches(None) else { return };
    for (branch, btype) in branches.flatten() {
        let Some(name) = branch.name().ok().flatten() else { continue };
        let is_remote = btype == BranchType::Remote;
        let tip = branch
            .get()
            .target()
            .map(|o| short_id(&o.to_string()))
            .unwrap_or_default();
        let upstream = branch
            .upstream()
            .ok()
            .and_then(|u| u.name().ok().flatten().map(|s| s.to_string()));
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
    if let (Some(l), Some(u)) = (local, upstream) {
        if let Ok((ahead, behind)) = repo.graph_ahead_behind(l, u) {
            return (ahead as u32, behind as u32);
        }
    }
    (0, 0)
}

fn read_tags(repo: &Repository, model: &mut RepoModel) {
    let Ok(names) = repo.tag_names(None) else { return };
    for name in names.iter().flatten() {
        let full = format!("refs/tags/{name}");
        let target = repo
            .revparse_single(&full)
            .ok()
            .map(|o| short_id(&o.id().to_string()))
            .unwrap_or_default();
        // Annotated-tag message, if any.
        let message = repo
            .revparse_single(&full)
            .ok()
            .and_then(|o| o.into_tag().ok())
            .and_then(|t| t.message().map(|m| m.trim().to_string()));
        model.tags.push(Tag {
            name: name.to_string(),
            target,
            message,
        });
    }
}

fn read_stashes(repo: &Repository, model: &mut RepoModel) {
    // `stash_foreach` needs a mutable repo; re-open to avoid threading the
    // mutability through the read pipeline.
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
    for name in names.iter().flatten() {
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

/// Coarse relative time string (e.g. "3 hours ago"). Uses `chrono` for
/// the "now" reference; falls back to absolute seconds on overflow.
fn relative_time(seconds: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let delta = now.saturating_sub(seconds);
    if delta < 0 {
        return "just now".into();
    }
    let mins = delta / 60;
    let hours = delta / 3600;
    let days = delta / 86_400;
    if delta < 60 {
        "just now".into()
    } else if mins < 60 {
        format!("{mins} minute{} ago", plural(mins))
    } else if hours < 24 {
        format!("{hours} hour{} ago", plural(hours))
    } else if days < 7 {
        format!("{days} day{} ago", plural(days))
    } else if days < 30 {
        let weeks = days / 7;
        format!("{weeks} week{} ago", plural(weeks))
    } else if days < 365 {
        let months = days / 30;
        format!("{months} month{} ago", plural(months))
    } else {
        let years = days / 365;
        format!("{years} year{} ago", plural(years))
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 { "" } else { "s" }
}
