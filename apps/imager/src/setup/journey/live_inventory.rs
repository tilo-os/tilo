//! Inventory storage derived only from the acknowledged preparation custody.
use super::draft::{DraftStatus, DraftStore, ReadyDraft};
#[cfg(all(test, feature = "native-capture-qualification"))]
use crate::setup::migration::PinnedDirectory;
use imager_backend::{
    migration::changed,
    migration_review::{InventoryScratch, InventoryScratchCustody},
    prepared_identity::PreparedIdentity,
};
use std::{io, sync::Arc};

struct PreparationCustody {
    prepared: Arc<PreparedIdentity>,
    ready: ReadyDraft,
}
impl InventoryScratchCustody for PreparationCustody {
    fn check(&self) -> io::Result<()> {
        let _current = self.ready.acquire_preparation_current()?;
        self.prepared.metadata().map(|_| ())
    }
}

pub(super) fn review(prepared: Arc<PreparedIdentity>) -> io::Result<Arc<InventoryScratch>> {
    prepared.metadata()?;
    let ready = match DraftStore::for_session()?.review()? {
        DraftStatus::Ready(ready) => ready,
        _ => return Err(changed("Inventory storage needs acknowledged preparation")),
    };
    let acknowledged = ready.resume()?;
    let current = ready.acquire_preparation_current()?;
    let operation = ready.operation_directory();
    if ready.acknowledged_public_key() != Some(prepared.public_key())
        || acknowledged.custody_directory() != prepared.custody_directory()
        || acknowledged.enrolled_device_public_key() != prepared.enrolled_device_public_key()
    {
        return Err(changed("Inventory preparation custody differs"));
    }
    imager_backend::setup::inventory_storage::review(
        &operation,
        Arc::new(PreparationCustody { prepared, ready }),
        current,
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
            deferred: false,
            target: None,
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
