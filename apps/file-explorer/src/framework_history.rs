//! Files supplies actual mounted-file custody and a pure immutable preview.
use collab_types::files::*;
#[cfg(unix)]
use std::fs::File;
use std::{path::Path, sync::Arc};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryScopeDefinition, checked_files::*,
};
#[cfg(unix)]
use xattr::FileExt;

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "dev.tilo.file-explorer",
    scopes: &[HistoryScopeDefinition {
        key: "selected-file",
        title: "Selected file",
        presentation: HistoryPresentation::CheckedFiles { create: preview },
        content_schema: None,
    }],
};

/// A descriptor pins the kernel object. A later path replacement cannot retarget
/// a queued review, immutable read, restore, or exact intent lookup.
pub struct MountedFileHistory {
    #[cfg(unix)]
    file: File,
}
impl MountedFileHistory {
    #[cfg(unix)]
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
        })
    }
    #[cfg(not(unix))]
    pub fn open(_path: &Path) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Tilo filesystem projection is unavailable on this host",
        ))
    }
    #[cfg(unix)]
    fn get(&self, name: &str) -> Result<Vec<u8>, std::io::Error> {
        self.file
            .get_xattr(name)?
            .ok_or_else(|| std::io::Error::from_raw_os_error(libc::ENODATA))
    }
}
#[cfg(unix)]
impl FileHistoryTransport for MountedFileHistory {
    fn read(
        &self,
        query: FileHistoryReadQueryV1,
    ) -> Result<FileHistoryReadValueV1, FileHistoryReadErrorV1> {
        let bytes = self.get(&query.xattr_name()).map_err(read_error)?;
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
        version: collab_types::FileVersionId,
    ) -> Result<Vec<u8>, FileHistoryReadErrorV1> {
        let bytes = self
            .get(
                &FileHistoryReadQueryV1::Preview {
                    frontier: scope.frontier,
                    version_id: version,
                }
                .xattr_name(),
            )
            .map_err(read_error)?;
        if bytes.len() > collab_types::MAX_HISTORY_PREVIEW_BYTES {
            return Err(FileHistoryReadErrorV1::TooLarge);
        }
        Ok(bytes)
    }
    fn review(&self) -> Result<FileRestoreReviewV1, CheckedHistoryOutcomeV1> {
        let bytes = self
            .get(CHECKED_HISTORY_REVIEW_XATTR)
            .map_err(outcome_error)?;
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
        match self.file.set_xattr(CHECKED_HISTORY_RESTORE_XATTR, &bytes) {
            Ok(()) => self.result(request.intent_id),
            Err(error) if error.raw_os_error() == Some(libc::EALREADY) => {
                CheckedHistoryOutcomeV1::Unchanged {
                    intent_id: request.intent_id,
                    review: request.review,
                }
            }
            Err(error) => outcome_error(error),
        }
    }
    fn result(&self, intent: uuid::Uuid) -> CheckedHistoryOutcomeV1 {
        let bytes = match self.get(&format!("{CHECKED_HISTORY_RESULT_XATTR_PREFIX}{intent}")) {
            Ok(bytes) => bytes,
            Err(error) => return outcome_error(error),
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
#[cfg(not(unix))]
impl FileHistoryTransport for MountedFileHistory {
    fn read(
        &self,
        _query: FileHistoryReadQueryV1,
    ) -> Result<FileHistoryReadValueV1, FileHistoryReadErrorV1> {
        Err(FileHistoryReadErrorV1::Unavailable)
    }
    fn preview(
        &self,
        _scope: FileHistoryReadScopeV1,
        _version: collab_types::FileVersionId,
    ) -> Result<Vec<u8>, FileHistoryReadErrorV1> {
        Err(FileHistoryReadErrorV1::Unavailable)
    }
    fn review(&self) -> Result<FileRestoreReviewV1, CheckedHistoryOutcomeV1> {
        Err(unavailable_projection())
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
        unavailable_projection()
    }
    fn result(&self, _intent: uuid::Uuid) -> CheckedHistoryOutcomeV1 {
        unavailable_projection()
    }
}
#[cfg(not(unix))]
fn unavailable_projection() -> CheckedHistoryOutcomeV1 {
    CheckedHistoryOutcomeV1::UnsupportedOwner {
        mapping: "Tilo filesystem projection is unavailable on this host".into(),
    }
}
#[cfg(unix)]
fn outcome_error(error: std::io::Error) -> CheckedHistoryOutcomeV1 {
    match error.raw_os_error() {
        Some(libc::EBUSY) => CheckedHistoryOutcomeV1::ReviewRequired,
        Some(libc::EAGAIN) => CheckedHistoryOutcomeV1::PendingWrites,
        Some(libc::ENOENT) => CheckedHistoryOutcomeV1::NotVisible,
        Some(libc::EROFS | libc::EACCES | libc::EPERM) => CheckedHistoryOutcomeV1::ReadOnly,
        Some(libc::ENOTSUP) => CheckedHistoryOutcomeV1::UnsupportedOwner {
            mapping: "The filesystem has no checked restore mapping for this content owner".into(),
        },
        Some(libc::EINVAL) => CheckedHistoryOutcomeV1::InvalidRequest,
        _ => CheckedHistoryOutcomeV1::Unavailable,
    }
}
#[cfg(unix)]
fn read_error(error: std::io::Error) -> FileHistoryReadErrorV1 {
    match error.raw_os_error() {
        Some(libc::ENOENT | libc::EACCES | libc::EPERM) => FileHistoryReadErrorV1::NotVisible,
        Some(libc::EBUSY) => FileHistoryReadErrorV1::Stale,
        Some(libc::ENODATA) => FileHistoryReadErrorV1::MissingVersion,
        Some(libc::E2BIG) => FileHistoryReadErrorV1::TooLarge,
        Some(libc::EINVAL) => FileHistoryReadErrorV1::InvalidRequest,
        _ => FileHistoryReadErrorV1::Unavailable,
    }
}
pub fn target(
    path: &Path,
    record: collab_types::FileRecordId,
    label: String,
) -> std::io::Result<FileHistoryTarget> {
    Ok(FileHistoryTarget {
        binding: uuid::Uuid::new_v4(),
        record,
        label,
        transport: Arc::new(MountedFileHistory::open(path)?),
    })
}

struct FilePreview {
    title: String,
    content: String,
    view: ui::history_mode::PreviewView,
    bounds: vello::kurbo::Rect,
}
fn preview(input: FilePreviewInput) -> Result<Box<dyn ui::history_mode::HistoryPreview>, String> {
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
        let x = self.bounds.x0 + 20.0;
        let mut y = self.bounds.y0 + 24.0 - self.view.scroll.1;
        text.draw_text(
            scene,
            &self.title,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((x, y)),
            Some((self.bounds.width() - 40.0).max(0.0) as f32),
        );
        y += 32.0;
        for line in self.content.lines() {
            if y >= self.bounds.y0 && y < self.bounds.y1 {
                text.draw_text(
                    scene,
                    line,
                    theme.tokens.typography.scale.base,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((x, y)),
                    Some((self.bounds.width() - 40.0).max(0.0) as f32),
                );
            }
            y += 24.0;
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn scope() -> FileHistoryReadScopeV1 {
        FileHistoryReadScopeV1 {
            workspace_id: uuid::Uuid::from_u128(1),
            mount_lifetime: uuid::Uuid::from_u128(2),
            record_id: collab_types::FileRecordId(uuid::Uuid::from_u128(3)),
            frontier: [4; 32],
        }
    }
    #[test]
    fn checked_xattr_success_reads_exact_intent_and_does_not_claim_applied() {
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
                version_id: collab_types::FileVersionId(uuid::Uuid::from_u128(6)),
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
            xattr::get(&path, crate::model::HISTORY_CONTROL_XATTR)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn scoped_preview_name_fits_macos_and_descriptor_does_not_follow_path_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("record");
        std::fs::write(&path, b"current bytes").unwrap();
        let version = collab_types::FileVersionId(uuid::Uuid::from_u128(9));
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
