//! Project files through the hosted editor: saving, opening, the unsaved
//! state the window and a reader are told, the question before an open
//! replaces work that exists nowhere else, and the failures a person sees.
//! The host chooser is a recording one, answered by each test.

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

fn press(app: &mut HeadlessApp<VideoEditor>, name: &str) {
    let node = app
        .harness()
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("no {name} in the tree"));
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
}

fn keys(app: &mut HeadlessApp<VideoEditor>, modifiers: &[&str], key: &str) {
    let modifiers: Vec<String> = modifiers.iter().map(|m| (*m).to_owned()).collect();
    app.harness().click_named("video_editor_root").unwrap();
    app.harness().set_modifiers(&modifiers);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
    app.settle();
}

fn save_as(app: &mut HeadlessApp<VideoEditor>, path: &Path) -> Result<(), String> {
    let saved = app.effect(&VideoEditorEffect::SaveAs {
        path: Some(path.display().to_string()),
    });
    app.settle();
    saved.map(drop).map_err(|error| error.to_string())
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
    video_editor::project_file::save(path, &project).unwrap();
}

fn notice_text() -> String {
    ui::window_toolbar::notice()
        .map(|n| n.text)
        .unwrap_or_default()
}

#[test]
fn a_saved_project_opens_back_whole_in_a_fresh_editor_as_one_undo_step() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Coastline.qvp");
    let (mut app, _recorder, _host) = editor();
    let clips = app.state().clips.len();
    save_as(&mut app, &path).unwrap();
    assert!(path.is_file());
    assert!(
        spoken(&mut app).contains("saved at"),
        "{}",
        spoken(&mut app)
    );

    let mut fresh = HeadlessApp::<VideoEditor>::new().unwrap();
    assert!(fresh.state().clips.is_empty());
    let versions = fresh.versions().len();
    fresh
        .effect(&VideoEditorEffect::OpenProject {
            path: Some(path.display().to_string()),
        })
        .unwrap();
    fresh.settle();
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
    save_as(&mut app, &dir.path().join("Current.qvp")).unwrap();
    app.effect(&VideoEditorEffect::OpenProject {
        path: Some(small.display().to_string()),
    })
    .unwrap();
    app.settle();
    let project = app.state();
    assert_eq!((project.clips.len(), project.media.len()), (1, 1));
    assert_eq!(project.name, "Small");
}

#[test]
fn the_unsaved_state_follows_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("D.qvp");
    let (mut app, _recorder, _host) = editor();
    assert!(spoken(&mut app).contains("not saved to a file yet"));
    save_as(&mut app, &path).unwrap();
    assert!(!spoken(&mut app).contains("unsaved"));

    // A shortcut's edit goes through the host, as a peer's would; the
    // project reads as unsaved again, and Ctrl+S writes straight back.
    edit(&mut app);
    assert!(
        spoken(&mut app).contains("unsaved changes"),
        "{}",
        spoken(&mut app)
    );
    keys(&mut app, &["ctrl"], "s");
    assert!(
        !spoken(&mut app).contains("unsaved"),
        "{}",
        spoken(&mut app)
    );
    let saved = video_editor::project_file::load(&path).unwrap();
    assert_eq!(
        saved.markers.len(),
        app.state().markers.len(),
        "the file holds the edit"
    );
}

#[test]
fn an_empty_never_saved_project_is_not_unsaved_work() {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    let said = spoken(&mut app);
    assert!(
        said.contains("empty project") && !said.contains("unsaved"),
        "{said}"
    );
}

#[test]
fn ctrl_o_opens_a_project_ctrl_shift_o_imports_and_save_as_always_asks() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, recorder, _host) = editor();
    keys(&mut app, &["ctrl"], "o");
    keys(&mut app, &["ctrl", "shift"], "o");
    save_as(&mut app, &dir.path().join("K.qvp")).unwrap();
    keys(&mut app, &["ctrl"], "s");
    keys(&mut app, &["ctrl", "shift"], "s");
    let titles = chooser_titles(&recorder);
    assert_eq!(
        titles.len(),
        3,
        "Ctrl+S on a saved project asks nothing: {titles:?}"
    );
    assert_eq!(titles[0], "Open project");
    assert_ne!(titles[1], "Open project", "Ctrl+Shift+O imports media");
    assert_eq!(titles[2], "Save project");
}

#[test]
fn a_chosen_destination_saves_there_and_opens_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Portal.qvp");
    let (mut app, recorder, _host) = editor();
    recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
    keys(&mut app, &["ctrl", "shift"], "s");
    assert!(path.is_file(), "the save went where the chooser said");

    let mut fresh = HeadlessApp::<VideoEditor>::new().unwrap();
    recorder.respond_to_pick(PortalOutcome::Opened(path));
    keys(&mut fresh, &["ctrl"], "o");
    assert_eq!(fresh.state().clips.len(), app.state().clips.len());
}

#[test]
fn opening_over_unsaved_work_asks_and_each_answer_does_what_it_says() {
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("Other.qvp");
    small_project(&other, "Other");
    let (mut app, recorder, _host) = editor();
    let before = app.state().clips.len();

    // Cancel: nothing changes.
    recorder.respond_to_pick(PortalOutcome::Opened(other.clone()));
    keys(&mut app, &["ctrl"], "o");
    assert!(
        app.harness().a11y_node_named("Open anyway").is_some(),
        "it stopped to ask"
    );
    assert_eq!(app.state().clips.len(), before, "and changed nothing yet");
    press(&mut app, "Cancel");
    assert!(app.harness().a11y_node_named("Open anyway").is_none());
    assert_eq!(app.state().clips.len(), before);

    // Save…: the work at risk is written, then the open goes through.
    let kept = dir.path().join("Kept.qvp");
    recorder.respond_to_pick(PortalOutcome::Opened(other));
    keys(&mut app, &["ctrl"], "o");
    recorder.respond_to_pick(PortalOutcome::Opened(kept.clone()));
    press(&mut app, "Save…");
    app.settle();
    assert_eq!(
        video_editor::project_file::load(&kept).unwrap().clips.len(),
        before,
        "the project that was at risk is on disk"
    );
    assert_eq!(app.state().name, "Other", "and the open went through");

    // A project with a file opens another without asking.
    let next = dir.path().join("Next.qvp");
    small_project(&next, "Next");
    recorder.respond_to_pick(PortalOutcome::Opened(next));
    keys(&mut app, &["ctrl"], "o");
    assert_eq!(app.state().name, "Next", "it just opened");
}

#[test]
fn open_anyway_replaces_unsaved_work_and_undo_brings_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("Other.qvp");
    small_project(&other, "Other");
    let (mut app, recorder, _host) = editor();
    let before = app.state().clips.len();
    recorder.respond_to_pick(PortalOutcome::Opened(other));
    keys(&mut app, &["ctrl"], "o");
    press(&mut app, "Open anyway");
    assert_eq!(app.state().name, "Other");
    assert!(app.undo());
    assert_eq!(app.state().clips.len(), before);
}

#[test]
fn a_failed_save_says_so_offers_try_again_and_claims_no_file() {
    let (mut app, _recorder, _host) = editor();
    assert!(
        save_as(
            &mut app,
            &PathBuf::from("/definitely/not/a/directory/x.qvp")
        )
        .is_err()
    );
    let notice = ui::window_toolbar::notice().expect("the failure is in the bar");
    assert!(notice.text.starts_with("Could not save"), "{}", notice.text);
    assert_eq!(notice.kind, ui::widget::toast::ToastKind::Error);
    assert!(
        notice.actions.iter().any(|a| a.name == "Try again"),
        "{:?}",
        notice.actions
    );
    assert!(spoken(&mut app).contains("not saved to a file yet"));
}

#[test]
fn opening_what_is_not_a_project_reports_it_and_keeps_the_project() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("holiday.mp4");
    std::fs::write(&junk, b"not a project at all, just some bytes").unwrap();
    let (mut app, _recorder, _host) = editor();
    save_as(&mut app, &dir.path().join("Cur.qvp")).unwrap();
    let before = app.state().clips.len();
    assert!(
        app.effect(&VideoEditorEffect::OpenProject {
            path: Some(junk.display().to_string()),
        })
        .is_err()
    );
    app.settle();
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
    video_editor::project_file::save(&path, &project).unwrap();
    let (mut app, _recorder, _host) = editor();
    save_as(&mut app, &dir.path().join("Cur.qvp")).unwrap();
    app.effect(&VideoEditorEffect::OpenProject {
        path: Some(path.display().to_string()),
    })
    .unwrap();
    app.settle();
    let notice = ui::window_toolbar::notice().expect("the open reported something");
    assert!(
        notice.text.contains("Opened Moved") && notice.text.contains("Drone.mov"),
        "{}",
        notice.text
    );
    assert_eq!(notice.kind, ui::widget::toast::ToastKind::Warning);
}
