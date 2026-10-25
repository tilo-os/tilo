use super::*;

use tilo_environment::dir_grant;

fn granted(dir: &Path) -> Policy {
    Policy::new(vec![dir_grant(dir, true).unwrap()])
}

fn history(dir: &Path) -> Recoverable {
    Recoverable::at(dir.join("recoverable"))
}

/// Undo one invocation the way `undo file-action ID` does: from what the
/// history recorded, not from what the caller happens to have returned.
fn undo_id(id: &str, history: &Recoverable) {
    let entries = history
        .undo_set(id)
        .unwrap_or_else(|| panic!("{id}: no such action in the recoverable history"));
    undo(&entries, history).unwrap();
}

fn write(path: &Path, text: &str) {
    match path.parent() {
        Some(parent) if !parent.exists() => fs::create_dir_all(parent).unwrap(),
        _ => {}
    }
    fs::write(path, text).unwrap();
}

#[test]
fn a_removal_is_recoverable_and_puts_the_tree_back() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("a/one.txt"), "one");
    write(&root.join("a/two.txt"), "two");
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::Remove(RemoveOp {
        paths: vec![root.join("a")],
        recursive: true,
        force: false,
    });
    let outcome = perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert!(
        !root.join("a").exists(),
        "the path is gone from where it was"
    );
    assert_eq!(
        history.entries().len(),
        1,
        "the removal is one history entry"
    );
    assert_eq!(history.entries()[0].path, root.join("a"));

    undo_id(&outcome.id, &history);
    assert_eq!(fs::read_to_string(root.join("a/one.txt")).unwrap(), "one");
    assert_eq!(fs::read_to_string(root.join("a/two.txt")).unwrap(), "two");
}

#[test]
fn an_overwritten_copy_keeps_the_previous_version_and_undo_restores_it() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("new.txt"), "new");
    write(&root.join("old.txt"), "old");
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::Copy(CopyOp {
        sources: vec![root.join("new.txt")],
        destination: root.join("old.txt"),
        recursive: false,
        update: false,
        no_clobber: false,
    });
    let outcome = perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert_eq!(fs::read_to_string(root.join("old.txt")).unwrap(), "new");

    undo_id(&outcome.id, &history);
    assert_eq!(
        fs::read_to_string(root.join("old.txt")).unwrap(),
        "old",
        "undo puts the previous version back, not nothing"
    );
}

#[test]
fn a_move_keeps_both_ends_recoverable() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("a.txt"), "a");
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::Move(MoveOp {
        sources: vec![root.join("a.txt")],
        destination: root.join("b.txt"),
        no_clobber: false,
    });
    let outcome = perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert!(!root.join("a.txt").exists());
    assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "a");

    undo_id(&outcome.id, &history);
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "a");
    assert!(
        !root.join("b.txt").exists(),
        "undo takes back the copy it made"
    );
}

#[test]
fn truncate_keeps_what_it_shrinks() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("log"), "the whole log");
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::Truncate(TruncateOp {
        files: vec![root.join("log")],
        size: 0,
        create: true,
    });
    let outcome = perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert_eq!(fs::read(root.join("log")).unwrap(), Vec::<u8>::new());

    undo_id(&outcome.id, &history);
    assert_eq!(
        fs::read_to_string(root.join("log")).unwrap(),
        "the whole log"
    );
}

#[test]
fn an_append_adds_to_the_end_and_undo_puts_the_previous_text_back() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("today.md"), "# Today\n");
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::Append(AppendFileOp {
        files: vec![root.join("today.md")],
        text: "- bought milk\n".into(),
    });
    let outcome = perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("today.md")).unwrap(),
        "# Today\n- bought milk\n"
    );

    undo_id(&outcome.id, &history);
    assert_eq!(
        fs::read_to_string(root.join("today.md")).unwrap(),
        "# Today\n",
        "undo puts the text back as it was, not just the file"
    );
}

#[test]
fn an_append_to_a_file_that_is_not_there_makes_it() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    fs::create_dir_all(&root).unwrap();
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::Append(AppendFileOp {
        files: vec![root.join("new.md")],
        text: "first line\n".into(),
    });
    let outcome = perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("new.md")).unwrap(),
        "first line\n"
    );

    undo_id(&outcome.id, &history);
    assert!(!root.join("new.md").exists(), "nothing was there before");
}

#[test]
fn touch_keeps_the_text_a_file_already_has() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("draft.md"), "a paragraph I wrote");
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::Touch(AppendFileOp {
        files: vec![root.join("draft.md"), root.join("empty.md")],
        text: String::new(),
    });
    let outcome = perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("draft.md")).unwrap(),
        "a paragraph I wrote",
        "touch is not truncate: what is there stays"
    );
    assert_eq!(
        fs::read(root.join("empty.md")).unwrap(),
        Vec::<u8>::new(),
        "a missing file is created empty"
    );

    undo_id(&outcome.id, &history);
    assert!(!root.join("empty.md").exists(), "the made file is gone");
    assert_eq!(
        fs::read_to_string(root.join("draft.md")).unwrap(),
        "a paragraph I wrote"
    );
}

#[test]
fn touching_a_directory_is_refused_and_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    fs::create_dir_all(root.join("notes")).unwrap();
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::Touch(AppendFileOp {
        files: vec![root.join("notes")],
        text: String::new(),
    });
    let error = perform(&action, &policy, &history, Some(1_000)).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("{}/notes: is a directory", root.display())
    );
    assert!(root.join("notes").is_dir(), "nothing was written");
}

#[test]
fn a_write_outside_every_grant_is_refused_and_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let granted_dir = home.path().join("granted");
    let elsewhere = home.path().join("elsewhere");
    fs::create_dir_all(&granted_dir).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    write(&elsewhere.join("keep.txt"), "keep");
    let policy = granted(&granted_dir);
    let history = history(home.path());

    let action = FileAction::Remove(RemoveOp {
        paths: vec![elsewhere.join("keep.txt")],
        recursive: false,
        force: false,
    });
    let error = perform(&action, &policy, &history, Some(1_000)).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "{}: not granted to this environment; grant its directory with `perm trust-dir`",
            elsewhere.join("keep.txt").display()
        )
    );
    assert!(
        elsewhere.join("keep.txt").exists(),
        "the check runs before anything is written"
    );
    assert!(
        history.entries().is_empty(),
        "a refused action records nothing"
    );
}

#[test]
fn a_whole_invocation_is_checked_before_any_of_it_runs() {
    let home = tempfile::tempdir().unwrap();
    let granted_dir = home.path().join("granted");
    let outside = home.path().join("outside");
    fs::create_dir_all(&granted_dir).unwrap();
    write(&outside.join("secret.txt"), "secret");
    let policy = granted(&granted_dir);
    let history = history(home.path());

    // The first path is granted, the second is not: neither is removed.
    let action = FileAction::Remove(RemoveOp {
        paths: vec![granted_dir.join("gone.txt"), outside.join("secret.txt")],
        recursive: false,
        force: false,
    });
    write(&granted_dir.join("gone.txt"), "gone");
    assert!(perform(&action, &policy, &history, Some(1_000)).is_err());
    assert!(
        granted_dir.join("gone.txt").exists(),
        "one refused operand refuses the invocation, not just its own step"
    );
    assert!(outside.join("secret.txt").exists());
}

#[test]
fn a_new_file_may_be_created_in_a_granted_directory() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    fs::create_dir_all(&root).unwrap();
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::MakeDir(MakeDirOp {
        dirs: vec![root.join("deep/inside")],
        parents: true,
    });
    perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert!(root.join("deep/inside").is_dir());
    assert_eq!(
        history.entries().len(),
        2,
        "each directory that was made is its own history entry"
    );
}

#[test]
fn removing_a_directory_that_still_holds_something_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("full/file"), "x");
    fs::create_dir_all(root.join("empty")).unwrap();
    let policy = granted(&root);
    let history = history(home.path());

    let refused = FileAction::RemoveDir(RemoveDirOp {
        dirs: vec![root.join("full")],
        parents: false,
    });
    assert_eq!(
        perform(&refused, &policy, &history, Some(1_000))
            .unwrap_err()
            .to_string(),
        format!("{}: directory not empty", root.join("full").display())
    );
    let empty = FileAction::RemoveDir(RemoveDirOp {
        dirs: vec![root.join("empty")],
        parents: false,
    });
    let outcome = perform(&empty, &policy, &history, Some(1_000)).unwrap();
    assert!(!root.join("empty").exists());
    undo_id(&outcome.id, &history);
    assert!(
        root.join("empty").is_dir(),
        "undo makes the directory again"
    );
}

#[test]
fn a_symbolic_link_is_removed_by_undo_and_the_link_target_is_untouched() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("target.txt"), "kept");
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::Link(LinkOp {
        target: root.join("target.txt"),
        path: root.join("link.txt"),
        symbolic: true,
        force: true,
        into_directory: false,
    });
    let outcome = perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert!(root.join("link.txt").is_symlink());
    assert_eq!(
        fs::read_to_string(root.join("link.txt")).unwrap(),
        "kept",
        "the link reaches what it names"
    );

    undo_id(&outcome.id, &history);
    assert!(!root.join("link.txt").exists());
    assert!(root.join("target.txt").exists());
}

#[test]
fn a_forced_removal_is_named_irreversible_and_keeps_nothing() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("a"), "a");
    let policy = granted(&root);
    let history = history(home.path());

    let action = FileAction::Remove(RemoveOp {
        paths: vec![root.join("a")],
        recursive: false,
        force: true,
    });
    assert!(
        action.irreversible(),
        "a forced removal is what the permission warning warns about"
    );
    let outcome = perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert!(!root.join("a").exists());
    assert!(
        !history.entries().iter().any(|entry| entry.op == "keep"),
        "`-f` does not keep the path, so there is nothing to put back"
    );
    assert!(
        history.undo_set(&outcome.id).is_some_and(|entries| entries
            == vec![UndoEntry::Discarded {
                path: root.join("a")
            }]),
        "undoing it has nothing to put back, and says so"
    );
}

#[test]
fn undoing_an_invocation_takes_back_only_its_operations() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("a/one"), "one");
    write(&root.join("b/two"), "two");
    let policy = granted(&root);
    let history = history(home.path());

    let first = perform(
        &FileAction::Remove(RemoveOp {
            paths: vec![root.join("a")],
            recursive: true,
            force: false,
        }),
        &policy,
        &history,
        Some(1_000),
    )
    .unwrap();
    let second = perform(
        &FileAction::Remove(RemoveOp {
            paths: vec![root.join("b")],
            recursive: true,
            force: false,
        }),
        &policy,
        &history,
        Some(1_001),
    )
    .unwrap();
    assert_ne!(first.id, second.id, "two invocations, two ids");

    undo_id(&second.id, &history);
    assert!(root.join("b/two").exists(), "the second removal is back");
    assert!(
        !root.join("a").exists(),
        "undoing one action leaves the other alone"
    );
    undo_id(&first.id, &history);
    assert!(root.join("a/one").exists());
}

#[test]
fn an_invocation_is_undone_once() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("a"), "a");
    let policy = granted(&root);
    let history = history(home.path());

    let outcome = perform(
        &FileAction::Remove(RemoveOp {
            paths: vec![root.join("a")],
            recursive: false,
            force: false,
        }),
        &policy,
        &history,
        Some(1_000),
    )
    .unwrap();
    undo_id(&outcome.id, &history);
    assert!(root.join("a").exists());

    undo_id(&outcome.id, &history);
    assert_eq!(
        fs::read_to_string(root.join("a")).unwrap(),
        "a",
        "undoing twice is not a second removal followed by nothing"
    );
}

#[test]
fn undo_refuses_to_write_over_what_arrived_since() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("a"), "a");
    let policy = granted(&root);
    let history = history(home.path());

    let outcome = perform(
        &FileAction::Remove(RemoveOp {
            paths: vec![root.join("a")],
            recursive: false,
            force: false,
        }),
        &policy,
        &history,
        Some(1_000),
    )
    .unwrap();
    write(&root.join("a"), "written by something else");

    let entries = history.undo_set(&outcome.id).unwrap();
    let error = undo(&entries, &history).unwrap_err();
    assert_eq!(
        fs::read_to_string(root.join("a")).unwrap(),
        "written by something else",
        "undo reports the clash instead of overwriting"
    );
    assert!(
        error
            .to_string()
            .contains(&root.join("a").display().to_string())
    );
}

#[test]
fn an_unknown_invocation_id_undoes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let history = history(home.path());
    assert!(history.undo_set("nothing-recorded-this").is_none());
}

#[test]
fn a_copy_lands_inside_the_grant_and_outside_it_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let granted_dir = home.path().join("granted");
    let outside = home.path().join("outside");
    write(&outside.join("secret.txt"), "secret");
    fs::create_dir_all(&granted_dir).unwrap();
    let policy = granted(&granted_dir);

    // The first copy lands inside the grant, the second does not: nothing is
    // copied, so the invocation leaves no half a tree behind.
    let action = FileAction::Copy(CopyOp {
        sources: vec![outside.join("secret.txt")],
        destination: granted_dir.join("copy.txt"),
        recursive: false,
        update: false,
        no_clobber: false,
    });
    let history = history(home.path());
    perform(&action, &policy, &history, Some(1_000)).unwrap();
    assert!(granted_dir.join("copy.txt").exists());

    let refused = FileAction::Copy(CopyOp {
        sources: vec![outside.join("secret.txt")],
        destination: outside.join("elsewhere.txt"),
        recursive: false,
        update: false,
        no_clobber: false,
    });
    assert!(perform(&refused, &policy, &history, Some(1_001)).is_err());
    assert!(!outside.join("elsewhere.txt").exists());
}

#[test]
fn the_audit_line_names_the_verb_and_the_paths_once() {
    let action = FileAction::Truncate(TruncateOp {
        files: vec![
            PathBuf::from("/home/me/./log"),
            PathBuf::from("/home/me/log"),
        ],
        size: 0,
        create: false,
    });
    assert_eq!(action.verb(), "truncate");
    assert!(action.irreversible());
    assert_eq!(subjects(&action), "/home/me/log /home/me/log");
    let record = audit_record("s", "env", &action);
    assert_eq!(record.request, "truncate");
    assert!(record.allowed);
}

#[test]
fn the_broker_runs_a_file_action_inside_the_grant_and_names_its_undo() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = brokered_state(dir.path());
    let allowed = dir.path().join("allowed");
    let gone = allowed.join("note.txt");
    fs::write(&gone, "note").unwrap();

    let answer = state.handle(crate::broker::Request::FileAction {
        action: FileAction::Remove(RemoveOp {
            paths: vec![gone.clone()],
            recursive: false,
            force: false,
        }),
    });
    let crate::broker::Response::FileAction { id, operations } = answer else {
        panic!("the removal was refused: {answer:?}");
    };
    assert_eq!(operations, 1);
    assert!(!gone.exists(), "the file left the directory");
    assert!(
        history_in(&state.environment).is_dir(),
        "the history lives under the environment's own /var, not the guest's"
    );

    let undone = state.handle(crate::broker::Request::UndoFileAction { id: id.clone() });
    assert_eq!(
        undone,
        crate::broker::Response::Undone { id, operations: 1 },
        "undo answers with the id it undid"
    );
    assert_eq!(fs::read_to_string(&gone).unwrap(), "note");
}

#[test]
fn the_broker_denies_and_logs_a_file_action_outside_the_grant() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = brokered_state(dir.path());
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("secret.txt");
    fs::write(&secret, "secret").unwrap();

    let answer = state.handle(crate::broker::Request::FileAction {
        action: FileAction::Remove(RemoveOp {
            paths: vec![secret.clone()],
            recursive: false,
            force: false,
        }),
    });
    let crate::broker::Response::Denied { reason } = answer else {
        panic!("a path outside every grant was allowed: {answer:?}");
    };
    assert!(reason.contains("not granted"), "{reason}");
    assert!(secret.exists(), "nothing was written");
    let logged: Vec<_> = state
        .audit
        .read_all()
        .into_iter()
        .filter(|record| record.request == "remove")
        .collect();
    assert_eq!(logged.len(), 1, "the refusal is in the audit log");
    assert!(!logged[0].allowed);
}

#[test]
fn the_broker_denies_an_unknown_undo_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = brokered_state(dir.path());
    let answer = state.handle(crate::broker::Request::UndoFileAction {
        id: "never-recorded-this".into(),
    });
    assert!(
        matches!(answer, crate::broker::Response::Denied { .. }),
        "{answer:?}"
    );
}

/// A broker whose environment has been granted `<dir>/allowed` for writing,
/// through the same request the person's `perm trust-dir` sends.
fn brokered_state(dir: &Path) -> crate::broker::BrokerState {
    let mut state = crate::broker::tests::state(dir);
    let allowed = dir.join("allowed");
    fs::create_dir_all(&allowed).unwrap();
    let answer = state.handle(crate::broker::Request::TrustDir { path: allowed });
    assert!(
        matches!(
            answer,
            crate::broker::Response::Applied { .. } | crate::broker::Response::NeedsRestart { .. }
        ),
        "the grant did not take: {answer:?}"
    );
    state
}
