//! Service credentials in the accounts vault, with exact origin and purpose binding.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::secrets::{BrokerError, canonical_origin};
use crate::{AccountId, Secret, SecretVault, VaultBackend};

const PREFIX: &str = "tilo-service-credential:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialRef(String);

/// Authorization derived by a trusted caller from its execution context.
/// All three fields must match the encrypted binding to read, replace or remove.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceAuthorization {
    service: String,
    purpose: String,
    origin: String,
}

impl ServiceAuthorization {
    /// # Errors
    /// Rejects empty, oversized or invalid service, purpose and origin values.
    pub fn new(service: &str, purpose: &str, origin: &str) -> Result<Self, BrokerError> {
        if service.is_empty()
            || service.len() > 255
            || purpose.is_empty()
            || purpose.len() > 160
            || service.chars().any(char::is_control)
            || purpose.chars().any(char::is_control)
        {
            return Err(BrokerError::InvalidPurpose);
        }
        Ok(Self {
            service: service.into(),
            purpose: purpose.into(),
            origin: canonical_origin(origin)?,
        })
    }

    /// Stable namespace reference. A reference alone never authorizes access.
    pub fn credential_ref(&self, id: &str) -> Result<CredentialRef, BrokerError> {
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(BrokerError::InvalidAppId);
        }
        let identity = serde_json::to_vec(&(&self.service, &self.purpose, id))
            .map_err(|_| BrokerError::CorruptItem)?;
        Ok(CredentialRef(hex::encode(identity)))
    }
}

/// The service a model provider's API key is bound to: the model service
/// that sends it, on a Tilo computer and on a server alike.
pub const MODEL_SERVICE: &str = "model-gateway";

/// What a model provider's API key is kept for.
const PROVIDER_API_KEY: &str = "provider-api-key";

/// Where a model provider's API key is kept: under the provider's id
/// (`profile_id`), bound to the model service, to being that provider's key,
/// and to the origin of its `endpoint`. A Tilo computer and a server keep a
/// person's keys in this one way, and a key never follows an endpoint edited
/// to point at another origin: it is not found there, and has to be entered
/// again.
///
/// # Errors
/// The endpoint names no origin, or the id is no provider id.
pub fn provider_api_key(
    profile_id: &str,
    endpoint: &str,
) -> Result<(CredentialRef, ServiceAuthorization), BrokerError> {
    let authorization = ServiceAuthorization::new(MODEL_SERVICE, PROVIDER_API_KEY, endpoint)?;
    let reference = authorization.credential_ref(profile_id)?;
    Ok((reference, authorization))
}

#[derive(Serialize, Deserialize)]
struct Entry {
    authorization: ServiceAuthorization,
    value: String,
}

/// Trusted account credentials over the shared sealed vault.
///
/// Adopts existing account-ID password and OAuth entries in place: keeping the
/// canonical key preserves Settings edits, removal and token revocation. Unlike
/// origin-bound service credentials, this API is for account owners and system
/// refreshers, not untrusted apps requesting interactive password disclosure.
/// No credential is copied into account metadata or a second inventory.
pub struct AccountCredentials {
    vault: SecretVault,
}

impl AccountCredentials {
    /// Open signed-in user ownership, refusing an unreadable or unavailable vault.
    pub fn open() -> Result<Self, String> {
        Self::open_person(None)
    }

    /// Open the credentials kept under the config root `root`; see
    /// [`SecretVault::open_person`] for what each root's ownership is.
    pub fn open_in(root: &Path) -> Result<Self, String> {
        Self::open_person(Some(root))
    }

    /// Open the person's ownership under `config_root` (their own when `None`):
    /// the keyring where it answers, otherwise an owner-only key file.
    pub fn open_person(config_root: Option<&Path>) -> Result<Self, String> {
        Self::from_vault(SecretVault::open_person(config_root))
    }

    fn from_vault(vault: SecretVault) -> Result<Self, String> {
        if let Some(error) = vault.recovery_error() {
            return Err(format!(
                "vault recovery required; restore access and reopen: {error}"
            ));
        }
        Ok(Self { vault })
    }

    pub fn get(&self, id: &AccountId) -> Option<Secret> {
        self.vault.get(id)
    }

    /// Stage a complete credential replacement; preserve token fields when editing passwords.
    pub fn put(&mut self, id: &AccountId, secret: &Secret) {
        self.vault.put(id, secret);
    }

    pub fn remove(&mut self, id: &AccountId) -> bool {
        self.vault.remove(id)
    }

    /// Stage `secret` as the secret of `account`, typed for the servers its
    /// settings name now ([`SecretVault::put_for`]).
    pub fn put_for(&mut self, account: &crate::Account, secret: &Secret) {
        self.vault.put_for(account, secret);
    }

    /// Stage `secret` under `id` as typed just now: bound where its account
    /// is first joined ([`SecretVault::put_anew`]).
    pub fn put_anew(&mut self, id: &AccountId, secret: &Secret) {
        self.vault.put_anew(id, secret);
    }

    /// The secret of `account`, while its settings name only servers the
    /// secret was typed for ([`SecretVault::get_for`]).
    #[must_use]
    pub fn get_for(&self, account: &crate::Account) -> Option<Secret> {
        self.vault.get_for(account)
    }

    /// The servers the secret under `id` was typed for and when it was
    /// bound to them ([`SecretVault::typed_for`]).
    #[must_use]
    pub fn typed_for(&self, id: &AccountId) -> Option<crate::typed_for::TypedFor> {
        self.vault.typed_for(id)
    }

    /// Bind each secret of `accounts` that has no servers on it yet to the
    /// servers its account names now ([`SecretVault::bind_first`]), and
    /// save where one was bound.
    ///
    /// # Errors
    /// The vault could not be saved.
    pub fn bind_first<'a>(
        &mut self,
        accounts: impl IntoIterator<Item = &'a crate::Account>,
    ) -> Result<(), String> {
        let mut bound = false;
        for account in accounts {
            bound |= self.vault.bind_first(account);
        }
        if bound { self.vault.try_save() } else { Ok(()) }
    }

    /// Commit staged edits using the vault's locked, conflict-checked atomic write.
    pub fn try_save(&self) -> Result<(), String> {
        self.vault.try_save()
    }
}

/// Shared service/provider facade; no second vault or plaintext credential index.
pub struct ServiceCredentials {
    root: Option<PathBuf>,
}

impl ServiceCredentials {
    pub fn open() -> Self {
        Self { root: None }
    }
    pub fn open_in(root: &Path) -> Self {
        Self {
            root: Some(root.into()),
        }
    }

    fn vault(&self) -> Result<SecretVault, BrokerError> {
        let vault = SecretVault::open_person(self.root.as_deref());
        if let Some(error) = vault.recovery_error() {
            return Err(BrokerError::Storage(error.into()));
        }
        Ok(vault)
    }

    pub fn backend(&self) -> Result<VaultBackend, BrokerError> {
        Ok(self.vault()?.backend())
    }

    fn key(reference: &CredentialRef) -> AccountId {
        AccountId::new(format!("{PREFIX}{}", reference.0))
    }

    fn entry(
        vault: &SecretVault,
        reference: &CredentialRef,
        authorization: &ServiceAuthorization,
    ) -> Result<Option<Entry>, BrokerError> {
        let decoded = hex::decode(&reference.0).map_err(|_| BrokerError::Denied)?;
        let (service, purpose, _id): (String, String, String) =
            serde_json::from_slice(&decoded).map_err(|_| BrokerError::Denied)?;
        if service != authorization.service || purpose != authorization.purpose {
            return Err(BrokerError::Denied);
        }
        let Some(secret) = vault.get(&Self::key(reference)) else {
            return Ok(None);
        };
        let entry: Entry =
            serde_json::from_str(secret.token.as_deref().ok_or(BrokerError::CorruptItem)?)
                .map_err(|_| BrokerError::CorruptItem)?;
        if entry.authorization != *authorization {
            return Err(BrokerError::Denied);
        }
        Ok(Some(entry))
    }

    /// # Errors
    /// Returns authorization, integrity or storage failures, never false absence.
    pub fn status(
        &self,
        reference: &CredentialRef,
        authorization: &ServiceAuthorization,
    ) -> Result<bool, BrokerError> {
        Ok(Self::entry(&self.vault()?, reference, authorization)?.is_some())
    }

    pub fn resolve(
        &self,
        reference: &CredentialRef,
        authorization: &ServiceAuthorization,
    ) -> Result<Option<Zeroizing<String>>, BrokerError> {
        Ok(Self::entry(&self.vault()?, reference, authorization)?
            .map(|entry| Zeroizing::new(entry.value)))
    }

    pub fn store(
        &self,
        reference: &CredentialRef,
        authorization: &ServiceAuthorization,
        value: &str,
    ) -> Result<(), BrokerError> {
        if value.trim().is_empty() || value.len() > 16 * 1024 {
            return Err(BrokerError::InvalidPurpose);
        }
        let mut vault = self.vault()?;
        Self::entry(&vault, reference, authorization)?;
        Self::put(&mut vault, reference, authorization, value)?;
        vault.try_save().map_err(BrokerError::Storage)
    }

    fn put(
        vault: &mut SecretVault,
        reference: &CredentialRef,
        authorization: &ServiceAuthorization,
        value: &str,
    ) -> Result<(), BrokerError> {
        let payload = Zeroizing::new(
            serde_json::to_string(&Entry {
                authorization: authorization.clone(),
                value: value.into(),
            })
            .map_err(|_| BrokerError::CorruptItem)?,
        );
        vault.put(
            &Self::key(reference),
            &Secret {
                password: String::new(),
                token: Some(payload.to_string()),
            },
        );
        Ok(())
    }

    pub fn remove(
        &self,
        reference: &CredentialRef,
        authorization: &ServiceAuthorization,
    ) -> Result<bool, BrokerError> {
        let mut vault = self.vault()?;
        if Self::entry(&vault, reference, authorization)?.is_none() {
            return Ok(false);
        }
        vault.remove(&Self::key(reference));
        vault.try_save().map_err(BrokerError::Storage)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The person's vault opened by its root is the person's vault opened
    /// from the environment: the same ownership, opened alike, and never read
    /// under a key file of its own.
    #[test]
    fn the_persons_root_opens_under_the_persons_ownership() {
        let Some(root) = crate::config_root() else {
            return;
        };
        let by_root =
            AccountCredentials::open_in(&root).map(|credentials| credentials.vault.backend());
        let session = AccountCredentials::open().map(|credentials| credentials.vault.backend());
        assert_eq!(by_root, session);
    }
}
