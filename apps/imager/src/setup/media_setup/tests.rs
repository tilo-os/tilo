use super::*;
use crate::setup::portable_transfer::ExportReview;
use std::{fs, os::unix::fs::PermissionsExt};
use zeroize::Zeroizing;
const PASSWORD: &[u8] = b"the chosen destination password";
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.join("prepared");
    fs::create_dir(&source).unwrap();
    fs::create_dir(source.join("accounts")).unwrap();
    fs::write(source.join("accounts/vault"), b"private-account-content").unwrap();
    fs::write(source.join("identity-backup"), b"private-identity-content").unwrap();
    let package = root.join("setup.sealed");
    let cancel = AtomicBool::new(false);
    ExportReview::review(
        &source,
        &package,
        Purpose::PreparedSetup,
        Limits {
            plaintext_bytes: 4 * 1024 * 1024,
            entries: 128,
            metadata_bytes: 1024 * 1024,
        },
    )
    .unwrap()
    .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
    .unwrap();
    (temp, root, package)
}
#[test]
fn file_delivery_writes_verified_ciphertext_without_plaintext_or_raw_disk_io() {
    let (_temp, root, package) = fixture();
    let media = root.join("mounted-filesystem-fixture");
    fs::create_dir(&media).unwrap();
    let pin = PinnedDirectory::open_absolute(&media).unwrap();
    let cancel = AtomicBool::new(false);
    let mut reviewed = PreparedSetupDelivery::review(&package, PASSWORD, &cancel).unwrap();
    let expected = reviewed.expectation();
    let mut profile = crate::model::InstallProfile {
        prepared_setup: Some(expected.clone()),
        ..Default::default()
    };
    profile.user.password = "never serialize this password".into();
    profile.validate_media_profile().unwrap();
    let encoded = profile.to_media_profile_json().unwrap();
    let public: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        public["prepared_setup"],
        serde_json::to_value(&expected).unwrap()
    );
    assert!(
        !String::from_utf8(encoded)
            .unwrap()
            .contains(&profile.user.password)
    );
    let receipt = deliver_ciphertext(&mut reviewed.package, &pin, &cancel).unwrap();
    let target = media.join(&receipt.relative_path);
    assert_eq!(fs::read(&target).unwrap(), fs::read(&package).unwrap());
    assert_eq!(fs::metadata(&target).unwrap().len(), receipt.sealed_bytes);
    let expectation: serde_json::Value =
        serde_json::from_slice(&fs::read(media.join(DIRECTORY).join(RECEIPT_FILE)).unwrap())
            .unwrap();
    assert_eq!(expectation, receipt.expectation());
    assert_eq!(expectation, public["prepared_setup"]);
    use sha2::{Digest, Sha256};
    assert_eq!(
        expectation["ciphertext_sha256"],
        hex::encode(Sha256::digest(fs::read(&target).unwrap()))
    );
    assert!(
        !fs::read(&target)
            .unwrap()
            .windows(b"private-account-content".len())
            .any(|part| part == b"private-account-content")
    );
    assert!(deliver_ciphertext(&mut reviewed.package, &pin, &cancel).is_err());
    assert_eq!(fs::read(target).unwrap(), fs::read(&package).unwrap());
}
#[test]
fn existing_receipt_refuses_delivery_without_changing_existing_custody() {
    let (_temp, root, package) = fixture();
    let media = root.join("media");
    fs::create_dir(&media).unwrap();
    let directory = media.join(DIRECTORY);
    fs::create_dir(&directory).unwrap();
    let existing = b"an earlier delivery expectation";
    fs::write(directory.join(RECEIPT_FILE), existing).unwrap();
    let pin = PinnedDirectory::open_absolute(&media).unwrap();
    let cancel = AtomicBool::new(false);
    let mut review = PreparedSetupDelivery::review(&package, PASSWORD, &cancel).unwrap();
    assert!(deliver_ciphertext(&mut review.package, &pin, &cancel).is_err());
    assert_eq!(fs::read(directory.join(RECEIPT_FILE)).unwrap(), existing);
    assert!(!directory.join(FILE).exists());
}
#[test]
fn automatic_retry_acknowledges_only_identical_completed_ciphertext_and_receipt() {
    let (_temp, root, package) = fixture();
    let media = root.join("media");
    fs::create_dir(&media).unwrap();
    let pin = PinnedDirectory::open_absolute(&media).unwrap();
    let cancel = AtomicBool::new(false);
    let mut review = PreparedSetupDelivery::review(&package, PASSWORD, &cancel).unwrap();
    let first = deliver_ciphertext(&mut review.package, &pin, &cancel).unwrap();
    let original = fs::read(media.join(&first.relative_path)).unwrap();
    let recovered = existing::verify(&mut review.package, &pin, &cancel)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.expectation(), first.expectation());
    assert_eq!(
        fs::read(media.join(&first.relative_path)).unwrap(),
        original
    );
    let path = media.join(&first.relative_path);
    let mut changed = original;
    let byte = changed.last_mut().unwrap();
    *byte ^= 1;
    fs::write(&path, &changed).unwrap();
    assert!(existing::verify(&mut review.package, &pin, &cancel).is_err());
    assert_eq!(fs::read(&path).unwrap(), changed);
    let receipt = media.join(DIRECTORY).join(RECEIPT_FILE);
    let forged =
        b"{\"schema\":1,\"sealed_bytes\":1000,\"ciphertext_sha256\":\"not the same package\"}";
    fs::write(&receipt, forged).unwrap();
    assert!(existing::verify(&mut review.package, &pin, &cancel).is_err());
    assert_eq!(fs::read(receipt).unwrap(), forged);
}
#[test]
fn changed_package_wrong_password_and_redirected_filesystem_paths_are_refused() {
    let (_temp, root, package) = fixture();
    let cancel = AtomicBool::new(false);
    assert!(PreparedSetupDelivery::review(&package, b"incorrect", &cancel).is_err());
    let media = root.join("media");
    fs::create_dir(&media).unwrap();
    let outside = root.join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, media.join(DIRECTORY)).unwrap();
    let pin = PinnedDirectory::open_absolute(&media).unwrap();
    let mut review = PreparedSetupDelivery::review(&package, PASSWORD, &cancel).unwrap();
    assert!(deliver_ciphertext(&mut review.package, &pin, &cancel).is_err());
    assert!(!outside.join(FILE).exists());
    fs::remove_file(media.join(DIRECTORY)).unwrap();
    let mut bytes = fs::read(&package).unwrap();
    let end = bytes.len() - 1;
    bytes[end] ^= 1;
    fs::write(&package, bytes).unwrap();
    assert!(deliver_ciphertext(&mut review.package, &pin, &cancel).is_err());
    assert!(!media.join(DIRECTORY).join(FILE).exists());
}
#[test]
fn media_partition_binding_rejects_other_devices_and_unmounted_filesystems() {
    let target:TargetDrive=serde_json::from_value(serde_json::json!({"name":"sdb","model":"USB fixture","stable_id":"device-serial","size":1073741824u64,"kind":"Usb","tran":"usb","removable":true,"is_system":false,"partitions":[]})).unwrap();
    let partition = BlockDevice {
        name: "/dev/sdb1".into(),
        kind: "part".into(),
        fstype: Some("vfat".into()),
        uuid: Some("fs-fixture".into()),
        partuuid: Some("part-fixture".into()),
        maj_min: Some("8:17".into()),
        mountpoints: vec!["/fixture/mount".into()],
        ..BlockDevice::default()
    };
    let disk = BlockDevice {
        name: "/dev/sdb".into(),
        kind: "disk".into(),
        size: target.size,
        children: vec![partition],
        ..BlockDevice::default()
    };
    let device = libc::makedev(8, 17);
    let binding = select_partition(
        std::slice::from_ref(&disk),
        &target,
        Path::new("/fixture/mount"),
        device,
    )
    .unwrap();
    assert_eq!(binding.name, "/dev/sdb1");
    assert!(
        select_partition(
            std::slice::from_ref(&disk),
            &target,
            Path::new("/ungranted/path"),
            device
        )
        .is_err()
    );
    assert!(
        select_partition(
            std::slice::from_ref(&disk),
            &target,
            Path::new("/fixture/mount"),
            libc::makedev(8, 18)
        )
        .is_err()
    );
    let mut removed = disk;
    removed.children[0].read_only = true;
    assert!(select_partition(&[removed], &target, Path::new("/fixture/mount"), device).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn completed_retry_requires_exact_receipt_and_actual_ciphertext() {
    let (_temp, root, package) = fixture();
    let media = root.join("completed-generation");
    fs::create_dir(&media).unwrap();
    fs::set_permissions(&media, fs::Permissions::from_mode(0o700)).unwrap();
    let pin = PinnedDirectory::open_absolute(&media).unwrap();
    let cancel = AtomicBool::new(false);
    let mut delivery = PreparedSetupDelivery::review(&package, PASSWORD, &cancel).unwrap();
    assert!(
        verify_completed_ciphertext(&mut delivery, &pin, &cancel)
            .unwrap()
            .is_none()
    );
    let receipt = deliver_completed_ciphertext(&mut delivery, &pin, &cancel).unwrap();
    let ciphertext = media.join(&receipt.relative_path);
    let receipt_path = media.join(DIRECTORY).join(RECEIPT_FILE);
    let original_receipt = fs::read(&receipt_path).unwrap();
    let original_ciphertext = fs::read(&ciphertext).unwrap();
    assert!(
        verify_completed_ciphertext(&mut delivery, &pin, &cancel)
            .unwrap()
            .is_some()
    );
    fs::remove_file(&receipt_path).unwrap();
    assert!(
        verify_completed_ciphertext(&mut delivery, &pin, &cancel)
            .unwrap()
            .is_none()
    );
    fs::write(&receipt_path, &original_receipt).unwrap();
    let mut other = delivery.expectation();
    other.ciphertext_sha256 = "00".repeat(32);
    fs::write(&receipt_path, serde_json::to_vec(&other).unwrap()).unwrap();
    assert!(verify_completed_ciphertext(&mut delivery, &pin, &cancel).is_err());
    assert_eq!(fs::read(&ciphertext).unwrap(), original_ciphertext);
    fs::write(&receipt_path, original_receipt).unwrap();
    let mut changed = original_ciphertext.clone();
    let last = changed.len() - 1;
    changed[last] ^= 1;
    fs::write(&ciphertext, changed).unwrap();
    assert!(verify_completed_ciphertext(&mut delivery, &pin, &cancel).is_err());
    assert_eq!(fs::read(&package).unwrap(), original_ciphertext);
}
