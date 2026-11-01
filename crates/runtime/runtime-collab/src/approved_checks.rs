//! Native project checks selected by ID. Commands and execution bounds are host-only.
use serde::{Deserialize, Serialize};

pub const MAX_REQUEST_BYTES: usize = 1024;
pub const MAX_RECEIPT_BYTES: usize = 16384;
pub const MAX_PREVIEW_BYTES: usize = 2048;
pub const ACTION_ID: &str = "run-approved-check";

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn invocation(value: &str) -> bool {
    crate::valid_uuid(value)
        && !value.bytes().any(|b| b.is_ascii_uppercase())
        && value != "00000000-0000-0000-0000-000000000000"
}

/// Inert correlation only; the broker must recover actual target custody.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u8,
    pub invocation: String,
    pub check_id: String,
}
impl Request {
    /// Encode bounded correlation data, never execution authority.
    ///
    /// # Errors
    /// Refuses invalid identities, unknown schema or oversized data.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if self.version != 1 || !invocation(&self.invocation) || !identifier(&self.check_id) {
            return Err("invalid approved-check request".into());
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("approved-check request too large".into());
        }
        Ok(bytes)
    }
    /// Decode the closed request vocabulary.
    ///
    /// # Errors
    /// Refuses malformed, duplicate, unknown or invalid fields and oversized input.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("approved-check request too large".into());
        }
        let request: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        request.encode()?;
        Ok(request)
    }
}

/// Maximum encoded informational report, independent of execution output bounds.
pub const MAX_REPORT_BYTES: usize = 2048;

/// Closed reasons for disabling informational observation, never execution outcomes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneRefusal {
    InvalidReport,
    OversizedReport,
    UnknownPhase,
    CorrelationChanged,
    SequenceChanged,
    OutputUnavailable,
    Backpressure,
}

/// Command observations do not establish success or authorize any operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReportObservation {
    Reported {
        phase: String,
        completed: Option<u64>,
        total: Option<u64>,
    },
    Disabled {
        reason: LaneRefusal,
    },
}

/// Inert observation bound to the original check invocation by the broker.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub invocation: String,
    pub check_id: String,
    pub sequence: u64,
    pub observation: ReportObservation,
}
impl Report {
    /// Encode a bounded informational observation.
    ///
    /// # Errors
    /// Refuses invalid correlation, sequence, phase, counts or oversized data.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if !invocation(&self.invocation) || !identifier(&self.check_id) || self.sequence == 0 {
            return Err("invalid approved-check report correlation".into());
        }
        if let ReportObservation::Reported {
            phase,
            completed,
            total,
        } = &self.observation
            && (phase.trim().is_empty()
                || phase.len() > 512
                || phase.chars().count() > 128
                || phase.chars().any(char::is_control)
                || matches!((completed, total), (Some(done), Some(limit)) if done > limit))
        {
            return Err("invalid approved-check reported phase".into());
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_REPORT_BYTES {
            return Err("approved-check report too large".into());
        }
        Ok(bytes)
    }

    /// Decode the closed informational vocabulary.
    ///
    /// # Errors
    /// Refuses unknown, duplicate, malformed, invalid or oversized fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_REPORT_BYTES {
            return Err("approved-check report too large".into());
        }
        let report: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        report.encode()?;
        Ok(report)
    }
}

/// Canonical resource identity resolved by the admitted project host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub app_id: String,
    pub workspace: String,
    pub host: [u8; 32],
    pub record: String,
    pub resource: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Exited,
    Cancelled,
    TimedOut,
    OutputLimit,
    Failed,
}

/// A bounded UTF-8 diagnostic excerpt; its hash identifies the complete captured stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub sha256: Option<String>,
    pub preview: String,
    pub truncated: bool,
}

/// Observed execution metadata. This is not a grader verdict or a model score.
/// The full private receipt remains with the trusted host under its content hash.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub version: u8,
    pub invocation: String,
    pub check_id: String,
    pub binding: Binding,
    pub artifact_sha256: String,
    pub config_sha256: String,
    pub runtime_binary_sha256: String,
    pub supervisor_source_sha256: String,
    pub outcome: Outcome,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub elapsed_ms: u64,
    pub cleanup_observed: bool,
    pub stdout: Diagnostic,
    pub stderr: Diagnostic,
    /// Why the host could not supervise the check, apart from the command's own streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<Diagnostic>,
}
impl Receipt {
    fn valid(&self) -> bool {
        self.version == 1
            && invocation(&self.invocation)
            && identifier(&self.check_id)
            && identifier(&self.binding.app_id)
            && invocation(&self.binding.workspace)
            && !self.binding.record.is_empty()
            && self.binding.record.len() <= 1024
            && !self.binding.record.contains('\0')
            && !self.binding.resource.is_empty()
            && self.binding.resource.len() <= 1024
            && !self.binding.resource.contains('\0')
            && self.binding.host != [0; 32]
            && [
                &self.artifact_sha256,
                &self.config_sha256,
                &self.runtime_binary_sha256,
                &self.supervisor_source_sha256,
            ]
            .into_iter()
            .all(|v| digest(v))
            && [&self.stdout, &self.stderr]
                .into_iter()
                .chain(self.failure.as_ref())
                .all(|v| {
                    v.preview.len() <= MAX_PREVIEW_BYTES
                        && v.sha256.as_ref().is_none_or(|s| digest(s))
                        && (v.sha256.is_some() || v.preview.is_empty() && !v.truncated)
                })
            && (self.exit_code.is_none() || self.signal.is_none())
            && (self.outcome != Outcome::Exited
                || self.cleanup_observed && (self.exit_code.is_some() || self.signal.is_some()))
    }
    /// Encode only bounded observed metadata and diagnostics.
    ///
    /// # Errors
    /// Refuses invalid or oversized receipts.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if !self.valid() {
            return Err("invalid approved-check receipt".into());
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err("approved-check receipt too large".into());
        }
        Ok(bytes)
    }
    /// Decode one bounded receipt without trusting its contents as authority.
    ///
    /// # Errors
    /// Refuses malformed, duplicate, unknown, invalid or oversized data.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err("approved-check receipt too large".into());
        }
        let receipt: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        receipt.encode()?;
        Ok(receipt)
    }
}

#[cfg(feature = "client")]
impl crate::ChannelHandle {
    /// Execute one host-approved ID for this target's actual accepted action.
    ///
    /// # Errors
    /// Refuses invalid requests, unavailable transport, stale custody or invalid replies.
    pub async fn approved_check(&self, request: &Request) -> Result<Receipt, crate::NotAvailable> {
        let encoded = request.encode().map_err(crate::NotAvailable::Refused)?;
        let reply = self.approved_check_command(encoded).await?;
        let receipt = Receipt::decode(&reply).map_err(crate::NotAvailable::Refused)?;
        if receipt.invocation != request.invocation || receipt.check_id != request.check_id {
            return Err(crate::NotAvailable::Refused(
                "approved-check response correlation changed".into(),
            ));
        }
        Ok(receipt)
    }
}

/// One result of a check's reported progress.
#[cfg(feature = "client")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReportedProgressUpdate {
    Report(Report),
    Receipt(Box<Receipt>),
}

/// A check's reported progress, then its receipt; this does not own an
/// execution job.
#[cfg(feature = "client")]
pub struct ApprovedCheckReportedProgressStream {
    feed: Option<crate::client::Feed>,
    request: Request,
    sequence: u64,
    terminal: bool,
    disabled: bool,
}

#[cfg(feature = "client")]
fn report_error(error: impl Into<String>) -> crate::NotAvailable {
    crate::NotAvailable::Refused(error.into())
}

#[cfg(feature = "client")]
impl ApprovedCheckReportedProgressStream {
    pub(crate) const fn new(feed: crate::client::Feed, request: Request) -> Self {
        Self {
            feed: Some(feed),
            request,
            sequence: 0,
            terminal: false,
            disabled: false,
        }
    }

    /// Pull one bounded observation or the unchanged terminal receipt.
    ///
    /// # Errors
    /// Refuses malformed or miscorrelated observations, nonmonotonic
    /// sequences and reads after terminal completion. Any refusal freezes
    /// this stream.
    pub async fn read(&mut self) -> Result<ReportedProgressUpdate, crate::NotAvailable> {
        if self.terminal {
            return Err(report_error("approved-check report lane is terminal"));
        }
        let progress = match &self.feed {
            Some(feed) => match feed.event().await {
                Ok(crate::wire::Event::ApprovedCheck(progress)) => Ok(progress),
                Ok(other) => Err(report_error(format!(
                    "the collaboration host delivered {other:?}"
                ))),
                Err(error) => Err(error),
            },
            None => Err(report_error("approved-check report lane is closed")),
        };
        self.accept(progress)
    }

    fn accept(
        &mut self,
        progress: Result<crate::wire::CheckProgress, crate::NotAvailable>,
    ) -> Result<ReportedProgressUpdate, crate::NotAvailable> {
        let result = progress.and_then(|progress| self.check(progress));
        if !matches!(result, Ok(ReportedProgressUpdate::Report(_))) {
            self.terminal = true;
            self.feed = None;
        }
        result
    }

    fn check(
        &mut self,
        progress: crate::wire::CheckProgress,
    ) -> Result<ReportedProgressUpdate, crate::NotAvailable> {
        use crate::wire::CheckProgress;
        let report = match progress {
            CheckProgress::Failed(error) => {
                if error.len() > MAX_REPORT_BYTES {
                    return Err(report_error("approved-check report refusal too large"));
                }
                return Err(report_error(error));
            }
            CheckProgress::Receipt(receipt) => {
                let receipt = Receipt::decode(&receipt).map_err(report_error)?;
                if receipt.invocation != self.request.invocation
                    || receipt.check_id != self.request.check_id
                {
                    return Err(report_error("approved-check response correlation changed"));
                }
                return Ok(ReportedProgressUpdate::Receipt(Box::new(receipt)));
            }
            CheckProgress::Report(report) => report,
        };
        let report = Report::decode(&report).map_err(report_error)?;
        if report.invocation != self.request.invocation
            || report.check_id != self.request.check_id
            || report.sequence <= self.sequence
        {
            return Err(report_error(
                "approved-check report correlation or sequence changed",
            ));
        }
        if self.disabled {
            return Err(report_error("approved-check reporting lane is disabled"));
        }
        self.disabled = matches!(report.observation, ReportObservation::Disabled { .. });
        self.sequence = report.sequence;
        Ok(ReportedProgressUpdate::Report(report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn receipt() -> Receipt {
        Receipt {
            version: 1,
            invocation: ID.into(),
            check_id: "regression".into(),
            binding: Binding {
                app_id: "so.tilo.git".into(),
                workspace: ID.into(),
                host: [7; 32],
                record: "project".into(),
                resource: "repository".into(),
            },
            artifact_sha256: "a".repeat(64),
            config_sha256: "b".repeat(64),
            runtime_binary_sha256: "c".repeat(64),
            supervisor_source_sha256: "d".repeat(64),
            outcome: Outcome::Exited,
            exit_code: Some(1),
            signal: None,
            elapsed_ms: 34,
            cleanup_observed: true,
            stdout: Diagnostic {
                sha256: Some("e".repeat(64)),
                preview: "failed regression".into(),
                truncated: false,
            },
            stderr: Diagnostic {
                sha256: None,
                preview: String::new(),
                truncated: false,
            },
            failure: None,
        }
    }

    fn report(sequence: u64) -> Report {
        Report {
            invocation: ID.into(),
            check_id: "regression".into(),
            sequence,
            observation: ReportObservation::Reported {
                phase: "Routing".into(),
                completed: Some(4),
                total: Some(10),
            },
        }
    }

    #[test]
    fn approved_check_report_closed_vocabulary_and_bounds() {
        let value = report(1);
        assert_eq!(Report::decode(&value.encode().unwrap()).unwrap(), value);
        for phase in ["", " ", "line\nbreak", &"é".repeat(129)] {
            let mut changed = value.clone();
            changed.observation = ReportObservation::Reported {
                phase: phase.into(),
                completed: None,
                total: None,
            };
            assert!(changed.encode().is_err());
        }
        assert!(report(0).encode().is_err());
        let mut bad = serde_json::to_value(&value).unwrap();
        bad["authority"] = serde_json::json!(true);
        assert!(Report::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
        let mut bad = serde_json::to_value(&value).unwrap();
        bad["observation"]["argv"] = serde_json::json!(["anything"]);
        assert!(Report::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
        bad["observation"] =
            serde_json::json!({"kind":"reported", "phase":"Routing", "completed":11, "total":10});
        assert!(Report::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
        assert!(Report::decode(&vec![b' '; MAX_REPORT_BYTES + 1]).is_err());
        let disabled = Report {
            observation: ReportObservation::Disabled {
                reason: LaneRefusal::Backpressure,
            },
            ..value
        };
        assert_eq!(
            Report::decode(&disabled.encode().unwrap()).unwrap(),
            disabled
        );
    }

    #[cfg(feature = "client")]
    use crate::wire::CheckProgress;

    /// A stream fed `frames` in order, then nothing.
    #[cfg(feature = "client")]
    struct Frames {
        stream: ApprovedCheckReportedProgressStream,
        frames: std::collections::VecDeque<CheckProgress>,
    }

    #[cfg(feature = "client")]
    impl Frames {
        fn read(&mut self) -> Result<ReportedProgressUpdate, crate::NotAvailable> {
            if self.stream.terminal {
                return Err(report_error("approved-check report lane is terminal"));
            }
            let next = self
                .frames
                .pop_front()
                .ok_or_else(|| report_error("unexpected extra read"));
            self.stream.accept(next)
        }
    }

    #[cfg(feature = "client")]
    fn wire_reports(frames: Vec<CheckProgress>) -> Frames {
        Frames {
            stream: ApprovedCheckReportedProgressStream {
                feed: None,
                request: Request {
                    version: 1,
                    invocation: ID.into(),
                    check_id: "regression".into(),
                },
                sequence: 0,
                terminal: false,
                disabled: false,
            },
            frames: frames.into(),
        }
    }

    #[cfg(feature = "client")]
    #[test]
    fn approved_check_report_stream_keeps_terminal_receipt_separate_and_final() {
        let observed = report(1);
        let terminal = receipt();
        let mut stream = wire_reports(vec![
            CheckProgress::Report(observed.encode().unwrap()),
            CheckProgress::Receipt(terminal.encode().unwrap()),
            CheckProgress::Report(report(2).encode().unwrap()),
        ]);
        assert_eq!(
            stream.read().unwrap(),
            ReportedProgressUpdate::Report(observed)
        );
        assert_eq!(
            stream.read().unwrap(),
            ReportedProgressUpdate::Receipt(Box::new(terminal))
        );
        assert!(
            stream.read().is_err(),
            "late observation cannot replace receipt"
        );
    }

    #[cfg(feature = "client")]
    #[test]
    fn approved_check_disabled_report_lane_accepts_only_terminal_receipt() {
        let disabled = Report {
            observation: ReportObservation::Disabled {
                reason: LaneRefusal::InvalidReport,
            },
            ..report(1)
        };
        let mut stream = wire_reports(vec![
            CheckProgress::Report(disabled.encode().unwrap()),
            CheckProgress::Receipt(receipt().encode().unwrap()),
        ]);
        assert_eq!(
            stream.read().unwrap(),
            ReportedProgressUpdate::Report(disabled.clone())
        );
        assert_eq!(
            stream.read().unwrap(),
            ReportedProgressUpdate::Receipt(Box::new(receipt()))
        );
        let mut stream = wire_reports(vec![
            CheckProgress::Report(disabled.encode().unwrap()),
            CheckProgress::Report(report(2).encode().unwrap()),
        ]);
        assert!(matches!(
            stream.read().unwrap(),
            ReportedProgressUpdate::Report(_)
        ));
        assert!(
            stream.read().is_err(),
            "disabled observation lane cannot restart"
        );
    }

    #[cfg(feature = "client")]
    #[test]
    fn approved_check_report_stream_refuses_empty_foreign_stale_and_oversized() {
        let mut foreign = report(1);
        foreign.invocation = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into();
        for frame in [
            CheckProgress::Report(Vec::new()),
            CheckProgress::Receipt(Vec::new()),
            CheckProgress::Report(foreign.encode().unwrap()),
            CheckProgress::Report(vec![b' '; MAX_REPORT_BYTES + 1]),
            CheckProgress::Failed("x".repeat(MAX_REPORT_BYTES + 1)),
        ] {
            let mut stream = wire_reports(vec![frame]);
            assert!(stream.read().is_err());
            assert!(stream.read().is_err(), "refusal freezes lane");
        }
        let mut stream = wire_reports(vec![
            CheckProgress::Report(report(3).encode().unwrap()),
            CheckProgress::Report(report(3).encode().unwrap()),
        ]);
        assert!(matches!(
            stream.read().unwrap(),
            ReportedProgressUpdate::Report(_)
        ));
        assert!(stream.read().is_err());
    }

    #[test]
    fn approved_check_request_refuses_execution_inputs_and_ambiguous_identities() {
        let request = Request {
            version: 1,
            invocation: ID.into(),
            check_id: "regression".into(),
        };
        let bytes = request.encode().unwrap();
        assert_eq!(Request::decode(&bytes).unwrap(), request);
        for extra in [
            "command",
            "argv",
            "environment",
            "checkout",
            "host",
            "certificate",
        ] {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            value[extra] = serde_json::json!("caller-supplied");
            assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let duplicate = format!(
            r#"{{"version":1,"invocation":"{ID}","check_id":"regression","check_id":"other"}}"#
        );
        assert!(Request::decode(duplicate.as_bytes()).is_err());
        for check_id in ["../regression", "regression;id", "-option", "", "run test"] {
            assert!(
                Request {
                    check_id: check_id.into(),
                    ..request.clone()
                }
                .encode()
                .is_err()
            );
        }
        assert!(Request::decode(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
    }

    #[test]
    fn approved_check_receipt_preserves_failure_and_refuses_unobserved_exit_or_unbounded_diagnostics()
     {
        let actual = receipt();
        assert_eq!(
            Receipt::decode(&actual.encode().unwrap())
                .unwrap()
                .exit_code,
            Some(1)
        );
        let mut changed = actual.clone();
        changed.cleanup_observed = false;
        assert!(changed.encode().is_err());
        changed.outcome = Outcome::Failed;
        assert!(
            changed.encode().is_ok(),
            "failed cleanup remains reportable as failed execution"
        );
        changed = actual.clone();
        changed.exit_code = None;
        assert!(changed.encode().is_err());
        changed = actual.clone();
        changed.signal = Some(9);
        assert!(changed.encode().is_err());
        changed = actual.clone();
        changed.stdout.preview = "x".repeat(MAX_PREVIEW_BYTES + 1);
        assert!(changed.encode().is_err());
        changed = actual.clone();
        changed.stdout.sha256 = None;
        assert!(
            changed.encode().is_err(),
            "captured diagnostic must identify the complete stream"
        );
        changed = actual.clone();
        changed.binding.host = [0; 32];
        assert!(changed.encode().is_err());
        let duplicate = String::from_utf8(actual.encode().unwrap())
            .unwrap()
            .replacen('{', r#"{"version":1,"#, 1);
        assert!(Receipt::decode(duplicate.as_bytes()).is_err());
        assert!(Receipt::decode(&vec![b' '; MAX_RECEIPT_BYTES + 1]).is_err());
    }

    #[test]
    fn approved_check_receipt_carries_a_bounded_supervision_failure_beside_empty_streams() {
        let mut value = receipt();
        value.outcome = Outcome::Failed;
        value.exit_code = None;
        value.cleanup_observed = false;
        value.stdout = value.stderr.clone();
        value.failure = Some(Diagnostic {
            sha256: Some("f".repeat(64)),
            preview: "user scope bus unavailable".into(),
            truncated: false,
        });
        let encoded = value.encode().unwrap();
        assert_eq!(Receipt::decode(&encoded).unwrap(), value);
        let duplicate = String::from_utf8(encoded).unwrap().replacen(
            "\"failure\":",
            "\"failure\":null,\"failure\":",
            1,
        );
        assert!(Receipt::decode(duplicate.as_bytes()).is_err());
        for invalid in [
            Diagnostic {
                sha256: None,
                preview: "unbound reason".into(),
                truncated: false,
            },
            Diagnostic {
                sha256: Some("F".repeat(64)),
                preview: "reason".into(),
                truncated: false,
            },
            Diagnostic {
                sha256: Some("f".repeat(64)),
                preview: "x".repeat(MAX_PREVIEW_BYTES + 1),
                truncated: true,
            },
        ] {
            value.failure = Some(invalid);
            assert!(value.encode().is_err());
            assert!(Receipt::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
}
