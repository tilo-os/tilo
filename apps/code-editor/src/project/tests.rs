//! The project on its own: its verbs and field edits through the door the
//! host uses, its texts read on demand, and merging.

use super::*;
use ui::tilo_resource::{Invoke, Reader, Replica};

fn apply(project: &mut Replica<Project>, invoke: Invoke<Project>) -> Result<(), String> {
    project
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn act(project: &mut Replica<Project>, action: &crate::actions::Action) -> Result<(), String> {
    apply(
        project,
        Invoke::apply(action).map_err(|error| error.to_string())?,
    )
}

fn ids(project: &Replica<Project>) -> Vec<Id> {
    project.state().unwrap().files.ids().cloned().collect()
}

/// A new file named `name` in `folder`, holding `text`, and its id.
fn file(project: &mut Replica<Project>, name: &str, folder: Option<&Id>, text: &str) -> Id {
    let before = ids(project);
    act(
        project,
        &crate::actions::new_file_action(name, folder.map(Id::as_str), text),
    )
    .unwrap();
    ids(project)
        .into_iter()
        .find(|id| !before.contains(id))
        .expect("a new file")
}

fn folder(project: &mut Replica<Project>, name: &str, parent: Option<&Id>) -> Id {
    let before = ids(project);
    act(
        project,
        &crate::actions::new_folder_action(name, parent.map(Id::as_str)),
    )
    .unwrap();
    ids(project)
        .into_iter()
        .find(|id| !before.contains(id))
        .expect("a new folder")
}

fn text(project: &Replica<Project>, id: &Id) -> String {
    crate::doc::EditorDoc::read_buffer_key(project.doc(), id.as_str()).unwrap_or_default()
}

fn path(project: &Replica<Project>, id: &Id) -> String {
    project.state().unwrap().path_of(id).unwrap()
}

/// Files and folders are made, renamed and moved with names unique in
/// their folder, a folder never inside itself, and deleted once empty.
#[test]
fn files_and_folders_keep_their_tree_whole() {
    let mut project = Replica::<Project>::new();
    let src = folder(&mut project, "src", None);
    let main = file(&mut project, "main.rs", Some(&src), "fn main() {}\n");
    assert_eq!(path(&project, &main), "src/main.rs");
    assert_eq!(
        project.state().unwrap().files.get(&main).unwrap().language,
        "Rust",
        "a new file's language follows its name"
    );
    assert_eq!(text(&project, &main), "fn main() {}\n");
    assert!(
        act(
            &mut project,
            &crate::actions::new_file_action("MAIN.rs", Some(src.as_str()), "")
        )
        .is_err(),
        "names are unique in a folder, whatever their case"
    );

    act(
        &mut project,
        &crate::actions::rename_action(main.as_str(), "lib.rs"),
    )
    .unwrap();
    assert_eq!(path(&project, &main), "src/lib.rs");
    assert!(
        act(
            &mut project,
            &crate::actions::rename_action(main.as_str(), "a/b")
        )
        .is_err()
    );

    let nested = folder(&mut project, "nested", Some(&src));
    assert!(
        act(
            &mut project,
            &crate::actions::move_action(src.as_str(), Some(nested.as_str()), None)
        )
        .is_err(),
        "a folder cannot go inside itself"
    );
    act(
        &mut project,
        &crate::actions::move_action(main.as_str(), None, Some(src.as_str())),
    )
    .unwrap();
    assert_eq!(path(&project, &main), "lib.rs");
    assert_eq!(ids(&project)[0], main, "it went before the folder");

    assert!(
        project
            .apply_confirmed(Invoke::apply(&crate::actions::delete_action(src.as_str())).unwrap())
            .is_err(),
        "a folder is deleted once it is empty"
    );
    project
        .apply_confirmed(Invoke::apply(&crate::actions::delete_action(main.as_str())).unwrap())
        .unwrap();
    assert!(!project.state().unwrap().buffers.contains_key(main.as_str()));
    assert!(
        project
            .apply(Invoke::apply(&crate::actions::delete_action(nested.as_str())).unwrap())
            .is_err(),
        "deleting asks for confirmation"
    );
}

/// What the editor read from disk is recorded once: an entry already
/// there, or at a path a collaborator recorded, is left as it is; text
/// already there is never replaced by a disk read.
#[test]
fn disk_entries_and_texts_are_recorded_once() {
    let mut project = Replica::<Project>::new();
    let id = "00000000-0000-4000-8000-000000000001";
    let record = |id: &str| {
        crate::actions::record_entry_action(id, EntryKind::File, "main.rs", None, "Rust")
    };
    act(&mut project, &record(id)).unwrap();
    assert_eq!(ids(&project), [Id::from(id)]);
    assert!(
        !project.state().unwrap().buffers.contains_key(id),
        "a file's text is read when it is opened"
    );
    act(&mut project, &record(id)).unwrap();
    act(
        &mut project,
        &record("00000000-0000-4000-8000-000000000002"),
    )
    .unwrap();
    assert_eq!(ids(&project), [Id::from(id)], "one entry per path");

    act(
        &mut project,
        &crate::actions::record_text_action(id, "on disk"),
    )
    .unwrap();
    act(
        &mut project,
        &crate::actions::record_text_action(id, "read again"),
    )
    .unwrap();
    assert_eq!(text(&project, &Id::from(id)), "on disk");
    assert!(
        act(
            &mut project,
            &crate::actions::set_language_action(id, "Klingon")
        )
        .is_err(),
        "a language the editor does not know is refused"
    );
    act(
        &mut project,
        &crate::actions::set_language_action(id, "Plain Text"),
    )
    .unwrap();
    assert_eq!(
        project
            .state()
            .unwrap()
            .files
            .get(&Id::from(id))
            .unwrap()
            .language,
        "Plain Text"
    );
}

/// A contextual edit lands where it was made, even after a collaborator
/// changed the text elsewhere, and is refused where the place is gone or
/// ambiguous.
#[test]
fn contextual_edits_land_where_they_were_made_or_not_at_all() {
    let mut project = Replica::<Project>::new();
    let id = file(&mut project, "notes.txt", None, "START middle END");
    let made =
        crate::actions::text_edit_action(id.as_str(), "START middle END", "START repaired END");
    act(
        &mut project,
        &crate::actions::text_edit_action(
            id.as_str(),
            "START middle END",
            "prefix START middle END",
        ),
    )
    .unwrap();
    act(&mut project, &made).unwrap();
    assert_eq!(text(&project, &id), "prefix START repaired END");
    assert!(
        act(&mut project, &made).is_err(),
        "the place it was made is gone"
    );

    act(
        &mut project,
        &crate::actions::replace_text_action(id.as_str(), "L middle R / L middle R"),
    )
    .unwrap();
    let ambiguous = crate::actions::text_edit_action(id.as_str(), "L middle R", "L wrong R");
    assert!(act(&mut project, &ambiguous).is_err());
    assert_eq!(text(&project, &id), "L middle R / L middle R");

    let unicode = file(&mut project, "unicode.txt", None, "αβ😀γ");
    act(
        &mut project,
        &crate::actions::text_edit_action(unicode.as_str(), "αβ😀γ", "αX😀γ"),
    )
    .unwrap();
    assert_eq!(text(&project, &unicode), "αX😀γ");
}

/// A file never grows past what one file holds.
#[test]
fn an_edit_never_grows_a_file_past_its_limit() {
    let mut project = Replica::<Project>::new();
    let id = file(&mut project, "big.txt", None, "");
    let big = "x".repeat(MAX_TEXT_BYTES);
    act(
        &mut project,
        &crate::actions::replace_text_action(id.as_str(), big.clone()),
    )
    .unwrap();
    assert!(
        act(
            &mut project,
            &crate::actions::text_edit_action(id.as_str(), &big, &format!("{big}y"))
        )
        .is_err()
    );
    assert_eq!(text(&project, &id).len(), MAX_TEXT_BYTES);
}

/// Reading the project reads no text; the texts the views show are read,
/// follow every change and let go once nobody shows them, at whatever
/// revision the document is.
#[test]
fn only_the_texts_a_view_shows_are_read() {
    let mut project = Replica::<Project>::new();
    let first = file(&mut project, "first.rs", None, "first body");
    let other = file(&mut project, "other.rs", None, "other body");
    let history = project.heads();
    let mut reader = Reader::<Project>::read(project.doc()).unwrap();
    assert_eq!(reader.state().read_texts().0, 0);
    let show = |reader: &mut Reader<Project>, doc: &Doc, id: &Id| {
        reader
            .demand(doc, Demand::none().with_keys([id.to_string()]))
            .unwrap();
    };
    show(&mut reader, project.doc(), &first);
    assert_eq!(reader.state().text(first.as_str()), Some("first body"));
    assert_eq!(reader.state().read_texts().0, 1);
    show(&mut reader, project.doc(), &other);
    assert_eq!(reader.state().text(first.as_str()), None);
    assert_eq!(reader.state().text(other.as_str()), Some("other body"));

    act(
        &mut project,
        &crate::actions::text_edit_action(other.as_str(), "other body", "other body, edited"),
    )
    .unwrap();
    reader.sync(project.doc()).unwrap();
    assert_eq!(
        reader.state().text(other.as_str()),
        Some("other body, edited")
    );

    let past = project.doc_mut().fork_at(&history).unwrap();
    let mut then = Reader::<Project>::read(&past).unwrap();
    show(&mut then, &past, &other);
    assert_eq!(then.state().text(other.as_str()), Some("other body"));
}

/// Two people typing in one file, and making files at once, end with the
/// same project.
#[test]
fn two_replicas_converge() {
    let mut left = Replica::<Project>::new();
    let shared = file(&mut left, "main.rs", None, "fn main() {}\n");
    let mut right = left.fork();
    act(
        &mut left,
        &crate::actions::text_edit_action(
            shared.as_str(),
            "fn main() {}\n",
            "// head\nfn main() {}\n",
        ),
    )
    .unwrap();
    act(
        &mut right,
        &crate::actions::text_edit_action(
            shared.as_str(),
            "fn main() {}\n",
            "fn main() {}\n// tail\n",
        ),
    )
    .unwrap();
    let mine = file(&mut left, "left.rs", None, "");
    let theirs = file(&mut right, "right.rs", None, "");
    left.sync_with(&mut right).unwrap();
    assert_eq!(ids(&left), ids(&right));
    assert!(ids(&left).contains(&mine) && ids(&left).contains(&theirs));
    assert_eq!(text(&left, &shared), "// head\nfn main() {}\n// tail\n");
    assert_eq!(text(&left, &shared), text(&right, &shared));
}

/// The terminal's records land in the project's terminal panel.
#[test]
fn the_terminal_session_lives_in_one_panel_of_the_project() {
    let mut project = Replica::<Project>::new();
    let target = terminal::target::SessionTarget::default_environment().encode();
    let record = crate::actions::terminal(Terminal::RECORD_TARGET.with(target).build(0).unwrap());
    act(&mut project, &record).unwrap();
    let pane = "0b5c6a3e-2d4f-4c4b-9a7e-6f1d2c3b4a59";
    act(
        &mut project,
        &crate::actions::terminal(Terminal::CLAIM_PANE.with(pane.to_owned()).build(0).unwrap()),
    )
    .unwrap();
    let session = project.state().unwrap().terminal_panel.unwrap();
    assert!(session.session_target().is_some());
    assert_eq!(session.pane_id(), pane);
    assert!(
        act(
            &mut project,
            &crate::actions::terminal(
                Terminal::CLAIM_PANE
                    .with("not a pane".to_owned())
                    .build(0)
                    .unwrap()
            ),
        )
        .is_err()
    );
}

/// The terminal's runs land in its panel, and a command asked of it is
/// queued for the process running it, as in Terminal's own window.
#[test]
fn the_terminal_records_its_runs_and_queues_asked_commands() {
    let mut project = Replica::<Project>::new();
    assert!(
        act(
            &mut project,
            &crate::actions::terminal(Terminal::RUN.with("ls".to_owned()).build(0).unwrap()),
        )
        .is_err(),
        "nothing runs the terminal yet"
    );
    let pane = "0b5c6a3e-2d4f-4c4b-9a7e-6f1d2c3b4a59";
    act(
        &mut project,
        &crate::actions::terminal(Terminal::CLAIM_PANE.with(pane.to_owned()).build(0).unwrap()),
    )
    .unwrap();
    let finished = terminal::resource::Finished {
        command: "cargo test".into(),
        cwd: "/src".into(),
        exit: 101,
        run: Id::from("r1"),
        ..Default::default()
    };
    act(
        &mut project,
        &crate::actions::terminal(Terminal::RECORD_RUN.with(finished).build(0).unwrap()),
    )
    .unwrap();
    act(
        &mut project,
        &crate::actions::terminal(Terminal::RERUN.with(Id::from("r1")).build(0).unwrap()),
    )
    .unwrap();
    let state = project.state().unwrap();
    let session = state.terminal_panel.as_ref().unwrap();
    assert_eq!(session.runs.get(&Id::from("r1")).unwrap().exit, 101);
    let asked: Vec<(&str, &str)> = state
        .terminal_requests
        .pending()
        .map(|(_, request)| (request.command.0.as_str(), request.pane.0.as_str()))
        .collect();
    assert_eq!(asked, [("cargo test", pane)]);
}

/// Recording a file's text is one change of the verb: the deterministic text
/// object it starts from is made beside it, never by splitting the verb's
/// own writes into a change of their own, so a replica replaying the action
/// makes the same changes.
#[test]
fn recording_text_is_one_change_of_the_verb() {
    let mut project = Replica::<Project>::new();
    let id = "00000000-0000-4000-8000-000000000001";
    act(
        &mut project,
        &crate::actions::record_entry_action(id, EntryKind::File, "main.rs", None, "Rust"),
    )
    .unwrap();
    // A record from before texts were kept has no map for them yet: the
    // verb makes it, in its own change.
    {
        use automerge::transaction::Transactable as _;
        let doc = project.doc_mut();
        doc.delete(automerge::ROOT, "buffers").unwrap();
        doc.commit();
    }
    let mut cold = project.fork();
    // Read whole, as an action with no record kept from the last one does.
    ui::tilo_resource::__private::forget_all();
    let before = project.doc_mut().get_heads();
    act(
        &mut project,
        &crate::actions::record_text_action(id, "on disk"),
    )
    .unwrap();
    let structure = tilo_actions::FrameworkChange::Structure.change_message();
    let verbs = project
        .doc_mut()
        .get_changes(&before)
        .into_iter()
        .filter(|change| change.message() != Some(&structure))
        .count();
    assert_eq!(verbs, 1, "one change of the verb besides the slots");
    let envelope = project.log().last().unwrap().clone();
    cold.replay(&envelope).unwrap();
    assert_eq!(
        cold.doc_mut().get_heads().len(),
        project.doc_mut().get_heads().len()
    );
    let mut theirs = cold.doc_mut().get_heads();
    let mut ours = project.doc_mut().get_heads();
    theirs.sort();
    ours.sort();
    assert_eq!(theirs, ours, "the replay makes the same changes");
}

/// Replacing a file's text from the file writes only what differs, so a
/// collaborator's typing elsewhere in it survives; a folder has no text.
#[test]
fn replacing_a_files_text_keeps_a_collaborators_typing() {
    let mut left = Replica::<Project>::new();
    let id = file(&mut left, "main.rs", None, "fn main() {}\n");
    let src = folder(&mut left, "src", None);
    let mut right = left.fork();
    apply(
        &mut left,
        Project::REPLACE_TEXT.with(project::ReplaceText {
            file: id.clone(),
            text: "fn main() { run() }\n".into(),
        }),
    )
    .unwrap();
    act(
        &mut right,
        &crate::actions::text_edit_action(id.as_str(), "fn main() {}\n", "// head\nfn main() {}\n"),
    )
    .unwrap();
    left.sync_with(&mut right).unwrap();
    assert_eq!(text(&left, &id), "// head\nfn main() { run() }\n");
    assert!(
        apply(
            &mut left,
            Project::REPLACE_TEXT.with(project::ReplaceText {
                file: src,
                text: "x".into(),
            }),
        )
        .is_err()
    );
}

/// A range a person selected is replaced exactly, in characters, and the
/// rest of the file stays; a caret inserts; a place outside the text or on a
/// folder is refused.
#[test]
fn a_selected_range_is_replaced_exactly() {
    let mut project = Replica::<Project>::new();
    let id = file(&mut project, "main.rs", None, "let grüße = late();\n");
    let src = folder(&mut project, "src", None);
    let replace = |project: &mut Replica<Project>, at: tilo_resource::Place, text: &str| {
        apply(
            project,
            Project::REPLACE_RANGE.with(project::ReplaceRange {
                at,
                text: text.into(),
            }),
        )
    };
    replace(
        &mut project,
        tilo_resource::Place::Range {
            item: id.to_string(),
            start: 12,
            end: 16,
        },
        "early",
    )
    .unwrap();
    assert_eq!(text(&project, &id), "let grüße = early();\n");
    replace(
        &mut project,
        tilo_resource::Place::Caret {
            item: id.to_string(),
            at: 0,
        },
        "// greet\n",
    )
    .unwrap();
    assert_eq!(text(&project, &id), "// greet\nlet grüße = early();\n");
    let outside = tilo_resource::Place::Range {
        item: id.to_string(),
        start: 10,
        end: 999,
    };
    assert!(replace(&mut project, outside, "x").is_err());
    let folder = tilo_resource::Place::Caret {
        item: src.to_string(),
        at: 0,
    };
    assert!(replace(&mut project, folder, "x").is_err());
    assert_eq!(text(&project, &id), "// greet\nlet grüße = early();\n");
}
