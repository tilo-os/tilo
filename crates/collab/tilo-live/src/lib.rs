//! Native `MoQ` adapters for Tilo calls: the tilo-voice/1
//! and tilo-video/1 object profiles and, behind `relay`, `moq-lite` relays and clients over
//! WebTransport (see [`carrier`] for the session layout).

pub mod scope;
pub mod video;
pub mod voice;

#[cfg(feature = "tls")]
pub mod tls;

#[cfg(feature = "relay")]
pub mod carrier;

#[cfg(feature = "relay")]
pub mod live;

#[cfg(feature = "relay")]
mod mirror;

#[cfg(feature = "relay")]
pub mod relay;

#[cfg(feature = "relay")]
mod authority;

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
    pub(crate) fn is_finished(&self) -> bool {
        self.0
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }
    /// Finish owned work without detaching it if this wait is cancelled.
    pub(crate) async fn wait(&mut self) -> Result<(), tokio::task::JoinError> {
        let result = if let Some(task) = self.0.as_mut() {
            task.await
        } else {
            Ok(())
        };
        self.0.take();
        result
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
