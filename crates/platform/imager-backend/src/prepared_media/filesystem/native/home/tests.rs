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
        MediaFilesystemGrant::resolve_fixture(target, root, &self.inventory()?)
    }
}
fn fixture() -> (tempfile::TempDir, TargetDrive, Fixture) {
    let (temp, _path, raw) = disk();
    let root = temp.path().join("mounted");
    fs::create_dir(&root).expect("mount fixture");
    let mut target: TargetDrive = serde_json::from_value(serde_json::json!({"name":"sdb","model":"USB fixture","stable_id":"device-serial","size":1_073_741_824_u64,"kind":"Usb","tran":"usb","removable":true,"is_system":false,"partitions":[]})).unwrap();
    target.size = raw.metadata().expect("geometry").len();
    let stable = target.stable_id.as_deref().expect("stable medium");
    let binding =
        crate::media_layout::provision(&raw, 4 * 1024 * 1024, target.size, stable, &|| false)
            .expect("real GPT provisioning");
    let device = fs::metadata(&root).expect("mount identity").dev();
    let part = BlockDevice {
        name: format!("{}2", target.dev_path()),
        kind: "part".into(),
        size: binding
            .last_lba
            .checked_sub(binding.first_lba)
            .and_then(|sectors| sectors.checked_add(1))
            .and_then(|sectors| sectors.checked_mul(512))
            .expect("fixture partition byte size"),
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
    let (_image_temp, _image_path, image) = disk();
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

use std::os::unix::fs::FileExt;
const IMAGE_BYTES: u64 = 4 * 1024 * 1024;
const MEDIA_BYTES: u64 = 64 * 1024 * 1024;
fn field(raw: &mut [u8], start: usize, value: &[u8]) {
    raw.get_mut(start..start.checked_add(value.len()).expect("fixture field end"))
        .expect("fixture field")
        .copy_from_slice(value);
}
fn header(current: u64, alternate: u64, array_lba: u64, entries: &[u8]) -> Vec<u8> {
    let mut raw = vec![0; 512];
    field(&mut raw, 0, b"EFI PART");
    field(&mut raw, 8, &0x10000u32.to_le_bytes());
    field(&mut raw, 12, &92u32.to_le_bytes());
    for (at, value) in [
        (24, current),
        (32, alternate),
        (40, 34),
        (48, IMAGE_BYTES / 512 - 34),
        (72, array_lba),
    ] {
        field(&mut raw, at, &value.to_le_bytes());
    }
    field(&mut raw, 56, &uuid::Uuid::from_u128(5).to_bytes_le());
    field(&mut raw, 80, &128u32.to_le_bytes());
    field(&mut raw, 84, &128u32.to_le_bytes());
    field(&mut raw, 88, &crc32fast::hash(entries).to_le_bytes());
    let crc = crc32fast::hash(raw.get(..92).expect("fixture header"));
    field(&mut raw, 16, &crc.to_le_bytes());
    raw
}
pub(crate) fn disk() -> (tempfile::TempDir, std::path::PathBuf, File) {
    let dir = tempfile::tempdir().expect("fixture directory");
    let path = dir.path().join("medium");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .expect("fixture disk");
    file.set_len(MEDIA_BYTES).expect("fixture geometry");
    let mut entries = vec![0; 16384];
    field(
        &mut entries,
        0,
        &uuid::Uuid::parse_str("c12a7328-f81f-11d2-ba4b-00a0c93ec93b")
            .expect("ESP")
            .to_bytes_le(),
    );
    field(&mut entries, 16, &uuid::Uuid::from_u128(6).to_bytes_le());
    field(&mut entries, 32, &2048u64.to_le_bytes());
    field(&mut entries, 40, &4095u64.to_le_bytes());
    let old = IMAGE_BYTES / 512 - 1;
    let mut mbr = vec![0; 512];
    field(&mut mbr, 450, &[0xee]);
    field(&mut mbr, 454, &1u32.to_le_bytes());
    field(
        &mut mbr,
        458,
        &u32::try_from(old).expect("fixture sectors").to_le_bytes(),
    );
    field(&mut mbr, 510, &[0x55, 0xaa]);
    file.write_all_at(&mbr, 0).expect("MBR");
    file.write_all_at(&entries, 1024).expect("primary entries");
    file.write_all_at(
        &entries,
        old.checked_sub(32)
            .and_then(|sector| sector.checked_mul(512))
            .expect("fixture backup entries offset"),
    )
    .expect("backup entries");
    file.write_all_at(&header(1, old, 2, &entries), 512)
        .expect("primary GPT");
    file.write_all_at(&header(old, 1, old - 32, &entries), old * 512)
        .expect("backup GPT");
    file.write_all_at(b"signed boot payload", 2048 * 512)
        .expect("boot payload");
    (dir, path, file)
}
