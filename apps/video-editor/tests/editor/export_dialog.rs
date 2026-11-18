//! The export dialog as a person and a screen reader meet it: Ctrl+E opens
//! it, it says why it cannot run instead of offering a button it would
//! refuse, and a render keeps announcing itself after the dialog is gone.

use crate::hosted;

use hosted::*;
use ui::accesskit::Role;

fn dialog_open(app: &mut Editor) -> bool {
    node(app, Role::Dialog, "Export video").is_some()
}

/// How many render progress bars a reader is offered.
fn progress_bars(app: &mut Editor) -> usize {
    nodes(app, |n| n.label() == Some("Export progress")).len()
}

#[test]
fn ctrl_e_opens_the_dialog_and_escape_or_close_dismisses_it() {
    let mut app = sample_editor();
    assert!(!dialog_open(&mut app));
    chord(&mut app, "ctrl", "e");
    assert!(dialog_open(&mut app), "Ctrl+E opens it");
    app.harness().key("Escape");
    assert!(!dialog_open(&mut app), "Escape closes it");
    chord(&mut app, "ctrl", "e");
    press(&mut app, Role::Button, "Close export");
    assert!(!dialog_open(&mut app), "and so does its Close button");
}

#[test]
fn export_is_withheld_with_its_reason_while_the_footage_is_elsewhere() {
    // The demo's sources are not on this device, so nothing can be read.
    let mut app = sample_editor();
    chord(&mut app, "ctrl", "e");
    let (start, button) = node(&mut app, Role::Button, "Start export").unwrap();
    assert!(
        button.is_disabled(),
        "the dialog does not offer what it would refuse"
    );
    let (_, why) = node(&mut app, Role::Label, "Why export cannot run").unwrap();
    let reason = why.value().unwrap_or_default().to_string();
    assert!(
        reason.contains("Drone_Coastline.mov") && reason.contains("does not have"),
        "and says why: {reason}"
    );
    let _ = app
        .harness()
        .perform_a11y_action(start, &ui::a11y::Action::Activate);
    assert!(dialog_open(&mut app));
    assert!(
        progress_bars(&mut app) == 0,
        "pressing it anyway starts no render"
    );
}

#[test]
fn the_shortcut_sheet_keeps_ctrl_e_from_opening_export_under_it() {
    let mut app = sample_editor();
    app.harness().key("?");
    assert!(node(&mut app, Role::Dialog, "Keyboard shortcuts").is_some());
    chord(&mut app, "ctrl", "e");
    assert!(!dialog_open(&mut app), "nothing opens under the sheet");
    assert!(
        node(&mut app, Role::Dialog, "Keyboard shortcuts").is_some(),
        "the sheet stays"
    );
    app.harness().key("?");
    assert!(
        node(&mut app, Role::Dialog, "Keyboard shortcuts").is_none(),
        "? closes it"
    );
}

#[cfg(feature = "gst")]
mod media_stack {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/media_stack.rs"
    ));
}

/// A title `seconds` long: generated, so the cut has nothing to fetch.
fn titled_cut(seconds: f64) -> video_editor::Project {
    use video_editor::Project;
    use video_editor::model::{MediaItem, MediaKind, TextSource, Track, TrackKind};
    let mut project = Project::new("Tiny");
    project.set_frame_size(320, 180);
    let media = uuid::Uuid::new_v4();
    project.media.push(MediaItem {
        id: media,
        name: "Title".into(),
        kind: MediaKind::Text,
        duration: seconds,
        text: Some(TextSource::new("Hi")),
        ..MediaItem::default()
    });
    let track = uuid::Uuid::new_v4();
    project.tracks.push(Track {
        id: track,
        name: "V1".into(),
        kind: TrackKind::Video,
        index: 1,
        ..Track::default()
    });
    let clip = project.add_clip(track, media, 0.0).unwrap();
    project.clip_mut(clip).unwrap().out_point = seconds;
    project
}

/// Export asks where to save, and a person who cancels the chooser gets the
/// export answered rather than left waiting.
#[test]
#[cfg(feature = "gst")]
fn a_cancelled_chooser_answers_the_export() {
    let (services, host) = ui::process::ProcessServices::recording();
    ui::process::install_default(services);
    let mut app = editor_with(&titled_cut(1.0));
    let answer = app.effect(&video_editor::actions::VideoEditorEffect::Export { path: None });
    let refused = answer.expect_err("nothing was exported").to_string();
    assert!(refused.contains("no file was chosen"), "{refused}");
    let asked: Vec<String> = host
        .pick_requests()
        .iter()
        .map(|request| request.title().to_owned())
        .collect();
    assert_eq!(asked, ["Export video"]);
}

/// Export renders into the file the person chose, and answers once it is
/// there.
#[test]
#[cfg(feature = "gst")]
#[serial_test::serial(video_editor_gst)]
fn export_renders_into_the_chosen_file() {
    if !media_stack::media_stack() {
        return;
    }
    let (services, host) = ui::process::ProcessServices::recording();
    ui::process::install_default(services);
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("Tiny.mp4");
    host.respond_to_pick(ui::portal::PortalOutcome::Opened(file.clone()));
    let mut app = editor_with(&titled_cut(1.0));
    app.effect(&video_editor::actions::VideoEditorEffect::Export { path: None })
        .expect("the export answers once the file is written");
    let written = std::fs::metadata(&file).map_or(0, |meta| meta.len());
    assert!(written > 0, "the render went where the chooser said");
}

/// A render outlives its dialog: once dismissed, the progress stays on the
/// tree a reader hears, exactly once.
#[test]
#[cfg(feature = "gst")]
#[serial_test::serial(video_editor_gst)]
fn a_dismissed_render_is_still_announced() {
    if !media_stack::media_stack() {
        return;
    }
    let (services, host) = ui::process::ProcessServices::recording();
    ui::process::install_default(services);
    let out = tempfile::tempdir().unwrap();
    host.respond_to_pick(ui::portal::PortalOutcome::Opened(
        out.path().join("Tiny.mp4"),
    ));
    // It runs long enough that the render is still going when the dialog
    // is dismissed.
    let mut app = editor_with(&titled_cut(30.0));

    chord(&mut app, "ctrl", "e");
    press(&mut app, Role::Button, "Start export");
    tilo_test_env::wait_until("the render to start", || {
        app.settle();
        progress_bars(&mut app) > 0
    });
    app.harness().key("Escape");
    assert!(!dialog_open(&mut app), "the dialog is gone");
    assert_eq!(
        progress_bars(&mut app),
        1,
        "the render is announced once, not zero times"
    );
}
