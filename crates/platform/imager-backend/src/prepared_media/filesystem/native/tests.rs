use super::*;
#[test]
fn grant_adapter_requires_canonical_descriptor_and_rejects_replaced_ancestors() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("parent");
    std::fs::create_dir(&parent).unwrap();
    let root = parent.join("medium");
    std::fs::create_dir(&root).unwrap();
    let pin = PinnedRoot::open_absolute(&root).unwrap();
    let authority = MediaFilesystemGrant {
        target: serde_json::from_value(serde_json::json!({"name":"sdb","model":"USB fixture","stable_id":"device-serial","size":1_073_741_824_u64,"kind":"Usb","tran":"usb","removable":true,"is_system":false,"partitions":[]})).unwrap(),
        root: root.clone(),
        pin,
        partition: PartitionBinding {
            name: "/dev/sdb1".into(), filesystem: "vfat".into(), uuid: "filesystem".into(),
            partition_uuid: "partition".into(), number: "8:17".into(),
        },
    };
    let exact = std::fs::File::open(&root).unwrap();
    authority.check_directory_identity(&exact).unwrap();
    let elsewhere = std::fs::File::open(temp.path()).unwrap();
    assert!(authority.check_directory_identity(&elsewhere).is_err());
    let clone = authority.clone();
    clone.check_directory_identity(&exact).unwrap();
    std::fs::rename(&parent, temp.path().join("old-parent")).unwrap();
    std::fs::create_dir(&parent).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert!(authority.check_directory_identity(&exact).is_err());
    assert!(clone.check_directory_identity(&exact).is_err());
    let redirected = temp.path().join("redirected");
    std::os::unix::fs::symlink(&root, &redirected).unwrap();
    assert!(PinnedRoot::open_absolute(&redirected).is_err());
}

#[test]
fn media_partition_binding_rejects_other_devices_and_unmounted_filesystems() {
    let target:TargetDrive=serde_json::from_value(serde_json::json!({"name":"sdb","model":"USB fixture","stable_id":"device-serial","size":1_073_741_824_u64,"kind":"Usb","tran":"usb","removable":true,"is_system":false,"partitions":[]})).unwrap();
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
