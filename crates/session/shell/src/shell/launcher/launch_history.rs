//! Bounded, owner-only startup evidence. Never retain argv, environment or app output.
use std::collections::VecDeque;

use serde::Serialize;

const MAX_ATTEMPTS: usize = 64;
const MAX_EVENTS: usize = 12;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Requested,
    Prepared,
    Spawned,
    SpawnFailed,
    Mapped,
    FirstPresented,
    Retired,
    Exited,
    BindingStarted,
    BindingChallenge,
    BindingAnswer,
    BindingAdmitted,
    BindingRefused,
    BindingAbandoned,
}

#[derive(Clone, Debug, Serialize)]
struct Event {
    phase: Phase,
    original_epoch_ns: u128,
    elapsed_us: u128,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Process {
    pub(crate) pid: u32,
    pub(crate) kernel_start_ticks: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StderrState {
    Pending,
    Complete,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Exit {
    pub(crate) code: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) success: bool,
    pub(crate) wait_failed: bool,
    /// Fixed, localized failure category; never arbitrary stderr.
    pub(crate) reason_key: Option<&'static str>,
    pub(crate) retained_stderr_lines: usize,
    pub(crate) stderr_redacted: bool,
    pub(crate) stderr_state: StderrState,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Attempt {
    launch_id: u64,
    package_id: String,
    requested_epoch_ns: u128,
    #[serde(skip)]
    requested_at: std::time::Instant,
    record_instance: Option<u64>,
    saved_tab: Option<u64>,
    workspace_id: Option<String>,
    package_hash: Option<String>,
    payload_root: Option<String>,
    /// No runtime was authenticated by this observer: the component receipt
    /// and signed package/runtime pins remain the source of cohort evidence.
    runtime_cohort: Option<String>,
    process: Option<Process>,
    surface_id: Option<u64>,
    exit: Option<Exit>,
    events: VecDeque<Event>,
    discarded_events: u64,
}

#[derive(Default)]
pub(crate) struct History {
    attempts: VecDeque<Attempt>,
    discarded_attempts: u64,
}

pub(crate) fn epoch_ns() -> u128 {
    u128::from(tilo_clock::unix_now_ns())
}

impl History {
    pub(crate) fn requested(&mut self, id: u64, package: &str, saved_tab: Option<u64>) {
        if self.attempts.len() == MAX_ATTEMPTS {
            self.attempts.pop_front();
            self.discarded_attempts = self.discarded_attempts.saturating_add(1);
        }
        let requested_epoch_ns = epoch_ns();
        self.attempts.push_back(Attempt {
            launch_id: id,
            package_id: package.chars().take(256).collect(),
            requested_epoch_ns,
            requested_at: std::time::Instant::now(),
            saved_tab,
            record_instance: None,
            workspace_id: None,
            package_hash: None,
            payload_root: None,
            runtime_cohort: None,
            process: None,
            surface_id: None,
            exit: None,
            events: VecDeque::from([Event {
                phase: Phase::Requested,
                original_epoch_ns: requested_epoch_ns,
                elapsed_us: 0,
            }]),
            discarded_events: 0,
        });
    }

    fn attempt(&mut self, id: u64) -> Option<&mut Attempt> {
        self.attempts
            .iter_mut()
            .find(|attempt| attempt.launch_id == id)
    }

    pub(crate) fn event(&mut self, id: u64, phase: Phase) {
        let Some(attempt) = self.attempt(id) else {
            return;
        };
        if attempt.events.iter().any(|event| event.phase == phase) {
            return;
        }
        if attempt.events.len() == MAX_EVENTS {
            attempt.events.pop_front();
            attempt.discarded_events = attempt.discarded_events.saturating_add(1);
        }
        attempt.events.push_back(Event {
            phase,
            original_epoch_ns: epoch_ns(),
            elapsed_us: attempt.requested_at.elapsed().as_micros(),
        });
    }

    pub(crate) fn prepared(&mut self, id: u64, command: &super::LaunchCommand) {
        let Some(attempt) = self.attempt(id) else {
            return;
        };
        attempt.record_instance = command.env.iter().find_map(|(name, value)| {
            (name == "TILO_CONTINUITY_INSTANCE")
                .then(|| value.parse().ok())
                .flatten()
        });
        attempt.payload_root = command.args.windows(2).find_map(|pair| {
            (pair.first().is_some_and(|flag| flag == "--payload-root"))
                .then(|| pair.get(1).map(|root| root.chars().take(1024).collect()))
                .flatten()
        });
        if let Some(identity) = &command.service_identity {
            attempt.workspace_id = Some(identity.workspace_id.chars().take(64).collect());
            attempt.package_hash = identity
                .application_creation
                .as_ref()
                .filter(|evidence| evidence.valid())
                .map(|evidence| evidence.package_hash.clone());
        }
        self.event(id, Phase::Prepared);
    }

    pub(crate) fn spawned(&mut self, id: u64, pid: u32) {
        if let Some(attempt) = self.attempt(id) {
            attempt.process = super::proc_stat(pid).map(|(_, kernel_start_ticks)| Process {
                pid,
                kernel_start_ticks,
            });
        }
        self.event(id, Phase::Spawned);
    }

    pub(crate) fn mapped(&mut self, id: u64, surface: u64) {
        if let Some(attempt) = self.attempt(id) {
            attempt.surface_id = Some(surface);
        }
        self.event(id, Phase::Mapped);
    }

    pub(crate) fn presented(&mut self, surface: u64) {
        let id = self
            .attempts
            .iter()
            .find(|attempt| attempt.surface_id == Some(surface))
            .map(|attempt| attempt.launch_id);
        if let Some(id) = id {
            self.event(id, Phase::FirstPresented);
        }
    }

    pub(crate) fn exited(&mut self, id: u64, exit: Exit) {
        if let Some(attempt) = self.attempt(id) {
            attempt.exit = Some(exit);
        }
        self.event(id, Phase::Exited);
    }

    pub(crate) fn binding(&mut self, pid: u32, phase: Phase) -> Option<u64> {
        let (_, start) = super::proc_stat(pid)?;
        let id = self
            .attempts
            .iter()
            .find(|attempt| {
                attempt.process.as_ref().is_some_and(|process| {
                    process.pid == pid && process.kernel_start_ticks == start
                })
            })
            .map(|attempt| attempt.launch_id);
        if let Some(id) = id {
            self.event(id, phase);
        }
        id
    }

    pub(crate) fn value(&self) -> serde_json::Value {
        serde_json::json!({
            "launch_generation": tilo_startup_metrics::launch_generation(),
            "origin": "compositor_owned_attempt_history",
            "replayed": false,
            "observer_pid": std::process::id(),
            "observed_epoch_ns": epoch_ns(),
            "max_attempts": MAX_ATTEMPTS,
            "max_events_per_attempt": MAX_EVENTS,
            "discarded_attempts": self.discarded_attempts,
            "stderr_policy": "fully_redacted_fixed_reason_only",
            "source_and_runtime_cohort": "requires_matched_component_receipt_and_signed_pins",
            "attempts": self.attempts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_receipt_bounds_retention_and_observes_only_the_exact_live_process() {
        let mut history = History::default();
        for id in 1..=65 {
            history.requested(id, "dev.tilo.history", Some(id));
        }
        let value = history.value();
        assert_eq!(value["discarded_attempts"], 1);
        assert_eq!(value["attempts"].as_array().unwrap().len(), MAX_ATTEMPTS);
        assert_eq!(value["attempts"][0]["launch_id"], 2);
        let pid = std::process::id();
        history.spawned(65, pid);
        history.binding(pid, Phase::BindingChallenge);
        history.binding(pid, Phase::BindingAnswer);
        history.binding(pid, Phase::BindingAdmitted);
        history.binding(pid, Phase::BindingAdmitted);
        history.binding(u32::MAX, Phase::BindingRefused);
        history
            .attempt(65)
            .unwrap()
            .process
            .as_mut()
            .unwrap()
            .kernel_start_ticks += 1;
        history.binding(pid, Phase::BindingAbandoned);
        let value = history.value();
        let attempt = value["attempts"].as_array().unwrap().last().unwrap();
        let events = attempt["events"].as_array().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["phase"] == "binding_admitted")
                .count(),
            1
        );
        assert!(
            !events
                .iter()
                .any(|event| event["phase"] == "binding_abandoned"
                    || event["phase"] == "binding_refused")
        );
        assert!(
            events
                .iter()
                .all(|event| event["original_epoch_ns"].as_u64().unwrap() > 0)
        );
        assert!(value.to_string().len() < 100_000);
    }
}
