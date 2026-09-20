//! Unix storage composition. Native handles, privacy and durability remain in atomic-file.
use super::*;
use atomic_file::{
    DirectoryReconcileError, ExistingCommitError, FileIdentity, Privacy, ProvisionError,
    PublicationError, PublicationState, PublishMode, StorageDirectory,
};
use std::{
    ffi::OsStr,
    fmt,
    fs::File,
    io::{self, Read},
};

#[derive(Debug)]
pub(super) enum StorageError {
    Io(&'static str, io::Error),
    Provision(ProvisionError),
    Reconcile(DirectoryReconcileError),
    KeyCommit(ExistingCommitError),
    Publication(&'static str, PublicationError),
    PublicationAdmission(PublicationError, Box<StorageError>),
    Custody(String),
    Recovery,
    Serialize(serde_json::Error),
    Crypto,
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(phase, error) => write!(f, "vault {phase}: {error}"),
            Self::Provision(error) => write!(
                f,
                "vault directory provisioning {:?}: {error}",
                error.state()
            ),
            Self::Reconcile(error) => write!(
                f,
                "vault existing ancestry commit (binding changed={}): {error}",
                error.source_changed()
            ),
            Self::KeyCommit(error) => write!(
                f,
                "vault existing key commit (binding changed={}): {error}",
                error.source_changed()
            ),
            Self::Publication(phase, error) => {
                write!(f, "vault {phase} {:?}: {error}", error.state())
            }
            Self::PublicationAdmission(publication, admission) => write!(
                f,
                "vault key publication {:?}: {publication}; commit retry refused: {admission}",
                publication.state()
            ),
            Self::Custody(error) => write!(f, "vault Secret Service: {error}"),
            Self::Recovery => {
                f.write_str("vault recovery required: ciphertext exists without its key")
            }
            Self::Serialize(error) => write!(f, "vault serialization: {error}"),
            Self::Crypto => f.write_str("vault encryption failed"),
        }
    }
}
fn io_phase(phase: &'static str) -> impl FnOnce(io::Error) -> StorageError {
    move |error| StorageError::Io(phase, error)
}
struct Root {
    directory: StorageDirectory,
    path: PathBuf,
    identity: FileIdentity,
}
impl Root {
    fn admit(path: &Path, directory: StorageDirectory) -> Result<Self, StorageError> {
        directory
            .validate_namespace_control()
            .map_err(io_phase("namespace admission"))?;
        let identity = directory
            .identity()
            .map_err(io_phase("directory identity"))?;
        let root = Self {
            directory,
            path: path.to_owned(),
            identity,
        };
        root.validate()?;
        Ok(root)
    }
    fn validate(&self) -> Result<(), StorageError> {
        self.directory
            .validate_namespace_control()
            .map_err(io_phase("namespace revalidation"))?;
        let named = StorageDirectory::open(&self.path).map_err(io_phase("directory binding"))?;
        if named.identity().map_err(io_phase("directory identity"))? != self.identity {
            return Err(StorageError::Io(
                "directory binding",
                io::Error::other("vault root was replaced"),
            ));
        }
        Ok(())
    }
    fn prepare(path: &Path) -> Result<Self, StorageError> {
        match StorageDirectory::open(path) {
            Ok(held) => {
                let root = Self::admit(path, held)?;
                atomic_file::reconcile_directory(path, &root.directory, Privacy::Inherit)
                    .map_err(StorageError::Reconcile)?;
                root.validate()?;
                Ok(root)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let held = atomic_file::provision_directory(path, Privacy::OwnerOnly)
                    .map_err(StorageError::Provision)?;
                Self::admit(path, held)
            }
            Err(error) => Err(StorageError::Io("directory open", error)),
        }
    }
}

// The live handle prevents identity reuse while bytes/identity authorize a save.
struct LocalKey {
    bytes: Zeroizing<[u8; KEY_LEN]>,
    identity: FileIdentity,
    _file: File,
}
fn read_key(root: &Root) -> Result<Option<LocalKey>, StorageError> {
    root.validate()?;
    #[cfg(test)]
    super::unix_tests::key_checkpoint(super::unix_tests::KeyPoint::Open)
        .map_err(io_phase("key admission"))?;
    let file = match root
        .directory
        .open_read(OsStr::new("vault.key"), Privacy::OwnerOnly)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StorageError::Io("key admission", error)),
    };
    #[cfg(test)]
    super::unix_tests::key_checkpoint(super::unix_tests::KeyPoint::Identity)
        .map_err(io_phase("key identity"))?;
    let identity = FileIdentity::of(&file).map_err(io_phase("key identity"))?;
    // Fixed zeroizing storage, including a one-byte EOF probe. No unbounded
    // allocation or metadata-length assumption may admit key material.
    let mut bounded = Zeroizing::new([0u8; KEY_LEN + 1]);
    let mut length = 0;
    let mut reader = &file;
    while length < bounded.len() {
        #[cfg(test)]
        super::unix_tests::key_checkpoint(super::unix_tests::KeyPoint::Read)
            .map_err(io_phase("key read"))?;
        match reader.read(&mut bounded[length..]) {
            Ok(0) => break,
            Ok(count) => length += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(StorageError::Io("key read", error)),
        }
    }
    if length != KEY_LEN {
        return Err(StorageError::Io(
            "key length",
            io::Error::new(
                io::ErrorKind::InvalidData,
                "key must contain exactly 32 bytes and EOF",
            ),
        ));
    }
    let mut bytes = Zeroizing::new([0; KEY_LEN]);
    bytes.copy_from_slice(&bounded[..KEY_LEN]);
    root.validate()?;
    Ok(Some(LocalKey {
        bytes,
        identity,
        _file: file,
    }))
}
fn commit_key(root: &Root, key: LocalKey) -> Result<Zeroizing<[u8; KEY_LEN]>, StorageError> {
    root.validate()?;
    root.directory
        .commit_existing(OsStr::new("vault.key"), &key.identity, Privacy::OwnerOnly)
        .map_err(StorageError::KeyCommit)?;
    root.validate()?;
    Ok(key.bytes)
}
fn ensure_initialization(root: &Root) -> Result<(), StorageError> {
    root.validate()?;
    #[cfg(test)]
    super::unix_tests::key_checkpoint(super::unix_tests::KeyPoint::CiphertextQuery)
        .map_err(io_phase("ciphertext absence query"))?;
    match root
        .directory
        .open_read(OsStr::new("vault.enc"), Privacy::Inherit)
    {
        Ok(_) => Err(StorageError::Recovery),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StorageError::Io("ciphertext absence query", error)),
    }
}
fn generate() -> Result<Zeroizing<[u8; KEY_LEN]>, StorageError> {
    #[cfg(test)]
    super::unix_tests::key_checkpoint(super::unix_tests::KeyPoint::Generate)
        .map_err(io_phase("key generation"))?;
    let mut key = Zeroizing::new([0; KEY_LEN]);
    getrandom::getrandom(key.as_mut_slice())
        .map_err(|error| StorageError::Io("key generation", io::Error::other(error.to_string())))?;
    Ok(key)
}
fn local_key(root: &Root) -> Result<Zeroizing<[u8; KEY_LEN]>, StorageError> {
    if let Some(key) = read_key(root)? {
        return commit_key(root, key);
    }
    ensure_initialization(root)?;
    let candidate = generate()?;
    root.validate()?;
    match root.directory.publish(
        OsStr::new("vault.key"),
        candidate.as_ref(),
        PublishMode::IfAbsent,
        Privacy::OwnerOnly,
    ) {
        Ok(()) => {}
        Err(error) if error.state() == PublicationState::PublishedDurabilityUnknown => {
            if let Err(admission) = root.validate() {
                return Err(StorageError::PublicationAdmission(
                    error,
                    Box::new(admission),
                ));
            }
            error
                .retry_commit()
                .map_err(|error| StorageError::Publication("key commit retry", error))?;
            root.validate()?;
        }
        Err(error)
            if error.state() == PublicationState::NotPublished
                && error.io_error().kind() == io::ErrorKind::AlreadyExists =>
        {
            // Staging exhaustion can also report AlreadyExists. Only an actual
            // admitted winner below establishes authority; no recursive RNG.
        }
        Err(error) => return Err(StorageError::Publication("key publication", error)),
    }
    drop(candidate);
    root.validate()?;
    let winner = read_key(root)?.ok_or_else(|| {
        StorageError::Io(
            "key winner admission",
            io::Error::new(io::ErrorKind::NotFound, "no key winner exists"),
        )
    })?;
    commit_key(root, winner)
}
fn service_key(root: &Root) -> Result<Zeroizing<[u8; KEY_LEN]>, StorageError> {
    if let Some(key) = lookup_secret_service_key().map_err(StorageError::Custody)? {
        return Ok(key);
    }
    // Same bounded held local reader as local custody. No fallback after a
    // failed lookup. Transport and ambient removal remain the existing protocol.
    let (key, migrated) = match read_key(root)? {
        Some(key) => (commit_key(root, key)?, true),
        None => {
            ensure_initialization(root)?;
            (generate()?, false)
        }
    };
    root.validate()?;
    store_secret_service_key(&key).map_err(StorageError::Custody)?;
    match lookup_secret_service_key().map_err(StorageError::Custody)? {
        Some(stored) if stored.as_ref() == key.as_ref() => {
            if migrated && let Err(error) = std::fs::remove_file(root.path.join("vault.key")) {
                tracing::warn!(%error, "accounts: migrated vault key but could not remove local copy");
            }
            Ok(key)
        }
        _ => Err(StorageError::Custody(
            "service did not return the stored key".into(),
        )),
    }
}
// Retained key-creation tests enter the same admitted-root/key-publication
// operations as save(), rather than the superseded path-based implementation.
#[cfg(test)]
pub(super) fn load_or_create_local_key_for_test(
    path: &Path,
) -> Result<Zeroizing<[u8; KEY_LEN]>, StorageError> {
    let root = Root::prepare(path)?;
    local_key(&root)
}

pub(super) fn save(vault: &SecretVault) -> Result<(), StorageError> {
    let root = Root::prepare(&vault.dir)?;
    let key = match vault.custody {
        SecretCustody::LocalFile => local_key(&root)?,
        SecretCustody::SecretService => service_key(&root)?,
    };
    root.validate()?;
    let plaintext =
        Zeroizing::new(serde_json::to_vec(&vault.entries).map_err(StorageError::Serialize)?);
    #[cfg(test)]
    super::unix_tests::key_checkpoint(super::unix_tests::KeyPoint::Encrypt)
        .map_err(io_phase("encryption"))?;
    let blob = seal(&key, &plaintext).ok_or(StorageError::Crypto)?;
    root.validate()?;
    root.directory
        .publish(
            OsStr::new("vault.enc"),
            &blob,
            PublishMode::Replace,
            Privacy::Inherit,
        )
        .map_err(|error| StorageError::Publication("ciphertext publication", error))?;
    root.validate()
}
pub(super) fn load(
    path: &Path,
    custody: SecretCustody,
) -> Result<BTreeMap<String, Secret>, StorageError> {
    let held = match StorageDirectory::open(path) {
        Ok(held) => held,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(StorageError::Io("directory open", error)),
    };
    let root = Root::admit(path, held)?;
    let mut file = match root
        .directory
        .open_read(OsStr::new("vault.enc"), Privacy::Inherit)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(StorageError::Io("ciphertext admission", error)),
    };
    let service = if custody == SecretCustody::SecretService {
        lookup_secret_service_key().map_err(StorageError::Custody)?
    } else {
        None
    };
    let key = match service {
        Some(key) => key,
        None => read_key(&root)?.ok_or(StorageError::Recovery)?.bytes,
    };
    let mut blob = Vec::new();
    file.read_to_end(&mut blob)
        .map_err(io_phase("ciphertext read"))?;
    root.validate()?;
    let plaintext = open(&key, &blob).ok_or(StorageError::Crypto)?;
    serde_json::from_slice(&plaintext).map_err(StorageError::Serialize)
}
