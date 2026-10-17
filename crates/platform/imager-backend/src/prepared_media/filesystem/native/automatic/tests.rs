use super::*;
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt},
};

struct Fixture {
    rows: RefCell<Vec<BlockDevice>>,
    calls: RefCell<Vec<String>>,
    root: PathBuf,
}
impl Backend for Fixture {
    fn revalidate(&self, _: &TargetDrive) -> io::Result<()> {
        Ok(())
    }
    fn inventory(&self) -> io::Result<Vec<BlockDevice>> {
        Ok(self.rows.borrow().clone())
    }
    fn rescan(&self, _: &TargetDrive) -> io::Result<()> {
        self.calls.borrow_mut().push("rescan".into());
        Ok(())
    }
    fn mount(&self, _: &str) -> io::Result<PathBuf> {
        self.calls.borrow_mut().push("mount".into());
        self.rows.borrow_mut()[0].children[0].mountpoints =
            vec![self.root.to_string_lossy().into_owned()];
        Ok(self.root.clone())
    }
    fn unmount(&self, _: &str) -> io::Result<()> {
        self.calls.borrow_mut().push("unmount".into());
        self.rows.borrow_mut()[0].children[0].mountpoints.clear();
        Ok(())
    }
    fn grant(&self, target: &TargetDrive, root: &Path) -> io::Result<MediaFilesystemGrant> {
        let pin = PinnedRoot::open_absolute(root)?;
        let partition = select_partition(&self.inventory()?, target, root, pin.identity.dev())?;
        Ok(MediaFilesystemGrant {
            target: target.clone(),
            root: root.to_owned(),
            pin,
            partition,
        })
    }
}
fn fixture() -> (tempfile::TempDir, TargetDrive, Fixture) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let target: TargetDrive = serde_json::from_value(serde_json::json!({"name":"sdb","model":"USB fixture","stable_id":"device-serial","size":1_073_741_824_u64,"kind":"Usb","tran":"usb","removable":true,"is_system":false,"partitions":[]})).unwrap();
    assert!(target.stable_id.is_some());
    let dev = fs::metadata(&root).unwrap().dev();
    let part = BlockDevice {
        name: format!("{}1", target.dev_path()),
        kind: "part".into(),
        fstype: Some("vfat".into()),
        uuid: Some("EFI-filesystem".into()),
        partuuid: Some("EFI-partition".into()),
        parttype: Some(ESP.into()),
        maj_min: Some(format!("{}:{}", libc::major(dev), libc::minor(dev))),
        ..Default::default()
    };
    let disk = BlockDevice {
        name: target.dev_path(),
        kind: "disk".into(),
        size: target.size,
        children: vec![part],
        ..Default::default()
    };
    (
        temp,
        target,
        Fixture {
            rows: RefCell::new(vec![disk]),
            calls: RefCell::new(Vec::new()),
            root,
        },
    )
}
#[test]
fn automatic_mount_resolves_actual_grant_and_releases_only_its_own_mount() {
    let (_temp, target, backend) = fixture();
    let cancel = AtomicBool::new(false);
    with_mount(&backend, &target, &cancel, |grant| {
        assert_eq!(grant.root, backend.root);
        grant.pin.validate_destination()?;
        fs::write(
            grant.root.join("actual-ciphertext-output"),
            b"verified-output",
        )?;
        Ok(())
    })
    .unwrap();
    assert_eq!(*backend.calls.borrow(), ["rescan", "mount", "unmount"]);
    assert_eq!(
        fs::read(backend.root.join("actual-ciphertext-output")).unwrap(),
        b"verified-output"
    );
    backend.calls.borrow_mut().clear();
    backend.rows.borrow_mut()[0].children[0].mountpoints =
        vec![backend.root.to_string_lossy().into_owned()];
    with_mount(&backend, &target, &cancel, |_| Ok(())).unwrap();
    assert_eq!(*backend.calls.borrow(), ["rescan"]);
}
#[test]
fn failed_delivery_and_cancellation_release_mount_but_changed_device_is_never_unmounted() {
    let (_temp, target, backend) = fixture();
    let cancel = AtomicBool::new(false);
    assert!(
        with_mount(&backend, &target, &cancel, |_| Err::<(), _>(
            io::Error::other("copy failed")
        ))
        .is_err()
    );
    assert_eq!(*backend.calls.borrow(), ["rescan", "mount", "unmount"]);
    backend.calls.borrow_mut().clear();
    assert!(
        with_mount(&backend, &target, &cancel, |_| {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            Err::<(), _>(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
        })
        .is_err()
    );
    assert_eq!(*backend.calls.borrow(), ["rescan", "mount", "unmount"]);
    backend.calls.borrow_mut().clear();
    cancel.store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(
        with_mount(&backend, &target, &cancel, |_| {
            backend.rows.borrow_mut()[0].children[0].partuuid = Some("different-partition".into());
            Ok(())
        })
        .is_err()
    );
    assert_eq!(*backend.calls.borrow(), ["rescan", "mount"]);
}
#[test]
fn ambiguous_or_untrusted_efi_filesystems_are_refused_before_mounting() {
    let (_temp, target, backend) = fixture();
    let part = backend.rows.borrow()[0].children[0].clone();
    backend.rows.borrow_mut()[0].children.push(part);
    assert!(with_mount(&backend, &target, &AtomicBool::new(false), |_| Ok(())).is_err());
    assert_eq!(*backend.calls.borrow(), ["rescan"]);
    backend.rows.borrow_mut()[0].children.pop();
    backend.calls.borrow_mut().clear();
    backend.rows.borrow_mut()[0].children[0].fstype = Some("ext4".into());
    assert!(with_mount(&backend, &target, &AtomicBool::new(false), |_| Ok(())).is_err());
    assert_eq!(*backend.calls.borrow(), ["rescan"]);
    assert!(
        selected(
            &backend.inventory().unwrap(),
            &TargetDrive {
                stable_id: None,
                ..target
            }
        )
        .is_err()
    );
}
