//! Private root controller composition. No serialized caller can choose source
//! credentials or substitute an unguarded source stream for this owned session.
use super::{
    installed_retained_actor::InstalledRetainedLaunchInput,
    retained_source_relay::RetainedSourceRelay, retained_source_worker::RetainedSourceWorker,
};
use runtime_fs::selected_client::SelectedFileConnection;
use std::{
    ffi::OsStr,
    io,
    os::unix::net::UnixStream,
    sync::{Arc, atomic::AtomicBool},
};
/// Existing root jobs retain this owner through every source/target operation.
/// There is deliberately no public UI action or full-folder capability yet.
pub(super) struct RetainedSourceSession {
    relay: Option<RetainedSourceRelay>,
    worker: RetainedSourceWorker,
}
impl RetainedSourceSession {
    pub(super) fn serve(
        input: Arc<InstalledRetainedLaunchInput>,
        name: &OsStr,
        target: UnixStream,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        // Exact kernel peer is checked before opening the original selected leaf.
        super::retained_source_relay::target_peer(&target, &input)?;
        input.revalidate(&cancel)?;
        let (mut worker, source) =
            RetainedSourceWorker::start(Arc::clone(&input), name, Arc::clone(&cancel))?;
        let source = match SelectedFileConnection::connected(source) {
            Ok(source) => source,
            Err(error) => {
                worker.retire()?;
                return Err(error);
            }
        };
        match RetainedSourceRelay::serve(target, source, input, cancel) {
            Ok(relay) => Ok(Self {
                relay: Some(relay),
                worker,
            }),
            Err(error) => {
                worker.retire()?;
                Err(error)
            }
        }
    }
    /// Off UI: stop new calls, finish all admitted callbacks, disconnect source,
    /// and then observe the actual private namespace stopped. Both are attempted.
    pub(super) fn retire(&mut self) -> io::Result<()> {
        let relay = self
            .relay
            .take()
            .ok_or_else(|| io::Error::other("Retained session already retired"))?
            .retire();
        let worker = self.worker.retire();
        relay.and(worker)
    }
}
impl Drop for RetainedSourceSession {
    fn drop(&mut self) {
        if self.relay.is_some() {
            let _ = self.retire();
        }
    }
}
