//! Protected continuation of an authenticated original-copy installation.
//! This record is not a user plan, app grant, credential token or launch permit.
use super::{Manifest, PreparedSetupReview, installed_retained::chain, invalid};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, io, path::Path, sync::Arc};
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
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
fn operation(value: &str) -> io::Result<()> {
    if uuid::Uuid::parse_str(value)
        .ok()
        .is_none_or(|id| id.to_string() != value)
    {
        return Err(invalid());
    }
    Ok(())
}
use crate::prepared_setup::installed_namespace::bytes;
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
pub struct AuthenticatedInstalledOriginalCopy {
    lease: crate::prepared_setup::installed_namespace::InstalledRecordLease,
    manifest: Manifest,
}
impl AuthenticatedInstalledOriginalCopy {
    pub(in crate::setup) fn open_installed(operation: &str) -> io::Result<Arc<Self>> {
        Self::open_at(Path::new("/"), operation)
    }
    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub fn open_fixture(system: &Path, id: &str) -> io::Result<Arc<Self>> {
        Self::open_at(system, id)
    }
    fn open_at(system: &Path, id: &str) -> io::Result<Arc<Self>> {
        operation(id)?;
        Self::open_directory(&system.join(LOCATION), id)
    }
    fn open_directory(path: &Path, id: &str) -> io::Result<Arc<Self>> {
        root()?;
        operation(id)?;
        let lease =
            crate::prepared_setup::installed_namespace::InstalledRecordLease::open(path, id)?;
        let parsed: Record = serde_json::from_slice(lease.bytes())?;
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
            lease,
            manifest: parsed.manifest,
        });
        result.revalidate()?;
        Ok(result)
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn revalidate(&self) -> io::Result<()> {
        root()?;
        self.lease.revalidate()
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
