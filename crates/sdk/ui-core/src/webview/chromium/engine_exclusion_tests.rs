use super::*;
use browser_continuity::engine_host::exclude_migration_from_engine;
use std::os::fd::AsRawFd;
use std::os::unix::fs::symlink;

const CHILD_ROOT: &str = "TILO_TEST_ENGINE_READ_EXCLUSION_ROOT";
#[test]
fn native_engine_command_cannot_read_migration_files_even_through_aliases() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        for denied in [
            root.join("migration/Bookmarks"),
            root.join("alias/Bookmarks"),
            PathBuf::from(std::env::var_os("TILO_TEST_ENGINE_READ_EXCLUSION_PROC").unwrap()),
        ] {
            if std::env::var_os("TILO_TEST_ENGINE_EXCLUSION_CONTROL").is_some() {
                assert_eq!(std::fs::read(denied).unwrap(), b"legacy private bookmarks");
            } else {
                let error =
                    std::fs::read(denied).expect_err("native child read the legacy projection");
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            }
        }
        assert_eq!(
            std::fs::read(root.join("allowed")).unwrap(),
            b"ordinary engine data"
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let denied = root.path().join("migration");
    std::fs::create_dir(&denied).unwrap();
    std::fs::write(denied.join("Bookmarks"), b"legacy private bookmarks").unwrap();
    std::fs::write(root.path().join("allowed"), b"ordinary engine data").unwrap();
    symlink(&denied, root.path().join("alias")).unwrap();
    let held = std::fs::File::open(&denied).unwrap();
    let executable = std::env::current_exe().unwrap();
    // This is the actual engine command factory and the exact child policy
    // attachment called by it; only the projection location is a test fixture.
    for confined in [false, true] {
        let mut command = host_command(&executable).unwrap();
        if confined {
            exclude_migration_from_engine(&mut command, &denied).unwrap();
        } else {
            command.env("TILO_TEST_ENGINE_EXCLUSION_CONTROL", "1");
        }
        let output = command.arg("--exact")
        .arg("webview::chromium::engine_exclusion_tests::native_engine_command_cannot_read_migration_files_even_through_aliases")
        .arg("--nocapture").env(CHILD_ROOT, root.path())
        .env("TILO_TEST_ENGINE_READ_EXCLUSION_PROC", format!("/proc/{}/fd/{}/Bookmarks", std::process::id(), held.as_raw_fd()))
        .output().unwrap();
        assert!(
            output.status.success(),
            "actual child confinement failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains("1 passed"),
            "the actual child test did not run: {stdout}"
        );
    }
    assert_eq!(
        std::fs::read(denied.join("Bookmarks")).unwrap(),
        b"legacy private bookmarks"
    );
}

#[test]
fn absent_projection_preserves_engine_spawn_and_redirected_projection_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let mut command = host_command(Path::new("/bin/sh")).unwrap();
    exclude_migration_from_engine(&mut command, &root.path().join("absent")).unwrap();
    let output = command
        .args(["-c", "printf engine-ready"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"engine-ready");
    let alias = root.path().join("alias");
    symlink(root.path(), &alias).unwrap();
    let mut redirected = host_command(Path::new("/bin/sh")).unwrap();
    assert!(exclude_migration_from_engine(&mut redirected, &alias).is_err());
}

#[test]
fn dedicated_service_factory_binds_import_and_confirms_child_exit_before_cleanup() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    struct ReapOnPanic(Arc<ChromiumEngine>);
    impl Drop for ReapOnPanic {
        fn drop(&mut self) {
            // The guard is declared after TempDir and therefore reaps first on
            // every assertion failure, preserving the live child's namespace.
            if self.0.runtime.service_session.is_some() {
                let _ = self.0.shutdown_service_session();
            } else {
                // Normal profiles deliberately refuse the dedicated public
                // stop operation. The fixture owner follows Runtime::drop's
                // lifecycle and waits for its actual supervisor before cleanup.
                self.0
                    .runtime
                    .process
                    .stopping
                    .store(true, Ordering::Release);
                let _ = self.0.tx.push(json!({"op":"shutdown"}));
                self.0.tx.close();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
                while !self.0.runtime.exited.load(Ordering::Acquire)
                    && std::time::Instant::now() < deadline
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let helper = root.path().join("protocol-host");
    std::fs::write(&helper, r#"#!/usr/bin/python3
import json,struct,sys,pathlib,os
profile=next(a[10:] for a in sys.argv[1:] if a.startswith('--profile='))
pathlib.Path(profile,'host-args.json').write_text(json.dumps(sys.argv[1:]))
def send(value):
    data=b'\x01'+json.dumps(value).encode()
    sys.stdout.buffer.write(struct.pack('<I',len(data))+data);sys.stdout.buffer.flush()
send({'event':'ready','cef_version':'fixture','chromium_version':'fixture'})
while True:
    prefix=sys.stdin.buffer.read(4)
    if not prefix: break
    count,=struct.unpack('<I',prefix)
    data=sys.stdin.buffer.read(count)
    command=json.loads(data[1:])
    if command['op']=='shutdown':
        pathlib.Path(profile,'child-exited').write_text('reaped fixture');break
    if command['op']=='service_session_import':
        send({'event':'service_session_result','tab':1,'request_id':command['request_id'],
            'session_id':command['session_id'],'service':command['service'],'imported':False,'cookie_count':0})
"#).unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let profile = root.path().join("selected");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&profile)
        .unwrap();
    let engine = ChromiumEngine::with_host_extensions(
        &helper,
        &profile,
        &[],
        None,
        Some("selected-slack-session"),
    )
    .unwrap();
    let _reap = ReapOnPanic(engine.clone());
    let args = std::fs::read_to_string(profile.join("host-args.json")).unwrap();
    assert!(args.contains("--service-session=selected-slack-session"));
    assert!(!args.contains("--private"));
    let mut request = super::tests::selected_service_request();
    request.session.session_id = "foreign-session".into();
    assert!(
        engine.import_service_session(request).is_err(),
        "foreign session reached native host"
    );
    engine
        .import_service_session(super::tests::selected_service_request())
        .unwrap();
    engine.shutdown_service_session().unwrap();
    assert_eq!(
        std::fs::read(profile.join("child-exited")).unwrap(),
        b"reaped fixture"
    );
    assert!(engine.runtime.exited.load(Ordering::Acquire));
    assert!(
        engine
            .import_service_session(super::tests::selected_service_request())
            .is_err()
    );
    // Same process confirmation remains idempotent, even while retained WebViews
    // still hold their engine handles. Files may now be removed by their owner.
    engine.shutdown_service_session().unwrap();
    let ordinary = root.path().join("ordinary");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&ordinary)
        .unwrap();
    let default =
        ChromiumEngine::with_host_extensions(&helper, &ordinary, &[], None, None).unwrap();
    let default_reap = ReapOnPanic(default.clone());
    assert!(
        default
            .import_service_session(super::tests::selected_service_request())
            .is_err()
    );
    assert!(
        default.shutdown_service_session().is_err(),
        "unrelated normal host could be stopped"
    );
    let exited = default.runtime.exited.clone();
    drop(default);
    drop(default_reap);
    tilo_test_env::wait_until("normal fixture child exit before removing profile", || {
        exited.load(Ordering::Acquire)
    });
}
