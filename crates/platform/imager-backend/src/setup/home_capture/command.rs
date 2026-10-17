//! Privileged synchronous command core; the app-facing actor runs it off UI.
use super::{
    Broker, Command, Response, Worker, failure, host_runner::Event, process::Custody, worker,
};
use crate::capture::host_session::{HostProcess, HostSession};
use std::{
    fs::File,
    io,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

impl Broker {
    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub fn run_command(
        &mut self,
        executable: &Path,
        command: Command,
        output: Option<File>,
        cancel: &AtomicBool,
    ) -> io::Result<Response> {
        self.run_command_reporting(executable, command, output, cancel, &mut |_| Ok(()))
    }
    pub fn run_command_reporting(
        &mut self,
        executable: &Path,
        command: Command,
        output: Option<File>,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(
            super::super::portable_transfer::ArchiveProgress,
        ) -> io::Result<()>,
    ) -> io::Result<Response> {
        self.validate(cancel)?;
        if self.worker.is_some() {
            return Err(failure("capture command already active"));
        }
        let needs_output = !matches!(command, Command::Review);
        let export_operation = match &command {
            Command::Review => None,
            Command::ReviewOriginal { .. }
            | Command::ExportOriginal { .. }
            | Command::ValidateOriginal { .. }
            | Command::ExportOriginalApplications { .. }
            | Command::ValidateOriginalApplications { .. } => {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Command::Export {
                export_operation, ..
            }
            | Command::ValidateExisting {
                export_operation, ..
            } => {
                super::operation(export_operation)?;
                Some(export_operation.clone())
            }
        };
        if needs_output != output.is_some() {
            return Err(failure("capture command output authority differs"));
        }
        if let Some(output) = &output {
            self.validate_output(output)?;
        }
        let mut exchange = worker::Exchange::new(&self.record.grant.operation, &command);
        let request = self.worker_request(command)?;
        let source = self
            .lease
            .as_ref()
            .ok_or_else(|| failure("capture missing"))?
            .selected()
            .try_clone()?;
        let scratch = self.store.inventory_scratch(&self.record)?;
        let mut session =
            match HostSession::spawn(executable, self.record.grant.caller, output.is_some()) {
                Ok(value) => value,
                Err(error) => {
                    scratch.receipt.cleanup()?;
                    return Err(error);
                }
            };
        let process = session.process();
        let custody = Custody {
            host: session.identity(),
            namespace: None,
            projection: Some(session.projection_receipt()),
            launch_acknowledged: false,
            inventory_scratch: Some(scratch.receipt.clone()),
        };
        // Durable BEFORE any approved subtree or secret crosses into the host.
        self.record.worker = Some(custody.clone());
        if let Err(error) = self.store.save(&self.record, false) {
            process.abort_unlaunched()?;
            session.projection_receipt().cleanup()?;
            scratch.receipt.cleanup()?;
            return Err(error);
        }
        let owned = Arc::new(Process {
            process,
            custody: Mutex::new(custody),
            complete: AtomicBool::new(false),
            inventory_scratch: scratch,
        });
        self.worker = Some(Box::new(owned.clone()));
        owned.inventory_scratch.validate_backing()?;
        self.validate(cancel)?;
        session.handoff(
            &source,
            output.as_ref(),
            (
                &owned.inventory_scratch.directory,
                owned.inventory_scratch.quota_bytes,
            ),
            &request,
            &self.record.grant.operation,
        )?;
        let mut reader = session.events()?;
        let (send, receive) = mpsc::sync_channel(8);
        let latest = Arc::new(Mutex::new(None));
        let reader_latest = latest.clone();
        std::thread::spawn(move || {
            loop {
                let mut event = worker::read_frame(&mut reader, 16 * 1024).and_then(|bytes| {
                    serde_json::from_slice::<Event>(&bytes).map_err(io::Error::other)
                });
                if let Ok(Event::Progress {
                    operation,
                    export_operation,
                    progress,
                }) = &event
                {
                    let result = exchange
                        .progress(operation, export_operation, *progress)
                        .and_then(|()| {
                            *reader_latest
                                .lock()
                                .map_err(|_| failure("capture progress poisoned"))? =
                                Some(*progress);
                            Ok(())
                        });
                    match result {
                        Ok(()) => continue,
                        Err(error) => event = Err(error),
                    }
                }
                if let Ok(Event::Response { operation, .. }) = &event {
                    if let Err(error) = exchange.response(operation) {
                        event = Err(error);
                    }
                }
                let finished = event.is_err()
                    || matches!(
                        &event,
                        Ok(Event::Complete { .. } | Event::Failed { .. } | Event::Refused { .. })
                    );
                if send.try_send(event).is_err() || finished {
                    break;
                }
            }
        });
        let operation = self.record.grant.operation.clone();
        let mut response = None;
        loop {
            owned.inventory_scratch.validate_backing()?;
            self.renew_active(cancel)?;
            if self
                .record
                .worker
                .as_ref()
                .and_then(|worker| worker.namespace)
                .is_some()
            {
                let value = latest
                    .lock()
                    .map_err(|_| failure("capture progress poisoned"))?
                    .take();
                if let Some(value) = value {
                    progress(value)?;
                }
            }
            match receive.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(Event::Refused {
                    operation: actual,
                    phase,
                    class,
                })) if actual == operation => {
                    return Err(io::Error::other(format!(
                        "capture host refused at {phase:?}: {class:?}"
                    )));
                }
                Ok(Ok(Event::Launching { operation: actual })) if actual == operation => {
                    let mut protected = self
                        .record
                        .worker
                        .clone()
                        .ok_or_else(|| failure("capture host custody lost"))?;
                    if protected.launch_acknowledged {
                        return Err(failure("capture launch was declared twice"));
                    }
                    protected.launch_acknowledged = true;
                    self.record.worker = Some(protected.clone());
                    self.store.save(&self.record, false)?;
                    *owned
                        .custody
                        .lock()
                        .map_err(|_| failure("capture custody poisoned"))? = protected;
                    session.acknowledge()?;
                }
                Ok(Ok(Event::Namespace {
                    operation: actual,
                    custody,
                })) if actual == operation => {
                    let mut protected = self
                        .record
                        .worker
                        .clone()
                        .ok_or_else(|| failure("capture host custody lost"))?;
                    if protected.namespace.is_some() {
                        return Err(failure("capture namespace was declared twice"));
                    }
                    protected.admit_namespace(custody)?;
                    self.record.worker = Some(protected.clone());
                    self.store.save(&self.record, false)?;
                    *owned
                        .custody
                        .lock()
                        .map_err(|_| failure("capture custody poisoned"))? = protected;
                    session.acknowledge()?;
                }
                Ok(Ok(Event::Response {
                    operation: actual,
                    response: value,
                })) if actual == operation => {
                    if response.is_some()
                        || self
                            .record
                            .worker
                            .as_ref()
                            .and_then(|worker| worker.namespace)
                            .is_none()
                    {
                        return Err(failure("capture response has no unique admitted namespace"));
                    }
                    if let Response::Export { expected } = &value {
                        expected.validate()?;
                        if export_operation.as_deref() != Some(expected.operation.as_str()) {
                            return Err(failure("capture export operation changed"));
                        }
                    }
                    response = Some(value);
                }
                Ok(Ok(Event::Complete { operation: actual })) if actual == operation => {
                    if response.is_none() {
                        return Err(failure("capture completed without a response"));
                    }
                    let status = owned.process.wait()?;
                    if !status.success() {
                        return Err(failure("capture native host exited unsuccessfully"));
                    }
                    owned
                        .custody
                        .lock()
                        .map_err(|_| failure("capture custody poisoned"))?
                        .require_stopped()?;
                    owned.complete.store(true, Ordering::Release);
                    self.record.worker = None;
                    self.store.save(&self.record, false)?;
                    self.worker = None;
                    return response.ok_or_else(|| failure("capture response lost"));
                }
                Ok(Ok(_)) => return Err(failure("capture host event differs from operation")),
                Ok(Err(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(failure("capture host channel closed"));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if owned.process.try_wait()?.is_some() {
                        return Err(failure("capture host exited without completion"));
                    }
                }
            }
        }
    }
}

struct Process {
    process: HostProcess,
    custody: Mutex<Custody>,
    complete: AtomicBool,
    inventory_scratch: super::store::ScratchLease,
}
impl Drop for Process {
    fn drop(&mut self) {
        if !self.complete.load(Ordering::Acquire)
            && self
                .custody
                .get_mut()
                .map_or_else(
                    |_| Err(io::ErrorKind::Other.into()),
                    |custody| custody.require_stopped(),
                )
                .is_err()
        {
            // Quarantine the allocator lease on unproved termination. There is
            // no recursive scratch destructor and no reuse of this child.
            self.inventory_scratch.retain();
        }
    }
}
impl Worker for Arc<Process> {
    fn cancel_and_reap(&mut self) -> io::Result<()> {
        if self.complete.load(Ordering::Acquire) {
            return Ok(());
        }
        let _ = self.process.revoke();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.process.try_wait()?.is_some() {
                break;
            }
            if Instant::now() >= deadline {
                self.custody
                    .lock()
                    .map_err(|_| failure("capture custody poisoned"))?
                    .stop_and_observe()?;
                self.process.wait()?;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.custody
            .lock()
            .map_err(|_| failure("capture custody poisoned"))?
            .require_stopped()
    }
}
