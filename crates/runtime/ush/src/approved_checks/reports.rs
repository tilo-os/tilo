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

/// A command's informational claim, without execution or evidence authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckReport {
    pub phase_id: String,
    pub phase: String,
    pub completed: Option<u64>,
    pub total: Option<u64>,
}

/// Closed, bounded reasons for refusing the informational lane.
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

/// Coalesced informational events; terminal status is always the check receipt.
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
        // Bound each authority poll even when a producer continuously refills
        // the queue. After capture joins, one full batch drains its remainder.
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
            // Reserved reports are newline-framed; a partial final frame is refused.
            self.refuse(CheckReportRefusal::InvalidReport);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parser() -> ReportParser {
        ReportParser {
            phases: vec![ReportedCheckPhase {
                id: "route".into(),
                label: "Routing".into(),
            }],
            token: "owned-token".into(),
            check_id: "check".into(),
            next_sequence: Some(1),
            line: Vec::new(),
            discarding: false,
            disabled: false,
            latest: None,
        }
    }
    fn frame(sequence: u64) -> serde_json::Value {
        serde_json::json!({"token":"owned-token","check_id":"check","sequence":sequence,"phase_id":"route","completed":4,"total":10})
    }
    fn encoded(frame: &serde_json::Value) -> Vec<u8> {
        let mut bytes = PREFIX.to_vec();
        bytes.extend(serde_json::to_vec(frame).unwrap());
        bytes.push(b'\n');
        bytes
    }
    #[test]
    fn host_phase_vocabulary_obeys_wire_bounds_and_rejects_duplicate_ids() {
        let phase = ReportedCheckPhase {
            id: "route".into(),
            label: "🦀".repeat(128),
        };
        assert!(validate_phases(std::slice::from_ref(&phase)).is_ok());
        assert!(validate_phases(&[phase.clone(), phase]).is_err());
        for label in [
            "a".repeat(129),
            "🦀".repeat(129),
            "Routing\ncomplete".into(),
            " ".into(),
        ] {
            assert!(
                validate_phases(&[ReportedCheckPhase {
                    id: "route".into(),
                    label
                }])
                .is_err()
            );
        }
    }

    #[test]
    fn captured_frames_are_chunk_independent_coalesced_and_host_labelled() {
        let mut parser = parser();
        parser.feed(b"ordinary diagnostics\n");
        let first = encoded(&frame(1));
        for chunk in first.chunks(3) {
            parser.feed(chunk);
        }
        let mut second = frame(2);
        second["completed"] = 8.into();
        parser.feed(&encoded(&second));
        assert_eq!(
            parser.latest,
            Some(ReportedCheckEvent::Reported(CheckReport {
                phase_id: "route".into(),
                phase: "Routing".into(),
                completed: Some(8),
                total: Some(10),
            }))
        );
        assert!(!parser.disabled);
    }
    #[test]
    fn untrusted_frames_refuse_only_the_informational_lane() {
        for (key, value, reason) in [
            (
                "token",
                serde_json::json!("stale"),
                CheckReportRefusal::CorrelationChanged,
            ),
            (
                "check_id",
                serde_json::json!("foreign"),
                CheckReportRefusal::CorrelationChanged,
            ),
            (
                "sequence",
                serde_json::json!(2),
                CheckReportRefusal::SequenceChanged,
            ),
            (
                "phase_id",
                serde_json::json!("unknown"),
                CheckReportRefusal::UnknownPhase,
            ),
            (
                "completed",
                serde_json::json!(11),
                CheckReportRefusal::InvalidReport,
            ),
            (
                "success",
                serde_json::json!(true),
                CheckReportRefusal::InvalidReport,
            ),
        ] {
            let mut parser = parser();
            let mut bad = frame(1);
            bad[key] = value;
            parser.feed(&encoded(&bad));
            assert_eq!(parser.latest, Some(ReportedCheckEvent::Disabled(reason)));
            assert!(parser.disabled);
            parser.feed(&encoded(&frame(1)));
            assert_eq!(parser.latest, Some(ReportedCheckEvent::Disabled(reason)));
        }
        for malformed in [
            b"TILO_CHECK_REPORT_V1\n".as_slice(),
            b"TILO_CHECK_REPORT_V2 {}\n".as_slice(),
            b"TILO_CHECK_REPORT_V1 not-json\n".as_slice(),
        ] {
            let mut parser = parser();
            parser.feed(malformed);
            assert_eq!(
                parser.latest,
                Some(ReportedCheckEvent::Disabled(
                    CheckReportRefusal::InvalidReport
                ))
            );
        }
        let mut oversized = parser();
        let mut bytes = PREFIX.to_vec();
        bytes.extend(vec![b'x'; MAX_FRAME_BYTES]);
        oversized.feed(&bytes);
        assert_eq!(
            oversized.latest,
            Some(ReportedCheckEvent::Disabled(
                CheckReportRefusal::OversizedReport
            ))
        );
        let mut ordinary = parser();
        ordinary.feed(&vec![b'x'; MAX_FRAME_BYTES * 10]);
        ordinary.feed(b"\n");
        ordinary.feed(&encoded(&frame(1)));
        assert!(matches!(
            ordinary.latest,
            Some(ReportedCheckEvent::Reported(_))
        ));
    }
    #[test]
    fn truncated_reports_and_revoked_callbacks_are_refused() {
        let mut parser = parser();
        let bytes = encoded(&frame(1));
        parser.feed(&bytes[..bytes.len() - 1]);
        parser.finish();
        assert_eq!(
            parser.latest,
            Some(ReportedCheckEvent::Disabled(
                CheckReportRefusal::InvalidReport
            ))
        );
        assert!(
            parser
                .publish(&|| Err("revoked".into()), &|_| panic!("revoked callback"))
                .is_err()
        );
        assert!(parser.latest.is_some());
    }
}
