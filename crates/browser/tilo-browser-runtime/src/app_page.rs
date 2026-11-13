//! The record a page showing one app opens.
//!
//! A page shows one application alone, on the record its link names, on the
//! workspace's own document of it, or on a new one. In a workspace whose host
//! sends a device a record only once it opens it, a record that is not here is
//! not a new one: the page waits for the workspace to send it, or to answer it
//! holds none of it, and starts one only after that answer. Opening it before
//! founded a fresh document, and its first publication overwrote or forked the
//! workspace's own.
//!
//! The store's side of that is [`RecordStore::ask_workspace_for_missing`];
//! this is the page's, so what it opens can be read here without a window.

use std::cell::RefCell;
use std::rc::Rc;

use ui::application::{ApplicationDefinition, LaunchTarget};
use ui::record::{RecordCheckout, RecordId, RecordOpenError, RecordOwners, RecordStore};

/// What a page showing one app opens now.
pub(crate) enum Opening {
    /// What it opens, here and now.
    Target(LaunchTarget),
    /// A new record, which the host names: the app founds it when its factory
    /// runs.
    New(RecordId),
    /// Nothing of it is here and the workspace has not answered: the page
    /// shows the app arriving, and opens it once the record is here or the
    /// workspace answered it holds none of it — then as a new record.
    Arriving,
}

/// Why a page showing one app cannot open what it opens.
#[derive(Debug)]
pub(crate) enum Unopened {
    /// The record id the page names is not one.
    InvalidRecord,
    /// The store refused it, saying why.
    Records(RecordOpenError),
}

/// Why a window's app was not built on the record it shows.
#[derive(Debug)]
pub(crate) enum Unbuilt {
    /// The record is still on its way from the workspace (a window saved on it, or one a shared
    /// desk names): the window shows it loading, and is built again once it is here. Not a
    /// failure, so the person never reads one.
    Arriving,
    /// What the window says instead.
    Failed(String),
}

impl Unbuilt {
    /// What preparing or building the app answered, as the window shows it.
    pub(crate) fn of(error: ui::launch::LaunchError) -> Self {
        match error {
            ui::launch::LaunchError::Record(RecordOpenError::Arriving) => Self::Arriving,
            ui::launch::LaunchError::Record(error) => Self::Failed(error.to_string()),
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<String> for Unbuilt {
    fn from(detail: String) -> Self {
        Self::Failed(detail)
    }
}

/// The store a page showing `definition` reads, opened once into `stores` so
/// what the workspace answered about a record stays known while the window it
/// opened is built. A page in a workspace whose host sends a device a record
/// only once it is opened asks it for what is not here.
///
/// # Errors
/// Rejects a package or workspace the records cannot be named in.
pub(crate) fn store(
    owners: &mut RecordOwners,
    definition: &ApplicationDefinition,
    workspace: &str,
    replicated: bool,
    stores: &Rc<RefCell<Vec<RecordStore>>>,
) -> Result<RecordStore, RecordOpenError> {
    if let Some(store) = stores.borrow().first() {
        return Ok(store.clone());
    }
    let store = owners.open(definition.app_id(), workspace)?;
    if replicated {
        store.ask_workspace_for_missing();
    }
    stores.borrow_mut().push(store.clone());
    Ok(store)
}

/// What a page showing `definition` opens in `records`: `record` is the id its
/// link names, else the instance's default record or the app's own document of
/// the workspace.
pub(crate) fn opening(
    definition: &ApplicationDefinition,
    records: &RecordStore,
    record: Option<&str>,
    new_document: bool,
    instance: u64,
) -> Result<Opening, Unopened> {
    // The instance's default record, named by the host so a reload opens the
    // same document, as a native launch does.
    let default = RecordCheckout::new(
        RecordId::derived("default", &RecordId::for_tab(instance)),
        definition.record_kind.clone(),
    );
    match record {
        // The workspace's own document of an app that keeps one: opened as a
        // launch naming nothing opens it, so a link to it works before anyone
        // wrote it (the first write founds it), as it does natively.
        Some(existing)
            if definition.keeps_workspace_record()
                && existing == RecordId::package_primary().as_str() =>
        {
            unsaved(definition, records)
        }
        Some(existing) => Ok(Opening::Target(LaunchTarget::Record {
            checkout: RecordCheckout::new(
                RecordId::parse(existing).ok_or(Unopened::InvalidRecord)?,
                definition.record_kind.clone(),
            ),
            reveal: None,
        })),
        None if new_document || definition.keeps_workspace_record() => unsaved(definition, records),
        None => match records.try_open_record(&default) {
            Ok(_) => Ok(Opening::Target(LaunchTarget::Record {
                checkout: default,
                reveal: None,
            })),
            Err(RecordOpenError::MissingRecord) => Ok(Opening::New(default.id)),
            Err(RecordOpenError::Arriving) => Ok(Opening::Arriving),
            Err(error) => Err(Unopened::Records(error)),
        },
    }
}

/// What a page naming no record opens where the application decides: the
/// workspace's own document of it while it is here, else a new one.
fn unsaved(definition: &ApplicationDefinition, records: &RecordStore) -> Result<Opening, Unopened> {
    match definition.unsaved_target(records) {
        Ok(target) => Ok(Opening::Target(target)),
        Err(RecordOpenError::Arriving) => Ok(Opening::Arriving),
        Err(error) => Err(Unopened::Records(error)),
    }
}

#[cfg(test)]
mod tests;
