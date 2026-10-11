//! A device-valid commit is not an executing-source-bound side-effect lease.
use super::*;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::{net::UnixStream, process::CommandExt as _};
use std::process::Command;
use tilo_action_broker::services::LauncherServices as _;

const TEST: &str = "standalone::owner::tests::source_proof_tests::device_commit_request_cannot_fence_a_retired_executing_source";
const HELPER: &str = "TILO_OWNER_SOURCE_PROOF_FD";

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn held_child(reexec: bool) -> (Child, UnixStream, launcher::services::ExecutingPeer) {
    let (mut host, app) = UnixStream::pair().unwrap();
    runtime_portal::process_binding::arm(&host).unwrap();
    host.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let fd = app.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--nocapture"])
        .env(HELPER, fd.to_string())
        .env(
            "TILO_OWNER_PROOF_SAME_SOURCE",
            if reexec { "1" } else { "0" },
        );
    // SAFETY: only fcntl runs in the child between fork and exec, on our FD.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = Child(command.spawn().unwrap());
    drop(app);
    let nonce = [71; 32];
    host.write_all(&runtime_portal::process_binding::MAGIC)
        .unwrap();
    host.write_all(&nonce).unwrap();
    let peer = runtime_portal::process_binding::receive(&host, &nonce, unsafe { libc::geteuid() })
        .unwrap();
    host.write_all(&[1]).unwrap();
    (child, host, peer)
}

#[test]
fn device_commit_request_cannot_fence_a_retired_executing_source() {
    if let Ok(raw) = std::env::var(HELPER) {
        let raw: i32 = raw.parse().unwrap();
        // SAFETY: the parent transferred this one owned inherited socket.
        let mut stream = unsafe { UnixStream::from_raw_fd(raw) };
        if std::env::var_os("TILO_OWNER_PROOF_AFTER_EXEC").is_some() {
            stream.write_all(&[2]).unwrap();
            let mut ended = [0];
            let _ = stream.read_exact(&mut ended);
            return;
        }
        runtime_portal::process_binding::complete(&mut stream).unwrap();
        let mut exec = [0];
        stream.read_exact(&mut exec).unwrap();
        assert_eq!(exec, [1]);
        let error = if std::env::var("TILO_OWNER_PROOF_SAME_SOURCE").unwrap() == "1" {
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env("TILO_OWNER_PROOF_AFTER_EXEC", "1")
                .exec()
        } else {
            Command::new("/bin/sleep").arg("30").exec()
        };
        panic!("disposable exec failed: {error}");
    }
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let mut document = automerge::AutoCommit::new();
    commit(
        &owner,
        &write(&profile, &mut document, "before"),
        Some(KIND),
    )
    .unwrap();
    let request = write(&profile, &mut document, "queued-before-source-replacement");
    let apps = crate::apps::Apps::new(
        None,
        profile.dir.path(),
        &profile.room,
        PathBuf::new(),
        Vec::new(),
    )
    .with_development_mode(true);
    let (child, mut control, peer) = held_child(false);
    let launcher = std::process::id();
    let source = apps
        .launch_source(APP, &std::env::current_exe().unwrap())
        .unwrap();
    apps.register_independent(launcher, child.0.id(), (APP, &profile.room), source)
        .unwrap();
    assert!(apps.attest_executing_process(launcher, &peer, APP, &profile.room));
    let custody = apps
        .authenticated_launch(launcher, APP, &profile.room)
        .unwrap()
        .unwrap()
        .execution_custody
        .unwrap();
    assert!((custody.0)(child.0.id()));
    control.write_all(&[1]).unwrap();
    tilo_test_env::wait_until("actual exec retires native source custody", || {
        !(custody.0)(child.0.id())
    });
    assert!(
        peer.current(),
        "pidfd still identifies the same live process after exec"
    );
    assert!(!apps.attest_executing_process(launcher, &peer, APP, &profile.room));
    // The real canonical owner validates device/governance/signature and
    // durability. Its request does not carry this host-local executing lease.
    let receipt = commit(&owner, &request, None).unwrap();
    receipt.validate_for(&request).unwrap();
    assert_eq!(
        text(&owner).as_deref(),
        Some("queued-before-source-replacement")
    );
    drop(owner);
    assert_eq!(
        text(&open(&profile, creation(true, false))).as_deref(),
        Some("queued-before-source-replacement")
    );
}

#[test]
fn same_executable_reexec_is_not_observed_by_the_source_stamp() {
    let profile = profile();
    let apps = crate::apps::Apps::new(
        None,
        profile.dir.path(),
        &profile.room,
        PathBuf::new(),
        Vec::new(),
    )
    .with_development_mode(true);
    let (child, mut control, peer) = held_child(true);
    let launcher = std::process::id();
    let source = apps
        .launch_source(APP, &std::env::current_exe().unwrap())
        .unwrap();
    apps.register_independent(launcher, child.0.id(), (APP, &profile.room), source)
        .unwrap();
    assert!(apps.attest_executing_process(launcher, &peer, APP, &profile.room));
    let custody = apps
        .authenticated_launch(launcher, APP, &profile.room)
        .unwrap()
        .unwrap()
        .execution_custody
        .unwrap();
    assert!((custody.0)(child.0.id()));
    control.write_all(&[1]).unwrap();
    let mut replaced = [0];
    control.read_exact(&mut replaced).unwrap();
    assert_eq!(
        replaced,
        [2],
        "the actual exec'd program acknowledged its replacement phase"
    );
    assert!(peer.current());
    assert!(
        (custody.0)(child.0.id()),
        "the code stamp is not an exec generation fence"
    );
    assert!(apps.attest_executing_process(launcher, &peer, APP, &profile.room));
}
