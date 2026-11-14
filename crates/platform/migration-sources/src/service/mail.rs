//! A mail account as a migration source. The person types the address and password once; the
//! password goes straight into the vault, beside an email account that waits for the review.
//! Bringing it over publishes that account to Mail, which signs in with the vaulted password
//! and syncs the account's mail itself.

use std::path::Path;

use accounts::email::{AccountConfig, AuthMethod, TYPE_ID};
use accounts::{Account, AccountId, AccountStore};

use super::{ReviewGroup, ReviewItem, ServiceAddress, ServiceError};

/// Keep the account waiting for review, its password in the vault.
pub fn connect(config_root: &Path, address: &ServiceAddress) -> Result<AccountId, ServiceError> {
    let email = address.login.trim().to_ascii_lowercase();
    let Some((_, domain)) = email.split_once('@') else {
        return Err(ServiceError::InvalidAddress);
    };
    if domain.is_empty() || address.secret.is_empty() || email.len() > 256 {
        return Err(ServiceError::InvalidAddress);
    }
    let mut config = AccountConfig {
        email: email.clone(),
        username: email.clone(),
        ..AccountConfig::default()
    };
    let known = config.autofill_from_email();
    let server = address.server.trim();
    if !server.is_empty() {
        if server.contains(['/', ' ', '@']) {
            return Err(ServiceError::InvalidAddress);
        }
        server.clone_into(&mut config.imap_host);
        if !known {
            config.smtp_host = server.replacen("imap.", "smtp.", 1);
        }
    }
    if config.imap_host.is_empty() {
        config.imap_host = format!("imap.{domain}");
        config.smtp_host = format!("smtp.{domain}");
    }
    if config.auth == AuthMethod::OAuth {
        // Gmail and Outlook refuse a password; their sign-in is Mail's own.
        return Err(ServiceError::Refused);
    }
    config.import_pending = true;
    let id = AccountId::new(email.clone());
    let store = AccountStore::load_in(config_root);
    if let Some(existing) = store.get(&id)
        && existing.config.get("service_source").is_none()
    {
        return Err(ServiceError::AlreadyThere);
    }
    let mut account = Account::new(id, TYPE_ID);
    account.display_name = email;
    account.config = config
        .to_value()
        .map_err(|_| ServiceError::InvalidAddress)?;
    account.config["service_source"] = serde_json::Value::Bool(true);
    crate::vault::persist(
        config_root,
        || Ok(AccountStore::load_in(config_root)),
        account,
        accounts::Secret::password(address.secret.to_string()),
    )
}

/// The account itself, as the one thing this source brings.
pub fn review(account: &Account) -> (String, Vec<ReviewGroup>) {
    let host = account
        .config
        .get("imap_host")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let title = account.display_name.clone();
    (
        title.clone(),
        vec![ReviewGroup {
            key: account.id.0.clone(),
            title: title.clone(),
            items: vec![ReviewItem {
                key: account.id.0.clone(),
                title,
                detail: host.to_owned(),
            }],
            chosen: true,
        }],
    )
}

/// Hand the reviewed account to Mail: it stops waiting and signs in with its vaulted password.
pub fn publish(config_root: &Path, account: &str) -> Result<(), String> {
    let mut store = AccountStore::load_in(config_root);
    let mut stored = store
        .get(&AccountId::new(account))
        .cloned()
        .ok_or_else(|| "the mail account is gone".to_owned())?;
    stored.config["import_pending"] = serde_json::Value::Bool(false);
    store.upsert(stored);
    store
        .save_in(config_root)
        .map_err(|error| error.to_string())
}
