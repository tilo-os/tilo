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
    model::{Edits, OpenImage},
};
use ui::widget::Widget;

/// The device content store as these launches see it: one capability server
/// shared by every child process, reached through the descriptor the launcher
/// would hand the app, so a photograph stored by one process is read by the
/// next through the same scoped capability rather than a shared directory.
mod content_store {
    use runtime_fs::blobs::{
        BlobBackend, BlobError, BlobFuture, BlobPrincipal, BlobRef, BlobScope, ReadOutcome,
        spawn_blob_server, write_handoff,
    };
    use std::collections::HashMap;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Memory(Mutex<HashMap<(BlobScope, String), Vec<u8>>>);

    impl BlobBackend for Memory {
        fn put(
            self: Arc<Self>,
            scope: BlobScope,
            bytes: Vec<u8>,
            mime: String,
        ) -> BlobFuture<Result<BlobRef, BlobError>> {
            Box::pin(async move {
                let hash = tilo_content::hash_bytes(&bytes);
                let len = bytes.len() as u64;
                self.0.lock().unwrap().insert((scope, hash.clone()), bytes);
                Ok(BlobRef { hash, len, mime })
            })
        }
        fn read(
            self: Arc<Self>,
            scope: BlobScope,
            hash: String,
            offset: u64,
            max_len: u32,
        ) -> BlobFuture<ReadOutcome> {
            Box::pin(async move {
                let map = self.0.lock().unwrap();
                let Some(bytes) = map.get(&(scope, hash)) else {
                    return ReadOutcome::Unavailable;
                };
                let start = usize::try_from(offset).unwrap().min(bytes.len());
                let end = start.saturating_add(max_len as usize).min(bytes.len());
                let outcome = ReadOutcome::Ready {
                    bytes: bytes[start..end].to_vec(),
                    total_len: bytes.len() as u64,
                };
                drop(map);
                outcome
            })
        }
        fn has(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<bool> {
            Box::pin(async move { self.0.lock().unwrap().contains_key(&(scope, hash)) })
        }
        fn size(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<Option<u64>> {
            Box::pin(async move {
                self.0
                    .lock()
                    .unwrap()
                    .get(&(scope, hash))
                    .map(|bytes| bytes.len() as u64)
            })
        }
    }

    pub struct Server {
        socket: std::path::PathBuf,
        _dir: tempfile::TempDir,
        _runtime: tokio::runtime::Runtime,
    }

    impl Server {
        pub fn start() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("fs-blobs.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap();
            spawn_blob_server(
                listener,
                Arc::new(Memory::default()),
                runtime.handle().clone(),
            )
            .unwrap();
            Self {
                socket,
                _dir: dir,
                _runtime: runtime,
            }
        }

        /// Connect as the launcher does and hand the descriptor to `command`.
        /// The returned stream must outlive the spawn.
        pub fn attach(
            &self,
            command: &mut std::process::Command,
        ) -> std::os::unix::net::UnixStream {
            let mut stream = std::os::unix::net::UnixStream::connect(&self.socket).unwrap();
            let scope = BlobScope::new("photos-launch-room", "so.tilo.photos").unwrap();
            write_handoff(&mut stream, &BlobPrincipal::App(scope)).unwrap();
            let fd = stream.as_raw_fd();
            command.env("TILO_SVC_BLOBS_FD", fd.to_string());
            // SAFETY: only async-signal-safe fcntl runs between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            stream
        }
    }
}
fn bounded_output(command: &mut std::process::Command) -> std::process::Output {
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    let start = ui::time::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > tilo_test_env::hang_bound() {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "headless test child never exited within the hang bound: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
/// The `#[test]` each stage child re-runs, as the `it` harness names it (module path
/// included). With `--exact`, a stale name selects nothing and the child "passes" without
/// running its stage, so `child` also requires `CHILD_DONE` on the child's stdout.
const CHILD_TEST: &str = "framework_history_launch::actual_binary_saved_current_blank_history_restore_and_explicit_import";
const CHILD_DONE: &str = "photos-launch-stage-done";

fn child(store: &content_store::Server, stage: &str, dir: &std::path::Path, kind: &str) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env("TILO_PHOTOS_LAUNCH_STAGE", stage)
        .env("TILO_PHOTOS_LAUNCH_KIND", kind)
        .env("TILO_PHOTOS_TEST_HOME", dir)
        .env("XDG_DATA_HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env("TILO_PACKAGE_ID", "so.tilo.photos");
    let _capability = store.attach(&mut command);
    let out = bounded_output(&mut command);
    assert!(
        out.status.success(),
        "{stage}/{kind}: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&format!("{CHILD_DONE} {stage}")),
        "{stage}/{kind}: the child never ran its stage (is {CHILD_TEST} still the test name?): {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn binary(
    store: &content_store::Server,
    dir: &std::path::Path,
    actions: &serde_json::Value,
    import: bool,
) {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_photos"));
    let _capability = store.attach(&mut command);
    command
        .env("XDG_DATA_HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
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
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end launch scenario whose steps depend on each other in order"
)]
fn actual_binary_saved_current_blank_history_restore_and_explicit_import() {
    if let Ok(stage) = std::env::var("TILO_PHOTOS_LAUNCH_STAGE") {
        assert!(
            tilo_content::ContentStore::device().is_some(),
            "the child was handed the content-store capability"
        );
        let dir = std::path::PathBuf::from(std::env::var_os("TILO_PHOTOS_TEST_HOME").unwrap());
        let kind = std::env::var("TILO_PHOTOS_LAUNCH_KIND").unwrap();
        let (mut state, mut saver) = ui::persist::try_open("so.tilo.photos").unwrap();
        let rgba = photos::shots_pattern(6, 4);
        let bytes = photos::pixels::encode_png(&rgba).unwrap();
        // The same device content store the launched binary reads.
        let content = tilo_content::ContentStore::for_app();
        let hash = content.put(&bytes).unwrap();
        let image = OpenImage::from_path("same.png", 6, 4).with_content(hash);
        match stage.as_str() {
            "seed" => {
                state
                    .doc
                    .put(ROOT, "operational_receipt", "current receipt")
                    .unwrap();
                if kind == "current" {
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
                let image = image.with_content(content.put(&bytes).unwrap());
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
                let mut app = photos::PhotosApp::with_content(content);
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
                let mut app = photos::PhotosApp::with_content(content);
                app.sync(&mut state.doc, &ROOT);
                assert_eq!(
                    app.edited_pixels().unwrap().pixel(0, 0),
                    Some([0, 255, 0, 255])
                );
            }
            _ => panic!("unexpected test stage"),
        }
        println!("{CHILD_DONE} {stage}");
        return;
    }
    let store = content_store::Server::start();
    for kind in ["current", "blank"] {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        child(&store, "seed", dir, kind);
        binary(
            &store,
            dir,
            &serde_json::json!([{"click_named":"history-open"}]),
            false,
        );
        child(&store, "verify_unchanged", dir, kind);
        child(&store, "change", dir, kind);
        let selected = std::fs::read_to_string(dir.join("selected")).unwrap();
        let target = format!(
            "timeline-entry:{}",
            ui::widget::history_group::hex16(&ui::history_source::entry_id_from_hash(&selected))
        );
        binary(
            &store,
            dir,
            &serde_json::json!([{"click_named":"history-open"},{"perform":{"target":"activity_timeline","semantic_target":target,"action":"activate"}},{"click_named":"history-restore"},{"key":"Enter"}]),
            false,
        );
        child(&store, "verify_restored", dir, kind);
        binary(&store, dir, &serde_json::json!([]), true);
        child(&store, "verify_import", dir, kind);
    }
}
