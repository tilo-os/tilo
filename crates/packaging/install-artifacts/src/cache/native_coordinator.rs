#[cfg(target_os = "linux")]
use super::linux as platform;
#[cfg(windows)]
use super::windows as platform;
#[path = "coordinator.rs"]
pub(crate) mod driver;

impl driver::Worker {
    pub(crate) fn spawn(
        consumer: impl FnMut(
            &crate::transport::SelectedManifest,
            &crate::download::Cancellation<'_>,
            &mut dyn crate::cache::HeldReadSeek,
            &mut dyn crate::cache::HeldReadSeek,
        ) -> Result<(), crate::error::Failure>
        + Send
        + 'static,
    ) -> Result<Self, crate::error::Failure> {
        let transport = crate::transport::InternalTransport::new()?;
        Self::spawn_runner(move |shared| {
            driver::worker_loop(
                shared,
                transport,
                consumer,
                crate::preference::PreferenceStore::live(),
            )
        })
    }
}
