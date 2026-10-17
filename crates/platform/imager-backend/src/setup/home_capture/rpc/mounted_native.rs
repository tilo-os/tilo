//! Actual admitted original-user grants and finite RPC into protected Store.
use super::*;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    io::{Read, Write},
    os::{
        fd::FromRawFd,
        unix::{ffi::OsStrExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command},
};
const CHILD: &str = "TILO_MOUNTED_RPC_FIXTURE_FD";
const PAIRED: &str = "TILO_MOUNTED_RPC_PAIRED_FULL_PLAN";
const PAIRED_PLAN_BOUND: usize = 9 * 1024 * 1024;
const CHILD_TEST: &str =
    "setup::home_capture::rpc::mounted_native::original_user_rpc_fixture_child";
struct Owned(Child);
impl Owned {
    fn finish(&mut self) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            if std::time::Instant::now() >= deadline {
                self.0.kill().unwrap();
                self.0.wait().unwrap();
                assert!(
                    std::time::Instant::now() < deadline,
                    "Actual original-user RPC fixture exceeded owned watchdog"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        self.0.wait().unwrap();
    }
}
struct Bind(PathBuf);
impl Bind {
    fn new(source: &Path, destination: &Path) -> Self {
        let source = std::ffi::CString::new(source.as_os_str().as_bytes()).unwrap();
        let target = std::ffi::CString::new(destination.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::mount(
                    source.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            },
            0,
            "{}",
            io::Error::last_os_error()
        );
        Self(destination.to_owned())
    }
}
impl Drop for Bind {
    fn drop(&mut self) {
        let target = std::ffi::CString::new(self.0.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::umount2(target.as_ptr(), 0) },
            0,
            "owned unmount after all grants close: {}",
            io::Error::last_os_error()
        );
    }
}
fn owner(path: &Path, uid: u32, gid: u32, mode: u32) {
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path_c.as_ptr(), uid, gid) }, 0);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn original_user_rpc_fixture_child() {
    let Ok(fd) = std::env::var(CHILD) else {
        return;
    };
    assert_eq!(rustix::process::geteuid().as_raw(), 1000);
    assert_eq!(rustix::process::getegid().as_raw(), 1001);
    let mut parent = unsafe { UnixStream::from_raw_fd(fd.parse().unwrap()) };
    let home = PathBuf::from(std::env::var_os("TILO_MOUNTED_RPC_HOME").unwrap());
    let request_dir = PathBuf::from(std::env::var_os("TILO_MOUNTED_RPC_REQUEST").unwrap());
    let operation = std::env::var("TILO_MOUNTED_RPC_OPERATION").unwrap();
    let root_pid = std::env::var("TILO_MOUNTED_RPC_PARENT")
        .unwrap()
        .parse()
        .unwrap();
    let cancel = AtomicBool::new(false);
    let primary = PinnedDirectory::open_absolute(&home).unwrap();
    let report =
        crate::setup::migration_review::MountedHomeLocations::discover(&primary, &home, &cancel)
            .unwrap();
    let refusal = report.require_single_root(&cancel).unwrap_err();
    let mut roots =
        crate::setup::portable_home_media::HomeSource::mounted_root_set(&refusal).unwrap();
    for (id, placement, approved) in roots.locations() {
        assert!(!approved);
        // Exercise the native boundary with a real selected descriptor. Platform
        // picker integration is covered by the frontend's grant tests.
        let request = roots.begin_request(&id, &cancel).unwrap();
        let path = home.join(placement);
        let selected = match request.kind() {
            crate::setup::RootKind::Directory => request.select_directory(path).unwrap(),
            crate::setup::RootKind::File => {
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(path)
                    .unwrap();
                request.select_file(file).unwrap()
            }
        };
        roots.approve(selected, &cancel).unwrap();
    }
    let roots = roots.into_granted_roots(&cancel).unwrap();
    let paired = match std::env::var(PAIRED) {
        Ok(value) => {
            assert_eq!(value, "1", "Exact paired fixture mode required");
            true
        }
        Err(std::env::VarError::NotPresent) => false,
        Err(error) => panic!("Invalid paired fixture mode: {error}"),
    };
    let plan = if paired {
        let bytes = worker::read_frame(&mut parent, PAIRED_PLAN_BOUND).unwrap();
        let plan: crate::setup::migration_handoff::Plan = serde_json::from_slice(&bytes).unwrap();
        plan.validate().unwrap();
        assert_eq!(plan.operation_id, operation);
        assert_eq!(plan.home_uid(), 1000);
        plan
    } else {
        crate::setup::migration_handoff::capture(
            &home,
            &operation,
            crate::setup::migration_handoff::Categories {
                home: true,
                flatpak: false,
                accounts: false,
            },
        )
        .unwrap()
    };
    let (rendezvous, listener) = Rendezvous::create(&request_dir).unwrap();
    let mut request =
        crate::setup::home_capture_helper::OriginalRequest::new(&home, plan, rendezvous).unwrap();
    if paired {
        // The real original-owner publication seam binds inventory bytes to the
        // private sidecar's device/inode/length before root admission reads it.
        let published =
            crate::setup::home_capture_helper::publish_original_request(&request_dir, &mut request)
                .unwrap();
        assert_eq!(published.metadata().unwrap().uid(), 1000);
        assert_eq!(published.metadata().unwrap().mode() & 0o777, 0o600);
        assert!(std::env::var("TILO_MOUNTED_RPC_CANCEL").is_err());
        assert!(std::env::var("TILO_MOUNTED_RPC_FAIL_ACK").is_err());
        assert!(std::env::var("TILO_MOUNTED_RPC_CANCEL_ACK").is_err());
    }
    worker::write_frame(
        &mut parent,
        &serde_json::to_vec(&request).unwrap(),
        64 * 1024,
    )
    .unwrap();
    let mut session = listener.accept(root_pid, &operation, &cancel).unwrap();
    let entries = roots
        .with_roots(&cancel, |granted| {
            Ok(granted[1..]
                .iter()
                .map(MountedInput::from_granted)
                .collect())
        })
        .unwrap();
    let generation = roots.generation();
    let cancelled = std::env::var("TILO_MOUNTED_RPC_CANCEL").as_deref() == Ok("1");
    if cancelled {
        cancel.store(true, Ordering::Release);
    }
    let cancelled_ack = std::env::var("TILO_MOUNTED_RPC_CANCEL_ACK").as_deref() == Ok("1");
    let cancellation = cancelled_ack.then(|| {
        let mut control = parent.try_clone().unwrap();
        let channel = session.channel.stream().try_clone().unwrap();
        std::thread::spawn(move || {
            let mut signal = [0];
            control.read_exact(&mut signal).unwrap();
            assert_eq!(signal, [9]);
            // Genuine client-owned transport cancellation; RootSession observes
            // actual EOF and its actor cancellation, not a fabricated bool.
            channel.shutdown(std::net::Shutdown::Both).unwrap();
        })
    });
    let result = session.call_with_mounted(
        Action::RegisterOriginalMounted {
            generation,
            entries,
        },
        None,
        Some(&roots),
        Some(&cancel),
        &mut |_| panic!("Registration cannot report archive progress"),
    );
    if cancelled {
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::Interrupted);
        session.shutdown();
        drop((session, roots, report, refusal, primary));
        parent
            .write_all(b"actual-caller-cancelled-before-root-registration")
            .unwrap();
        return;
    }
    if let Some(cancellation) = cancellation {
        cancellation.join().unwrap();
    }
    if cancelled_ack || std::env::var("TILO_MOUNTED_RPC_FAIL_ACK").as_deref() == Ok("1") {
        assert!(
            result.is_err(),
            "Actual broken reply transport cannot report registration success"
        );
        session.shutdown();
        drop((session, roots, report, refusal, primary));
        parent
            .write_all(b"actual-caller-failed-ack-no-success")
            .unwrap();
        return;
    }
    let reply = result.unwrap();
    match reply {
        Reply::MountedOriginalRegistered {
            generation: returned,
            witness,
            original_caller,
        } => {
            assert_eq!(returned, generation);
            assert_eq!(witness.operation(), operation);
            assert_eq!(original_caller.uid(), 1000);
            assert_eq!(original_caller.primary_gid(), 1001);
            assert_eq!(original_caller.supplementary_gids(), [1002, 1003]);
        }
        _ => panic!("Actual protected mounted witness required"),
    }
    session.shutdown();
    drop((session, roots, report, refusal, primary));
    parent
        .write_all(b"actual-caller-grants-and-protected-registration-complete")
        .unwrap();
}
#[test]
fn actual_original_user_grants_cross_authenticated_rpc_and_register_protected_roots() {
    run_original_user_case(false, false, false, Acknowledgement::Normal, 0);
}
#[test]
fn actual_original_user_cancel_retires_rpc_without_mounted_record_or_receipt() {
    run_original_user_case(true, false, false, Acknowledgement::Normal, 0);
}
#[test]
fn actual_foreign_rpc_ack_holds_primary_and_participant_leases_until_final_write() {
    run_original_user_case(false, true, false, Acknowledgement::Normal, 0);
}
#[test]
fn actual_foreign_rpc_failed_ack_retires_all_leases_without_sender_success() {
    run_original_user_case(false, true, true, Acknowledgement::Normal, 0);
}
#[test]
fn actual_foreign_rpc_cancelled_channel_retires_all_ack_custody() {
    run_original_user_case(false, true, false, Acknowledgement::Cancelled, 0);
}
#[test]
fn actual_foreign_rpc_queued_aggregate_corruption_never_admits_ack() {
    run_original_user_case(false, true, false, Acknowledgement::Normal, 1);
}
#[test]
fn actual_foreign_rpc_active_aggregate_replacement_never_admits_ack() {
    run_original_user_case(false, true, false, Acknowledgement::Normal, 2);
}
enum Acknowledgement {
    Normal,
    Cancelled,
}
fn run_original_user_case(
    cancelled: bool,
    foreign: bool,
    failed_ack: bool,
    acknowledgement: Acknowledgement,
    tamper: u8,
) {
    let cancelled_ack = matches!(acknowledgement, Acknowledgement::Cancelled);
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "isolated native root qualification required"
    );
    assert_eq!(
        unsafe { libc::unshare(libc::CLONE_NEWNS) },
        0,
        "private native mount namespace required"
    );
    assert_eq!(
        unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_PRIVATE | libc::MS_REC,
                std::ptr::null(),
            )
        },
        0
    );
    let source = PathBuf::from(
        std::env::var_os("TILO_ORIGINAL_SOURCE_GPT_STORE")
            .expect("actual unique disposable direct GPT filesystem required"),
    );
    let marker = source.join(".tilo-original-source-qualification");
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        b"disposable-original-source-only"
    );
    assert_eq!(std::fs::metadata(&marker).unwrap().uid(), 0);
    assert_eq!(std::fs::metadata(&marker).unwrap().mode() & 0o777, 0o600);
    let temp = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o711))
        .tempdir_in(&source)
        .unwrap();
    let home = temp.path().join("alice");
    std::fs::create_dir(&home).unwrap();
    owner(&home, 1000, 1001, 0o700);
    let state = temp.path().join("state");
    std::fs::create_dir(&state).unwrap();
    owner(&state, 1000, 1001, 0o700);
    let request_dir = temp.path().join("request");
    std::fs::create_dir(&request_dir).unwrap();
    owner(&request_dir, 1000, 1001, 0o700);
    let foreign_mount = foreign.then(|| {
        PathBuf::from(
            std::env::var_os("TILO_MOUNTED_PARTICIPANT_GPT_STORE")
                .expect("second actual unique disposable GPT filesystem required"),
        )
    });
    let foreign_temp = foreign_mount.as_ref().map(|mount| {
        let marker = mount.join(".tilo-original-source-qualification");
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            b"disposable-original-source-only"
        );
        assert_eq!(std::fs::metadata(&marker).unwrap().uid(), 0);
        assert_eq!(std::fs::metadata(&marker).unwrap().mode() & 0o777, 0o600);
        assert_ne!(
            std::fs::metadata(mount).unwrap().dev(),
            std::fs::metadata(&source).unwrap().dev()
        );
        let owned = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o711))
            .tempdir_in(mount)
            .unwrap();
        owner(owned.path(), 0, 0, 0o711);
        owned
    });
    let directory = foreign_temp.as_ref().map_or_else(
        || temp.path().join("group-directory"),
        |owned| owned.path().join("group-directory"),
    );
    std::fs::create_dir(&directory).unwrap();
    owner(&directory, 1003, 1002, 0o750);
    let file = directory.join("group-readable");
    std::fs::write(&file, b"group-only original bytes").unwrap();
    owner(&file, 1003, 1002, 0o640);
    let folder = home.join("mounted-directory");
    std::fs::create_dir(&folder).unwrap();
    owner(&folder, 1000, 1001, 0o700);
    let leaf = home.join("mounted-file");
    std::fs::write(&leaf, b"underlying original bytes").unwrap();
    owner(&leaf, 1000, 1001, 0o600);
    let store = temp.path().join("var/lib/tilo-imager/home-capture");
    std::fs::create_dir_all(&store).unwrap();
    owner(&store, 0, 0, 0o700);
    let target = Path::new("/var/lib/tilo-imager/home-capture");
    assert!(
        target.is_dir(),
        "canonical protected Store mountpoint must be explicitly provisioned in the isolated qualifier"
    );
    let binds = [
        Bind::new(&directory, &folder),
        Bind::new(&file, &leaf),
        Bind::new(&store, target),
    ];
    let operation = uuid::Uuid::new_v4().to_string();
    let (mut parent, child_socket) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .unwrap();
    let executable = File::open(std::env::current_exe().unwrap()).unwrap();
    let fds = [executable.as_raw_fd(), child_socket.as_raw_fd()];
    let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
    command
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env(CHILD, child_socket.as_raw_fd().to_string())
        .env("TILO_MOUNTED_RPC_HOME", &home)
        .env("TILO_MOUNTED_RPC_REQUEST", &request_dir)
        .env("TILO_MOUNTED_RPC_OPERATION", &operation)
        .env("TILO_MOUNTED_RPC_PARENT", std::process::id().to_string())
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("TILO_MOUNTED_RPC_CANCEL", if cancelled { "1" } else { "0" })
        .env(
            "TILO_MOUNTED_RPC_FAIL_ACK",
            if failed_ack || tamper != 0 { "1" } else { "0" },
        )
        .env(
            "TILO_MOUNTED_RPC_CANCEL_ACK",
            if cancelled_ack { "1" } else { "0" },
        );
    unsafe {
        command.pre_exec(move || {
            for fd in fds {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let groups = [1002, 1002, 1003];
            if libc::setgroups(groups.len(), groups.as_ptr()) != 0
                || libc::setgid(1001) != 0
                || libc::setuid(1000) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = Owned(command.spawn().unwrap());
    drop(child_socket);
    let bytes = worker::read_frame(&mut parent, 64 * 1024).unwrap();
    let request: crate::setup::home_capture_helper::OriginalRequest =
        serde_json::from_slice(&bytes).unwrap();
    assert_eq!(request.plan.operation_id, operation);
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut session, _) = RootSession::connect_original(
        request.channel_for_registration(),
        &request_dir,
        1000,
        &request,
        cancel,
    )
    .unwrap();
    let event = session
        .incoming
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();
    if cancelled {
        assert!(
            event.is_err(),
            "Cancelled peer cannot deliver registered witness"
        );
        assert!(
            !target
                .join("records")
                .join(format!("{operation}.mounted-roots.json"))
                .exists()
        );
        let expected = b"actual-caller-cancelled-before-root-registration";
        let mut marker = vec![0; expected.len()];
        parent.read_exact(&mut marker).unwrap();
        assert_eq!(marker, expected);
        assert!(child.finish().success());
        drop(session);
        drop(binds);
        assert_eq!(std::fs::read(&file).unwrap(), b"group-only original bytes");
        assert_eq!(std::fs::read(&leaf).unwrap(), b"underlying original bytes");
        return;
    }
    let mut incoming = event.unwrap();
    let participant_root = foreign_mount
        .as_ref()
        .map(|mount| mount.join("var/lib/tilo-imager/home-capture"));
    // This is the genuine RPC queue window: registration returned already,
    // but the helper has not begun or written any final acknowledgment.
    assert_operation_busy(super::super::Store::system(&operation));
    if let Some(root) = &participant_root {
        assert_operation_busy(super::super::Store::open(
            PinnedDirectory::open_absolute(root).unwrap(),
            &operation,
        ));
    }
    assert!(incoming.output.is_none());
    let generation = match &incoming.call.action {
        Action::RegisterOriginalMounted { generation, .. } => Some(*generation),
        _ => None,
    }
    .expect("Exact mounted action required");
    let aggregate_path = target
        .join("records")
        .join(format!("{operation}.mounted-roots.json"));
    let original_aggregate = std::fs::read(&aggregate_path).unwrap();
    if tamper == 1 {
        std::fs::write(&aggregate_path, b"actual queued aggregate corruption").unwrap();
    }
    let begun = session.begin(&mut incoming);
    if tamper == 1 {
        assert_eq!(
            begun.as_ref().unwrap_err().to_string(),
            "Mounted aggregate changed before acknowledgement"
        );
    } else {
        begun.as_ref().unwrap();
    }
    let (returned, witness) = incoming.mounted.take().unwrap();
    drop(incoming);
    if tamper != 1 {
        // After Incoming is consumed, the private RootSession guard still owns
        // primary AND foreign participant operation locks through the ACK write.
        assert_operation_busy(super::super::Store::system(&operation));
        if let Some(root) = &participant_root {
            assert_operation_busy(super::super::Store::open(
                PinnedDirectory::open_absolute(root).unwrap(),
                &operation,
            ));
        }
    }
    assert_eq!(returned, generation);
    let protected = target
        .join("records")
        .join(format!("{operation}.mounted-roots.json"));
    let protected_bytes = std::fs::read(&protected).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&original_aggregate).unwrap();
    assert_eq!(record["caller"]["uid"], 1000);
    assert_eq!(record["entries"].as_array().unwrap().len(), 2);
    assert!(
        record["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["binding"]["home"]["original_uid"] == 1003)
    );
    assert_eq!(std::fs::metadata(&protected).unwrap().uid(), 0);
    if cancelled_ack {
        parent.write_all(&[9]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !session.cancel.load(Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "RootSession must observe actual client EOF"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_operation_busy(super::super::Store::system(&operation));
    }
    if failed_ack {
        // Real socket shutdown makes the canonical final reply write fail.
        // No fake cancellation/controller outcome or fake lease is injected.
        session.writer.shutdown(std::net::Shutdown::Both).unwrap();
    }
    if tamper == 2 {
        let replacement = protected.with_extension("replacement");
        std::fs::write(&replacement, &protected_bytes).unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::rename(&replacement, &protected).unwrap();
    }
    let reply = if tamper == 1 {
        session.writer.shutdown(std::net::Shutdown::Both).unwrap();
        begun
    } else {
        let original_caller = session
            .registration
            .as_ref()
            .unwrap()
            .1
            .original_caller()
            .unwrap();
        session.reply(&Reply::MountedOriginalRegistered {
            generation,
            witness,
            original_caller,
        })
    };
    if tamper == 2 {
        assert_eq!(
            reply.as_ref().unwrap_err().to_string(),
            "Mounted aggregate changed before acknowledgement"
        );
    }
    if failed_ack || cancelled_ack || tamper != 0 {
        assert!(reply.is_err());
    } else {
        reply.unwrap();
    }
    drop(
        super::super::Store::system(&operation)
            .expect("Primary lock must retire after actual reply attempt"),
    );
    if let Some(root) = &participant_root {
        drop(
            super::super::Store::open(PinnedDirectory::open_absolute(root).unwrap(), &operation)
                .expect("Participant lock must retire after actual reply attempt"),
        );
    }
    let expected: &[u8] = if failed_ack || cancelled_ack || tamper != 0 {
        b"actual-caller-failed-ack-no-success"
    } else {
        b"actual-caller-grants-and-protected-registration-complete"
    };
    let mut completed = vec![0; expected.len()];
    parent.read_exact(&mut completed).unwrap();
    assert_eq!(completed, expected);
    assert!(child.finish().success());
    drop(session);
    drop(binds);
    assert_eq!(std::fs::read(&file).unwrap(), b"group-only original bytes");
    assert_eq!(std::fs::read(&leaf).unwrap(), b"underlying original bytes");
    if let Some(root) = participant_root {
        // All actual guards and source descriptors are gone. Retire only our
        // exact immutable operation files, not any other protected namespace.
        std::fs::remove_file(
            root.join("records")
                .join(format!("{operation}.mounted-participant.json")),
        )
        .unwrap();
        std::fs::remove_file(root.join("records").join(format!("{operation}.lock"))).unwrap();
    }
}

fn assert_operation_busy(result: io::Result<super::super::Store>) {
    let error = result
        .err()
        .expect("Actual operation lease must remain held before final RPC reply");
    assert!(
        matches!(
            error.get_ref().and_then(
                |cause| cause.downcast_ref::<tilo_files::native::atomic::lease::AcquireError>()
            ),
            Some(tilo_files::native::atomic::lease::AcquireError::Busy)
        ),
        "Expected actual conflicting native lease, got {error}"
    );
}

/// Runs the actual original-owner chooser and authenticated mounted registration
/// for a caller-owned complete plan. Caller owns the source/participant mounts
/// and canonical protected Store bind throughout the returned witnesses' use.
pub(in crate::setup::home_capture) fn register_for_paired(
    home: &Path,
    plan: &crate::setup::migration_handoff::Plan,
    request_dir: &Path,
) -> (
    super::super::SourceWitness,
    super::super::MountedWitness,
    super::super::OriginalCallerFacts,
) {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "Native root fixture required"
    );
    plan.validate().unwrap();
    assert_eq!(plan.home_uid(), 1000);
    let request_facts = std::fs::metadata(request_dir).unwrap();
    assert_eq!(request_facts.uid(), 1000);
    assert_eq!(request_facts.mode() & 0o777, 0o700);
    let (mut parent, child_socket) = UnixStream::pair().unwrap();
    for socket in [&parent, &child_socket] {
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .unwrap();
        socket
            .set_write_timeout(Some(std::time::Duration::from_secs(30)))
            .unwrap();
    }
    let executable = File::open(std::env::current_exe().unwrap()).unwrap();
    let fds = [executable.as_raw_fd(), child_socket.as_raw_fd()];
    let mut command = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()));
    command
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env(CHILD, child_socket.as_raw_fd().to_string())
        .env(PAIRED, "1")
        .env("TILO_MOUNTED_RPC_HOME", home)
        .env("TILO_MOUNTED_RPC_REQUEST", request_dir)
        .env("TILO_MOUNTED_RPC_OPERATION", &plan.operation_id)
        .env("TILO_MOUNTED_RPC_PARENT", std::process::id().to_string())
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env_remove("TILO_MOUNTED_RPC_CANCEL")
        .env_remove("TILO_MOUNTED_RPC_FAIL_ACK")
        .env_remove("TILO_MOUNTED_RPC_CANCEL_ACK");
    unsafe {
        command.pre_exec(move || {
            for fd in fds {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let groups = [1002, 1002, 1003];
            if libc::setgroups(groups.len(), groups.as_ptr()) != 0
                || libc::setgid(1001) != 0
                || libc::setuid(1000) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = Owned(command.spawn().unwrap());
    drop(child_socket);
    worker::write_frame(
        &mut parent,
        &serde_json::to_vec(plan).unwrap(),
        PAIRED_PLAN_BOUND,
    )
    .unwrap();
    let bytes = worker::read_frame(&mut parent, 64 * 1024).unwrap();
    let request: crate::setup::home_capture_helper::OriginalRequest =
        serde_json::from_slice(&bytes).unwrap();
    assert_eq!(request.plan.operation_id, plan.operation_id);
    assert_eq!(request.home.decode().unwrap(), home);
    // The request has the protected inventory binding, whereas the complete
    // selected plan supplied above still carries the actual reviewed inventory.
    let (expected_plan, _) = plan.original_inventory_selection().unwrap();
    assert_eq!(
        serde_json::to_value(&request.plan).unwrap(),
        serde_json::to_value(expected_plan).unwrap(),
    );
    let (mut session, (admitted_source, original_caller)) = RootSession::connect_original(
        request.channel_for_registration(),
        request_dir,
        1000,
        &request,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(original_caller.uid(), 1000);
    assert_eq!(original_caller.primary_gid(), 1001);
    assert_eq!(original_caller.supplementary_gids(), [1002, 1003]);
    let mut incoming = session
        .incoming
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap()
        .unwrap();
    assert!(incoming.output.is_none());
    let (generation, entries) = match &incoming.call.action {
        Action::RegisterOriginalMounted {
            generation,
            entries,
        } => Some((generation, entries)),
        _ => None,
    }
    .expect("Actual mounted registration action required");
    assert!(
        entries
            .iter()
            .any(|entry| { entry.kind == crate::setup::migration_handoff::EntryKind::Directory })
    );
    assert!(
        entries
            .iter()
            .any(|entry| { entry.kind == crate::setup::migration_handoff::EntryKind::File })
    );
    let generation = *generation;
    assert_operation_busy(super::super::Store::system(&plan.operation_id));
    session.begin(&mut incoming).unwrap();
    let (returned, mounted) = incoming.mounted.take().unwrap();
    assert_eq!(returned, generation);
    let source = mounted.primary().clone();
    assert_eq!(source, admitted_source);
    mounted.matches_source(&source).unwrap();
    drop(incoming);
    let retained_caller = session
        .registration
        .as_ref()
        .unwrap()
        .1
        .original_caller()
        .unwrap();
    assert_eq!(retained_caller, original_caller);
    session
        .reply(&Reply::MountedOriginalRegistered {
            generation,
            witness: mounted.clone(),
            original_caller: retained_caller,
        })
        .unwrap();
    let expected = b"actual-caller-grants-and-protected-registration-complete";
    let mut completed = vec![0; expected.len()];
    parent.read_exact(&mut completed).unwrap();
    assert_eq!(completed, expected);
    assert!(child.finish().success());
    drop(session);
    drop(super::super::Store::system(&plan.operation_id).unwrap());
    (source, mounted, original_caller)
}
