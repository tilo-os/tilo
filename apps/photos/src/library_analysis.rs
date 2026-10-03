//! Device-local orchestration state for whole-library image analysis.
//!
//! The analyzers themselves live in [`crate::analysis`].  This module only
//! keeps the bounded, inspectable job state used by the Photos UI and semantic
//! actions.  It deliberately stores stable photo ids rather than pixels or
//! model output: original bytes are fetched immediately before one worker job
//! and are never retained by the queue.

use std::collections::VecDeque;

use tilo_actions::InvocationId;

/// Enough failures to explain what happened without allowing a corrupt
/// library to grow an unbounded in-memory log.  The aggregate failure count is
/// independent, so truncating detail never makes the summary dishonest.
pub const FAILURE_DETAIL_LIMIT: usize = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibraryAnalysisPhase {
    #[default]
    Idle,
    Running,
    Paused,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryAnalysisFailure {
    pub photo_id: String,
    pub name: String,
    pub message: String,
}

/// One explicit library scan. `targets` is a bounded list of resource ids,
/// never image bytes. Exactly one target may be in flight; the shared worker's
/// sync channel supplies the second bound between UI and model runtime.
#[derive(Debug, Default)]
pub struct LibraryAnalysisState {
    pub phase: LibraryAnalysisPhase,
    pub invocation: Option<InvocationId>,
    pub model_label: Option<String>,
    pub model_revision: Option<String>,
    pub total: usize,
    pub completed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub unavailable: usize,
    pub current_name: Option<String>,
    targets: VecDeque<String>,
    in_flight: Option<String>,
    failures: VecDeque<LibraryAnalysisFailure>,
}

impl LibraryAnalysisState {
    pub fn begin(
        &mut self,
        invocation: InvocationId,
        targets: impl IntoIterator<Item = String>,
        unavailable: usize,
        model_label: String,
        model_revision: String,
    ) {
        self.phase = LibraryAnalysisPhase::Running;
        self.invocation = Some(invocation);
        self.model_label = Some(model_label);
        self.model_revision = Some(model_revision);
        self.targets = targets.into_iter().collect();
        self.total = self.targets.len();
        self.completed = 0;
        self.succeeded = 0;
        self.failed = 0;
        self.unavailable = unavailable;
        self.current_name = None;
        self.in_flight = None;
        self.failures.clear();
    }

    pub fn begin_retry(
        &mut self,
        invocation: InvocationId,
        model_label: String,
        model_revision: String,
    ) -> bool {
        if self.failures.is_empty() || self.phase == LibraryAnalysisPhase::Running {
            return false;
        }
        self.targets = self
            .failures
            .iter()
            .map(|failure| failure.photo_id.clone())
            .collect();
        self.total = self.targets.len();
        self.completed = 0;
        self.succeeded = 0;
        self.failed = 0;
        self.unavailable = 0;
        self.failures.clear();
        self.invocation = Some(invocation);
        self.model_label = Some(model_label);
        self.model_revision = Some(model_revision);
        self.current_name = None;
        self.in_flight = None;
        self.phase = LibraryAnalysisPhase::Running;
        true
    }

    pub fn take_next(&mut self, name: impl FnOnce(&str) -> Option<String>) -> Option<String> {
        if self.phase != LibraryAnalysisPhase::Running || self.in_flight.is_some() {
            return None;
        }
        let id = self.targets.pop_front()?;
        self.current_name = name(&id);
        self.in_flight = Some(id.clone());
        Some(id)
    }

    pub fn record_success(&mut self, photo_id: &str) -> bool {
        if self.in_flight.as_deref() != Some(photo_id) {
            return false;
        }
        self.in_flight = None;
        self.current_name = None;
        self.completed = self.completed.saturating_add(1);
        self.succeeded = self.succeeded.saturating_add(1);
        self.finish_if_empty();
        true
    }

    pub fn record_failure(&mut self, failure: LibraryAnalysisFailure) -> bool {
        if self.in_flight.as_deref() != Some(failure.photo_id.as_str()) {
            return false;
        }
        self.in_flight = None;
        self.current_name = None;
        self.completed = self.completed.saturating_add(1);
        self.failed = self.failed.saturating_add(1);
        if self.failures.len() == FAILURE_DETAIL_LIMIT {
            self.failures.pop_front();
        }
        self.failures.push_back(failure);
        self.finish_if_empty();
        true
    }

    pub fn pause(&mut self) -> bool {
        if self.phase != LibraryAnalysisPhase::Running {
            return false;
        }
        self.phase = LibraryAnalysisPhase::Paused;
        true
    }

    pub fn resume(&mut self, invocation: InvocationId) -> bool {
        if self.phase != LibraryAnalysisPhase::Paused || self.in_flight.is_some() {
            return false;
        }
        self.invocation = Some(invocation);
        self.phase = LibraryAnalysisPhase::Running;
        true
    }

    pub fn finish_if_empty(&mut self) -> bool {
        if self.phase == LibraryAnalysisPhase::Running
            && self.targets.is_empty()
            && self.in_flight.is_none()
        {
            self.phase = LibraryAnalysisPhase::Complete;
            return true;
        }
        false
    }

    #[must_use]
    pub fn remaining(&self) -> usize {
        self.targets.len() + usize::from(self.in_flight.is_some())
    }

    #[must_use]
    pub fn progress(&self) -> f64 {
        if self.total == 0 {
            return if self.phase == LibraryAnalysisPhase::Complete {
                1.0
            } else {
                0.0
            };
        }
        self.completed.min(self.total) as f64 / self.total as f64
    }

    pub fn failures(
        &self,
    ) -> impl DoubleEndedIterator<Item = &LibraryAnalysisFailure> + ExactSizeIterator {
        self.failures.iter()
    }

    #[must_use]
    pub fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    #[must_use]
    pub fn is_running_invocation(&self, invocation: InvocationId) -> bool {
        self.phase == LibraryAnalysisPhase::Running && self.invocation == Some(invocation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_queue_is_bounded_and_counts_honestly() {
        eprintln!("phase queue_keeps_only_ids_and_one_in_flight_item");
        {
            let invocation = InvocationId::new();
            let mut state = LibraryAnalysisState::default();
            state.begin(
                invocation,
                ["one".to_owned(), "two".to_owned()],
                3,
                "Local vision".to_owned(),
                "r1".to_owned(),
            );
            assert_eq!(state.remaining(), 2);
            assert_eq!(
                state.take_next(|id| Some(format!("{id}.jpg"))),
                Some("one".to_owned())
            );
            assert!(state.take_next(|_| None).is_none());
            assert_eq!(state.current_name.as_deref(), Some("one.jpg"));
            assert!(state.record_success("one"));
            assert!((state.progress() - 0.5).abs() < f64::EPSILON);
            assert_eq!(state.unavailable, 3);
        }

        eprintln!("phase pause_resume_and_retry_preserve_honest_counts");
        {
            let mut state = LibraryAnalysisState::default();
            state.begin(
                InvocationId::new(),
                ["one".to_owned(), "two".to_owned()],
                0,
                "Model".to_owned(),
                "r1".to_owned(),
            );
            assert!(state.pause());
            assert!(state.take_next(|_| None).is_none());
            assert!(state.resume(InvocationId::new()));
            assert_eq!(
                state.take_next(|_| Some("One".into())).as_deref(),
                Some("one")
            );
            assert!(state.record_failure(LibraryAnalysisFailure {
                photo_id: "one".into(),
                name: "One".into(),
                message: "decode failed".into(),
            }));
            assert_eq!(
                state.take_next(|_| Some("Two".into())).as_deref(),
                Some("two")
            );
            assert!(state.record_success("two"));
            assert_eq!(state.phase, LibraryAnalysisPhase::Complete);
            assert_eq!((state.succeeded, state.failed), (1, 1));

            assert!(state.begin_retry(InvocationId::new(), "Model".into(), "r1".into()));
            assert_eq!(state.total, 1);
            assert_eq!(state.remaining(), 1);
            assert_eq!(state.failed, 0);
        }

        eprintln!("phase failure_details_are_bounded_without_losing_total");
        {
            let total = FAILURE_DETAIL_LIMIT + 5;
            let mut state = LibraryAnalysisState::default();
            state.begin(
                InvocationId::new(),
                (0..total).map(|n| n.to_string()),
                0,
                "Model".into(),
                "r1".into(),
            );
            for n in 0..total {
                let id = n.to_string();
                assert_eq!(
                    state.take_next(|_| Some(id.clone())).as_deref(),
                    Some(id.as_str())
                );
                assert!(state.record_failure(LibraryAnalysisFailure {
                    photo_id: id.clone(),
                    name: id,
                    message: "failed".into(),
                }));
            }
            assert_eq!(state.failed, total);
            assert_eq!(state.failures().count(), FAILURE_DETAIL_LIMIT);
            assert_eq!(state.phase, LibraryAnalysisPhase::Complete);
        }
    }
}
