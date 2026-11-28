//! Answers the session's loop asks for without waiting on them: the loop draws every frame and
//! takes every key, so what it asks another process (the device's daemon deciding a launch, a
//! store read, a system service's dump) runs on the session's runtime, and the answer is kept
//! under the id it was asked with until the woken loop takes it.

use std::future::Future;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

/// What wakes the session's loop once an answer is kept.
pub(crate) type Wake = Arc<dyn Fn() + Send + Sync>;

/// Answers asked for off the session's loop, by the id each was asked with, oldest first.
pub(crate) struct Pending<Id, T>(Arc<Mutex<Vec<(Id, T)>>>);

impl<Id, T> Default for Pending<Id, T> {
    fn default() -> Self {
        Self(Arc::default())
    }
}

impl<Id: Send + 'static, T: Send + 'static> Pending<Id, T> {
    /// Run `asking` on the session's runtime, keep what it answers under `id`, and call `wake`.
    ///
    /// # Errors
    /// The session has no runtime to run it on.
    pub(crate) fn ask(
        &self,
        id: Id,
        asking: impl Future<Output = T> + Send + 'static,
        wake: Wake,
    ) -> Result<(), String> {
        let runtime = tilo_rt::try_app().map_err(|error| error.to_string())?;
        let kept = Arc::clone(&self.0);
        runtime.spawn(async move {
            let answer = asking.await;
            kept.lock_recover().push((id, answer));
            wake();
        });
        Ok(())
    }

    /// The answers that came since the loop last took them, oldest first.
    pub(crate) fn take(&self) -> Vec<(Id, T)> {
        std::mem::take(&mut *self.0.lock_recover())
    }

    /// Whether an answer came that the loop has not taken yet.
    pub(crate) fn waiting(&self) -> bool {
        !self.0.lock_recover().is_empty()
    }
}
