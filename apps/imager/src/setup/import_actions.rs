//! What folder imports offer the palette, agents and scripts: their effects
//! and the live rows of the imports on this computer.
//!
//! Imports copy private folders on this device. They are effects of the
//! imager resource, never document actions, and their published rows and
//! receipts stay path-free.

use tilo_actions::{ActionSpec, ResourcePresentation};
use ui::framework::{EffectStatus, LiveRow};
use ui::tilo_resource::{self, ActionRef, Effect};

use super::strings;

/// The live row of the folder imports, beneath the imager resource.
pub const IMPORT_ROW_ID: &str = "imager.import";

/// Folder imports' effects. Each answers with the imports' path-free status
/// ([`ImportProjection`]) as it stands afterwards; a job answers when its
/// work settled.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "imager.import", device_only)]
pub enum ImportEffect {
    /// Read current import status.
    ///
    /// For the person at this computer it includes the chosen folders.
    #[effect(returns, kind = "read")]
    ReadStatus,
    /// Open Accounts to explicitly connect providers after trusted identity setup.
    #[effect(view, returns, kind = "read")]
    OpenAccounts,
    /// Open Store to choose apps; installation remains a separate action.
    #[effect(view, returns, kind = "read")]
    OpenStore,
    /// Choose source folder.
    #[effect(returns, kind = "read")]
    ChooseSource,
    /// Choose destination folder.
    #[effect(returns, kind = "read")]
    ChooseDestination,
    /// Clear folder selection.
    #[effect(returns, kind = "read")]
    ClearSelection,
    /// Prepare copy review.
    #[effect(job)]
    PrepareReview,
    /// Check identity setup.
    #[effect(returns, kind = "read")]
    CheckIdentity,
    /// Copy reviewed folders.
    #[effect(job, cancellable)]
    StartReviewedFolders,
    /// Decide later for all folders.
    #[effect(returns)]
    LaterAll,
    /// Start copy.
    #[effect(job, cancellable)]
    StartCopy {
        #[param("Id of the reviewed copy")]
        operation_id: String,
    },
    /// Continue copy.
    #[effect(job, cancellable)]
    RetryCopy {
        #[param("Id of the copy to continue")]
        operation_id: String,
    },
    /// Request to stop copy.
    #[effect(returns)]
    CancelCopy {
        #[param("Id of the running copy")]
        operation_id: String,
    },
    /// Show or hide copy details.
    #[effect(returns, kind = "read")]
    ToggleDetails {
        #[param("Id of the copy")]
        operation_id: String,
    },
    /// Load next issue page.
    #[effect(returns, kind = "read")]
    NextIssues {
        #[param("Id of the copy")]
        operation_id: String,
    },
    /// Return to first issue page.
    #[effect(returns, kind = "read")]
    FirstIssues {
        #[param("Id of the copy")]
        operation_id: String,
    },
    /// Review destination.
    #[effect(job, cancellable)]
    ReviewFolderDestination {
        #[param("Id of the saved folder choice")]
        folder_id: String,
    },
    /// Choose another destination.
    #[effect(job, cancellable)]
    ChooseFolderDestination {
        #[param("Id of the saved folder choice")]
        folder_id: String,
    },
    /// Use one-time Copy.
    #[effect(returns)]
    SetFolderCopy {
        #[param("Id of the saved folder choice")]
        folder_id: String,
    },
    /// Decide later.
    #[effect(returns)]
    SetFolderLater {
        #[param("Id of the saved folder choice")]
        folder_id: String,
    },
    /// Use Connected folders.
    #[effect(returns)]
    SetFolderConnected {
        #[param("Id of the saved folder choice")]
        folder_id: String,
    },
}

impl ImportEffect {
    /// Whether the effect answers once its background work settled.
    pub(super) const fn background(&self) -> bool {
        matches!(
            self,
            Self::PrepareReview
                | Self::StartReviewedFolders
                | Self::StartCopy { .. }
                | Self::RetryCopy { .. }
                | Self::ReviewFolderDestination { .. }
                | Self::ChooseFolderDestination { .. }
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
    pub journey_available: bool,
}

impl ImportProjection {
    #[must_use]
    pub fn value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

/// Each effect's label, as a message key.
const LABELS: &[(&str, &str)] = &[
    ("read-status", "imager-import-action-read-status"),
    ("open-accounts", "imager-journey-accounts"),
    ("open-store", "imager-journey-store"),
    ("choose-source", "imager-import-action-choose-source"),
    (
        "choose-destination",
        "imager-import-action-choose-destination",
    ),
    ("clear-selection", "imager-import-action-clear-selection"),
    ("prepare-review", "imager-import-action-prepare-review"),
    ("check-identity", "imager-import-action-check-identity"),
    (
        "start-reviewed-folders",
        "imager-import-action-start-reviewed-folders",
    ),
    ("later-all", "imager-import-action-later-all"),
    ("start-copy", "imager-import-action-start"),
    ("retry-copy", "imager-import-action-retry"),
    ("cancel-copy", "imager-import-action-cancel"),
    ("toggle-details", "imager-import-action-toggle-details"),
    ("next-issues", "imager-import-action-next-issues"),
    ("first-issues", "imager-import-action-first-issues"),
    (
        "review-folder-destination",
        "imager-import-action-review-destination",
    ),
    (
        "choose-folder-destination",
        "imager-import-action-choose-folder-destination",
    ),
    ("set-folder-copy", "imager-import-action-folder-copy"),
    ("set-folder-later", "imager-import-action-folder-later"),
    (
        "set-folder-connected",
        "imager-import-action-folder-connected",
    ),
];

/// The label of the effect `id` in the person's language.
fn label(id: &str) -> String {
    LABELS
        .iter()
        .find(|(effect, _)| *effect == id)
        .map_or_else(|| id.to_owned(), |(_, key)| strings::t(key))
}

/// An effect that cannot run now: `code` names why for programs, `reason`
/// (a message key) for people.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blocked {
    pub id: &'static str,
    pub code: &'static str,
    pub reason: &'static str,
}

/// The import effects that cannot run in `state`, with why.
#[must_use]
pub fn blocked(state: &ImportProjection) -> Vec<Blocked> {
    let continuation = state.continuation.as_ref();
    let journey_reason = if continuation.is_some_and(|c| c.identity_ready) {
        "imager-journey-navigation-pending"
    } else {
        "imager-journey-identity-required"
    };
    [
        (
            ImportEffect::OPEN_ACCOUNTS.id,
            state.journey_available,
            "journey_unavailable",
            journey_reason,
        ),
        (
            ImportEffect::OPEN_STORE.id,
            state.journey_available,
            "journey_unavailable",
            journey_reason,
        ),
        (
            ImportEffect::CHOOSE_SOURCE.id,
            state.picker_available,
            "picker_unavailable",
            "imager-import-error-picker-unavailable",
        ),
        (
            ImportEffect::CHOOSE_DESTINATION.id,
            state.picker_available,
            "picker_unavailable",
            "imager-import-error-picker-unavailable",
        ),
        (
            ImportEffect::CLEAR_SELECTION.id,
            state.clear_available,
            "selection_empty",
            "imager-import-error-selection-empty",
        ),
        (
            ImportEffect::PREPARE_REVIEW.id,
            state.prepare_review_available,
            "review_unavailable",
            "imager-import-error-review-unavailable",
        ),
        (
            ImportEffect::CHECK_IDENTITY.id,
            continuation.is_some_and(|continuation| continuation.identity_check_available),
            "identity_check_unavailable",
            "imager-import-error-identity-check-unavailable",
        ),
        (
            ImportEffect::START_REVIEWED_FOLDERS.id,
            continuation.is_some_and(|continuation| continuation.start_batch_available),
            "batch_unavailable",
            "imager-import-error-batch-unavailable",
        ),
        (
            ImportEffect::LATER_ALL.id,
            continuation.is_some_and(|continuation| continuation.later_all_available),
            "later_all_unavailable",
            "imager-import-error-later-all-unavailable",
        ),
        (
            ImportEffect::SET_FOLDER_CONNECTED.id,
            false,
            "connected_unavailable",
            "imager-import-error-connected-unavailable",
        ),
    ]
    .into_iter()
    .filter(|(_, open, ..)| !open)
    .map(|(id, _, code, reason)| Blocked { id, code, reason })
    .collect()
}

/// The import effects as callers see them now: in the person's language,
/// with what cannot run.
#[must_use]
pub fn statuses(state: &ImportProjection) -> Vec<EffectStatus> {
    let effect = |id| ActionRef::<ImportEffect, ()>::new(id, &[]);
    LABELS
        .iter()
        .map(|(id, key)| EffectStatus::labelled(effect(id), strings::t(key)))
        .chain(blocked(state).into_iter().map(|blocked| {
            EffectStatus::unavailable(effect(blocked.id), blocked.code, strings::t(blocked.reason))
        }))
        .collect()
}

/// `effect` bound to its row, offered when `available`.
fn offer(effect: &ImportEffect, available: bool, code: &str, reason: &str) -> ActionSpec {
    let mut spec = effect.bound();
    spec.label = label(effect.id());
    if available {
        spec
    } else {
        spec.unavailable(code, strings::t(reason))
    }
}

fn operation_row(operation: &OperationProjection) -> LiveRow {
    let operation_id = || operation.identity.clone();
    LiveRow::new(
        format!("{IMPORT_ROW_ID}.operation.{}", operation.identity),
        "imager.import.operation",
        strings::t("imager-import-resource-operation"),
    )
    .presentation(ResourcePresentation {
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
    .operation(offer(
        &ImportEffect::StartCopy {
            operation_id: operation_id(),
        },
        operation.start_available,
        "start_unavailable",
        "imager-import-error-start-unavailable",
    ))
    .operation(offer(
        &ImportEffect::RetryCopy {
            operation_id: operation_id(),
        },
        operation.retry_available,
        "retry_unavailable",
        "imager-import-error-retry-unavailable",
    ))
    .operation(offer(
        &ImportEffect::CancelCopy {
            operation_id: operation_id(),
        },
        operation.cancel_available,
        "cancel_unavailable",
        "imager-import-error-cancel-unavailable",
    ))
    .operation(offer(
        &ImportEffect::ToggleDetails {
            operation_id: operation_id(),
        },
        operation.details_available,
        "details_unavailable",
        "imager-import-error-details-unavailable",
    ))
    .operation(offer(
        &ImportEffect::NextIssues {
            operation_id: operation_id(),
        },
        operation.next_issues_available,
        "next_issues_unavailable",
        "imager-import-error-next-issues-unavailable",
    ))
    .operation(offer(
        &ImportEffect::FirstIssues {
            operation_id: operation_id(),
        },
        operation.first_issues_available,
        "first_issues_unavailable",
        "imager-import-error-first-issues-unavailable",
    ))
}

fn folder_row(folder: &FolderProjection) -> LiveRow {
    let folder_id = || folder.identity.clone();
    LiveRow::new(
        format!("{IMPORT_ROW_ID}.folder.{}", folder.identity),
        "imager.import.folder",
        strings::t_args(
            "imager-import-resource-folder",
            &[("number", folder.number.into())],
        ),
    )
    .presentation(ResourcePresentation {
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
    .operation(offer(
        &ImportEffect::ReviewFolderDestination {
            folder_id: folder_id(),
        },
        folder.review_available,
        "folder_review_unavailable",
        "imager-import-error-folder-review-unavailable",
    ))
    .operation(offer(
        &ImportEffect::ChooseFolderDestination {
            folder_id: folder_id(),
        },
        folder.picker_available,
        "folder_picker_unavailable",
        "imager-import-error-picker-unavailable",
    ))
    .operation(offer(
        &ImportEffect::SetFolderCopy {
            folder_id: folder_id(),
        },
        folder.mode_available,
        "folder_mode_unavailable",
        "imager-import-error-folder-mode-unavailable",
    ))
    .operation(offer(
        &ImportEffect::SetFolderLater {
            folder_id: folder_id(),
        },
        folder.mode_available,
        "folder_mode_unavailable",
        "imager-import-error-folder-mode-unavailable",
    ))
    .operation(offer(
        &ImportEffect::SetFolderConnected {
            folder_id: folder_id(),
        },
        false,
        "connected_unavailable",
        "imager-import-error-connected-unavailable",
    ))
}

/// The imports on this computer as one live row beneath the imager
/// resource: the saved folder choices, then each copy.
#[must_use]
pub fn row(state: &ImportProjection) -> LiveRow {
    let mut row = LiveRow::new(
        IMPORT_ROW_ID,
        "imager.import",
        strings::t("imager-import-resource-root"),
    )
    .presentation(ResourcePresentation {
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
    });
    let ready = state.journey_available;
    let reason = if state
        .continuation
        .as_ref()
        .is_some_and(|c| c.identity_ready)
    {
        "imager-journey-navigation-pending"
    } else {
        "imager-journey-identity-required"
    };
    row = row
        .operation(offer(
            &ImportEffect::OpenAccounts,
            ready,
            "journey_unavailable",
            reason,
        ))
        .operation(offer(
            &ImportEffect::OpenStore,
            ready,
            "journey_unavailable",
            reason,
        ));
    if let Some(continuation) = &state.continuation {
        for folder in &continuation.folders {
            row = row.row(folder_row(folder));
        }
    }
    for operation in &state.operations {
        row = row.row(operation_row(operation));
    }
    row
}

const fn flag(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{ExecutionMode, OperationKind};

    fn declared(id: &str) -> ActionSpec {
        ImportEffect::specs()
            .into_iter()
            .find(|spec| spec.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
    }

    #[test]
    fn copies_and_reviews_are_background_effects_answered_when_settled() {
        for (id, cancellable) in [
            ("start-copy", true),
            ("retry-copy", true),
            ("start-reviewed-folders", true),
            ("review-folder-destination", true),
            ("choose-folder-destination", true),
            ("prepare-review", false),
        ] {
            let spec = declared(id);
            assert_eq!(spec.kind, OperationKind::Effect, "{id}");
            assert_eq!(
                spec.contract.execution,
                ExecutionMode::Background { cancellable },
                "{id}"
            );
        }
        for effect in [
            ImportEffect::PrepareReview,
            ImportEffect::StartCopy {
                operation_id: "copy-1".into(),
            },
            ImportEffect::ReviewFolderDestination {
                folder_id: "folder-1".into(),
            },
        ] {
            let job = ImportEffect::declarations()
                .into_iter()
                .any(|decl| decl.id == effect.id() && decl.is_job());
            assert_eq!(effect.background(), job, "{}", effect.id());
        }
        assert_eq!(
            declared("read-status").contract.execution,
            ExecutionMode::Immediate
        );
        assert!(!ImportEffect::ReadStatus.background());
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
            journey_available: false,
        };

        let published = row(&projection).into_resource();

        assert_eq!(published.label, "FR imports");
        assert_eq!(
            published.children[0]
                .actions
                .iter()
                .find(|action| action.id == "first-issues")
                .unwrap()
                .label,
            "FR première page"
        );
        let statuses = statuses(&projection);
        assert!(statuses.contains(&EffectStatus::labelled(
            ImportEffect::READ_STATUS,
            "FR lire"
        )));
        assert!(statuses.contains(&EffectStatus::labelled(
            ImportEffect::CHOOSE_SOURCE,
            "FR source"
        )));
        assert_eq!(ui::I18nManager::current_locale(), "fr");
    }
}
