//! Project files through the hosted editor: the project is kept as it is
//! edited, a project file is exported from it and opened back in as one
//! undoable edit, and the failures a person sees. The host chooser is a
//! recording one, answered by each test.

use std::path::{Path, PathBuf};

use ui::host::{HostRecorder, HostServices};
use ui::portal::PortalOutcome;
use ui::prelude::*;
use video_editor::actions::VideoEditorEffect;
use video_editor::model::{Clip, MediaItem, MediaKind, Track};
use video_editor::resource::load_project;
use video_editor::seed::sample_project;
use video_editor::{Project, VideoEditor};

/// The demo project (seven clips, nothing saved), and the chooser that
/// answers it.
fn editor() -> (HeadlessApp<VideoEditor>, HostRecorder, ui::host::HostScope) {
    let (services, recorder) = HostServices::recording();
    let guard = ui::host::enter(&services);
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(&sample_project())).unwrap())
        .unwrap();
    (app, recorder, guard)
}

/// What the editor's own node tells a reader about the project.
fn spoken(app: &mut HeadlessApp<VideoEditor>) -> String {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| n.label() == Some("Video editor"))
        .and_then(|(_, n)| n.value().map(str::to_owned))
        .unwrap_or_default()
}

fn keys(app: &mut HeadlessApp<VideoEditor>, modifiers: &[&str], key: &str) {
    let modifiers: Vec<String> = modifiers.iter().map(|m| (*m).to_owned()).collect();
    app.harness().click_named("video_editor_root").unwrap();
    app.harness().set_modifiers(&modifiers);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
    app.settle();
}

fn export(app: &mut HeadlessApp<VideoEditor>, path: &Path) -> Result<(), String> {
    let exported = app.effect_confirmed(&VideoEditorEffect::ExportProjectFile {
        path: Some(path.display().to_string()),
    });
    app.settle();
    exported.map(drop).map_err(|error| error.to_string())
}

fn open(app: &mut HeadlessApp<VideoEditor>, path: &Path) -> Result<(), String> {
    let opened = app.effect(&VideoEditorEffect::OpenProjectFile {
        path: Some(path.display().to_string()),
    });
    app.settle();
    opened.map(drop).map_err(|error| error.to_string())
}

/// Write `project` to `path` as a project file.
fn write(path: &Path, project: &Project) {
    std::fs::write(path, video_editor::project_file::encode(project).unwrap()).unwrap();
}

/// An edit that changes the project: a marker at the playhead.
fn edit(app: &mut HeadlessApp<VideoEditor>) {
    keys(app, &[], "m");
}

fn chooser_titles(recorder: &HostRecorder) -> Vec<String> {
    recorder
        .pick_requests()
        .iter()
        .map(|r| r.title().to_owned())
        .collect()
}

/// A one-clip project named `name`, written to `path`.
fn small_project(path: &Path, name: &str) {
    let mut project = Project::new(name);
    let media = uuid::Uuid::new_v4();
    project.media.push(MediaItem {
        id: media,
        name: "One.mp4".into(),
        kind: MediaKind::Video,
        duration: 4.0,
        ..Default::default()
    });
    let track = uuid::Uuid::new_v4();
    project.tracks.push(Track {
        id: track,
        name: "V1".into(),
        index: 1,
        ..Default::default()
    });
    project.clips.push(Clip {
        id: uuid::Uuid::new_v4(),
        track,
        media,
        out_point: 4.0,
        ..Default::default()
    });
    write(path, &project);
}

fn notice_text() -> String {
    ui::window_toolbar::notice()
        .map(|n| n.text)
        .unwrap_or_default()
}

#[test]
fn an_exported_project_file_opens_back_whole_in_a_fresh_editor_as_one_undo_step() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Coastline.qvp");
    let (mut app, _recorder, _host) = editor();
    let clips = app.state().clips.len();
    export(&mut app, &path).unwrap();
    assert!(path.is_file());
    assert_eq!(notice_text(), "Exported Coastline");

    let mut fresh = HeadlessApp::<VideoEditor>::new().unwrap();
    assert!(fresh.state().clips.is_empty());
    let versions = fresh.versions().len();
    open(&mut fresh, &path).unwrap();
    assert_eq!(fresh.state().clips.len(), clips, "every clip came back");
    assert_eq!(fresh.versions().len(), versions + 1, "the open is one edit");
    assert!(fresh.undo());
    assert!(fresh.state().clips.is_empty(), "and it undoes");
}

#[test]
fn opening_a_second_project_replaces_the_first_instead_of_merging() {
    let dir = tempfile::tempdir().unwrap();
    let small = dir.path().join("Small.qvp");
    small_project(&small, "Small");
    let (mut app, _recorder, _host) = editor();
    open(&mut app, &small).unwrap();
    let project = app.state();
    assert_eq!((project.clips.len(), project.media.len()), (1, 1));
    assert_eq!(project.name, "Small");
}

#[test]
fn the_project_is_kept_as_it_is_edited_with_nothing_to_save() {
    let (mut app, _recorder, _host) = editor();
    let name = app.state().name;
    let versions = app.versions().len();
    edit(&mut app);
    assert!(app.versions().len() > versions, "the edit is kept at once");
    let said = spoken(&mut app);
    assert!(said.starts_with(&name) && !said.contains("saved"), "{said}");
}

#[test]
fn ctrl_o_opens_a_project_file_ctrl_shift_o_imports_and_ctrl_shift_s_exports() {
    let (mut app, recorder, _host) = editor();
    keys(&mut app, &["ctrl"], "o");
    keys(&mut app, &["ctrl", "shift"], "o");
    keys(&mut app, &["ctrl"], "s");
    keys(&mut app, &["ctrl", "shift"], "s");
    let titles = chooser_titles(&recorder);
    assert_eq!(titles.len(), 3, "Ctrl+S asks nothing: {titles:?}");
    assert_eq!(titles[0], "Open project file");
    assert_ne!(titles[1], "Open project file", "Ctrl+Shift+O imports media");
    assert_eq!(titles[2], "Export project file");
}

#[test]
fn a_chosen_destination_receives_the_export_and_opens_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Portal.qvp");
    let (mut app, recorder, _host) = editor();
    recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
    keys(&mut app, &["ctrl", "shift"], "s");
    assert!(path.is_file(), "the export went where the chooser said");

    let mut fresh = HeadlessApp::<VideoEditor>::new().unwrap();
    recorder.respond_to_pick(PortalOutcome::Opened(path));
    keys(&mut fresh, &["ctrl"], "o");
    assert_eq!(fresh.state().clips.len(), app.state().clips.len());
}

#[test]
fn opening_over_a_timeline_asks_nothing_and_undo_brings_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("Other.qvp");
    small_project(&other, "Other");
    let (mut app, recorder, _host) = editor();
    let before = app.state().clips.len();
    recorder.respond_to_pick(PortalOutcome::Opened(other));
    keys(&mut app, &["ctrl"], "o");
    assert_eq!(app.state().name, "Other", "it just opened");
    assert!(
        notice_text().ends_with("Undo to go back"),
        "{}",
        notice_text()
    );
    assert!(app.undo());
    assert_eq!(app.state().clips.len(), before);
}

#[test]
fn a_failed_export_says_so_and_changes_nothing() {
    let (mut app, _recorder, _host) = editor();
    let versions = app.versions().len();
    assert!(
        export(
            &mut app,
            &PathBuf::from("/definitely/not/a/directory/x.qvp")
        )
        .is_err()
    );
    let notice = ui::window_toolbar::notice().expect("the failure is in the bar");
    assert!(
        notice.text.starts_with("Could not export"),
        "{}",
        notice.text
    );
    assert_eq!(notice.kind, ui::widget::toast::ToastKind::Error);
    assert_eq!(app.versions().len(), versions);
}

#[test]
fn opening_what_is_not_a_project_reports_it_and_keeps_the_project() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("holiday.mp4");
    std::fs::write(&junk, b"not a project at all, just some bytes").unwrap();
    let (mut app, _recorder, _host) = editor();
    let before = app.state().clips.len();
    assert!(open(&mut app, &junk).is_err());
    assert_eq!(app.state().clips.len(), before);
    assert_eq!(notice_text(), "Not a video project file");
}

#[test]
fn opening_a_project_whose_footage_is_elsewhere_names_the_source() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_VE_MEDIA_DIR");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Moved.qvp");
    let mut project = Project::new("Moved");
    project.media.push(MediaItem {
        id: uuid::Uuid::new_v4(),
        name: "Drone.mov".into(),
        kind: MediaKind::Video,
        duration: 5.0,
        width: 1920,
        height: 1080,
        path: "/definitely/not/here-xyz.mov".into(),
        ..Default::default()
    });
    write(&path, &project);
    let (mut app, _recorder, _host) = editor();
    open(&mut app, &path).unwrap();
    let notice = ui::window_toolbar::notice().expect("the open reported something");
    assert!(
        notice.text.contains("Opened Moved") && notice.text.contains("Drone.mov"),
        "{}",
        notice.text
    );
    assert_eq!(notice.kind, ui::widget::toast::ToastKind::Warning);
}

#[test]
fn footage_beside_the_file_comes_in_with_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("take.mp4"), b"bytes").unwrap();
    let path = dir.path().join("Beside.qvp");
    let mut project = Project::new("Beside");
    project.media.push(MediaItem {
        id: uuid::Uuid::new_v4(),
        name: "take.mp4".into(),
        kind: MediaKind::Video,
        duration: 5.0,
        path: "take.mp4".into(),
        ..Default::default()
    });
    write(&path, &project);
    let (mut app, _recorder, _host) = editor();
    open(&mut app, &path).unwrap();
    assert_eq!(
        Path::new(&app.state().media[0].path),
        dir.path().join("take.mp4").as_path(),
        "the project holds where the footage is, not where the file was"
    );
    assert_eq!(notice_text(), "Opened Beside. Undo to go back");
}
