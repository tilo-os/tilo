//! Certified completion of an authenticated browser carrier. Callers authorize
//! the certificate and owner-controlled ancestor namespace before trusting it.
use crate::{PLAN_FILE, PROFILES_DIRECTORY, Plan};
use cap_fs_ext::OpenOptionsMaybeDirExt;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};
use tilo_files::native::atomic::{Privacy, StorageDirectory};

pub const FILE_NAME: &str = "completion.json";
pub const MAX_BYTES: usize = 64 * 1024;
/// Container under the canonical browser-continuity operation directory that
/// holds a first-launch projection prepared from a retained same-computer
/// source. The launcher admits the same exact `import/home` shape it already
/// admits for an installation-media projection.
pub const DEFERRED_CONTAINER: &str = "browser-import-deferred";
const DEFERRED_ROOT: [u8; 16] = [
    0x4f, 0x8a, 0x3d, 0x55, 0x7f, 0x8e, 0x5c, 0x31, 0xa1, 0xc2, 0x0d, 0x7e, 0xfe, 0xbb, 0x6a, 0x41,
];
const DEFERRED_SETUP_DOMAIN: &[u8] = b"tilo-browser-deferred-first-launch-v1\0";
const DOMAIN: &[u8] = b"tilo-browser-authenticated-carrier-completion-v1\0";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Stamp {
    inode: u64,
    owner: u32,
    mode: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn of(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(failed());
        }
        // Boot-local device numbers are deliberately excluded. Inodes, owner,
        // complete mode and immutable change times bind the retained namespace.
        Ok(Self {
            inode: metadata.ino(),
            owner: metadata.uid(),
            mode: metadata.mode(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: Vec<String>,
    stamp: Stamp,
    hash: Option<[u8; 32]>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Body {
    schema: u32,
    plan: Plan,
    setup_digest: [u8; 32],
    browser_digest: [u8; 32],
    operation: String,
    projection: PathBuf,
    entries: Vec<Entry>,
}
/// Unsigned capture: only the authenticated helper may certify this body.
pub struct Unsigned {
    body: Body,
}
/// Public bounded receipt; no password, signing key or browser content.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    body: Body,
    certificate: Vec<u8>,
    signature: Vec<u8>,
}
fn failed() -> io::Error {
    io::Error::other("Browser completion is unavailable or changed")
}
fn signing_bytes(body: &Body) -> io::Result<Vec<u8>> {
    let mut bytes = DOMAIN.to_vec();
    bytes.extend(serde_json::to_vec(body)?);
    if bytes.len() > MAX_BYTES {
        return Err(failed());
    }
    Ok(bytes)
}
impl Unsigned {
    /// Certify a capture after the helper revalidates its authenticated archive.
    ///
    /// # Errors
    /// Refuses an oversized body or certificate.
    pub fn sign(self, signer: &SigningKey, certificate: Vec<u8>) -> io::Result<Completion> {
        if certificate.is_empty() || certificate.len() > 4096 {
            return Err(failed());
        }
        let signature = signer.sign(&signing_bytes(&self.body)?).to_bytes().to_vec();
        let completion = Completion {
            body: self.body,
            certificate,
            signature,
        };
        completion.encode()?;
        Ok(completion)
    }
    /// Reattest the capture after independent archive verification and before signing.
    ///
    /// # Errors
    /// Refuses any changed projection member or namespace stamp.
    pub fn verify_unchanged(&self) -> io::Result<()> {
        if capture_entries(&self.body.projection, &self.body.plan)? != self.body.entries {
            return Err(failed());
        }
        Ok(())
    }
}
impl Completion {
    /// Read one owner-private regular receipt, bounded before decoding.
    ///
    /// # Errors
    /// Refuses unsafe paths, foreign files, oversized or malformed receipts.
    pub fn read(path: &Path) -> io::Result<Self> {
        let parent = open_directory(path.parent().ok_or_else(failed)?)?;
        let storage = StorageDirectory::from_file(parent)?;
        storage.validate_privacy(Privacy::OwnerOnly)?;
        let file = storage.open_read(path.file_name().ok_or_else(failed)?, Privacy::OwnerOnly)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() > MAX_BYTES as u64 {
            return Err(failed());
        }
        let mut bytes = Vec::new();
        file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BYTES {
            return Err(failed());
        }
        let receipt: Self = serde_json::from_slice(&bytes)?;
        receipt.encode()?;
        Ok(receipt)
    }
    /// Encode a bounded public completion receipt.
    ///
    /// # Errors
    /// Refuses malformed structure or output exceeding the fixed bound.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.body.plan.validate().map_err(io::Error::other)?;
        if self.body.schema != 1
            || self.body.entries.len() > 128
            || self.signature.len() != 64
            || self.certificate.is_empty()
            || self.certificate.len() > 4096
            || self.body.operation.is_empty()
            || self.body.operation.len() > 64
        {
            return Err(failed());
        }
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_BYTES {
            return Err(failed());
        }
        Ok(bytes)
    }
    #[must_use]
    pub fn certificate(&self) -> &[u8] {
        &self.certificate
    }
    #[must_use]
    pub fn plan(&self) -> &Plan {
        &self.body.plan
    }
    #[must_use]
    pub fn setup_digest(&self) -> [u8; 32] {
        self.body.setup_digest
    }
    #[must_use]
    pub fn browser_digest(&self) -> [u8; 32] {
        self.body.browser_digest
    }
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.body.operation
    }
    #[must_use]
    pub fn projection(&self) -> &Path {
        &self.body.projection
    }
    /// Match the previously authenticated identity, selection and both carriers.
    ///
    /// # Errors
    /// Refuses a different operation, person, source selection or ciphertext.
    pub fn verify_binding(
        &self,
        plan: &Plan,
        setup_digest: [u8; 32],
        browser_digest: [u8; 32],
        operation: &str,
    ) -> io::Result<()> {
        self.encode()?;
        if &self.body.plan != plan
            || self.body.setup_digest != setup_digest
            || self.body.browser_digest != browser_digest
            || self.body.operation != operation
        {
            return Err(failed());
        }
        Ok(())
    }
    /// Verify the signature and all current bounded projection bytes and stamps.
    /// The caller must first authorize the certificate for the current person,
    /// fleet and persisted local device, and admit the exact fixed workspace.
    ///
    /// # Errors
    /// Refuses a foreign signature, redirected namespace or changed data.
    pub fn verify_projection(&self, current_device_public: [u8; 32]) -> io::Result<PathBuf> {
        self.encode()?;
        let key = VerifyingKey::from_bytes(&current_device_public).map_err(io::Error::other)?;
        let signature = Signature::from_slice(&self.signature).map_err(io::Error::other)?;
        key.verify_strict(&signing_bytes(&self.body)?, &signature)
            .map_err(io::Error::other)?;
        if capture_entries(&self.body.projection, &self.body.plan)? != self.body.entries {
            return Err(failed());
        }
        Ok(self.body.projection.clone())
    }
}
/// Canonical operation directory for a deferred first-launch projection. The
/// operation name is the reviewed migration operation; the directory itself is
/// a UUID so the launcher's existing operation shape stays unchanged.
#[must_use]
pub fn deferred_root(home: &Path, operation: &str) -> PathBuf {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&DEFERRED_ROOT);
    hasher.update(operation.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    home.join(".local/state/tilo/browser-continuity")
        .join(uuid::Uuid::from_bytes(bytes).to_string())
}

/// Exact projection member for a deferred first-launch capture.
#[must_use]
pub fn deferred_projection(root: &Path) -> PathBuf {
    root.join(DEFERRED_CONTAINER).join("import").join("home")
}

/// Domain-separated binding for a completion that carries no installation
/// ciphertext. Both the publisher and the first-launch consumer derive it from
/// the reviewed migration operation, so a receipt cannot be replayed for a
/// different operation.
#[must_use]
pub fn deferred_setup_digest(operation: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(DEFERRED_SETUP_DOMAIN);
    hasher.update(operation.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Capture only the exact fixed files in a verified owner-private projection.
///
/// # Errors
/// Refuses unselected members, unstable files, unsafe paths or byte bounds.
pub fn capture(
    plan: Plan,
    setup_digest: [u8; 32],
    browser_digest: [u8; 32],
    operation: String,
    projection: PathBuf,
) -> io::Result<Unsigned> {
    plan.validate().map_err(io::Error::other)?;
    if operation.is_empty() || operation.len() > 64 {
        return Err(failed());
    }
    let entries = capture_entries(&projection, &plan)?;
    let unsigned = Unsigned {
        body: Body {
            schema: 1,
            plan,
            setup_digest,
            browser_digest,
            operation,
            projection,
            entries,
        },
    };
    signing_bytes(&unsigned.body)?;
    unsigned.verify_unchanged()?;
    Ok(unsigned)
}
fn readable(directory: &File) -> io::Result<File> {
    let mut options = cap_primitives::fs::OpenOptions::new();
    options.read(true).maybe_dir(true);
    cap_primitives::fs::open(directory, Path::new("."), &options)
}
fn open_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(failed());
    }
    let mut current = File::open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = readable(&cap_primitives::fs::open_dir_nofollow(
                    &current,
                    Path::new(name),
                )?)?;
            }
            _ => return Err(failed()),
        }
    }
    Ok(current)
}
fn exact(directory: &File, allowed: &[&str]) -> io::Result<()> {
    for entry in cap_primitives::fs::read_base_dir(directory)? {
        if !entry?
            .file_name()
            .to_str()
            .is_some_and(|name| allowed.contains(&name))
        {
            return Err(failed());
        }
    }
    Ok(())
}
fn directory_entry(directory: &File, path: Vec<String>) -> io::Result<Entry> {
    StorageDirectory::from_file(directory.try_clone()?)?.validate_namespace_control()?;
    Ok(Entry {
        path,
        stamp: Stamp::of(directory)?,
        hash: None,
    })
}
fn file_entry(
    directory: &StorageDirectory,
    name: &str,
    path: Vec<String>,
    budget: &mut u64,
) -> io::Result<Option<Entry>> {
    let file = match directory.open_read(name.as_ref(), Privacy::OwnerOnly) {
        Ok(file) => file,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                && matches!(name, "History-wal" | "places.sqlite-wal") =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(failed());
    }
    let stamp = Stamp::of(&file)?;
    let limit = if name == PLAN_FILE {
        crate::MAX_PLAN_BYTES as u64
    } else {
        crate::snapshot::MAX_FILE_BYTES
    };
    if stamp.length > limit {
        return Err(failed());
    }
    let total_limit = crate::snapshot::MAX_SNAPSHOT_BYTES
        .checked_add(u64::try_from(crate::MAX_PLAN_BYTES).map_err(|_| failed())?)
        .ok_or_else(failed)?;
    *budget = budget
        .checked_add(stamp.length)
        .filter(|total| *total <= total_limit)
        .ok_or_else(failed)?;
    let mut hasher = blake3::Hasher::new();
    let mut input = file.take(limit.checked_add(1).ok_or_else(failed)?);
    let mut bytes = 0u64;
    let mut buffer = vec![0; 65_536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(u64::try_from(count).map_err(|_| failed())?)
            .ok_or_else(failed)?;
        hasher.update(buffer.get(..count).ok_or_else(failed)?);
    }
    if bytes != stamp.length
        || Stamp::of(&input.into_inner())? != stamp
        || Stamp::of(&directory.open_read(name.as_ref(), Privacy::OwnerOnly)?)? != stamp
    {
        return Err(failed());
    }
    Ok(Some(Entry {
        path,
        stamp,
        hash: Some(*hasher.finalize().as_bytes()),
    }))
}
fn capture_entries(projection: &Path, plan: &Plan) -> io::Result<Vec<Entry>> {
    let root = open_directory(projection)?;
    let storage = StorageDirectory::from_file(root.try_clone()?)?;
    storage.validate_privacy(Privacy::OwnerOnly)?;
    exact(&root, &[PLAN_FILE, PROFILES_DIRECTORY])?;
    let mut entries = vec![directory_entry(&root, vec![])?];
    let mut total = 0;
    entries.extend(file_entry(
        &storage,
        PLAN_FILE,
        vec![PLAN_FILE.into()],
        &mut total,
    )?);
    let mut bytes = Vec::new();
    storage
        .open_read(PLAN_FILE.as_ref(), Privacy::OwnerOnly)?
        .take((crate::MAX_PLAN_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if Plan::decode(&bytes).map_err(io::Error::other)? != *plan {
        return Err(failed());
    }
    let profiles = readable(&cap_primitives::fs::open_dir_nofollow(
        &root,
        Path::new(PROFILES_DIRECTORY),
    )?)?;
    exact(
        &profiles,
        &plan
            .profiles
            .iter()
            .map(|profile| profile.id.as_str())
            .collect::<Vec<_>>(),
    )?;
    entries.push(directory_entry(&profiles, vec![PROFILES_DIRECTORY.into()])?);
    for profile in &plan.profiles {
        let directory = crate::snapshot::profile_directory(&root, profile)?;
        exact(&directory, profile.allowed_files())?;
        let path = vec![PROFILES_DIRECTORY.into(), profile.id.clone()];
        entries.push(directory_entry(&directory, path.clone())?);
        let storage = StorageDirectory::from_file(directory.try_clone()?)?;
        for name in profile.allowed_files() {
            let mut path = path.clone();
            path.push((*name).into());
            entries.extend(file_entry(&storage, name, path, &mut total)?);
        }
        if directory_entry(&directory, path)?
            != *entries
                .iter()
                .rev()
                .find(|entry| entry.hash.is_none())
                .ok_or_else(failed)?
        {
            return Err(failed());
        }
    }
    if directory_entry(&root, vec![])? != *entries.first().ok_or_else(failed)?
        || entries.len() > 128
    {
        return Err(failed());
    }
    Ok(entries)
}
