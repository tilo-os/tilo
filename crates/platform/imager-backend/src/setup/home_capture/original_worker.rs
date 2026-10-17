//! Same canonical signed provider, with original source/output admission retained.
//! Worker responses are untrusted until whole-stream verification and quiescence.
use super::OriginalReaderRequest;
#[cfg(test)]
use super::offline_output::OfflineOutputTransaction;
use launcher::owned_service::{
    CaptureAdmission, ServiceAuthority, ServiceCancellation, ServiceEndpoint,
};
use std::{
    io,
    path::Path,
    sync::{Arc, Mutex},
};
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
/// Installed and true-medium output owners retain distinct admission policies.
/// This interface only shares signed execution, bounded exchange and quiescence.
pub(in crate::setup::home_capture) trait OriginalWorkerOwner:
    ServiceAuthority + 'static
{
    fn operation(&self) -> &str;
    fn read_claim_digest(&self) -> [u8; 32];
    fn cancellation(&self) -> &ServiceCancellation;
    fn record_namespace(&self, endpoint: &ServiceEndpoint) -> io::Result<()>;
}
use crate::setup::home_capture::{
    runtime::{PACKAGE, TrustedCaptureService},
    worker,
};
use launcher::{
    owned_service::{self, ServiceLaunch, ServiceLeaseKey, SupervisedRun},
    run::{IdentitySource, LaunchMode, RunOptions},
};
use std::{
    os::unix::fs::PermissionsExt,
    thread::JoinHandle,
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

static UNKNOWN_RETIREMENT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
pub(in crate::setup::home_capture) fn retirement_pending() -> bool {
    UNKNOWN_RETIREMENT.load(std::sync::atomic::Ordering::Acquire)
}

/// Keeps working ancestry and cleanup evidence alive even when exchange fails.
/// No response alone represents verified ciphertext or completed installation.
pub(in crate::setup::home_capture) struct OriginalWorkerRun<O: OriginalWorkerOwner> {
    pub(in crate::setup::home_capture) supervised: SupervisedRun,
    pub(in crate::setup::home_capture) response: io::Result<worker::Response>,
    pub(in crate::setup::home_capture) key: ServiceLeaseKey,
    working: Option<tempfile::TempDir>,
    owner: Option<Arc<O>>,
    exchange: Option<JoinHandle<io::Result<worker::Response>>>,
    invalid_exchange: bool,
}
impl<O: OriginalWorkerOwner> Drop for OriginalWorkerRun<O> {
    fn drop(&mut self) {
        if self.supervised.shutdown.is_none()
            || self.exchange.as_ref().is_some_and(|job| !job.is_finished())
        {
            if let Some(working) = self.working.take() {
                // Unknown namespace custody cannot authorize deleting its
                // protected working ancestry. Keep it quarantined for recovery.
                UNKNOWN_RETIREMENT.store(true, std::sync::atomic::Ordering::Release);
                let _ = working.keep();
                if let Some(owner) = self.owner.take() {
                    std::mem::forget(owner);
                }
                if let Some(exchange) = self.exchange.take() {
                    std::mem::forget(exchange);
                }
            }
        }
    }
}
impl<O: OriginalWorkerOwner> OriginalWorkerRun<O> {
    pub(in crate::setup::home_capture) fn observe_exchange_stopped(&mut self) -> bool {
        let Some(job) = self.exchange.as_ref() else {
            return true;
        };
        if !job.is_finished() {
            return false;
        }
        let Some(job) = self.exchange.take() else {
            return false;
        };
        let response = job
            .join()
            .unwrap_or_else(|_| Err(refused("Original worker exchange panicked")));
        self.response = if self.invalid_exchange {
            Err(refused("Original worker exchange lock failed"))
        } else {
            response
        };
        true
    }
    pub(in crate::setup::home_capture) fn belongs_to(&self, transaction: &Arc<O>) -> bool {
        self.owner
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, transaction))
    }
    pub(in crate::setup::home_capture) fn working_path(&self) -> io::Result<&Path> {
        self.working
            .as_ref()
            .map(|working| working.path())
            .ok_or_else(|| refused("Original working custody was retired"))
    }
    pub(in crate::setup::home_capture) fn quiescent_response(
        &self,
    ) -> io::Result<&worker::Response> {
        let exit = self
            .supervised
            .result
            .as_ref()
            .map_err(|_| refused("Original worker did not complete quiescently"))?;
        if !exit.matches(&self.key, self.working_path()?, PACKAGE) {
            return Err(refused("Original worker quiescence differs"));
        }
        self.response
            .as_ref()
            .map_err(|_| refused("Original worker exchange failed"))
    }
}
pub(in crate::setup::home_capture) fn launch<O: OriginalWorkerOwner>(
    transaction: Arc<O>,
    admission: CaptureAdmission,
    request: OriginalReaderRequest,
    service: TrustedCaptureService,
) -> io::Result<OriginalWorkerRun<O>> {
    transaction.check()?;
    request.validate_protocol()?;
    if request.custody.is_some()
        || request.operation != transaction.operation()
        || admission.original_read_claim_digest() != Some(transaction.read_claim_digest())
    {
        return Err(refused("Original worker request authority differs"));
    }
    let bytes = Zeroizing::new(serde_json::to_vec(&request)?);
    if bytes.len() > 64 * 1024 {
        return Err(refused("Original worker request exceeds bounds"));
    }
    let mut exchange = worker::Exchange::new(&request.operation, &request.command);
    drop(request);
    let working = tempfile::Builder::new()
        .prefix("tilo-original-output-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/run")?;
    let key = ServiceLeaseKey::default();
    let admission =
        admission.with_installation(service.installed.clone(), &service.service_publisher)?;
    let jobs: Arc<Mutex<Option<JoinHandle<io::Result<worker::Response>>>>> =
        Arc::new(Mutex::new(None));
    let connected_jobs = jobs.clone();
    let connected_transaction = transaction.clone();
    let connected = Box::new(move |endpoint: owned_service::ServiceEndpoint| {
        connected_transaction.record_namespace(&endpoint)?;
        // Record is durable before a provider receives any password/command.
        connected_transaction.check()?;
        // Obtain the slot BEFORE spawning: a poisoned slot must never detach
        // an already running exchange thread from canonical shutdown/join.
        let mut job_slot = connected_jobs
            .lock()
            .map_err(|_| refused("Original worker exchange lock failed"))?;
        if job_slot.is_some() {
            return Err(refused("Original worker exchange already started"));
        }
        let job_transaction = connected_transaction.clone();
        let job = std::thread::Builder::new()
            .name("original-output-exchange".into())
            .spawn(move || {
                let result = (|| {
                    let deadline = Instant::now() + Duration::from_secs(30);
                    let mut channel = loop {
                        job_transaction.check()?;
                        match endpoint.connect() {
                            Ok(channel) => break channel,
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                                ) && Instant::now() < deadline =>
                            {
                                std::thread::sleep(Duration::from_millis(10))
                            }
                            Err(error) => return Err(error),
                        }
                    };
                    worker::write_frame(&mut channel, &bytes, 64 * 1024)?;
                    loop {
                        job_transaction.check()?;
                        let bytes = worker::read_frame(&mut channel, 16 * 1024)?;
                        let frame: worker::Frame = serde_json::from_slice(&bytes)?;
                        match frame {
                            worker::Frame::Progress {
                                operation,
                                export_operation,
                                progress,
                            } => {
                                exchange.progress(&operation, &export_operation, progress)?;
                            }
                            worker::Frame::Response {
                                operation,
                                export_operation,
                                response,
                            } => {
                                exchange.finish(&operation, export_operation.as_deref())?;
                                job_transaction.check()?;
                                return Ok(response);
                            }
                        }
                    }
                })();
                if result.is_err() {
                    job_transaction.cancellation().cancel();
                }
                result
            })?;
        *job_slot = Some(job);
        Ok(())
    });
    let supervised = owned_service::run_capture_connected_observed(
        ServiceLaunch {
            manifest: service.manifest.clone(),
            payload: service.payload.clone(),
            options: RunOptions {
                no_services: true,
                expected_package_hash: Some(service.package_hash.clone()),
                app_id: Some(PACKAGE.into()),
                data_home: Some(working.path().to_path_buf()),
                mode: LaunchMode::Service {
                    passthrough_argv: Vec::new(),
                },
                identity: IdentitySource::None,
                ..RunOptions::default()
            },
        },
        admission,
        transaction.clone(),
        transaction.cancellation().clone(),
        key.clone(),
        Some(connected),
    );
    // Construct the strong working/owner guard BEFORE every fallible exchange
    // collection. A missing shutdown witness cannot delete ancestry on error.
    let mut run = OriginalWorkerRun {
        supervised,
        response: Err(refused("Original worker provider was not admitted")),
        key,
        working: Some(working),
        owner: Some(transaction),
        exchange: None,
        invalid_exchange: false,
    };
    // Recovering a poisoned INTERNAL slot transfers only owned JoinHandle
    // custody. It does not authenticate a worker response or an output grant.
    let poisoned = match jobs.lock() {
        Ok(mut jobs) => {
            run.exchange = jobs.take();
            false
        }
        Err(poisoned) => {
            run.exchange = poisoned.into_inner().take();
            true
        }
    };
    run.invalid_exchange = poisoned;
    if run.supervised.shutdown.is_none() {
        if let Some(owner) = &run.owner {
            owner.cancellation().cancel();
        }
    }
    // Supervision normally closes/reaps the provider. Even on an unobserved
    // namespace, collection has a finite budget and keeps a still-live thread
    // inside this strong owner/working guard instead of blocking or detaching.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !run.observe_exchange_stopped() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    if poisoned {
        run.response = Err(refused("Original worker exchange lock failed"));
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_output_unknown_shutdown_preserves_private_working_ancestry() {
        let working = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let path = working.path().to_path_buf();
        let run: OriginalWorkerRun<OfflineOutputTransaction> = OriginalWorkerRun {
            supervised: SupervisedRun {
                result: Err(io::Error::from(io::ErrorKind::Interrupted).into()),
                shutdown: None,
            },
            response: Err(io::ErrorKind::Interrupted.into()),
            key: ServiceLeaseKey::default(),
            working: Some(working),
            owner: None,
            exchange: None,
            invalid_exchange: false,
        };
        assert!(run.quiescent_response().is_err());
        drop(run);
        let facts = std::fs::symlink_metadata(&path).unwrap();
        assert!(facts.is_dir());
        assert_eq!(facts.permissions().mode() & 0o7777, 0o700);
        // The test owns this exact unlaunched directory, so it can remove its
        // fixture. Production retirement cannot invent this knowledge.
        std::fs::remove_dir(&path).unwrap();
    }
}
