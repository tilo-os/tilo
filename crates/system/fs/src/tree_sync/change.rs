//! A change of a workspace's tree made where the tree is, as an ordinary local change of the
//! tree this device syncs: the files a put brings, a file manager's verbs, and the reads that
//! answer what the tree holds.
//!
//! What a caller changes is made in the [`Namespace`] this device syncs, journaled before this
//! answers, and handed to the workspace by the same [`TreeSync`] as every other change — the
//! chunks the workspace does not hold first, the tree's own record that names them last. What a
//! caller reads is read from the tree as the workspace has it now.

use std::time::Duration;

use tilo_lock::MutexRecover as _;

use super::TreeSync;
pub use crate::namespace::{FileToPut, PlacedFile};

/// How long a change waits for the workspace's tree to be read here.
const CHANGE_OPEN_WITHIN: Duration = Duration::from_secs(30);
/// How long a change waits for the workspace to take it.
const CHANGE_SENT_WITHIN: Duration = Duration::from_secs(30);
/// How long a read waits for the sync's next pass over the tree.
const READ_WITHIN: Duration = Duration::from_secs(5);

impl TreeSync {
    /// Change this tree as `acting` where one is named: `change` makes the change in the tree,
    /// which is kept on disk before this answers, and the sync hands it to the workspace in the
    /// order the tree may go out. It answers what `change` answered once the workspace's records
    /// carry it.
    ///
    /// The tree the workspace holds is read first: a change made before that would land on a
    /// tree founded here beside it, and the workspace's own would read as replaced.
    ///
    /// # Errors
    /// The workspace's tree did not come here, what `change` refuses, a change that could not be
    /// kept on disk, or a workspace that did not take it in time.
    pub async fn change<R>(
        &self,
        acting: Option<tilo_actions::ActionContext>,
        change: impl FnOnce(&mut crate::namespace::Namespace) -> Result<R, String>,
    ) -> Result<R, String> {
        self.wait_open(CHANGE_OPEN_WITHIN).await?;
        let changed = self.change_now(acting, change)?;
        self.wake.notify_waiters();
        self.wait_sent(CHANGE_SENT_WITHIN).await?;
        Ok(changed)
    }

    /// What `read` answers of this tree, as the workspace has it now: every record the tree lists
    /// is asked of the workspace again, and the tree is read once that pass ended (or
    /// [`READ_WITHIN`] went by).
    pub async fn read<R>(&self, read: impl FnOnce(&mut crate::namespace::Namespace) -> R) -> R {
        let _ = tokio::time::timeout(READ_WITHIN, async {
            loop {
                let before = self.passes.load(std::sync::atomic::Ordering::Relaxed);
                self.refresh
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.wake.notify_waiters();
                while self.passes.load(std::sync::atomic::Ordering::Relaxed) == before {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                // The pass that ended took the request: a reader that set it again meanwhile is
                // served by the next one.
                if !self.refresh.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
            }
        })
        .await;
        let mut namespace = self.namespace.lock_recover();
        read(&mut namespace)
    }

    /// [`Self::change`] as it is made: the change in the tree this device holds, kept on disk
    /// before this answers. The guard is taken and given back here, never held across an await.
    fn change_now<R>(
        &self,
        acting: Option<tilo_actions::ActionContext>,
        change: impl FnOnce(&mut crate::namespace::Namespace) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut namespace = self.namespace.lock_recover();
        let _acting = crate::namespace::acting_as(acting);
        let changed = change(&mut namespace)?;
        self.kept
            .journal(&mut namespace)
            .map_err(|error| format!("the change is not kept on this computer: {error}"))?;
        drop(namespace);
        Ok(changed)
    }

    /// Wait until the workspace's tree is read here, at most `within`.
    async fn wait_open(&self, within: Duration) -> Result<(), String> {
        let started = std::time::Instant::now();
        while !self.status().opened {
            if started.elapsed() >= within {
                return Err(format!(
                    "the workspace's tree has not been read here within {}s",
                    within.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }
}
