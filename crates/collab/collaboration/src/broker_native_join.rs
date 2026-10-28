//! The wait for a native window's join, made on a thread of its own, so the
//! window's thread never waits for it.

use super::{CollaborationManager, native_launch_refusal};
use std::sync::{Arc, mpsc};
use tilo_lock::MutexRecover as _;

/// The wait for a joining manager's primary route
/// ([`CollaborationManager::join_native`]), made on a thread of its own: a
/// window whose record is not on this device yet builds once the route is
/// joined, from the room's committed history. Its outcome is an event
/// ([`Self::when_done`]); no clock decides it. The window's thread shows the
/// window's loading state meanwhile and builds the window once it is told
/// ([`Self::finish`]).
pub struct NativeJoin {
    outcome: mpsc::Receiver<Result<CollaborationManager, crate::NativeLaunchError>>,
    news: Arc<std::sync::Mutex<JoinNews>>,
}

enum JoinNews {
    /// Still joining; whom to tell when it is done.
    Joining(Option<Box<dyn FnOnce() + Send>>),
    Done,
}

impl std::fmt::Debug for NativeJoin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeJoin")
            .field("done", &self.is_done())
            .finish_non_exhaustive()
    }
}

impl NativeJoin {
    /// Wait for `manager`'s join off this thread. The manager keeps the wake
    /// scope it was made in: the app that takes it is the one its worker
    /// wakes.
    ///
    /// # Errors
    ///
    /// [`crate::NativeLaunchError::Operation`] where the thread cannot start.
    pub fn start(manager: CollaborationManager) -> Result<Self, crate::NativeLaunchError> {
        /// Tells whoever waits once the join is over, however its thread
        /// ended.
        struct Tell(Arc<std::sync::Mutex<JoinNews>>);
        impl Drop for Tell {
            fn drop(&mut self) {
                let waiting = std::mem::replace(&mut *self.0.lock_recover(), JoinNews::Done);
                if let JoinNews::Joining(Some(told)) = waiting {
                    told();
                }
            }
        }
        let (sender, outcome) = mpsc::sync_channel(1);
        let news = Arc::new(std::sync::Mutex::new(JoinNews::Joining(None)));
        let tell = Tell(Arc::clone(&news));
        std::thread::Builder::new()
            .name("ui-collab-join".into())
            .spawn(move || {
                let _tell = tell;
                let _ = sender.send(
                    manager
                        .finish_join()
                        .map_err(|error| native_launch_refusal(&error)),
                );
            })
            .map_err(|error| {
                crate::NativeLaunchError::Operation(format!(
                    "the collaboration join could not start: {error}"
                ))
            })?;
        Ok(Self { outcome, news })
    }

    /// Whether the join is over: [`Self::finish`] answers at once.
    pub fn is_done(&self) -> bool {
        matches!(*self.news.lock_recover(), JoinNews::Done)
    }

    /// Call `told` once the join is over, on the join's thread, or at once
    /// when it already is. Replaces an earlier `told`.
    pub fn when_done(&self, told: impl FnOnce() + Send + 'static) {
        let mut news = self.news.lock_recover();
        match &mut *news {
            JoinNews::Joining(waiting) => *waiting = Some(Box::new(told)),
            JoinNews::Done => {
                drop(news);
                told();
            }
        }
    }

    /// The window's manager, joined, or why it could not join. Waits for
    /// the join's own outcome, never a clock; a window's thread calls it
    /// once told ([`Self::when_done`]).
    ///
    /// # Errors
    ///
    /// As [`CollaborationManager::for_native_launch`].
    pub fn finish(self) -> Result<CollaborationManager, crate::NativeLaunchError> {
        match self.outcome.recv() {
            Ok(joined) => joined,
            Err(mpsc::RecvError) => Err(crate::NativeLaunchError::Operation(
                "the collaboration join ended without an outcome".into(),
            )),
        }
    }
}
