//! Host-authorized provider administration, separate from inference authority.
//! Credentials are write-only here; custody stays in the shared backend broker.

use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};

use crate::{ProviderConfig, protocol::ModelError};

/// Non-secret diagnosis of a provider's credential custody.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStatus {
    Missing,
    Configured,
    MigrationRequired,
}

/// Authoritative backend settings and their compare-and-swap revision.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProviderSnapshot {
    pub config: ProviderConfig,
    pub revision: String,
}

/// Administration returns data rather than borrowing UI callbacks. Browser
/// transports preserve this contract using the platform's Send-safe fetch API.
pub type AdminFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ModelError>> + Send + 'a>>;

/// An explicitly granted capability, not part of an ordinary model session.
/// Hosts bind the authenticated user before providing this interface. Every
/// operation remains subject to backend authorization and durable-write checks.
/// Dropping a mutation abandons its response, not its durable commit. Refresh
/// configuration/status before deciding whether an interrupted write needs retry.
pub trait ProviderAdmin: Send + Sync {
    fn get_config(&self) -> AdminFuture<'_, ProviderSnapshot>;
    fn set_config(
        &self,
        expected_revision: String,
        config: ProviderConfig,
    ) -> AdminFuture<'_, ProviderSnapshot>;
    fn credential_status(&self, profile_id: String) -> AdminFuture<'_, CredentialStatus>;
    fn set_api_key(&self, profile_id: String, api_key: String) -> AdminFuture<'_, ()>;
    fn remove_api_key(&self, profile_id: String) -> AdminFuture<'_, ()>;
    fn migrate_api_key(&self, profile_id: String, original_endpoint: String)
    -> AdminFuture<'_, ()>;
    fn remove_legacy_api_key(&self, profile_id: String) -> AdminFuture<'_, ()>;
}
