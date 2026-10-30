use super::*;

use tilo_environment::dir_grant;

fn granted(dir: &Path) -> Policy {
    Policy::new(vec![dir_grant(dir, true).unwrap()])
}

fn store(dir: &Path) -> Recoverable {
    Recoverable::at(dir.join("recoverable"))
}

/// Carry `action` out and take it back, the two halves of a task.
fn done(action: &FileAction, policy: &Policy, store: &Recoverable) -> Vec<UndoEntry> {
    perform(action, policy, store, &ContentStore::memory(), Some(1_000))
        .unwrap_or_else(|(error, _)| panic!("{} was refused: {error}", action.verb()))
}

fn refused(action: &FileAction, policy: &Policy, store: &Recoverable) -> String {
    match perform(action, policy, store, &ContentStore::memory(), Some(1_000)) {
        Ok(done) => panic!("{} was carried out: {done:?}", action.verb()),
        Err((error, partial)) => {
            assert!(partial.is_empty(), "a refusal does nothing first");
            error.to_string()
        }
    }
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
    let store = store(home.path());

    let entries = done(
        &FileAction::Remove(RemoveOp {
            paths: vec![root.join("a")],
            recursive: true,
            force: false,
        }),
        &policy,
        &store,
    );
    assert!(
        !root.join("a").exists(),
        "the path is gone from where it was"
    );
    let kept = store.kept();
    assert_eq!(kept.len(), 1, "the removal keeps one entry");
    assert_eq!(kept[0].path, root.join("a"));

    undo(&entries, &store).unwrap();
    assert_eq!(fs::read_to_string(root.join("a/one.txt")).unwrap(), "one");
    assert_eq!(fs::read_to_string(root.join("a/two.txt")).unwrap(), "two");
    assert!(store.kept().is_empty(), "what was put back is kept no more");
}

#[test]
fn a_forced_removal_is_still_kept_aside() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("a"), "a");
    let policy = granted(&root);
    let store = store(home.path());

    let entries = done(
        &FileAction::Remove(RemoveOp {
            paths: vec![root.join("a"), root.join("never-there")],
            recursive: false,
            force: true,
        }),
        &policy,
        &store,
    );
    assert!(!root.join("a").exists());
    assert_eq!(
        store.kept().len(),
        1,
        "`-f` silences a missing path; it does not unlink what is there"
    );
    undo(&entries, &store).unwrap();
    assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "a");
}

#[test]
fn an_overwritten_copy_keeps_the_previous_version_and_undo_restores_it() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("new.txt"), "new");
    write(&root.join("old.txt"), "old");
    let policy = granted(&root);
    let store = store(home.path());

    let entries = done(
        &FileAction::Copy(CopyOp {
            sources: vec![root.join("new.txt")],
            destination: root.join("old.txt"),
            ..CopyOp::default()
        }),
        &policy,
        &store,
    );
    assert_eq!(fs::read_to_string(root.join("old.txt")).unwrap(), "new");

    undo(&entries, &store).unwrap();
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
    let store = store(home.path());

    let entries = done(
        &FileAction::Move(MoveOp {
            sources: vec![root.join("a.txt")],
            destination: root.join("b.txt"),
            no_clobber: false,
        }),
        &policy,
        &store,
    );
    assert!(!root.join("a.txt").exists());
    assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "a");

    undo(&entries, &store).unwrap();
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
    let store = store(home.path());

    let entries = done(
        &FileAction::Truncate(TruncateOp {
            files: vec![root.join("log")],
            size: 0,
            create: true,
        }),
        &policy,
        &store,
    );
    assert_eq!(fs::read(root.join("log")).unwrap(), Vec::<u8>::new());

    undo(&entries, &store).unwrap();
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
    let store = store(home.path());

    let entries = done(
        &FileAction::Append(AppendFileOp {
            files: vec![root.join("today.md")],
            text: "- bought milk\n".into(),
            content: None,
        }),
        &policy,
        &store,
    );
    assert_eq!(
        fs::read_to_string(root.join("today.md")).unwrap(),
        "# Today\n- bought milk\n"
    );

    undo(&entries, &store).unwrap();
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
    let store = store(home.path());

    let entries = done(
        &FileAction::Append(AppendFileOp {
            files: vec![root.join("new.md")],
            text: "first line\n".into(),
            content: None,
        }),
        &policy,
        &store,
    );
    assert_eq!(
        fs::read_to_string(root.join("new.md")).unwrap(),
        "first line\n"
    );

    undo(&entries, &store).unwrap();
    assert!(!root.join("new.md").exists(), "nothing was there before");
}

#[test]
fn a_write_replaces_the_contents_and_undo_puts_the_previous_ones_back() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("a.txt"), "before\n");
    let policy = granted(&root);
    let store = store(home.path());

    let entries = done(
        &FileAction::Write(WriteOp::of(
            vec![root.join("a.txt")],
            b"hi\n".to_vec(),
            false,
        )),
        &policy,
        &store,
    );
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "hi\n");
    undo(&entries, &store).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("a.txt")).unwrap(),
        "before\n",
        "undo puts back what the write replaced"
    );

    // Bytes that are not text travel as base64 and arrive as they were.
    let bytes = vec![0_u8, 0xff, 0xfe, b'\n'];
    let op = WriteOp::of(vec![root.join("b.bin")], bytes.clone(), false);
    assert!(op.base64);
    let entries = done(&FileAction::Write(op), &policy, &store);
    assert_eq!(fs::read(root.join("b.bin")).unwrap(), bytes);
    undo(&entries, &store).unwrap();
    assert!(!root.join("b.bin").exists(), "nothing was there before");
}

#[test]
fn a_no_clobber_write_leaves_what_is_there_and_a_link_out_of_the_grant_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("kept.txt"), "kept\n");
    write(&home.path().join("outside.txt"), "outside\n");
    std::os::unix::fs::symlink(home.path().join("outside.txt"), root.join("link")).unwrap();
    let policy = granted(&root);
    let store = store(home.path());

    let error = refused(
        &FileAction::Write(WriteOp::of(
            vec![root.join("kept.txt")],
            b"x".to_vec(),
            true,
        )),
        &policy,
        &store,
    );
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(fs::read_to_string(root.join("kept.txt")).unwrap(), "kept\n");

    let error = refused(
        &FileAction::Write(WriteOp::of(vec![root.join("link")], b"x".to_vec(), false)),
        &policy,
        &store,
    );
    assert!(error.contains("not granted"), "{error}");
    assert_eq!(
        fs::read_to_string(home.path().join("outside.txt")).unwrap(),
        "outside\n",
        "a write through a link reaches only what the grants allow"
    );
}

#[test]
fn touch_keeps_the_text_a_file_already_has() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("draft.md"), "a paragraph I wrote");
    let policy = granted(&root);
    let store = store(home.path());

    let entries = done(
        &FileAction::Touch(AppendFileOp {
            files: vec![root.join("draft.md"), root.join("empty.md")],
            text: String::new(),
            content: None,
        }),
        &policy,
        &store,
    );
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

    undo(&entries, &store).unwrap();
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
    let store = store(home.path());

    let error = refused(
        &FileAction::Touch(AppendFileOp {
            files: vec![root.join("notes")],
            text: String::new(),
            content: None,
        }),
        &policy,
        &store,
    );
    assert_eq!(error, format!("{}/notes: is a directory", root.display()));
    assert!(root.join("notes").is_dir(), "nothing was written");
}

#[test]
fn a_write_outside_every_grant_is_refused_and_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let granted_dir = home.path().join("granted");
    let elsewhere = home.path().join("elsewhere");
    fs::create_dir_all(&granted_dir).unwrap();
    write(&elsewhere.join("keep.txt"), "keep");
    let policy = granted(&granted_dir);
    let store = store(home.path());

    let error = refused(
        &FileAction::Remove(RemoveOp {
            paths: vec![elsewhere.join("keep.txt")],
            ..RemoveOp::default()
        }),
        &policy,
        &store,
    );
    assert_eq!(
        error,
        format!(
            "{}: not granted to this environment; grant its directory with `perm trust-dir`",
            elsewhere.join("keep.txt").display()
        )
    );
    assert!(
        elsewhere.join("keep.txt").exists(),
        "the check runs before anything is written"
    );
    assert!(store.kept().is_empty(), "a refused action keeps nothing");
}

#[test]
fn a_whole_invocation_is_checked_before_any_of_it_runs() {
    let home = tempfile::tempdir().unwrap();
    let granted_dir = home.path().join("granted");
    let outside = home.path().join("outside");
    write(&granted_dir.join("gone.txt"), "gone");
    write(&outside.join("secret.txt"), "secret");
    let policy = granted(&granted_dir);
    let store = store(home.path());

    // The first path is granted, the second is not: neither is removed.
    refused(
        &FileAction::Remove(RemoveOp {
            paths: vec![granted_dir.join("gone.txt"), outside.join("secret.txt")],
            ..RemoveOp::default()
        }),
        &policy,
        &store,
    );
    assert!(
        granted_dir.join("gone.txt").exists(),
        "one refused operand refuses the invocation, not just its own step"
    );
    assert!(outside.join("secret.txt").exists());
}

#[test]
fn a_new_folder_may_be_made_in_a_granted_directory() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    fs::create_dir_all(&root).unwrap();
    let policy = granted(&root);
    let store = store(home.path());

    let entries = done(
        &FileAction::MakeDir(MakeDirOp {
            dirs: vec![root.join("deep/inside")],
            parents: true,
        }),
        &policy,
        &store,
    );
    assert!(root.join("deep/inside").is_dir());
    assert_eq!(
        entries.len(),
        2,
        "each directory that was made is its own step"
    );
    undo(&entries, &store).unwrap();
    assert!(!root.join("deep").exists(), "undo takes back exactly those");
}

#[test]
fn removing_a_directory_that_still_holds_something_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("full/file"), "x");
    fs::create_dir_all(root.join("empty")).unwrap();
    let policy = granted(&root);
    let store = store(home.path());

    assert_eq!(
        refused(
            &FileAction::RemoveDir(RemoveDirOp {
                dirs: vec![root.join("full")],
                parents: false,
            }),
            &policy,
            &store,
        ),
        format!("{}: directory not empty", root.join("full").display())
    );
    let entries = done(
        &FileAction::RemoveDir(RemoveDirOp {
            dirs: vec![root.join("empty")],
            parents: false,
        }),
        &policy,
        &store,
    );
    assert!(!root.join("empty").exists());
    undo(&entries, &store).unwrap();
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
    let store = store(home.path());

    let entries = done(
        &FileAction::Link(LinkOp {
            target: root.join("target.txt"),
            path: root.join("link.txt"),
            symbolic: true,
            force: true,
            into_directory: false,
        }),
        &policy,
        &store,
    );
    assert!(root.join("link.txt").is_symlink());
    assert_eq!(
        fs::read_to_string(root.join("link.txt")).unwrap(),
        "kept",
        "the link reaches what it names"
    );

    undo(&entries, &store).unwrap();
    assert!(!root.join("link.txt").exists());
    assert!(root.join("target.txt").exists());
}

#[test]
fn undo_refuses_to_write_over_what_arrived_since() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("src");
    write(&root.join("a"), "a");
    let policy = granted(&root);
    let store = store(home.path());

    let entries = done(
        &FileAction::Remove(RemoveOp {
            paths: vec![root.join("a")],
            ..RemoveOp::default()
        }),
        &policy,
        &store,
    );
    write(&root.join("a"), "written by something else");

    let error = undo(&entries, &store).unwrap_err();
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
fn a_source_outside_the_grant_is_never_read_into_it() {
    let home = tempfile::tempdir().unwrap();
    let granted_dir = home.path().join("granted");
    let outside = home.path().join("outside");
    write(&outside.join("secret.txt"), "secret");
    write(&granted_dir.join("mine.txt"), "mine");
    let policy = granted(&granted_dir);
    let store = store(home.path());

    // The runtime reads the host's filesystem, which is wider than what the
    // guest was given: a copy may not carry in what the guest cannot see,
    // and a move may not take away what it may not write.
    refused(
        &FileAction::Copy(CopyOp {
            sources: vec![outside.join("secret.txt")],
            destination: granted_dir.join("copy.txt"),
            ..CopyOp::default()
        }),
        &policy,
        &store,
    );
    assert!(!granted_dir.join("copy.txt").exists());
    refused(
        &FileAction::Move(MoveOp {
            sources: vec![outside.join("secret.txt")],
            destination: granted_dir.join("moved.txt"),
            no_clobber: false,
        }),
        &policy,
        &store,
    );
    assert!(outside.join("secret.txt").exists());
    refused(
        &FileAction::Link(LinkOp {
            target: outside.join("secret.txt"),
            path: granted_dir.join("hard.txt"),
            ..LinkOp::default()
        }),
        &policy,
        &store,
    );
    assert!(!granted_dir.join("hard.txt").exists());

    done(
        &FileAction::Copy(CopyOp {
            sources: vec![granted_dir.join("mine.txt")],
            destination: granted_dir.join("copy.txt"),
            ..CopyOp::default()
        }),
        &policy,
        &store,
    );
    assert!(granted_dir.join("copy.txt").exists());
}

#[test]
fn relative_operands_are_read_from_where_the_command_was_typed() {
    let cwd = Path::new("/srv/work/notes");
    let FileAction::Move(op) = anchored(
        FileAction::Move(MoveOp {
            sources: vec!["a.md".into(), "/abs/b.md".into()],
            destination: "archive/".into(),
            no_clobber: false,
        }),
        cwd,
    ) else {
        panic!("a move stays a move");
    };
    assert_eq!(
        op.sources,
        vec![PathBuf::from("/srv/work/notes/a.md"), "/abs/b.md".into()]
    );
    assert!(
        op.destination
            .to_string_lossy()
            .ends_with("/srv/work/notes/archive/"),
        "a trailing separator still says `a directory`: {}",
        op.destination.display()
    );
    let FileAction::Link(link) = anchored(
        FileAction::Link(LinkOp {
            target: "../shared/config".into(),
            path: "config".into(),
            symbolic: true,
            ..LinkOp::default()
        }),
        cwd,
    ) else {
        panic!("a link stays a link");
    };
    assert_eq!(
        link.target,
        PathBuf::from("../shared/config"),
        "a symbolic link's target is read from the link"
    );
    assert_eq!(link.path, PathBuf::from("/srv/work/notes/config"));
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
    assert_eq!(subjects(&action), "/home/me/log /home/me/log");
    let record = audit_record("s", "env", &action);
    assert_eq!(record.request, "truncate");
    assert!(record.allowed);
}

// The record's half: one action per command, attributed, in History, and
// undone from there.

/// The record's History entries for the verb `verb`.
pub(crate) fn entries_for(
    state: &crate::broker::BrokerState,
    verb: &str,
) -> Vec<tilo_actions::ActionContext> {
    state
        .environment
        .records
        .history(&state.environment.key)
        .unwrap()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|context| {
            matches!(
                &context.what,
                Some(tilo_actions::Did::Verb(call)) if call.action.action == verb
            )
        })
        .collect()
}

/// Undo the History entry `entry` the way History does: apply the
/// compensation it recorded, as the person, through the record's door. The
/// session's runtime then carries the revert out on this device.
pub(crate) fn undo_from_history(
    state: &crate::broker::BrokerState,
    entry: &tilo_actions::ActionContext,
) {
    let compensation = entry
        .compensation
        .as_ref()
        .expect("a file action records the compensation its undo applies");
    let undo: <Environment as tilo_resource::Resource>::Action =
        serde_json::from_slice(&compensation.payload).unwrap();
    state
        .environment
        .records
        .act(
            &state.environment.key,
            &Invoker::Person(state.person.clone()),
            |replica| {
                replica
                    .apply_action(&undo)
                    .map(drop)
                    .map_err(|e| RecordError::Refused(e.to_string()))
            },
        )
        .unwrap();
    carry_out_queued(state);
}

#[test]
fn mv_in_a_guest_is_one_history_entry_attributed_to_the_person_and_undone_from_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = brokered_state(dir.path());
    let allowed = dir.path().join("allowed");
    write(&allowed.join("a"), "a's text");

    let answer = state.handle(crate::broker::Request::FileAction {
        run: None,
        action: FileAction::Move(MoveOp {
            sources: vec![allowed.join("a")],
            destination: allowed.join("b"),
            no_clobber: false,
        }),
    });
    let crate::broker::Response::FileAction { id, operations } = answer else {
        panic!("the move was refused: {answer:?}");
    };
    assert_eq!(operations, 2, "the copy and the source kept aside");
    assert!(!allowed.join("a").exists());
    assert_eq!(fs::read_to_string(allowed.join("b")).unwrap(), "a's text");

    let moves = entries_for(&state, "move");
    assert_eq!(moves.len(), 1, "one command is one History entry");
    let invoker = moves[0]
        .invoker
        .as_ref()
        .expect("the entry names its actor");
    assert_eq!(invoker.actor, state.person);
    assert_eq!(invoker.kind, tilo_actions::ActorKind::Person);
    assert_eq!(moves[0].kind, Some(tilo_actions::ActionKind::Outside));
    let queued = state.environment.current();
    assert!(
        queued
            .files
            .get(&Id::from_raw(id))
            .is_some_and(|task| task.settled.as_ref().is_some_and(Settled::is_done)),
        "the answer names the record's entry, settled here"
    );

    undo_from_history(&state, &moves[0]);
    assert_eq!(
        fs::read_to_string(allowed.join("a")).unwrap(),
        "a's text",
        "undo from History puts it back"
    );
    assert!(!allowed.join("b").exists());
    assert_eq!(
        entries_for(&state, "move").len(),
        1,
        "undoing is not a second move"
    );
}

#[test]
fn an_agents_direct_move_is_the_same_action_attributed_to_the_agent_for_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let state = brokered_state(dir.path());
    let allowed = dir.path().join("allowed");
    write(&allowed.join("draft.md"), "draft");
    let paths = |names: &[&str]| -> Vec<String> {
        names
            .iter()
            .map(|name| allowed.join(name).display().to_string())
            .collect()
    };

    // No shell: the agent calls the published `move` with paths.
    let answer = invoke(
        &state.environment.records,
        &state.environment.key,
        &Invoker::Agent {
            agent: "agent-claude".into(),
            person: state.person.clone(),
        },
        None,
        Some(
            Environment::MOVE_PATHS.with(tilo_environment::environment::MovePaths {
                device: state.device.clone(),
                sources: paths(&["draft.md"]),
                destination: allowed.join("final.md").display().to_string(),
                no_clobber: None,
            }),
        ),
        &state.device,
        &Recoverable::at(store_in(&state.environment)),
        &content_in(&state.environment),
    )
    .unwrap();
    assert!(answer.settled.as_ref().is_some_and(Settled::is_done));
    assert_eq!(
        fs::read_to_string(allowed.join("final.md")).unwrap(),
        "draft"
    );

    let moves = entries_for(&state, "move");
    assert_eq!(moves.len(), 1);
    let invoker = moves[0].invoker.as_ref().unwrap();
    assert_eq!(invoker.actor, "agent-claude");
    assert_eq!(invoker.kind, tilo_actions::ActorKind::Agent);
    assert_eq!(invoker.on_behalf_of.as_deref(), Some(state.person.as_str()));

    undo_from_history(&state, &moves[0]);
    assert_eq!(
        fs::read_to_string(allowed.join("draft.md")).unwrap(),
        "draft"
    );
    assert!(!allowed.join("final.md").exists());
}

#[test]
fn a_forty_operand_copy_beyond_a_grant_is_refused_whole() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = brokered_state(dir.path());
    let allowed = dir.path().join("allowed");
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let sources: Vec<PathBuf> = (0..39)
        .map(|n| {
            let path = allowed.join(format!("{n:02}.txt"));
            write(&path, "x");
            path
        })
        .collect();

    let answer = state.handle(crate::broker::Request::FileAction {
        run: None,
        action: FileAction::Copy(CopyOp {
            sources: sources.clone(),
            destination: outside.clone(),
            ..CopyOp::default()
        }),
    });
    let crate::broker::Response::Denied { reason } = answer else {
        panic!("a copy beyond the grant was allowed: {answer:?}");
    };
    assert!(reason.contains("not granted"), "{reason}");
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "not one of the forty is copied"
    );
    assert!(
        state.environment.current().files.is_empty(),
        "the verb refused it: nothing was queued"
    );
    assert!(entries_for(&state, "copy").is_empty(), "nothing in History");

    // An agent asking the action directly gets the same answer.
    let direct = invoke(
        &state.environment.records,
        &state.environment.key,
        &Invoker::Agent {
            agent: "agent-claude".into(),
            person: state.person.clone(),
        },
        None,
        Some(Environment::file_invoke(
            &state.device,
            &FileAction::Copy(CopyOp {
                sources,
                destination: outside.clone(),
                ..CopyOp::default()
            }),
        )),
        &state.device,
        &Recoverable::at(store_in(&state.environment)),
        &content_in(&state.environment),
    );
    assert!(matches!(direct, Err(RecordError::Refused(_))), "{direct:?}");
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn rm_stays_recoverable_through_undo_from_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = brokered_state(dir.path());
    let allowed = dir.path().join("allowed");
    write(&allowed.join("notes/today.md"), "today");

    let answer = state.handle(crate::broker::Request::FileAction {
        run: None,
        action: FileAction::Remove(RemoveOp {
            paths: vec![allowed.join("notes")],
            recursive: true,
            force: true,
        }),
    });
    assert!(
        matches!(answer, crate::broker::Response::FileAction { .. }),
        "{answer:?}"
    );
    assert!(!allowed.join("notes").exists());
    let store = Recoverable::at(store_in(&state.environment));
    assert!(
        store.dir().starts_with(&state.environment.var_dir),
        "kept under the environment's own /var, not the guest's"
    );
    assert_eq!(store.kept().len(), 1, "`rm -rf` kept it aside");

    let removals = entries_for(&state, "remove");
    assert_eq!(removals.len(), 1);
    undo_from_history(&state, &removals[0]);
    assert_eq!(
        fs::read_to_string(allowed.join("notes/today.md")).unwrap(),
        "today"
    );
}

#[test]
fn what_an_older_runtime_kept_stays_listed_and_comes_back_through_the_action() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = brokered_state(dir.path());
    let allowed = dir.path().join("allowed");
    fs::create_dir_all(&allowed).unwrap();
    // An entry as the runtime wrote it while it kept its own history: one
    // slot per operation, the kept one holding the payload.
    let store = Recoverable::at(store_in(&state.environment));
    let slot = store.dir().join("1793051200-000");
    fs::create_dir_all(&slot).unwrap();
    fs::write(slot.join("payload"), "an old note").unwrap();
    fs::write(
        slot.join(ENTRY_FILE),
        serde_json::json!({
            "id": "1793051200-000",
            "action": "1793051200-000",
            "at": 1_793_051_200u64,
            "op": "keep",
            "path": allowed.join("old.md"),
            "file": slot,
        })
        .to_string(),
    )
    .unwrap();
    let other = store.dir().join("1793051200-001");
    fs::create_dir_all(&other).unwrap();
    fs::write(
        other.join(ENTRY_FILE),
        serde_json::json!({
            "id": "1793051200-001",
            "action": "1793051200-000",
            "at": 1_793_051_200u64,
            "op": "make-dir",
            "path": allowed.join("made"),
            "file": other,
        })
        .to_string(),
    )
    .unwrap();

    let kept = store.kept();
    assert_eq!(kept.len(), 1, "only what holds contents is listed");
    assert_eq!(kept[0].path, allowed.join("old.md"));

    let answer = state.handle(crate::broker::Request::FileAction {
        run: None,
        action: FileAction::Restore(RestoreOp {
            kept: "1793051200-000".into(),
        }),
    });
    assert!(
        matches!(answer, crate::broker::Response::FileAction { .. }),
        "{answer:?}"
    );
    assert_eq!(
        fs::read_to_string(allowed.join("old.md")).unwrap(),
        "an old note"
    );
    assert!(store.kept().is_empty());

    // The restore is an action like the others: undone from History, the
    // note goes back where it was kept, nothing deleted.
    let restores = entries_for(&state, "restore");
    assert_eq!(restores.len(), 1);
    undo_from_history(&state, &restores[0]);
    assert!(!allowed.join("old.md").exists());
    assert_eq!(store.kept().len(), 1);
    assert_eq!(
        fs::read_to_string(slot.join("payload")).unwrap(),
        "an old note"
    );
}

#[test]
fn the_broker_denies_and_logs_a_file_action_outside_the_grant() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = brokered_state(dir.path());
    let outside = dir.path().join("outside");
    let secret = outside.join("secret.txt");
    write(&secret, "secret");

    let answer = state.handle(crate::broker::Request::FileAction {
        run: None,
        action: FileAction::Remove(RemoveOp {
            paths: vec![secret.clone()],
            ..RemoveOp::default()
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
fn a_task_that_fails_on_the_tree_settles_failed_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = brokered_state(dir.path());
    let allowed = dir.path().join("allowed");

    let answer = state.handle(crate::broker::Request::FileAction {
        run: None,
        action: FileAction::Remove(RemoveOp {
            paths: vec![allowed.join("not-there")],
            ..RemoveOp::default()
        }),
    });
    let crate::broker::Response::Denied { reason } = answer else {
        panic!("removing nothing succeeded: {answer:?}");
    };
    assert!(reason.contains("no such file"), "{reason}");
    let env = state.environment.current();
    let (_, task) = env.files.entries().next().expect("the action was queued");
    assert!(
        task.settled.as_ref().and_then(Settled::reason).is_some(),
        "and settled with why it failed"
    );
}

/// A broker whose environment has been granted `<dir>/allowed` for writing,
/// through the same request the person's `perm trust-dir` sends.
pub(crate) fn brokered_state(dir: &Path) -> crate::broker::BrokerState {
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

/// Every file action one command line of the shell asks for (`find -delete`
/// removing three files, say) is stamped with that run, so History
/// attributes and undoes the run as one.
#[test]
fn the_file_actions_of_one_shell_run_are_grouped_as_that_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = brokered_state(dir.path());
    let allowed = dir.path().join("allowed");
    for name in ["a.tmp", "b.tmp"] {
        write(&allowed.join(name), "x");
    }
    let run = crate::broker::RunGroup {
        id: "ush-1-2-3".into(),
        label: "find . -name '*.tmp' -delete".into(),
    };
    for name in ["a.tmp", "b.tmp"] {
        let answer = state.handle(crate::broker::Request::FileAction {
            run: Some(run.clone()),
            action: FileAction::Remove(RemoveOp {
                paths: vec![allowed.join(name)],
                ..RemoveOp::default()
            }),
        });
        assert!(
            matches!(answer, crate::broker::Response::FileAction { .. }),
            "{answer:?}"
        );
    }
    let removes = entries_for(&state, "remove");
    assert_eq!(removes.len(), 2);
    for entry in &removes {
        let group = entry.group.as_ref().expect("the run's group");
        assert_eq!(group.id, run.id);
        assert_eq!(group.label, run.label);
        assert_eq!(group.kind, crate::broker::COMMAND_RUN);
    }
}

/// Carrying out what is queued reads the record, which opens its lock files
/// for writing; the watch must report the document only, or it wakes itself
/// forever.
#[test]
fn only_the_record_document_counts_as_a_change() {
    let dir = tempfile::tempdir().unwrap();
    assert!(watch::is_document(
        &dir.path().join(app_state::DOCUMENT_FILE)
    ));
    assert!(!watch::is_document(&dir.path().join("lock")));
    assert!(!watch::is_document(dir.path()));
}
