//! What running apps are doing for the person: each invocation the action
//! host still follows is a row, reading its state and the progress its app
//! reported, with a row of its own to cancel it where it can be stopped.
//!
//! A job's row shows only what the app reported through `Progress`
//! (bounded text, never logs or outcome evidence), so nothing here reaches
//! into the invocation's output.

use tilo_actions::InvocationUpdate;
use ui::search::FieldWeight;
use ui::search::system::{ResourceKind, rank_fields};

use crate::i18n::{t, t_args};
use crate::model::{Hit, Source, Target};

/// Separates the parts of a row id.
const ID_SEPARATOR: char = '\u{1f}';
/// The most characters of an app's reported phase a row shows.
const MAX_PHASE_CHARS: usize = 128;

/// One invocation the action host follows.
#[derive(Clone, Debug, PartialEq)]
pub struct JobEntry {
    pub app: String,
    pub room: String,
    /// The invocation's id at the host.
    pub invocation: String,
    /// The action that started it, as people read it ("Export PDF").
    pub label: String,
    /// The app's name.
    pub app_name: String,
    /// Its latest state.
    pub update: InvocationUpdate,
    /// Whether the host can ask it to stop.
    pub cancellable: bool,
}

/// The kind jobs are grouped under, in the desktop's language.
#[must_use]
pub fn jobs_kind() -> ResourceKind {
    ResourceKind::Custom(t("search-kind-jobs"))
}

fn running(update: &InvocationUpdate) -> bool {
    matches!(
        update,
        InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Item { .. }
    )
}

/// A job's state as its row reads it, in the desktop's language.
fn state_label(update: &InvocationUpdate) -> String {
    t(match update {
        InvocationUpdate::Accepted { .. } => "search-job-accepted",
        InvocationUpdate::Progress { .. } => "search-job-running",
        InvocationUpdate::Item { .. } => "search-job-streaming",
        InvocationUpdate::Succeeded { .. } => "search-job-succeeded",
        InvocationUpdate::Failed { .. } => "search-job-failed",
        InvocationUpdate::Cancelled { .. } => "search-job-cancelled",
    })
}

/// What a job's row says of it: the progress its app reported, bounded and
/// stripped of control characters, else its state.
#[must_use]
pub fn job_subtitle(update: &InvocationUpdate) -> String {
    let InvocationUpdate::Progress {
        phase,
        completed,
        total,
        ..
    } = update
    else {
        return state_label(update);
    };
    let phase: String = phase
        .chars()
        .take(MAX_PHASE_CHARS)
        .filter(|ch| !ch.is_control())
        .collect();
    let phase = phase.trim();
    if phase.is_empty() {
        return state_label(update);
    }
    let mut args = vec![("phase", phase.into())];
    if let Some(completed) = completed {
        args.push(("completed", completed.to_string().into()));
    }
    if let Some(total) = total {
        args.push(("total", total.to_string().into()));
    }
    let key = match (completed, total) {
        (Some(_), Some(_)) => "search-job-reported-counts",
        (Some(_), None) => "search-job-reported-completed",
        (None, Some(_)) => "search-job-reported-total",
        (None, None) => "search-job-reported-phase",
    };
    t_args(key, &args)
}

pub struct Jobs(pub Vec<JobEntry>);

impl Source for Jobs {
    fn id(&self) -> &'static str {
        "app-jobs"
    }

    /// Unasked, what is still running; asked, the jobs whose action or app
    /// is named, finished ones too.
    fn query(&self, needle: &str) -> Vec<Hit> {
        let browsing = needle.trim().is_empty();
        let mut hits = Vec::new();
        for job in &self.0 {
            let score = if browsing {
                if !running(&job.update) {
                    continue;
                }
                20
            } else {
                let Some(score) = rank_fields(
                    needle,
                    &[
                        (&job.label, FieldWeight::TITLE),
                        (&job.app_name, FieldWeight::SUBTITLE),
                    ],
                ) else {
                    continue;
                };
                score
            };
            let id = format!("{}{ID_SEPARATOR}{}", job.app, job.invocation);
            hits.push(Hit {
                kind: jobs_kind(),
                provider: self.id().into(),
                id: format!("job{ID_SEPARATOR}{id}"),
                title: job.label.clone(),
                subtitle: Some(job_subtitle(&job.update)),
                facts: vec![job.app_name.clone()],
                score,
                icon: None,
                // The app doing it comes forward.
                target: Target::Launch {
                    app: job.app.clone(),
                },
            });
            if job.cancellable && running(&job.update) {
                hits.push(Hit {
                    kind: jobs_kind(),
                    provider: self.id().into(),
                    id: format!("cancel{ID_SEPARATOR}{id}"),
                    title: t_args("search-job-cancel", &[("job", job.label.as_str().into())]),
                    subtitle: Some(t("search-job-cancel-detail")),
                    facts: Vec::new(),
                    score: score.saturating_sub(1).max(1),
                    icon: Some(ui::icons::Icon::X),
                    target: Target::Cancel {
                        app: job.app.clone(),
                        room: job.room.clone(),
                        invocation: job.invocation.clone(),
                    },
                });
            }
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(update: InvocationUpdate) -> JobEntry {
        JobEntry {
            app: "so.tilo.video-editor".into(),
            room: "room".into(),
            invocation: "inv-1".into(),
            label: "Export video".into(),
            app_name: "Video Editor".into(),
            update,
            cancellable: true,
        }
    }

    fn progress(phase: &str, completed: Option<u64>, total: Option<u64>) -> InvocationUpdate {
        InvocationUpdate::Progress {
            invocation: tilo_actions::InvocationId::new(),
            sequence: 1,
            phase: phase.into(),
            completed,
            total,
            message: None,
        }
    }

    #[test]
    fn a_running_job_reads_its_reported_progress_and_offers_to_cancel() {
        let jobs = Jobs(vec![job(progress("Encoding\u{7}", Some(3), Some(10)))]);
        let hits = jobs.query("");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].subtitle.as_deref(), Some("Encoding (3 / 10)"));
        assert_eq!(
            hits[1].target,
            Target::Cancel {
                app: "so.tilo.video-editor".into(),
                room: "room".into(),
                invocation: "inv-1".into()
            }
        );
        assert_eq!(jobs.query("export").len(), 2);
        assert!(jobs.query("photos").is_empty());
    }
}
