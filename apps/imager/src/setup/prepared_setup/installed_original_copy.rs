//! Protected continuation of an authenticated original-copy installation.
//! This record is not a user plan, app grant, credential token or launch permit.
use super::{Manifest, PreparedSetupReview, installed_retained::chain, invalid};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
const LIMIT: u64 = 132 * 1024;
const LOCATION: &str = "var/lib/tilo/original-copy-sources";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    purpose: String,
    manifest: Manifest,
}
fn root() -> io::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
fn operation(value: &str) -> io::Result<()> {
    if uuid::Uuid::parse_str(value)
        .ok()
        .filter(|id| id.to_string() == value)
        .is_none()
    {
        return Err(invalid());
    }
    Ok(())
}
fn controlled(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(invalid());
    }
    StorageDirectory::from_file(file.try_clone()?)?.validate_namespace_control()
}
fn stamp(file: &File) -> io::Result<(u64, u64, u64, i64, i64)> {
    let m = file.metadata()?;
    if !m.is_file()
        || m.uid() != 0
        || m.mode() & 0o7777 != 0o600
        || m.nlink() != 1
        || m.len() > LIMIT
    {
        return Err(invalid());
    }
    Ok((m.dev(), m.ino(), m.len(), m.ctime(), m.ctime_nsec()))
}
fn bytes(file: &File) -> io::Result<Vec<u8>> {
    // A clone shares file position; independent positioned IO avoids mutation
    // of an admission's retained handle during concurrent supervision checks.
    use std::os::unix::fs::FileExt;
    let before = stamp(file)?;
    let mut bytes = vec![0; usize::try_from(before.2).map_err(io::Error::other)?];
    file.read_exact_at(&mut bytes, 0)?;
    if stamp(file)? != before {
        return Err(invalid());
    }
    Ok(bytes)
}
fn validate(record: &Record) -> io::Result<()> {
    record.manifest.validate()?;
    let declaration = record
        .manifest
        .installed_original_copy_declaration()?
        .ok_or_else(invalid)?;
    declaration.validate()?;
    if record.schema != 1
        || record.purpose != "installed-original-copy-source"
        || declaration.setup_binding() != record.manifest.setup_binding
        || declaration.identity_public_key() != record.manifest.identity_public_key
    {
        return Err(invalid());
    }
    operation(&declaration.source().operation_id)
}

/// Root-only native custody. Only authenticated installation can publish it;
/// the constructor is private and no deserialized Record is an admission.
pub(in crate::setup) struct AuthenticatedInstalledOriginalCopy {
    path: PathBuf,
    chain: Vec<File>,
    name: String,
    record: File,
    bytes: Vec<u8>,
    manifest: Manifest,
}
impl AuthenticatedInstalledOriginalCopy {
    pub(in crate::setup) fn open_installed(operation: &str) -> io::Result<Arc<Self>> {
        Self::open_at(Path::new("/"), operation)
    }
    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub(in crate::setup) fn open_fixture(system: &Path, id: &str) -> io::Result<Arc<Self>> {
        Self::open_at(system, id)
    }
    fn open_at(system: &Path, id: &str) -> io::Result<Arc<Self>> {
        operation(id)?;
        Self::open_directory(&system.join(LOCATION), id)
    }
    fn open_directory(path: &Path, id: &str) -> io::Result<Arc<Self>> {
        root()?;
        operation(id)?;
        let path = path.to_owned();
        let chain = chain(&path, false)?;
        let storage = StorageDirectory::from_file(chain.last().ok_or_else(invalid)?.try_clone()?)?;
        let name = format!("{id}.json");
        let record = storage.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
        let bytes = bytes(&record)?;
        let parsed: Record = serde_json::from_slice(&bytes)?;
        validate(&parsed)?;
        if parsed
            .manifest
            .installed_original_copy_declaration()?
            .ok_or_else(invalid)?
            .source()
            .operation_id
            != id
        {
            return Err(invalid());
        }
        let result = Arc::new(Self {
            path,
            chain,
            name,
            record,
            bytes,
            manifest: parsed.manifest,
        });
        result.revalidate()?;
        Ok(result)
    }
    pub(in crate::setup) fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub(in crate::setup) fn revalidate(&self) -> io::Result<()> {
        root()?;
        let current = chain(&self.path, false)?;
        if current.len() != self.chain.len() {
            return Err(invalid());
        }
        for (actual, held) in current.iter().zip(&self.chain) {
            controlled(held)?;
            let (a, h) = (actual.metadata()?, held.metadata()?);
            if (a.dev(), a.ino()) != (h.dev(), h.ino()) {
                return Err(invalid());
            }
        }
        let storage =
            StorageDirectory::from_file(current.last().ok_or_else(invalid)?.try_clone()?)?;
        let named = storage.open_read(OsStr::new(&self.name), Privacy::OwnerOnly)?;
        if stamp(&named)? != stamp(&self.record)?
            || bytes(&named)? != self.bytes
            || bytes(&self.record)? != self.bytes
        {
            return Err(invalid());
        }
        Ok(())
    }
}
pub(super) fn publish_namespace(setup: &PreparedSetupReview, namespace: &Path) -> io::Result<()> {
    setup.require_completed_export()?;
    if setup
        .manifest
        .installed_original_copy_declaration()?
        .is_none()
    {
        return Ok(());
    }
    root()?;
    setup.validate()?;
    let record = Record {
        schema: 1,
        purpose: "installed-original-copy-source".into(),
        manifest: setup.manifest.clone(),
    };
    validate(&record)?;
    let bytes = serde_json::to_vec(&record)?;
    if bytes.len() as u64 > LIMIT {
        return Err(invalid());
    }
    let id = &record
        .manifest
        .installed_original_copy_declaration()?
        .ok_or_else(invalid)?
        .source()
        .operation_id;
    // --system-root is the identity namespace itself (/var/lib/tilo),
    // not the OS root. No parent traversal or guessed mount root is used.
    let location = namespace.join("original-copy-sources");
    let held = chain(&location, true)?;
    let storage = StorageDirectory::from_file(held.last().ok_or_else(invalid)?.try_clone()?)?;
    let name = format!("{id}.json");
    match storage.open_read(OsStr::new(&name), Privacy::OwnerOnly) {
        Ok(existing) => {
            if self::bytes(&existing)? != bytes {
                return Err(invalid());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            storage
                .publish(
                    OsStr::new(&name),
                    &bytes,
                    PublishMode::IfAbsent,
                    Privacy::OwnerOnly,
                )
                .map_err(io::Error::other)?;
        }
        Err(error) => return Err(error),
    }
    setup.validate()?;
    AuthenticatedInstalledOriginalCopy::open_directory(&location, id)?.revalidate()
}
