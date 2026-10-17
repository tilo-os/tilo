//! The helper publishes only complete files, retries cleanly after any
//! interruption, keeps what is already there and never follows a symlink.
//!
//! The library cases drive `publish`, `write_file`, `defaults` and
//! `ownership` through `Files` doubles that interrupt or observe them; the
//! command-line cases run the built helper as `tilo-firstboot-user` does.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test fixture: any failed fixture call fails the test"
)]

use std::cell::RefCell;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rustix::fs::{CWD, FileType, Mode, OFlags, fstat, openat};
use tilo_firstboot_files::{
    DIR, Error, Files, Owner, Result, System, Writer, defaults, ownership, publish, write_file,
};

const HELPER: &str = env!("CARGO_BIN_EXE_tilo-firstboot-files");
const COMPLETE_INTENT: &[u8] = b"fixtureuser:!tilo-firstboot-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";

fn open_dir(path: &Path) -> OwnedFd {
    openat(CWD, path, DIR, Mode::empty()).unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
}

fn me() -> Owner {
    Owner::current()
}

fn interrupted(message: &str) -> Error {
    Error::from(io::Error::new(
        io::ErrorKind::Interrupted,
        message.to_owned(),
    ))
}

fn is_interrupted(result: &Result<()>) -> bool {
    matches!(result, Err(error) if error.os_error().is_some_and(|e| e.kind() == io::ErrorKind::Interrupted))
}

fn write_complete(
    directory: BorrowedFd<'_>,
    name: &OsStr,
    content: &[u8],
    mode: Mode,
) -> Result<()> {
    write_file(&System, directory, name, &mut &content[..], mode, me())
}

/// Hands out three bytes, then fails like an interrupted copy.
struct InterruptedCopy {
    data: &'static [u8],
    sent: bool,
}

impl Read for InterruptedCopy {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.sent {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "copy interrupted",
            ));
        }
        self.sent = true;
        let count = 3.min(buffer.len()).min(self.data.len());
        buffer[..count].copy_from_slice(&self.data[..count]);
        Ok(count)
    }
}

#[test]
fn interrupted_file_copy_has_no_final_and_retry_is_complete() {
    let work = tempfile::tempdir().unwrap();
    let directory = open_dir(work.path());
    let mut source = InterruptedCopy {
        data: b"complete",
        sent: false,
    };
    let result = publish(
        &System,
        directory.as_fd(),
        OsStr::new("default"),
        &mut |name| {
            write_file(
                &System,
                directory.as_fd(),
                name,
                &mut source,
                Mode::from_raw_mode(0o640),
                me(),
            )
        },
    );
    assert!(is_interrupted(&result), "{result:?}");
    assert!(!work.path().join("default").exists());
    assert_eq!(
        fs::read_dir(work.path()).unwrap().count(),
        0,
        "the temporary is removed"
    );
    publish(
        &System,
        directory.as_fd(),
        OsStr::new("default"),
        &mut |name| {
            write_complete(
                directory.as_fd(),
                name,
                b"complete",
                Mode::from_raw_mode(0o640),
            )
        },
    )
    .unwrap();
    assert_eq!(fs::read(work.path().join("default")).unwrap(), b"complete");
    assert_eq!(mode(&work.path().join("default")), 0o640);
}

/// The environment variable that turns `killed_child_entry` into the child.
const KILL_CHILD: &str = "TILO_FIRSTBOOT_FILES_KILLED_CHILD";

/// Not a case of its own: the process `killed_partial_temporary_never_becomes_default_or_intent`
/// starts, which publishes with a writer that is killed half-way. Run directly
/// it has nothing to do.
#[test]
fn killed_child_entry() {
    let Some(spec) = std::env::var_os(KILL_CHILD) else {
        return;
    };
    let spec = PathBuf::from(spec);
    let directory = open_dir(spec.parent().unwrap());
    let name = spec.file_name().unwrap().to_owned();
    let _ = publish(&System, directory.as_fd(), &name, &mut |temporary| {
        let fd = openat(
            directory.as_fd(),
            temporary,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY,
            Mode::from_raw_mode(0o600),
        )
        .unwrap();
        let mut file = fs::File::from(fd);
        file.write_all(b"partial").unwrap();
        file.sync_all().unwrap();
        rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL)
            .unwrap();
        // The signal can land a moment after kill() returns; the writer must
        // not finish in that moment.
        loop {
            std::thread::park();
        }
    });
    panic!("the child outlived SIGKILL");
}

#[test]
fn killed_partial_temporary_never_becomes_default_or_intent() {
    use std::os::unix::process::ExitStatusExt;
    let work = tempfile::tempdir().unwrap();
    let directory = open_dir(work.path());
    for name in ["intent", "default"] {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "killed_child_entry",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(KILL_CHILD, work.path().join(name))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(9), "{name}: {status:?}");
        assert!(!work.path().join(name).exists(), "{name}");
        publish(
            &System,
            directory.as_fd(),
            OsStr::new(name),
            &mut |temporary| {
                write_complete(
                    directory.as_fd(),
                    temporary,
                    b"complete",
                    Mode::from_raw_mode(0o600),
                )
            },
        )
        .unwrap();
        assert_eq!(fs::read(work.path().join(name)).unwrap(), b"complete");
        assert_eq!(fs::metadata(work.path().join(name)).unwrap().nlink(), 1);
    }
}

/// Records the file type of every descriptor synced.
#[derive(Default)]
struct RecordingSyncs {
    synced: RefCell<Vec<FileType>>,
}

impl Files for RecordingSyncs {
    fn fsync(&self, fd: BorrowedFd<'_>) -> Result<()> {
        self.synced
            .borrow_mut()
            .push(FileType::from_raw_mode(fstat(fd).unwrap().st_mode));
        System.fsync(fd)
    }
}

#[test]
fn truncated_intent_before_publication_never_blocks_retry() {
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    let directory = open_dir(root);
    let result = publish(
        &System,
        directory.as_fd(),
        OsStr::new("intent"),
        &mut |name| {
            fs::write(root.join(name), b"fixtureuser:!tilo-").unwrap();
            Err(interrupted("before publication"))
        },
    );
    assert!(is_interrupted(&result), "{result:?}");
    assert!(!root.join("intent").exists());
    assert_eq!(
        fs::read_dir(root).unwrap().count(),
        0,
        "the temporary is removed"
    );

    let recorder = RecordingSyncs::default();
    publish(
        &recorder,
        directory.as_fd(),
        OsStr::new("intent"),
        &mut |name| {
            write_file(
                &recorder,
                directory.as_fd(),
                name,
                &mut &b"complete\n"[..],
                Mode::from_raw_mode(0o600),
                me(),
            )
        },
    )
    .unwrap();
    assert_eq!(fs::read(root.join("intent")).unwrap(), b"complete\n");
    assert_eq!(fs::metadata(root.join("intent")).unwrap().nlink(), 1);
    let synced = recorder.synced.borrow();
    // The file's bytes are durable before its name is, and the name before
    // the publication returns.
    assert_eq!(synced[0], FileType::RegularFile);
    assert_eq!(synced[1], FileType::Directory);

    let result = publish(
        &System,
        directory.as_fd(),
        OsStr::new("intent"),
        &mut |name| {
            fs::write(root.join(name), b"replacement").unwrap();
            Ok(())
        },
    );
    let error = result.unwrap_err();
    assert_eq!(
        error.os_error().unwrap().kind(),
        io::ErrorKind::AlreadyExists,
        "{error}"
    );
    assert_eq!(fs::read(root.join("intent")).unwrap(), b"complete\n");
    assert_eq!(
        fs::read_dir(root).unwrap().count(),
        1,
        "the refused temporary is removed"
    );
}

/// Lists only the given names and interrupts the publication of one of them.
struct InterruptBetweenDefaults {
    names: Vec<OsString>,
    interrupt: &'static str,
}

impl Files for InterruptBetweenDefaults {
    fn list(&self, _directory: BorrowedFd<'_>) -> Result<Vec<OsString>> {
        Ok(self.names.clone())
    }

    fn publish(
        &self,
        directory: BorrowedFd<'_>,
        name: &OsStr,
        writer: &mut Writer<'_>,
    ) -> Result<()> {
        if name == self.interrupt {
            return Err(interrupted("between defaults"));
        }
        publish(self, directory, name, writer)
    }
}

#[test]
fn partial_defaults_retry_preserves_seeds_links_modes_and_ownership() {
    let work = tempfile::tempdir().unwrap();
    let skel = work.path().join("skel");
    let home = work.path().join("home");
    fs::create_dir(&skel).unwrap();
    fs::create_dir(&home).unwrap();
    fs::write(skel.join("first"), "one").unwrap();
    fs::write(skel.join("second"), "two").unwrap();
    fs::set_permissions(skel.join("second"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::write(skel.join("seed"), "default").unwrap();
    fs::write(home.join("seed"), "installer").unwrap();
    symlink("../literal", skel.join("link")).unwrap();
    let source = open_dir(&skel);
    let destination = open_dir(&home);
    let interrupting = InterruptBetweenDefaults {
        names: vec!["first".into(), "second".into()],
        interrupt: "second",
    };
    let result = defaults(&interrupting, source.as_fd(), destination.as_fd(), me());
    assert!(is_interrupted(&result), "{result:?}");
    assert_eq!(fs::read_to_string(home.join("first")).unwrap(), "one");
    assert!(!home.join("second").exists());

    defaults(&System, source.as_fd(), destination.as_fd(), me()).unwrap();
    ownership(&System, destination.as_fd(), me()).unwrap();
    assert_eq!(fs::read_to_string(home.join("seed")).unwrap(), "installer");
    assert_eq!(fs::read_to_string(home.join("second")).unwrap(), "two");
    assert_eq!(mode(&home.join("second")), 0o640);
    assert_eq!(
        fs::read_link(home.join("link")).unwrap(),
        Path::new("../literal")
    );
    let (uid, gid) = (
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw(),
    );
    for entry in fs::read_dir(&home).unwrap() {
        let metadata = fs::symlink_metadata(entry.unwrap().path()).unwrap();
        assert_eq!((metadata.uid(), metadata.gid()), (uid, gid));
    }
}

#[test]
fn destination_directory_symlink_is_never_followed() {
    let work = tempfile::tempdir().unwrap();
    let skel = work.path().join("skel");
    let home = work.path().join("home");
    let outside = work.path().join("outside");
    fs::create_dir_all(skel.join("nested")).unwrap();
    fs::write(skel.join("nested/default"), "default").unwrap();
    fs::create_dir(&home).unwrap();
    fs::create_dir(&outside).unwrap();
    symlink(&outside, home.join("nested")).unwrap();
    let result = defaults(
        &System,
        open_dir(&skel).as_fd(),
        open_dir(&home).as_fd(),
        me(),
    );
    assert!(
        result
            .as_ref()
            .is_err_and(|error| error.os_error().is_some()),
        "{result:?}"
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn defaults_copy_nested_directories_with_their_modes() {
    let work = tempfile::tempdir().unwrap();
    let skel = work.path().join("skel");
    let home = work.path().join("home");
    fs::create_dir_all(skel.join("config/app")).unwrap();
    fs::write(skel.join("config/app/settings"), "x").unwrap();
    fs::set_permissions(skel.join("config/app"), fs::Permissions::from_mode(0o750)).unwrap();
    fs::create_dir(&home).unwrap();
    defaults(
        &System,
        open_dir(&skel).as_fd(),
        open_dir(&home).as_fd(),
        me(),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(home.join("config/app/settings")).unwrap(),
        "x"
    );
    assert_eq!(mode(&home.join("config/app")), 0o750);
    // An existing directory keeps its own mode and gains what it lacks.
    fs::set_permissions(home.join("config/app"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(home.join("config/app/settings")).unwrap();
    defaults(
        &System,
        open_dir(&skel).as_fd(),
        open_dir(&home).as_fd(),
        me(),
    )
    .unwrap();
    assert_eq!(mode(&home.join("config/app")), 0o700);
    assert!(home.join("config/app/settings").exists());
}

#[test]
fn unsupported_skeleton_inode_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let skel = work.path().join("skel");
    let home = work.path().join("home");
    fs::create_dir(&skel).unwrap();
    fs::create_dir(&home).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(skel.join("socket")).unwrap();
    let error = defaults(
        &System,
        open_dir(&skel).as_fd(),
        open_dir(&home).as_fd(),
        me(),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "unsupported skeleton inode");
    assert!(!home.join("socket").exists());
}

#[test]
fn ownership_refuses_a_hard_linked_home_file() {
    let work = tempfile::tempdir().unwrap();
    let home = work.path().join("home");
    fs::create_dir(&home).unwrap();
    fs::write(work.path().join("outside"), "secret").unwrap();
    fs::hard_link(work.path().join("outside"), home.join("link")).unwrap();
    let error = ownership(&System, open_dir(&home).as_fd(), me()).unwrap_err();
    assert_eq!(error.to_string(), "hardlinked home file");
}

#[test]
fn ids_are_refused_only_when_a_chown_needs_them() {
    let work = tempfile::tempdir().unwrap();
    let directory = open_dir(work.path());
    for (owner, message) in [
        (Owner { uid: -2, gid: 0 }, "uid is less than minimum"),
        (
            Owner {
                uid: 0,
                gid: 1 << 32,
            },
            "gid is greater than maximum",
        ),
    ] {
        let result = write_file(
            &System,
            directory.as_fd(),
            OsStr::new("file"),
            &mut &b"x"[..],
            Mode::from_raw_mode(0o600),
            owner,
        );
        assert_eq!(result.unwrap_err().to_string(), message);
        fs::remove_file(work.path().join("file")).unwrap();
    }
    // -1 and 2**32 - 1 leave an id unchanged, as os.fchown does.
    let unchanged = Owner {
        uid: -1,
        gid: i128::from(u32::MAX),
    };
    write_file(
        &System,
        directory.as_fd(),
        OsStr::new("file"),
        &mut &b"x"[..],
        Mode::from_raw_mode(0o600),
        unchanged,
    )
    .unwrap();
}

fn helper(args: &[&OsStr], stdin: &[u8]) -> Output {
    let mut child = Command::new(HELPER)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The helper may fail before it reads its input.
    let _ = child.stdin.take().unwrap().write_all(stdin);
    child.wait_with_output().unwrap()
}

fn os(text: &str) -> &OsStr {
    OsStr::new(text)
}

#[test]
fn truncated_intent_input_is_never_published() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("intent");
    let output = helper(
        &[os("intent"), path.as_os_str()],
        b"fixtureuser:!tilo-firstboot-",
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("incomplete intent record"));
    assert!(!path.exists());
    let output = helper(&[os("intent"), path.as_os_str()], COMPLETE_INTENT);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&path).unwrap(), COMPLETE_INTENT);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(fs::read_dir(work.path()).unwrap().count(), 1);
}

#[test]
fn intent_record_must_match_exactly() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("intent");
    let lock = "!tilo-firstboot-".to_owned() + &"a".repeat(32);
    for record in [
        format!("Fixture:{lock}\n"),
        format!("1user:{lock}\n"),
        format!("{}:{lock}\n", "a".repeat(33)),
        format!(":{lock}\n"),
        format!("user:{lock}"),
        format!("user:{lock}\n\n"),
        format!("user:{lock}A\n"),
        format!("user:!tilo-firstboot-{}\n", "A".repeat(32)),
        format!("user:!tilo-firstboot-{}\n", "a".repeat(31)),
        format!("user:x{lock}\n"),
        format!("user:{lock}\n") + &"x".repeat(1100),
    ] {
        let output = helper(&[os("intent"), path.as_os_str()], record.as_bytes());
        assert_eq!(output.status.code(), Some(1), "{record:?}");
        assert!(!path.exists(), "{record:?}");
    }
    for record in [
        format!("_{}:{lock}\n", "a-9_".repeat(7) + "abc"),
        format!("a:{lock}\n"),
    ] {
        let output = helper(&[os("intent"), path.as_os_str()], record.as_bytes());
        assert!(
            output.status.success(),
            "{record:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::remove_file(&path).unwrap();
    }
}

#[test]
fn intent_never_replaces_an_existing_record() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("intent");
    fs::write(&path, "unexpected").unwrap();
    let output = helper(&[os("intent"), path.as_os_str()], COMPLETE_INTENT);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(fs::read_to_string(&path).unwrap(), "unexpected");
    assert_eq!(fs::read_dir(work.path()).unwrap().count(), 1);
}

fn ids() -> (String, String) {
    (
        rustix::process::getuid().as_raw().to_string(),
        rustix::process::getgid().as_raw().to_string(),
    )
}

#[test]
fn home_is_created_private_with_every_missing_default() {
    let work = tempfile::tempdir().unwrap();
    let skel = work.path().join("skel");
    fs::create_dir_all(skel.join(".config")).unwrap();
    fs::write(skel.join(".bashrc"), "rc").unwrap();
    fs::set_permissions(skel.join(".bashrc"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(skel.join(".config/seeded"), "skel").unwrap();
    symlink(".bashrc", skel.join(".profile")).unwrap();
    // The account's parent is reached through a symlink, as /home is.
    fs::create_dir_all(work.path().join("var/home")).unwrap();
    symlink("var/home", work.path().join("home")).unwrap();
    let home = work.path().join("home/alice");
    let (uid, gid) = ids();
    let run = || {
        helper(
            &[
                os("home"),
                home.as_os_str(),
                os(&uid),
                os(&gid),
                skel.as_os_str(),
            ],
            b"",
        )
    };
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let real = work.path().join("var/home/alice");
    assert_eq!(mode(&real), 0o700);
    assert_eq!(fs::read_to_string(real.join(".bashrc")).unwrap(), "rc");
    assert_eq!(mode(&real.join(".bashrc")), 0o644);
    assert_eq!(
        fs::read_link(real.join(".profile")).unwrap(),
        Path::new(".bashrc")
    );
    assert_eq!(
        fs::read_to_string(real.join(".config/seeded")).unwrap(),
        "skel"
    );

    // A retry keeps what the user or installer already has.
    fs::write(real.join(".config/seeded"), "installer").unwrap();
    fs::remove_file(real.join(".bashrc")).unwrap();
    let output = run();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(real.join(".config/seeded")).unwrap(),
        "installer"
    );
    assert_eq!(fs::read_to_string(real.join(".bashrc")).unwrap(), "rc");
    let names: Vec<_> = fs::read_dir(&real)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        names
            .iter()
            .all(|n| !n.to_string_lossy().starts_with(".tilo-firstboot-")),
        "{names:?}"
    );
}

#[test]
fn home_symlink_is_refused_and_nothing_outside_changes() {
    let work = tempfile::tempdir().unwrap();
    let skel = work.path().join("skel");
    fs::create_dir(&skel).unwrap();
    fs::write(skel.join("default"), "default").unwrap();
    let outside = work.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let home = work.path().join("alice");
    symlink(&outside, &home).unwrap();
    let (uid, gid) = ids();
    let output = helper(
        &[
            os("home"),
            home.as_os_str(),
            os(&uid),
            os(&gid),
            skel.as_os_str(),
        ],
        b"",
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn malformed_command_lines_fail() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("x");
    let skel = work.path().as_os_str();
    for args in [
        vec![],
        vec![os("intent")],
        vec![os("unknown"), path.as_os_str()],
        vec![os("home"), path.as_os_str()],
        vec![os("home"), path.as_os_str(), os("1")],
        vec![os("home"), path.as_os_str(), os("x"), os("1"), skel],
        vec![os("home"), path.as_os_str(), os("1.0"), os("1"), skel],
        vec![os("intent"), os("/nonexistent-tilo-fixture/intent")],
    ] {
        let output = helper(&args, COMPLETE_INTENT);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(
            output.stderr.starts_with(b"tilo-firstboot-files: "),
            "{args:?}"
        );
        assert!(!path.exists(), "{args:?}");
    }
}

#[test]
fn ownership_only_home_preserves_content_source_witnesses_on_retry() {
    let work = tempfile::tempdir().unwrap();
    let home = work.path().join("home");
    let (uid, gid) = ids();
    let args = [os("home"), home.as_os_str(), os(&uid), os(&gid)];
    assert!(helper(&args, b"").status.success());
    assert!(home.is_dir());
    assert_eq!(mode(&home), 0o700);
    fs::write(home.join("attested"), b"captured content").unwrap();
    symlink("attested", home.join("link")).unwrap();
    let paths = [home.clone(), home.join("attested"), home.join("link")];
    let witness = |path: &PathBuf| {
        let metadata = fs::symlink_metadata(path).unwrap();
        (
            metadata.ino(),
            metadata.uid(),
            metadata.gid(),
            metadata.mode(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    let before: Vec<_> = paths.iter().map(witness).collect();
    let output = helper(&args, b"");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(paths.iter().map(witness).collect::<Vec<_>>(), before);
    assert_eq!(
        fs::read(home.join("attested")).unwrap(),
        b"captured content"
    );
}
