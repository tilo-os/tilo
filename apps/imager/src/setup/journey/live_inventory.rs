//! Inventory storage derived only from the acknowledged preparation custody.
use super::draft::{DraftStatus, DraftStore, ReadyDraft};
use crate::setup::{
    home_capture::BackingWitness,
    migration::{PinnedDirectory, changed},
    migration_durability,
    migration_review::{InventoryScratch, InventoryScratchCustody},
    prepared_identity::PreparedIdentity,
};
use std::{io, os::fd::AsRawFd, sync::Arc};

const RESERVE: u64 = 256 * 1024 * 1024;

struct PreparationCustody {
    prepared: Arc<PreparedIdentity>,
    ready: ReadyDraft,
    backing: BackingWitness,
    directory: Arc<PinnedDirectory>,
}
impl InventoryScratchCustody for PreparationCustody {
    fn check(&self) -> io::Result<()> {
        let _current = self.ready.acquire_current()?;
        self.prepared.metadata()?;
        self.directory.validate_destination()?;
        self.backing.validate_directory(&self.directory.file)
    }
}

pub(super) fn review(prepared: Arc<PreparedIdentity>) -> io::Result<Arc<InventoryScratch>> {
    prepared.metadata()?;
    let ready = match DraftStore::for_session()?.review()? {
        DraftStatus::Ready(ready) => ready,
        _ => return Err(changed("Inventory storage needs acknowledged preparation")),
    };
    let acknowledged = ready.resume()?;
    let current = ready.acquire_current()?;
    let operation = ready.operation_directory();
    if ready.acknowledged_public_key() != Some(prepared.public_key())
        || acknowledged.custody_directory() != prepared.custody_directory()
        || acknowledged.enrolled_device_public_key() != prepared.enrolled_device_public_key()
    {
        return Err(changed("Inventory preparation custody differs"));
    }
    let parent = PinnedDirectory::open_absolute(&operation)?;
    parent.validate_destination()?;
    crate::setup::destination_security::check_private_file(&parent.file)?;
    // Fresh exclusive child, held below the already acknowledged operation.
    // A failed review preserves this private custody rather than deleting data.
    let parent_readable = migration_durability::readable_directory(&parent.file)?;
    let parent_backing = BackingWitness::review(&parent_readable)?;
    parent_backing.validate_directory(&parent.file)?;
    let name = format!("inventory-{}", uuid::Uuid::new_v4());
    let directory = parent.create_child(std::ffi::OsStr::new(&name))?;
    let readable = migration_durability::readable_directory(&directory.file)?;
    let backing = BackingWitness::review(&readable)?;
    let mut capacity: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: writes the complete statvfs for this retained directory descriptor.
    if unsafe { libc::fstatvfs(readable.as_raw_fd(), &raw mut capacity) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let allowance = capacity
        .f_bavail
        .checked_mul(capacity.f_frsize)
        .and_then(|bytes| bytes.checked_sub(RESERVE))
        .filter(|bytes| *bytes > 0)
        .ok_or(io::ErrorKind::StorageFull)?;
    drop(current);
    InventoryScratch::review_with_custody(
        directory.clone(),
        allowance,
        Arc::new(PreparationCustody {
            prepared,
            ready,
            backing,
            directory,
        }),
    )
}

#[cfg(all(test, feature = "native-capture-qualification"))]
mod tests {
    use super::*;
    use crate::setup::journey::draft::{DestinationChoice, IdentityKind, PublicChoices};
    use std::{fs, os::unix::fs::PermissionsExt, sync::atomic::AtomicBool};
    use tilo_identity_host::provisioning::IdentityStorage;

    #[test]
    fn acknowledged_live_preparation_spills_and_retirement_revokes_storage() {
        let mut env = tilo_test_env::ScopedEnv::new();
        let subject = std::env::var_os("TILO_FS_NATIVE_STORE")
            .expect("native inventory qualification requires owned block-backed Store");
        let subject_path = std::path::PathBuf::from(subject);
        let subject = PinnedDirectory::open_absolute(&subject_path).unwrap();
        subject.validate_destination().unwrap();
        crate::setup::destination_security::check_private_file(&subject.file).unwrap();
        let root = tempfile::tempdir_in(&subject_path).unwrap();
        let home = root.path().join("home");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        env.set("HOME", &home);
        env.set("XDG_STATE_HOME", home.join(".local/state"));
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        // An ambient tempfile selection must not participate in this path.
        env.set("TMPDIR", root.path().join("ungranted-missing"));
        let store = DraftStore::for_session().unwrap();
        let choices = PublicChoices {
            destination: Some(DestinationChoice::Another),
            migrate: Some(true),
            categories: [true, false, false],
            browser_domains: [false; 2],
            excluded_flatpak_apps: Default::default(),
            device_label: "Native storage fixture".into(),
            purpose: install_profile_model::default_purpose(),
        };
        let pending = store.reserve(&choices, IdentityKind::New).unwrap();
        let prepared = Arc::new(
            PreparedIdentity::create(
                &pending.identity_directory(),
                "Ada",
                [1, 2, 3],
                IdentityStorage::Files,
            )
            .unwrap(),
        );
        assert!(review(prepared.clone()).is_err());
        store.acknowledge(&pending, &prepared, None).unwrap();
        for index in 0..1025 {
            fs::write(
                home.join(format!("document-{index:04}")),
                b"original document",
            )
            .unwrap();
        }
        let scratch = review(prepared.clone()).unwrap();
        let cancel = AtomicBool::new(false);
        let source =
            crate::setup::portable_home_media::HomeSource::review_live_with_working_storage(
                &home,
                scratch.clone(),
                &cancel,
            )
            .unwrap();
        assert_eq!(source.summary().unwrap().files, 1025);
        source.validate_with_cancel(&cancel).unwrap();
        let refreshed = source.review_latest(&cancel).unwrap();
        refreshed.validate_with_cancel(&cancel).unwrap();
        store.retire().unwrap();
        assert!(source.validate_with_cancel(&cancel).is_err());
        assert!(refreshed.validate_with_cancel(&cancel).is_err());
        assert!(review(prepared.clone()).is_err());
        assert_eq!(
            fs::read(home.join("document-0000")).unwrap(),
            b"original document"
        );
        assert!(prepared.metadata().is_ok());
    }
}
