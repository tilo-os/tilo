//! Genuine original-user producer for the optional receiving Apps+Home case.
//! Every declaration below comes from the normal capture and registration path.
use crate::setup::{
    flatpak_transfer::InstalledRef, journey::prepared_media::PreparedMedia,
    migration_intent::MigrationIntent, prepared_identity::PreparedIdentity,
    prepared_setup::PreparedSetupReview, staging,
};
use std::{
    io,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
use zeroize::Zeroizing;

pub(super) struct SelectedGrant {
    identity: PreparedIdentity,
    intent: MigrationIntent,
    prepared: Arc<PreparedMedia>,
}
fn denied() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
impl SelectedGrant {
    /// `home` has already been populated by the real signed runnable source
    /// owner, on the genuinely held GPT filesystem, before either grant exists.
    pub(super) fn capture(
        home: &Path,
        directory: &Path,
        selected: Vec<InstalledRef>,
        installation: &staging::plan::Plan,
    ) -> io::Result<Self> {
        (|| {
            if rustix::process::getuid().as_raw() != 1000 || selected.is_empty() {
                return Err(denied());
            }
            let document = SourceLeaf::open(&home.join("receiver-document.txt"))?;
            let excluded =
                SourceLeaf::open(&home.join(".var/app/org.tilo.Excluded/data/document"))?;
            let shared = SourceLeaf::open(&home.join("owner-document"))?;
            let grants = directory.join("grant");
            std::fs::create_dir(&grants)?;
            let captured = MigrationIntent::capture_computer_applications(
                home,
                &grants,
                [true, true, false],
                selected,
            )?;
            let intent = MigrationIntent::load(&grants)?;
            if intent != captured || intent.for_media()?.home_uid() != 1000 {
                return Err(denied());
            }
            // Persisted grant reload must preserve both actual independently
            // captured permissions; no serialized bool repairs a missing child.
            let captured_apps = captured.computer_applications()?.ok_or_else(denied)?;
            let loaded_apps = intent.computer_applications()?.ok_or_else(denied)?;
            if captured_apps != loaded_apps {
                return Err(denied());
            }
            let identity = PreparedIdentity::create(
                &directory.join("identity"),
                "Owned selected application person",
                [1, 2, 3],
                tilo_identity_host::provisioning::IdentityStorage::Files,
            )?;
            let password = Zeroizing::new(
                String::from_utf8(b"owned retained source fixture installation words".to_vec())
                    .map_err(io::Error::other)?,
            );
            let initial = PreparedMedia::prepare_with_migration(
                &identity,
                None,
                None,
                Some(intent.clone()),
                password,
                uuid::Uuid::new_v4().to_string(),
                None,
            )?;
            let stage = uuid::Uuid::new_v4().to_string();
            let reservation = intent.application_minimum();
            let cancel = AtomicBool::new(false);
            let prepared = initial.prepare_computer_applications(
                &intent,
                &stage,
                installation,
                reservation,
                &cancel,
            )?;
            let original_plan = intent.for_media()?;
            if prepared.desktop_manifest()?.portable_home.is_some()
                || prepared.desktop_manifest()?.completed_home.is_some()
                || prepared
                    .desktop_manifest()?
                    .retained_home
                    .as_ref()
                    .is_none_or(|p| p.as_plan() != &original_plan)
            {
                return Err(denied());
            }
            let original = std::fs::read(prepared.package())?;
            for replay in [&initial, &prepared] {
                let cached = replay.prepare_computer_applications(
                    &intent,
                    &stage,
                    installation,
                    reservation,
                    &cancel,
                )?;
                if !Arc::ptr_eq(&cached, &prepared) {
                    return Err(denied());
                }
            }
            // A valid second placement retains the SAME source/stage UUID and
            // reservation. Only the actual full staging geometry changes.
            let sector = installation.basis.disk.sector_bytes;
            let bytes = |first: u64, last: u64| -> io::Result<u64> {
                last.checked_sub(first)
                    .and_then(|n| n.checked_add(1))
                    .and_then(|n| n.checked_mul(sector))
                    .ok_or_else(denied)
            };
            let target = bytes(
                installation.target_extent.first_lba,
                installation.target_extent.last_lba,
            )?;
            let changed = staging::plan::compute(
                &installation.basis,
                staging::plan::Requirements {
                    staging_esp_bytes: bytes(
                        installation.staging_esp.first_lba,
                        installation.staging_esp.last_lba,
                    )?,
                    staging_root_bytes: bytes(
                        installation.staging_root.first_lba,
                        installation.staging_root.last_lba,
                    )?,
                    target_bytes: target
                        .checked_add(staging::ALIGN_BYTES)
                        .ok_or_else(denied)?,
                },
            )
            .map_err(|error| {
                io::Error::other(format!("Actual second placement refused: {error:?}"))
            })?;
            if changed == *installation {
                return Err(denied());
            }
            for replay in [&initial, &prepared] {
                if replay
                    .prepare_computer_applications(&intent, &stage, &changed, reservation, &cancel)
                    .is_ok()
                {
                    return Err(denied());
                }
                let cached = replay.prepare_computer_applications(
                    &intent,
                    &stage,
                    installation,
                    reservation,
                    &cancel,
                )?;
                if !Arc::ptr_eq(&cached, &prepared) || std::fs::read(cached.package())? != original
                {
                    return Err(denied());
                }
            }
            prepared.validate_for_write(&cancel)?;
            document.check()?;
            excluded.check()?;
            shared.check()?;
            Ok(Self {
                identity,
                intent,
                prepared,
            })
        })()
    }
    fn package(&self) -> io::Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        self.prepared.validate_for_write(&AtomicBool::new(false))?;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(self.prepared.package())
    }
    pub(super) fn review(&self, quarantine: &Path) -> io::Result<PreparedSetupReview> {
        self.prepared.validate_for_write(&AtomicBool::new(false))?;
        let review = PreparedSetupReview::open(
            &self.prepared.package(),
            b"owned retained source fixture installation words",
            quarantine,
            &AtomicBool::new(false),
        )?;
        let original_plan = self.intent.for_media()?;
        if review.manifest() != self.prepared.desktop_manifest()?
            || review.manifest().identity_public_key != self.identity.public_key()
            || review
                .manifest()
                .retained_home
                .as_ref()
                .is_none_or(|p| p.as_plan() != &original_plan)
        {
            return Err(denied());
        }
        Ok(review)
    }
}

struct SourceLeaf {
    file: std::fs::File,
    path: std::path::PathBuf,
    attrs: (u64, u64, u64, u32, u32, u32, i64, i64, i64, i64),
    digest: Vec<u8>,
}
impl SourceLeaf {
    fn attrs(m: &std::fs::Metadata) -> (u64, u64, u64, u32, u32, u32, i64, i64, i64, i64) {
        use std::os::unix::fs::MetadataExt;
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.uid(),
            m.gid(),
            m.mode(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    }
    fn read(file: &std::fs::File) -> io::Result<Vec<u8>> {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::FileExt;
        let mut hash = Sha256::new();
        let mut offset = 0;
        let mut buffer = [0; 65536];
        loop {
            let n = file.read_at(&mut buffer, offset)?;
            if n == 0 {
                break;
            }
            offset += n as u64;
            if offset > 16 * 1024 * 1024 {
                return Err(denied());
            }
            hash.update(&buffer[..n]);
        }
        Ok(hash.finalize().to_vec())
    }
    fn open(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        let facts = file.metadata()?;
        if !facts.is_file() {
            return Err(denied());
        }
        let result = Self {
            attrs: Self::attrs(&facts),
            digest: Self::read(&file)?,
            file,
            path: path.into(),
        };
        result.check()?;
        Ok(result)
    }
    fn check(&self) -> io::Result<()> {
        if Self::attrs(&self.file.metadata()?) != self.attrs
            || Self::attrs(&std::fs::symlink_metadata(&self.path)?) != self.attrs
            || Self::read(&self.file)? != self.digest
            || Self::attrs(&self.file.metadata()?) != self.attrs
        {
            return Err(denied());
        }
        Ok(())
    }
}

/// Called only by the frontend fixture dispatcher with an admitted native parent.
pub(super) fn fixture(request: &serde_json::Value) -> serde_json::Value {
    let parent = std::path::PathBuf::from(request["registration_main"].as_str().unwrap());
    imager_backend::setup::qualification_registration::with_frontend_parent(parent, || {
        let home = Path::new(request["home"].as_str().unwrap());
        let directory = Path::new(request["directory"].as_str().unwrap());
        let selected =
            serde_json::from_value(request["selected"].clone()).map_err(io::Error::other)?;
        let installation =
            serde_json::from_value(request["installation"].clone()).map_err(io::Error::other)?;
        let grant = SelectedGrant::capture(home, directory, selected, &installation)?;
        let review = grant.review(&directory.join("frontend-checked"))?;
        let package = directory.join("frontend-selected-setup.sealed");
        let mut input = grant.package()?;
        use std::os::unix::fs::OpenOptionsExt;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&package)?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        Ok(serde_json::json!({"package": package, "manifest": review.manifest()}))
    })
    .unwrap()
}
