use super::*;
use std::{
    io::{BufRead, Write},
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    process::{Child, Command as Process, Stdio},
};

struct Live(PathBuf);
impl Drop for Live {
    fn drop(&mut self) {
        btrfs::delete_subvolume(&self.0).expect("dispose only this test's live subvolume");
    }
}
struct Writer(Child);
impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        self.0.wait().expect("reap test SQLite writer");
    }
}

fn query(path: &Path, sql: &str) -> String {
    let output = Process::new("sqlite3")
        .arg(path)
        .arg(sql)
        .output()
        .expect("real SQLite CLI required");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[test]
fn actual_pinned_broker_capture_preserves_live_sqlite_wal_hidden_data_links_and_xattrs() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "canonical disposable privileged native qualifier required"
    );
    let store = PathBuf::from(
        std::env::var_os("TILO_FS_NATIVE_STORE")
            .expect("canonical disposable Btrfs volume is required"),
    );
    let temporary = tempfile::Builder::new()
        .prefix("imager-home-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(store)
        .unwrap();
    let live_path = temporary.path().join("live");
    btrfs::create_subvolume(&live_path).unwrap();
    let live = Live(live_path);
    let home = live.0.join("alice");
    let data = home.join(".var/app/so.test/data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(home.join("Documents")).unwrap();
    std::fs::create_dir_all(live.0.join("bob")).unwrap();
    std::fs::write(live.0.join("bob/private"), b"unapproved sibling").unwrap();
    let document = data.join("document");
    std::fs::write(&document, b"hidden application document").unwrap();
    std::fs::set_permissions(&document, std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::hard_link(&document, home.join("Documents/same-document")).unwrap();
    let file = File::open(&document).unwrap();
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
    let database = data.join("state.sqlite");
    let mut child = Process::new("python3")
        .arg("-u")
        .arg("-c")
        .arg(
            r"
import sqlite3, sys, time
c = sqlite3.connect(sys.argv[1])
c.execute('PRAGMA journal_mode=WAL')
c.execute('PRAGMA wal_autocheckpoint=0')
c.execute('CREATE TABLE value(n INTEGER)')
c.execute('CREATE TABLE mirror(n INTEGER)')
c.execute('INSERT INTO value VALUES(0)')
c.execute('INSERT INTO mirror VALUES(0)')
c.commit()
n=0
while True:
    n += 1
    c.execute('BEGIN IMMEDIATE')
    c.execute('UPDATE value SET n=?', (n,))
    c.execute('UPDATE mirror SET n=?', (n,))
    c.commit()
    if n == 1: print('READY', flush=True)
    time.sleep(.002)
",
        )
        .arg(&database)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let mut writer = Writer(child);
    assert_eq!(ready.trim(), "READY");
    let before: u64 = query(&database, "SELECT n FROM value").parse().unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let grant = Grant::review(&home, &operation).unwrap();
    let request_directory = temporary.path().join("request");
    std::fs::create_dir(&request_directory).unwrap();
    std::fs::set_permissions(&request_directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let request = super::super::home_capture_helper::Request {
        schema: 1,
        action: super::super::home_capture_helper::Action::Capture,
        grant,
        channel: None,
    };
    super::super::home_capture_helper::publish_request(&request_directory, &request).unwrap();
    let admitted =
        super::super::home_capture_helper::read_request(&request_directory.join("request.json"), 0)
            .unwrap();
    let broker_directory = temporary.path().join("broker");
    std::fs::create_dir(&broker_directory).unwrap();
    std::fs::set_permissions(&broker_directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let private = Store::open(
        PinnedDirectory::open_absolute(&broker_directory).unwrap(),
        &operation,
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let mut broker = Broker::capture_in(admitted.grant, 0, &cancel, private).unwrap();
    assert_eq!(broker.status().state, State::Ready);
    let lease = broker.lease.as_ref().unwrap();
    let selected = PathBuf::from(format!("/proc/self/fd/{}", lease.selected().as_raw_fd()));
    let captured = selected.join(".var/app/so.test/data");
    let captured_document = captured.join("document");
    assert_eq!(
        std::fs::read(&captured_document).unwrap(),
        b"hidden application document"
    );
    let original = std::fs::metadata(&document).unwrap();
    let facts = std::fs::metadata(&captured_document).unwrap();
    assert_eq!(
        (facts.uid(), facts.gid(), facts.mode() & 0o7777),
        (original.uid(), original.gid(), original.mode() & 0o7777)
    );
    assert_eq!(
        facts.ino(),
        std::fs::metadata(selected.join("Documents/same-document"))
            .unwrap()
            .ino()
    );
    let captured_file = File::open(&captured_document).unwrap();
    let mut value = [0u8; 128];
    let length = unsafe {
        libc::fgetxattr(
            captured_file.as_raw_fd(),
            c"user.tilo.capture".as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    assert_eq!(usize::try_from(length).unwrap(), attribute.len());
    assert_eq!(&value[..attribute.len()], attribute);
    assert!(
        File::options()
            .write(true)
            .open(&captured_document)
            .and_then(|mut file| file.write_all(b"change"))
            .is_err()
    );
    // SQLite gets a private SHM working copy for checking; the capture itself
    // keeps every raw database/WAL/SHM byte, without provider-specific filtering.
    let check = tempfile::tempdir().unwrap();
    for suffix in ["", "-wal", "-shm"] {
        std::fs::copy(
            captured.join(format!("state.sqlite{suffix}")),
            check.path().join(format!("state.sqlite{suffix}")),
        )
        .unwrap();
    }
    assert_eq!(
        query(
            &check.path().join("state.sqlite"),
            "PRAGMA integrity_check; SELECT value.n=mirror.n AND value.n>0 FROM value,mirror"
        ),
        "ok\n1"
    );
    let epoch_wal = std::fs::read(captured.join("state.sqlite-wal")).unwrap();
    // The live database moves on past the captured epoch.
    tilo_test_env::wait_until("the live database to move on", || {
        query(&database, "SELECT n FROM value")
            .parse::<u64>()
            .unwrap()
            > before
    });
    broker.validate(&cancel).unwrap();
    assert_eq!(
        std::fs::read(captured.join("state.sqlite-wal")).unwrap(),
        epoch_wal
    );
    assert!(
        query(&database, "SELECT n FROM value")
            .parse::<u64>()
            .unwrap()
            > before
    );
    assert!(writer.0.try_wait().unwrap().is_none());
    broker.release().unwrap();
    assert_eq!(broker.status().state, State::Released);
    assert!(
        !broker_directory
            .join("snapshots")
            .join(format!("home-{operation}"))
            .exists()
    );
    assert!(writer.0.try_wait().unwrap().is_none());
    assert_eq!(
        std::fs::read(&document).unwrap(),
        b"hidden application document"
    );
}

mod signed_export;

mod live_filesystems;

mod retained_projection;

mod retained_home_projection;
