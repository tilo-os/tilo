//! Durable installer identity storage, separate from the host's active persona; dropping a handle
//! never deletes a prepared identity or its provider secret.
use crate::{destination_security, migration::PinnedDirectory, migration_durability};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use tilo_identity::provisioning::{IdentityStorage, create_primary_identity};
use zeroize::Zeroizing;

pub struct PreparedIdentity {
    directory: PathBuf,
    pin: Arc<PinnedDirectory>,
    public_key: [u8; 32],
    enrolled: Option<Arc<tilo_identity::enrolled_transport::EnrolledDevice>>,
    current: Option<Arc<tilo_identity::enrolled_transport::CurrentDeviceReference>>,
}

impl PreparedIdentity {
    pub fn recover_created(directory: &Path) -> io::Result<Self> {
        let pin = PinnedDirectory::open_absolute(directory)?;
        pin.validate_destination()?;
        destination_security::check_private_file(&pin.file)?;
        let metadata =
            tilo_identity_client::load_metadata_from_path(&directory.join("primary.json"))
                .map_err(|error| io::Error::other(error.to_string()))?;
        Self::resume(directory, metadata.group_public_key)
    }

    pub fn resume_enrolled(
        operation: &Path,
        record: &Path,
        public_key: [u8; 32],
        device_key: [u8; 32],
    ) -> io::Result<Self> {
        let now = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos(),
        )
        .map_err(io::Error::other)?;
        let enrolled = tilo_identity::enrolled_transport::EnrolledDevice::review(
            &operation.join("future-device"),
            record,
            public_key,
            device_key,
            now,
        )?;
        Self::from_enrolled(Arc::new(enrolled))
    }

    /// Creates exclusively in a fresh private directory under a granted private parent. Neither the
    /// host's active identity nor an existing preparation is adopted or replaced, and a failed attempt
    /// keeps ownership for explicit recovery.
    pub fn create(
        directory: &Path,
        name: &str,
        color: [u8; 3],
        storage: IdentityStorage,
    ) -> io::Result<Self> {
        let parent = PinnedDirectory::open_absolute(directory.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Identity ownership requires a private parent",
            )
        })?)?;
        parent.validate_destination()?;
        destination_security::check_private_file(&parent.file)?;
        let pin = parent.create_child(directory.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Identity ownership requires a new directory",
            )
        })?)?;
        create_primary_identity(directory, name, color, storage)?;
        pin.validate()?;
        let signer = tilo_identity::store::load_from_path(directory.join("primary.json"))
            .map_err(|error| io::Error::other(error.to_string()))?;
        migration_durability::sync_directory(&pin.file)?;
        migration_durability::sync_directory(&parent.file)?;
        Ok(Self {
            directory: directory.to_owned(),
            pin,
            public_key: signer.group_public_key(),
            enrolled: None,
            current: None,
        })
    }

    /// The source backup is preserved and no replacement signing key is minted.
    pub fn restore_backup(
        directory: &Path,
        backup: &Path,
        password: Zeroizing<String>,
    ) -> io::Result<Self> {
        let info = tilo_identity::store::verify_portable_backup(backup, password.as_bytes())?;
        let parent = PinnedDirectory::open_absolute(
            directory
                .parent()
                .ok_or_else(|| io::Error::other("missing ownership parent"))?,
        )?;
        parent.validate_destination()?;
        destination_security::check_private_file(&parent.file)?;
        let pin = parent.create_child(
            directory
                .file_name()
                .ok_or_else(|| io::Error::other("missing ownership name"))?,
        )?;
        tilo_identity::store::import_portable_backup_matching(
            backup,
            password.as_bytes(),
            directory,
            "primary",
            info.group_public_key,
        )?;
        pin.validate()?;
        migration_durability::sync_directory(&pin.file)?;
        migration_durability::sync_directory(&parent.file)?;
        Self::resume(directory, info.group_public_key)
    }

    pub fn from_enrolled(
        enrolled: Arc<tilo_identity::enrolled_transport::EnrolledDevice>,
    ) -> io::Result<Self> {
        enrolled.validate()?;
        let directory = enrolled.identity_directory().to_owned();
        let pin = PinnedDirectory::open_absolute(&directory)?;
        pin.validate_destination()?;
        destination_security::check_private_file(&pin.file)?;
        Ok(Self {
            directory,
            pin,
            public_key: enrolled.public_key(),
            enrolled: Some(enrolled),
            current: None,
        })
    }
    pub fn from_current(
        directory: &Path,
        current: Arc<tilo_identity::enrolled_transport::CurrentDeviceReference>,
    ) -> io::Result<Self> {
        current.validate()?;
        let parent = PinnedDirectory::open_absolute(
            directory
                .parent()
                .ok_or_else(|| io::Error::other("missing ownership parent"))?,
        )?;
        parent.validate_destination()?;
        destination_security::check_private_file(&parent.file)?;
        let pin = parent.create_child(
            directory
                .file_name()
                .ok_or_else(|| io::Error::other("missing ownership name"))?,
        )?;
        let metadata = current.metadata()?;
        let bytes = tilo_identity::store::public_member_record_with_presentation(
            &metadata.display_name,
            metadata.avatar_color,
            metadata.group_public_key,
            metadata.profile_photo,
        )?;
        use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
        let storage =
            StorageDirectory::from_file(migration_durability::readable_directory(&pin.file)?)?;
        storage
            .publish(
                std::ffi::OsStr::new("primary.json"),
                &bytes,
                PublishMode::IfAbsent,
                Privacy::OwnerOnly,
            )
            .map_err(tilo_files::native::atomic::PublicationError::into_io_error)?;
        current.validate()?;
        let prepared = Self {
            directory: directory.to_owned(),
            pin,
            public_key: current.public_key(),
            enrolled: None,
            current: Some(current),
        };
        prepared.validate()?;
        Ok(prepared)
    }
    pub fn resume_current(
        directory: &Path,
        claim: &tilo_identity::enrolled_transport::CurrentDeviceDeclaration,
    ) -> io::Result<Self> {
        let current = Arc::new(Self::review_current()?);
        if &current.declaration()? != claim {
            return Err(io::Error::other("Current identity changed"));
        }
        let prepared = Self {
            directory: directory.to_owned(),
            pin: PinnedDirectory::open_absolute(directory)?,
            public_key: current.public_key(),
            enrolled: None,
            current: Some(current),
        };
        prepared.validate()?;
        Ok(prepared)
    }
    pub fn review_current() -> io::Result<tilo_identity::enrolled_transport::CurrentDeviceReference>
    {
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        let system = paths
            .device_cert
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| io::Error::other("missing host identity namespace"))?;
        let directory = tilo_identity::store::identity_dir_from_env()?;
        let current =
            tilo_identity::enrolled_transport::CurrentDeviceReference::review(system, &directory)?;
        let registry = tilo_identity::store::IdentityRegistryStore::at(&directory).load()?;
        if !registry.identities.values().any(|profile| {
            profile.id == hex::encode(current.public_key())
                && profile.user_public_key == current.public_key()
                && profile.record_path == directory.join(current.identity_record_name())
        }) {
            return Err(io::Error::other("Current identity profile is unavailable"));
        }
        current.validate()?;
        Ok(current)
    }
    pub fn current_source(
        &self,
    ) -> Option<Arc<tilo_identity::enrolled_transport::CurrentDeviceReference>> {
        self.current.clone()
    }
    pub fn current_declaration(
        &self,
    ) -> io::Result<Option<tilo_identity::enrolled_transport::CurrentDeviceDeclaration>> {
        self.current
            .as_ref()
            .map(|current| current.declaration())
            .transpose()
    }
    pub fn current_device_public_key(&self) -> Option<[u8; 32]> {
        self.current
            .as_ref()
            .map(|current| current.device_public_key())
    }
    pub fn device_public_key(&self) -> Option<[u8; 32]> {
        self.current_device_public_key()
            .or_else(|| self.enrolled_device_public_key())
    }
    pub fn record_name(&self) -> Option<&str> {
        self.current
            .as_ref()
            .map(|current| current.identity_record_name())
            .or_else(|| self.enrolled_record_name())
    }
    pub fn export_current(
        &self,
        destination: &Path,
        password: &[u8],
        binding: &str,
    ) -> io::Result<tilo_identity::enrolled_transport::TransportReceipt> {
        self.validate()?;
        self.current
            .as_ref()
            .ok_or_else(|| io::Error::other("not current identity storage"))?
            .export_password_sealed(destination, password, binding)
    }
    pub fn enrolled_device_public_key(&self) -> Option<[u8; 32]> {
        self.enrolled
            .as_ref()
            .map(|enrolled| enrolled.device_public_key())
    }
    pub fn export_enrolled(
        &self,
        destination: &Path,
        password: &[u8],
        binding: &str,
    ) -> io::Result<tilo_identity::enrolled_transport::TransportReceipt> {
        self.enrolled
            .as_ref()
            .ok_or_else(|| io::Error::other("identity is not enrolled ownership"))?
            .export_password_sealed(destination, password, binding)
    }

    pub fn metadata(&self) -> io::Result<tilo_identity_client::IdentityMetadata> {
        self.validate()?;
        if let Some(current) = &self.current {
            return current.metadata();
        }
        if let Some(enrolled) = &self.enrolled {
            return enrolled.metadata();
        }
        tilo_identity_client::load_metadata_from_path(&self.directory.join("primary.json"))
            .map_err(|error| io::Error::other(error.to_string()))
    }

    /// Only the exact previously acknowledged identity; no new key is generated when its provider is
    /// unavailable or its metadata was replaced.
    pub fn resume(directory: &Path, expected_public_key: [u8; 32]) -> io::Result<Self> {
        let pin = PinnedDirectory::open_absolute(directory)?;
        pin.validate_destination()?;
        destination_security::check_private_file(&pin.file)?;
        let prepared = Self {
            directory: directory.to_owned(),
            pin,
            public_key: expected_public_key,
            enrolled: None,
            current: None,
        };
        prepared.validate()?;
        Ok(prepared)
    }

    pub fn ownership_directory(&self) -> &Path {
        &self.directory
    }

    pub fn enrolled_device_label(&self) -> Option<&str> {
        self.current
            .as_ref()
            .map(|current| current.device_label())
            .or_else(|| {
                self.enrolled
                    .as_ref()
                    .map(|enrolled| enrolled.device_label())
            })
    }

    pub fn enrolled_record_name(&self) -> Option<&str> {
        self.enrolled
            .as_ref()
            .map(|enrolled| enrolled.identity_record_name())
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    /// The root capability and public record a fresh destination joins before it can receive this
    /// person's prepared package. The root secret stays in this vault; only a signature leaves it.
    pub fn bootstrap_source_identity(&self) -> io::Result<tilo_handoff::SourceIdentity> {
        self.validate()?;
        if self.enrolled.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "This identity's root is held by another device",
            ));
        }
        let metadata = self.metadata()?;
        let record = tilo_handoff::public_member_record(
            &metadata.display_name,
            metadata.avatar_color,
            self.public_key,
            metadata.profile_photo.clone(),
        )?;
        // A prepared or restored identity keeps its root here; an explicitly reviewed current
        // identity signs through this session's active root record, which the signer rechecks.
        let record_path = if self.current.is_some() {
            let directory = tilo_identity::store::identity_dir_from_env()?;
            tilo_identity::store::resolve_active_identity(&directory).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "This identity's active root is unavailable",
                )
            })?
        } else {
            self.directory.join("primary.json")
        };
        let expected = self.public_key;
        let signer = Arc::new(move |digest: &[u8]| {
            let identity = tilo_identity::store::load_from_path(record_path.clone())
                .map_err(|error| io::Error::other(error.to_string()))?;
            if identity.group_public_key() != expected {
                return Err(io::Error::other(
                    "The prepared identity's root signer changed",
                ));
            }
            identity
                .sign(digest)
                .map_err(|error| io::Error::other(error.to_string()))
        });
        Ok(tilo_handoff::SourceIdentity {
            identity_public_key: self.public_key,
            identity_record: record,
            certificates: Vec::new(),
            signer,
        })
    }

    fn validate(&self) -> io::Result<()> {
        self.pin.validate_destination()?;
        if let Some(current) = &self.current {
            current.validate()?;
            let metadata =
                tilo_identity_client::load_metadata_from_path(&self.directory.join("primary.json"))
                    .map_err(|error| io::Error::other(error.to_string()))?;
            let expected = current.metadata()?;
            if metadata.group_public_key != self.public_key
                || metadata.display_name != expected.display_name
                || metadata.avatar_color != expected.avatar_color
                || metadata.profile_photo != expected.profile_photo
            {
                return Err(io::Error::other("Current identity capsule changed"));
            }
            return self.pin.validate();
        }
        if let Some(enrolled) = &self.enrolled {
            return enrolled.validate();
        }
        let signer = tilo_identity::store::load_from_path(self.directory.join("primary.json"))
            .map_err(|error| io::Error::other(error.to_string()))?;
        if signer.group_public_key() != self.public_key {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Prepared identity was replaced",
            ));
        }
        self.pin.validate()
    }

    pub fn update_photo(
        &self,
        photo: Option<tilo_identity_client::ProfilePhoto>,
    ) -> io::Result<()> {
        self.validate()?;
        if self.enrolled.is_some() || self.current.is_some() {
            if self.metadata()?.profile_photo == photo {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "enrolled identity presentation is retained from its approval",
            ));
        }
        tilo_identity::store::update_identity_photo(&self.directory, "primary", photo)?;
        self.validate()
    }

    pub fn update_presentation(
        &self,
        name: &str,
        photo: Option<tilo_identity_client::ProfilePhoto>,
    ) -> io::Result<tilo_identity_client::IdentityMetadata> {
        self.validate()?;
        let before = self.metadata()?;
        if self.enrolled.is_some() || self.current.is_some() {
            if before.display_name == name && before.profile_photo == photo {
                return Ok(before);
            }
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "enrolled identity presentation is retained from its approval",
            ));
        }
        let metadata = tilo_identity::store::update_identity_presentation(
            &self.directory,
            "primary",
            name,
            before.avatar_color,
            photo,
        )?;
        self.validate()?;
        Ok(metadata)
    }

    pub fn update_name(&self, name: &str) -> io::Result<tilo_identity_client::IdentityMetadata> {
        self.validate()?;
        let metadata = self.metadata()?;
        self.update_profile(name, metadata.avatar_color)
    }

    /// Change public presentation without minting or replacing the identity.
    pub fn update_profile(
        &self,
        name: &str,
        color: [u8; 3],
    ) -> io::Result<tilo_identity_client::IdentityMetadata> {
        self.validate()?;
        if self.enrolled.is_some() || self.current.is_some() {
            let metadata = self.metadata()?;
            if metadata.display_name == name && metadata.avatar_color == color {
                return Ok(metadata);
            }
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "enrolled identity presentation is retained from its approval",
            ));
        }
        let metadata =
            tilo_identity::store::update_identity_profile(&self.directory, "primary", name, color)?;
        self.validate()?;
        Ok(metadata)
    }

    /// Seals a new installation transport artifact through canonical identity backup ownership; this does
    /// not mark first-login recovery backup complete.
    pub fn export(
        &self,
        destination: &Path,
        password: Zeroizing<String>,
    ) -> io::Result<tilo_identity::store::PortableBackupInfo> {
        crate::startup_password::validate(&password)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        self.validate()?;
        let parent = PinnedDirectory::open_absolute(destination.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Identity transport requires a parent",
            )
        })?)?;
        parent.validate_destination()?;
        destination_security::check_private_file(&parent.file)?;
        if self.current.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Current identity requires its certified reference",
            ));
        }
        let info = tilo_identity::store::export_portable_backup(
            self.directory.join("primary.json"),
            destination,
            password.as_bytes(),
        )?;
        self.validate()?;
        if info.group_public_key != self.public_key {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Identity transport contains another identity",
            ));
        }
        Ok(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn real_preparation_survives_drop_edits_and_encrypted_target_import() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(tilo_identity::store::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let ownership = root.join("prepared");
        let prepared =
            PreparedIdentity::create(&ownership, "Ada", [1, 2, 3], IdentityStorage::Files).unwrap();
        let public_key = prepared.public_key();
        let before = tilo_identity::store::load_from_path(ownership.join("primary.json"))
            .unwrap()
            .sign(b"proof")
            .unwrap();
        prepared.update_profile("Ada", [4, 5, 6]).unwrap();
        let photo = tilo_identity_client::ProfilePhoto::new(1, 1, vec![15, 80, 210, 255]).unwrap();
        prepared.update_photo(Some(photo.clone())).unwrap();
        assert_eq!(
            prepared.metadata().unwrap().profile_photo,
            Some(photo.clone())
        );
        drop(prepared);
        let prepared = PreparedIdentity::resume(&ownership, public_key).unwrap();
        let artifact = root.join("identity.sealed");
        let password = "several unrelated installation words";
        assert_eq!(
            prepared
                .export(&artifact, Zeroizing::new(password.into()))
                .unwrap()
                .group_public_key,
            public_key
        );
        assert!(
            PreparedIdentity::create(&ownership, "Replacement", [0; 3], IdentityStorage::Files)
                .is_err()
        );
        assert!(PreparedIdentity::resume(&ownership, [0; 32]).is_err());
        assert!(
            prepared
                .export(&artifact, Zeroizing::new(password.into()))
                .is_err()
        );
        let target = root.join("target");
        let imported = tilo_identity::store::import_portable_backup_matching(
            &artifact,
            password.as_bytes(),
            &target,
            "primary",
            public_key,
        )
        .unwrap();
        let signer = tilo_identity::store::load_from_path(imported).unwrap();
        assert_eq!(signer.display_name(), "Ada");
        assert_eq!(signer.avatar_color(), [4, 5, 6]);
        assert_eq!(signer.profile_photo(), Some(&photo));
        assert_eq!(signer.sign(b"proof").unwrap(), before);
        assert!(ownership.join("primary.json").exists());
    }
}
