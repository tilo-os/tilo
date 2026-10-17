//! True boot-medium ownership delegates only execution to the canonical runner.
use super::*;
use crate::setup::home_capture::{original_worker, runtime::TrustedCaptureService};
use launcher::owned_service::ServiceEndpoint;
pub(super) type OriginalWorkerRun = original_worker::OriginalWorkerRun<OfflineOutputTransaction>;
impl original_worker::OriginalWorkerOwner for OfflineOutputTransaction {
    fn operation(&self) -> &str {
        &self.binding.operation
    }
    fn read_claim_digest(&self) -> [u8; 32] {
        self.binding.read_claim_digest
    }
    fn cancellation(&self) -> &ServiceCancellation {
        &self.cancellation
    }
    fn record_namespace(&self, endpoint: &ServiceEndpoint) -> io::Result<()> {
        self.record_namespace(&endpoint.custody()?)
    }
}
impl OfflineOutputTransaction {
    pub(super) fn launch_request(
        self: &Arc<Self>,
        admission: CaptureAdmission,
        request: super::super::OriginalReaderRequest,
        service: TrustedCaptureService,
    ) -> io::Result<OriginalWorkerRun> {
        original_worker::launch(self.clone(), admission, request, service)
    }
}
