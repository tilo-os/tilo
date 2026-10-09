//! Signed inverse records for maintained developer cohort selections.
//! These records are private user-store bookkeeping, never publisher trust.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Error, InstalledPackage, PackageStore};

const MAX_RECORD_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub before: Option<InstalledPackage>,
    pub intent: Option<SelectionStamp>,
    #[serde(default)]
    pub committed: Option<SelectionStamp>,
}

impl Record {
    fn owns(&self, row: &InstalledPackage) -> bool {
        [&self.intent, &self.committed]
            .into_iter()
            .flatten()
            .any(|stamp| row.package_id_hex == stamp.package_id_hex && row.source == stamp.source)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SelectionStamp {
    id: String,
    package_id_hex: String,
    source: String,
}

fn source(ticket: &str) -> String {
    format!("developer-cohort:{ticket}")
}

fn directory(store: &PackageStore, ticket: &str) -> Result<PathBuf, Error> {
    if ticket.is_empty()
        || ticket.len() > 128
        || !ticket
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(Error::Policy("invalid cohort retention ticket".into()));
    }
    Ok(store.layout().root().join("cohorts").join(ticket))
}

fn paths(store: &PackageStore, ticket: &str, id: &str) -> Result<(PathBuf, PathBuf), Error> {
    let directory = directory(store, ticket)?;
    let name = tilo_app_package::BlobHash::of(id.as_bytes()).to_hex();
    Ok((
        directory.join(format!("{name}.json")),
        directory.join(format!("{name}.tapp")),
    ))
}

pub(super) fn read(store: &PackageStore, ticket: &str, id: &str) -> Result<Record, Error> {
    let (record, _) = paths(store, ticket, id)?;
    let mut bytes = Vec::new();
    std::fs::File::open(record)?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(Error::Policy(
            "cohort retention record exceeds its bound".into(),
        ));
    }
    let record: Record = serde_json::from_slice(&bytes)
        .map_err(|_| Error::Policy("invalid cohort retention record".into()))?;
    if record.before.as_ref().is_some_and(|row| row.id != id)
        || [&record.intent, &record.committed]
            .into_iter()
            .flatten()
            .any(|row| row.id != id || row.source != source(ticket))
    {
        return Err(Error::Policy(
            "cohort retention record has a different selection".into(),
        ));
    }
    Ok(record)
}

fn save(path: &Path, record: &Record) -> Result<(), Error> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Policy("invalid retention path".into()))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, record)
        .map_err(|_| Error::Policy("could not encode cohort retention record".into()))?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| Error::Io(error.error))?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub(super) fn prepare(ticket: &str, ids: &[String]) -> Result<(), Error> {
    let store = super::store()?;
    let directory = directory(&store, ticket)?;
    std::fs::create_dir_all(&directory)?;
    for id in ids {
        let (record_path, archive_path) = paths(&store, ticket, id)?;
        if record_path.exists() {
            let record = read(&store, ticket, id)?;
            if store
                .registry()
                .map_err(super::store_error)?
                .find(id)
                .is_some_and(|row| record.owns(row))
            {
                continue;
            }
        }
        let archive = tempfile::NamedTempFile::new_in(&directory)?;
        let before = match store.export_current(id, archive) {
            Ok((row, archive)) => {
                archive.as_file().sync_all()?;
                archive
                    .persist(&archive_path)
                    .map_err(|error| Error::Io(error.error))?;
                Some(row)
            }
            Err(package_store::StoreError::NotInstalled(_)) => None,
            Err(error) => return Err(super::store_error(error)),
        };
        if let Some(row) = &before {
            super::cohort_runtime::retain(&row.runtime)?;
        }
        if let Some(row) = super::layers(id)?.running() {
            super::cohort_runtime::retain(&row.runtime)?;
        }
        save(
            &record_path,
            &Record {
                before,
                intent: None,
                committed: None,
            },
        )?;
        println!("cohort-retained: {id}");
    }
    Ok(())
}

pub(super) fn select(
    store: &PackageStore,
    ticket: &str,
    checked: &super::Checked,
    candidate: &super::Candidate,
    admission: impl FnOnce(&tilo_app_package::PackageMeta) -> Result<package_store::Admission, String>
    + Send,
) -> Result<package_store::InstallSelection, Error> {
    let id = checked.id();
    super::cohort_runtime::retain(checked.runtime_ref().unwrap_or_default())?;
    let mut record = read(store, ticket, id)?;
    let current = store
        .registry()
        .map_err(super::store_error)?
        .find(id)
        .cloned();
    let already_selected = current.as_ref().is_some_and(|row| record.owns(row));
    if already_selected && let Some(row) = &current {
        // This is the observed canonical selection, not an uncommitted intent.
        // Retain it before attempting a replacement that may refuse or crash.
        record.committed = Some(SelectionStamp {
            id: row.id.clone(),
            package_id_hex: row.package_id_hex.clone(),
            source: row.source.clone(),
        });
    }
    let expected = if already_selected {
        current.as_ref()
    } else {
        record.before.as_ref()
    };
    let source = source(ticket);
    // Record the exact intent durably before the normal registry commit. If
    // installation refuses, its root/source will not match and rollback leaves it alone.
    let intent = SelectionStamp {
        id: id.into(),
        package_id_hex: checked.meta.id.to_hex(),
        source: source.clone(),
    };
    let expected = expected.cloned();
    record.intent = Some(intent);
    let (path, _) = paths(store, ticket, id)?;
    save(&path, &record)?;
    store
        .install_if_current(
            candidate.reader()?,
            expected.as_ref(),
            |meta| {
                let mut admitted = admission(meta)?;
                admitted.source = source;
                Ok(admitted)
            },
            None,
        )
        .map_err(super::store_error)
}

pub(super) fn restore(
    ticket: &str,
    ids: &[String],
    trust: &super::TrustFlags,
) -> Result<(), Error> {
    let store = super::store()?;
    for id in ids {
        let record = read(&store, ticket, id)?;
        let current = store
            .registry()
            .map_err(super::store_error)?
            .find(id)
            .cloned();
        let owned = current.as_ref().is_some_and(|row| record.owns(row));
        let changed = if owned {
            let current = current
                .as_ref()
                .ok_or_else(|| Error::Policy("cohort selection disappeared".into()))?;
            if let Some(before) = &record.before {
                let (_, archive) = paths(&store, ticket, id)?;
                restore_previous(&store, current, before, &archive, trust)?
            } else {
                let removed = store
                    .remove_if_current(current)
                    .map_err(super::store_error)?;
                if let Some(removed) = &removed
                    && let Some(row) = &removed.package
                    && !row.cli_commands.is_empty()
                {
                    super::shim::remove_shims(&super::bin_root()?, &row.cli_commands)
                        .map_err(|error| Error::Store(error.to_string()))?;
                }
                removed.is_some()
            }
        } else {
            false
        };
        println!(
            "cohort-restoration: {}",
            serde_json::json!({"id": id, "changed": changed})
        );
    }
    Ok(())
}

fn restore_previous(
    store: &PackageStore,
    current: &InstalledPackage,
    before: &InstalledPackage,
    archive: &Path,
    trust: &super::TrustFlags,
) -> Result<bool, Error> {
    let mut trust = trust.clone();
    if trust
        .require_package_root
        .as_ref()
        .is_some_and(|root| root != &before.package_id_hex)
        || trust
            .require_publisher
            .as_ref()
            .is_some_and(|publisher| !publisher.eq_ignore_ascii_case(&before.publisher_key_hex))
    {
        return Err(Error::Policy(
            "the retained package differs from the required identity".into(),
        ));
    }
    trust.require_package_root = Some(before.package_id_hex.clone());
    trust.require_publisher = Some(before.publisher_key_hex.clone());
    let candidate = super::Candidate::File(archive.to_path_buf());
    let checked = super::check(&candidate, &trust)?;
    super::fits_os(&checked)?;
    super::satisfy_runtime(&checked, &super::RuntimeFlags::default())?;
    let restored = store
        .install_if_current(
            candidate.reader()?,
            Some(current),
            super::checked_admission(&checked, current.pinned, before.held, before.source.clone()),
            None,
        )
        .map_err(super::store_error)?;
    match restored {
        package_store::InstallSelection::Installed(installed) => {
            super::replace_cli_shims(installed.replaced.as_ref(), &checked.manifest, checked.id())?;
            Ok(true)
        }
        package_store::InstallSelection::Preserved(_) => Ok(false),
    }
}
