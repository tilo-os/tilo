//! Bounded raw-home provider protocol inside the canonical capture sandbox.
//! Source and output are fixed launcher projections, never command paths.
use super::super::portable_transfer::{ArchivePhase, ArchiveProgress};
#[cfg(feature = "capture-worker")]
use super::super::{
    migration::PinnedDirectory,
    portable_home_media::{HomeSource, MediaGrant},
};
use super::super::{migration_review::CaptureCustody, portable_home_media::Expectation};
use super::failure;
mod original;
#[cfg(feature = "capture-worker")]
mod original_applications;
#[cfg(feature = "capture-worker")]
mod original_applications_only;
#[cfg(feature = "capture-worker")]
use super::operation;
pub(in crate::setup::home_capture) use original::OriginalMountedClaim;
pub use original::OriginalReadClaim;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
#[cfg(feature = "capture-worker")]
use std::{path::Path, sync::atomic::AtomicBool};
use zeroize::{Zeroize, Zeroizing};

#[cfg(any(feature = "capture-worker", test))]
const REQUEST_BOUND: usize = 64 * 1024;
#[cfg(feature = "capture-worker")]
const RESPONSE_BOUND: usize = 16 * 1024;
// Protocol 5 additionally binds original mounted roots to the sealed FD51 table.
// Old signed workers cannot silently inventory only the unmounted Home view.
pub(super) use crate::capture::{APPLICATIONS_PROTOCOL, WORKER_PROTOCOL};

/// Standalone signed service entry. The canonical launcher creates a fresh
/// private app directory; an existing provider name is never removed/reused.
#[cfg(feature = "capture-worker")]
pub fn run_provider() -> io::Result<()> {
    use std::os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixListener,
    };
    use std::time::Duration;
    if rustix::process::getuid().as_raw() == 0
        || rustix::process::geteuid().as_raw() != rustix::process::getuid().as_raw()
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let directory = PinnedDirectory::open_absolute(Path::new(launcher::appdir::SANDBOX_APP_DIR))?;
    let facts = directory.file.metadata()?;
    if facts.uid() != rustix::process::getuid().as_raw() || facts.mode() & 0o077 != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let path = Path::new(launcher::appdir::SANDBOX_APP_DIR).join("provider.sock");
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let created = std::fs::symlink_metadata(&path)?;
    let (mut stream, _) = listener.accept()?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let result = serve_once(&mut stream, &AtomicBool::new(false));
    drop(stream);
    drop(listener);
    let current = std::fs::symlink_metadata(&path)?;
    if (current.dev(), current.ino()) != (created.dev(), created.ino()) {
        return Err(failure("capture provider socket was substituted"));
    }
    std::fs::remove_file(path)?;
    result
}

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(Vec<u8>);
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl Secret {
    pub fn new(mut value: Zeroizing<Vec<u8>>) -> Self {
        Self(std::mem::take(&mut *value))
    }
    #[cfg(feature = "capture-worker")]
    fn take(mut self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(std::mem::take(&mut self.0))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub(super) schema: u32,
    pub(super) operation: String,
    pub(super) custody: Option<CaptureCustody>,
    pub(super) command: Command,
}
impl Request {
    pub(super) fn validate_protocol(&self) -> io::Result<()> {
        let required = self
            .command
            .original_authority()
            .map_or(WORKER_PROTOCOL, OriginalReadClaim::required_protocol);
        if self.schema != required {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "capture worker protocol version is unsupported",
            ));
        }
        if let Some(authority) = self.command.original_authority() {
            authority.validate_shape(&self.operation)?;
            if self.custody.is_some() {
                return Err(failure("Original reader cannot accept snapshot custody"));
            }
        }
        match &self.command {
            Command::ExportOriginal {
                authority,
                export_operation,
                setup_binding,
                ..
            } => authority.validate_export(export_operation, setup_binding, None)?,
            Command::ValidateOriginal {
                authority,
                export_operation,
                setup_binding,
                expected,
                applications,
                ..
            } => {
                authority.validate_export(export_operation, setup_binding, Some(expected))?;
                authority.validate_applications(applications.as_ref(), expected)?;
            }
            Command::ExportOriginalApplications { authority, .. } => {
                authority.validate_applications_only(None)?
            }
            Command::ValidateOriginalApplications {
                authority,
                expected,
                ..
            } => authority.validate_applications_only(Some(expected))?,
            _ => {}
        }
        Ok(())
    }
    pub(super) fn require_capture_route(&self) -> io::Result<()> {
        self.validate_protocol()?;
        if self.command.original_authority().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Original reader requires authenticated offline root admission",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Command {
    Review,
    ReviewOriginal {
        authority: OriginalReadClaim,
    },
    ExportOriginalApplications {
        authority: OriginalReadClaim,
        password: Secret,
    },
    ValidateOriginalApplications {
        authority: OriginalReadClaim,
        expected: crate::setup::flatpak_transfer::portable::Expectation,
        password: Secret,
    },
    ExportOriginal {
        authority: OriginalReadClaim,
        export_operation: String,
        setup_binding: String,
        password: Secret,
    },
    ValidateOriginal {
        authority: OriginalReadClaim,
        export_operation: String,
        expected: Expectation,
        #[serde(default)]
        applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
        setup_binding: String,
        password: Secret,
    },
    Export {
        export_operation: String,
        setup_binding: String,
        password: Secret,
    },
    ValidateExisting {
        export_operation: String,
        expected: Expectation,
        setup_binding: String,
        password: Secret,
    },
}

impl Command {
    pub(super) fn original_authority(&self) -> Option<&OriginalReadClaim> {
        match self {
            Self::ReviewOriginal { authority }
            | Self::ExportOriginal { authority, .. }
            | Self::ValidateOriginal { authority, .. }
            | Self::ExportOriginalApplications { authority, .. }
            | Self::ValidateOriginalApplications { authority, .. } => Some(authority),
            _ => None,
        }
    }
}
impl Request {
    pub fn into_export(mut self, password: Zeroizing<Vec<u8>>) -> io::Result<Self> {
        self.validate_protocol()?;
        let Command::ReviewOriginal { authority } = self.command else {
            return Err(io::ErrorKind::PermissionDenied.into());
        };
        check_original_secret(&password)?;
        if authority.is_applications_only() {
            self.command = Command::ExportOriginalApplications {
                authority,
                password: Secret::new(password),
            };
            self.validate_protocol()?;
            return Ok(self);
        }
        self.command = Command::ExportOriginal {
            export_operation: authority.export_operation().into(),
            setup_binding: authority.setup_binding().into(),
            authority,
            password: Secret::new(password),
        };
        self.validate_protocol()?;
        Ok(self)
    }
    pub fn into_validate_applications(
        mut self,
        expected: crate::setup::flatpak_transfer::portable::Expectation,
        password: Zeroizing<Vec<u8>>,
    ) -> io::Result<Self> {
        self.validate_protocol()?;
        let Command::ReviewOriginal { authority } = self.command else {
            return Err(io::ErrorKind::PermissionDenied.into());
        };
        check_original_secret(&password)?;
        authority.validate_applications_only(Some(&expected))?;
        self.command = Command::ValidateOriginalApplications {
            authority,
            expected,
            password: Secret::new(password),
        };
        self.validate_protocol()?;
        Ok(self)
    }
    #[cfg(test)]
    pub fn into_validate(
        self,
        expected: Expectation,
        password: Zeroizing<Vec<u8>>,
    ) -> io::Result<Self> {
        self.into_validate_selected(expected, None, password)
    }
    pub fn into_validate_selected(
        mut self,
        expected: Expectation,
        applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
        password: Zeroizing<Vec<u8>>,
    ) -> io::Result<Self> {
        self.validate_protocol()?;
        let Command::ReviewOriginal { authority } = self.command else {
            return Err(io::ErrorKind::PermissionDenied.into());
        };
        check_original_secret(&password)?;
        self.command = Command::ValidateOriginal {
            export_operation: authority.export_operation().into(),
            setup_binding: authority.setup_binding().into(),
            authority,
            expected,
            applications,
            password: Secret::new(password),
        };
        self.validate_protocol()?;
        Ok(self)
    }
}
fn check_original_secret(password: &[u8]) -> io::Result<()> {
    if password.is_empty() || password.len() > 4096 {
        return Err(failure("Original export secret exceeds bounds"));
    }
    Ok(())
}

// Serde accepts unknown fields on an internally tagged unit variant even
// with deny_unknown_fields. A zero-field struct variant closes that hole
// without changing the native Command::Review constructor.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
enum StrictCommand {
    Review {},
    ReviewOriginal {
        authority: OriginalReadClaim,
    },
    ExportOriginalApplications {
        authority: OriginalReadClaim,
        password: Secret,
    },
    ValidateOriginalApplications {
        authority: OriginalReadClaim,
        expected: crate::setup::flatpak_transfer::portable::Expectation,
        password: Secret,
    },
    ExportOriginal {
        authority: OriginalReadClaim,
        export_operation: String,
        setup_binding: String,
        password: Secret,
    },
    ValidateOriginal {
        authority: OriginalReadClaim,
        export_operation: String,
        expected: Expectation,
        #[serde(default)]
        applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
        setup_binding: String,
        password: Secret,
    },
    Export {
        export_operation: String,
        setup_binding: String,
        password: Secret,
    },
    ValidateExisting {
        export_operation: String,
        expected: Expectation,
        setup_binding: String,
        password: Secret,
    },
}
impl<'de> Deserialize<'de> for Command {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match StrictCommand::deserialize(deserializer)? {
            StrictCommand::Review {} => Self::Review,
            StrictCommand::ReviewOriginal { authority } => Self::ReviewOriginal { authority },
            StrictCommand::ExportOriginalApplications {
                authority,
                password,
            } => Self::ExportOriginalApplications {
                authority,
                password,
            },
            StrictCommand::ValidateOriginalApplications {
                authority,
                expected,
                password,
            } => Self::ValidateOriginalApplications {
                authority,
                expected,
                password,
            },
            StrictCommand::ExportOriginal {
                authority,
                export_operation,
                setup_binding,
                password,
            } => Self::ExportOriginal {
                authority,
                export_operation,
                setup_binding,
                password,
            },
            StrictCommand::ValidateOriginal {
                authority,
                export_operation,
                expected,
                applications,
                setup_binding,
                password,
            } => Self::ValidateOriginal {
                authority,
                export_operation,
                expected,
                applications,
                setup_binding,
                password,
            },
            StrictCommand::Export {
                export_operation,
                setup_binding,
                password,
            } => Self::Export {
                export_operation,
                setup_binding,
                password,
            },
            StrictCommand::ValidateExisting {
                export_operation,
                expected,
                setup_binding,
                password,
            } => Self::ValidateExisting {
                export_operation,
                expected,
                setup_binding,
                password,
            },
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Response {
    Review {
        files: u64,
        bytes: u64,
        links: u64,
        runtime_sockets: u64,
        sealed_reservation: u64,
        entries: usize,
    },
    Export {
        expected: Expectation,
    },
    ReviewOriginalApplications {
        staging_bytes: u64,
        sealed_bytes: u64,
    },
    ExportOriginalApplications {
        applications: crate::setup::flatpak_transfer::portable::Expectation,
    },
    ExportOriginal {
        home: Expectation,
        applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
    },
    Validated,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum Frame {
    Progress {
        operation: String,
        export_operation: String,
        progress: ArchiveProgress,
    },
    Response {
        operation: String,
        export_operation: Option<String>,
        response: Response,
    },
}

/// Bind every frame to one command and reject regressing or invented totals.
pub(super) struct Exchange {
    operation: String,
    export_operation: Option<String>,
    last: [Option<ArchiveProgress>; 4],
    finished: bool,
    delivery_total: Option<u64>,
}
impl Exchange {
    pub(super) fn new(operation: &str, command: &Command) -> Self {
        Self {
            operation: operation.into(),
            export_operation: match command {
                Command::Review | Command::ReviewOriginal { .. } => None,
                Command::ExportOriginalApplications { authority, .. }
                | Command::ValidateOriginalApplications { authority, .. } => {
                    authority.applications_operation().ok().map(str::to_owned)
                }
                Command::Export {
                    export_operation, ..
                }
                | Command::ValidateExisting {
                    export_operation, ..
                }
                | Command::ExportOriginal {
                    export_operation, ..
                }
                | Command::ValidateOriginal {
                    export_operation, ..
                } => Some(export_operation.clone()),
            },
            last: [None; 4],
            finished: false,
            delivery_total: match command {
                Command::ValidateExisting { expected, .. }
                | Command::ValidateOriginal { expected, .. } => Some(expected.sealed_bytes),
                _ => None,
            },
        }
    }
    pub(super) fn progress(
        &mut self,
        operation: &str,
        export_operation: &str,
        progress: ArchiveProgress,
    ) -> io::Result<()> {
        if self.finished
            || operation != self.operation
            || self.export_operation.as_deref() != Some(export_operation)
        {
            return Err(failure("capture progress command binding differs"));
        }
        let index = match progress.phase {
            ArchivePhase::Archiving => 0,
            ArchivePhase::VerifyingArchive => 1,
            ArchivePhase::VerifyingSource => 2,
            ArchivePhase::DeliveringCiphertext => 3,
        };
        match progress.phase {
            ArchivePhase::DeliveringCiphertext
                if progress.total == self.delivery_total
                    && progress
                        .total
                        .is_some_and(|total| total > 0 && progress.completed <= total) => {}
            ArchivePhase::DeliveringCiphertext => {
                return Err(failure("capture delivery progress bounds differ"));
            }
            _ if progress.total.is_some() => {
                return Err(failure(
                    "capture archive progress has an unauthenticated total",
                ));
            }
            _ => {}
        }
        if self.last[index]
            .is_some_and(|last| progress.completed < last.completed || progress.total != last.total)
        {
            return Err(failure("capture progress regressed"));
        }
        self.last[index] = Some(progress);
        Ok(())
    }
    pub(super) fn finish(
        &mut self,
        operation: &str,
        export_operation: Option<&str>,
    ) -> io::Result<()> {
        if self.finished
            || operation != self.operation
            || export_operation != self.export_operation.as_deref()
        {
            return Err(failure("capture final response command binding differs"));
        }
        self.finished = true;
        Ok(())
    }
    pub(super) fn response(&mut self, operation: &str) -> io::Result<()> {
        let export_operation = self.export_operation.clone();
        self.finish(operation, export_operation.as_deref())
    }
}

/// Receives secrets only on the launcher's authenticated private provider
/// connection. No request, password, or plaintext inventory is persisted.
#[cfg(feature = "capture-worker")]
pub fn serve_once(channel: &mut (impl Read + Write), cancel: &AtomicBool) -> io::Result<()> {
    let bytes = Zeroizing::new(read_frame(channel, REQUEST_BOUND)?);
    let request: Request = serde_json::from_slice(&bytes)?;
    request.validate_protocol()?;
    operation(&request.operation)?;
    let mut exchange = Exchange::new(&request.operation, &request.command);
    let export_operation = exchange.export_operation.clone();
    let mut last_sent: Option<(ArchivePhase, std::time::Instant)> = None;
    let mut pending_progress = None;
    let mut progress = |progress: ArchiveProgress| {
        let export_operation = export_operation
            .as_deref()
            .ok_or_else(|| failure("capture review cannot report archive progress"))?;
        exchange.progress(&request.operation, export_operation, progress)?;
        pending_progress = Some(progress);
        let now = std::time::Instant::now();
        if last_sent.is_some_and(|(phase, sent)| {
            phase == progress.phase
                && now.duration_since(sent) < std::time::Duration::from_millis(100)
        }) {
            return Ok(());
        }
        last_sent = Some((progress.phase, now));
        pending_progress = None;
        write_frame(
            channel,
            &serde_json::to_vec(&Frame::Progress {
                operation: request.operation.clone(),
                export_operation: export_operation.into(),
                progress,
            })?,
            RESPONSE_BOUND,
        )
    };
    let pin =
        PinnedDirectory::open_absolute(Path::new(launcher::owned_service::CAPTURE_SOURCE_PATH))?;
    let original_authority = request.command.original_authority().cloned();
    if original_authority
        .as_ref()
        .is_some_and(OriginalReadClaim::is_applications_only)
    {
        let response =
            original_applications_only::execute(request.command, pin, cancel, &mut progress)?;
        if let Some(progress) = pending_progress {
            write_frame(
                channel,
                &serde_json::to_vec(&Frame::Progress {
                    operation: request.operation.clone(),
                    export_operation: export_operation
                        .clone()
                        .ok_or_else(|| failure("Applications progress has no command"))?,
                    progress,
                })?,
                RESPONSE_BOUND,
            )?;
        }
        exchange.finish(&request.operation, export_operation.as_deref())?;
        return write_frame(
            channel,
            &serde_json::to_vec(&Frame::Response {
                operation: request.operation,
                export_operation,
                response,
            })?,
            RESPONSE_BOUND,
        );
    }
    // Snapshot and original workers both require root-admitted working storage.
    // Public request bytes cannot substitute a path or credentials for this FD.
    let allowance = std::env::var(launcher::owned_service::INVENTORY_SCRATCH_BYTES_ENV)
        .map_err(|_| failure("Home review needs admitted inventory storage"))?
        .parse::<u64>()
        .map_err(|_| failure("Inventory storage allowance is invalid"))?;
    let directory =
        PinnedDirectory::open_absolute(Path::new(launcher::owned_service::INVENTORY_SCRATCH_PATH))?;
    let scratch = crate::setup::migration_review::InventoryScratch::review(directory, allowance)?;
    let source = if let Some(authority) = &original_authority {
        HomeSource::review_original_reader(pin, authority, scratch, cancel)?
    } else {
        HomeSource::review_captured(pin, request.custody.as_ref(), scratch, cancel)?
    };
    let response = match request.command {
        Command::Review | Command::ReviewOriginal { .. } => {
            let summary = source.summary()?;
            Response::Review {
                files: summary.files,
                bytes: summary.bytes,
                links: summary.links,
                runtime_sockets: summary.runtime_sockets,
                sealed_reservation: source.sealed_reservation()?,
                entries: source.entry_count(),
            }
        }
        Command::Export {
            export_operation,
            setup_binding,
            password,
        } => {
            let password = password.take();
            operation(&export_operation)?;
            check_binding(&setup_binding, &password)?;
            let output = MediaGrant::open(Path::new(launcher::owned_service::CAPTURE_OUTPUT_PATH))?;
            let output = if let Some(authority) = &original_authority {
                output.limit_original_reservation(authority.sealed_reservation())?
            } else {
                output
            };
            let expected = source.export_to_reporting(
                &output,
                &export_operation,
                &setup_binding,
                password,
                cancel,
                &mut progress,
            )?;
            Response::Export { expected }
        }
        Command::ExportOriginalApplications { .. }
        | Command::ValidateOriginalApplications { .. } => {
            return Err(failure(
                "Applications-only phase cannot enter Home inventory",
            ));
        }
        Command::ExportOriginal {
            authority,
            export_operation,
            setup_binding,
            password,
        } => original_applications::export(
            &source,
            &authority,
            &export_operation,
            &setup_binding,
            password.take(),
            cancel,
            &mut progress,
        )?,
        Command::ValidateOriginal {
            authority,
            export_operation,
            expected,
            applications,
            setup_binding,
            password,
        } => {
            original_applications::validate(
                &source,
                &authority,
                (&expected, applications.as_ref()),
                &setup_binding,
                &password.take(),
                cancel,
                &mut progress,
            )?;
            // Both locators were bound to export_operation by validate_protocol.
            operation(&export_operation)?;
            Response::Validated
        }
        Command::ValidateExisting {
            export_operation,
            expected,
            setup_binding,
            password,
        } => {
            let password = password.take();
            operation(&export_operation)?;
            check_binding(&setup_binding, &password)?;
            expected.validate()?;
            if expected.operation != export_operation {
                return Err(failure("capture worker package operation differs"));
            }
            let output = MediaGrant::open(Path::new(launcher::owned_service::CAPTURE_OUTPUT_PATH))?;
            let output = if let Some(authority) = &original_authority {
                output.limit_original_reservation(authority.sealed_reservation())?
            } else {
                output
            };
            source.validate_existing_reporting(
                &output,
                &expected,
                &setup_binding,
                &password,
                cancel,
                &mut progress,
            )?;
            Response::Validated
        }
    };
    if let Some(authority) = &original_authority {
        let held = PinnedDirectory::open_absolute(Path::new(
            launcher::owned_service::CAPTURE_SOURCE_PATH,
        ))?;
        authority.validate_reader(&held.file)?;
        source.validate_with_cancel(cancel)?;
    }
    if let Some(progress) = pending_progress {
        write_frame(
            channel,
            &serde_json::to_vec(&Frame::Progress {
                operation: request.operation.clone(),
                export_operation: export_operation
                    .clone()
                    .ok_or_else(|| failure("capture progress has no command"))?,
                progress,
            })?,
            RESPONSE_BOUND,
        )?;
    }
    exchange.finish(&request.operation, export_operation.as_deref())?;
    write_frame(
        channel,
        &serde_json::to_vec(&Frame::Response {
            operation: request.operation,
            export_operation,
            response,
        })?,
        RESPONSE_BOUND,
    )
}

#[cfg(feature = "capture-worker")]
fn check_binding(binding: &str, password: &[u8]) -> io::Result<()> {
    if binding.is_empty() || binding.len() > 256 || password.is_empty() || password.len() > 4096 {
        return Err(failure("capture worker binding or secret exceeds bounds"));
    }
    Ok(())
}

pub(super) fn read_frame(input: &mut impl Read, bound: usize) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    input.read_exact(&mut header)?;
    let length = usize::try_from(u32::from_be_bytes(header)).map_err(io::Error::other)?;
    if length == 0 || length > bound {
        return Err(failure("capture private frame exceeds bounds"));
    }
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn write_frame(output: &mut impl Write, bytes: &[u8], bound: usize) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > bound {
        return Err(failure("capture private frame exceeds bounds"));
    }
    output.write_all(
        &u32::try_from(bytes.len())
            .map_err(io::Error::other)?
            .to_be_bytes(),
    )?;
    output.write_all(bytes)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_frames_refuse_oversize_before_allocating_and_truncated_input() {
        let mut input = (REQUEST_BOUND as u32 + 1).to_be_bytes().as_slice().to_vec();
        assert!(read_frame(&mut input.as_slice(), REQUEST_BOUND).is_err());
        input = vec![0, 0, 0, 2, 1];
        assert!(read_frame(&mut input.as_slice(), REQUEST_BOUND).is_err());
        assert!(write_frame(&mut Vec::new(), &[0; 4], 3).is_err());
    }
    #[test]
    fn worker_commands_have_no_source_or_output_path_authority() {
        let input = br#"{"schema":1,"operation":"01951e63-767d-7625-839e-a6be33ebec18","custody":null,"command":{"action":"review","source":"/other-home"}}"#;
        assert!(serde_json::from_slice::<Request>(input).is_err());
        let input = br#"{"schema":1,"operation":"01951e63-767d-7625-839e-a6be33ebec18","custody":null,"command":{"action":"review"},"output":"/other-volume"}"#;
        assert!(serde_json::from_slice::<Request>(input).is_err());
    }
    #[test]
    fn worker_protocol_rejects_old_requests_before_source_or_media_admission() {
        let mut request = Request {
            schema: 1,
            operation: uuid::Uuid::new_v4().to_string(),
            custody: None,
            command: Command::Review,
        };
        assert_eq!(
            request.validate_protocol().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        request.schema = WORKER_PROTOCOL;
        request.validate_protocol().unwrap();
    }
    #[test]
    fn progress_is_bound_monotonic_and_cannot_replace_final_reply() {
        let command = Command::Export {
            export_operation: "package-operation".into(),
            setup_binding: "binding".into(),
            password: Secret::new(Zeroizing::new(b"private secret".to_vec())),
        };
        let mut exchange = Exchange::new("capture-operation", &command);
        let progress = ArchiveProgress {
            phase: ArchivePhase::Archiving,
            completed: 12,
            total: None,
        };
        assert!(
            exchange
                .progress("another-capture", "package-operation", progress)
                .is_err()
        );
        assert!(
            exchange
                .progress("capture-operation", "another-package", progress)
                .is_err()
        );
        exchange
            .progress("capture-operation", "package-operation", progress)
            .unwrap();
        assert!(
            exchange
                .progress(
                    "capture-operation",
                    "package-operation",
                    ArchiveProgress {
                        completed: 11,
                        ..progress
                    }
                )
                .is_err()
        );
        assert!(
            exchange
                .progress(
                    "capture-operation",
                    "package-operation",
                    ArchiveProgress {
                        total: Some(100),
                        ..progress
                    }
                )
                .is_err()
        );
        exchange
            .finish("capture-operation", Some("package-operation"))
            .unwrap();
        assert!(
            exchange
                .progress("capture-operation", "package-operation", progress)
                .is_err()
        );
        assert!(
            exchange
                .finish("capture-operation", Some("package-operation"))
                .is_err()
        );
        let bytes = serde_json::to_vec(&Frame::Progress {
            operation: "capture-operation".into(),
            export_operation: "package-operation".into(),
            progress,
        })
        .unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("private secret"));
    }
    #[test]
    fn review_cannot_claim_archive_progress_or_a_package_reply() {
        let mut exchange = Exchange::new("capture", &Command::Review);
        assert!(
            exchange
                .progress(
                    "capture",
                    "package",
                    ArchiveProgress {
                        phase: ArchivePhase::VerifyingSource,
                        completed: 0,
                        total: None
                    }
                )
                .is_err()
        );
        assert!(exchange.finish("capture", Some("package")).is_err());
        exchange.finish("capture", None).unwrap();
    }
    fn original_review_request() -> Request {
        let operation = uuid::Uuid::new_v4().to_string();
        serde_json::from_value(serde_json::json!({
            "schema":WORKER_PROTOCOL,"operation":operation,"custody":null,
            "command":{"action":"review-original","authority":{
                "schema":2,"operation":operation,"export_operation":operation,
                "sealed_reservation":1048576,"omissions":[],"setup_binding":"authenticated-setup",
                "identity_public_key":(vec![7u8;32]),"source_plan_sha256":(vec![8u8;32]),"declaration_sha256":(vec![9u8;32]),
                "source":[9,10,1000],"caller":{"schema":1,"uid":1000,
                    "primary_gid":1001,"supplementary_gids":[1002]}
            }}
        }))
        .unwrap()
    }
    #[test]
    fn original_review_is_distinct_and_refused_by_actual_normal_capture_route() {
        let request = original_review_request();
        request.validate_protocol().unwrap();
        assert_eq!(
            request.require_capture_route().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["command"]["action"], "review-original");
        assert!(encoded.get("source").is_none());
        assert!(encoded["command"].get("password").is_none());
        assert!(encoded["command"].get("output").is_none());
        let mut snapshot_claim = encoded.clone();
        snapshot_claim["custody"] = serde_json::json!({
            "relative":crate::setup::migration_store::NativePath::encode(
                std::path::Path::new(".local/state/tilo/installer/preparations")),
            "stable":[1,448,1000,1001]});
        let snapshot_request = serde_json::from_value::<Request>(snapshot_claim).unwrap();
        assert!(snapshot_request.validate_protocol().is_err());
        let mut mismatch = encoded.clone();
        mismatch["operation"] = uuid::Uuid::new_v4().to_string().into();
        assert!(
            serde_json::from_value::<Request>(mismatch)
                .unwrap()
                .validate_protocol()
                .is_err()
        );
        let mut invented_output = encoded;
        invented_output["command"]["output"] = "/other-volume".into();
        assert!(serde_json::from_value::<Request>(invented_output).is_err());
    }
    #[test]
    fn original_review_cannot_report_export_progress_or_become_an_export_command() {
        let request = original_review_request();
        let mut exchange = Exchange::new(&request.operation, &request.command);
        assert!(
            exchange
                .progress(
                    &request.operation,
                    "invented-output",
                    ArchiveProgress {
                        phase: ArchivePhase::Archiving,
                        completed: 1,
                        total: None,
                    }
                )
                .is_err()
        );
        assert!(
            exchange
                .finish(&request.operation, Some("invented-output"))
                .is_err()
        );
        exchange.finish(&request.operation, None).unwrap();
        let mut attempted = serde_json::to_value(&request).unwrap();
        attempted["command"]["action"] = "export".into();
        attempted["command"]["export_operation"] = uuid::Uuid::new_v4().to_string().into();
        attempted["command"]["setup_binding"] = "binding".into();
        attempted["command"]["password"] = serde_json::json!([1, 2, 3]);
        assert!(serde_json::from_value::<Request>(attempted).is_err());
        let normal = Request {
            schema: WORKER_PROTOCOL,
            operation: uuid::Uuid::new_v4().to_string(),
            custody: None,
            command: Command::Review,
        };
        normal.require_capture_route().unwrap();
    }
}
