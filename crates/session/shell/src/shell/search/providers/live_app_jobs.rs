//! How a running invocation reads as a row: its state, the label it shows,
//! the progress it may report, and the ids a row's actions are addressed by.
//!
//! A job's row shows only what the app reported through `Progress` — bounded
//! text, never logs or outcome evidence — so nothing here reaches into the
//! invocation's output.

use crate::shell::i18n::{t, t_args};

/// The separator a row id and a job id are joined with ([`super::PROVIDER_ID`]'s
/// own ids use it too).
pub(crate) const ID_SEPARATOR: char = '\u{1f}';

pub(crate) const JOB_UNAVAILABLE_MESSAGE: &str = "invocation is unavailable or not visible; this does not establish the original action's outcome";

pub(crate) fn job_control_target(id: &str) -> Option<(u64, bool)> {
    let (kind, number) = id.split_once(ID_SEPARATOR)?;
    let cancel = match kind {
        "job" => false,
        "job-cancel" => true,
        _ => return None,
    };
    Some((number.parse().ok()?, cancel))
}

pub(crate) fn job_state(update: &tilo_actions::InvocationUpdate) -> &'static str {
    match update {
        tilo_actions::InvocationUpdate::Accepted { .. } => "accepted",
        tilo_actions::InvocationUpdate::Progress { .. } => "running",
        tilo_actions::InvocationUpdate::Item { .. } => "streaming",
        tilo_actions::InvocationUpdate::Succeeded { .. } => "succeeded",
        tilo_actions::InvocationUpdate::Failed { .. } => "failed",
        tilo_actions::InvocationUpdate::Cancelled { .. } => "cancelled",
    }
}

/// A [`job_state`] as the job's row reads it, in the desktop's language.
pub(crate) fn job_state_label(state: &str) -> String {
    match state {
        "accepted" => t("search-app-job-accepted"),
        "running" => t("search-app-job-running"),
        "streaming" => t("search-app-job-streaming"),
        "succeeded" => t("search-app-job-succeeded"),
        "failed" => t("search-app-job-failed"),
        _ => t("search-app-job-cancelled"),
    }
}

/// Display only bounded application-reported progress, never outcome evidence or logs.
pub(crate) fn job_subtitle(update: &tilo_actions::InvocationUpdate) -> String {
    let tilo_actions::InvocationUpdate::Progress {
        phase,
        completed,
        total,
        ..
    } = update
    else {
        return job_state_label(job_state(update));
    };
    let phase: String = phase
        .chars()
        .take(128)
        .filter(|ch| !ch.is_control())
        .collect();
    let phase = phase.trim();
    if phase.is_empty() {
        return job_state_label("running");
    }
    let mut args = vec![("phase", phase.into())];
    if let Some(completed) = completed {
        args.push(("completed", completed.to_string().into()));
    }
    if let Some(total) = total {
        args.push(("total", total.to_string().into()));
    }
    let key = match (completed, total) {
        (Some(_), Some(_)) => "search-app-job-reported-counts",
        (Some(_), None) => "search-app-job-reported-completed",
        (None, Some(_)) => "search-app-job-reported-total",
        (None, None) => "search-app-job-reported-phase",
    };
    t_args(key, &args)
}

pub fn job_status_id(id: u64) -> String {
    format!("job{ID_SEPARATOR}{id}")
}

pub fn job_cancel_id(id: u64) -> String {
    format!("job-cancel{ID_SEPARATOR}{id}")
}
