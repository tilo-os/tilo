//! Same-device initializer for the fixed, disposable paired qualifier only.
use super::*;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};

pub(super) fn output(target: &crate::model::TargetDrive, source: &Path) {
    assert_eq!(rustix::process::getuid().as_raw(), 0);
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    assert!(target.is_media_target());
    assert_eq!(target.tran.as_deref(), Some("usb"));
    assert_eq!(target.stable_id.as_deref(), Some("tilo-install-target"));
    // Resolve source ancestry from actual kernel devices, never an invented
    // Plan or a different filesystem with an arbitrary ownership marker.
    let source_dev = fs::metadata(source).unwrap().dev();
    let source_number = format!("{}:{}", libc::major(source_dev), libc::minor(source_dev));
    let devices = inventory();
    let parents: Vec<_> = devices
        .iter()
        .filter(|disk| {
            disk.kind == "disk"
                && disk
                    .children
                    .iter()
                    .any(|part| part.maj_min.as_deref() == Some(&source_number))
        })
        .collect();
    assert_eq!(parents.len(), 1, "unique actual source parent required");
    let source_disk = parents[0];
    assert_ne!(source_disk.name, target.dev_path());
    let outputs: Vec<_> = devices
        .iter()
        .filter(|disk| disk.name == target.dev_path())
        .collect();
    assert_eq!(outputs.len(), 1, "unique selected output required");
    let output = outputs[0];
    assert!(output.all_mountpoints().is_empty());
    assert!(
        output
            .children
            .iter()
            .all(|part| part.all_mountpoints().is_empty())
    );
    let releases = crate::release::bundled_releases().expect("matched embedded catalog required");
    assert!(
        !releases.is_empty(),
        "no self-certified raw image descriptor fallback"
    );
    let binding = crate::write::with_qualification_udisks_target(target, |device| {
        let mut matched = Vec::new();
        for release in &releases {
            if release.size == 0 || release.size >= target.size {
                continue;
            }
            let mut prefix = device.try_clone()?;
            prefix.seek(SeekFrom::Start(0))?;
            let mut bounded = prefix.take(release.size);
            let mut digest = Sha256::new();
            let mut bytes = [0u8; 128 * 1024];
            let mut total = 0u64;
            loop {
                let count = bounded.read(&mut bytes)?;
                if count == 0 {
                    break;
                }
                total += count as u64;
                digest.update(&bytes[..count]);
            }
            if total == release.size && hex::encode(digest.finalize()) == release.image_sha256 {
                matched.push(release);
            }
        }
        let [release] = matched.as_slice() else {
            return Err(std::io::Error::other(
                "seeded output must match exactly one actual embedded catalog image",
            ));
        };
        let binding = crate::media_layout::provision(
            &device,
            release.size,
            target.size,
            target.stable_id.as_deref().unwrap(),
            &|| false,
        )?;
        crate::write::attest_target_descriptor(target, &device)?;
        // Close the exclusive raw writer before native partition formatting,
        // retaining the SAME canonical write lock through mount retirement.
        drop(device);
        crate::setup::media_setup::automatic::deliver_home(
            target,
            &AtomicBool::new(false),
            |grant| {
                let root = grant.portable_home_grant()?.clone_reviewed_directory()?;
                grant.validate_offline_media(
                    &Udisks::system()
                        .map_err(std::io::Error::other)?
                        .open_read_only(&target.dev_path())
                        .map_err(std::io::Error::other)?,
                    &binding,
                )?;
                let mut options = cap_primitives::fs::OpenOptions::new();
                use cap_fs_ext::OpenOptionsFollowExt as _;
                use cap_primitives::fs::OpenOptionsExt as _;
                options
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .follow(cap_primitives::fs::FollowSymlinks::No);
                let mut marker = cap_primitives::fs::open(
                    &root,
                    Path::new(".tilo-original-output-qualification"),
                    &options,
                )?;
                marker.write_all(b"disposable-original-output-only")?;
                marker.sync_all()?;
                crate::setup::migration_durability::sync_directory(&root)?;
                Ok(())
            },
        )?;
        Ok(binding)
    })
    .unwrap();
    let daemon = Udisks::system().unwrap();
    let part = partition(&binding.partition_guid);
    assert!(
        daemon
            .filesystem(&part)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty(),
        "owned initializer mount must retire"
    );
    daemon.mount_migration(&part).unwrap();
    assert_eq!(
        daemon
            .filesystem(&part)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .len(),
        1
    );
}
