//! Bounded metadata-only review of owner-local canonical connections.
use super::{Inventory, SourceRoots};
use crate::account_store::ConnectionError;
use accounts::{Account, AccountStore};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, serde::Serialize)]
pub struct TiloCandidate {
    pub source_id: String,
    pub account_id: String,
    pub label: String,
    pub type_id: String,
    /// Pins provider, owner, enabled state and configuration without projecting
    /// arbitrary configuration fields or reading the vault during discovery.
    pub metadata_sha256: [u8; 32],
}

pub fn fingerprint(account: &Account) -> Result<[u8; 32], ConnectionError> {
    let bytes = zeroize::Zeroizing::new(
        serde_json::to_vec(account).map_err(|_| ConnectionError::InvalidConfiguration)?,
    );
    Ok(Sha256::digest(&*bytes).into())
}

fn supported(account: &Account) -> bool {
    matches!(
        account.type_id.as_str(),
        "google-drive" | "onedrive" | "dropbox" | "icloud-drive" | "nextcloud" | "webdav"
    ) || (account.type_id.as_str() == "spotify"
        && matches!(
            account.id.as_str(),
            "music.spotify" | "music.spotify.streaming"
        ))
}

pub(super) fn discover(roots: &SourceRoots, inventory: &mut Inventory) {
    let root = roots.config.join("tilo");
    let result = (|| {
        let path = accounts::store_path_in(&root);
        let text = super::metadata(&roots.config, &path)
            .map_err(|_| ConnectionError::StorageUnavailable)?;
        let store: AccountStore = match text {
            Some(text) => {
                serde_json::from_str(&text).map_err(|_| ConnectionError::StorageUnavailable)?
            }
            None => AccountStore::new(),
        };
        if store.all().len() > 64 {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut candidates = Vec::new();
        for account in store.all() {
            if !ids.insert(account.id.as_str()) {
                return Err(ConnectionError::InvalidConfiguration);
            }
            if account.owner.is_some() || !account.enabled || !supported(account) {
                continue;
            }
            if account.id.as_str().is_empty()
                || account.id.as_str().len() > 256
                || account.id.as_str().chars().any(char::is_control)
            {
                return Err(ConnectionError::InvalidConfiguration);
            }
            let source_id = format!(
                "tilo-account-{}",
                hex::encode(Sha256::digest(account.id.as_str().as_bytes()))
            );
            candidates.push(TiloCandidate {
                source_id,
                account_id: account.id.as_str().into(),
                label: account
                    .display_name
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(256)
                    .collect(),
                type_id: account.type_id.as_str().into(),
                metadata_sha256: fingerprint(account)?,
            });
        }
        Ok(candidates)
    })();
    match result {
        Ok(candidates) => inventory.tilo_accounts = candidates,
        Err(_) => inventory.unreadable_sources.push("Tilo accounts"),
    }
}
