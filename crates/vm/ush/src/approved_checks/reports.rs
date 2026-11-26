//! Informational phase reports copied from the existing owned stderr capture.
//! This parser never changes command output, execution outcome or supervision.
use super::{ApprovedCheck, identifier};
use serde::{Deserialize, Serialize};

const RESERVED: &[u8] = b"TILO_CHECK_REPORT_";
const PREFIX: &[u8] = b"TILO_CHECK_REPORT_V1 ";
const MAX_FRAME_BYTES: usize = 2048;

/// A phase label chosen by the trusted project host, never by command output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedCheckPhase {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckReport {
    pub phase_id: String,
    pub phase: String,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum CheckReportRefusal {
    InvalidReport,
    OversizedReport,
    UnknownPhase,
    CorrelationChanged,
    SequenceChanged,
    OutputUnavailable,
    Backpressure,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ReportedCheckEvent {
    Reported(CheckReport),
    Disabled(CheckReportRefusal),
}

pub(super) fn validate_phases(phases: &[ReportedCheckPhase]) -> Result<(), String> {
    let mut ids = std::collections::BTreeSet::new();
    if phases.len() > 32
        || phases.iter().any(|phase| {
            !identifier(&phase.id)
                || phase.label.trim().is_empty()
                || phase.label.len() > 512
                || phase.label.chars().count() > 128
                || phase.label.chars().any(char::is_control)
                || !ids.insert(&phase.id)
        })
    {
        return Err("approved reported phases are invalid or too large".into());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    token: String,
    check_id: String,
    sequence: u64,
    phase_id: String,
    completed: Option<u64>,
    total: Option<u64>,
}

pub(super) struct ReportParser {
    phases: Vec<ReportedCheckPhase>,
    token: String,
    check_id: String,
    next_sequence: Option<u64>,
    line: Vec<u8>,
    discarding: bool,
    disabled: bool,
    latest: Option<ReportedCheckEvent>,
}

impl ReportParser {
    pub(super) fn new(check: &ApprovedCheck, token: &str) -> Self {
        Self {
            phases: check.reported_phases.clone(),
            token: token.into(),
            check_id: check.id.clone(),
            next_sequence: Some(1),
            line: Vec::new(),
            discarding: false,
            disabled: check.reported_phases.is_empty(),
            latest: None,
        }
    }

    fn refuse(&mut self, reason: CheckReportRefusal) {
        self.disabled = true;
        self.line.clear();
        self.latest = Some(ReportedCheckEvent::Disabled(reason));
    }

    fn feed(&mut self, bytes: &[u8]) {
        if self.disabled {
            return;
        }
        for &byte in bytes {
            if byte == b'\n' {
                if !self.discarding {
                    self.frame();
                    if self.disabled {
                        return;
                    }
                }
                self.line.clear();
                self.discarding = false;
            } else if !self.discarding {
                if self.line.len() == MAX_FRAME_BYTES {
                    if self.line.starts_with(RESERVED) {
                        self.refuse(CheckReportRefusal::OversizedReport);
                        return;
                    }
                    self.line.clear();
                    self.discarding = true;
                } else {
                    self.line.push(byte);
                }
            }
        }
    }

    fn frame(&mut self) {
        if !self.line.starts_with(RESERVED) {
            return;
        }
        let Some(bytes) = self.line.strip_prefix(PREFIX) else {
            self.refuse(CheckReportRefusal::InvalidReport);
            return;
        };
        let Ok(frame) = serde_json::from_slice::<Frame>(bytes) else {
            self.refuse(CheckReportRefusal::InvalidReport);
            return;
        };
        if frame.token != self.token || frame.check_id != self.check_id {
            self.refuse(CheckReportRefusal::CorrelationChanged);
            return;
        }
        if Some(frame.sequence) != self.next_sequence {
            self.refuse(CheckReportRefusal::SequenceChanged);
            return;
        }
        if frame
            .completed
            .zip(frame.total)
            .is_some_and(|(done, total)| done > total)
        {
            self.refuse(CheckReportRefusal::InvalidReport);
            return;
        }
        let Some(phase) = self.phases.iter().find(|phase| phase.id == frame.phase_id) else {
            self.refuse(CheckReportRefusal::UnknownPhase);
            return;
        };
        self.next_sequence = frame.sequence.checked_add(1);
        self.latest = Some(ReportedCheckEvent::Reported(CheckReport {
            phase_id: phase.id.clone(),
            phase: phase.label.clone(),
            completed: frame.completed,
            total: frame.total,
        }));
    }

    pub(super) fn drain(
        &mut self,
        child: &mut bounded_command::AsyncCommand,
        authority: &impl Fn() -> Result<(), String>,
        report: &impl Fn(ReportedCheckEvent),
    ) -> Result<(), String> {
        authority()?;
        for _ in 0..bounded_command::STDERR_OBSERVATION_CHUNKS {
            let Some(observation) = child.try_stderr() else {
                break;
            };
            match observation {
                bounded_command::StderrObservation::Bytes(bytes) => self.feed(&bytes),
                bounded_command::StderrObservation::Overflow => {
                    if !self.disabled {
                        self.refuse(CheckReportRefusal::Backpressure);
                    }
                }
            }
        }
        self.publish(authority, report)
    }

    pub(super) fn publish(
        &mut self,
        authority: &impl Fn() -> Result<(), String>,
        report: &impl Fn(ReportedCheckEvent),
    ) -> Result<(), String> {
        authority()?;
        if let Some(latest) = self.latest.take() {
            report(latest);
        }
        Ok(())
    }

    pub(super) fn finish(&mut self) {
        if !self.disabled && self.line.starts_with(RESERVED) {
            self.refuse(CheckReportRefusal::InvalidReport);
        }
    }
}
