use accounts::{CredentialOwner, SecretId, SecretItem, SecretStore};
use std::sync::Arc;
use ui::collaboration::document_content::ContentLink;
use ui::framework::{ResourceOpener, WorkspaceResource};
use ui::resource::ResourceSaveStatus;
use ui::tilo_resource::Id;
use zeroize::Zeroizing;

use passwords_model::WorkspaceVaultEntries;

pub(crate) use ui::tilo_actions::policy::roles::WORKSPACE_VAULT_DOCUMENT;

pub(crate) struct WorkspaceVault {
    opener: ResourceOpener<WorkspaceVaultEntries>,
}

impl WorkspaceVault {
    pub(crate) fn new(opener: ResourceOpener<WorkspaceVaultEntries>) -> Self {
        Self { opener }
    }

    fn record(&self) -> Result<WorkspaceResource<WorkspaceVaultEntries>, String> {
        self.opener
            .open(WORKSPACE_VAULT_DOCUMENT)
            .ok_or_else(|| "the workspace vault is unavailable".into())
    }

    fn link(record: &WorkspaceResource<WorkspaceVaultEntries>) -> Result<Arc<ContentLink>, String> {
        if !record.may_read() {
            return Err("you may not read this vault".into());
        }
        let route = record
            .record()
            .route()
            .ok_or("the vault has no workspace route")?;
        Ok(ContentLink::of(&route))
    }

    pub(crate) fn revision(&self) -> u64 {
        let Ok(record) = self.record() else {
            return 0;
        };
        let epoch = Self::link(&record)
            .ok()
            .and_then(|link| link.private_field_epoch())
            .map_or(0, |epoch| epoch.wrapping_add(1));
        record.revision().rotate_left(16) ^ epoch
    }

    pub(crate) fn items(&self) -> Result<Vec<SecretItem>, String> {
        let record = self.record()?;
        let link = Self::link(&record)?;
        if link.private_field_epoch().is_none() {
            return Err("the workspace vault keys are unavailable".into());
        }
        let state = record
            .try_get()
            .ok_or("the workspace vault is unavailable")?;
        state
            .entries()
            .map(|(id, sealed)| Self::open_item(&link, id, &sealed.0))
            .collect()
    }

    pub(crate) fn check_read(&self) -> Result<(), String> {
        let record = self.record()?;
        Self::link(&record)?
            .private_field_epoch()
            .map(drop)
            .ok_or_else(|| "the workspace vault keys are unavailable".into())
    }

    pub(crate) fn item(&self, id: &SecretId) -> Result<SecretItem, String> {
        let record = self.record()?;
        let link = Self::link(&record)?;
        let state = record
            .try_get()
            .ok_or("the workspace vault is unavailable")?;
        let id = Id::from_raw(&id.0);
        let sealed = state.entry(&id).ok_or("item no longer exists")?;
        Self::open_item(&link, &id, &sealed.0)
    }

    fn open_item(link: &ContentLink, id: &Id, sealed: &str) -> Result<SecretItem, String> {
        let payload = link.open_private_field(id.as_str(), sealed)?;
        let item: SecretItem =
            serde_json::from_slice(&payload).map_err(|_| "invalid encrypted vault item")?;
        if item.id.0 != id.as_str() {
            return Err("vault item identifier mismatch".into());
        }
        Ok(item)
    }

    pub(crate) fn put(&self, item: &SecretItem) -> Result<(), String> {
        let record = self.record()?;
        let link = Self::link(&record)?;
        let sealed = Self::seal_item(&link, item)?;
        record
            .invoke(&WorkspaceVaultEntries::KEEP_SEALED.with((item.id.0.clone(), sealed)))
            .map_err(|error| error.to_string())
    }

    fn seal_item(link: &ContentLink, item: &SecretItem) -> Result<String, String> {
        if !item.may_share_with_workspace() {
            return Err("a device credential must stay with its device".into());
        }
        let payload = Zeroizing::new(serde_json::to_vec(item).map_err(|_| "invalid vault item")?);
        link.seal_private_field(&item.id.0, &payload)
    }

    /// Whether the document already holds `id`, or once held it and deleted it: such an
    /// item is never imported again, as `import-sealed` refuses it too.
    fn already_here(record: &WorkspaceResource<WorkspaceVaultEntries>, id: &SecretId) -> bool {
        let id = Id::from_raw(&id.0);
        record
            .try_get()
            .is_some_and(|state| state.removed.contains(&id) || state.entry(&id).is_some())
    }

    /// Seal into the document every item the device vault holds that may be shared
    /// ([`crate::travels_to_workspace`]), and say which items are now the workspace's: each
    /// id, and how many this pass sealed. `None` where the device vault cannot be read on
    /// this thread now, leaving the conversion for the next refresh.
    ///
    /// Nothing leaves the device here: the caller removes the ids once the document has
    /// kept them.
    ///
    /// # Errors
    ///
    /// Why the device vault cannot be read or the document cannot be written.
    pub(crate) fn convert_device_vault(
        &self,
        owner: Option<&CredentialOwner>,
        local_device_vault: bool,
    ) -> Result<Option<(Vec<SecretId>, usize)>, String> {
        let record = self.record()?;
        if !record.may("import-sealed") {
            return Err("you may not import into this vault".into());
        }
        let link = Self::link(&record)?;
        if link.private_field_epoch().is_none() {
            return Err("the workspace vault keys are unavailable".into());
        }
        let items = if let Some(owner) = owner {
            if !owner.is_open() && ui::storage::waiting::forbidden() {
                return Ok(None);
            }
            owner
                .read(device_items)
                .map_err(|error| format!("could not read vault: {error:?}"))??
        } else {
            if !local_device_vault || ui::storage::waiting::forbidden() {
                return Ok(None);
            }
            device_items(&SecretStore::open())?
        };
        let mut here = Vec::new();
        let mut sealed = 0_usize;
        for item in items
            .iter()
            .filter(|item| crate::travels_to_workspace(item))
        {
            if !Self::already_here(&record, &item.id) {
                let ciphertext = Self::seal_item(&link, item)?;
                let action =
                    WorkspaceVaultEntries::IMPORT_SEALED.with((item.id.0.clone(), ciphertext));
                record.invoke(&action).map_err(|error| error.to_string())?;
                sealed = sealed.saturating_add(1);
            }
            here.push(item.id.clone());
        }
        Ok(Some((here, sealed)))
    }

    pub(crate) fn remove(&self, id: &SecretId) -> Result<bool, String> {
        let record = self.record()?;
        if Self::link(&record)?.private_field_epoch().is_none() {
            return Err("the workspace vault keys are unavailable".into());
        }
        let id = Id::from_raw(&id.0);
        let state = record
            .try_get()
            .ok_or("the workspace vault is unavailable")?;
        if state.entry(&id).is_none() {
            return Ok(false);
        }
        record
            .invoke(&WorkspaceVaultEntries::REMOVE_SEALED.with(id.as_str().to_owned()))
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    pub(crate) fn flush(&self) -> Result<bool, String> {
        match self
            .record()?
            .record()
            .save_status()
            .map_err(|error| error.to_string())?
        {
            ResourceSaveStatus::Saved => Ok(true),
            ResourceSaveStatus::Saving => Ok(false),
            ResourceSaveStatus::Failed(error) => Err(error),
            ResourceSaveStatus::MemoryOnly => {
                Err("the workspace vault has no durable store".into())
            }
        }
    }
}

/// Take `ids` out of the device vault, once the document holds them.
///
/// # Errors
///
/// Why the device vault refuses the removal.
pub(crate) fn remove_device_items(
    owner: Option<&CredentialOwner>,
    local_device_vault: bool,
    ids: &[SecretId],
) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    if let Some(owner) = owner {
        return owner
            .edit(|store| remove_all(store, ids))
            .map_err(|error| format!("could not remove converted logins: {error:?}"))?
            .map_err(|error| format!("could not remove converted logins: {error:?}"));
    }
    if !local_device_vault {
        return Ok(());
    }
    remove_all(&mut SecretStore::open(), ids)
        .map_err(|error| format!("could not remove converted logins: {error:?}"))
}

/// The items a device vault holds, or why it cannot be read.
fn device_items(store: &SecretStore) -> Result<Vec<SecretItem>, String> {
    match store.recovery_error() {
        Some(error) => Err(error.to_owned()),
        None => Ok(store.list()),
    }
}

/// Take every `id` out of `store`.
fn remove_all(store: &mut SecretStore, ids: &[SecretId]) -> Result<(), accounts::BrokerError> {
    for id in ids {
        store.remove(id)?;
    }
    Ok(())
}
