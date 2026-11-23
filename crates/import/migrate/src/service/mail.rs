//! A mail account as a migration source. The person types the address and password once; the
//! password goes straight into the vault, beside an email account that waits for the review.
//! Bringing it over publishes that account to Mail, which signs in with the vaulted password
//! and syncs the account's mail itself.

use std::path::Path;

use accounts::email::{AccountConfig, AuthMethod, TYPE_ID};
use accounts::person::AccountList;
use accounts::{Account, AccountId};
use tilo_resource::Invoke;

use super::{ReviewGroup, ReviewItem, ServiceAddress, ServiceError};

/// The settings Mail signs in to the account `address` names with: its provider's servers, or
/// its own `imap.` and `smtp.` names for a domain no provider is known by, or the incoming server
/// the person named.
///
/// # Errors
/// The address is invalid, no password was typed, or the provider takes no password (Gmail and
/// Outlook: their sign-in is Mail's own).
pub fn settings(address: &ServiceAddress) -> Result<AccountConfig, ServiceError> {
    let email = address.login.trim().to_ascii_lowercase();
    let Some((_, domain)) = email.split_once('@') else {
        return Err(ServiceError::InvalidAddress);
    };
    if domain.is_empty() || address.secret.is_empty() || email.len() > 256 {
        return Err(ServiceError::InvalidAddress);
    }
    let mut config = AccountConfig {
        email: email.clone(),
        username: email,
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
    if config.auth == AuthMethod::OAuth {
        return Err(ServiceError::Refused);
    }
    Ok(config)
}

/// Keep the account waiting for review, its password in the vault, and say the edit that keeps
/// it in the workspace's record.
///
/// # Errors
/// The address is invalid, the account is already there, or the vault could not be written.
pub fn connect(
    config_root: &Path,
    record: &AccountList,
    address: &ServiceAddress,
) -> Result<(AccountId, Invoke<AccountList>), ServiceError> {
    let mut config = settings(address)?;
    config.import_pending = true;
    let email = config.email.clone();
    let id = AccountId::new(email.clone());
    if let Some(existing) = record.account(id.as_str())
        && existing.config.get("service_source").is_none()
    {
        return Err(ServiceError::AlreadyThere);
    }
    let mut account = Account::new(id.clone(), TYPE_ID);
    account.display_name = email;
    account.config = config
        .to_value()
        .map_err(|_| ServiceError::InvalidAddress)?;
    account.config["service_source"] = serde_json::Value::Bool(true);
    crate::vault::persist::<ServiceError>(
        config_root,
        &account,
        accounts::Secret::password(address.secret.to_string()),
    )?;
    Ok((id, AccountList::saving(&account)))
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
                facts: vec![host.to_owned()],
            }],
            chosen: true,
        }],
    )
}

/// Hand the reviewed account to Mail: it stops waiting and signs in with its vaulted password.
///
/// # Errors
/// The record no longer holds the account.
pub fn publish(record: &AccountList, account: &str) -> Result<Invoke<AccountList>, String> {
    let mut stored = record
        .account(account)
        .ok_or_else(|| "the mail account is gone".to_owned())?;
    stored.config["import_pending"] = serde_json::Value::Bool(false);
    Ok(AccountList::saving(&stored))
}
