//! Actual kernel peer, GPT source, directory/file binds and protected bytes.
use super::*;
use crate::setup::migration_review::MountedHomeLocations;
use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt, sync::atomic::AtomicBool};

struct Bind(PathBuf);
impl Bind {
    fn new(source: &Path, target: &Path) -> Self {
        let source = std::ffi::CString::new(source.as_os_str().as_bytes()).unwrap();
        let dest = std::ffi::CString::new(target.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe {
                libc::mount(
                    source.as_ptr(),
                    dest.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            },
            0,
            "real disposable bind required: {}",
            io::Error::last_os_error()
        );
        Self(target.into())
    }
}
impl Drop for Bind {
    fn drop(&mut self) {
        let path = std::ffi::CString::new(self.0.as_os_str().as_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::umount2(path.as_ptr(), 0) },
            0,
            "release after every grant FD closes: {}",
            io::Error::last_os_error()
        );
    }
}
fn owner(path: &Path, uid: u32, gid: u32, mode: u32) {
    let value = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(value.as_ptr(), uid, gid) }, 0);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
#[test]
fn actual_mounted_root_registration_binds_kernel_caller_independently_of_source_owners() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "isolated native root qualification required"
    );
    let mount = PathBuf::from(
        std::env::var_os("TILO_ORIGINAL_SOURCE_GPT_STORE")
            .expect("actual disposable GPT source required"),
    );
    let marker = mount.join(".tilo-original-source-qualification");
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        b"disposable-original-source-only"
    );
    assert_eq!(std::fs::metadata(&marker).unwrap().uid(), 0);
    assert_eq!(std::fs::metadata(&marker).unwrap().mode() & 0o777, 0o600);
    let temp = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&mount)
        .unwrap();
    // The admitted source user can traverse the fixture ancestor; broker
    // custody remains root-controlled and the actual Home remains private.
    owner(temp.path(), 0, 0, 0o711);
    let home = temp.path().join("alice");
    std::fs::create_dir(&home).unwrap();
    owner(&home, 1000, 1001, 0o700);
    std::fs::write(home.join(".hidden"), b"primary unchanged").unwrap();
    let child = temp.path().join("group-owned-location");
    std::fs::create_dir(&child).unwrap();
    owner(&child, 1003, 1002, 0o750);
    let file = child.join("data");
    std::fs::write(&file, b"group-readable unchanged").unwrap();
    owner(&file, 1003, 1002, 0o640);
    let folder = home.join("mounted-directory");
    std::fs::create_dir(&folder).unwrap();
    let leaf = home.join("mounted-file");
    std::fs::write(&leaf, b"hidden underlying file").unwrap();
    let root = temp.path().join("var/lib/tilo-imager/home-capture");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let plan = crate::setup::migration_handoff::capture(
        &home,
        &operation,
        crate::setup::migration_handoff::Categories {
            home: true,
            flatpak: false,
            accounts: false,
        },
    )
    .expect("actual GPT source binding");
    let (mut admitted, mut channel) = peer([1002, 1002, 1003]);
    let mut store =
        Store::open(PinnedDirectory::open_absolute(&root).unwrap(), &operation).unwrap();
    store.records_path = Some(root.join("records"));
    let primary = store
        .register_original_selection(&home, &plan, &channel)
        .unwrap();
    let binds = [Bind::new(&child, &folder), Bind::new(&file, &leaf)];
    let cancel = AtomicBool::new(false);
    let report = MountedHomeLocations::discover(
        &PinnedDirectory::open_absolute(&home).unwrap(),
        &home,
        &cancel,
    )
    .unwrap();
    let refusal = report.require_single_root(&cancel).unwrap_err();
    let mut roots =
        crate::setup::portable_home_media::HomeSource::mounted_root_set(&refusal).unwrap();
    for (id, placement, approved) in roots.locations() {
        assert!(!approved);
        // The frontend tests the picker; native admission receives a real path/FD.
        let request = roots.begin_request(&id, &cancel).unwrap();
        let path = home.join(placement);
        let answer = match request.kind() {
            crate::setup::RootKind::Directory => request.select_directory(path).unwrap(),
            crate::setup::RootKind::File => {
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(path)
                    .unwrap();
                request.select_file(file).unwrap()
            }
        };
        roots.approve(answer, &cancel).unwrap();
    }
    let roots = roots.into_granted_roots(&cancel).unwrap();
    let path = root
        .join("records")
        .join(format!("{operation}.mounted-roots.json"));
    assert!(
        store
            .register_mounted_roots(&home, &plan, &roots, &channel, &AtomicBool::new(true))
            .is_err()
    );
    assert!(
        !path.exists(),
        "cancelled admission cannot publish mounted source authority"
    );
    let witness = store
        .register_mounted_roots(&home, &plan, &roots, &channel, &cancel)
        .unwrap();

    let bytes = std::fs::read(&path).unwrap();
    let before = std::fs::metadata(&path).unwrap();
    assert_eq!(
        store
            .register_mounted_roots(&home, &plan, &roots, &channel, &cancel)
            .unwrap(),
        witness
    );
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(
        (before.ino(), before.mtime(), before.ctime()),
        (after.ino(), after.mtime(), after.ctime())
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let record: super::super::mounted::MountedRecord = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record.caller.uid(), 1000);
    assert_eq!(record.caller.primary_gid(), 1001);
    assert_eq!(record.caller.supplementary_gids(), [1002, 1003]);
    assert_eq!(record.entries.len(), 2);
    assert!(
        record
            .entries
            .iter()
            .all(|entry| entry.binding.owner() == 1003)
    );
    assert_eq!(
        record.entries[0].binding.kind(),
        crate::setup::migration_handoff::EntryKind::Directory
    );
    assert_eq!(
        record.entries[1].binding.kind(),
        crate::setup::migration_handoff::EntryKind::File
    );
    let (mut other, mut changed) = peer([1002, 1002, 1004]);
    assert!(
        store
            .register_mounted_roots(&home, &plan, &roots, &changed, &cancel)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    changed.stream().write_all(&[7]).unwrap();
    assert!(other.0.wait().unwrap().success());
    channel.stream().write_all(&[7]).unwrap();
    assert!(admitted.0.wait().unwrap().success());
    drop((roots, report, refusal, binds, store));
    let readonly = ReadOnly::enter(&mount);
    let source = crate::setup::migration_handoff::resolve(&plan, &mount).unwrap();
    let context = SourceReadContext::review_witness(source, &primary.witness().unwrap()).unwrap();
    context.verify_mounted_witness(&witness).unwrap();
    context
        .with_mounted_sources(&witness, &cancel, |sources| {
            assert_eq!(sources.len(), 2);
            assert_eq!(sources[0].placement(), Path::new("mounted-directory"));
            assert_eq!(sources[1].placement(), Path::new("mounted-file"));
            assert_eq!(
                sources[0].kind(),
                crate::setup::migration_handoff::EntryKind::Directory
            );
            assert_eq!(
                sources[1].kind(),
                crate::setup::migration_handoff::EntryKind::File
            );
            assert_eq!(
                sources[0].file().metadata()?.ino(),
                std::fs::metadata(&child)?.ino()
            );
            assert_eq!(
                sources[1].file().metadata()?.ino(),
                std::fs::metadata(&file)?.ino()
            );
            for entry in sources {
                assert_eq!(entry.file().metadata()?.uid(), 1003);
                assert_eq!(entry.file().metadata()?.gid(), 1002);
                let flags = unsafe { libc::fcntl(entry.file().as_raw_fd(), libc::F_GETFL) };
                assert_ne!(flags & libc::O_PATH, 0);
                let mut data = [0; 1];
                assert_eq!(
                    unsafe { libc::read(entry.file().as_raw_fd(), data.as_mut_ptr().cast(), 1) },
                    -1
                );
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
            }
            // Reopen does not install source-owner credentials in the receiver.
            assert_eq!(rustix::process::geteuid().as_raw(), 0);
            assert_eq!(context.original().uid(), 1000);
            Ok(())
        })
        .unwrap();
    assert!(
        context
            .with_mounted_sources(&witness, &AtomicBool::new(true), |_| -> io::Result<()> {
                panic!("Cancelled source use must not expose descriptors")
            })
            .is_err()
    );
    let mut forged = serde_json::to_value(&witness).unwrap();
    let old = forged["sha256"][0].as_u64().unwrap();
    forged["sha256"][0] = serde_json::json!(old ^ 1);
    let forged = serde_json::from_value(forged).unwrap();
    assert!(
        context.verify_mounted_witness(&forged).is_err(),
        "request bytes do not replace protected record authority"
    );
    assert!(
        context
            .with_mounted_sources(&forged, &cancel, |_| -> io::Result<()> {
                panic!("Public witness cannot substitute protected source authority")
            })
            .is_err()
    );
    drop(context);
    drop(readonly);
    let old = child.join("retained-original");
    std::fs::rename(&file, &old).unwrap();
    std::fs::write(&file, b"substituted same-owner file").unwrap();
    owner(&file, 1003, 1002, 0o640);
    let readonly = ReadOnly::enter(&mount);
    let source = crate::setup::migration_handoff::resolve(&plan, &mount).unwrap();
    let context = SourceReadContext::review_witness(source, &primary.witness().unwrap()).unwrap();
    assert!(
        context
            .with_mounted_sources(&witness, &cancel, |_| -> io::Result<()> {
                panic!("Actual replacement inode must refuse before projection")
            })
            .is_err()
    );
    drop(context);
    drop(readonly);
    std::fs::remove_file(&file).unwrap();
    std::fs::rename(&old, &file).unwrap();
    owner(&path, 1000, 1001, 0o600);
    let readonly = ReadOnly::enter(&mount);
    let source = crate::setup::migration_handoff::resolve(&plan, &mount).unwrap();
    let context = SourceReadContext::review_witness(source, &primary.witness().unwrap()).unwrap();
    assert!(
        context.verify_mounted_witness(&witness).is_err(),
        "caller-owned identical bytes are not root authority"
    );
    drop(context);
    drop(readonly);
    assert_eq!(std::fs::read(&file).unwrap(), b"group-readable unchanged");
    assert_eq!(
        std::fs::read(home.join(".hidden")).unwrap(),
        b"primary unchanged"
    );
    assert_eq!(std::fs::read(&leaf).unwrap(), b"hidden underlying file");
}

#[test]
fn actual_foreign_mounted_participant_record_binds_its_own_filesystem_and_kernel_caller() {
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "isolated native root qualification required"
    );
    let mount = PathBuf::from(
        std::env::var_os("TILO_ORIGINAL_SOURCE_GPT_STORE")
            .expect("actual disposable GPT source required"),
    );
    let marker = mount.join(".tilo-original-source-qualification");
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        b"disposable-original-source-only"
    );
    assert_eq!(std::fs::metadata(&marker).unwrap().uid(), 0);
    assert_eq!(std::fs::metadata(&marker).unwrap().mode() & 0o777, 0o600);
    let temp = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&mount)
        .unwrap();
    // The admitted source user can traverse the fixture ancestor; broker
    // custody remains root-controlled and the actual Home remains private.
    owner(temp.path(), 0, 0, 0o711);
    let home = temp.path().join("alice");
    std::fs::create_dir(&home).unwrap();
    owner(&home, 1000, 1001, 0o700);
    std::fs::write(home.join(".hidden"), b"primary unchanged").unwrap();
    let foreign_mount = PathBuf::from(
        std::env::var_os("TILO_MOUNTED_PARTICIPANT_GPT_STORE")
            .expect("second actual disposable GPT filesystem required"),
    );
    let foreign_marker = foreign_mount.join(".tilo-original-source-qualification");
    assert_eq!(
        std::fs::read(&foreign_marker).unwrap(),
        b"disposable-original-source-only"
    );
    assert_eq!(std::fs::metadata(&foreign_marker).unwrap().uid(), 0);
    assert_eq!(
        std::fs::metadata(&foreign_marker).unwrap().mode() & 0o777,
        0o600
    );
    assert_ne!(
        std::fs::metadata(&mount).unwrap().dev(),
        std::fs::metadata(&foreign_mount).unwrap().dev(),
        "actual distinct source filesystems required"
    );
    let foreign = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&foreign_mount)
        .unwrap();
    owner(foreign.path(), 0, 0, 0o711);
    let child = foreign.path().join("group-owned-location");
    std::fs::create_dir(&child).unwrap();
    owner(&child, 1003, 1002, 0o750);
    let file = child.join("data");
    std::fs::write(&file, b"group-readable unchanged").unwrap();
    owner(&file, 1003, 1002, 0o640);
    let folder = home.join("mounted-directory");
    std::fs::create_dir(&folder).unwrap();
    let leaf = home.join("mounted-file");
    std::fs::write(&leaf, b"hidden underlying file").unwrap();
    let root = temp.path().join("var/lib/tilo-imager/home-capture");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let plan = crate::setup::migration_handoff::capture(
        &home,
        &operation,
        crate::setup::migration_handoff::Categories {
            home: true,
            flatpak: false,
            accounts: false,
        },
    )
    .expect("actual GPT source binding");
    let (mut admitted, mut channel) = peer([1002, 1002, 1003]);
    let mut store =
        Store::open(PinnedDirectory::open_absolute(&root).unwrap(), &operation).unwrap();
    store.records_path = Some(root.join("records"));
    let primary = store
        .register_original_selection(&home, &plan, &channel)
        .unwrap();
    let binds = [Bind::new(&child, &folder), Bind::new(&file, &leaf)];
    let cancel = AtomicBool::new(false);
    let report = MountedHomeLocations::discover(
        &PinnedDirectory::open_absolute(&home).unwrap(),
        &home,
        &cancel,
    )
    .unwrap();
    let refusal = report.require_single_root(&cancel).unwrap_err();
    let mut roots =
        crate::setup::portable_home_media::HomeSource::mounted_root_set(&refusal).unwrap();
    for (id, placement, approved) in roots.locations() {
        assert!(!approved);
        // The frontend tests the picker; native admission receives a real path/FD.
        let request = roots.begin_request(&id, &cancel).unwrap();
        let path = home.join(placement);
        let answer = match request.kind() {
            crate::setup::RootKind::Directory => request.select_directory(path).unwrap(),
            crate::setup::RootKind::File => {
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(path)
                    .unwrap();
                request.select_file(file).unwrap()
            }
        };
        roots.approve(answer, &cancel).unwrap();
    }
    let roots = roots.into_granted_roots(&cancel).unwrap();
    let path = root
        .join("records")
        .join(format!("{operation}.mounted-roots.json"));
    assert!(
        store
            .register_mounted_roots(&home, &plan, &roots, &channel, &AtomicBool::new(true))
            .is_err()
    );
    assert!(
        !path.exists(),
        "cancelled admission cannot publish mounted source authority"
    );
    let witness = store
        .register_mounted_roots(&home, &plan, &roots, &channel, &cancel)
        .unwrap();

    let participant = foreign_mount
        .join("var/lib/tilo-imager/home-capture/records")
        .join(format!("{operation}.mounted-participant.json"));
    let participant_bytes = std::fs::read(&participant).unwrap();
    let participant_before = std::fs::metadata(&participant).unwrap();
    assert_eq!(participant_before.uid(), 0);
    assert_eq!(participant_before.mode() & 0o777, 0o600);
    assert_eq!(
        participant_before.dev(),
        std::fs::metadata(&child).unwrap().dev()
    );
    let bytes = std::fs::read(&path).unwrap();
    let before = std::fs::metadata(&path).unwrap();
    assert_eq!(
        store
            .register_mounted_roots(&home, &plan, &roots, &channel, &cancel)
            .unwrap(),
        witness
    );
    let participant_after = std::fs::metadata(&participant).unwrap();
    assert_eq!(
        (
            participant_before.ino(),
            participant_before.mtime(),
            participant_before.ctime()
        ),
        (
            participant_after.ino(),
            participant_after.mtime(),
            participant_after.ctime()
        )
    );
    assert_eq!(std::fs::read(&participant).unwrap(), participant_bytes);
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(
        (before.ino(), before.mtime(), before.ctime()),
        (after.ino(), after.mtime(), after.ctime())
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let record: super::super::mounted::MountedRecord = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record.caller.uid(), 1000);
    assert_eq!(record.caller.primary_gid(), 1001);
    assert_eq!(record.caller.supplementary_gids(), [1002, 1003]);
    assert_eq!(record.entries.len(), 2);
    assert!(
        record
            .entries
            .iter()
            .all(|entry| entry.binding.owner() == 1003)
    );
    assert_eq!(
        record.entries[0].binding.kind(),
        crate::setup::migration_handoff::EntryKind::Directory
    );
    assert_eq!(
        record.entries[1].binding.kind(),
        crate::setup::migration_handoff::EntryKind::File
    );
    let (mut other, mut changed) = peer([1002, 1002, 1004]);
    assert!(
        store
            .register_mounted_roots(&home, &plan, &roots, &changed, &cancel)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    changed.stream().write_all(&[7]).unwrap();
    assert!(other.0.wait().unwrap().success());
    std::fs::write(&participant, b"tampered actual participant").unwrap();
    assert!(
        store
            .register_mounted_roots(&home, &plan, &roots, &channel, &cancel)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::write(&participant, &participant_bytes).unwrap();
    owner(&participant, 1000, 1001, 0o600);
    assert!(
        store
            .register_mounted_roots(&home, &plan, &roots, &channel, &cancel)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    owner(&participant, 0, 0, 0o600);
    channel.stream().write_all(&[7]).unwrap();
    assert!(admitted.0.wait().unwrap().success());
    drop((roots, report, refusal, binds, store));
    let writable_alias_target = temp.path().join("foreign-writable-alias");
    std::fs::create_dir(&writable_alias_target).unwrap();
    let writable_alias = Bind::new(&child, &writable_alias_target);
    let readonly_primary = ReadOnly::enter(&mount);
    let readonly_foreign = ReadOnly::enter(&foreign_mount);
    let source = crate::setup::migration_handoff::resolve(&plan, &mount).unwrap();
    let context = SourceReadContext::review_witness(source, &primary.witness().unwrap()).unwrap();
    assert_eq!(
        context
            .verify_mounted_witness(&witness)
            .unwrap_err()
            .to_string(),
        "Participant source also has a writable mount"
    );
    // The rejected context owns no alias mount. Retire that exact fixture bind
    // normally before accepting the genuinely readonly participant epoch.
    drop(writable_alias);
    context.verify_mounted_witness(&witness).unwrap();
    context
        .with_mounted_sources(&witness, &cancel, |sources| {
            assert_eq!(sources.len(), 2);
            for entry in sources {
                assert_eq!(
                    entry.file().metadata()?.dev(),
                    std::fs::metadata(&child)?.dev()
                );
                assert_eq!(entry.file().metadata()?.uid(), 1003);
                assert_eq!(entry.file().metadata()?.gid(), 1002);
                let flags = unsafe { libc::fcntl(entry.file().as_raw_fd(), libc::F_GETFL) };
                assert_ne!(flags & libc::O_PATH, 0);
                let mut bytes = [0; 1];
                assert_eq!(
                    unsafe { libc::read(entry.file().as_raw_fd(), bytes.as_mut_ptr().cast(), 1) },
                    -1
                );
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
            }
            assert_eq!(
                sources[0].file().metadata()?.ino(),
                std::fs::metadata(&child)?.ino()
            );
            assert_eq!(
                sources[1].file().metadata()?.ino(),
                std::fs::metadata(&file)?.ino()
            );
            assert_eq!(
                rustix::process::geteuid().as_raw(),
                0,
                "Source owners never become receiver credentials"
            );
            Ok(())
        })
        .unwrap();
    let epoch = context.retain_mounted_sources(&witness, &cancel).unwrap();
    // These are actual protected two-filesystem inputs, not a request table.
    let digest = epoch.table_sha256(&cancel).unwrap();
    assert_ne!(digest, [0; 32]);
    let inputs = epoch.launch_inputs(&cancel).unwrap();
    assert_eq!(inputs.len(), 2);
    drop(inputs);
    let mut source_geometry = vec![plan.bytes().unwrap()];
    epoch
        .append_source_geometry(&mut source_geometry, &cancel)
        .unwrap();
    assert_eq!(source_geometry.len(), 2);
    let geometry =
        crate::setup::migration_handoff::SourceGeometry::parse(&source_geometry[1]).unwrap();
    let observed_foreign = crate::setup::migration_handoff::capture_mounted_entry(
        &child,
        &OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH)
            .open(&child)
            .unwrap(),
        crate::setup::migration_handoff::EntryKind::Directory,
    )
    .unwrap();
    assert_eq!(geometry.volume(), observed_foreign.volume());
    let mut repeat = vec![plan.bytes().unwrap()];
    epoch.append_source_geometry(&mut repeat, &cancel).unwrap();
    assert_eq!(repeat, source_geometry);

    drop(context);
    // Actual records, foreign source pins and primary mount survive the
    // caller's context. Reentrant supervisor checks must not deadlock while
    // the callback owns its cloned exact inputs.
    epoch
        .with_sources(&cancel, |sources| {
            assert_eq!(sources.len(), 2);
            epoch.with_sources(&cancel, |rechecked| {
                for (outer, inner) in sources.iter().zip(rechecked) {
                    assert_eq!(
                        outer.file().metadata()?.dev(),
                        inner.file().metadata()?.dev()
                    );
                    assert_eq!(
                        outer.file().metadata()?.ino(),
                        inner.file().metadata()?.ino()
                    );
                }
                Ok(())
            })
        })
        .unwrap();
    remount(&foreign_mount, false);
    assert!(
        epoch
            .with_sources(&cancel, |_| -> io::Result<()> {
                panic!("Writable foreign source must not reach signed worker inputs")
            })
            .is_err()
    );
    remount(&foreign_mount, true);
    epoch.with_sources(&cancel, |_| Ok(())).unwrap();
    assert!(
        epoch
            .with_sources(&AtomicBool::new(true), |_| -> io::Result<()> {
                panic!("Cancelled retained root set cannot reach reader")
            })
            .is_err()
    );
    // Raise REAL cancellation inside the actual source callback after changing
    // native readonly state. The postcondition must observe custody failure,
    // not return early with Interrupted before inspecting protected sources.
    let callback_cancel = AtomicBool::new(false);
    let postcondition = epoch
        .with_sources(&callback_cancel, |_| {
            remount(&foreign_mount, false);
            callback_cancel.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        })
        .unwrap_err();
    assert_eq!(
        postcondition.to_string(),
        "Kernel source filesystem is writable; no transfer was authorized."
    );
    remount(&foreign_mount, true);
    let callback_cancel = AtomicBool::new(false);
    let cancelled = epoch
        .with_sources(&callback_cancel, |_| {
            callback_cancel.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        })
        .unwrap_err();
    assert_eq!(cancelled.kind(), io::ErrorKind::Interrupted);
    epoch.with_sources(&cancel, |_| Ok(())).unwrap();
    drop(epoch);
    drop((readonly_foreign, readonly_primary));
    assert_eq!(std::fs::read(&file).unwrap(), b"group-readable unchanged");
    assert_eq!(std::fs::read(&leaf).unwrap(), b"hidden underlying file");
    std::fs::remove_file(&participant).unwrap();
    std::fs::remove_file(
        foreign_mount
            .join("var/lib/tilo-imager/home-capture/records")
            .join(format!("{operation}.lock")),
    )
    .unwrap();
}
