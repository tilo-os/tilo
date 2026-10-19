//! Actual signed shim/worker, native-byte Directory/File projection and DAC.
use super::*;
use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
const SELECTOR: &str = "capture_service::mounted_source::signed_original_mounted_roots_project_before_caller_drop_and_close_all_inputs";
struct ReadonlyBind(std::ffi::CString);
impl ReadonlyBind {
    fn new(path: &Path) -> Self {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::mount(
                    path.as_ptr(),
                    path.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            },
            0
        );
        assert_eq!(
            unsafe {
                libc::mount(
                    std::ptr::null(),
                    path.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                    std::ptr::null(),
                )
            },
            0
        );
        Self(path)
    }
}
impl Drop for ReadonlyBind {
    fn drop(&mut self) {
        assert_eq!(
            unsafe { libc::umount2(self.0.as_ptr(), 0) },
            0,
            "actual inputs must close before mount retirement"
        );
    }
}
fn owned(path: &Path, uid: u32, gid: u32, mode: u32) {
    let named = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(named.as_ptr(), uid, gid) }, 0);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
fn run_mounted_child(child_env: &str) -> bool {
    if std::env::var_os(child_env).is_none() {
        let witness = tempfile::tempdir().unwrap();
        std::fs::set_permissions(witness.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let marker = witness.path().join("complete");
        let mut child = std::process::Command::new("/usr/bin/unshare")
            .args(["--mount", "--propagation", "private"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", SELECTOR, "--nocapture"])
            .env(child_env, &marker)
            .spawn()
            .unwrap();
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if started.elapsed() >= Duration::from_secs(60) {
                child.kill().unwrap();
                child.wait().unwrap();
                assert!(
                    started.elapsed() < Duration::from_secs(60),
                    "Owned signed mounted-reader timed out"
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read(marker).unwrap(),
            b"signed mounted projection and actual retirement complete"
        );
        return true;
    }
    false
}

fn mounted_inputs(
    directory: &Path,
    file: &Path,
    writable: &Path,
) -> Vec<launcher::owned_service::OriginalMountedInput> {
    let pathfd = |path: &Path| {
        File::options()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .unwrap()
    };
    vec![
        launcher::owned_service::OriginalMountedInput::from_reviewed(
            "mounted-directory".into(),
            pathfd(directory),
        )
        .unwrap(),
        launcher::owned_service::OriginalMountedInput::from_reviewed(
            "mounted-file".into(),
            pathfd(file),
        )
        .unwrap(),
        launcher::owned_service::OriginalMountedInput::from_reviewed(
            PathBuf::from(std::ffi::OsStr::from_bytes(b"raw-\xff")),
            pathfd(writable),
        )
        .unwrap(),
    ]
}

#[test]
fn signed_original_mounted_roots_project_before_caller_drop_and_close_all_inputs() {
    const CHILD: &str = "TILO_MOUNTED_READER_CHILD";
    tilo_test_env::require_capability!(
        Chown,
        tilo_test_env::switch_identity(65534),
        "the mounted reader mounts roots as root in a mount namespace of its own"
    );
    if run_mounted_child(CHILD) {
        return;
    }
    let temporary = tempfile::Builder::new()
        .prefix("tilo-mounted-reader-")
        .tempdir_in("/run")
        .unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TMPDIR", temporary.path());
    let payload = super::super::support::capture_worker_payload();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env().set(launcher::run::DEBUG_STDERR_ENV, "1");
    let root = home.root();
    let (runtime, publisher) = signed_runtime(root);
    let installed = installed_worker(root, payload);
    let (source, source_path) = original_reader_source(root);
    let directory = root.join("approved-directory");
    std::fs::create_dir(&directory).unwrap();
    owned(&directory, 1003, 1002, 0o750);
    let file = directory.join("leaf");
    std::fs::write(&file, b"group mounted bytes").unwrap();
    owned(&file, 1003, 1002, 0o640);
    let writable = directory.join("owner-writable");
    std::fs::write(&writable, b"owner mounted bytes").unwrap();
    owned(&writable, 1000, 1001, 0o600);
    let bind = ReadonlyBind::new(&directory);
    let inputs = mounted_inputs(&directory, &file, &writable);
    let admission = CaptureAdmission::verify_original_source(
        &runtime,
        &publisher,
        File::open(&source).unwrap(),
        1000,
        1001,
        &[1002],
        [7; 32],
    )
    .unwrap()
    .with_original_mounted_roots(inputs)
    .unwrap()
    .with_archive_helper()
    .unwrap()
    .with_installation(installed.clone(), &installed.publisher_key_hex)
    .unwrap();
    let data = root.join("mounted-work");
    std::fs::create_dir(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut launch = original_reader_launch(&installed, &data, &root.join("source-secret"));
    let LaunchMode::Service { passthrough_argv } = &mut launch.options.mode else {
        panic!("actual service mode required")
    };
    passthrough_argv[0] = "--original-mounted-read".into();
    let key = ServiceLeaseKey::default();
    let exit = returning_callback_retains_original_custody(
        launch,
        admission,
        File::open(&source).unwrap(),
        key.clone(),
        false,
    )
    .unwrap_or_else(|error| {
        panic!(
            "{error:?}\n{}",
            std::fs::read_to_string(data.join("launcher-debug-output.log")).unwrap()
        )
    });
    assert!(exit.matches(&key, &data, PACKAGE));
    // /run/tilo/app is the canonical package/room projection, not data_home.
    // The worker writes this proof before publishing its verified socket reply.
    let app = launcher::appdir::host_app_dir(&data, &launcher::appdir::room_slug(None), PACKAGE);
    assert!(!data.join("worker-proof").exists());
    assert_eq!(
        std::fs::read(app.join("worker-proof")).unwrap(),
        b"fresh private worker state"
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"group mounted bytes");
    assert_eq!(std::fs::read(&writable).unwrap(), b"owner mounted bytes");
    assert_eq!(
        std::fs::read(source.join("mounted-file")).unwrap(),
        b"underlying file remains"
    );
    assert_eq!(
        std::fs::read(source.join(std::ffi::OsStr::from_bytes(b"raw-\xff"))).unwrap(),
        b"underlying raw remains"
    );
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    drop(bind);
    assert_eq!(unsafe { libc::umount2(source_path.as_ptr(), 0) }, 0);
    std::fs::write(
        std::env::var_os(CHILD).unwrap(),
        b"signed mounted projection and actual retirement complete",
    )
    .unwrap();
}
