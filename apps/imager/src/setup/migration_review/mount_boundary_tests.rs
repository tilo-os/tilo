//! Real disposable mount qualification, never a fake inventory or skipped test.
use super::*;
use crate::setup::migration::PinnedDirectory;
use std::{
    ffi::CString,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    process::{Child, Command, ExitStatus},
    time::{Duration, Instant},
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

const CHILD: &str = "TILO_HOME_MOUNT_BOUNDARY_CHILD";
const WITNESS: &str = "TILO_HOME_MOUNT_BOUNDARY_WITNESS";
const COMPLETE: &[u8] = b"all-mount-assertions-complete\n";

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn bounded_child(command: &mut Command) -> ExitStatus {
    let mut child = OwnedChild(
        command
            .spawn()
            .expect("actual private mount namespace required"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.0.try_wait().expect("observe owned fixture child") {
            return status;
        }
        if Instant::now() >= deadline {
            child
                .0
                .kill()
                .expect("terminate exact stalled fixture child");
            child.0.wait().expect("reap exact stalled fixture child");
            panic!("mounted Home qualification exceeded bounded watchdog");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
const SELECTOR: &str = "setup::migration_review::mount_boundary_tests::actual_same_filesystem_directory_and_file_binds_refuse_home_review";

fn target_mount(target: &Path) -> u64 {
    use std::os::fd::FromRawFd;
    let target = CString::new(target.as_os_str().as_bytes()).unwrap();
    // SAFETY: literal disposable target; O_PATH pins metadata without reading.
    let descriptor = unsafe {
        libc::open(
            target.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    assert!(
        descriptor >= 0,
        "observe actual fixture mount: {}",
        io::Error::last_os_error()
    );
    // SAFETY: successful open returned a fresh owned descriptor.
    let file = unsafe { std::fs::File::from_raw_fd(descriptor) };
    fs_native::snapshot_lease::mount_id(&file).unwrap()
}
mod grant_tests;

struct Binding {
    target: PathBuf,
    previous_mount: u64,
    mount: u64,
    retired: bool,
}
impl Binding {
    fn mount(source: &Path, target: &Path) -> Self {
        let previous_mount = target_mount(target);
        let source = CString::new(source.as_os_str().as_bytes()).unwrap();
        let target_c = CString::new(target.as_os_str().as_bytes()).unwrap();
        // SAFETY: isolated fixture namespace, live terminated paths, bind only
        // the disposable synthetic files this fixture owns.
        assert_eq!(
            unsafe {
                libc::mount(
                    source.as_ptr(),
                    target_c.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            },
            0,
            "real bind mount required: {}",
            io::Error::last_os_error()
        );
        let mount = target_mount(target);
        assert_ne!(mount, previous_mount);
        Self {
            target: target.to_path_buf(),
            previous_mount,
            mount,
            retired: false,
        }
    }
    /// Explicit fixture topology change while reviewed descriptors still pin
    /// the detached mount; ordinary teardown remains non-lazy below.
    fn retire(mut self) {
        assert_eq!(
            target_mount(&self.target),
            self.mount,
            "retire only the exact owned fixture mount"
        );
        let target = CString::new(self.target.as_os_str().as_bytes()).unwrap();
        // SAFETY: exact recorded fixture mount in its private namespace only.
        assert_eq!(
            unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) },
            0,
            "retire owned fixture topology: {}",
            io::Error::last_os_error()
        );
        self.retired = true;
        assert_eq!(
            target_mount(&self.target),
            self.previous_mount,
            "target must resolve to its original namespace mount after retirement"
        );
    }
}
impl Drop for Binding {
    fn drop(&mut self) {
        if self.retired {
            return;
        }
        assert_eq!(
            target_mount(&self.target),
            self.mount,
            "ordinary teardown must still own the exact fixture mount"
        );
        let target = CString::new(self.target.as_os_str().as_bytes()).unwrap();
        // SAFETY: unmount only this fixture's exact disposable bind target.
        assert_eq!(
            unsafe { libc::umount2(target.as_ptr(), 0) },
            0,
            "release real bind: {}",
            io::Error::last_os_error()
        );
        assert_eq!(target_mount(&self.target), self.previous_mount);
    }
}

#[test]
fn actual_same_filesystem_directory_and_file_binds_refuse_home_review() {
    // SAFETY: geteuid only observes the test process's current credentials.
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "canonical privileged native qualification required"
    );
    if std::env::var_os(CHILD).is_none() {
        let witness = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let witness_root = std::fs::canonicalize(witness.path()).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", SELECTOR, "--nocapture"])
            .env(CHILD, "1")
            .env(WITNESS, &witness_root);
        // SAFETY: child-only, async-signal-safe namespace/mount syscalls before
        // exec; no test runner or real host mount namespace is mutated.
        unsafe {
            command.pre_exec(|| {
                if libc::unshare(libc::CLONE_NEWNS) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::mount(
                    std::ptr::null(),
                    c"/".as_ptr(),
                    std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE,
                    std::ptr::null(),
                ) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        assert!(bounded_child(&mut command).success());
        let marker = witness_root.join("complete");
        let metadata = std::fs::symlink_metadata(&marker)
            .expect("child must complete actual assertions, not zero selected tests");
        assert!(metadata.is_file() && !metadata.is_symlink());
        assert_eq!(metadata.uid(), 0);
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(metadata.len(), COMPLETE.len() as u64);
        assert_eq!(std::fs::read(marker).unwrap(), COMPLETE);
        return;
    }
    let witness_root =
        PathBuf::from(std::env::var_os(WITNESS).expect("owned parent witness directory required"));
    let metadata = std::fs::symlink_metadata(&witness_root).unwrap();
    assert!(metadata.is_dir() && !metadata.is_symlink());
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    let temporary = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&witness_root)
        .unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let home = root.join("home");
    let sibling = root.join("other-user");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&sibling).unwrap();
    std::fs::write(sibling.join("private"), b"unapproved sibling bytes").unwrap();
    std::fs::write(home.join(".hidden"), b"approved hidden bytes").unwrap();
    let directory_target = home.join("bound-directory");
    std::fs::create_dir(&directory_target).unwrap();
    let sentinel = directory_target.join("must-not-enumerate");
    std::fs::create_dir(&sentinel).unwrap();
    std::fs::write(sentinel.join("private"), b"unread nested sentinel").unwrap();
    let file_target = home.join("bound-file");
    std::fs::write(&file_target, b"approved ordinary file").unwrap();
    let pin = PinnedDirectory::open_absolute(&home).unwrap();
    let reviewed = SourceInventory::capture_home(&pin).unwrap();
    reviewed.validate(&pin).unwrap();
    let sibling_file = sibling.join("private");
    for (source, target) in [(&sibling, &directory_target), (&sibling_file, &file_target)] {
        let binding = Binding::mount(source, target);
        assert_eq!(
            std::fs::metadata(target).unwrap().dev(),
            std::fs::metadata(&home).unwrap().dev(),
            "fixture must exercise same-filesystem bind, not the existing device check"
        );
        assert!(
            fs_native::snapshot_lease::mount_id(&std::fs::File::open(target).unwrap()).unwrap()
                != fs_native::snapshot_lease::mount_id(&pin.file).unwrap()
        );
        let error = match SourceInventory::capture_home(&pin) {
            Ok(_) => panic!("bound entry must not produce Home inventory"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(nested_home_mount(&error));
        let cancel = AtomicBool::new(false);
        let locations = MountedHomeLocations::discover(&pin, &home, &cancel).unwrap();
        assert_eq!(
            locations.display_names(),
            vec![target.file_name().unwrap().to_string_lossy().into_owned()]
        );
        locations.validate(&cancel).unwrap();
        grant_tests::exercise_matching_platform_grants(locations.clone(), target, &home, &cancel);
        let refusal = locations.require_single_root(&cancel).unwrap_err();
        assert_eq!(
            mounted_home_locations(&refusal).unwrap().display_names(),
            locations.display_names()
        );
        let preparation_error =
            match super::super::portable_home_media::HomeSource::review_for_media_preparation(
                &home, &cancel,
            ) {
                Ok(_) => panic!(
                    "mounted Home must not become prepared, including the original-only route"
                ),
                Err(error) => error,
            };
        assert_eq!(
            mounted_home_locations(&preparation_error)
                .unwrap()
                .display_names(),
            locations.display_names()
        );
        assert!(
            reviewed.validate(&pin).is_err(),
            "previous review must reject a newly bound entry"
        );
        let admission_error = match super::super::portable_home_media::HomeSource::review(&home) {
            Ok(_) => panic!("production Home exporter must refuse before export"),
            Err(error) => error,
        };
        assert!(
            nested_home_mount(&admission_error),
            "actual exporter refusal must retain the mount-specific review category"
        );
        let (mut pending_roots, pending_selection) =
            grant_tests::pending_grant(locations.clone(), target, &cancel);
        binding.retire();
        assert!(
            pending_roots.approve(pending_selection, &cancel).is_err(),
            "a retired source cannot adopt a pending platform grant"
        );
        drop(pending_roots);
        assert!(
            locations.validate(&cancel).is_err(),
            "a retired bind must revoke the retained candidate"
        );
        drop((
            locations,
            refusal,
            preparation_error,
            admission_error,
            error,
        ));
        reviewed.validate(&pin).unwrap();
    }
    {
        let binding = std::rc::Rc::new(std::cell::RefCell::new(None));
        let owned = binding.clone();
        let target = directory_target.clone();
        let observer = DiscoveryObserver::install(move |path| {
            assert_ne!(
                path,
                Path::new("bound-directory/must-not-enumerate"),
                "discovery must refuse before enumerating the substituted mount"
            );
            if path == Path::new("bound-directory") {
                assert!(owned.borrow().is_none());
                *owned.borrow_mut() = Some(Binding::mount(&target, &target));
            }
        });
        let error =
            MountedHomeLocations::discover(&pin, &home, &AtomicBool::new(false)).unwrap_err();
        assert!(
            source_changed(&error),
            "same-inode mount substitution must refuse before readdir"
        );
        assert_eq!(
            error.to_string(),
            "Approved Home mount changed before directory discovery",
            "late exit validation is not pre-enumeration refusal"
        );
        assert!(
            binding.borrow().is_some(),
            "real bind must occur after entry attestation and before reopening"
        );
        drop(observer);
        drop(binding.borrow_mut().take());
        reviewed.validate(&pin).unwrap();
    }
    {
        let directory = Binding::mount(&sibling, &directory_target);
        let file = Binding::mount(&sibling_file, &file_target);
        let cancel = AtomicBool::new(false);
        let locations = MountedHomeLocations::discover(&pin, &home, &cancel).unwrap();
        assert_eq!(
            locations.display_names(),
            vec!["bound-directory", "bound-file"]
        );
        assert!(locations.require_single_root(&cancel).is_err());
        file.retire();
        directory.retire();
        assert!(locations.validate(&cancel).is_err());
        drop(locations);
    }
    assert_eq!(
        std::fs::read(sibling.join("private")).unwrap(),
        b"unapproved sibling bytes"
    );
    assert_eq!(
        std::fs::read(home.join(".hidden")).unwrap(),
        b"approved hidden bytes"
    );
    assert_eq!(
        std::fs::read(&file_target).unwrap(),
        b"approved ordinary file"
    );
    assert_eq!(
        std::fs::read(sentinel.join("private")).unwrap(),
        b"unread nested sentinel"
    );
    drop((reviewed, pin, temporary));
    let mut marker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(witness_root.join("complete"))
        .unwrap();
    std::io::Write::write_all(&mut marker, COMPLETE).unwrap();
    marker.sync_all().unwrap();
}
