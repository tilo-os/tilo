//! Media coming into the bin: the chooser, a dropped file, a launch naming
//! files, and what the bin and the window bar say when a file is refused,
//! carries no frame rate, or has lost its bytes.

use std::path::{Path, PathBuf};

use ui::portal::{MultiOutcome, PortalOutcome};
use ui::prelude::*;
use ui::widget::toast::ToastKind;
use video_editor::VideoEditor;
use video_editor::actions::VideoEditorEffect;
use video_editor::model::MediaKind;
use video_editor::timecode::timecode;

const NOT_MEDIA: &[u8] = b"<!DOCTYPE html><html><body>nope</body></html>";

/// A scratch directory of real files, removed with the test.
fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// A real PNG header with known dimensions: the probe reads real bytes.
fn png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&w.to_be_bytes());
    bytes.extend_from_slice(&h.to_be_bytes());
    bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn not_media(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, NOT_MEDIA).unwrap();
    path
}

fn blank() -> HeadlessApp<VideoEditor> {
    let mut app = HeadlessApp::<VideoEditor>::new().unwrap();
    app.harness().resize((1280, 800)).unwrap();
    app
}

fn import(app: &mut HeadlessApp<VideoEditor>, path: &Path) {
    app.effect(&VideoEditorEffect::Import {
        path: Some(path.to_string_lossy().into_owned()),
    })
    .unwrap();
    app.settle();
}

fn bin_row_value(app: &mut HeadlessApp<VideoEditor>, name: &str) -> String {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| n.label() == Some(name) && n.role() == ui::accesskit::Role::ListItem)
        .and_then(|(_, n)| n.value().map(ToString::to_string))
        .unwrap_or_else(|| panic!("{name} has a bin row"))
}

#[test]
fn a_picked_file_lands_in_the_document_with_probed_metadata() {
    let dir = scratch();
    let still = png(dir.path(), "still.png", 480, 270);
    let mut app = blank();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    recorder.respond_to_pick_multiple(MultiOutcome::Selected(vec![still]));
    app.effect(&VideoEditorEffect::Import { path: None })
        .unwrap();
    app.settle();
    assert_eq!(recorder.pick_requests()[0].title(), "Import media");

    let project = app.state();
    assert_eq!(project.media.len(), 1);
    let m = &project.media[0];
    assert_eq!(m.name, "still.png");
    assert_eq!(m.kind, MediaKind::Image);
    assert_eq!((m.width, m.height), (480, 270));
    assert!(
        (m.duration - video_editor::probe::DEFAULT_IMAGE_DURATION).abs() < 1e-9,
        "a still gets the default duration"
    );
    assert!(
        m.fps.abs() < f64::EPSILON,
        "a still states no frame rate, not an invented 30"
    );
}

#[test]
fn a_dropped_file_imports_into_the_bin() {
    let dir = scratch();
    let still = png(dir.path(), "dropped.png", 200, 120);
    let mut app = blank();
    app.harness()
        .send(&ui::winit::event::WindowEvent::DroppedFile(still));
    app.settle();
    let project = app.state();
    assert_eq!(project.media.len(), 1);
    assert_eq!(project.media[0].width, 200);
}

#[test]
fn a_launch_naming_a_file_places_it_under_the_playhead() {
    let dir = scratch();
    let still = png(dir.path(), "launched.png", 640, 360);
    let mut app = HeadlessApp::<VideoEditor>::launched(&[still.to_str().unwrap()]).unwrap();
    app.settle();
    let project = app.state();
    assert_eq!(project.media.len(), 1);
    assert_eq!(project.tracks.len(), 1, "a track was made for it");
    assert_eq!(project.clips.len(), 1);
    let clip = &project.clips[0];
    let playhead = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, n)| n.label() == Some("Transport"))
        .and_then(|(_, n)| n.value().map(ToString::to_string))
        .unwrap();
    assert_ne!(
        playhead,
        timecode(clip.start, project.fps),
        "parked inside, not at the edge"
    );
    assert_ne!(playhead, timecode(clip.end(), project.fps));
}

#[test]
fn a_refused_file_is_named_with_the_probes_reason_as_a_failure() {
    let dir = scratch();
    let bad = not_media(dir.path(), "not-a-video.mp4");
    let mut app = blank();
    let refusal = app
        .effect(&VideoEditorEffect::Import {
            path: Some(bad.to_string_lossy().into_owned()),
        })
        .unwrap_err()
        .to_string();
    assert!(app.state().media.is_empty(), "nothing was added");
    assert!(refusal.contains("not-a-video.mp4"), "{refusal}");
    assert!(refusal.contains("not a media file"), "{refusal}");
    let notice = ui::window_toolbar::notice().expect("the refusal is said");
    assert_eq!(notice.kind, ToastKind::Error, "{}", notice.text);
}

#[test]
fn a_partly_refused_batch_names_what_it_refused_and_reads_as_a_failure() {
    let dir = scratch();
    let batch = vec![
        png(dir.path(), "a.png", 40, 12),
        not_media(dir.path(), "holiday.mp4"),
        png(dir.path(), "b.png", 41, 12),
        not_media(dir.path(), "notes.mov"),
    ];
    let mut app = blank();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    recorder.respond_to_pick_multiple(MultiOutcome::Selected(batch));
    let report = app
        .effect(&VideoEditorEffect::Import { path: None })
        .unwrap_err()
        .to_string();
    assert_eq!(app.state().media.len(), 2, "the two stills landed");
    for needle in [
        "Imported 2 of 4",
        "holiday.mp4",
        "notes.mov",
        "not a media file",
    ] {
        assert!(report.contains(needle), "{needle}: {report}");
    }
    let notice = ui::window_toolbar::notice().expect("the batch reports itself");
    assert_eq!(notice.kind, ToastKind::Error, "{}", notice.text);
}

#[test]
fn a_chooser_refusal_reads_as_a_failure_in_its_own_words() {
    // Severity is data: this wording matches none of the English a tint
    // could be guessed from.
    const REFUSAL: &str = "the file picker needs the real-desktop build (`--features portal`)";
    let mut app = blank();
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);

    recorder.respond_to_pick_multiple(MultiOutcome::Failed(REFUSAL.into()));
    let _ = app.effect(&VideoEditorEffect::Import { path: None });
    app.settle();
    let shown = ui::window_toolbar::notice().unwrap();
    assert_eq!(
        (shown.text.as_str(), shown.kind),
        (REFUSAL, ToastKind::Error)
    );

    recorder.respond_to_pick(PortalOutcome::Failed(REFUSAL.into()));
    let _ = app.effect(&VideoEditorEffect::OpenProject { path: None });
    app.settle();
    let shown = ui::window_toolbar::notice().unwrap();
    assert_eq!(
        (shown.text.as_str(), shown.kind),
        (REFUSAL, ToastKind::Error)
    );
}

#[test]
fn a_source_whose_bytes_are_gone_says_so_in_the_bin() {
    let dir = scratch();
    let still = png(dir.path(), "absent.png", 640, 360);
    let mut app = blank();
    import(&mut app, &still);
    assert!(!bin_row_value(&mut app, "absent.png").contains("not downloaded"));

    // The reference survives, the bytes do not: a project opened on a
    // second device.
    std::fs::remove_file(&still).unwrap();
    tilo_test_env::wait_until("the bin discloses the missing bytes", || {
        app.harness().idle();
        bin_row_value(&mut app, "absent.png").contains("not downloaded yet")
    });
}

#[test]
fn a_loaded_source_with_no_stated_length_reads_as_unknown() {
    // Opening a project is not an import, so no probe refuses its entries:
    // a source saved without metadata must not be read out as 0:00.
    let dir = scratch();
    let page = not_media(dir.path(), "page.mp4");
    let mut project = video_editor::Project::new("Loaded");
    project.media.push(video_editor::model::MediaItem {
        id: uuid::Uuid::new_v4(),
        name: "page.mp4".into(),
        path: page.to_string_lossy().into_owned(),
        ..video_editor::model::MediaItem::default()
    });
    let mut app = blank();
    app.invoke_confirmed(Invoke::edit(&video_editor::resource::load_project(&project)).unwrap())
        .unwrap();
    let value = bin_row_value(&mut app, "page.mp4");
    assert!(
        value.contains("unknown") && !value.contains("0:00"),
        "{value}"
    );
}
