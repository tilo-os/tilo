//! Password-free continuation of an already authenticated root installation.
//! This record is not a user plan, app grant, credential token or launch permit.
use super::{Manifest, PreparedSetupReview, invalid};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, io, path::Path, sync::Arc};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
const LIMIT: u64 = 132 * 1024;
const LOCATION: &str = "var/lib/tilo/retained-sources";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    purpose: String,
    manifest: Manifest,
}
use crate::prepared_setup::installed_namespace::bytes;
pub(super) use crate::prepared_setup::installed_namespace::{chain, public_certificate_chain};
fn root() -> io::Result<()> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
fn operation(value: &str) -> io::Result<()> {
    crate::prepared_setup::installed_namespace::operation(value)
}
fn validate(record: &Record) -> io::Result<()> {
    record.manifest.validate()?;
    let home = record.manifest.retained_home.as_ref().ok_or_else(invalid)?;
    let caller = record
        .manifest
        .retained_caller
        .as_ref()
        .ok_or_else(invalid)?;
    if record.schema != 1 || record.purpose != "installed-retained-source" {
        return Err(invalid());
    }
    operation(&home.as_plan().operation_id)?;
    caller.validate_binding(
        home.as_plan(),
        &record.manifest.setup_binding,
        &record.manifest.identity_public_key,
    )
}

/// Root-only native custody. Only authenticated installation can publish it;
/// the constructor is private and no deserialized Record is an admission.
pub struct AuthenticatedInstalledRetained {
    lease: crate::prepared_setup::installed_namespace::InstalledRecordLease,
    manifest: Manifest,
}
impl AuthenticatedInstalledRetained {
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
        operation(id)?;
        let lease =
            crate::prepared_setup::installed_namespace::InstalledRecordLease::open(path, id)?;
        let parsed: Record = serde_json::from_slice(lease.bytes())?;
        validate(&parsed)?;
        if parsed
            .manifest
            .retained_home
            .as_ref()
            .ok_or_else(invalid)?
            .as_plan()
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
        self.lease.revalidate()
    }
}
pub(super) fn publish_namespace(setup: &PreparedSetupReview, namespace: &Path) -> io::Result<()> {
    setup.require_completed_export()?;
    if setup.manifest.retained_caller.is_none() {
        return Ok(());
    }
    root()?;
    setup.validate()?;
    let record = Record {
        schema: 1,
        purpose: "installed-retained-source".into(),
        manifest: setup.manifest.clone(),
    };
    validate(&record)?;
    let bytes = serde_json::to_vec(&record)?;
    if bytes.len() as u64 > LIMIT {
        return Err(invalid());
    }
    let id = &record
        .manifest
        .retained_home
        .as_ref()
        .ok_or_else(invalid)?
        .as_plan()
        .operation_id;
    // --system-root is the identity namespace itself (/var/lib/tilo),
    // not the OS root. No parent traversal or guessed mount root is used.
    let location = namespace.join("retained-sources");
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
    AuthenticatedInstalledRetained::open_directory(&location, id)?.revalidate()
}
