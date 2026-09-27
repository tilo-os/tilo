//! Native `MoQ` adapters for Tilo voice and live presence: the tilo-voice/1
//! object profile and, behind `relay`, `moq-lite` relays and clients over
//! WebTransport (see [`carrier`] for the session layout).

pub mod voice;

#[cfg(feature = "relay")]
pub mod carrier;

#[cfg(feature = "relay")]
mod mirror;

#[cfg(feature = "relay")]
pub mod relay;

#[cfg(feature = "relay")]
pub mod live;

#[cfg(feature = "relay")]
pub mod client;

/// `at + age`, or `at` when the clock cannot represent the sum: an
/// immediately expired deadline fails closed instead of panicking.
#[cfg(feature = "relay")]
pub(crate) fn later(at: std::time::Instant, age: std::time::Duration) -> std::time::Instant {
    at.checked_add(age).unwrap_or(at)
}

/// Aborts its task on drop, so no pump outlives the state that owns it.
#[cfg(feature = "relay")]
pub(crate) struct Task(Option<tokio::task::JoinHandle<()>>);

#[cfg(feature = "relay")]
impl Task {
    pub(crate) fn spawn(future: impl std::future::Future<Output = ()> + Send + 'static) -> Self {
        Self(Some(tokio::spawn(future)))
    }
    /// Release without aborting: used by a task that is finishing itself.
    pub(crate) fn detach(mut self) {
        self.0.take();
    }
    pub(crate) fn is_finished(&self) -> bool {
        self.0
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }
}

#[cfg(feature = "relay")]
impl Drop for Task {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}
