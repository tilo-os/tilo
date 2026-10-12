//! Private host-owned account setup, separate from portable account metadata.
use crate::{AccountState, SendError, ServiceAccount};
use std::{future::Future, pin::Pin, sync::Arc};
use zeroize::Zeroizing;

/// An ephemeral linking presentation. No serialization, logs or shared record
/// represent its payload. The shared QR widget must render it as secret.
pub struct Pairing {
    pub account: ServiceAccount,
    pub attempt: String,
    pub generation: u64,
    payload: Zeroizing<Vec<u8>>,
}
impl Pairing {
    pub fn new(
        account: ServiceAccount,
        attempt: String,
        generation: u64,
        payload: Zeroizing<Vec<u8>>,
    ) -> Self {
        Self {
            account,
            attempt,
            generation,
            payload,
        }
    }
    pub fn qr_bytes(&self) -> &[u8] {
        &self.payload
    }
}
impl std::fmt::Debug for Pairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pairing(<private>)")
    }
}
/// Verified account identity; linking alone never grants a message route or
/// promises an installed, ready sender and receiver.
#[derive(Clone)]
pub struct LinkedAccount {
    pub account: ServiceAccount,
    pub generation: u64,
    pub scope: String,
}
pub type PairingFuture = Pin<Box<dyn Future<Output = Result<Pairing, SendError>> + Send + 'static>>;
pub type LinkingFuture =
    Pin<Box<dyn Future<Output = Result<LinkedAccount, SendError>> + Send + 'static>>;
pub trait SetupHost: Send + Sync {
    fn state(&self) -> AccountState;
    /// Time until private presentation must be rechecked on the owning UI loop.
    /// This schedules a frame; it never extends the provider's expiry.
    fn wake_after(&self) -> Option<std::time::Duration> {
        None
    }
    fn begin(&self) -> PairingFuture;
    fn finish(&self, attempt: String) -> LinkingFuture;
    fn cancel(&self);
}
/// Lent by the native host to one person, service/account and link generation.
#[derive(Clone)]
pub struct SetupClient {
    principal: String,
    account: ServiceAccount,
    generation: u64,
    host: Arc<dyn SetupHost>,
}
impl SetupClient {
    pub fn new(
        principal: String,
        account: ServiceAccount,
        generation: u64,
        host: Arc<dyn SetupHost>,
    ) -> Self {
        Self {
            principal,
            account,
            generation,
            host,
        }
    }
    pub fn principal(&self) -> &str {
        &self.principal
    }
    pub fn account(&self) -> &ServiceAccount {
        &self.account
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn state(&self) -> AccountState {
        self.host.state()
    }
    pub fn wake_after(&self) -> Option<std::time::Duration> {
        self.host.wake_after()
    }
    /// The app invokes this only through its sensitive setup action.
    /// # Errors
    /// Refuses invalid generation or another account's private presentation.
    pub async fn begin(&self) -> Result<Pairing, SendError> {
        if self.generation == 0 {
            return Err(SendError::InvalidRequest);
        }
        let private = self.host.begin().await?;
        if private.account != self.account
            || private.generation != self.generation
            || private.attempt.is_empty()
            || private.attempt.len() > 512
            || private
                .attempt
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
            || private.payload.is_empty()
            || private.payload.len() > 4096
        {
            self.host.cancel();
            return Err(SendError::Revoked);
        }
        Ok(private)
    }
    /// # Errors
    /// Refuses a mismatched provider account or generation after completion.
    pub async fn finish(&self, attempt: String) -> Result<LinkedAccount, SendError> {
        if attempt.is_empty()
            || attempt.len() > 512
            || attempt.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(SendError::InvalidRequest);
        }
        let linked = self.host.finish(attempt).await?;
        if linked.account != self.account
            || linked.generation != self.generation
            || linked.scope.is_empty()
            || linked.scope.len() > 512
        {
            self.host.cancel();
            return Err(SendError::Revoked);
        }
        Ok(linked)
    }
    pub fn cancel(&self) {
        self.host.cancel();
    }
}
