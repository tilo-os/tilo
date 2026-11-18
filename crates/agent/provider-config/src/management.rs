//! Host-authorized provider administration, separate from inference authority.
//! No provider key passes here: keys are kept where the person's workspace
//! is, and set there (`tilo.models`).

use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};

use crate::{ModelDescriptor, ProviderConfig, protocol::ModelError};

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
    /// The models the provider lists, asked as its sign-in or, for a local
    /// server, as itself: also the proof that the endpoint answers.
    /// [`crate::protocol::ErrorKind::Unsupported`] when it lists none.
    fn list_models(&self, profile_id: String) -> AdminFuture<'_, Vec<ModelDescriptor>>;
}
