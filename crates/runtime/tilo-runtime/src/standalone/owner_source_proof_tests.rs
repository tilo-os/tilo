//! A device-valid commit is not an executing-source-bound side-effect lease.
use super::*;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::{net::UnixStream, process::CommandExt as _};
use std::process::Command;
use tilo_action_broker::services::LauncherServices as _;
use tilo_peer_state::owner_commit::{DocumentAccess, DocumentCommitLease, DocumentIntent};

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

fn trace_stop(pid: u32) -> i32 {
    let waiting = i32::try_from(pid).unwrap();
    let mut status = 0;
    tilo_test_env::wait_until("owned child kernel exec stop", || {
        // SAFETY: initialized wait status; wait for only our owned child.
        let result = unsafe { libc::waitpid(waiting, &raw mut status, libc::WNOHANG) };
        assert!(result == 0 || result == waiting, "owned child wait failed");
        result != 0
    });
    assert!(
        libc::WIFSTOPPED(status),
        "expected owned child ptrace stop: {status}"
    );
    status
}

fn trace_continue(pid: u32) {
    // SAFETY: resume only our stopped, traced child without injecting a signal.
    assert_eq!(unsafe { libc::ptrace(libc::PTRACE_CONT, pid, 0, 0) }, 0);
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

fn lease(
    profile: &Profile,
    check: Box<dyn Fn() -> Result<(), String> + Send + Sync>,
) -> Arc<DocumentCommitLease> {
    Arc::new(DocumentCommitLease::new(
        DocumentIntent::new(
            APP.into(),
            profile.room.clone(),
            vec![(RECORD.into(), DocumentAccess::Write)],
            SystemTime::now()
                .checked_add(std::time::Duration::from_secs(60))
                .unwrap(),
        )
        .unwrap(),
        check,
    ))
}

fn guarded(
    owner: &RoomOwner,
    request: OwnerCommitRequest,
    lease: Arc<DocumentCommitLease>,
) -> Result<OwnerCommitReceipt, String> {
    tilo_rt::try_app()
        .unwrap()
        .block_on(owner.commit_with_document_lease(request, lease))
        .unwrap()
}

#[test]
fn document_lease_commits_exact_existing_record_then_refuses_process_exit() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let mut document = automerge::AutoCommit::new();
    commit(
        &owner,
        &write(&profile, &mut document, "before"),
        Some(KIND),
    )
    .unwrap();
    let (mut child, _control, peer) = held_child(false);
    let lease = lease(
        &profile,
        Box::new(move || {
            peer.current()
                .then_some(())
                .ok_or_else(|| "executing process ended".into())
        }),
    );
    let request = write(&profile, &mut document, "guarded");
    guarded(&owner, request.clone(), Arc::clone(&lease))
        .unwrap()
        .validate_for(&request)
        .unwrap();
    let queued = write(&profile, &mut document, "after-exit");
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(
        guarded(&owner, queued, lease)
            .unwrap_err()
            .contains("executing process ended")
    );
    assert_eq!(text(&owner).as_deref(), Some("guarded"));
    drop(owner);
    assert_eq!(
        text(&open(&profile, creation(true, false))).as_deref(),
        Some("guarded")
    );
}

#[test]
fn document_lease_refuses_queued_revocation_and_downgrade_at_owner() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let mut document = automerge::AutoCommit::new();
    commit(
        &owner,
        &write(&profile, &mut document, "before"),
        Some(KIND),
    )
    .unwrap();
    let queued = write(&profile, &mut document, "queued");
    let revoked = lease(&profile, Box::new(|| Ok(())));
    revoked.revoke();
    assert!(
        guarded(&owner, queued.clone(), revoked)
            .unwrap_err()
            .contains("revoked")
    );
    let downgraded = lease(&profile, Box::new(|| Ok(())));
    downgraded.downgrade_to_read();
    assert!(
        guarded(&owner, queued, downgraded)
            .unwrap_err()
            .contains("read-only")
    );
    assert_eq!(text(&owner).as_deref(), Some("before"));
}

#[test]
fn document_lease_refuses_expiry_at_owner_without_extending_duration() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let mut document = automerge::AutoCommit::new();
    commit(
        &owner,
        &write(&profile, &mut document, "before"),
        Some(KIND),
    )
    .unwrap();
    let queued = write(&profile, &mut document, "after-deadline");
    let deadline = SystemTime::now() + std::time::Duration::from_secs(1);
    let lease = Arc::new(DocumentCommitLease::new(
        DocumentIntent::new(
            APP.into(),
            profile.room.clone(),
            vec![(RECORD.into(), DocumentAccess::Write)],
            deadline,
        )
        .unwrap(),
        Box::new(|| Ok(())),
    ));
    std::thread::sleep(
        deadline
            .duration_since(SystemTime::now())
            .unwrap_or_default(),
    );
    assert!(
        guarded(&owner, queued, lease)
            .unwrap_err()
            .contains("expired")
    );
    assert_eq!(text(&owner).as_deref(), Some("before"));
}

#[test]
fn document_lease_rechecks_execution_at_durable_publication() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let mut document = automerge::AutoCommit::new();
    commit(
        &owner,
        &write(&profile, &mut document, "before"),
        Some(KIND),
    )
    .unwrap();
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let lease = lease(
        &profile,
        Box::new(move || {
            if checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Ok(())
            } else {
                Err("execution fenced after queue admission".into())
            }
        }),
    );
    let queued = write(&profile, &mut document, "must-not-save");
    assert!(
        guarded(&owner, queued, lease)
            .unwrap_err()
            .contains("after queue admission")
    );
    drop(owner);
    assert_eq!(
        text(&open(&profile, creation(true, false))).as_deref(),
        Some("before")
    );
}

#[test]
fn document_lease_cannot_bootstrap_or_widen_the_selected_operation() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let mut document = automerge::AutoCommit::new();
    let request = write(&profile, &mut document, "must-not-create");
    assert!(
        guarded(
            &owner,
            request.clone(),
            lease(&profile, Box::new(|| Ok(())))
        )
        .unwrap_err()
        .contains("existing record")
    );
    commit(&owner, &request, Some(KIND)).unwrap();
    for target in ["room", "app", "record", "family"] {
        let mut outside = write(&profile, &mut document, "outside");
        match target {
            "room" => outside.room = tilo_desktop_model::RoomId::new().to_string(),
            "app" | "record" => {
                let fields = match &mut outside.operation {
                    CommitOperation::Document { app, record, .. } => Some((app, record)),
                    _ => None,
                };
                let (app, record) = fields.expect("signed fixture must be a document");
                if target == "app" {
                    *app = "another.app".into();
                } else {
                    *record = "another.record".into();
                }
            }
            _ => {
                outside.operation = CommitOperation::EffectReceipt {
                    invocation: uuid::Uuid::new_v4(),
                    redacted: Vec::new(),
                }
            }
        }
        assert!(
            guarded(&owner, outside, lease(&profile, Box::new(|| Ok(())))).is_err(),
            "{target}"
        );
    }
    let readonly = Arc::new(DocumentCommitLease::new(
        DocumentIntent::new(
            APP.into(),
            profile.room.clone(),
            vec![(RECORD.into(), DocumentAccess::Read)],
            SystemTime::now() + std::time::Duration::from_secs(60),
        )
        .unwrap(),
        Box::new(|| Ok(())),
    ));
    assert!(
        guarded(
            &owner,
            write(&profile, &mut document, "read-only-write"),
            readonly
        )
        .unwrap_err()
        .contains("Write records")
    );
    assert_eq!(text(&owner).as_deref(), Some("must-not-create"));
}

#[test]
fn owned_parent_exec_event_fences_same_image_before_replacement_runs() {
    use std::process::Stdio;
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let mut document = automerge::AutoCommit::new();
    commit(
        &owner,
        &write(&profile, &mut document, "before"),
        Some(KIND),
    )
    .unwrap();
    // A shell has one thread, unlike Rust's test harness. This exercises only
    // a disposable parent-owned kernel exec event, not SDK launch admission.
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "printf 1; read command; exec /bin/sh -c 'printf 2; read ended'",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    // SAFETY: one ptrace syscall in our child before its first exec.
    unsafe {
        command.pre_exec(|| {
            if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = Child(command.spawn().unwrap());
    let status = trace_stop(child.0.id());
    assert_eq!(libc::WSTOPSIG(status), libc::SIGTRAP);
    // SAFETY: this thread is the parent tracer of this exact stopped child.
    assert_eq!(
        unsafe {
            libc::ptrace(
                libc::PTRACE_SETOPTIONS,
                child.0.id(),
                0,
                libc::PTRACE_O_TRACEEXEC | libc::PTRACE_O_EXITKILL,
            )
        },
        0
    );
    let original = std::fs::read_link(format!("/proc/{}/exe", child.0.id())).unwrap();
    trace_continue(child.0.id());
    let mut acknowledged = [0];
    child
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut acknowledged)
        .unwrap();
    assert_eq!(acknowledged, [b'1']);
    let lease = lease(&profile, Box::new(|| Ok(())));
    let queued = write(&profile, &mut document, "queued-before-same-image-exec");
    child
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"exec\n")
        .unwrap();
    let status = trace_stop(child.0.id());
    assert_eq!(libc::WSTOPSIG(status), libc::SIGTRAP);
    assert_eq!(
        status >> 16,
        libc::PTRACE_EVENT_EXEC,
        "actual same-image exec event"
    );
    assert_eq!(
        std::fs::read_link(format!("/proc/{}/exe", child.0.id())).unwrap(),
        original
    );
    // The new image is stopped. This proves event observation and lease
    // withdrawal; pre-syscall exec/commit serialization still needs wiring.
    lease.revoke();
    assert!(
        guarded(&owner, queued, lease)
            .unwrap_err()
            .contains("revoked")
    );
    trace_continue(child.0.id());
    child
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut acknowledged)
        .unwrap();
    assert_eq!(acknowledged, [b'2']);
    drop(child);
    drop(owner);
    assert_eq!(
        text(&open(&profile, creation(true, false))).as_deref(),
        Some("before")
    );
}
