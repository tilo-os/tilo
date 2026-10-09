//! The workspace on its own (verbs, field edits, merging, the published
//! tree) and as the app hosts it.

use super::*;
use ui::framework::testing::TestApp;
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{Edit, Replica, Resource as _};

/// The id file `id` is published under.
fn file_resource_id(id: &Id) -> String {
    item_resource_id::<Workspace>("files", id)
}

/// A `.typ` file in a fresh directory.
fn typ_file(name: &str, source: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, source).unwrap();
    (dir, path.to_str().unwrap().to_owned())
}

fn edit(workspace: &mut Replica<Workspace>, edit: &Edit<Workspace>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    workspace
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The id of the only file, or the first.
fn first(files: &Workspace) -> Id {
    files.files.ids().next().unwrap().clone()
}

fn only(app: &TestApp<TypstEditorShell>) -> TypstFile {
    let state = app.state();
    assert_eq!(state.files.len(), 1);
    state.files.values().next().unwrap().clone()
}

/// Creating, renaming, typing, saving, loading and closing: plain fields
/// through field edits, the rest through verbs.
#[test]
fn verbs_and_field_edits_change_one_file() {
    let mut workspace = Replica::<Workspace>::new();
    workspace
        .apply(new_file(" report.typ ", None, "= Report\n"))
        .unwrap();
    let state = workspace.state().unwrap();
    let id = first(&state);
    let created = state.file(&id).unwrap();
    assert_eq!(created.name.0, "report.typ", "the name is trimmed");
    assert_eq!(created.source, "= Report\n");
    assert!(created.path.is_none());

    let paths = Workspace::paths().files.item(&id);
    edit(&mut workspace, &paths.name.set(Atom("draft.typ".into()))).unwrap();
    edit(&mut workspace, &paths.source.splice(9, 0, "Body")).unwrap();
    assert!(
        edit(&mut workspace, &paths.path.set(Some(Atom("/x.typ".into())))).is_err(),
        "only saving and opening move a file"
    );
    workspace
        .apply(Workspace::SAVED_AT.with(workspace::SavedAt {
            file: id.clone(),
            path: "/docs/final.typ".into(),
        }))
        .unwrap();
    let saved = workspace.state().unwrap().file(&id).cloned().unwrap();
    assert_eq!(
        saved.name.0, "final.typ",
        "a saved file takes its path's name"
    );
    assert_eq!(saved.path, Some(Atom("/docs/final.typ".into())));
    assert_eq!(saved.source, "= Report\nBody");

    workspace
        .apply(Workspace::LOAD.with(workspace::Load {
            file: id.clone(),
            path: "/docs/other.typ".into(),
            source: "= Other\n".into(),
        }))
        .unwrap();
    let loaded = workspace.state().unwrap().file(&id).cloned().unwrap();
    assert_eq!(
        (loaded.name.0.as_str(), loaded.source.as_str()),
        ("other.typ", "= Other\n")
    );

    assert!(
        workspace
            .apply(Workspace::CLOSE_FILE.with(id.clone()))
            .is_err(),
        "closing asks for confirmation"
    );
    workspace
        .apply_confirmed(Workspace::CLOSE_FILE.with(id.clone()))
        .unwrap();
    assert!(workspace.state().unwrap().files.is_empty());
    assert!(
        workspace
            .apply(Workspace::SAVED_AT.with(workspace::SavedAt {
                file: id,
                path: "/gone.typ".into(),
            }))
            .is_err(),
        "a file that is gone is not saved"
    );
}

/// Two people typing in one file at once converge character by character;
/// a rename is written whole.
#[test]
fn concurrent_edits_converge() {
    let mut ours = Replica::<Workspace>::new();
    ours.apply(new_file("notes.typ", None, "= Notes\n"))
        .unwrap();
    let id = first(&ours.state().unwrap());
    let paths = Workspace::paths().files.item(&id);
    let mut theirs = ours.fork();

    edit(&mut theirs, &paths.source.splice(8, 0, "theirs\n")).unwrap();
    edit(&mut theirs, &paths.name.set(Atom("theirs.typ".into()))).unwrap();
    edit(&mut ours, &paths.source.splice(2, 5, "Minutes")).unwrap();

    ours.sync_with(&mut theirs).unwrap();
    assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
    let merged = ours.state().unwrap().file(&id).cloned().unwrap();
    assert_eq!(merged.source, "= Minutes\ntheirs\n");
    assert_eq!(merged.name.0, "theirs.typ");
}

#[test]
fn a_launched_file_opens_its_own_workspace_once() {
    let (_dir, path) = typ_file("notes.typ", "= Notes\n");
    let app = TestApp::<TypstEditorShell>::launched(&[&path]).unwrap();
    let record = app.record().clone();
    let file = only(&app);
    assert_eq!(file.name.0, "notes.typ");
    assert_eq!(file.path, Some(Atom(path.clone())));
    assert_eq!(file.source, "= Notes\n");

    let app = app.relaunch(&[&path]).unwrap();
    assert_eq!(app.record(), &record, "the file keeps its own workspace");
    assert_eq!(app.state().files.len(), 1, "a reopen adds nothing");
}

/// A window edits one document: it starts blank, and Open loads a file
/// into it rather than beside it.
#[test]
fn a_window_edits_one_document_that_open_replaces() {
    let mut app = TestApp::<TypstEditorShell>::new().unwrap();
    let blank = only(&app);
    assert_eq!(blank.name.0, "untitled.typ");
    assert!(
        blank.source.is_empty(),
        "a new document never contains sample text"
    );

    let (_dir, path) = typ_file("other.typ", "= Other\n");
    app.effect_confirmed(&crate::actions::TypstEffect::Open {
        path: Some(path.clone()),
    })
    .unwrap();
    app.settle();
    let opened = only(&app);
    assert_eq!(opened.name.0, "other.typ");
    assert_eq!(opened.path, Some(Atom(path)));
    assert_eq!(opened.source, "= Other\n");
    assert!(app.undo(), "opening is one undo step");
    assert_eq!(only(&app), blank);
}

#[test]
fn saving_to_a_path_through_the_effect_writes_the_shown_source() {
    let (dir, path) = typ_file("notes.typ", "= Notes\n");
    let mut app = TestApp::<TypstEditorShell>::launched(&[&path]).unwrap();
    let destination = dir.path().join("saved.typ");
    app.effect_confirmed(&crate::actions::TypstEffect::Save {
        path: Some(destination.display().to_string()),
    })
    .unwrap();
    app.settle();
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "= Notes\n");
    let saved = only(&app);
    assert_eq!(saved.name.0, "saved.typ");
    assert_eq!(saved.path, Some(Atom(destination.display().to_string())));
}

#[test]
fn a_files_source_is_a_field_every_edit_an_undoable_splice() {
    let (_dir, path) = typ_file("notes.typ", "= Notes\n");
    let mut app = TestApp::<TypstEditorShell>::launched(&[&path]).unwrap();
    let file = first(&app.state());
    app.invoke(new_file("other.typ", None, "= Other\n"))
        .unwrap();

    let source = Workspace::paths().files.item(&file).source;
    app.invoke(Invoke::edit(&source.splice(8, 0, "Body")).unwrap())
        .unwrap();
    let state = app.state();
    assert_eq!(state.file(&file).unwrap().source, "= Notes\nBody");
    assert!(
        state
            .files
            .iter()
            .any(|(id, other)| id != &file && other.source == "= Other\n"),
        "another file's text is its own"
    );
    let inspection = app.harness().inspection_json().unwrap();
    assert!(
        inspection.contains("Body"),
        "the editor shows it: {inspection}"
    );

    assert!(app.undo());
    assert_eq!(app.state().file(&file).unwrap().source, "= Notes\n");
}

#[test]
#[ignore = "Edit::coalesces ends a word step before a space instead of after it"]
fn typing_splices_the_open_file_and_undoes_a_word_at_a_time() {
    let (_dir, path) = typ_file("notes.typ", "");
    let mut app = TestApp::<TypstEditorShell>::launched(&[&path]).unwrap();
    app.harness().type_text("hello world");
    assert_eq!(only(&app).source, "hello world");
    assert!(app.undo());
    assert_eq!(only(&app).source, "hello ");
    app.set_role(ui::Role::Viewer);
    app.harness().type_text("!");
    assert_eq!(only(&app).source, "hello ", "a viewer types nothing");
}

/// An agent reaches each file and the app's operations through the one
/// published tree: each change is one call.
#[test]
fn an_agent_edits_the_workspace_one_call_each() {
    let (_dir, path) = typ_file("notes.typ", "= Notes\n");
    let mut app = TestApp::<TypstEditorShell>::launched(&[&path]).unwrap();
    let file = first(&app.state());
    let surface = app.agent_surface().to_string();
    for needle in [
        "new-file",
        "close-file",
        "saved-at",
        "\"load\"",
        "field-edit",
        "\"open\"",
        "\"save\"",
        "export-pdf",
        "compile",
        "zoom-in",
        file_resource_id(&file).as_str(),
    ] {
        assert!(surface.contains(needle), "missing {needle} in {surface}");
    }

    let paths = Workspace::paths().files.item(&file);
    for edit in [
        paths.name.set(Atom("renamed.typ".into())),
        paths.source.replace("= Renamed\n"),
    ] {
        app.call("field-edit", serde_json::json!({ "edit": edit }))
            .unwrap();
    }
    let renamed = only(&app);
    assert_eq!(renamed.name.0, "renamed.typ");
    assert_eq!(renamed.source, "= Renamed\n");

    app.call(
        "new-file",
        serde_json::json!({ "name": "appendix.typ", "source": "= Appendix\n" }),
    )
    .unwrap();
    assert_eq!(app.state().files.len(), 2);

    app.call_on(
        &file_resource_id(&file),
        "close-file",
        serde_json::json!({}),
    )
    .expect_err("closing asks for confirmation");
    app.call_confirmed_on(
        &file_resource_id(&file),
        "close-file",
        serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(only(&app).name.0, "appendix.typ");
}

/// What an agent does for a person is history the person reads: the
/// app's sentence for each field and verb, naming the file as it was
/// called.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let (_dir, path) = typ_file("notes.typ", "= Notes\n");
    let mut app = TestApp::<TypstEditorShell>::launched(&[&path]).unwrap();
    let file = first(&app.state());
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let item = file_resource_id(&file);
    let paths = Workspace::paths().files.item(&file);
    for edit in [
        paths.source.splice(0, 0, "#set page(width: 10cm)\n"),
        paths.name.set(Atom("report.typ".into())),
    ] {
        app.call_by(
            agent.clone(),
            Workspace::KIND,
            "field-edit",
            serde_json::json!({ "edit": edit }),
        )
        .unwrap();
    }
    app.call_by(
        agent.clone(),
        &item,
        "saved-at",
        serde_json::json!({ "path": "/docs/final.typ" }),
    )
    .unwrap();
    app.call_by(
        agent,
        Workspace::KIND,
        "new-file",
        serde_json::json!({ "name": "appendix.typ" }),
    )
    .unwrap();

    let english = LOCALES
        .iter()
        .find(|(locale, _)| *locale == "en-US")
        .map(|(_, source)| *source)
        .unwrap();
    let catalogue = tilo_history::Catalogue::new("en-US", &[english]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let entries: Vec<_> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .collect();
    let sentences: Vec<String> = entries.iter().map(|entry| reader.sentence(entry)).collect();
    assert_eq!(
        sentences,
        [
            "Edited notes.typ",
            "Renamed notes.typ to report.typ",
            "Saved report.typ as /docs/final.typ",
            "Created appendix.typ",
        ]
    );
}

#[test]
fn a_capture_shows_the_examples_with_their_problems_as_live_rows() {
    let mut app = TestApp::<TypstEditorShell>::capture(Some("errors")).unwrap();
    let state = app.state();
    assert_eq!(state.files.len(), 1, "a window edits one document");
    assert_eq!(
        state.file(&sample_id(DEFAULT_INDEX)).unwrap().source,
        crate::samples::ERROR_WARNING
    );
    let surface = app.agent_surface().to_string();
    assert!(surface.contains("typst.diagnostic"), "{surface}");
}
