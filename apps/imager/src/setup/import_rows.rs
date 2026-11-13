//! What the folder import's rows say: one issue, one path, one copy state.

use std::path::{Path, PathBuf};

use ui::icons::Icon;
use ui::widget::ListRow;

use super::desktop_continuation::ChoiceMode;
use crate::resource::CATALOG;
use imager_backend::migration_store::{OperationRecord, OperationState};

pub(super) fn issue_row(path: &Path, reason: &str) -> ListRow {
    let shown = if path.as_os_str().is_empty() {
        CATALOG.t("imager-folder-itself")
    } else {
        path.display().to_string()
    };
    ListRow::new(format!("import.issue.{}", path.display()), shown)
        .icon(Icon::TriangleAlert)
        .secondary(reason.to_string())
}

pub(super) fn items_phrase(count: u64) -> String {
    if count == 1 {
        "1 item".to_string()
    } else {
        format!("{count} items")
    }
}

thread_local! {
    static SHOT_PATH_ALIAS: std::cell::RefCell<Option<(PathBuf, PathBuf)>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn set_shot_path_alias(from: PathBuf, to: PathBuf) {
    SHOT_PATH_ALIAS.with(|alias| *alias.borrow_mut() = Some((from, to)));
}

pub(super) fn shown_path(path: &Path) -> String {
    SHOT_PATH_ALIAS.with(|alias| {
        alias
            .borrow()
            .as_ref()
            .and_then(|(from, to)| path.strip_prefix(from).ok().map(|rest| to.join(rest)))
            .unwrap_or_else(|| path.to_path_buf())
            .display()
            .to_string()
    })
}

pub(super) fn operation_details(
    operation: &OperationRecord,
) -> (Vec<(PathBuf, String)>, Vec<String>) {
    let issues = operation.issues().unwrap_or_default();
    let mut notes = operation.metadata_limitations().to_vec();
    if let Some(failure) = operation.failure() {
        notes.push(super::strings::t_args(
            "imager-import-error-copy-cannot-continue",
            &[("failure", failure.into())],
        ));
    }
    (issues, notes)
}

pub(super) fn operation_state_label(state: OperationState) -> String {
    super::strings::t(operation_state_message(state))
}

pub(super) fn operation_state_message(state: OperationState) -> &'static str {
    match state {
        OperationState::AwaitingReview => "imager-import-state-awaiting-review",
        OperationState::Ready => "imager-import-state-ready",
        OperationState::Running => "imager-import-state-running",
        OperationState::Interrupted => "imager-import-state-interrupted",
        OperationState::Cancelled => "imager-import-state-cancelled",
        OperationState::Incomplete => "imager-import-state-incomplete",
        OperationState::Failed => "imager-import-state-failed",
        OperationState::Complete => "imager-import-state-complete",
    }
}

pub(super) const fn operation_state_key(state: OperationState) -> &'static str {
    match state {
        OperationState::AwaitingReview => "awaiting_review",
        OperationState::Ready => "ready",
        OperationState::Running => "running",
        OperationState::Interrupted => "interrupted",
        OperationState::Cancelled => "cancelled",
        OperationState::Incomplete => "incomplete",
        OperationState::Failed => "failed",
        OperationState::Complete => "complete",
    }
}

pub(super) const fn choice_mode_key(mode: ChoiceMode) -> &'static str {
    match mode {
        ChoiceMode::Copy => "copy",
        ChoiceMode::Connected => "connected",
        ChoiceMode::Later => "later",
    }
}

pub(super) fn state_summary(state: OperationState) -> String {
    let message = match state {
        OperationState::AwaitingReview => "installer-copy-awaiting-review",
        OperationState::Ready => "installer-copy-ready",
        OperationState::Running => "installer-copy-running",
        OperationState::Interrupted => "installer-copy-interrupted",
        OperationState::Cancelled => "installer-copy-stopped",
        OperationState::Incomplete => "installer-copy-incomplete",
        OperationState::Failed => "installer-copy-failed",
        OperationState::Complete => "installer-copy-complete",
    };
    crate::resource::CATALOG.t(message)
}
