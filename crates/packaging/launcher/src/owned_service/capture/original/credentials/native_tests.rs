//! Opt-in actual reduced-capability credential restoration, in owned namespaces.
//! This exercises the production sealed bootstrap, projection and enter path.
use super::*;
use crate::owned_service::capture::original::{self, Bootstrap, OriginalMountedInput};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt, fs::PermissionsExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MODE: &str = "TILO_ORIGINAL_DAC_RESTORATION_MODE";
const MARKER: &str = "TILO_ORIGINAL_DAC_RESTORATION_MARKER";
const SELECTOR: &str = "owned_service::capture::original::credentials::native_tests::original_projection_error_restores_actual_filesystem_credentials_without_claim";
const ROOT_PROOF: &str = "/run/tilo/migration/root-restoration-proof";
const COMPLETE: &[u8] = b"actual reduced-capability filesystem restoration and no claim";

fn named(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).unwrap()
}
fn mount(source: &CString, target: &CString, kind: Option<&CString>, flags: libc::c_ulong) {
    // SAFETY: CString pointers are live and terminate all named syscall inputs.
    assert_eq!(
        unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                kind.map_or(std::ptr::null(), |kind| kind.as_ptr()),
                flags,
                std::ptr::null(),
            )
        },
        0,
        "actual fixture mount failed: {}",
        io::Error::last_os_error()
    );
}
struct Mounted(CString);
impl Mounted {
    fn readonly(path: &Path) -> Self {
        let path = named(path);
        mount(&path, &path, None, libc::MS_BIND);
        mount(
            &CString::new("").unwrap(),
            &path,
            None,
            libc::MS_BIND
                | libc::MS_REMOUNT
                | libc::MS_RDONLY
                | libc::MS_NOSUID
                | libc::MS_NODEV
                | libc::MS_NOEXEC,
        );
        Self(path)
    }
}
impl Drop for Mounted {
    fn drop(&mut self) {
        // SAFETY: the owned mount pathname stays alive through the syscall.
        assert_eq!(
            unsafe { libc::umount2(self.0.as_ptr(), 0) },
            0,
            "owned source descriptors must close before mount retirement: {}",
            io::Error::last_os_error()
        );
    }
}
fn owned(path: &Path, uid: u32, gid: u32, mode: u32) {
    let path_name = named(path);
    // SAFETY: the owned fixture pathname is a live NUL-terminated input.
    assert_eq!(unsafe { libc::chown(path_name.as_ptr(), uid, gid) }, 0);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
fn pathfd(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .unwrap()
}
fn current_command(mode: &str) -> Command {
    let mut command = Command::new("/usr/bin/unshare");
    command.args(["--mount", "--propagation", "private"]);
    if mode == "inner" {
        command.args([
            "--pid",
            "--fork",
            "--kill-child=SIGKILL",
            "--net",
            "--ipc",
            "--uts",
            "--mount-proc",
        ]);
    }
    command
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", SELECTOR, "--nocapture", "--test-threads=1"])
        .env(MODE, mode)
        .stdin(Stdio::null());
    command
}
struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            // Kill and reap only this fixture's child, including unwind/error paths.
            // A prior try_wait may already have reaped it; kill/wait remain harmless.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn wait_owned(command: &mut Command) {
    let mut owned = OwnedChild(Some(command.spawn().unwrap()));
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(status) = owned.0.as_mut().unwrap().try_wait().unwrap() {
            // try_wait has reaped the exact child; disarm before any assertion.
            owned.0.take();
            assert!(
                status.success(),
                "actual isolated DAC restoration failed: {status}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "owned DAC restoration child timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn isolate_parent_death(command: &mut Command) {
    // SAFETY: getpid has no pointer arguments; pre_exec performs syscall-only work.
    let parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(io::ErrorKind::Interrupted.into());
            }
            Ok(())
        });
    }
}
fn assert_filter_launch_splices(
    lease: &original::FilterLaunch,
    bootstrap_fd: i32,
    baseline_fd: i32,
    original_roots: &[(i32, i32)],
) {
    let original = [
        "--add-seccomp-fd",
        "40",
        "--add-seccomp-fd",
        "41",
        "--unshare-all",
        "__sandbox-exec",
    ];
    let mut argv: Vec<String> = original.iter().map(|value| (*value).into()).collect();
    lease.splice_argv(&mut argv).unwrap();
    assert_eq!(
        argv,
        ["--add-seccomp-fd", "40", "--unshare-all", "__sandbox-exec"]
    );
    let mut changed: Vec<String> = original.iter().map(|value| (*value).into()).collect();
    changed[1] = "39".into();
    let before = changed.clone();
    assert!(lease.splice_argv(&mut changed).is_err());
    assert_eq!(changed, before);
    let mut baseline = vec![(bootstrap_fd, 49), (baseline_fd, 41), (baseline_fd, 40)];
    baseline.extend_from_slice(original_roots);
    let mut pairs = baseline.clone();
    lease.splice_pairs(&mut pairs).unwrap();
    let replacements = lease.descriptor_pairs();
    assert_eq!(replacements.len(), original_roots.len() + 2);
    assert_eq!(pairs.len(), baseline.len());
    for (input, slot) in &replacements {
        let matching: Vec<_> = pairs
            .iter()
            .filter(|(_, candidate)| candidate == slot)
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(*matching[0], (*input, *slot));
    }
    assert_eq!(pairs[2], (baseline_fd, 40));
    let required: Vec<_> = [49, 41]
        .into_iter()
        .chain(original_roots.iter().map(|(_, slot)| *slot))
        .collect();
    for slot in required {
        let mut missing = baseline.clone();
        missing.retain(|(_, candidate)| *candidate != slot);
        let mut duplicate = baseline.clone();
        duplicate.push(
            *baseline
                .iter()
                .find(|(_, candidate)| *candidate == slot)
                .unwrap(),
        );
        for mut refused in [missing, duplicate] {
            let before = refused.clone();
            assert!(lease.splice_pairs(&mut refused).is_err());
            assert_eq!(
                refused, before,
                "failed splice must not partially rewrite any slot"
            );
        }
    }
}
fn root_identities(pairs: &[(i32, i32)]) -> Vec<(i32, u64, u64, u64)> {
    use std::os::fd::FromRawFd;
    pairs
        .iter()
        .filter(|(_, slot)| original::roots::reserved_fd(*slot))
        .map(|(input, slot)| {
            // SAFETY: duplicate the live, retained production lease descriptor.
            let raw = unsafe { libc::fcntl(*input, libc::F_DUPFD_CLOEXEC, 128) };
            assert!(raw >= 0);
            let file = unsafe { File::from_raw_fd(raw) };
            let (device, inode, mount) =
                crate::owned_service::capture::inventory_scratch::identity(&file).unwrap();
            (*slot, device, inode, mount)
        })
        .collect()
}
fn assert_fresh_detached_leases(
    original: &[(i32, i32)],
    first: &original::FilterLaunch,
    second: &original::FilterLaunch,
) {
    let baseline = root_identities(original);
    let first = root_identities(&first.descriptor_pairs());
    let second = root_identities(&second.descriptor_pairs());
    assert!(!baseline.is_empty());
    assert_eq!(baseline.len(), first.len());
    assert_eq!(baseline.len(), second.len());
    for ((base, first), second) in baseline.iter().zip(&first).zip(&second) {
        assert_eq!((base.0, base.1, base.2), (first.0, first.1, first.2));
        assert_eq!((base.0, base.1, base.2), (second.0, second.1, second.2));
        assert_ne!(
            base.3, first.3,
            "first launch must own a fresh detached mount"
        );
        assert_ne!(
            base.3, second.3,
            "second launch must not reuse original custody"
        );
        assert_ne!(
            first.3, second.3,
            "concurrent launch leases must not share a detached mount"
        );
    }
    assert_eq!(
        root_identities(original),
        baseline,
        "cloning must leave original mount custody unchanged"
    );
}
fn outer() {
    // This /run overlay is visible only in our already-unshared mount namespace.
    let run = CString::new("/run").unwrap();
    let tmpfs = CString::new("tmpfs").unwrap();
    mount(&tmpfs, &run, Some(&tmpfs), libc::MS_NOSUID | libc::MS_NODEV);
    let private_run = Mounted(run);
    owned(Path::new("/run"), 0, 0, 0o755);
    let temporary = tempfile::tempdir_in("/run").unwrap();
    let approved = temporary.path().join("approved");
    std::fs::create_dir(&approved).unwrap();
    owned(&approved, 1000, 1001, 0o700);
    let approved_mount = Mounted::readonly(&approved);
    let source_path = Path::new(crate::owned_service::capture::SOURCE_PATH);
    std::fs::create_dir_all(source_path).unwrap();
    owned(Path::new("/run/tilo"), 0, 0, 0o755);
    owned(Path::new("/run/tilo/migration"), 0, 0, 0o755);
    owned(source_path, 1000, 1001, 0o700);
    let allowed = source_path.join("allowed");
    std::fs::create_dir(&allowed).unwrap();
    owned(&allowed, 1000, 1001, 0o700);
    std::fs::write(ROOT_PROOF, b"root-only filesystem readback").unwrap();
    owned(Path::new(ROOT_PROOF), 0, 0, 0o600);
    let blocked = source_path.join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::create_dir(blocked.join("target")).unwrap();
    owned(&blocked, 1003, 1003, 0o700);
    let source_mount = Mounted::readonly(source_path);
    let source = pathfd(source_path);
    // A genuine admitted Home without mounted children keeps Bubblewrap's
    // ordinary baseline; no filter lease or rewritten bootstrap is produced.
    let home_only = Bootstrap::admit(&source, 1000, 1001, &[1002], [7; 32]).unwrap();
    let untouched_token = home_only.descriptor();
    let empty_compiler: std::os::fd::OwnedFd = tempfile::tempfile().unwrap().into();
    assert!(
        home_only
            .filter_for_launch(&empty_compiler)
            .unwrap()
            .is_none()
    );
    assert_eq!(home_only.descriptor(), untouched_token);
    assert!(home_only.root_pairs().is_empty());
    assert!(
        home_only
            .facts
            .projection_filter_binding()
            .unwrap()
            .is_none()
    );
    // Inputs::admit rejects empty attachments today. Retain that refusal rather
    // than manufacturing a test-only source input or weakening admission.
    assert!(home_only.with_roots(Vec::new()).is_err());
    let input =
        OriginalMountedInput::from_reviewed("blocked/target".into(), pathfd(&approved)).unwrap();
    let mut bootstrap = Bootstrap::admit(&source, 1000, 1001, &[1002], [7; 32])
        .unwrap()
        .with_roots(vec![input])
        .unwrap();
    let bytes =
        sandbox_core::seccomp::compile_baseline(sandbox_core::seccomp::SeccompProfile::TiloDefault)
            .unwrap();
    let mut compiler = tempfile::tempfile().unwrap();
    std::io::Write::write_all(&mut compiler, &bytes).unwrap();
    let compiler: std::os::fd::OwnedFd = compiler.into();
    let retained_roots = std::mem::take(&mut bootstrap.facts.roots);
    assert!(bootstrap.filter_for_launch(&compiler).is_err());
    bootstrap.facts.roots = retained_roots;
    let filtered = bootstrap
        .filter_for_launch(&compiler)
        .unwrap()
        .expect("roots require canonical baseline custody");
    use std::os::fd::AsRawFd;
    let original_pairs = bootstrap.root_pairs();
    let second = bootstrap
        .filter_for_launch(&compiler)
        .unwrap()
        .expect("second real launch must receive separate detached custody");
    assert_fresh_detached_leases(&original_pairs, &filtered, &second);
    assert_eq!(bootstrap.root_pairs(), original_pairs);
    assert_filter_launch_splices(
        &filtered,
        bootstrap.descriptor(),
        compiler.as_raw_fd(),
        &original_pairs,
    );
    let pairs = filtered.descriptor_pairs();
    let mut command = current_command("inner");
    isolate_parent_death(&mut command);
    // SAFETY: inherited slots belong to this isolated fixture; dup2 is async-signal-safe.
    unsafe {
        command.pre_exec(move || {
            for (input, slot) in &pairs {
                if libc::dup2(*input, *slot) < 0 || libc::fcntl(*slot, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    wait_owned(&mut command);
    assert_eq!(bootstrap.root_pairs(), original_pairs);
    assert_fresh_detached_leases(&original_pairs, &filtered, &second);
    drop((filtered, second, bootstrap, source));
    drop(source_mount);
    std::fs::remove_dir_all(source_path).unwrap();
    drop(approved_mount);
    drop(temporary);
    drop(private_run);
    std::fs::write(std::env::var_os(MARKER).unwrap(), COMPLETE).unwrap();
}
fn reduce_capabilities() {
    let header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let allowed = (1 << 6) | (1 << 7) | (1 << 21); // SETGID, SETUID, SYS_ADMIN; no DAC override.
    let desired = [
        Data {
            effective: allowed,
            permitted: allowed,
            inheritable: 0,
        },
        Data {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
    ];
    // SAFETY: capset reads the exact ABI header and two live data entries.
    assert_eq!(
        unsafe { libc::syscall(libc::SYS_capset, &raw const header, desired.as_ptr()) },
        0
    );
    assert!(
        capabilities().unwrap() == desired,
        "actual capabilities differ from reduced set"
    );
}
fn assert_root_restored(ids: (u32, u32), original_groups: &[u32], caps: &[Data; 2]) {
    assert_eq!(filesystem_ids(), ids);
    assert_eq!(groups().unwrap(), original_groups);
    assert!(
        capabilities().unwrap() == *caps,
        "actual root capabilities were not restored"
    );
    // SAFETY: real/effective credential getters have no arguments.
    assert_eq!(rustix::process::getuid().as_raw(), 0);
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    assert_eq!(rustix::process::getgid().as_raw(), 0);
    assert_eq!(rustix::process::getegid().as_raw(), 0);
    assert_eq!(
        std::fs::read(ROOT_PROOF).unwrap(),
        b"root-only filesystem readback"
    );
    assert_eq!(
        unsafe { libc::fcntl(original::CLAIM_FD, libc::F_GETFD) },
        -1
    );
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
}
fn inner() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "native isolated root prerequisite required"
    );
    // SAFETY: the real isolated process installs distinct valid supplementary IDs.
    let root_groups = [1004_u32, 1005];
    assert_eq!(
        unsafe { libc::setgroups(root_groups.len(), root_groups.as_ptr()) },
        0
    );
    assert_eq!(groups().unwrap(), root_groups);
    reduce_capabilities();
    let ids = filesystem_ids();
    let original_groups = groups().unwrap();
    let caps = capabilities().unwrap();
    let facts = original::receive().unwrap();
    assert!(facts.projection_filter_binding().unwrap().is_some());
    let mut changed = facts.clone();
    changed.baseline_filter = None;
    assert!(changed.projection_filter_binding().is_err());
    changed = facts.clone();
    changed.roots.clear();
    assert!(changed.projection_filter_binding().is_err());
    changed = facts.clone();
    changed.baseline_filter.as_mut().unwrap().bytes = 7;
    assert!(changed.projection_filter_binding().is_err());
    original::validate_namespace(&facts).unwrap();
    assert_root_restored(ids, &original_groups, &caps);
    let result: io::Result<()> = with_source_filesystem_access(&facts, |access| {
        access.validate()?;
        assert_eq!(filesystem_ids(), (1000, 1001));
        assert_eq!(groups().unwrap(), vec![1002]);
        // A real allowed source placement proves ancestors do not cause the denial.
        drop(pathfd(Path::new("/run/tilo/migration/source/allowed")));
        assert_eq!(
            File::open(ROOT_PROOF).unwrap_err().raw_os_error(),
            Some(libc::EACCES)
        );
        assert!(
            capabilities().unwrap() == caps,
            "actual scoped capabilities changed"
        );
        let result: io::Result<()> = access.with_root_mount(|| {
            assert_eq!(filesystem_ids(), ids);
            assert_eq!(groups().unwrap(), original_groups);
            assert!(
                capabilities().unwrap() == caps,
                "actual scoped capabilities changed"
            );
            let source = CString::new("/proc/self/fd/1800").unwrap();
            let absent = CString::new("/run/tilo/migration/absent-mount-target").unwrap();
            // SAFETY: the inherited reviewed source FD and missing literal target
            // are live named inputs; this real syscall must fail without a mount.
            if unsafe {
                libc::mount(
                    source.as_ptr(),
                    absent.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            panic!("mount unexpectedly accepted absent target")
        });
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ENOENT));
        access.validate()?;
        assert_eq!(filesystem_ids(), (1000, 1001));
        assert_eq!(groups().unwrap(), vec![1002]);
        assert!(
            capabilities().unwrap() == caps,
            "actual scoped capabilities changed"
        );
        original::roots::project(&facts.roots, access)
    });
    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EACCES));
    assert_root_restored(ids, &original_groups, &caps);
    let mut keep = Vec::new();
    let failure = original::enter(&mut keep).unwrap_err();
    assert_eq!(failure.raw_os_error(), Some(libc::EACCES));
    assert!(
        keep.is_empty(),
        "failed projection cannot authorize any worker descriptor"
    );
    assert_root_restored(ids, &original_groups, &caps);
}

#[test]
fn original_projection_error_restores_actual_filesystem_credentials_without_claim() {
    match std::env::var(MODE).ok().as_deref() {
        Some("outer") => outer(),
        Some("inner") => inner(),
        Some(other) => panic!("unexpected owned fixture mode {other}"),
        None => {
            let witness = tempfile::tempdir().unwrap();
            std::fs::set_permissions(witness.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let marker: PathBuf = witness.path().join("complete");
            let mut command = current_command("outer");
            command.env(MARKER, &marker);
            isolate_parent_death(&mut command);
            wait_owned(&mut command);
            assert_eq!(std::fs::read(marker).unwrap(), COMPLETE);
        }
    }
}

#[test]
fn actual_private_home_final_observation_restores_root_before_sealing_mounted_receipt() {
    const CHILD: &str = "TILO_MOUNTED_RECEIPT_DAC_CHILD";
    const SELECTOR: &str = "owned_service::capture::original::credentials::native_tests::actual_private_home_final_observation_restores_root_before_sealing_mounted_receipt";
    const COMPLETE: &[u8] = b"private Home observed as caller; sealed receipt owned by root";
    if let Some(marker) = std::env::var_os(CHILD) {
        assert_eq!(
            rustix::process::geteuid().as_raw(),
            0,
            "actual root prerequisite required"
        );
        let run = CString::new("/run").unwrap();
        let tmpfs = CString::new("tmpfs").unwrap();
        mount(&tmpfs, &run, Some(&tmpfs), libc::MS_NOSUID | libc::MS_NODEV);
        let private_run = Mounted(run);
        owned(Path::new("/run"), 0, 0, 0o755);
        let source_path = Path::new(crate::owned_service::capture::SOURCE_PATH);
        std::fs::create_dir_all(source_path.join("mounted")).unwrap();
        owned(Path::new("/run/tilo"), 0, 0, 0o755);
        owned(Path::new("/run/tilo/migration"), 0, 0, 0o755);
        owned(source_path, 1000, 1001, 0o700);
        let nested = source_path.join("mounted");
        owned(&nested, 1000, 1001, 0o700);
        std::fs::write(nested.join("document"), b"private original bytes").unwrap();
        owned(&nested.join("document"), 1000, 1001, 0o600);
        let mounted = Mounted::readonly(source_path);
        let source = pathfd(source_path);
        let selected = pathfd(&nested);
        use std::os::unix::fs::MetadataExt;
        let m = selected.metadata().unwrap();
        let table = original::original_mounted_table_sha256(&[original::OriginalMountedIdentity {
            placement: b"mounted".to_vec(),
            identity: (m.dev(), m.ino(), m.uid(), m.gid(), m.mode()),
        }])
        .unwrap();
        let bootstrap = Bootstrap::admit(&source, 1000, 1001, &[1002], [7; 32])
            .unwrap()
            .with_bound_roots(
                vec![OriginalMountedInput::from_reviewed("mounted".into(), selected).unwrap()],
                &original::OriginalMountedBinding {
                    read_claim_digest: [7; 32],
                    table_sha256: table,
                },
            )
            .unwrap();
        reduce_capabilities();
        assert_eq!(
            std::fs::read(nested.join("document")).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let facts = &bootstrap.facts;
        let observed = with_source_filesystem_access(facts, |_access| {
            assert_eq!(filesystem_ids(), (1000, 1001));
            assert_eq!(
                std::fs::read(nested.join("document"))?,
                b"private original bytes"
            );
            original::roots::observe(facts)
        })
        .unwrap();
        assert_eq!(filesystem_ids(), (0, 0));
        original::roots::publish(observed).unwrap();
        let receipt = unsafe { libc::fcntl(51, libc::F_DUPFD_CLOEXEC, 128) };
        assert!(receipt >= 0);
        use std::os::fd::{AsRawFd, FromRawFd};
        let mut receipt = unsafe { File::from_raw_fd(receipt) };
        let metadata = receipt.metadata().unwrap();
        assert_eq!(metadata.uid(), 0, "receipt must not inherit caller fsuid");
        assert_eq!(
            unsafe { libc::fcntl(receipt.as_raw_fd(), libc::F_GET_SEALS) },
            original::SEALS
        );
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut receipt, &mut bytes).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["schema"], 1);
        assert_eq!(value["table_sha256"], serde_json::json!(table));
        assert_eq!(
            value["read_claim_digest"],
            serde_json::to_value([7_u8; 32]).unwrap()
        );
        assert_eq!(value["roots"].as_array().unwrap().len(), 1);
        drop((receipt, bootstrap, source));
        assert_eq!(unsafe { libc::close(51) }, 0);
        drop(mounted);
        // The original backing file is still present; the scope above performed no writes.
        // Root no longer has DAC override, so inspect bytes under the same explicit caller IDs.
        apply_filesystem_ids(1000, 1001, &[1002]).unwrap();
        assert_eq!(
            std::fs::read(nested.join("document")).unwrap(),
            b"private original bytes"
        );
        apply_filesystem_ids(0, 0, &[]).unwrap();
        // The private mount namespace owns all fixture paths; retire its /run overlay.
        drop(private_run);
        std::fs::write(marker, COMPLETE).unwrap();
        return;
    }
    let witness = tempfile::tempdir().unwrap();
    std::fs::set_permissions(witness.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let marker = witness.path().join("complete");
    let mut command = Command::new("/usr/bin/unshare");
    command
        .args(["--mount", "--propagation", "private"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", SELECTOR, "--nocapture", "--test-threads=1"])
        .env(CHILD, &marker)
        .stdin(Stdio::null());
    isolate_parent_death(&mut command);
    wait_owned(&mut command);
    assert_eq!(std::fs::read(marker).unwrap(), COMPLETE);
}
