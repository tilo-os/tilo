//! Actual binary launch tests, always in child processes with isolated storage.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use photos::{
    doc::PhotosDoc,
    model::{Edits, LibraryPhoto, OpenImage, PhotoLibrary},
};
use ui::widget::Widget;
fn bounded_output(command: &mut std::process::Command) -> std::process::Output {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > std::time::Duration::from_secs(45) {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "headless test child timed out: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
fn child(stage: &str, dir: &std::path::Path, kind: &str) {
    let out = bounded_output(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "actual_binary_saved_current_legacy_blank_history_restore_and_explicit_import",
                "--nocapture",
            ])
            .env("QOS_PHOTOS_LAUNCH_STAGE", stage)
            .env("QOS_PHOTOS_LAUNCH_KIND", kind)
            .env("QOS_PHOTOS_TEST_HOME", dir)
            .env("XDG_DATA_HOME", dir)
            .env("XDG_CONFIG_HOME", dir)
            .env("TILO_BLOB_STORE", dir.join("blobs"))
            .env("TILO_PACKAGE_ID", "so.tilo.photos"),
    );
    assert!(
        out.status.success(),
        "{stage}/{kind}: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn binary(dir: &std::path::Path, actions: serde_json::Value, import: bool) {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_photos"));
    command
        .env("XDG_DATA_HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env("TILO_BLOB_STORE", dir.join("blobs"))
        .env("TILO_PACKAGE_ID", "so.tilo.photos")
        .args([
            "--headless-drive",
            &serde_json::json!({"actions":actions,"targets":dir.join("targets.json")}).to_string(),
        ]);
    if import {
        command.arg("--open").arg(dir.join("explicit.png"));
    }
    let out = bounded_output(&mut command);
    assert!(
        out.status.success(),
        "actual main: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        std::fs::read_to_string(dir.join("targets.json"))
            .unwrap()
            .contains(if import {
                "history-open"
            } else {
                "activity_timeline"
            })
    );
}
#[test]
fn actual_binary_saved_current_legacy_blank_history_restore_and_explicit_import() {
    if let Ok(stage) = std::env::var("QOS_PHOTOS_LAUNCH_STAGE") {
        let dir = std::path::PathBuf::from(std::env::var_os("QOS_PHOTOS_TEST_HOME").unwrap());
        let kind = std::env::var("QOS_PHOTOS_LAUNCH_KIND").unwrap();
        let (mut state, mut saver) = ui::persist::try_open("so.tilo.photos").unwrap();
        let rgba = photos::shots_pattern(6, 4);
        let bytes = photos::pixels::encode_png(&rgba).unwrap();
        let hash = qdoc::blobstore::put(&bytes).unwrap();
        let image = OpenImage::from_path("same.png", 6, 4).with_content(hash);
        match stage.as_str() {
            "seed" => {
                state
                    .doc
                    .put(ROOT, "operational_receipt", "current receipt")
                    .unwrap();
                if kind == "legacy" {
                    state
                        .doc
                        .put(ROOT, "image", serde_json::to_string(&Some(&image)).unwrap())
                        .unwrap();
                    state
                        .doc
                        .put(
                            ROOT,
                            "edits",
                            serde_json::to_string(&Edits::default().rotated_right()).unwrap(),
                        )
                        .unwrap();
                    state
                        .doc
                        .put(
                            ROOT,
                            "library",
                            serde_json::to_string(&PhotoLibrary {
                                photos: vec![LibraryPhoto::from_image(image)],
                                albums: vec![],
                            })
                            .unwrap(),
                        )
                        .unwrap();
                } else if kind == "current" {
                    PhotosDoc::set_image(&mut state.doc, Some(&image));
                    PhotosDoc::set_edits(&mut state.doc, &Edits::default().rotated_right());
                }
                let heads = state.doc.get_heads();
                std::fs::write(dir.join("selected"), heads[0].to_string()).unwrap();
                std::fs::write(dir.join("selected.am"), state.doc.save()).unwrap();
                saver.request(&mut state.doc);
                saver.try_flush().unwrap();
            }
            "verify_unchanged" => {
                let expected =
                    AutoCommit::load(&std::fs::read(dir.join("selected.am")).unwrap()).unwrap();
                assert_eq!(state.doc.base_heads(), expected.base_heads());
                let heads = state.doc.get_heads();
                photos::app_root().sync(&mut state.doc, &ROOT);
                assert_eq!(state.doc.get_heads(), heads);
            }
            "change" => {
                let different =
                    photos::pixels::Rgba::new(6, 4, [0, 255, 0, 255].repeat(24)).unwrap();
                let bytes = photos::pixels::encode_png(&different).unwrap();
                std::fs::write(dir.join("explicit.png"), &bytes).unwrap();
                let image = image.with_content(qdoc::blobstore::put(&bytes).unwrap());
                PhotosDoc::set_image(&mut state.doc, Some(&image));
                let heads = state.doc.get_heads();
                std::fs::write(dir.join("live"), heads[0].to_string()).unwrap();
                saver.request(&mut state.doc);
                saver.try_flush().unwrap();
            }
            "verify_restored" => {
                let selected: automerge::ChangeHash = std::fs::read_to_string(dir.join("selected"))
                    .unwrap()
                    .parse()
                    .unwrap();
                let live: automerge::ChangeHash = std::fs::read_to_string(dir.join("live"))
                    .unwrap()
                    .parse()
                    .unwrap();
                assert_eq!(state.doc.get_changes(&[live]).len(), 1);
                assert!(state.doc.get_change_by_hash(&selected).is_some());
                let target = state.doc.fork_at(&[selected]).unwrap();
                let heads = state.doc.get_heads();
                let plan = ui::history_mode::restore::prepare_content_restore(
                    &state.doc,
                    &photos::framework_history::SCHEMA,
                    &heads,
                    &[selected],
                )
                .unwrap();
                let current_plan = ui::history_mode::restore::prepare_content_restore(
                    &state.doc,
                    &photos::framework_history::SCHEMA,
                    &heads,
                    &heads,
                )
                .unwrap();
                assert_eq!(plan.target_projection(), current_plan.target_projection());
                assert_eq!(PhotosDoc::image(&state.doc), PhotosDoc::image(&target));
                let mut app = photos::PhotosApp::new();
                app.sync(&mut state.doc, &ROOT);
                assert_eq!(state.doc.get_heads(), heads);
                if kind == "blank" {
                    assert!(app.edited_pixels().is_none());
                } else {
                    assert_eq!(
                        app.edited_pixels(),
                        Some(&photos::pixels::render(&rgba, Edits::default().crop, 1))
                    );
                }
                if kind == "legacy" {
                    assert!(state.doc.get(ROOT, "edits").unwrap().is_some());
                    assert!(state.doc.get(ROOT, "photos_edits").unwrap().is_none());
                }
                assert_eq!(
                    state
                        .doc
                        .get(ROOT, "operational_receipt")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str(),
                    Some("current receipt")
                );
            }
            "verify_import" => {
                let image = PhotosDoc::image(&state.doc).unwrap();
                assert_eq!(image.name, "explicit.png");
                let mut app = photos::PhotosApp::new();
                app.sync(&mut state.doc, &ROOT);
                assert_eq!(
                    app.edited_pixels().unwrap().pixel(0, 0),
                    Some([0, 255, 0, 255])
                );
            }
            _ => panic!("unexpected test stage"),
        }
        return;
    }
    for kind in ["current", "legacy", "blank"] {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        child("seed", dir, kind);
        binary(
            dir,
            serde_json::json!([{"click_named":"history-open"}]),
            false,
        );
        child("verify_unchanged", dir, kind);
        child("change", dir, kind);
        let selected = std::fs::read_to_string(dir.join("selected")).unwrap();
        let target = format!(
            "timeline-entry:{}",
            ui::widget::history_group::hex16(&ui::history_source::entry_id_from_hash(&selected))
        );
        binary(
            dir,
            serde_json::json!([{"click_named":"history-open"},{"perform":{"target":"activity_timeline","semantic_target":target,"action":"activate"}},{"click_named":"history-restore"},{"key":"Enter"}]),
            false,
        );
        child("verify_restored", dir, kind);
        binary(dir, serde_json::json!([]), true);
        child("verify_import", dir, kind);
    }
}
