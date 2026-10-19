//! Fixed native original-user launcher entry, reached only through helper dispatch.
use super::{
    failure,
    runtime::{PACKAGE, TrustedCaptureService},
    transport::{CHANNEL_FD, WorkerChannel},
    worker,
};
use crate::capture::{host_owner::CaptureHostOwner, host_session as host};
use launcher::{
    owned_service::{self, CaptureOutputAdmission},
    run::RunError,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, net::UnixStream},
    },
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

pub(super) use crate::capture::process::Namespace;

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum Event {
    Refused {
        operation: String,
        phase: FailurePhase,
        class: FailureClass,
    },
    Launching {
        operation: String,
    },
    Namespace {
        operation: String,
        custody: Namespace,
    },
    Response {
        operation: String,
        response: worker::Response,
    },
    Progress {
        operation: String,
        export_operation: String,
        progress: super::super::portable_transfer::ArchiveProgress,
    },
    Complete {
        operation: String,
    },
    Failed {
        operation: String,
    },
}

/// Fixed diagnostics travel only over the authenticated broker channel. Never
/// serialize an underlying error, package path, command or secret here.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FailurePhase {
    PackageAdmission,
    RuntimeAdmission,
    InstallationAdmission,
    OutputAdmission,
    InventoryScratchAdmission,
    WorkingDirectory,
    LauncherStartup,
    LauncherManifest,
    LauncherPolicy,
    LauncherWorkingDirectory,
    LauncherServices,
    LauncherConfinement,
    LauncherRuntimePolicy,
    LauncherRuntimeLibraries,
    LauncherPreparation,
    LauncherNamespaceExec,
    LauncherNamespaceInfoEof,
    LauncherNamespaceInfoMalformed,
    ProviderExchange,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum FailureClass {
    PermissionDenied,
    NotFound,
    InvalidInput,
    InvalidData,
    Unsupported,
    Interrupted,
    Other,
}

fn admitted<T>(
    channel: &mut UnixStream,
    operation: &str,
    phase: FailurePhase,
    result: io::Result<T>,
) -> io::Result<T> {
    result.inspect_err(|error| {
        let class = match error.kind() {
            io::ErrorKind::PermissionDenied => FailureClass::PermissionDenied,
            io::ErrorKind::NotFound => FailureClass::NotFound,
            io::ErrorKind::InvalidInput => FailureClass::InvalidInput,
            io::ErrorKind::InvalidData => FailureClass::InvalidData,
            io::ErrorKind::Unsupported => FailureClass::Unsupported,
            io::ErrorKind::Interrupted => FailureClass::Interrupted,
            _ => FailureClass::Other,
        };
        let _ = write_event(
            channel,
            &Event::Refused {
                operation: operation.into(),
                phase,
                class,
            },
        );
    })
}

fn launcher_error_kind(error: &RunError) -> io::ErrorKind {
    let mut source: &dyn std::error::Error = error;
    for _ in 0..8 {
        if let Some(error) = source.downcast_ref::<io::Error>() {
            return error.kind();
        }
        let Some(next) = source.source() else {
            break;
        };
        source = next;
    }
    match error {
        RunError::RuntimeBundleMissing { .. } | RunError::RuntimeLibraryMissing { .. } => {
            io::ErrorKind::NotFound
        }
        RunError::RuntimeBundleUnsigned { .. } | RunError::RuntimeBundleSignature { .. } => {
            io::ErrorKind::PermissionDenied
        }
        RunError::InvalidServiceLaunch(_)
        | RunError::AppIdMismatch { .. }
        | RunError::InvalidCapturePlan
        | RunError::ArgvInvariant => io::ErrorKind::InvalidData,
        _ => io::ErrorKind::Other,
    }
}

fn launcher_failure_phase(error: &RunError) -> FailurePhase {
    match error {
        RunError::Spawn(_) => {
            let mut source: &dyn std::error::Error = error;
            for _ in 0..8 {
                if let Some(json) = source.downcast_ref::<serde_json::Error>() {
                    return if json.is_eof() {
                        FailurePhase::LauncherNamespaceInfoEof
                    } else {
                        FailurePhase::LauncherNamespaceInfoMalformed
                    };
                }
                // io::Error can wrap a typed payload whose own source is None.
                // Inspect that payload itself, never its Display text.
                let next = source
                    .downcast_ref::<io::Error>()
                    .and_then(io::Error::get_ref)
                    .map(|inner| inner as &dyn std::error::Error)
                    .or_else(|| source.source());
                let Some(next) = next else { break };
                source = next;
            }
            FailurePhase::LauncherNamespaceExec
        }
        RunError::Manifest(_) => FailurePhase::LauncherManifest,
        RunError::AppDir { .. } | RunError::NoDataHome(_) => FailurePhase::LauncherWorkingDirectory,
        RunError::Service(_) => FailurePhase::LauncherServices,
        RunError::Seccomp(_) => FailurePhase::LauncherConfinement,
        RunError::RuntimeLibraryMissing { .. } => FailurePhase::LauncherRuntimeLibraries,
        RunError::RuntimeBundleMissing { .. }
        | RunError::RuntimeReferenceMalformed(_)
        | RunError::RuntimeBundleUnsigned { .. }
        | RunError::RuntimeBundleSignature { .. } => FailurePhase::LauncherRuntimePolicy,
        RunError::Io(_) => FailurePhase::LauncherPreparation,
        _ => FailurePhase::LauncherPolicy,
    }
}

pub fn maybe_run_host(args: &[String]) -> Result<Option<i32>, String> {
    if !args.iter().any(|arg| arg == host::ARG) {
        return Ok(None);
    }
    if args.len() != 2 || args.get(1).map(String::as_str) != Some(host::ARG) {
        return Err("Capture host accepts only its fixed helper switch".into());
    }
    run()
        .map(|()| Some(0))
        .map_err(|_| "The native home capture worker could not finish".into())
}

fn run() -> io::Result<()> {
    let uid = rustix::process::getuid().as_raw();
    if uid == 0 || rustix::process::geteuid().as_raw() != uid {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    // This inherited socket is owned once. Authentication precedes all rights.
    let socket = unsafe { UnixStream::from_raw_fd(CHANNEL_FD) };
    let mut channel = WorkerChannel::authenticate(socket)?;
    let source = channel.receive(1, true)?;
    let input = channel.receive(3, false)?;
    let bytes = sealed_input(input)?;
    let request: worker::Request = serde_json::from_slice(&bytes)?;
    request.require_capture_route()?;
    super::operation(&request.operation)?;
    channel
        .authenticate_root(u32::try_from(unsafe { libc::getppid() }).map_err(io::Error::other)?)?;
    let (scratch, scratch_quota) = channel.receive_inventory_scratch()?;
    let output = if matches!(request.command, worker::Command::Review) {
        None
    } else {
        Some(channel.receive(2, true)?)
    };
    // Complete root output authentication before potentially slow package
    // verification, so the broker can observe an unlaunched host failure.
    let output_proof = output
        .as_ref()
        .map(|output| {
            CaptureOutputAdmission::receive_from_broker(
                channel.stream(),
                &source,
                output,
                uid,
                &request.operation,
            )
        })
        .transpose()?;
    let service = admitted(
        channel.stream(),
        &request.operation,
        FailurePhase::PackageAdmission,
        TrustedCaptureService::native(),
    )?;
    let admission = admitted(
        channel.stream(),
        &request.operation,
        FailurePhase::RuntimeAdmission,
        CaptureHostOwner::verify(&service.root, &service.publisher, source.try_clone()?, uid),
    )?;
    let mut admission = admitted(
        channel.stream(),
        &request.operation,
        FailurePhase::InstallationAdmission,
        admission.with_installation(service.installed.clone(), &service.service_publisher),
    )?;
    admission = admitted(
        channel.stream(),
        &request.operation,
        FailurePhase::InventoryScratchAdmission,
        admission.with_inventory_scratch(scratch, scratch_quota),
    )?;
    if let Some(output) = output {
        let proof = output_proof.ok_or_else(|| failure("capture output proof missing"))?;
        admission = admitted(
            channel.stream(),
            &request.operation,
            FailurePhase::OutputAdmission,
            admission.with_broker_output(output, proof),
        )?;
    }
    let working = admitted(
        channel.stream(),
        &request.operation,
        FailurePhase::WorkingDirectory,
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir(),
    )?;
    admitted(
        channel.stream(),
        &request.operation,
        FailurePhase::WorkingDirectory,
        launcher::appdir::prepare_private_host_app_dir(working.path(), None, PACKAGE),
    )?;
    let key = admission.lease_key();
    let cancellation = admission.cancellation();
    let stream = Arc::new(Mutex::new(channel.stream().try_clone()?));
    let connected_stream = stream.clone();
    let operation = request.operation.clone();
    let connected_operation = operation.clone();
    let connected_cancel = cancellation;
    let command = bytes;
    let mut exchange = worker::Exchange::new(&operation, &request.command);
    let jobs = Arc::new(Mutex::new(None));
    let connected_jobs = jobs.clone();
    let monitors = Arc::new(Mutex::new(None));
    let connected_monitors = monitors.clone();
    let connected = Box::new(move |endpoint: owned_service::ServiceEndpoint| {
        let facts = endpoint.custody()?;
        let mut channel = connected_stream
            .lock()
            .map_err(|_| failure("capture host channel poisoned"))?;
        write_event(
            &mut channel,
            &Event::Namespace {
                operation: connected_operation.clone(),
                custody: Namespace {
                    init_pid: facts.init_pid,
                    start_ticks: facts.start_ticks,
                    namespace_dev: facts.namespace_dev,
                    namespace_ino: facts.namespace_ino,
                },
            },
        )?;
        let mut ack = [0];
        channel.read_exact(&mut ack)?;
        if ack != [1] {
            return Err(failure("capture namespace custody was not acknowledged"));
        }
        // After durable ACK, only cancellation/EOF arrives in this direction.
        let mut control = channel.try_clone()?;
        control.set_read_timeout(None)?;
        let monitor_cancel = connected_cancel.clone();
        let monitor = std::thread::spawn(move || {
            let mut byte = [0];
            let _ = control.read(&mut byte);
            monitor_cancel.cancel();
        });
        *connected_monitors
            .lock()
            .map_err(|_| failure("capture monitor poisoned"))? = Some(monitor);
        drop(channel);
        let job = std::thread::spawn(move || {
            let result = (|| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                let mut provider = loop {
                    match endpoint.connect() {
                        Ok(provider) => break provider,
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                            ) && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(10))
                        }
                        Err(error) => return Err(error),
                    }
                };
                worker::write_frame(&mut provider, &command, 64 * 1024)?;
                loop {
                    let bytes = worker::read_frame(&mut provider, 16 * 1024)?;
                    let frame: worker::Frame = serde_json::from_slice(&bytes)?;
                    let event = match frame {
                        worker::Frame::Progress {
                            operation,
                            export_operation,
                            progress,
                        } => {
                            exchange.progress(&operation, &export_operation, progress)?;
                            Event::Progress {
                                operation,
                                export_operation,
                                progress,
                            }
                        }
                        worker::Frame::Response {
                            operation,
                            export_operation,
                            response,
                        } => {
                            exchange.finish(&operation, export_operation.as_deref())?;
                            let mut channel = connected_stream
                                .lock()
                                .map_err(|_| failure("capture host channel poisoned"))?;
                            return write_event(
                                &mut channel,
                                &Event::Response {
                                    operation,
                                    response,
                                },
                            );
                        }
                    };
                    let mut channel = connected_stream
                        .lock()
                        .map_err(|_| failure("capture host channel poisoned"))?;
                    write_event(&mut channel, &event)?;
                }
            })();
            if result.is_err() {
                connected_cancel.cancel();
            }
            result
        });
        *connected_jobs
            .lock()
            .map_err(|_| failure("capture job poisoned"))? = Some(job);
        Ok(())
    });
    {
        let mut channel = stream
            .lock()
            .map_err(|_| failure("capture host channel poisoned"))?;
        write_event(
            &mut channel,
            &Event::Launching {
                operation: operation.clone(),
            },
        )?;
        let mut ack = [0];
        channel.read_exact(&mut ack)?;
        if ack != [1] {
            return Err(failure("capture launch was not acknowledged"));
        }
    }
    let result = admission.run(working.path(), Some(connected));
    let job = jobs
        .lock()
        .map_err(|_| failure("capture job poisoned"))?
        .take();
    let job = {
        let mut channel = stream
            .lock()
            .map_err(|_| failure("capture host channel poisoned"))?;
        admitted(
            &mut channel,
            &operation,
            result
                .as_ref()
                .err()
                .map_or(FailurePhase::LauncherStartup, launcher_failure_phase),
            job.ok_or_else(|| {
                io::Error::from(
                    result
                        .as_ref()
                        .err()
                        .map_or(io::ErrorKind::InvalidData, launcher_error_kind),
                )
            }),
        )?
    };
    let completed_job = job
        .join()
        .map_err(|_| failure("capture provider thread panicked"))?;
    let mut channel = stream
        .lock()
        .map_err(|_| failure("capture host channel poisoned"))?;
    let completed_job = admitted(
        &mut channel,
        &operation,
        FailurePhase::ProviderExchange,
        completed_job,
    );
    let completed = match result {
        Ok(witness) if completed_job.is_ok() && witness.matches(&key, working.path(), PACKAGE) => {
            write_event(&mut channel, &Event::Complete { operation })?;
            true
        }
        _ => {
            write_event(&mut channel, &Event::Failed { operation })?;
            false
        }
    };
    channel.shutdown(std::net::Shutdown::Both)?;
    drop(channel);
    let monitor = monitors
        .lock()
        .map_err(|_| failure("capture monitor poisoned"))?
        .take();
    if let Some(monitor) = monitor {
        monitor
            .join()
            .map_err(|_| failure("capture monitor thread panicked"))?;
    }
    if completed {
        Ok(())
    } else {
        Err(failure("capture namespace did not finish cleanly"))
    }
}

fn sealed_input(mut file: File) -> io::Result<Zeroizing<Vec<u8>>> {
    let required = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if seals < 0 || seals & required != required || file.metadata()?.len() > 64 * 1024 {
        return Err(failure(
            "capture command is not bounded sealed native input",
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(failure("capture sealed input exceeds bound"));
    }
    Ok(bytes)
}

pub(super) fn write_event(channel: &mut UnixStream, event: &Event) -> io::Result<()> {
    worker::write_frame(channel, &serde_json::to_vec(event)?, 16 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_failure_frame_contains_only_fixed_phase_and_class() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let secret = "private password and /private/source/path";
        let result: io::Result<()> = admitted(
            &mut sender,
            &operation,
            FailurePhase::RuntimeAdmission,
            Err(io::Error::new(io::ErrorKind::PermissionDenied, secret)),
        );
        assert!(result.is_err());
        let frame = worker::read_frame(&mut receiver, 16 * 1024).unwrap();
        assert!(!String::from_utf8_lossy(&frame).contains(secret));
        assert!(matches!(
            serde_json::from_slice::<Event>(&frame).unwrap(),
            Event::Refused {
                operation: actual,
                phase: FailurePhase::RuntimeAdmission,
                class: FailureClass::PermissionDenied,
            } if actual == operation
        ));
    }

    #[test]
    fn startup_failure_preserves_nested_io_class_without_error_text() {
        let error = RunError::AppDir {
            package: "private package".into(),
            path: "/private/path".into(),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "secret detail"),
        };
        assert_eq!(launcher_error_kind(&error), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn startup_boundary_distinguishes_namespace_info_without_private_error_text() {
        let empty = serde_json::from_slice::<serde_json::Value>(b"").unwrap_err();
        let error = RunError::Spawn(io::Error::other(empty));
        assert!(matches!(
            launcher_failure_phase(&error),
            FailurePhase::LauncherNamespaceInfoEof
        ));
        let invalid =
            serde_json::from_slice::<serde_json::Value>(b"private-secret-path").unwrap_err();
        let error = RunError::Spawn(io::Error::other(invalid));
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let result: io::Result<()> = admitted(
            &mut sender,
            &operation,
            launcher_failure_phase(&error),
            Err(io::Error::from(launcher_error_kind(&error))),
        );
        assert!(result.is_err());
        let frame = worker::read_frame(&mut receiver, 16 * 1024).unwrap();
        assert!(!String::from_utf8_lossy(&frame).contains("private-secret-path"));
        assert!(matches!(
            serde_json::from_slice::<Event>(&frame).unwrap(),
            Event::Refused {
                operation: actual,
                phase: FailurePhase::LauncherNamespaceInfoMalformed,
                class: FailureClass::Other,
            } if actual == operation
        ));
        let private = RunError::AppDir {
            package: "private package".into(),
            path: "/private/path".into(),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "private password"),
        };
        assert!(matches!(
            launcher_failure_phase(&private),
            FailurePhase::LauncherWorkingDirectory
        ));
        assert!(matches!(
            launcher_failure_phase(&RunError::Spawn(io::Error::from(
                io::ErrorKind::PermissionDenied
            ))),
            FailurePhase::LauncherNamespaceExec
        ));
    }
}
