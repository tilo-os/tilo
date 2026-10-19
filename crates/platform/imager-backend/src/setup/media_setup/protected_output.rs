//! Root-private custody and delegated ciphertext inbox, provisioned by writer.
use super::*;
use std::{
    ffi::OsStr,
    fs::File,
    os::{fd::AsRawFd, unix::fs::MetadataExt as _},
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
const RECORD: &str = "original-pending-area.json";
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PendingArea {
    pub schema: u32,
    pub medium: crate::media_layout::MediaPartitionBinding,
    pub setup: crate::imager_model::PreparedSetupExpectation,
    pub plans_sha256: [u8; 32],
    pub operation: String,
    pub original_uid: u32,
    pub name: String,
    pub inode: u64,
}
impl PendingArea {
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(denied("Protected pending area receipt exceeds bounds"));
        }
        let record: Self = serde_json::from_slice(bytes)
            .map_err(|_| denied("Protected pending area receipt invalid"))?;
        if record.schema != 1
            || record.original_uid == 0
            || record.original_uid == u32::MAX
            || record.inode == 0
            || record.plans_sha256 == [0; 32]
            || !record.setup.valid()
            || uuid::Uuid::parse_str(&record.operation).is_err()
            || record.name != format!(".tilo-pending-{}", record.operation)
        {
            return Err(denied("Protected pending area receipt invalid"));
        }
        Ok(record)
    }
}
fn denied(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn operation(layout: &crate::setup::OfflineLayout, caller: u32) -> io::Result<String> {
    use crate::setup::migration_handoff::{HomePolicy, Plan};
    // Match the canonical offline source-disk admission bounds. This list is
    // produced from Home geometry plus every selected installation geometry;
    // system installation owners need not equal the authenticated Home caller.
    if caller == 0
        || caller == u32::MAX
        || !layout.setup.valid()
        || layout.source_plans.is_empty()
        || layout.source_plans.len() > 128
        || layout
            .source_plans
            .iter()
            .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
            .is_none_or(|total| total > 9 * 1024 * 1024)
    {
        return Err(denied(
            "Protected pending area source metadata exceeds bounds",
        ));
    }
    let plan = Plan::parse(&layout.source_plans[0])?;
    // OriginalSource::capture_in requires Home selection for pending Copy.
    if !plan.categories.home
        || plan.home_policy != HomePolicy::Copy
        || plan.flatpak_inventory.is_some()
        || plan.home_uid() != caller
        || !uuid::Uuid::parse_str(&plan.operation_id)
            .is_ok_and(|id| !id.is_nil() && id.to_string() == plan.operation_id)
    {
        return Err(denied(
            "Protected pending area source differs from authenticated Home caller",
        ));
    }
    for bytes in &layout.source_plans[1..] {
        match crate::setup::migration_handoff::SourceGeometry::parse(bytes)? {
            crate::setup::migration_handoff::SourceGeometry::Home(installation) => {
                if !plan.categories.flatpak
                    || installation.operation_id != plan.operation_id
                    || installation.categories.home
                    || !installation.categories.flatpak
                    || installation.categories.accounts
                    || installation.home_policy != HomePolicy::Copy
                    || installation.flatpak_inventory.is_some()
                {
                    return Err(denied(
                        "Protected pending area installation source differs from Home operation",
                    ));
                }
            }
            crate::setup::migration_handoff::SourceGeometry::Mounted { operation, .. } => {
                if operation != plan.operation_id {
                    return Err(denied(
                        "Protected pending mounted source belongs to another operation",
                    ));
                }
            }
        }
    }
    Ok(plan.operation_id)
}
pub fn validate_pending_caller(
    layout: &crate::setup::OfflineLayout,
    caller: u32,
) -> io::Result<()> {
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(denied(
            "Protected pending area requires authenticated root writer",
        ));
    }
    operation(layout, caller)?;
    Ok(())
}
fn create(
    grant: &MediaFilesystemGrant,
    layout: &crate::setup::OfflineLayout,
    caller: u32,
) -> io::Result<()> {
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(denied(
            "Protected pending area requires authenticated root writer",
        ));
    }
    grant.validate()?;
    create_directory(&grant.pin, layout, caller)?;
    grant.validate()
}
fn create_directory(
    root: &Arc<PinnedDirectory>,
    layout: &crate::setup::OfflineLayout,
    caller: u32,
) -> io::Result<()> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(denied("Protected pending area requires root"));
    }
    let operation = operation(layout, caller)?;
    let private = delivery_directory(root)?;
    let storage = StorageDirectory::from_file(durability::readable_directory(&private.file)?)?;
    storage.validate_privacy(Privacy::OwnerOnly)?;
    let name = format!(".tilo-pending-{operation}");
    // Existing directories are never adopted, chmodded or repaired.
    let stage = root.create_child(OsStr::new(&name))?;
    // Pins may be O_PATH. Reopen only '.' beneath the retained fresh pin for
    // publication, fchown and fsync; its ambient name never supplies authority.
    let file = durability::readable_directory(&stage.file)?;
    stage.validate_destination()?;
    let pinned = stage.file.metadata()?;
    let readable = file.metadata()?;
    if pinned.dev() != readable.dev() || pinned.ino() != readable.ino() {
        return Err(denied("Protected pending area held directory differs"));
    }
    use sha2::Digest;
    let record = PendingArea {
        schema: 1,
        medium: layout.binding.clone(),
        setup: layout.setup.clone(),
        plans_sha256: sha2::Sha256::digest(serde_json::to_vec(&layout.source_plans)?).into(),
        operation,
        original_uid: caller,
        name,
        inode: file.metadata()?.ino(),
    };
    let bytes = serde_json::to_vec(&record)?;
    if bytes.len() > 4096 {
        return Err(denied("Protected pending area receipt exceeds bounds"));
    }
    // Durable custody before the first delegated write. Only the fresh sibling
    // is delegated; GID stays root and root-private witness/store stay hidden.
    storage
        .publish(
            OsStr::new(RECORD),
            &bytes,
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        )
        .map_err(|e| e.into_io_error())?;
    if unsafe { libc::fchown(file.as_raw_fd(), caller, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    drop(stage);
    file.sync_all()?;
    durability::sync_directory(&root.file)?;
    review_directory(root, layout, caller)?;
    Ok(())
}
/// Existing TargetWriteLock is held after raw/GPT readback and before helper
/// success. Only initial whole-drive authorization can reach this formatter.
pub fn provision_after_write(
    target: &TargetDrive,
    layout: &crate::setup::OfflineLayout,
    caller: u32,
    should_cancel: &impl Fn() -> bool,
) -> io::Result<()> {
    if should_cancel() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    validate_pending_caller(layout, caller)?;
    // Independently refuse overlap BEFORE any writable mount/format/witness.
    let daemon = block_devices::udisks::Udisks::system().map_err(io::Error::other)?;
    let raw = daemon
        .open_read_only(&target.dev_path())
        .map_err(io::Error::other)?;
    crate::write::attest_target_descriptor(target, &raw)?;
    if raw.metadata()?.rdev() != layout.target_number {
        return Err(denied("Protected pending raw medium differs"));
    }
    let stable = target
        .stable_id
        .as_deref()
        .ok_or_else(|| denied("Protected pending medium identity missing"))?;
    if crate::media_layout::binding(&raw, target.size, stable)? != layout.binding {
        return Err(denied("Protected pending GPT differs"));
    }
    crate::setup::migration_handoff::validate_offline_source_disks(
        &layout.source_plans,
        raw.metadata()?.rdev(),
    )?;
    let cancel = AtomicBool::new(false);
    automatic::deliver_home(target, &cancel, |grant| {
        if should_cancel() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let daemon = block_devices::udisks::Udisks::system().map_err(io::Error::other)?;
        let raw = daemon
            .open_read_only(&target.dev_path())
            .map_err(io::Error::other)?;
        if raw.metadata()?.rdev() != layout.target_number {
            return Err(denied("Protected pending raw medium differs"));
        }
        grant.validate_offline_media(&raw, &layout.binding)?;
        crate::setup::migration_handoff::validate_offline_source_disks(
            &layout.source_plans,
            layout.target_number,
        )?;
        create(grant, layout, caller)?;
        if should_cancel() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        grant.validate_offline_media(&raw, &layout.binding)
    })
}
/// Canonical root read-media helper supplies UID from its authenticated peer.
/// Returns exact delegated descriptor only, never the root witness namespace.
pub fn review(
    grant: &MediaFilesystemGrant,
    layout: &crate::setup::OfflineLayout,
    caller: u32,
) -> io::Result<File> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(denied("Protected pending area review requires root"));
    }
    grant.validate()?;
    let file = review_directory(&grant.pin, layout, caller)?;
    grant.validate()?;
    Ok(file)
}
fn review_directory(
    root: &Arc<PinnedDirectory>,
    layout: &crate::setup::OfflineLayout,
    caller: u32,
) -> io::Result<File> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(denied("Protected pending area review requires root"));
    }
    let operation = operation(layout, caller)?;
    let private = delivery_directory(root)?;
    let storage = StorageDirectory::from_file(durability::readable_directory(&private.file)?)?;
    storage.validate_privacy(Privacy::OwnerOnly)?;
    let mut receipt = storage.open_read(OsStr::new(RECORD), Privacy::OwnerOnly)?;
    if receipt.metadata()?.len() > 4096 {
        return Err(denied("Protected pending area receipt exceeds bounds"));
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    receipt.by_ref().take(4097).read_to_end(&mut bytes)?;
    let record = PendingArea::parse(&bytes)?;
    use sha2::Digest;
    let digest: [u8; 32] = sha2::Sha256::digest(serde_json::to_vec(&layout.source_plans)?).into();
    if record.schema != 1
        || record.medium != layout.binding
        || record.setup != layout.setup
        || record.plans_sha256 != digest
        || record.operation != operation
        || record.original_uid != caller
        || record.name != format!(".tilo-pending-{operation}")
    {
        return Err(denied("Protected pending area receipt differs"));
    }
    let metadata = crate::setup::nofollow::metadata_nofollow(&root.file, Path::new(&record.name))?;
    use cap_primitives::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK);
    let file = cap_primitives::fs::open(&root.file, Path::new(&record.name), &options)?;
    let actual = file.metadata()?;
    if !actual.is_dir()
        || actual.uid() != caller
        || actual.gid() != 0
        || actual.mode() & 0o7777 != 0o700
        || actual.ino() != record.inode
        || actual.ino() != cap_fs_ext::MetadataExt::ino(&metadata)
        || actual.dev() != root.file.metadata()?.dev()
    {
        return Err(denied("Protected pending area descriptor differs"));
    }
    root.validate_destination()?;
    Ok(file)
}

/// Existing root-session command owns this synchronous callback until its FD
/// recipient finishes and the selected-medium mount is reattested/unmounted.
pub fn with_reviewed_inbox<T>(
    target: &TargetDrive,
    held_raw: &File,
    layout: &crate::setup::OfflineLayout,
    caller: u32,
    cancel: &AtomicBool,
    receive: impl FnOnce(File, Vec<u8>) -> io::Result<T>,
) -> io::Result<T> {
    validate_pending_caller(layout, caller)?;
    check_cancel(cancel)?;
    crate::write::attest_target_descriptor(target, held_raw)?;
    if held_raw.metadata()?.rdev() != layout.target_number {
        return Err(denied("Protected pending raw medium differs"));
    }
    crate::setup::migration_handoff::validate_offline_source_disks(
        &layout.source_plans,
        held_raw.metadata()?.rdev(),
    )?;
    automatic::with_existing_home(target, &layout.binding, cancel, |grant| {
        if held_raw.metadata()?.rdev() != layout.target_number {
            return Err(denied("Protected pending raw medium differs"));
        }
        grant.validate_offline_media(held_raw, &layout.binding)?;
        let inbox = review(grant, layout, caller)?;
        let private = delivery_directory(&grant.pin)?;
        let storage = StorageDirectory::from_file(durability::readable_directory(&private.file)?)?;
        let mut receipt = storage.open_read(OsStr::new(RECORD), Privacy::OwnerOnly)?;
        use std::io::Read;
        let mut bytes = Vec::new();
        receipt.by_ref().take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(denied("Protected pending area receipt exceeds bounds"));
        }
        // Keep the actual directory identity across the authenticated recipient's
        // synchronous work. A replaced name or receipt must not receive a finish
        // acknowledgement even when the recipient reports cancellation/error.
        let held_inbox = inbox.try_clone()?;
        let receipt_identity = receipt.metadata()?;
        let expected_receipt = bytes.clone();
        let result = receive(inbox, bytes);
        grant.validate_offline_media(held_raw, &layout.binding)?;
        let reviewed_inbox = review(grant, layout, caller)?;
        let held = held_inbox.metadata()?;
        let reviewed = reviewed_inbox.metadata()?;
        if held.dev() != reviewed.dev() || held.ino() != reviewed.ino() {
            return Err(denied("Protected pending area changed during delivery"));
        }
        let mut current_receipt = storage.open_read(OsStr::new(RECORD), Privacy::OwnerOnly)?;
        let current_identity = current_receipt.metadata()?;
        let mut current_bytes = Vec::new();
        current_receipt
            .by_ref()
            .take(4097)
            .read_to_end(&mut current_bytes)?;
        if current_identity.dev() != receipt_identity.dev()
            || current_identity.ino() != receipt_identity.ino()
            || current_bytes != expected_receipt
        {
            return Err(denied(
                "Protected pending area receipt changed during delivery",
            ));
        }
        grant.validate_offline_media(held_raw, &layout.binding)?;
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn layout(caller: u32) -> crate::setup::OfflineLayout {
        let operation = uuid::Uuid::new_v4().to_string();
        let plan = serde_json::json!({"schema":1,"operation_id":operation,
            "categories":{"home":true,"flatpak":false,"accounts":false},
            "home":{"volume":{"disk_guid":"11111111-1111-4111-8111-111111111111",
                "disk_bytes":67108864,"sector_bytes":512,"partition_guid":"22222222-2222-4222-8222-222222222222",
                "first_lba":2048,"last_lba":4095,"filesystem_uuid":"33333333-3333-4333-8333-333333333333","filesystem":"ext4"},
                "relative":{"platform":"Unix","units":b"home/person".to_vec()},"inode":1234,"original_uid":caller}});
        let source =
            crate::setup::migration_handoff::Plan::parse(&serde_json::to_vec(&plan).unwrap())
                .unwrap();
        crate::setup::OfflineLayout {
            binding: crate::media_layout::MediaPartitionBinding {
                schema: 1,
                target_stable_id: "fixture".into(),
                disk_guid: uuid::Uuid::new_v4().to_string(),
                partition_guid: uuid::Uuid::new_v4().to_string(),
                number: 3,
                first_lba: 8192,
                last_lba: 16383,
                sector_bytes: 512,
                profile_offset: 4096,
            },
            reserved_bytes: 4096,
            setup: crate::imager_model::PreparedSetupExpectation {
                schema: 1,
                sealed_bytes: 128,
                ciphertext_sha256: "a".repeat(64),
            },
            source_plans: vec![source.bytes().unwrap()],
            target_number: 1,
        }
    }
    fn combined_layout(caller: u32) -> crate::setup::OfflineLayout {
        use crate::setup::{
            flatpak_transfer::HandoffInventory,
            migration_handoff::{Categories, Plan},
        };
        let mut layout = layout(caller);
        let mut home = Plan::parse(&layout.source_plans[0]).unwrap();
        let mut installation: serde_json::Value =
            serde_json::from_slice(&layout.source_plans[0]).unwrap();
        installation["categories"] = serde_json::to_value(Categories {
            home: false,
            flatpak: true,
            accounts: false,
        })
        .unwrap();
        installation["home"]["relative"]["units"] = serde_json::json!(b"var/lib/flatpak".to_vec());
        installation["home"]["inode"] = serde_json::json!(5678);
        installation["home"]["original_uid"] = serde_json::json!(0);
        let installation = Plan::parse(&serde_json::to_vec(&installation).unwrap()).unwrap();
        // Bounded public metadata fixture, passed through the genuine inventory
        // parser and producer. This does not manufacture a source grant or claim
        // that these fixture geometries resolve to a live kernel volume.
        let inventory = serde_json::json!({"schema":1,"inventory":{"refs":[],"remotes":[]},"selected":[],
            "installations":[{"installation":{"System":"default"},"source_plan":installation.bytes().unwrap(),
                "repositories":[],"deployments":[],"config_sha256":vec![0u8;32]}]});
        let inventory = HandoffInventory::parse(&serde_json::to_vec(&inventory).unwrap()).unwrap();
        home.categories.flatpak = true;
        home.flatpak_inventory = Some(serde_json::from_slice(&inventory.bytes().unwrap()).unwrap());
        layout.source_plans = crate::setup::source_geometry::original_source_plans(&home).unwrap();
        assert_eq!(layout.source_plans.len(), 2);
        layout
    }
    #[test]
    fn combined_source_admission_validates_the_entire_produced_list() {
        use crate::setup::migration_handoff::Plan;
        let layout = combined_layout(99);
        let home = Plan::parse(&layout.source_plans[0]).unwrap();
        assert_eq!(operation(&layout, 99).unwrap(), home.operation_id);
        assert_eq!(Plan::parse(&layout.source_plans[1]).unwrap().home_uid(), 0);
        assert!(operation(&layout, 100).is_err());
        let mut without_home = layout.clone();
        let mut source = Plan::parse(&without_home.source_plans[0]).unwrap();
        source.categories.home = false;
        without_home.source_plans[0] = source.bytes().unwrap();
        assert!(operation(&without_home, 99).is_err());
        let mut with_accounts = layout.clone();
        let mut source = Plan::parse(&with_accounts.source_plans[0]).unwrap();
        source.categories.accounts = true;
        with_accounts.source_plans[0] = source.bytes().unwrap();
        assert_eq!(operation(&with_accounts, 99).unwrap(), home.operation_id);
        for mutate in 0..9 {
            let mut invalid = layout.clone();
            match mutate {
                0 => invalid.source_plans.clear(),
                1 => invalid.source_plans.swap(0, 1),
                2 => invalid.source_plans[1] = b"{}".to_vec(),
                3 => {
                    let mut source = Plan::parse(&invalid.source_plans[1]).unwrap();
                    source.operation_id = uuid::Uuid::new_v4().to_string();
                    invalid.source_plans[1] = source.bytes().unwrap();
                }
                4 => invalid.source_plans.push(invalid.source_plans[0].clone()),
                5 => invalid
                    .source_plans
                    .resize(129, layout.source_plans[1].clone()),
                6 => {
                    let mut source = Plan::parse(&invalid.source_plans[0]).unwrap();
                    source.home_policy =
                        crate::setup::migration_handoff::HomePolicy::AccessReadWrite;
                    invalid.source_plans[0] = source.bytes().unwrap();
                }
                7 => {
                    let mut source = Plan::parse(&invalid.source_plans[0]).unwrap();
                    source.categories.flatpak = false;
                    invalid.source_plans[0] = source.bytes().unwrap();
                }
                8 => {
                    let mut source: serde_json::Value =
                        serde_json::from_slice(&invalid.source_plans[1]).unwrap();
                    source["home"]["volume"]["last_lba"] = serde_json::json!(u64::MAX);
                    invalid.source_plans[1] = serde_json::to_vec(&source).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                operation(&invalid, 99).is_err(),
                "invalid source case {mutate}"
            );
        }
    }
    #[test]
    fn protected_pending_area_actual_private_provisioning_and_replacement_refusal() {
        if rustix::process::geteuid().is_root() {
            tilo_test_env::require_capability!(
                Chown,
                tilo_test_env::chown_to(99),
                "as root the fixture provisions the area for uid 99"
            );
        }
        let temp = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let pin = PinnedDirectory::open_absolute(temp.path()).unwrap();
        let uid = rustix::process::geteuid().as_raw();
        let layout = combined_layout(if uid == 0 { 99 } else { uid });
        if uid != 0 {
            let error = create_directory(&pin, &layout, uid).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
            return;
        }
        create_directory(&pin, &layout, 99).unwrap();
        let held = review_directory(&pin, &layout, 99).unwrap();
        assert_eq!(held.metadata().unwrap().uid(), 99);
        assert_eq!(held.metadata().unwrap().mode() & 0o7777, 0o700);
        let private = temp.path().join(DIRECTORY);
        assert_eq!(std::fs::metadata(&private).unwrap().uid(), 0);
        assert_eq!(std::fs::metadata(&private).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(
            std::fs::metadata(private.join(RECORD)).unwrap().mode() & 0o7777,
            0o600
        );
        let mut changed = layout.clone();
        let mut source: serde_json::Value =
            serde_json::from_slice(&changed.source_plans[1]).unwrap();
        source["home"]["inode"] = serde_json::json!(5679);
        changed.source_plans[1] = serde_json::to_vec(&source).unwrap();
        assert!(review_directory(&pin, &changed, 99).is_err());
        assert!(create_directory(&pin, &layout, 99).is_err());
        assert!(review_directory(&pin, &layout, 100).is_err());
        let operation = operation(&layout, 99).unwrap();
        let name = temp.path().join(format!(".tilo-pending-{operation}"));
        std::fs::rename(&name, temp.path().join("retired-inbox")).unwrap();
        std::fs::create_dir(&name).unwrap();
        std::fs::set_permissions(&name, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(review_directory(&pin, &layout, 99).is_err());
        assert_eq!(held.metadata().unwrap().uid(), 99);
    }
}
