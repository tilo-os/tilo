//! Destination metadata for a selected GNOME Online Account.
//!
//! A same-computer handoff re-reads the source after first boot. A GOA refresh
//! grant is only a credential for the service scopes it was actually granted:
//! it is not proof that Mail, Calendar, Contacts or Files will accept it on the
//! destination. This module therefore creates canonical pending accounts that
//! name exactly the selected services, without copying source custody. An
//! account the destination already holds with its own usable credential is
//! left untouched, so a later Thunderbird import of the same mailbox cannot
//! replace its authentication.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;

use accounts::{Account, AccountId, AccountStore, SecretVault, thunderbird::PENDING_TYPE};
use serde::{Deserialize, Serialize};

use crate::account_sources::{Candidate, Inventory, Origin, Provider, SelectedAccount, Service};

use super::ConnectionError;

const CALDAV_TYPE: &str = "caldav";

/// What one selected source account became on the destination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountImportReceipt {
    /// The opaque discovery id; never an address, path or profile name.
    pub source_id: String,
    pub services: Vec<Service>,
    /// The account is usable only after an individual sign-in. Offline is not
    /// a failed import: the local selection and pending row remain.
    pub pending: bool,
}

/// Create or recognise canonical destination accounts for exactly `selected`.
///
/// No source vault, OAuth token or native keychain is opened here. The caller
/// has already re-discovered and revalidated `inventory` under the retained
/// source root. Existing destination accounts keep their own configuration and
/// credential; only missing service rows are added as pending metadata.
///
/// # Errors
/// Refuses a selection that no longer matches discovery, an unsafe identity, or
/// a canonical store that cannot be read or saved.
pub fn import_selected(
    target_config_root: &std::path::Path,
    inventory: &Inventory,
    selected: &[SelectedAccount],
) -> Result<Vec<AccountImportReceipt>, ConnectionError> {
    for wanted in selected {
        validate_selection(wanted)?;
        current_candidate(inventory, wanted)?;
    }
    ensure_private_root(target_config_root)?;
    let mut store = super::connect::load_store(target_config_root)?;
    let vault = SecretVault::open_read_only_in(target_config_root);
    let mut receipts = Vec::with_capacity(selected.len());
    let mut changed = false;
    for wanted in selected {
        let mut pending = false;
        for service in &wanted.services {
            let (touched, needs_sign_in) = match service {
                Service::Files | Service::Contacts => {
                    files_or_contacts(&mut store, &vault, wanted, *service)
                }
                Service::Mail => mail(&mut store, &vault, wanted)?,
                Service::Calendar => calendar(&mut store, &vault, wanted),
            };
            changed |= touched;
            pending |= needs_sign_in;
        }
        receipts.push(AccountImportReceipt {
            source_id: wanted.source_id.clone(),
            services: wanted.services.clone(),
            pending,
        });
    }
    if changed {
        store
            .save_in(target_config_root)
            .map_err(|_| ConnectionError::StorageUnavailable)?;
        let _ = std::fs::set_permissions(
            accounts::store_path_in(target_config_root),
            std::fs::Permissions::from_mode(0o600),
        );
    }
    Ok(receipts)
}

fn ensure_private_root(root: &std::path::Path) -> Result<(), ConnectionError> {
    use std::os::unix::fs::DirBuilderExt as _;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(root)
        .map_err(|_| ConnectionError::StorageUnavailable)
}

fn validate_selection(selected: &SelectedAccount) -> Result<(), ConnectionError> {
    if selected.source_id.is_empty()
        || selected.source_id.len() > 256
        || selected.identity.is_empty()
        || selected.identity.len() > 256
        || selected.identity.chars().any(char::is_control)
        || selected.label.len() > 256
        || selected.label.chars().any(char::is_control)
        || selected.services.is_empty()
        || selected.services.len() > 4
    {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let unique: BTreeSet<_> = selected.services.iter().collect();
    if unique.len() != selected.services.len()
        || !matches!(selected.provider, Provider::Google | Provider::Microsoft)
    {
        return Err(ConnectionError::InvalidConfiguration);
    }
    Ok(())
}

fn current_candidate<'a>(
    inventory: &'a Inventory,
    selected: &SelectedAccount,
) -> Result<&'a Candidate, ConnectionError> {
    inventory
        .candidates
        .iter()
        .find(|candidate| {
            candidate.origin == Origin::Gnome
                && candidate.id == selected.source_id
                && candidate.provider == selected.provider
                && candidate.identity.as_deref() == Some(selected.identity.as_str())
                && candidate.label == selected.label
                && selected
                    .services
                    .iter()
                    .all(|service| candidate.services.contains(service))
        })
        .ok_or(ConnectionError::AccountMissing)
}

fn has_credential(vault: &SecretVault, id: &AccountId) -> bool {
    vault.get(id).is_some_and(|secret| !secret.is_empty())
}

fn account_identity(account: &Account) -> Option<&str> {
    account
        .config
        .get("source_goa_identity")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            account
                .config
                .get("email")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            account
                .config
                .get("connected_identity")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            account
                .config
                .get("username")
                .and_then(serde_json::Value::as_str)
        })
}

fn same_identity(account: &Account, identity: &str) -> bool {
    account_identity(account).is_some_and(|value| value.eq_ignore_ascii_case(identity))
}

fn provider_type(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => accounts::cloud_types::GOOGLE_DRIVE,
        Provider::Microsoft => accounts::cloud_types::ONEDRIVE,
        _ => accounts::cloud_types::WEBDAV,
    }
}

fn files_or_contacts(
    store: &mut AccountStore,
    vault: &SecretVault,
    selected: &SelectedAccount,
    service: Service,
) -> (bool, bool) {
    if let Some(existing) = store.all().iter().find(|account| {
        account.owner.is_none()
            && matches!(
                account.type_id.as_str(),
                accounts::cloud_types::GOOGLE_DRIVE | accounts::cloud_types::ONEDRIVE
            )
            && same_identity(account, &selected.identity)
    }) {
        let usable_files = existing
            .config
            .get(accounts::cloud_types::MOUNT_CONFIG_KEY)
            .is_some_and(serde_json::Value::is_object)
            && has_credential(vault, &existing.id);
        let pending =
            existing.config.get("pending_reconnect") == Some(&serde_json::Value::Bool(true));
        return (
            false,
            !pending && (!matches!(service, Service::Files) || !usable_files),
        );
    }
    let id = AccountId::new(format!("migrated-goa-{}", selected.source_id));
    let mut account = Account::new(id, provider_type(selected.provider));
    account.display_name.clone_from(&selected.label);
    let mut config = serde_json::json!({
        "provider": provider_type(selected.provider),
        "source_goa_identity": selected.identity,
        "pending_reconnect": true,
        "use_files": selected.services.contains(&Service::Files),
        "use_contacts": selected.services.contains(&Service::Contacts),
    });
    if let Some(config) = config.as_object_mut() {
        config.retain(|_, value| value != &serde_json::Value::Bool(false));
    }
    account.config = config;
    store.upsert(account);
    (true, true)
}

fn mail(
    store: &mut AccountStore,
    vault: &SecretVault,
    selected: &SelectedAccount,
) -> Result<(bool, bool), ConnectionError> {
    if let Some(existing) = store.all().iter().find(|account| {
        account.owner.is_none()
            && matches!(account.type_id.as_str(), "email" | PENDING_TYPE)
            && (account.id.as_str().eq_ignore_ascii_case(&selected.identity)
                || same_identity(account, &selected.identity))
    }) {
        let usable = existing.type_id.as_str() == "email" && has_credential(vault, &existing.id);
        // Never rewrite a mailbox the person already has, including a pending
        // Thunderbird import with local messages and no credential yet.
        return Ok((false, !usable));
    }
    let issuer = match selected.provider {
        Provider::Google => account_custody::OAuthIssuer::Google,
        Provider::Microsoft => account_custody::OAuthIssuer::Microsoft,
        _ => return Err(ConnectionError::InvalidConfiguration),
    };
    let mut config = issuer
        .mail_config(&selected.identity)
        .ok_or(ConnectionError::InvalidConfiguration)?;
    config.display_name.clone_from(&selected.label);
    config.import_pending = true;
    config.password.clear();
    let id = AccountId::new(selected.identity.to_ascii_lowercase());
    if store.get(&id).is_some() {
        return Err(ConnectionError::AlreadyPresent);
    }
    let mut account = Account::new(id, PENDING_TYPE);
    account.display_name.clone_from(&selected.label);
    account.config = config
        .to_value()
        .map_err(|_| ConnectionError::InvalidConfiguration)?;
    if let Some(config) = account.config.as_object_mut() {
        config.insert(
            "source_goa_identity".into(),
            selected.identity.clone().into(),
        );
        config.insert("pending_reconnect".into(), true.into());
    }
    store.upsert(account);
    Ok((true, true))
}

fn calendar(
    store: &mut AccountStore,
    vault: &SecretVault,
    selected: &SelectedAccount,
) -> (bool, bool) {
    if let Some(existing) = store.all().iter().find(|account| {
        account.owner.is_none()
            && account.type_id.as_str() == CALDAV_TYPE
            && same_identity(account, &selected.identity)
    }) {
        let usable = has_credential(vault, &existing.id)
            && existing.config.get("use_calendar") == Some(&serde_json::Value::Bool(true));
        return (false, !usable);
    }
    let id = AccountId::new(format!("migrated-goa-calendar-{}", selected.source_id));
    let mut account = Account::new(id, CALDAV_TYPE);
    account.display_name.clone_from(&selected.label);
    let server_url = match selected.provider {
        Provider::Google => format!(
            "https://apidata.googleusercontent.com/caldav/v2/{}/user",
            selected.identity
        ),
        _ => String::new(),
    };
    account.config = serde_json::json!({
        "display_name": selected.label,
        "server_url": server_url,
        "username": selected.identity,
        "source_goa_identity": selected.identity,
        "use_calendar": true,
        "pending_reconnect": true,
        "calendar_provider": match selected.provider {
            Provider::Google => "google",
            Provider::Microsoft => "microsoft",
            _ => "",
        },
    });
    store.upsert(account);
    (true, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_sources::SourceRoots;

    fn goa(account: &str, identity: &str, services: &str) -> String {
        format!(
            "[Account {account}]\nProvider=google\nIdentity={identity}\nPresentationIdentity={identity}\nFilesEnabled={files}\nMailEnabled={mail}\nCalendarEnabled={calendar}\nContactsEnabled=false\n",
            files = services.contains("files"),
            mail = services.contains("mail"),
            calendar = services.contains("calendar"),
        )
    }

    fn inventory(text: &str) -> Inventory {
        let root = tempfile::tempdir().unwrap();
        let goa = root.path().join("goa-1.0");
        std::fs::create_dir_all(&goa).unwrap();
        std::fs::write(goa.join("accounts.conf"), text).unwrap();
        crate::account_sources::discover(&SourceRoots {
            home: root.path().into(),
            config: root.path().into(),
        })
    }

    #[test]
    fn one_excluded_goa_account_creates_only_the_selected_pending_services() {
        let text = format!(
            "{}{}",
            goa("account_1", "person@example.test", "files mail calendar"),
            goa("account_2", "other@example.test", "mail")
        );
        let inventory = inventory(&text);
        let selected: Vec<_> = inventory
            .candidates
            .iter()
            .filter(|candidate| candidate.identity.as_deref() == Some("person@example.test"))
            .filter_map(SelectedAccount::from_candidate)
            .collect();
        assert_eq!(selected.len(), 1);
        let target = tempfile::tempdir().unwrap();
        let root = target.path().join(".config/tilo");
        let receipts = import_selected(&root, &inventory, &selected).unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(
            receipts[0].services,
            vec![Service::Files, Service::Mail, Service::Calendar]
        );
        assert!(receipts[0].pending);
        assert!(
            !serde_json::to_string(&receipts)
                .unwrap()
                .contains("person@example.test")
        );
        let store = AccountStore::load_in(&root);
        assert!(store.all().iter().any(|account| {
            account.type_id.as_str() == PENDING_TYPE
                && account.config["email"] == "person@example.test"
                && account.config["source_goa_identity"] == "person@example.test"
        }));
        assert!(store.all().iter().any(|account| {
            account.type_id.as_str() == CALDAV_TYPE
                && account.config["username"] == "person@example.test"
        }));
        assert!(store.all().iter().any(|account| {
            account.type_id.as_str() == accounts::cloud_types::GOOGLE_DRIVE
                && account.config["use_files"] == true
        }));
        assert!(
            store
                .all()
                .iter()
                .all(|account| !same_identity(account, "other@example.test")),
            "an excluded GOA account must not create a destination row"
        );
    }

    #[test]
    fn an_existing_destination_mailbox_keeps_its_own_authentication() {
        let text = goa("account_1", "person@example.test", "mail");
        let inventory = inventory(&text);
        let selected: Vec<_> = inventory
            .candidates
            .iter()
            .filter_map(SelectedAccount::from_candidate)
            .collect();
        let target = tempfile::tempdir().unwrap();
        let root = target.path().join(".config/tilo");
        let mut store = AccountStore::new();
        let mut existing = Account::new(
            AccountId::new("person@example.test"),
            accounts::thunderbird::PENDING_TYPE,
        );
        existing.config = serde_json::json!({
            "display_name": "Personal",
            "email": "person@example.test",
            "import_pending": true,
            "source_provenance": {"mail_client": "thunderbird"},
        });
        store.upsert(existing);
        store.save_in(&root).unwrap();
        let receipts = import_selected(&root, &inventory, &selected).unwrap();
        assert!(receipts[0].pending);
        let after = AccountStore::load_in(&root);
        assert_eq!(
            after.len(),
            1,
            "the existing mailbox is reused, not replaced"
        );
        assert_eq!(
            after
                .get(&AccountId::new("person@example.test"))
                .unwrap()
                .config["source_provenance"]["mail_client"],
            "thunderbird"
        );
    }
}
