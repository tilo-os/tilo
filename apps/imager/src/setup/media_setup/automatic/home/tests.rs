use super::*;
use std::{cell::RefCell, fs, os::unix::fs::MetadataExt as _};
struct Fixture {
    raw: File,
    rows: RefCell<Vec<BlockDevice>>,
    calls: RefCell<Vec<String>>,
    root: PathBuf,
}
impl DataBackend for Fixture {
    fn revalidate(&self, _: &TargetDrive) -> io::Result<()> {
        Ok(())
    }
    fn raw(&self, _: &TargetDrive) -> io::Result<File> {
        self.raw.try_clone()
    }
    fn inventory(&self) -> io::Result<Vec<BlockDevice>> {
        Ok(self.rows.borrow().clone())
    }
    fn rescan(&self, _: &TargetDrive) -> io::Result<()> {
        Ok(())
    }
    fn format(&self, _: &str) -> io::Result<()> {
        self.calls.borrow_mut().push("format".into());
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
        let mut rows = self.rows.borrow_mut();
        let part = rows
            .first_mut()
            .and_then(|r| r.children.first_mut())
            .expect("partition");
        part.fstype = Some("ext4".into());
        part.uuid = Some("fresh-ext4".into());
        Ok(())
    }
    fn mount(&self, _: &str) -> io::Result<PathBuf> {
        self.calls.borrow_mut().push("mount".into());
        self.rows
            .borrow_mut()
            .first_mut()
            .and_then(|r| r.children.first_mut())
            .expect("partition")
            .mountpoints = vec![self.root.to_string_lossy().into_owned()];
        Ok(self.root.clone())
    }
    fn unmount(&self, _: &str) -> io::Result<()> {
        self.calls.borrow_mut().push("unmount".into());
        self.rows
            .borrow_mut()
            .first_mut()
            .and_then(|r| r.children.first_mut())
            .expect("partition")
            .mountpoints
            .clear();
        Ok(())
    }
    fn grant(&self, target: &TargetDrive, root: &Path) -> io::Result<MediaFilesystemGrant> {
        let pin = PinnedDirectory::open_absolute(root)?;
        pin.validate_destination()?;
        let partition = select_partition(&self.inventory()?, target, root, pin.identity.dev())?;
        Ok(MediaFilesystemGrant {
            target: target.clone(),
            root: root.into(),
            pin,
            partition,
        })
    }
}
fn fixture() -> (tempfile::TempDir, TargetDrive, Fixture) {
    let (temp, _path, raw) = crate::media_layout::tests::disk();
    let root = temp.path().join("mounted");
    fs::create_dir(&root).expect("mount fixture");
    let mut target = crate::model::sample_targets()
        .first()
        .expect("media target")
        .clone();
    target.size = raw.metadata().expect("geometry").len();
    let stable = target.stable_id.as_deref().expect("stable medium");
    let binding =
        crate::media_layout::provision(&raw, 4 * 1024 * 1024, target.size, stable, &|| false)
            .expect("real GPT provisioning");
    let device = fs::metadata(&root).expect("mount identity").dev();
    let part = BlockDevice {
        name: format!("{}2", target.dev_path()),
        kind: "part".into(),
        size: (binding.last_lba - binding.first_lba + 1) * 512,
        start: Some(binding.first_lba),
        partuuid: Some(binding.partition_guid),
        parttype: Some(DATA_PARTITION_TYPE.into()),
        maj_min: Some(format!("{}:{}", libc::major(device), libc::minor(device))),
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
            raw,
            rows: RefCell::new(vec![disk]),
            calls: RefCell::new(Vec::new()),
            root,
        },
    )
}
#[test]
fn exact_provisioned_partition_formats_once_and_reuses_verified_ext4() {
    let (_temp, target, backend) = fixture();
    with_data(&backend, &target, &AtomicBool::new(false), |grant| {
        fs::write(grant.root.join("sealed-home"), b"ciphertext")
    })
    .expect("actual delivery controller");
    assert_eq!(*backend.calls.borrow(), ["format", "mount", "unmount"]);
    assert_eq!(
        fs::read(backend.root.join("sealed-home")).expect("delivered bytes"),
        b"ciphertext"
    );
    backend.calls.borrow_mut().clear();
    with_data(&backend, &target, &AtomicBool::new(false), |_| Ok(())).expect("retry");
    assert_eq!(*backend.calls.borrow(), ["mount", "unmount"]);
}
#[test]
fn changed_extent_incompatible_filesystem_and_cancellation_never_format() {
    let (_temp, target, backend) = fixture();
    assert!(with_data(&backend, &target, &AtomicBool::new(true), |_| Ok(())).is_err());
    assert!(backend.calls.borrow().is_empty());
    backend
        .rows
        .borrow_mut()
        .first_mut()
        .and_then(|r| r.children.first_mut())
        .expect("partition")
        .fstype = Some("xfs".into());
    assert!(with_data(&backend, &target, &AtomicBool::new(false), |_| Ok(())).is_err());
    assert!(backend.calls.borrow().is_empty());
    let mut rows = backend.rows.borrow_mut();
    let part = rows
        .first_mut()
        .and_then(|r| r.children.first_mut())
        .expect("partition");
    part.fstype = None;
    part.start = Some(1);
    drop(rows);
    assert!(with_data(&backend, &target, &AtomicBool::new(false), |_| Ok(())).is_err());
    assert!(backend.calls.borrow().is_empty());
}

#[test]
fn new_partition_identity_reformats_old_ext4_but_exact_retry_preserves_export() {
    use std::os::unix::fs::FileExt;
    let (_temp, target, backend) = fixture();
    let cancel = AtomicBool::new(false);
    with_data(&backend, &target, &cancel, |grant| {
        fs::write(grant.root.join("old-ciphertext"), b"old sealed data")
    })
    .expect("first owned filesystem");
    backend.calls.borrow_mut().clear();
    with_data(&backend, &target, &cancel, |_| Ok(())).expect("exact retry");
    assert_eq!(*backend.calls.borrow(), ["mount", "unmount"]);
    assert_eq!(
        fs::read(backend.root.join("old-ciphertext")).expect("preserved ciphertext"),
        b"old sealed data"
    );
    let (_image_temp, _image_path, image) = crate::media_layout::tests::disk();
    let mut bytes = vec![0; 4 * 1024 * 1024];
    image.read_exact_at(&mut bytes, 0).expect("original image");
    backend
        .raw
        .write_all_at(&bytes, 0)
        .expect("rewrite approved installer image prefix");
    let new_binding = crate::media_layout::provision(
        &backend.raw,
        4 * 1024 * 1024,
        target.size,
        target.stable_id.as_deref().expect("stable media"),
        &|| false,
    )
    .expect("new GPT identity at same extent");
    backend
        .rows
        .borrow_mut()
        .first_mut()
        .and_then(|row| row.children.first_mut())
        .expect("partition")
        .partuuid = Some(new_binding.partition_guid);
    backend.calls.borrow_mut().clear();
    with_data(&backend, &target, &cancel, |grant| {
        fs::write(grant.root.join("new-ciphertext"), b"new sealed data")
    })
    .expect("new identity reformats old filesystem");
    assert_eq!(
        *backend.calls.borrow(),
        ["mount", "unmount", "format", "mount", "unmount"]
    );
    assert!(!backend.root.join("old-ciphertext").exists());
    assert_eq!(
        fs::read(backend.root.join("new-ciphertext")).expect("new export"),
        b"new sealed data"
    );
}
