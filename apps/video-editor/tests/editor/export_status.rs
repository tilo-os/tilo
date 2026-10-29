//! What a person and a reader are told when an export cannot run or fails:
//! the dialog withdraws Export and says why before any chooser opens, and a
//! failure stays readable with the dialog closed and in its details.

use ui::accesskit::{Node, Role};
use ui::host::HostServices;
use ui::prelude::*;
use video_editor::actions::VideoEditorEffect;
use video_editor::resource::load_project;
use video_editor::seed::sample_project;
use video_editor::{Project, VideoEditor};

fn hosting(project: &Project) -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.invoke_confirmed(Invoke::edit(&load_project(project)).unwrap())
        .unwrap();
    app.harness().click_named("video_editor_root").unwrap();
    app
}

fn node(app: &mut HeadlessApp<VideoEditor>, name: &str) -> Option<Node> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| n.label() == Some(name))
        .map(|(_, n)| n)
}

fn said(app: &mut HeadlessApp<VideoEditor>, name: &str) -> String {
    node(app, name)
        .and_then(|n| n.value().map(str::to_owned))
        .unwrap_or_else(|| panic!("no {name} in the tree"))
}

fn ctrl_e(app: &mut HeadlessApp<VideoEditor>) {
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("e");
    app.harness().set_modifiers(&[]);
    app.settle();
}

/// One clip whose footage this device has never read.
fn unreadable() -> Project {
    let mut project = Project::new("Unreadable");
    let track = video_editor::model::Track {
        id: uuid::Uuid::new_v4(),
        name: "V1".into(),
        kind: video_editor::model::TrackKind::Video,
        index: 1,
        ..Default::default()
    };
    let media = video_editor::model::MediaItem {
        id: uuid::Uuid::new_v4(),
        name: "Drone.mov".into(),
        kind: video_editor::model::MediaKind::Video,
        duration: 5.0,
        width: 1920,
        height: 1080,
        path: "/definitely/not/here-xyz.mov".into(),
        ..Default::default()
    };
    project.clips.push(video_editor::model::Clip {
        id: uuid::Uuid::new_v4(),
        track: track.id,
        media: media.id,
        out_point: 5.0,
        ..Default::default()
    });
    project.tracks.push(track);
    project.media.push(media);
    project
}

#[test]
fn footage_that_is_not_here_withdraws_export_before_any_chooser() {
    let (services, recorder) = HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut app = hosting(&sample_project());
    ctrl_e(&mut app);
    assert!(node(&mut app, "Export video").is_some_and(|n| n.role() == Role::Dialog));
    let why = said(&mut app, "Why export cannot run");
    assert!(
        why.contains("Drone_Coastline"),
        "it names the source: {why}"
    );
    assert!(
        !why.contains("stand-in") && !why.contains("export"),
        "in the person's words, not the encoder's: {why}"
    );
    assert!(
        node(&mut app, "Start export").is_some_and(|n| n.is_disabled()),
        "the Export a reader reaches is the disabled one"
    );

    let start = app.harness().a11y_node_named("Start export").unwrap();
    let _ = app
        .harness()
        .perform_a11y_action(start, &ui::a11y::Action::Activate);
    let _ = app.effect(&VideoEditorEffect::Export { path: None });
    app.settle();
    assert!(
        recorder.pick_requests().is_empty(),
        "a blocked export never asks where to save"
    );
}

#[test]
fn a_failed_export_is_readable_closed_and_in_its_details() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = hosting(&unreadable());
    let _ = app.effect(&VideoEditorEffect::Export {
        path: Some(dir.path().join("never.mp4").display().to_string()),
    });
    app.settle();
    let reason = said(&mut app, "Export failed");
    assert!(
        reason.contains("Drone.mov"),
        "the failure names the source: {reason}"
    );
    ctrl_e(&mut app);
    assert_eq!(said(&mut app, "Export failure details"), reason);
    assert!(
        !dir.path().join("never.mp4").exists(),
        "no file was claimed"
    );
}
