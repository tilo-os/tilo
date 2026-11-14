//! Who waits for whom while workspaces are created.

use std::collections::HashMap;
use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tokio::sync::Mutex as AsyncMutex;

/// How many workspaces are made at once: each writes and syncs its files on
/// the runtime's threads, and the rest of the host's traffic needs some free.
const CREATING_AT_ONCE: usize = 4;

/// Who waits for whom while workspaces are created: one identity at a time,
/// and [`CREATING_AT_ONCE`] workspaces in all.
pub(super) struct Creating {
    identities: std::sync::Mutex<HashMap<String, std::sync::Weak<AsyncMutex<()>>>>,
    turns: Arc<tokio::sync::Semaphore>,
}

impl Default for Creating {
    fn default() -> Self {
        Self {
            identities: std::sync::Mutex::default(),
            turns: Arc::new(tokio::sync::Semaphore::new(CREATING_AT_ONCE)),
        }
    }
}

impl Creating {
    /// Held while the workspace `identity` is created.
    pub(super) async fn one(&self, identity: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let held = {
            let mut identities = self.identities.lock_recover();
            identities.retain(|_, held| held.strong_count() > 0);
            if let Some(held) = identities.get(identity).and_then(std::sync::Weak::upgrade) {
                held
            } else {
                let held = Arc::new(AsyncMutex::new(()));
                identities.insert(identity.to_owned(), Arc::downgrade(&held));
                held
            }
        };
        held.lock_owned().await
    }

    /// A turn to create one workspace.
    pub(super) async fn turn(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.turns).acquire_owned().await.ok()
    }
}
