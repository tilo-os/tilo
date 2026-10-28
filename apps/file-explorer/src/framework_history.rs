//! Files supplies actual mounted-file custody and a pure immutable preview.
use fs_protocol::files::{
    CHECKED_HISTORY_RESTORE_XATTR, CHECKED_HISTORY_RESULT_XATTR_PREFIX,
    CHECKED_HISTORY_REVIEW_XATTR, CheckedHistoryOutcomeV1, CheckedHistoryRequestV1,
    CheckedHistoryResponseV1, CheckedHistoryReviewResponseV1, FileHistoryReadErrorV1,
    FileHistoryReadQueryV1, FileHistoryReadResponseV1, FileHistoryReadScopeV1,
    FileHistoryReadValueV1, FileRestoreReviewV1, MAX_CHECKED_HISTORY_REQUEST_BYTES,
    MAX_HISTORY_READ_BYTES,
};
use host_fs::attributes::{self, ErrorCode};
use std::fs::File;
use std::time::{Duration, Instant};
use std::{path::Path, sync::Arc};
use ui::history_mode::checked_files::{
    FileHistoricalContent, FileHistoryTarget, FileHistoryTransport, FilePreviewInput,
};

/// A descriptor pins the kernel object. A later path replacement cannot retarget
/// a queued review, immutable read, restore, or exact intent lookup.
pub struct MountedFileHistory {
    file: File,
}
impl MountedFileHistory {
    /// Opens a descriptor on the mounted file at `path`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error from opening `path`, or `Unsupported` on a host
    /// without extended attributes, which cannot carry the projection.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if !attributes::SUPPORTED {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Tilo filesystem projection is unavailable on this host",
            ));
        }
        Ok(Self {
            file: File::open(path)?,
        })
    }
    fn get(&self, name: &str) -> Result<Vec<u8>, std::io::Error> {
        attributes::get_open(&self.file, name)?.ok_or_else(|| ErrorCode::NoData.into_io_error())
    }
}
impl FileHistoryTransport for MountedFileHistory {
    fn read(
        &self,
        query: FileHistoryReadQueryV1,
    ) -> Result<FileHistoryReadValueV1, FileHistoryReadErrorV1> {
        let bytes = self
            .get(&query.xattr_name())
            .map_err(|error| read_error(&error))?;
        if bytes.len() > MAX_HISTORY_READ_BYTES {
            return Err(FileHistoryReadErrorV1::TooLarge);
        }
        let response: FileHistoryReadResponseV1 =
            serde_json::from_slice(&bytes).map_err(|_| FileHistoryReadErrorV1::InvalidRequest)?;
        if response.schema_version != 1 {
            return Err(FileHistoryReadErrorV1::InvalidRequest);
        }
        response.result
    }
    fn preview(
        &self,
        scope: FileHistoryReadScopeV1,
        version: fs_protocol::FileVersionId,
    ) -> Result<Vec<u8>, FileHistoryReadErrorV1> {
        let bytes = self
            .get(
                &FileHistoryReadQueryV1::Preview {
                    frontier: scope.frontier,
                    version_id: version,
                }
                .xattr_name(),
            )
            .map_err(|error| read_error(&error))?;
        if bytes.len() > fs_protocol::MAX_HISTORY_PREVIEW_BYTES {
            return Err(FileHistoryReadErrorV1::TooLarge);
        }
        Ok(bytes)
    }
    fn review(&self) -> Result<FileRestoreReviewV1, CheckedHistoryOutcomeV1> {
        let bytes = self
            .get(CHECKED_HISTORY_REVIEW_XATTR)
            .map_err(|error| outcome_error(&error))?;
        if bytes.len() > MAX_HISTORY_READ_BYTES {
            return Err(CheckedHistoryOutcomeV1::InvalidRequest);
        }
        let response: CheckedHistoryReviewResponseV1 =
            serde_json::from_slice(&bytes).map_err(|_| CheckedHistoryOutcomeV1::InvalidRequest)?;
        if response.schema_version != 1 {
            return Err(CheckedHistoryOutcomeV1::InvalidRequest);
        }
        let review = response.result?;
        review
            .validate()
            .map_err(|_| CheckedHistoryOutcomeV1::InvalidRequest)?;
        Ok(review)
    }
    fn submit(&self, request: CheckedHistoryRequestV1) -> CheckedHistoryOutcomeV1 {
        if request.validate().is_err() {
            return CheckedHistoryOutcomeV1::InvalidRequest;
        }
        let Ok(bytes) = serde_json::to_vec(&request) else {
            return CheckedHistoryOutcomeV1::InvalidRequest;
        };
        if bytes.len() > MAX_CHECKED_HISTORY_REQUEST_BYTES {
            return CheckedHistoryOutcomeV1::InvalidRequest;
        }
        match write_until_hydrated(HYDRATION_WAIT, || {
            attributes::set_open(&self.file, CHECKED_HISTORY_RESTORE_XATTR, &bytes)
        }) {
            Ok(()) => self.result(request.intent_id),
            Err(error) if ErrorCode::of(&error) == Some(ErrorCode::Already) => {
                CheckedHistoryOutcomeV1::Unchanged {
                    intent_id: request.intent_id,
                    review: request.review,
                }
            }
            // Still downloading after the wait: the same request, retried
            // later, picks the download up where it is.
            Err(error) if ErrorCode::of(&error) == Some(ErrorCode::InProgress) => {
                CheckedHistoryOutcomeV1::RecoveryPending {
                    intent_id: request.intent_id,
                }
            }
            Err(error) => outcome_error(&error),
        }
    }
    fn result(&self, intent: uuid::Uuid) -> CheckedHistoryOutcomeV1 {
        let bytes = match self.get(&format!("{CHECKED_HISTORY_RESULT_XATTR_PREFIX}{intent}")) {
            Ok(bytes) => bytes,
            Err(error) => return outcome_error(&error),
        };
        if bytes.len() > MAX_HISTORY_READ_BYTES {
            return CheckedHistoryOutcomeV1::Unavailable;
        }
        let response: CheckedHistoryResponseV1 = match serde_json::from_slice(&bytes) {
            Ok(response) => response,
            Err(_) => return CheckedHistoryOutcomeV1::Unavailable,
        };
        if response.schema_version != 1 {
            return CheckedHistoryOutcomeV1::Unavailable;
        }
        response.outcome
    }
}
/// How long a history request waits for the filesystem to download the
/// content it needs before it reports the request as still pending.
pub(crate) const HYDRATION_WAIT: Duration = Duration::from_secs(10 * 60);

/// Write a history request, repeating it while the filesystem answers that
/// the content it needs is still downloading (`EINPROGRESS`). The filesystem
/// downloads off its request path, so no FUSE worker waits meanwhile; the
/// repeat is the same idempotent request, and it commits once the bytes are
/// local. After `wait` the last `EINPROGRESS` is the answer.
pub(crate) fn write_until_hydrated(
    wait: Duration,
    write: impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let start = Instant::now();
    repeat_while_hydrating(wait, || start.elapsed(), std::thread::sleep, write)
}

/// [`write_until_hydrated`] on a given clock: `elapsed` reads the time since
/// the first write and `sleep` pauses between repeats.
fn repeat_while_hydrating(
    wait: Duration,
    mut elapsed: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
    mut write: impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut pause = Duration::from_millis(20);
    loop {
        match write() {
            Err(error)
                if ErrorCode::of(&error) == Some(ErrorCode::InProgress) && elapsed() < wait =>
            {
                sleep(pause.min(wait.saturating_sub(elapsed())));
                pause = pause.saturating_mul(2).min(Duration::from_millis(500));
            }
            result => return result,
        }
    }
}

fn outcome_error(error: &std::io::Error) -> CheckedHistoryOutcomeV1 {
    match ErrorCode::of(error) {
        Some(ErrorCode::Busy) => CheckedHistoryOutcomeV1::ReviewRequired,
        Some(ErrorCode::Again) => CheckedHistoryOutcomeV1::PendingWrites,
        Some(ErrorCode::NotFound) => CheckedHistoryOutcomeV1::NotVisible,
        Some(ErrorCode::ReadOnlyFilesystem | ErrorCode::AccessDenied | ErrorCode::NotPermitted) => {
            CheckedHistoryOutcomeV1::ReadOnly
        }
        Some(ErrorCode::NotSupported) => CheckedHistoryOutcomeV1::UnsupportedOwner {
            mapping: "The filesystem has no checked restore mapping for this content owner".into(),
        },
        Some(ErrorCode::InvalidArgument) => CheckedHistoryOutcomeV1::InvalidRequest,
        _ => CheckedHistoryOutcomeV1::Unavailable,
    }
}
/// The agent the version `version_id` of the file at `path` records as
/// having caused it, as the version's own signing device recorded it; `None`
/// for a version its person made themselves, or when the history cannot be
/// read.
pub(crate) fn version_attribution(
    path: &Path,
    version_id: fs_protocol::FileVersionId,
) -> Option<fs_protocol::FileAttributionV1> {
    let history = MountedFileHistory::open(path).ok()?;
    let FileHistoryReadValueV1::Page(page) = history
        .read(FileHistoryReadQueryV1::Page { cursor: None })
        .ok()?
    else {
        return None;
    };
    let FileHistoryReadValueV1::Version { attribution, .. } = history
        .read(FileHistoryReadQueryV1::Version {
            frontier: page.scope.frontier,
            version_id,
        })
        .ok()?
    else {
        return None;
    };
    attribution.filter(|attribution| attribution.validate().is_ok())
}

fn read_error(error: &std::io::Error) -> FileHistoryReadErrorV1 {
    match ErrorCode::of(error) {
        Some(ErrorCode::NotFound | ErrorCode::AccessDenied | ErrorCode::NotPermitted) => {
            FileHistoryReadErrorV1::NotVisible
        }
        Some(ErrorCode::Busy) => FileHistoryReadErrorV1::Stale,
        Some(ErrorCode::NoData) => FileHistoryReadErrorV1::MissingVersion,
        Some(ErrorCode::TooBig) => FileHistoryReadErrorV1::TooLarge,
        Some(ErrorCode::InvalidArgument) => FileHistoryReadErrorV1::InvalidRequest,
        _ => FileHistoryReadErrorV1::Unavailable,
    }
}
/// Builds a history target bound to the mounted file at `path`, whose
/// versions `authors` names (people, and agents on their behalf).
///
/// # Errors
///
/// Returns the I/O error from opening `path` (see [`MountedFileHistory::open`]).
pub fn target(
    path: &Path,
    record: fs_protocol::FileRecordId,
    label: String,
    authors: ui::history_source::AuthorDirectory,
) -> std::io::Result<FileHistoryTarget> {
    Ok(FileHistoryTarget {
        binding: uuid::Uuid::new_v4(),
        record,
        label,
        transport: Arc::new(MountedFileHistory::open(path)?),
        authors,
    })
}

struct FilePreview {
    title: String,
    content: String,
    view: ui::history_mode::PreviewView,
    bounds: vello::kurbo::Rect,
}
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature is fixed by `FileHistory::preview`"
)]
pub(crate) fn preview(
    input: FilePreviewInput,
) -> Result<Box<dyn ui::history_mode::HistoryPreview>, String> {
    let content = match input.snapshot.content {
        FileHistoricalContent::Bytes(bytes) => match std::str::from_utf8(&bytes) {
            Ok(text) if !text.contains('\0') => {
                if text.is_empty() {
                    "Empty recorded content".into()
                } else {
                    text.to_owned()
                }
            }
            _ => "This historical media format has no read-only Files preview".into(),
        },
        FileHistoricalContent::NoContent => "This version has no recorded content reference".into(),
        FileHistoricalContent::Unavailable(error) => {
            format!("Historical content unavailable: {error:?}")
        }
    };
    Ok(Box::new(FilePreview {
        title: format!("File revision {}", input.snapshot.version.id.0),
        content,
        view: input.initial_view,
        bounds: vello::kurbo::Rect::ZERO,
    }))
}
impl ui::history_mode::HistoryPreview for FilePreview {
    fn layout(
        &mut self,
        bounds: vello::kurbo::Rect,
        _: &mut ui::text_render::TextRenderer,
        _: &ui::Theme,
    ) -> Result<(), String> {
        self.bounds = bounds;
        Ok(())
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) {
        use vello::kurbo::Affine;
        let sp = &theme.tokens.spacing;
        let x = self.bounds.x0 + f64::from(sp.s6);
        let width = (self.bounds.width() - 2.0 * f64::from(sp.s6)).max(0.0) as f32;
        let mut y = self.bounds.y0 + f64::from(sp.s6) - self.view.scroll.1;
        let title = theme.text(ui::TextRole::Subheading);
        text.draw_style(
            scene,
            &self.title,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((x, y)),
            Some(width),
            &title,
        );
        let body = theme.text(ui::TextRole::Body);
        let (_, line_h) = text.measure_style("Ag", None, &body);
        y += f64::from(text.measure_style("Ag", None, &title).1) + f64::from(sp.s4);
        for line in self.content.lines() {
            if y >= self.bounds.y0 && y < self.bounds.y1 {
                text.draw_style(
                    scene,
                    line,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((x, y)),
                    Some(width),
                    &body,
                );
            }
            y += f64::from(line_h) + f64::from(sp.s1);
        }
    }
    fn navigate(&mut self, command: ui::history_mode::PreviewNavigation) -> bool {
        if let ui::history_mode::PreviewNavigation::Scroll { dy, .. } = command {
            self.view.scroll.1 = (self.view.scroll.1 - dy).max(0.0);
            true
        } else {
            false
        }
    }
    fn view(&self) -> ui::history_mode::PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> ui::history_mode::PreviewAccessibility {
        ui::history_mode::PreviewAccessibility {
            description: format!("{}\n{}", self.title, self.content),
            pages: vec![],
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use fs_protocol::files::CheckedHistoryOperationV1;
    use host_fs::attributes as xattr;
    fn scope() -> FileHistoryReadScopeV1 {
        FileHistoryReadScopeV1 {
            workspace_id: uuid::Uuid::from_u128(1),
            mount_lifetime: uuid::Uuid::from_u128(2),
            record_id: fs_protocol::FileRecordId(uuid::Uuid::from_u128(3)),
            frontier: [4; 32],
        }
    }
    /// A request the filesystem answers with `EINPROGRESS` is repeated until it
    /// settles, and the last `EINPROGRESS` is the answer once the wait is over.
    #[test]
    fn history_writes_repeat_while_content_hydrates() {
        // A clock that moves only when the loop pauses.
        let clock = std::cell::Cell::new(Duration::ZERO);
        let repeat = |wait, write: &mut dyn FnMut() -> std::io::Result<()>| {
            clock.set(Duration::ZERO);
            repeat_while_hydrating(
                wait,
                || clock.get(),
                |pause| clock.set(clock.get() + pause),
                write,
            )
        };
        // --- phase: repeats_until_the_content_is_local ---
        {
            let mut calls = 0_u32;
            let result = repeat(Duration::from_secs(30), &mut || {
                calls = calls.saturating_add(1);
                if calls < 3 {
                    Err(ErrorCode::InProgress.into_io_error())
                } else {
                    Ok(())
                }
            });
            assert!(result.is_ok());
            assert_eq!(calls, 3);
            assert_eq!(
                clock.get(),
                Duration::from_millis(60),
                "paused 20 then 40 ms"
            );
        }
        // --- phase: other_answers_return_at_once ---
        {
            let mut calls = 0_u32;
            let result = repeat(Duration::from_secs(30), &mut || {
                calls = calls.saturating_add(1);
                Err(ErrorCode::Busy.into_io_error())
            });
            assert_eq!(ErrorCode::of(&result.unwrap_err()), Some(ErrorCode::Busy));
            assert_eq!(calls, 1);
            assert_eq!(clock.get(), Duration::ZERO);
        }
        // --- phase: gives_up_after_the_wait ---
        {
            let mut calls = 0_u32;
            let result = repeat(Duration::from_millis(100), &mut || {
                calls = calls.saturating_add(1);
                Err(ErrorCode::InProgress.into_io_error())
            });
            assert_eq!(
                ErrorCode::of(&result.unwrap_err()),
                Some(ErrorCode::InProgress)
            );
            // Writes at 0, 20 and 60 ms pause 20, 40 and the remaining 40 ms;
            // the write at 100 ms is the answer.
            assert_eq!(calls, 4);
            assert_eq!(clock.get(), Duration::from_millis(100));
        }
    }
    /// Checked xattr success reads exact intent without claiming applied, and scoped preview names fit macOS with a descriptor that does not follow path replacement.
    #[test]
    fn checked_history_writes_read_exact_intent_through_a_stable_descriptor() {
        // --- phase: checked_xattr_success_reads_exact_intent_and_does_not_claim_applied ---
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("record");
            std::fs::write(&path, b"current").unwrap();
            let scope = scope();
            let request = CheckedHistoryRequestV1 {
                schema_version: 1,
                intent_id: uuid::Uuid::from_u128(5),
                review: FileRestoreReviewV1 {
                    schema_version: 1,
                    workspace_id: scope.workspace_id,
                    mount_lifetime: scope.mount_lifetime,
                    record_id: scope.record_id,
                    frontier: scope.frontier,
                },
                operation: CheckedHistoryOperationV1::Restore {
                    version_id: fs_protocol::FileVersionId(uuid::Uuid::from_u128(6)),
                },
            };
            let response = CheckedHistoryResponseV1 {
                schema_version: 1,
                outcome: CheckedHistoryOutcomeV1::RecoveryPending {
                    intent_id: request.intent_id,
                },
            };
            xattr::set(
                &path,
                format!("{CHECKED_HISTORY_RESULT_XATTR_PREFIX}{}", request.intent_id),
                &serde_json::to_vec(&response).unwrap(),
            )
            .unwrap();
            let transport = MountedFileHistory::open(&path).unwrap();
            assert_eq!(transport.submit(request), response.outcome);
            assert_eq!(transport.submit(request), response.outcome);
            let actual: CheckedHistoryRequestV1 = serde_json::from_slice(
                &xattr::get(&path, CHECKED_HISTORY_RESTORE_XATTR)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                actual, request,
                "exact retry keeps the original ID, frontier, record and selected version"
            );
            assert_eq!(
                transport.result(uuid::Uuid::from_u128(7)),
                CheckedHistoryOutcomeV1::Unavailable
            );
            assert!(
                xattr::get(&path, fs_protocol::HISTORY_CONTROL_XATTR)
                    .unwrap()
                    .is_none()
            );
        }

        // --- phase: scoped_preview_name_fits_macos_and_descriptor_does_not_follow_path_replacement ---
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("record");
            std::fs::write(&path, b"current bytes").unwrap();
            let version = fs_protocol::FileVersionId(uuid::Uuid::from_u128(9));
            let name = FileHistoryReadQueryV1::Preview {
                frontier: scope().frontier,
                version_id: version,
            }
            .xattr_name();
            assert!(name.len() <= 127);
            xattr::set(&path, &name, b"old immutable bytes").unwrap();
            let transport = MountedFileHistory::open(&path).unwrap();
            std::fs::rename(&path, directory.path().join("old-record")).unwrap();
            std::fs::write(&path, b"replacement current bytes").unwrap();
            xattr::set(&path, &name, b"wrong replacement history").unwrap();
            assert_eq!(
                transport.preview(scope(), version).unwrap(),
                b"old immutable bytes"
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"replacement current bytes");
        }
    }
}
