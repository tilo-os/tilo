//! Integration tests that drive the git client's PRODUCTION actions against
//! a real temporary git repository and assert the repo actually mutated.
//!
//! Nothing here is a tautology over in-memory state: each test creates a
//! fresh repo with `git2`, runs the same `crate::repo::*` function the app
//! invokes (or, where it adds value, drives the `GitClientApp` action that
//! calls it), and then re-opens the repo with `git2` to verify the index,
//! HEAD, commit tree, branches, or stash list changed as expected.
//!
//! These tests use libgit2 directly to BUILD fixtures and to VERIFY the
//! after-state — they never shell out to the `git` CLI.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use git2::{Repository, Signature};

use git_client::app::GitClientApp;
use git_client::model::ChangeArea;
use git_client::panels::BranchAction;
use git_client::repo;

/// 40 lines of `"line N\n"`, used as fixture content across several
/// hunk-boundary tests.
fn numbered_lines() -> String {
    (1..=40).fold(String::new(), |mut s, n| {
        let _ = writeln!(s, "line {n}");
        s
    })
}

/// Create a fresh repo in a tempdir with a configured identity and an
/// initial commit containing `file.txt`. Returns the tempdir (kept alive)
/// and the workdir path.
fn init_repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_path_buf();
    let repo = Repository::init(&path).expect("git init");
    {
        let mut cfg = repo.config().expect("config");
        cfg.set_str("user.name", "Test User").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        // These fixtures assert exact LF bytes. Do not inherit a host's
        // checkout policy; CRLF behavior has its own explicit coverage below.
        cfg.set_bool("core.autocrlf", false).unwrap();
    }
    fs::write(path.join("file.txt"), "line 1\nline 2\nline 3\n").unwrap();
    let sig = Signature::now("Test User", "test@example.com").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[])
        .unwrap();
    (dir, path)
}

/// Re-open the repo to inspect its true state.
fn open(path: &Path) -> Repository {
    Repository::discover(path).expect("discover repo")
}

/// Does the index have a staged entry for `file` differing from HEAD?
fn is_staged(path: &Path, file: &str) -> bool {
    let repo = open(path);
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let diff = repo
        .diff_tree_to_index(head_tree.as_ref(), None, None)
        .unwrap();
    diff.deltas().any(|d| {
        d.new_file().path() == Some(Path::new(file)) || d.old_file().path() == Some(Path::new(file))
    })
}

// =====================================================================

/// Whole-file index operations and commits: stage, unstage, stage-untracked then
/// commit, empty-message and missing-identity rejection, and stash save/apply.
/// Each phase is a former test with its own fresh fixture.
#[test]
fn file_staging_commit_and_stash_round_trip() {
    stage_file_writes_index();
    unstage_file_resets_index_to_head();
    stage_untracked_then_commit_creates_object_with_file();
    commit_with_empty_message_is_rejected();
    commit_without_identity_is_rejected();
    stash_save_and_apply_roundtrip();
}

fn stage_file_writes_index() {
    let (_d, path) = init_repo();
    // Modify the tracked file in the working tree.
    fs::write(path.join("file.txt"), "line 1\nCHANGED\nline 3\n").unwrap();
    assert!(!is_staged(&path, "file.txt"), "precondition: not staged");

    repo::stage_file(&path, "file.txt").expect("stage");

    assert!(
        is_staged(&path, "file.txt"),
        "stage_file must write the index"
    );
}

fn unstage_file_resets_index_to_head() {
    let (_d, path) = init_repo();
    fs::write(path.join("file.txt"), "line 1\nCHANGED\nline 3\n").unwrap();
    repo::stage_file(&path, "file.txt").unwrap();
    assert!(is_staged(&path, "file.txt"));

    repo::unstage_file(&path, "file.txt").expect("unstage");

    assert!(
        !is_staged(&path, "file.txt"),
        "unstage_file must reset the index entry"
    );
    // The working tree edit must be preserved (unstage != discard).
    let wt = fs::read_to_string(path.join("file.txt")).unwrap();
    assert!(
        wt.contains("CHANGED"),
        "working tree edit must survive unstage"
    );
}

fn stage_untracked_then_commit_creates_object_with_file() {
    let (_d, path) = init_repo();
    fs::write(path.join("new.rs"), "fn main() {}\n").unwrap();

    repo::stage_file(&path, "new.rs").unwrap();
    let short = repo::commit(&path, "add new.rs").expect("commit");
    assert_eq!(short.len(), 7);

    // The new HEAD commit's tree must contain new.rs as a blob.
    assert!(
        repo::tree_has_path(&path, "HEAD", "new.rs").unwrap(),
        "committed tree must contain the staged file"
    );
    // And HEAD's summary must be our message.
    let repo = open(&path);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.summary(), Ok(Some("add new.rs")));
    assert_eq!(head.author().name(), Ok("Test User"));
    assert_eq!(head.author().email(), Ok("test@example.com"));
}

fn commit_with_empty_message_is_rejected() {
    let (_d, path) = init_repo();
    fs::write(path.join("file.txt"), "x\n").unwrap();
    repo::stage_file(&path, "file.txt").unwrap();
    let err = repo::commit(&path, "   ");
    assert!(err.is_err(), "blank message must be rejected");
}

fn commit_without_identity_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let repo = Repository::init(&path).unwrap();
    // Empty local values shadow any system/global identity. Removing local
    // keys would expose the host's identity and silently skip the assertion.
    {
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "").unwrap();
        cfg.set_str("user.email", "").unwrap();
    }
    fs::write(path.join("a.txt"), "a\n").unwrap();
    repo::stage_file(&path, "a.txt").unwrap();
    assert!(open(&path).signature().is_err());
    assert!(repo::commit(&path, "msg").is_err());
}

/// Ref operations: create+checkout, checkout existing, delete (refusing HEAD),
/// and fetch/pull against a local remote. Each phase is a former test with its own fresh fixture.
#[test]
fn branch_and_remote_operations_move_refs() {
    create_branch_and_checkout_moves_head();
    checkout_existing_branch_switches_head();
    delete_branch_removes_ref_and_refuses_head();
    fetch_and_pull_against_a_local_remote();
}

fn create_branch_and_checkout_moves_head() {
    let (_d, path) = init_repo();
    repo::create_branch(&path, "feature", None, true).expect("create+checkout");

    let repo = open(&path);
    let head = repo.head().unwrap();
    assert!(head.is_branch());
    assert_eq!(
        head.shorthand(),
        Ok("feature"),
        "HEAD must move to the new branch"
    );
}

fn checkout_existing_branch_switches_head() {
    let (_d, path) = init_repo();
    repo::create_branch(&path, "feature", None, false).unwrap();
    // Still on the default branch.
    let original = open(&path).head().unwrap().shorthand().unwrap().to_string();
    assert_ne!(original, "feature");

    repo::checkout_branch(&path, "feature").expect("checkout");
    assert_eq!(open(&path).head().unwrap().shorthand(), Ok("feature"));

    repo::checkout_branch(&path, &original).expect("checkout back");
    assert_eq!(
        open(&path).head().unwrap().shorthand(),
        Ok(original.as_str())
    );
}

fn delete_branch_removes_ref_and_refuses_head() {
    let (_d, path) = init_repo();
    repo::create_branch(&path, "doomed", None, false).unwrap();
    assert!(
        open(&path)
            .find_branch("doomed", git2::BranchType::Local)
            .is_ok()
    );

    repo::delete_branch(&path, "doomed").expect("delete");
    assert!(
        open(&path)
            .find_branch("doomed", git2::BranchType::Local)
            .is_err(),
        "branch ref must be gone"
    );

    // Deleting the checked-out branch must fail.
    let head = open(&path).head().unwrap().shorthand().unwrap().to_string();
    assert!(
        repo::delete_branch(&path, &head).is_err(),
        "cannot delete HEAD branch"
    );
}

/// Hunk-level staging: stage and unstage one hunk, an untracked file's hunk stages
/// the whole file, and a partially staged file keeps distinct diffs. Each phase is a former test with its own fresh fixture.
#[test]
fn hunk_staging_touches_only_the_selected_hunk() {
    stage_hunk_stages_only_the_selected_hunk();
    unstage_hunk_reverses_a_staged_hunk();
    stage_hunk_of_untracked_file_stages_whole_file();
    partially_staged_file_keeps_distinct_staged_and_worktree_diffs();
}

fn stage_hunk_stages_only_the_selected_hunk() {
    let (_d, path) = init_repo();
    // A file with two well-separated change regions so the diff has 2 hunks.
    fs::write(path.join("multi.txt"), numbered_lines()).unwrap();
    repo::stage_file(&path, "multi.txt").unwrap();
    repo::commit(&path, "add multi").unwrap();

    // Edit line 2 and line 38 — two separate hunks.
    let mut lines: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
    lines[1] = "line 2 CHANGED".into();
    lines[37] = "line 38 CHANGED".into();
    fs::write(path.join("multi.txt"), lines.join("\n") + "\n").unwrap();

    // Sanity: the worktree diff has exactly two hunks.
    let repo = open(&path);
    let mut opts = git2::DiffOptions::new();
    opts.pathspec("multi.txt");
    let diff = repo.diff_index_to_workdir(None, Some(&mut opts)).unwrap();
    let nhunks = std::cell::Cell::new(0usize);
    diff.foreach(
        &mut |_, _| true,
        None,
        Some(&mut |_, _| {
            nhunks.set(nhunks.get().checked_add(1).unwrap());
            true
        }),
        None,
    )
    .unwrap();
    assert_eq!(nhunks.get(), 2, "fixture must produce two hunks");

    // Stage ONLY hunk 0.
    repo::apply_hunk(&path, "multi.txt", 0, false).expect("apply hunk 0");

    // The staged (HEAD..index) diff must contain the first change but NOT the
    // second; the working tree (index..workdir) must still hold the second.
    let repo = open(&path);
    let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
    let staged_diff = repo
        .diff_tree_to_index(Some(&head_tree), None, None)
        .unwrap();
    let staged_text = diff_to_string(&staged_diff);
    assert!(
        staged_text.contains("line 2 CHANGED"),
        "hunk 0 must be staged"
    );
    assert!(
        !staged_text.contains("line 38 CHANGED"),
        "hunk 1 must NOT be staged"
    );

    let wt_diff = repo.diff_index_to_workdir(None, None).unwrap();
    let wt_text = diff_to_string(&wt_diff);
    assert!(
        wt_text.contains("line 38 CHANGED"),
        "the un-staged hunk must remain in the working tree"
    );
}

fn unstage_hunk_reverses_a_staged_hunk() {
    let (_d, path) = init_repo();
    fs::write(path.join("multi.txt"), numbered_lines()).unwrap();
    repo::stage_file(&path, "multi.txt").unwrap();
    repo::commit(&path, "add multi").unwrap();

    let mut lines: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
    lines[1] = "line 2 CHANGED".into();
    lines[37] = "line 38 CHANGED".into();
    fs::write(path.join("multi.txt"), lines.join("\n") + "\n").unwrap();
    // Stage the whole file (both hunks).
    repo::stage_file(&path, "multi.txt").unwrap();

    // Now unstage just hunk 0 from the index.
    repo::apply_hunk(&path, "multi.txt", 0, true).expect("unstage hunk 0");

    let repo = open(&path);
    let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
    let staged_text = diff_to_string(
        &repo
            .diff_tree_to_index(Some(&head_tree), None, None)
            .unwrap(),
    );
    assert!(
        !staged_text.contains("line 2 CHANGED"),
        "hunk 0 must have been unstaged"
    );
    assert!(
        staged_text.contains("line 38 CHANGED"),
        "hunk 1 must remain staged"
    );
}

fn stash_save_and_apply_roundtrip() {
    let (_d, path) = init_repo();
    fs::write(path.join("file.txt"), "line 1\nWIP EDIT\nline 3\n").unwrap();

    let short = repo::stash_save(&path, "wip work").expect("stash save");
    assert_eq!(short.len(), 7);
    // After stashing, the working tree is clean again.
    let clean = fs::read_to_string(path.join("file.txt")).unwrap();
    assert!(
        !clean.contains("WIP EDIT"),
        "stash must clear the working edit"
    );
    // The stash list has one entry.
    {
        let mut repo = open(&path);
        let mut count = 0;
        repo.stash_foreach(|_, _, _| {
            count = usize::checked_add(count, 1).unwrap();
            true
        })
        .unwrap();
        assert_eq!(count, 1, "one stash entry expected");
    }

    repo::stash_apply(&path, 0).expect("stash apply");
    let restored = fs::read_to_string(path.join("file.txt")).unwrap();
    assert!(restored.contains("WIP EDIT"), "apply must restore the edit");
}

fn fetch_and_pull_against_a_local_remote() {
    // Build an "upstream" repo with two commits, clone it via a file:// path
    // into a "local" repo at the first commit, then fetch+pull to advance.
    let up_dir = tempfile::tempdir().unwrap();
    let up = up_dir.path().to_path_buf();
    let upstream = Repository::init_bare(&up).unwrap();
    // Seed the bare repo through a temporary working clone.
    let work_dir = tempfile::tempdir().unwrap();
    let work = work_dir.path().to_path_buf();
    let work_repo = Repository::clone(up.to_str().unwrap(), &work).unwrap();
    {
        let mut cfg = work_repo.config().unwrap();
        cfg.set_str("user.name", "Up Stream").unwrap();
        cfg.set_str("user.email", "up@example.com").unwrap();
    }
    let sig = Signature::now("Up Stream", "up@example.com").unwrap();
    // commit 1
    fs::write(work.join("a.txt"), "one\n").unwrap();
    let mut idx = work_repo.index().unwrap();
    idx.add_path(Path::new("a.txt")).unwrap();
    idx.write().unwrap();
    let t1 = work_repo.find_tree(idx.write_tree().unwrap()).unwrap();
    let c1 = work_repo
        .commit(Some("HEAD"), &sig, &sig, "commit one", &t1, &[])
        .unwrap();
    work_repo
        .find_remote("origin")
        .unwrap()
        .push(&["refs/heads/master:refs/heads/master"], None)
        .or_else(|_| {
            work_repo
                .find_remote("origin")
                .unwrap()
                .push(&["refs/heads/main:refs/heads/main"], None)
        })
        .ok();

    // Determine the default branch name the clone produced.
    let default_branch = work_repo.head().unwrap().shorthand().unwrap().to_string();
    let refspec = format!("refs/heads/{default_branch}:refs/heads/{default_branch}");
    work_repo
        .find_remote("origin")
        .unwrap()
        .push(&[&refspec], None)
        .unwrap();

    // Now clone the upstream into our "local" repo at commit 1.
    let local_dir = tempfile::tempdir().unwrap();
    let local = local_dir.path().to_path_buf();
    Repository::clone(up.to_str().unwrap(), &local).unwrap();

    // Advance upstream with a second commit.
    let c1_commit = work_repo.find_commit(c1).unwrap();
    fs::write(work.join("a.txt"), "one\ntwo\n").unwrap();
    let mut idx = work_repo.index().unwrap();
    idx.add_path(Path::new("a.txt")).unwrap();
    idx.write().unwrap();
    let t2 = work_repo.find_tree(idx.write_tree().unwrap()).unwrap();
    work_repo
        .commit(Some("HEAD"), &sig, &sig, "commit two", &t2, &[&c1_commit])
        .unwrap();
    work_repo
        .find_remote("origin")
        .unwrap()
        .push(&[&refspec], None)
        .unwrap();

    // Local repo: fetch must succeed and pull must fast-forward to commit two.
    let _ = upstream; // keep alive
    repo::fetch(&local, Some("origin")).expect("fetch from local remote");
    repo::pull(&local, Some("origin")).expect("ff pull from local remote");

    let local_repo = open(&local);
    let head = local_repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(
        head.summary(),
        Ok(Some("commit two")),
        "pull must fast-forward HEAD"
    );
    assert!(
        local.join("a.txt").exists()
            && fs::read_to_string(local.join("a.txt"))
                .unwrap()
                .contains("two"),
        "working tree must reflect the pulled content"
    );
}

// =====================================================================
// App-level: drive the GitClientApp's PRODUCTION action methods so the
// exact UI code path is exercised end-to-end against a real repo.
// =====================================================================

/// The app's production actions drive the real repository: stage and commit,
/// toggle-stage one file, checkout and create a branch, stage a hunk, discard a
/// file. Each phase is a former test with its own fresh fixture.
#[test]
fn app_production_actions_mutate_the_repository() {
    app_stage_and_commit_through_production_actions();
    app_toggle_stage_single_file_mutates_index();
    app_checkout_branch_through_production_action();
    app_create_branch_through_production_action();
    app_stage_hunk_through_production_action();
    app_discard_file_through_production_action();
}

fn app_stage_and_commit_through_production_actions() {
    let (_d, path) = init_repo();
    // Two pending changes: a tracked modification and a new untracked file.
    fs::write(path.join("file.txt"), "line 1\nEDITED\nline 3\n").unwrap();
    fs::write(path.join("added.rs"), "pub fn f() {}\n").unwrap();

    let mut app = GitClientApp::open_at(&path).expect("open repo");
    let (staged0, unstaged0, untracked0) = app.test_change_counts();
    assert_eq!(staged0, 0, "nothing staged initially");
    assert!(unstaged0 >= 1, "tracked edit shows as unstaged");
    assert!(untracked0 >= 1, "new file shows as untracked");

    // Stage everything via the production action, then commit via the
    // production action. Assert against the REAL repo afterwards.
    app.test_stage_all();
    assert!(is_staged(&path, "file.txt"), "stage_all staged the edit");
    assert!(
        is_staged(&path, "added.rs"),
        "stage_all staged the new file"
    );

    app.test_commit("wire real git");
    assert_eq!(app.test_last_error(), None, "commit must not error");

    // Verify a real commit landed at HEAD with both files in its tree.
    let repo = open(&path);
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.summary(), Ok(Some("wire real git")));
    assert!(repo::tree_has_path(&path, "HEAD", "added.rs").unwrap());
    // The model now reports a clean tree and shows the new commit first.
    let (s, u, n) = app.test_change_counts();
    assert_eq!((s, u, n), (0, 0, 0), "tree clean after commit");
    assert_eq!(
        app.test_commit_summaries()
            .first()
            .map(std::string::String::as_str),
        Some("wire real git"),
        "history reload shows the new commit at the tip"
    );
}

fn app_toggle_stage_single_file_mutates_index() {
    let (_d, path) = init_repo();
    fs::write(path.join("file.txt"), "line 1\nEDITED\nline 3\n").unwrap();
    let mut app = GitClientApp::open_at(&path).unwrap();

    // Stage the single unstaged file (index 0 in the unstaged list).
    app.test_toggle_stage(ChangeArea::Unstaged, 0);
    assert!(
        is_staged(&path, "file.txt"),
        "toggling unstaged file stages it"
    );

    // Now unstage it back (index 0 in the staged list).
    app.test_toggle_stage(ChangeArea::Staged, 0);
    assert!(
        !is_staged(&path, "file.txt"),
        "toggling staged file unstages it"
    );
}

fn app_checkout_branch_through_production_action() {
    let (_d, path) = init_repo();
    repo::create_branch(&path, "feature", None, false).unwrap();
    let mut app = GitClientApp::open_at(&path).unwrap();
    let original = app.test_head().to_string();
    assert_ne!(original, "feature");

    app.test_branch_action(BranchAction::Checkout("feature".into()));
    assert_eq!(app.test_last_error(), None);
    // The REAL repo HEAD moved.
    assert_eq!(open(&path).head().unwrap().shorthand(), Ok("feature"));
    // And the reloaded model agrees.
    assert_eq!(app.test_head(), "feature");
}

fn app_create_branch_through_production_action() {
    let (_d, path) = init_repo();
    let mut app = GitClientApp::open_at(&path).unwrap();

    app.test_create_branch("topic/awesome");
    assert_eq!(app.test_last_error(), None);
    // Real repo: branch exists and HEAD switched to it (create+checkout).
    assert!(
        open(&path)
            .find_branch("topic/awesome", git2::BranchType::Local)
            .is_ok()
    );
    assert_eq!(open(&path).head().unwrap().shorthand(), Ok("topic/awesome"));
    assert!(app.test_branch_names().iter().any(|n| n == "topic/awesome"));
}

fn app_stage_hunk_through_production_action() {
    let (_d, path) = init_repo();
    fs::write(path.join("multi.txt"), numbered_lines()).unwrap();
    repo::stage_file(&path, "multi.txt").unwrap();
    repo::commit(&path, "add multi").unwrap();
    let mut lines: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
    lines[1] = "line 2 CHANGED".into();
    lines[37] = "line 38 CHANGED".into();
    fs::write(path.join("multi.txt"), lines.join("\n") + "\n").unwrap();

    let mut app = GitClientApp::open_at(&path).unwrap();
    // Select the unstaged multi.txt, then stage just its first hunk.
    let unstaged_idx = {
        // Find multi.txt's index within the unstaged list via the model.
        // (open_at preselects the first change; we set it explicitly.)
        0usize
    };
    app.test_select(ChangeArea::Unstaged, unstaged_idx);
    app.test_stage_hunk(0);
    assert_eq!(app.test_last_error(), None, "hunk stage must not error");

    let repo = open(&path);
    let head_tree = repo.head().unwrap().peel_to_tree().unwrap();
    let staged_text = diff_to_string(
        &repo
            .diff_tree_to_index(Some(&head_tree), None, None)
            .unwrap(),
    );
    assert!(
        staged_text.contains("line 2 CHANGED"),
        "first hunk staged via app action"
    );
    assert!(
        !staged_text.contains("line 38 CHANGED"),
        "second hunk left unstaged"
    );
}

/// Opening repositories: a plain directory is handled gracefully and rejected by
/// the empty state, a real repository opens, and a Unicode repository commits an
/// exact Unicode file. Each phase is a former test with its own fresh fixture.
#[test]
fn opening_a_repository_accepts_real_repos_and_rejects_plain_directories() {
    not_a_repo_is_handled_gracefully();
    app_empty_state_opens_a_real_repository();
    app_empty_state_rejects_a_non_repository_path();
    app_opens_unicode_repository_and_commits_exact_unicode_file();
}

fn not_a_repo_is_handled_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    // A plain directory, no git.
    assert!(!repo::is_repo(dir.path()), "is_repo false for non-repo");
    assert!(
        GitClientApp::open_at(dir.path()).is_none(),
        "open_at returns None"
    );
    assert!(repo::load(dir.path()).is_none(), "load returns None");
}

fn stage_hunk_of_untracked_file_stages_whole_file() {
    // An untracked file has no index base for a per-hunk apply, so staging any
    // of its hunks must stage the whole file rather than erroring.
    let (_d, path) = init_repo();
    fs::write(path.join("new.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    assert!(!is_staged(&path, "new.rs"));

    repo::apply_hunk(&path, "new.rs", 0, false).expect("stage untracked hunk");
    assert!(
        is_staged(&path, "new.rs"),
        "untracked hunk-stage stages the file"
    );
}

/// Discarding worktree changes: a tracked edit reverts, an untracked file is
/// deleted, one hunk reverts alone, and CRLF checkout configuration survives.
/// Each phase is a former test with its own fresh fixture.
#[test]
fn discard_reverts_worktree_files_and_hunks() {
    discard_file_reverts_tracked_worktree_edit();
    discard_file_deletes_untracked_file();
    discard_hunk_reverts_only_the_selected_worktree_hunk();
    discard_hunk_and_file_preserve_configured_crlf_checkout();
}

fn discard_file_reverts_tracked_worktree_edit() {
    let (_d, path) = init_repo();
    let original = fs::read_to_string(path.join("file.txt")).unwrap();
    fs::write(path.join("file.txt"), "line 1\nDISCARD ME\nline 3\n").unwrap();

    repo::discard_file(&path, "file.txt").expect("discard tracked edit");

    let after = fs::read_to_string(path.join("file.txt")).unwrap();
    assert_eq!(
        after, original,
        "discard must restore the committed content"
    );
}

fn discard_file_deletes_untracked_file() {
    let (_d, path) = init_repo();
    fs::write(path.join("scratch.tmp"), "junk\n").unwrap();
    assert!(path.join("scratch.tmp").exists());

    repo::discard_file(&path, "scratch.tmp").expect("discard untracked");

    assert!(
        !path.join("scratch.tmp").exists(),
        "untracked discard deletes the file"
    );
}

fn discard_hunk_reverts_only_the_selected_worktree_hunk() {
    let (_d, path) = init_repo();
    fs::write(path.join("multi.txt"), numbered_lines()).unwrap();
    repo::stage_file(&path, "multi.txt").unwrap();
    repo::commit(&path, "add multi").unwrap();
    // Two well-separated edits → two worktree hunks.
    let mut lines: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
    lines[1] = "line 2 EDIT".into();
    lines[37] = "line 38 EDIT".into();
    fs::write(path.join("multi.txt"), lines.join("\n") + "\n").unwrap();

    // Discard only the first hunk from the working tree.
    repo::discard_hunk(&path, "multi.txt", 0).expect("discard hunk 0");

    let after = fs::read_to_string(path.join("multi.txt")).unwrap();
    assert!(
        after.contains("line 2\n"),
        "hunk 0 reverted to committed content"
    );
    assert!(!after.contains("line 2 EDIT"), "hunk 0's edit is gone");
    assert!(after.contains("line 38 EDIT"), "hunk 1's edit must remain");
}

fn discard_hunk_and_file_preserve_configured_crlf_checkout() {
    let (_dir, path) = init_repo();
    open(&path)
        .config()
        .unwrap()
        .set_bool("core.autocrlf", true)
        .unwrap();
    let committed = numbered_lines();
    let checkout = committed.replace('\n', "\r\n");
    fs::write(path.join("multi.txt"), &checkout).unwrap();
    repo::stage_file(&path, "multi.txt").unwrap();
    repo::commit(&path, "add CRLF worktree file").unwrap();
    let changed = checkout
        .replace("line 2\r\n", "line 2 EDIT\r\n")
        .replace("line 38\r\n", "line 38 EDIT\r\n");
    fs::write(path.join("multi.txt"), changed).unwrap();

    repo::discard_hunk(&path, "multi.txt", 0).unwrap();
    assert_eq!(
        fs::read_to_string(path.join("multi.txt")).unwrap(),
        checkout.replace("line 38\r\n", "line 38 EDIT\r\n"),
        "discarding the first hunk must preserve the second edit and every CRLF"
    );
    repo::discard_file(&path, "multi.txt").unwrap();
    assert_eq!(
        fs::read_to_string(path.join("multi.txt")).unwrap(),
        checkout
    );
    let repository = open(&path);
    let tree = repository.head().unwrap().peel_to_tree().unwrap();
    let entry = tree.get_path(Path::new("multi.txt")).unwrap();
    assert_eq!(
        repository.find_blob(entry.id()).unwrap().content(),
        committed.as_bytes(),
        "Git storage must remain LF while the checkout follows its CRLF policy"
    );
}

fn partially_staged_file_keeps_distinct_staged_and_worktree_diffs() {
    use git_client::model::DiffKey;
    let (_d, path) = init_repo();
    fs::write(path.join("multi.txt"), numbered_lines()).unwrap();
    repo::stage_file(&path, "multi.txt").unwrap();
    repo::commit(&path, "add multi").unwrap();
    // Edit + stage line 2, then edit line 38 without staging: the file is now
    // in BOTH the staged and unstaged lists with different content on each side.
    let mut lines: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
    lines[1] = "line 2 STAGED".into();
    fs::write(path.join("multi.txt"), lines.join("\n") + "\n").unwrap();
    repo::stage_file(&path, "multi.txt").unwrap();
    lines[37] = "line 38 UNSTAGED".into();
    fs::write(path.join("multi.txt"), lines.join("\n") + "\n").unwrap();

    let (_model, diffs) = repo::load(&path).expect("load repo");
    let staged = diffs
        .get(&DiffKey::staged("multi.txt"))
        .expect("a staged-side diff exists");
    let worktree = diffs
        .get(&DiffKey::worktree("multi.txt"))
        .expect("a worktree-side diff exists");

    let has = |d: &git_client::model::FileDiff, needle: &str| {
        d.lines.iter().any(|l| l.content.contains(needle))
    };
    assert!(
        has(staged, "line 2 STAGED"),
        "staged diff shows the staged edit"
    );
    assert!(
        !has(staged, "line 38 UNSTAGED"),
        "staged diff excludes the unstaged edit"
    );
    assert!(
        has(worktree, "line 38 UNSTAGED"),
        "worktree diff shows the unstaged edit"
    );
    assert!(
        !has(worktree, "line 2 STAGED"),
        "worktree diff excludes the staged edit"
    );
}

fn app_discard_file_through_production_action() {
    let (_d, path) = init_repo();
    let original = fs::read_to_string(path.join("file.txt")).unwrap();
    fs::write(path.join("file.txt"), "line 1\nWILL DISCARD\nline 3\n").unwrap();
    let mut app = GitClientApp::open_at(&path).unwrap();

    // The tracked edit shows as unstaged; discard it via the app action.
    app.test_discard_file(ChangeArea::Unstaged, 0);
    assert_eq!(app.test_last_error(), None, "discard must not error");
    assert_eq!(
        fs::read_to_string(path.join("file.txt")).unwrap(),
        original,
        "app discard reverts the working tree",
    );
    // Model reloaded to a clean tree.
    assert_eq!(app.test_change_counts(), (0, 0, 0));
}

fn app_empty_state_opens_a_real_repository() {
    let (_d, path) = init_repo();
    let mut app = GitClientApp::open_empty();
    assert!(app.test_awaiting_repo(), "no repo bound → empty state");

    app.test_open_path(&path.to_string_lossy());
    assert!(
        !app.test_awaiting_repo(),
        "opening a valid path binds the repo"
    );
    assert_eq!(app.test_last_error(), None);
    // HEAD came from the real repo we opened.
    let head = open(&path).head().unwrap().shorthand().unwrap().to_string();
    assert_eq!(app.test_head(), head);
}

fn app_empty_state_rejects_a_non_repository_path() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = GitClientApp::open_empty();
    app.test_open_path(&dir.path().to_string_lossy());
    assert!(
        app.test_awaiting_repo(),
        "a non-repo path leaves the empty state up"
    );
    assert!(app.test_last_error().is_some(), "and surfaces an error");
}

fn app_opens_unicode_repository_and_commits_exact_unicode_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Grün 🌍");
    fs::create_dir(&path).unwrap();
    let repository = Repository::init(&path).unwrap();
    let mut config = repository.config().unwrap();
    config.set_str("user.name", "Unicode Test").unwrap();
    config
        .set_str("user.email", "unicode@example.invalid")
        .unwrap();
    let file = "Grün 🌍.txt";
    let contents = "Grün 🌍\nSecond line\n";
    fs::write(path.join(file), contents).unwrap();

    let mut app = GitClientApp::open_empty();
    app.test_open_path(path.to_str().unwrap());
    assert!(!app.test_awaiting_repo());
    app.test_stage_all();
    assert_eq!(app.test_last_error(), None);
    app.test_commit("Keep Grün 🌍");
    assert_eq!(app.test_last_error(), None);
    assert_eq!(app.test_change_counts(), (0, 0, 0));

    let reopened = Repository::open(&path).unwrap();
    let head = reopened.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.message(), Ok("Keep Grün 🌍"));
    let tree = head.tree().unwrap();
    let entry = tree.get_path(Path::new(file)).unwrap();
    let blob = reopened.find_blob(entry.id()).unwrap();
    assert_eq!(blob.content(), contents.as_bytes());
    assert_eq!(fs::read_to_string(path.join(file)).unwrap(), contents);
}

#[test]
fn untracked_files_preview_their_contents_before_staging() {
    use git_client::model::DiffKey;

    let (_dir, path) = init_repo();
    fs::create_dir(path.join("nested")).unwrap();
    fs::write(path.join("nested/new.rs"), "fn main() {}\n// new file\n").unwrap();

    let (model, diffs) = repo::load(&path).unwrap();
    let change = model
        .untracked
        .iter()
        .find(|change| change.path == "nested/new.rs")
        .unwrap();
    assert_eq!((change.additions, change.deletions), (2, 0));
    let diff = &diffs[&DiffKey::worktree("nested/new.rs")];
    assert!(diff.is_new);
    assert!(diff.lines.iter().any(|line| line.content == "fn main() {}"));
    assert!(diff.lines.iter().any(|line| line.content == "// new file"));
    assert!(!is_staged(&path, "nested/new.rs"));
}

// --- helpers ---------------------------------------------------------

/// Render a git2 diff to its full patch text for substring assertions.
fn diff_to_string(diff: &git2::Diff) -> String {
    let mut out = String::new();
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            out.push(line.origin());
        }
        out.push_str(std::str::from_utf8(line.content()).unwrap_or(""));
        true
    })
    .unwrap();
    out
}

/// Separate checkouts preserve the caller's HEAD and dirty files and use the
/// explicitly selected revision, even when created from a linked checkout.
#[test]
fn linked_worktrees_use_selected_revision_and_preserve_existing_work() {
    let (dir, path) = init_repo();
    let repository = open(&path);
    let original_head = repository.head().unwrap().name().unwrap().to_owned();
    let initial = repository.head().unwrap().target().unwrap();
    fs::write(path.join("file.txt"), "new revision\n").unwrap();
    repo::stage_file(&path, "file.txt").unwrap();
    repo::commit(&path, "second revision").unwrap();
    fs::write(path.join("file.txt"), "uncommitted work\n").unwrap();
    let destination = dir.path().join("agent-checkout");
    repo::create_worktree(&path, &destination, "codex/tile", &initial.to_string()).unwrap();
    let linked = open(&destination);
    assert_eq!(linked.head().unwrap().shorthand().unwrap(), "codex/tile");
    assert_eq!(linked.head().unwrap().target(), Some(initial));
    assert_eq!(
        repository.head().unwrap().name().unwrap(),
        original_head.as_str()
    );
    assert_eq!(
        fs::read_to_string(path.join("file.txt")).unwrap(),
        "uncommitted work\n"
    );
    assert_eq!(
        fs::read_to_string(destination.join("file.txt")).unwrap(),
        "line 1\nline 2\nline 3\n"
    );
    let rows = repo::list_worktrees(&destination).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().filter(|row| row.current).count(), 1);
    assert!(
        rows.iter()
            .any(|row| row.current && row.head == "codex/tile")
    );
    let second = dir.path().join("second-checkout");
    repo::create_worktree(&destination, &second, "codex/verify", "HEAD").unwrap();
    // HEAD is resolved in the initiating checkout, not the main checkout.
    assert_eq!(open(&second).head().unwrap().target(), Some(initial));
    assert_eq!(repo::list_worktrees(&path).unwrap().len(), 3);
}

/// Existing paths and branches, invalid revisions and relative destinations
/// fail without replacing files, creating branches, or silently succeeding.
#[test]
fn linked_worktrees_refuse_unsafe_destinations_and_invalid_inputs() {
    let (dir, path) = init_repo();
    let destination = dir.path().join("occupied");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("keep.txt"), "keep").unwrap();
    assert!(repo::create_worktree(&path, &destination, "codex/new", "HEAD").is_err());
    assert_eq!(
        fs::read_to_string(destination.join("keep.txt")).unwrap(),
        "keep"
    );
    let repository = open(&path);
    assert!(
        repository
            .find_branch("codex/new", git2::BranchType::Local)
            .is_err()
    );
    let fresh = dir.path().join("fresh");
    let branch = repository.head().unwrap().shorthand().unwrap().to_owned();
    assert!(repo::create_worktree(&path, &fresh, &branch, "HEAD").is_err());
    assert!(repo::create_worktree(&path, &fresh, "codex/new", "missing-revision").is_err());
    assert!(repo::create_worktree(&path, Path::new("relative"), "codex/new", "HEAD").is_err());
    assert!(!fresh.exists());
    assert!(
        repository
            .find_branch("codex/new", git2::BranchType::Local)
            .is_err()
    );
}

/// Worktree enumeration failure is an explicit separate state: the ordinary
/// repository, its current edits and an unborn checkout remain usable.
#[test]
fn linked_worktree_enumeration_errors_preserve_the_repository() {
    let empty = tempfile::tempdir().unwrap();
    Repository::init(empty.path()).unwrap();
    let (model, _) = repo::load(empty.path()).expect("unborn repository still opens");
    assert!(model.worktree_error.is_none());
    assert_eq!(model.worktrees.len(), 1);
    assert!(model.worktrees[0].current);

    let (dir, path) = init_repo();
    let linked = dir.path().join("missing-checkout");
    repo::create_worktree(&path, &linked, "codex/missing", "HEAD").unwrap();
    fs::remove_dir_all(&linked).unwrap();
    fs::write(path.join("file.txt"), "current edit\n").unwrap();
    assert!(repo::list_worktrees(&path).is_err());
    let (model, _) = repo::load(&path).expect("current checkout still opens");
    assert!(
        model.worktree_error.is_some(),
        "failure must not look like an empty list"
    );
    assert_eq!(model.commits.len(), 1);
    assert!(
        model
            .unstaged
            .iter()
            .any(|change| change.path == "file.txt")
    );
    assert!(GitClientApp::open_at(&path).is_some());

    let bare = tempfile::tempdir().unwrap();
    Repository::init_bare(bare.path()).unwrap();
    let (model, _) = repo::load(bare.path()).expect("bare repository still opens");
    assert!(
        model.worktree_error.is_some(),
        "bare checkout absence is explicit"
    );
}
