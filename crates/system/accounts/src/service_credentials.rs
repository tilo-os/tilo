//! Service credentials in the accounts vault, with exact origin and purpose binding.
//!
//! The trusted service boundary constructs authorization from authenticated context;
//! these types are not an authentication protocol or a substitute for user consent.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::secrets::{BrokerError, canonical_origin};
use crate::{AccountId, Secret, SecretCustody, SecretVault};

const PREFIX: &str = "tilo-service-credential:";

/// Opaque stable reference suitable for non-secret provider configuration.
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
    ///
    /// # Errors
    /// Rejects invalid profile identifiers.
    pub fn credential_ref(&self, id: &str) -> Result<CredentialRef, BrokerError> {
        if id.is_empty()
            || id.len() > 128
            || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err(BrokerError::InvalidAppId);
        }
        // Structured encoding avoids delimiter collisions across namespaces.
        let identity = serde_json::to_vec(&(&self.service, &self.purpose, id))
            .map_err(|_| BrokerError::CorruptItem)?;
        Ok(CredentialRef(hex::encode(identity)))
    }
}

#[derive(Serialize, Deserialize)]
struct Entry {
    authorization: ServiceAuthorization,
    value: String,
}

/// Trusted account custody over the shared sealed vault.
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
    /// Open signed-in user custody, refusing an unreadable or unavailable vault.
    ///
    /// # Errors
    /// Returns credential storage failures rather than an empty account view.
    pub fn open() -> Result<Self, String> {
        Self::from_vault(SecretVault::open())
    }

    /// Open explicit-root custody for recovery and isolated tests.
    ///
    /// # Errors
    /// Returns credential storage failures.
    pub fn open_in(root: &Path) -> Result<Self, String> {
        Self::from_vault(SecretVault::open_in(root))
    }

    fn from_vault(vault: SecretVault) -> Result<Self, String> {
        if let Some(error) = vault.recovery_error() {
            return Err(format!(
                "vault recovery required; restore access and reopen: {error}"
            ));
        }
        Ok(Self { vault })
    }

    /// Read the canonical account credential, including legacy password/OAuth entries.
    pub fn get(&self, id: &AccountId) -> Option<Secret> {
        self.vault.get(id)
    }

    /// Stage a complete credential replacement; preserve token fields when editing passwords.
    pub fn put(&mut self, id: &AccountId, secret: &Secret) {
        self.vault.put(id, secret);
    }

    /// Stage account credential removal.
    pub fn remove(&mut self, id: &AccountId) -> bool {
        self.vault.remove(id)
    }

    /// Commit staged edits using the vault's locked, conflict-checked atomic write.
    ///
    /// # Errors
    /// Returns custody, concurrent modification and durable write failures.
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
        let vault = self
            .root
            .as_deref()
            .map_or_else(SecretVault::open, SecretVault::open_in);
        if let Some(error) = vault.recovery_error() {
            return Err(BrokerError::Storage(error.into()));
        }
        Ok(vault)
    }

    /// # Errors
    /// Returns storage errors instead of guessing when custody is unavailable.
    pub fn custody(&self) -> Result<SecretCustody, BrokerError> {
        Ok(self.vault()?.custody())
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

    /// # Errors
    /// Returns authorization, integrity or storage failures.
    pub fn resolve(
        &self,
        reference: &CredentialRef,
        authorization: &ServiceAuthorization,
    ) -> Result<Option<Zeroizing<String>>, BrokerError> {
        Ok(Self::entry(&self.vault()?, reference, authorization)?
            .map(|entry| Zeroizing::new(entry.value)))
    }

    /// # Errors
    /// Returns invalid-value, authorization, integrity or durable write failures.
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

    /// # Errors
    /// Returns authorization, integrity or durable write failures.
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

    fn legacy_agent_key(profile_id: &str) -> Result<AccountId, BrokerError> {
        // Reuse the bound-reference validation before constructing an old vault key.
        ServiceAuthorization::new(
            "model-gateway",
            "provider-api-key",
            "https://unused.example",
        )?
        .credential_ref(profile_id)?;
        Ok(AccountId::new(format!("agent-provider-{profile_id}")))
    }

    /// Diagnose an unbound provider key without disclosing it or guessing its origin.
    ///
    /// # Errors
    /// Returns invalid identifiers or custody failures.
    pub fn legacy_agent_provider_status(&self, profile_id: &str) -> Result<bool, BrokerError> {
        let key = Self::legacy_agent_key(profile_id)?;
        Ok(self.vault()?.get(&key).is_some())
    }

    /// Remove only the explicitly selected unbound provider entry.
    /// Bound credentials are never affected, including after an endpoint edit.
    ///
    /// # Errors
    /// Returns invalid identifiers, custody or durable write failures.
    pub fn remove_legacy_agent_provider(&self, profile_id: &str) -> Result<bool, BrokerError> {
        let key = Self::legacy_agent_key(profile_id)?;
        let mut vault = self.vault()?;
        let removed = vault.remove(&key);
        if removed {
            vault.try_save().map_err(BrokerError::Storage)?;
        }
        Ok(removed)
    }

    /// Explicitly adopt a legacy `agent-provider-{profile_id}` entry. The trusted
    /// configuration owner must supply the original endpoint, not a newly edited
    /// endpoint: old entries carried no origin binding. Never guess that origin.
    /// Existing bound entries win, making retries idempotent.
    ///
    /// # Errors
    /// Returns invalid identifiers, origin mismatch, integrity or write failures.
    pub fn migrate_agent_provider(
        &self,
        profile_id: &str,
        authorization: &ServiceAuthorization,
        original_origin: &str,
    ) -> Result<CredentialRef, BrokerError> {
        let reference = authorization.credential_ref(profile_id)?;
        if authorization.origin != canonical_origin(original_origin)? {
            return Err(BrokerError::Denied);
        }
        let mut vault = self.vault()?;
        let legacy = Self::legacy_agent_key(profile_id)?;
        if Self::entry(&vault, &reference, authorization)?.is_some() {
            // A bound replacement wins, but explicit adoption must not leave
            // an obsolete unbound copy behind.
            if vault.remove(&legacy) {
                vault.try_save().map_err(BrokerError::Storage)?;
            }
            return Ok(reference);
        }
        let secret = vault.get(&legacy).ok_or(BrokerError::NotFound)?;
        if secret.password.is_empty() {
            return Err(BrokerError::CorruptItem);
        }
        Self::put(&mut vault, &reference, authorization, &secret.password)?;
        vault.remove(&legacy);
        vault.try_save().map_err(BrokerError::Storage)?;
        Ok(reference)
    }
}
