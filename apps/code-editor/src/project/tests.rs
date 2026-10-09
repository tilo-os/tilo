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
