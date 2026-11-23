//! Backend qualification: real kernel caller binding and signed native worker.
//! The root-helper CLI and polkit elevation are deliberately outside this proof.
use super::super::{
    Broker, Command, Grant, Response, Secret, Store,
    runtime::{CaptureServicePins, verify_sidecar},
    transport::{HostChannel, WorkerChannel},
    worker,
};
use super::{Live, Writer, query};
use crate::setup::{
    migration::PinnedDirectory,
    portable_home_media::MediaGrant,
    portable_transfer::{ImportReview, Purpose},
};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command as Process, Stdio},
    sync::atomic::AtomicBool,
};
use zeroize::Zeroizing;
const TEST: &str = "setup::home_capture::native_tests::signed_export::actual_signed_export_and_native_import_preserve_active_home";
const MODE: &str = "TILO_CAPTURE_NATIVE_REVIEW_CHILD";
const PASSWORD: &[u8] = b"native capture qualification password";
const BINDING: &str = "native-capture-qualification";
const CHILD_FD: i32 = 70;

mod cancellation;

struct Reviewer {
    child: Child,
    channel: HostChannel,
}
impl Reviewer {
    fn finish(mut self) {
        self.channel.stream().write_all(&[1]).unwrap();
        assert!(self.child.wait().unwrap().success());
    }
}
impl Drop for Reviewer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn original_user_review() {
    assert_ne!(rustix::process::getuid().as_raw(), 0);
    assert_eq!(
        rustix::process::getuid().as_raw(),
        rustix::process::geteuid().as_raw()
    );
    let socket = unsafe { UnixStream::from_raw_fd(CHILD_FD) };
    let mut channel = WorkerChannel::authenticate(socket).unwrap();
    channel
        .authenticate_root(unsafe { libc::getppid() }.cast_unsigned())
        .unwrap();
    let home = PathBuf::from(std::env::var_os("TILO_CAPTURE_NATIVE_HOME").unwrap());
    let output = PathBuf::from(std::env::var_os("TILO_CAPTURE_NATIVE_OUTPUT").unwrap());
    let operation = std::env::var("TILO_CAPTURE_NATIVE_OPERATION").unwrap();
    let grant = Grant::review(&home, &operation).unwrap();
    let output = MediaGrant::open(&output)
        .unwrap()
        .clone_reviewed_directory()
        .unwrap();
    worker::write_frame(
        channel.stream(),
        &serde_json::to_vec(&grant).unwrap(),
        32 * 1024,
    )
    .unwrap();
    channel.send(2, &output).unwrap();
    let mut done = [0];
    channel.stream().read_exact(&mut done).unwrap();
    assert_eq!(done, [1]);
}
fn executable_digest(path: &Path) -> [u8; 32] {
    use sha2::Digest;
    let mut file = File::open(path).unwrap();
    let mut digest = sha2::Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let count = file.read(&mut bytes).unwrap();
        if count == 0 {
            return digest.finalize().into();
        }
        digest.update(bytes.get(..count).unwrap());
    }
}
fn stage_reviewer(directory: &Path) -> PathBuf {
    let current = std::env::current_exe().unwrap();
    let executable = directory.join("native-reviewer");
    std::fs::copy(&current, &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        executable_digest(Path::new("/proc/self/exe")),
        executable_digest(&executable)
    );
    let facts = std::fs::symlink_metadata(&executable).unwrap();
    assert!(facts.is_file());
    assert_eq!(facts.uid(), 0);
    executable
}
fn reviewer(
    executable: &Path,
    home: &Path,
    output: &Path,
    operation: &str,
    uid: u32,
    gid: u32,
) -> (Grant, File, Reviewer) {
    let (parent, child_socket) = UnixStream::pair().unwrap();
    let descriptor = child_socket.as_raw_fd();
    let mut command = Process::new(executable);
    command
        .args(["--exact", TEST, "--nocapture"])
        .env(MODE, "1")
        .env("TILO_CAPTURE_NATIVE_HOME", home)
        .env("TILO_CAPTURE_NATIVE_OUTPUT", output)
        .env("TILO_CAPTURE_NATIVE_OPERATION", operation)
        .env("HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) < 0
                || libc::setresgid(gid, gid, gid) < 0
                || libc::setresuid(uid, uid, uid) < 0
                || libc::dup2(descriptor, CHILD_FD) < 0
                || libc::fcntl(CHILD_FD, libc::F_SETFD, 0) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(child_socket);
    let authenticated = HostChannel::authenticate(parent, child.id(), uid);
    if authenticated.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let mut channel = authenticated.expect("real original-user reviewer authentication");
    channel.authenticate_as_root().unwrap();
    let grant: Grant =
        serde_json::from_slice(&worker::read_frame(channel.stream(), 32 * 1024).unwrap()).unwrap();
    assert_eq!(grant.caller, uid);
    assert_eq!(grant.operation, operation);
    let output = channel.receive(2).unwrap();
    (grant, output, Reviewer { child, channel })
}
fn ownership(root: &Path, uid: u32, gid: u32) {
    let path = std::ffi::CString::new(root.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::lchown(path.as_ptr(), uid, gid) }, 0);
    if std::fs::symlink_metadata(root).unwrap().is_dir() {
        for entry in std::fs::read_dir(root).unwrap() {
            ownership(&entry.unwrap().path(), uid, gid);
        }
    }
}
fn private(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn native_account() -> (u32, u32) {
    let account = unsafe { libc::getpwnam(c"nobody".as_ptr()) };
    assert!(
        !account.is_null(),
        "native qualifier requires the real nobody NSS account"
    );
    let (uid, gid) = unsafe { ((*account).pw_uid, (*account).pw_gid) };
    assert_ne!(uid, 0);
    (uid, gid)
}
fn artifact() -> PathBuf {
    let executable = PathBuf::from(
        std::env::var_os("TILO_CAPTURE_NATIVE_HOST")
            .expect("actual pinned Imager host artifact required"),
    )
    .canonicalize()
    .unwrap();
    let pins_path = PathBuf::from(
        std::env::var_os("TILO_CAPTURE_NATIVE_PINS")
            .expect("independent SDK-produced capture pins required"),
    );
    let pins_facts = std::fs::symlink_metadata(&pins_path).unwrap();
    assert!(
        pins_facts.is_file() && pins_facts.len() <= 16 * 1024 && pins_facts.mode() & 0o022 == 0
    );
    assert_eq!(pins_facts.uid(), rustix::process::geteuid().as_raw());
    let pins: CaptureServicePins =
        serde_json::from_slice(&std::fs::read(pins_path).unwrap()).unwrap();
    let parent = executable.parent().unwrap();
    let prefix = if parent.file_name() == Some(std::ffi::OsStr::new("bin")) {
        parent.parent().unwrap()
    } else {
        parent
    };
    let service = verify_sidecar(&prefix.join("share/tilo-imager/capture"), &pins).unwrap();
    assert_eq!(service.package_hash, pins.package_hash);
    let facts = std::fs::symlink_metadata(&executable).unwrap();
    assert!(facts.is_file() && facts.mode() & 0o111 != 0 && facts.mode() & 0o022 == 0);
    assert_eq!(
        facts.uid(),
        rustix::process::geteuid().as_raw(),
        "qualifier artifact must be owned by its privileged operator"
    );
    executable
}
fn writer(database: &Path) -> Writer {
    let mut child = Process::new("python3").args(["-u", "-c", r"
import sqlite3,sys,time
c=sqlite3.connect(sys.argv[1]); c.execute('PRAGMA journal_mode=WAL'); c.execute('PRAGMA wal_autocheckpoint=0')
c.execute('CREATE TABLE value(n INTEGER)'); c.execute('CREATE TABLE mirror(n INTEGER)')
c.execute('INSERT INTO value VALUES(0)'); c.execute('INSERT INTO mirror VALUES(0)'); c.commit()
n=0
while True:
 n+=1; c.execute('BEGIN IMMEDIATE'); c.execute('UPDATE value SET n=?',(n,)); c.execute('UPDATE mirror SET n=?',(n,)); c.commit()
 if n==1: print('READY',flush=True)
 time.sleep(.002)
"]).arg(database).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    let mut ready = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(child.stdout.take().unwrap()),
        &mut ready,
    )
    .unwrap();
    assert_eq!(ready.trim(), "READY");
    Writer(child)
}
fn dataset(home: &Path) -> PathBuf {
    let data = home.join(".var/app/so.test/data");
    private(&data);
    private(&home.join("Documents"));
    let document = data.join("document");
    std::fs::write(&document, b"hidden application document").unwrap();
    std::fs::set_permissions(&document, std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::hard_link(&document, home.join("Documents/same-document")).unwrap();
    std::os::unix::fs::symlink(".var/app/so.test/data/document", home.join("shortcut")).unwrap();
    let file = File::open(document).unwrap();
    let attribute = b"preserve this attribute";
    assert_eq!(
        unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                c"user.tilo.capture".as_ptr(),
                attribute.as_ptr().cast(),
                attribute.len(),
                0,
            )
        },
        0
    );
    data.join("state.sqlite")
}
fn verify_import(home: &Path) {
    let document = home.join(".var/app/so.test/data/document");
    assert_eq!(
        std::fs::read(&document).unwrap(),
        b"hidden application document"
    );
    let facts = std::fs::metadata(&document).unwrap();
    assert_eq!(facts.mode() & 0o7777, 0o640);
    assert_eq!(
        facts.ino(),
        std::fs::metadata(home.join("Documents/same-document"))
            .unwrap()
            .ino()
    );
    assert_eq!(
        std::fs::read_link(home.join("shortcut")).unwrap(),
        Path::new(".var/app/so.test/data/document")
    );
    let file = File::open(document).unwrap();
    let mut bytes = [0; 128];
    let length = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            c"user.tilo.capture".as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    assert_eq!(
        &bytes[..usize::try_from(length).unwrap()],
        b"preserve this attribute"
    );
    assert_eq!(
        query(
            &home.join(".var/app/so.test/data/state.sqlite"),
            "PRAGMA integrity_check; SELECT value.n=mirror.n AND value.n>0 FROM value,mirror"
        ),
        "ok\n1"
    );
    assert!(!home.join("bob").exists());
}
fn exported_archive(
    broker: &mut Broker,
    executable: &Path,
    output: &File,
    cancel: &AtomicBool,
) -> crate::setup::portable_home_media::Expectation {
    assert!(
        matches!(broker.run_command(executable,Command::Review,None,cancel).unwrap(),Response::Review { files, .. } if files>0)
    );
    let export_operation = uuid::Uuid::new_v4().to_string();
    let response = broker
        .run_command(
            executable,
            Command::Export {
                export_operation: export_operation.clone(),
                setup_binding: BINDING.into(),
                password: Secret::new(Zeroizing::new(PASSWORD.to_vec())),
            },
            Some(output.try_clone().unwrap()),
            cancel,
        )
        .unwrap();
    let expected = match response {
        Response::Export { expected } => Some(expected),
        _ => None,
    }
    .expect("real worker must return authenticated Home expectation");
    assert!(matches!(
        broker
            .run_command(
                executable,
                Command::ValidateExisting {
                    export_operation,
                    expected: expected.clone(),
                    setup_binding: BINDING.into(),
                    password: Secret::new(Zeroizing::new(PASSWORD.to_vec()))
                },
                Some(output.try_clone().unwrap()),
                cancel
            )
            .unwrap(),
        Response::Validated
    ));
    expected
}
fn imported_archive(
    expected: &crate::setup::portable_home_media::Expectation,
    media: &Path,
    destination: &Path,
    cancel: &AtomicBool,
) -> PathBuf {
    let archive = ImportReview::review(
        &media.join(&expected.filename),
        PASSWORD,
        Purpose::Home,
        expected.limits,
        cancel,
    )
    .unwrap();
    assert_eq!(archive.ciphertext_digest(), expected.ciphertext_sha256);
    assert_eq!(archive.info().setup_binding.as_deref(), Some(BINDING));
    let quarantine = destination.join("import");
    let tree = archive
        .extract(Zeroizing::new(PASSWORD.to_vec()), &quarantine, cancel)
        .unwrap();
    verify_import(&tree.root);
    tree.root
}
#[test]
fn actual_signed_export_and_native_import_preserve_active_home() {
    if std::env::var_os(MODE).is_some() {
        original_user_review();
        return;
    }
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "canonical privileged native-tier1 qualifier required"
    );
    let executable = artifact();
    let (uid, gid) = native_account();
    let store = PathBuf::from(
        std::env::var_os("TILO_FS_NATIVE_STORE")
            .expect("canonical disposable Btrfs volume required"),
    );
    let temporary = tempfile::Builder::new()
        .prefix("signed-home-")
        .permissions(std::fs::Permissions::from_mode(0o711))
        .tempdir_in(store)
        .unwrap();
    let live_path = temporary.path().join("live");
    fs_native::btrfs::create_subvolume(&live_path).unwrap();
    let live = Live(live_path);
    std::fs::set_permissions(&live.0, std::fs::Permissions::from_mode(0o711)).unwrap();
    let home = live.0.join("alice");
    private(&home);
    let database = dataset(&home);
    private(&live.0.join("bob"));
    std::fs::write(live.0.join("bob/private"), b"unapproved sibling").unwrap();
    let mut writer = writer(&database);
    ownership(&home, uid, gid);
    let media = temporary.path().join("media");
    private(&media);
    ownership(&media, uid, gid);
    let operation = uuid::Uuid::new_v4().to_string();
    let reviewer_executable = stage_reviewer(temporary.path());
    let (grant, output, reviewer) =
        reviewer(&reviewer_executable, &home, &media, &operation, uid, gid);
    let broker_directory = temporary.path().join("broker");
    private(&broker_directory);
    let store = Store::open(
        PinnedDirectory::open_absolute(&broker_directory).unwrap(),
        &operation,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let mut broker = Broker::capture_in(grant, uid, &cancel, store).unwrap();
    reviewer.finish();
    let expected = exported_archive(&mut broker, &executable, &output, &cancel);
    assert!(writer.0.try_wait().unwrap().is_none());
    let imported = imported_archive(&expected, &media, temporary.path(), &cancel);
    let captured_count = query(
        &imported.join(".var/app/so.test/data/state.sqlite"),
        "SELECT n FROM value",
    )
    .parse::<u64>()
    .unwrap();
    assert!(
        query(&database, "SELECT n FROM value")
            .parse::<u64>()
            .unwrap()
            > captured_count
    );
    assert!(writer.0.try_wait().unwrap().is_none());
    let home_name = std::ffi::CString::new(home.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(home_name.as_ptr(), 0, gid) }, 0);
    assert!(
        broker.validate(&cancel).is_err(),
        "changed original grant must revoke source access"
    );
    assert_eq!(unsafe { libc::chown(home_name.as_ptr(), uid, gid) }, 0);
    broker.validate(&cancel).unwrap();
    broker.release().unwrap();
    assert!(
        !broker_directory
            .join("snapshots")
            .join(format!("home-{operation}"))
            .exists()
    );
    assert!(writer.0.try_wait().unwrap().is_none());
    assert_eq!(
        std::fs::read(home.join(".var/app/so.test/data/document")).unwrap(),
        b"hidden application document"
    );
}
