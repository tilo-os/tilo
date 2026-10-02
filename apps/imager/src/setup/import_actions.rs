//! Path-free semantic operations for the local folder-import workflow.

use tilo_actions::{
    ActionSpec, EncodedAction, OperationKind, Resource, ResourcePresentation, ResultSpec,
};

use super::strings;

pub const IMPORT_RESOURCE_ID: &str = "imager.import";
pub const IMPORT_OPERATION_TYPE: &str = "imager.import.operation";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ImportOperation {
    Observe,
    PickSource,
    PickDestination,
    ClearSelection,
    PrepareReview,
    CheckIdentity,
    StartReviewedBatch,
    LaterAll,
    Start { operation_id: String },
    Retry { operation_id: String },
    Cancel { operation_id: String },
    ToggleDetails { operation_id: String },
    NextIssues { operation_id: String },
    FirstIssues { operation_id: String },
    ResolveFolder { folder_id: String },
    PickFolderDestination { folder_id: String },
    SetFolderCopy { folder_id: String },
    SetFolderLater { folder_id: String },
    SetFolderConnected { folder_id: String },
}

impl ImportOperation {
    pub(super) const fn kind(&self) -> OperationKind {
        match self {
            Self::PrepareReview
            | Self::StartReviewedBatch
            | Self::LaterAll
            | Self::Start { .. }
            | Self::Retry { .. }
            | Self::Cancel { .. }
            | Self::ResolveFolder { .. }
            | Self::PickFolderDestination { .. }
            | Self::SetFolderCopy { .. }
            | Self::SetFolderLater { .. }
            | Self::SetFolderConnected { .. } => OperationKind::Effect,
            _ => OperationKind::View,
        }
    }

    pub(super) const fn background(&self) -> bool {
        matches!(
            self,
            Self::PrepareReview
                | Self::StartReviewedBatch
                | Self::Start { .. }
                | Self::Retry { .. }
                | Self::ResolveFolder { .. }
                | Self::PickFolderDestination { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatusPhase {
    Ready,
    ChoosingFolder,
    CheckingIdentity,
    Reviewing,
    Copying,
    Saving,
    LoadingDetails,
    Error,
    Unavailable,
}

impl ImportStatusPhase {
    const fn key(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::ChoosingFolder => "choosing_folder",
            Self::CheckingIdentity => "checking_identity",
            Self::Reviewing => "reviewing",
            Self::Copying => "copying",
            Self::Saving => "saving",
            Self::LoadingDetails => "loading_details",
            Self::Error => "error",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SelectionProjection {
    pub source_selected: bool,
    pub destination_selected: bool,
    pub review_ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct IssueProjection {
    pub number: usize,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent operation capabilities are the public action-availability projection"
)]
pub struct OperationProjection {
    pub identity: String,
    pub state: &'static str,
    pub verified_files: usize,
    pub verified_bytes: u64,
    pub issue_count: usize,
    pub review_needed: bool,
    pub details_open: bool,
    pub details_loading: bool,
    pub issues: Vec<IssueProjection>,
    pub next_issues_available: bool,
    pub first_issues_available: bool,
    pub start_available: bool,
    pub retry_available: bool,
    pub cancel_available: bool,
    pub details_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent folder capabilities are the public action-availability projection"
)]
pub struct FolderProjection {
    pub identity: String,
    pub number: usize,
    pub mode: &'static str,
    pub destination_reviewed: bool,
    pub review_available: bool,
    pub picker_available: bool,
    pub mode_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags describe independent continuation actions rather than one state machine"
)]
pub struct ContinuationProjection {
    pub identity_ready: bool,
    pub identity_check_available: bool,
    pub start_batch_available: bool,
    pub later_all_available: bool,
    pub folders: Vec<FolderProjection>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags describe independent root actions rather than one state machine"
)]
pub struct ImportProjection {
    pub phase: ImportStatusPhase,
    pub status_is_error: bool,
    pub selection: SelectionProjection,
    pub active_operation: Option<String>,
    pub operations: Vec<OperationProjection>,
    pub continuation: Option<ContinuationProjection>,
    pub picker_available: bool,
    pub clear_available: bool,
    pub prepare_review_available: bool,
}

impl ImportProjection {
    #[must_use]
    pub fn value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

#[must_use]
pub fn encode(operation: &ImportOperation) -> EncodedAction {
    EncodedAction::encode_operation(IMPORT_OPERATION_TYPE, operation).unwrap_or_else(|_| {
        EncodedAction {
            type_id: IMPORT_OPERATION_TYPE.to_string(),
            payload: Vec::new(),
        }
    })
}

/// Decode one app-owned import operation without admitting a mutation.
///
/// # Errors
/// Returns a localized error when an import payload is malformed.
pub fn decode(type_id: &str, payload: &[u8]) -> Result<Option<ImportOperation>, String> {
    if type_id != IMPORT_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload).map(Some).map_err(|error| {
        strings::t_args(
            "imager-import-error-invalid-operation",
            &[("error", error.to_string().into())],
        )
    })
}

fn result_spec() -> ResultSpec {
    ResultSpec::Json {
        schema: serde_json::json!({ "type": "object" }),
    }
}

fn immediate(id: &str, label_key: &str, operation: ImportOperation) -> ActionSpec {
    let label = strings::t(label_key);
    let kind = operation.kind();
    let background = operation.background();
    let cancellable = !matches!(&operation, ImportOperation::PrepareReview);
    let mut action =
        ActionSpec::operation(id, label.clone(), kind, IMPORT_OPERATION_TYPE, &operation)
            .unwrap_or_else(|_| {
                ActionSpec::typed_operation(
                    id,
                    label,
                    "",
                    kind,
                    IMPORT_OPERATION_TYPE,
                    Vec::new(),
                    move |_| encode(&operation),
                )
            })
            .returns(result_spec());
    if kind == OperationKind::Effect {
        action.contract.tags.push("tilo.local-device-only".into());
    }
    if background {
        action = action.execution(tilo_actions::ExecutionMode::Background { cancellable });
    }
    action
}

fn available(action: ActionSpec, is_available: bool, code: &str, reason_key: &str) -> ActionSpec {
    if is_available {
        action
    } else {
        action.unavailable(code, strings::t(reason_key))
    }
}

fn operation_resource(operation: &OperationProjection) -> Resource {
    let identity = operation.identity.clone();
    let action = |id, label, operation, allowed, code, reason| {
        available(immediate(id, label, operation), allowed, code, reason)
    };
    Resource::new(
        format!("{IMPORT_RESOURCE_ID}.operation.{identity}"),
        "imager.import.operation",
        strings::t("imager-import-resource-operation"),
    )
    .with_presentation(ResourcePresentation {
        detail: Some(strings::t_args(
            "imager-import-detail-operation",
            &[
                ("state", operation.state.into()),
                ("files", operation.verified_files.into()),
                ("bytes", operation.verified_bytes.into()),
                ("issues", operation.issue_count.into()),
            ],
        )),
        badge: Some(strings::t("imager-import-badge-operation")),
        preview: None,
    })
    .with_action(action(
        "start",
        "imager-import-action-start",
        ImportOperation::Start {
            operation_id: identity.clone(),
        },
        operation.start_available,
        "start_unavailable",
        "imager-import-error-start-unavailable",
    ))
    .with_action(action(
        "retry",
        "imager-import-action-retry",
        ImportOperation::Retry {
            operation_id: identity.clone(),
        },
        operation.retry_available,
        "retry_unavailable",
        "imager-import-error-retry-unavailable",
    ))
    .with_action(action(
        "cancel",
        "imager-import-action-cancel",
        ImportOperation::Cancel {
            operation_id: identity.clone(),
        },
        operation.cancel_available,
        "cancel_unavailable",
        "imager-import-error-cancel-unavailable",
    ))
    .with_action(action(
        "toggle-details",
        "imager-import-action-toggle-details",
        ImportOperation::ToggleDetails {
            operation_id: identity.clone(),
        },
        operation.details_available,
        "details_unavailable",
        "imager-import-error-details-unavailable",
    ))
    .with_action(action(
        "next-issues",
        "imager-import-action-next-issues",
        ImportOperation::NextIssues {
            operation_id: identity,
        },
        operation.next_issues_available,
        "next_issues_unavailable",
        "imager-import-error-next-issues-unavailable",
    ))
    .with_action(action(
        "first-issues",
        "imager-import-action-first-issues",
        ImportOperation::FirstIssues {
            operation_id: operation.identity.clone(),
        },
        operation.first_issues_available,
        "first_issues_unavailable",
        "imager-import-error-first-issues-unavailable",
    ))
}

fn folder_resource(folder: &FolderProjection) -> Resource {
    let identity = folder.identity.clone();
    Resource::new(
        format!("{IMPORT_RESOURCE_ID}.folder.{identity}"),
        "imager.import.folder",
        strings::t_args(
            "imager-import-resource-folder",
            &[("number", folder.number.into())],
        ),
    )
    .with_presentation(ResourcePresentation {
        detail: Some(strings::t_args(
            "imager-import-detail-folder",
            &[
                ("mode", folder.mode.into()),
                ("reviewed", flag(folder.destination_reviewed).into()),
            ],
        )),
        badge: Some(strings::t("imager-import-badge-folder")),
        preview: None,
    })
    .with_action(available(
        immediate(
            "review-destination",
            "imager-import-action-review-destination",
            ImportOperation::ResolveFolder {
                folder_id: identity.clone(),
            },
        ),
        folder.review_available,
        "folder_review_unavailable",
        "imager-import-error-folder-review-unavailable",
    ))
    .with_action(available(
        immediate(
            "choose-destination",
            "imager-import-action-choose-folder-destination",
            ImportOperation::PickFolderDestination {
                folder_id: identity.clone(),
            },
        ),
        folder.picker_available,
        "folder_picker_unavailable",
        "imager-import-error-picker-unavailable",
    ))
    .with_action(available(
        immediate(
            "one-time-copy",
            "imager-import-action-folder-copy",
            ImportOperation::SetFolderCopy {
                folder_id: identity.clone(),
            },
        ),
        folder.mode_available,
        "folder_mode_unavailable",
        "imager-import-error-folder-mode-unavailable",
    ))
    .with_action(available(
        immediate(
            "later",
            "imager-import-action-folder-later",
            ImportOperation::SetFolderLater {
                folder_id: identity.clone(),
            },
        ),
        folder.mode_available,
        "folder_mode_unavailable",
        "imager-import-error-folder-mode-unavailable",
    ))
    .with_action(available(
        immediate(
            "connected",
            "imager-import-action-folder-connected",
            ImportOperation::SetFolderConnected {
                folder_id: identity,
            },
        ),
        false,
        "connected_unavailable",
        "imager-import-error-connected-unavailable",
    ))
}

/// Build the bounded, path-free resource projection for the import workflow.
#[must_use]
pub fn resource(state: &ImportProjection) -> Resource {
    let mut root = Resource::new(
        IMPORT_RESOURCE_ID,
        "imager.import",
        strings::t("imager-import-resource-root"),
    )
    .with_presentation(ResourcePresentation {
        detail: Some(strings::t_args(
            "imager-import-detail-root",
            &[
                ("phase", state.phase.key().into()),
                ("operations", state.operations.len().into()),
                ("error", flag(state.status_is_error).into()),
            ],
        )),
        badge: Some(strings::t("imager-import-badge-root")),
        preview: None,
    })
    .with_action(immediate(
        "read-status",
        "imager-import-action-read-status",
        ImportOperation::Observe,
    ))
    .with_action(available(
        immediate(
            "choose-source",
            "imager-import-action-choose-source",
            ImportOperation::PickSource,
        ),
        state.picker_available,
        "picker_unavailable",
        "imager-import-error-picker-unavailable",
    ))
    .with_action(available(
        immediate(
            "choose-destination",
            "imager-import-action-choose-destination",
            ImportOperation::PickDestination,
        ),
        state.picker_available,
        "picker_unavailable",
        "imager-import-error-picker-unavailable",
    ))
    .with_action(available(
        immediate(
            "clear-selection",
            "imager-import-action-clear-selection",
            ImportOperation::ClearSelection,
        ),
        state.clear_available,
        "selection_empty",
        "imager-import-error-selection-empty",
    ))
    .with_action(available(
        immediate(
            "prepare-review",
            "imager-import-action-prepare-review",
            ImportOperation::PrepareReview,
        ),
        state.prepare_review_available,
        "review_unavailable",
        "imager-import-error-review-unavailable",
    ));

    if let Some(continuation) = &state.continuation {
        root = root
            .with_action(available(
                immediate(
                    "check-identity",
                    "imager-import-action-check-identity",
                    ImportOperation::CheckIdentity,
                ),
                continuation.identity_check_available,
                "identity_check_unavailable",
                "imager-import-error-identity-check-unavailable",
            ))
            .with_action(available(
                immediate(
                    "start-reviewed-folders",
                    "imager-import-action-start-reviewed-folders",
                    ImportOperation::StartReviewedBatch,
                ),
                continuation.start_batch_available,
                "batch_unavailable",
                "imager-import-error-batch-unavailable",
            ))
            .with_action(available(
                immediate(
                    "later-all",
                    "imager-import-action-later-all",
                    ImportOperation::LaterAll,
                ),
                continuation.later_all_available,
                "later_all_unavailable",
                "imager-import-error-later-all-unavailable",
            ));
        root = root.with_children(continuation.folders.iter().map(folder_resource));
    }
    root.with_children(state.operations.iter().map(operation_resource))
}

const fn flag(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// Folder import as a resource. Choices stay local view operations; the palette
/// is still [`tilo_resource::publish`].
pub struct ImportGuide;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ImportDocumentAction {}

impl ui::tilo_actions::Action for ImportDocumentAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ui::tilo_actions::ActionError> {
        match self {}
    }
}

impl ui::tilo_resource::Resource for ImportGuide {
    const KIND: &'static str = "imager.import";
    const LABEL: &'static str = "Folder import";
    const ACTION_TYPE: &'static str = "imager.import.document";

    type State = ImportProjection;
    type Action = ImportDocumentAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        registry.register::<ImportDocumentAction>(Self::ACTION_TYPE);
    }

    fn read(
        _doc: &ui::tilo_resource::Doc,
    ) -> Result<ImportProjection, ui::tilo_resource::ModelError> {
        Ok(ImportProjection {
            phase: ImportStatusPhase::Ready,
            status_is_error: false,
            selection: SelectionProjection {
                source_selected: false,
                destination_selected: false,
                review_ready: false,
            },
            active_operation: None,
            operations: Vec::new(),
            continuation: None,
            picker_available: false,
            clear_available: false,
            prepare_review_available: false,
        })
    }

    fn bound_actions(state: &ImportProjection) -> Vec<ActionSpec> {
        resource(state).actions
    }

    fn children(state: &ImportProjection) -> Vec<Resource> {
        resource(state).children
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_actions_are_local_device_effects_with_background_completion() {
        let start = immediate(
            "start",
            "imager-import-action-start",
            ImportOperation::Start {
                operation_id: "copy-1".into(),
            },
        );
        assert_eq!(start.kind, OperationKind::Effect);
        assert!(
            start
                .contract
                .tags
                .iter()
                .any(|tag| tag == "tilo.local-device-only")
        );
        assert_eq!(
            start.contract.execution,
            tilo_actions::ExecutionMode::Background { cancellable: true }
        );

        let mode = immediate(
            "one-time-copy",
            "imager-import-action-folder-copy",
            ImportOperation::SetFolderCopy {
                folder_id: "folder-1".into(),
            },
        );
        assert_eq!(mode.kind, OperationKind::Effect);
        assert!(
            mode.contract
                .tags
                .iter()
                .any(|tag| tag == "tilo.local-device-only")
        );

        let review = immediate(
            "prepare-review",
            "imager-import-action-prepare-review",
            ImportOperation::PrepareReview,
        );
        assert_eq!(review.kind, OperationKind::Effect);
        assert_eq!(
            review.contract.execution,
            tilo_actions::ExecutionMode::Background { cancellable: false }
        );
        assert!(
            review
                .contract
                .tags
                .iter()
                .any(|tag| tag == "tilo.local-device-only")
        );

        let resolve = immediate(
            "review-destination",
            "imager-import-action-review-destination",
            ImportOperation::ResolveFolder {
                folder_id: "folder-1".into(),
            },
        );
        assert_eq!(resolve.kind, OperationKind::Effect);
        assert_eq!(
            resolve.contract.execution,
            tilo_actions::ExecutionMode::Background { cancellable: true }
        );
        let destination = immediate(
            "choose-destination",
            "imager-import-action-choose-folder-destination",
            ImportOperation::PickFolderDestination {
                folder_id: "folder-1".into(),
            },
        );
        assert_eq!(destination.kind, OperationKind::Effect);
        assert!(matches!(
            destination.contract.execution,
            tilo_actions::ExecutionMode::Background { .. }
        ));

        let observe = immediate(
            "read-status",
            "imager-import-action-read-status",
            ImportOperation::Observe,
        );
        assert_eq!(observe.kind, OperationKind::View);
        assert!(observe.contract.tags.is_empty());
    }

    #[test]
    fn import_action_presentation_uses_the_active_host_locale_catalog() {
        ui::I18nManager::install(
            "en-US",
            "fr",
            &[
                ("en-US", "test-fallback = fallback"),
                (
                    "fr",
                    r"
imager-import-resource-root = FR imports
imager-import-badge-root = FR badge
imager-import-detail-root = FR phase { $phase } { $operations } { $error }
imager-import-action-read-status = FR lire
imager-import-action-first-issues = FR première page
imager-import-action-choose-source = FR source
imager-import-action-choose-destination = FR destination
imager-import-action-clear-selection = FR clear
imager-import-action-prepare-review = FR review
",
                ),
            ],
        )
        .unwrap();
        super::super::strings::ensure();
        let projection = ImportProjection {
            phase: ImportStatusPhase::Ready,
            status_is_error: false,
            selection: SelectionProjection {
                source_selected: false,
                destination_selected: false,
                review_ready: false,
            },
            active_operation: None,
            operations: vec![OperationProjection {
                identity: "operation-1".into(),
                state: "incomplete",
                verified_files: 1,
                verified_bytes: 1,
                issue_count: 40,
                review_needed: false,
                details_open: true,
                details_loading: false,
                issues: Vec::new(),
                next_issues_available: false,
                first_issues_available: true,
                start_available: false,
                retry_available: false,
                cancel_available: false,
                details_available: true,
            }],
            continuation: None,
            picker_available: true,
            clear_available: false,
            prepare_review_available: false,
        };

        let resource = resource(&projection);

        assert_eq!(resource.label, "FR imports");
        assert_eq!(
            resource
                .actions
                .iter()
                .find(|action| action.id == "read-status")
                .unwrap()
                .label,
            "FR lire"
        );
        assert_eq!(
            resource
                .actions
                .iter()
                .find(|action| action.id == "choose-source")
                .unwrap()
                .label,
            "FR source"
        );
        assert_eq!(
            resource.children[0]
                .actions
                .iter()
                .find(|action| action.id == "first-issues")
                .unwrap()
                .label,
            "FR première page"
        );
        assert_eq!(ui::I18nManager::current_locale(), "fr");
    }
}
