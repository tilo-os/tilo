//! An app in its container, on a real Windows: Wine has no `AppContainer`, so the confined
//! scenarios run on a Windows worker alone (`tools/ci/windows/wine-skip.txt`).
//!
//! The confined process is this test executable again, started with [`PROBE`] naming what it
//! tries from inside: it runs only [`probe`], which passes when what it tried went as the
//! container's grants say.

#![allow(clippy::unwrap_used, reason = "test fixtures")]

use std::io::Read as _;

use super::*;

/// What the probe tries, from inside its container: `own`, `other` and `pipe`, separated by
/// newlines.
const PROBE: &str = "TILO_LAUNCHER_WINDOWS_PROBE";

#[test]
fn a_container_is_named_for_its_app_alone() {
    let name = Container::name_for("so.tilo.notes");
    assert_eq!(
        name,
        Container::name_for("so.tilo.notes"),
        "every launch's own"
    );
    assert_ne!(name, Container::name_for("so.tilo.mail"));
    assert!(
        name.len() <= 64,
        "{name} is longer than a container's name may be"
    );
    assert!(
        name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'),
        "{name}"
    );
}

/// Inside the container: its own storage is its to write; another app's is not even readable,
/// and the daemon's pipe refuses it. Outside one (no [`PROBE`]), nothing to do.
#[test]
fn probe() {
    let Some(asked) = std::env::var_os(PROBE) else {
        return;
    };
    let asked = asked.into_string().unwrap();
    if asked == "wait" {
        // Until the job ends it.
        std::thread::sleep(std::time::Duration::from_secs(600));
        return;
    }
    let mut lines = asked.lines();
    let (own, other, pipe) = (
        Path::new(lines.next().unwrap()),
        Path::new(lines.next().unwrap()),
        Path::new(lines.next().unwrap()),
    );
    std::fs::write(own.join("written"), b"mine").expect("its own storage is writable");
    let read = std::fs::read(other.join("secret"));
    assert_eq!(
        read.as_ref().map_err(io::Error::kind).err(),
        Some(io::ErrorKind::PermissionDenied),
        "another app's storage: {read:?}"
    );
    let written = std::fs::write(other.join("planted"), b"x");
    assert_eq!(
        written.as_ref().map_err(io::Error::kind).err(),
        Some(io::ErrorKind::PermissionDenied),
        "another app's storage: {written:?}"
    );
    let connected = tilo_socket::connect(pipe);
    assert_eq!(
        connected
            .as_ref()
            .map(|_| ())
            .map_err(io::Error::kind)
            .err(),
        Some(io::ErrorKind::PermissionDenied),
        "the daemon's pipe"
    );
}

#[test]
fn an_app_keeps_its_own_storage_and_reaches_no_other_apps_or_the_daemons_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let own = dir.path().join("own");
    let other = dir.path().join("other");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("secret"), b"not yours").unwrap();
    // The other app's storage is granted to its own container, never to this one.
    Container::for_app("so.tilo.test.other")
        .unwrap()
        .grant(&other, Access::ReadWrite)
        .unwrap();
    let pipe = dir.path().join("daemon.sock");
    let _listener = tilo_socket::bind(&pipe).unwrap();

    let program = std::env::current_exe().unwrap();
    let mut command = Command::new(&program);
    command
        .args([
            "tests::probe",
            "--exact",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(
            PROBE,
            format!("{}\n{}\n{}", own.display(), other.display(), pipe.display()),
        );
    let (_link, mut launched) = launch(LaunchRequest {
        package_id: "so.tilo.test.confined".to_owned(),
        command,
        read: vec![program.parent().unwrap().to_owned()],
        read_write: vec![own.clone()],
    })
    .unwrap();
    let mut said = String::new();
    if let Some(mut stderr) = launched.child.stderr.take() {
        stderr.read_to_string(&mut said).unwrap();
    }
    let status = launched.wait().unwrap();
    assert!(
        status.success(),
        "the probe failed in its container: {said}"
    );
    assert_eq!(std::fs::read(own.join("written")).unwrap(), b"mine");
    assert!(!other.join("planted").exists());
}

#[test]
fn the_job_ends_the_app_with_its_holder() {
    let program = std::env::current_exe().unwrap();
    let mut command = Command::new(&program);
    command
        .args(["tests::probe", "--exact", "--test-threads", "1"])
        .env(PROBE, "wait");
    let (_link, launched) = launch(LaunchRequest {
        package_id: "so.tilo.test.job".to_owned(),
        command,
        read: vec![program.parent().unwrap().to_owned()],
        read_write: Vec::new(),
    })
    .unwrap();
    let Launched { mut child, job } = launched;
    let asked = std::time::Instant::now();
    // Its holder lets the job go, as a daemon that ends does: the app ends with it.
    drop(job);
    let status = child.wait().unwrap();
    assert!(!status.success(), "it ended on its own: {status}");
    assert!(
        asked.elapsed() < std::time::Duration::from_secs(60),
        "the app outlived its job by {:?}",
        asked.elapsed()
    );
}
