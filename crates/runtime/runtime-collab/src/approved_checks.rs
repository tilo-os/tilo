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
            && [&self.stdout, &self.stderr].into_iter().all(|v| {
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
    /// No fallback exists on brokers without this method.
    ///
    /// # Errors
    /// Refuses invalid requests, unavailable transport, stale custody or invalid replies.
    pub async fn approved_check(&self, request: &Request) -> Result<Receipt, crate::NotAvailable> {
        let encoded = request
            .encode()
            .map_err(|error| crate::NotAvailable::Capnp(capnp::Error::failed(error)))?;
        let mut call = self.inner.approved_check_request();
        call.get().set_request(&encoded);
        let response = call.send().promise.await?;
        let receipt = Receipt::decode(response.get()?.get_receipt()?)
            .map_err(|error| crate::NotAvailable::Capnp(capnp::Error::failed(error)))?;
        if receipt.invocation != request.invocation || receipt.check_id != request.check_id {
            return Err(crate::NotAvailable::Capnp(capnp::Error::failed(
                "approved-check response correlation changed".into(),
            )));
        }
        Ok(receipt)
    }
}

/// One result of a pulled check-specific observation capability.
#[cfg(feature = "client")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReportedProgressUpdate {
    Report(Report),
    Receipt(Box<Receipt>),
}

/// Pulled reports on the original capability; this does not own an execution job.
#[cfg(feature = "client")]
pub struct ApprovedCheckReportedProgressStream {
    inner: crate::collab_capnp::approved_check_reported_progress_stream::Client,
    request: Request,
    sequence: u64,
    terminal: bool,
    disabled: bool,
}

#[cfg(feature = "client")]
fn report_transport_error(error: impl Into<String>) -> crate::NotAvailable {
    crate::NotAvailable::Capnp(capnp::Error::failed(error.into()))
}

#[cfg(feature = "client")]
impl crate::ChannelHandle {
    /// Open the reported-observation lane for this exact accepted check.
    ///
    /// # Errors
    /// Refuses invalid requests or unsupported brokers without falling back to @58.
    pub async fn approved_check_reported_progress(
        &self,
        request: &Request,
    ) -> Result<ApprovedCheckReportedProgressStream, crate::NotAvailable> {
        let encoded = request.encode().map_err(report_transport_error)?;
        let mut call = self.inner.approved_check_reported_progress_request();
        call.get().set_request(&encoded);
        let response = call.send().promise.await?;
        Ok(ApprovedCheckReportedProgressStream {
            inner: response.get()?.get_stream()?,
            request: request.clone(),
            sequence: 0,
            terminal: false,
            disabled: false,
        })
    }
}

#[cfg(feature = "client")]
impl ApprovedCheckReportedProgressStream {
    /// Pull one bounded observation or the unchanged terminal receipt.
    ///
    /// # Errors
    /// Refuses ambiguous fields, malformed or miscorrelated observations, nonmonotonic
    /// sequences and reads after terminal completion. Any refusal freezes this client.
    pub async fn read(&mut self) -> Result<ReportedProgressUpdate, crate::NotAvailable> {
        if self.terminal {
            return Err(report_transport_error(
                "approved-check report lane is terminal",
            ));
        }
        let result = self.read_next().await;
        if !matches!(result, Ok(ReportedProgressUpdate::Report(_))) {
            self.terminal = true;
        }
        result
    }

    async fn read_next(&mut self) -> Result<ReportedProgressUpdate, crate::NotAvailable> {
        let response = self.inner.read_request().send().promise.await?;
        let reply = response.get()?;
        let report = reply.get_report()?;
        let receipt = reply.get_receipt()?;
        let error = reply.get_error()?;
        if !matches!(
            (report.is_empty(), receipt.is_empty(), error.is_empty()),
            (false, true, true) | (true, false, true) | (true, true, false)
        ) {
            return Err(report_transport_error(
                "ambiguous approved-check report reply",
            ));
        }
        if !error.is_empty() {
            if error.len() > MAX_REPORT_BYTES {
                return Err(report_transport_error(
                    "approved-check report refusal too large",
                ));
            }
            return Err(report_transport_error(
                error
                    .to_str()
                    .map_err(|error| report_transport_error(error.to_string()))?
                    .to_owned(),
            ));
        }
        if !receipt.is_empty() {
            let receipt = Receipt::decode(receipt).map_err(report_transport_error)?;
            if receipt.invocation != self.request.invocation
                || receipt.check_id != self.request.check_id
            {
                return Err(report_transport_error(
                    "approved-check response correlation changed",
                ));
            }
            return Ok(ReportedProgressUpdate::Receipt(Box::new(receipt)));
        }
        let report = Report::decode(report).map_err(report_transport_error)?;
        if report.invocation != self.request.invocation
            || report.check_id != self.request.check_id
            || report.sequence <= self.sequence
        {
            return Err(report_transport_error(
                "approved-check report correlation or sequence changed",
            ));
        }
        if self.disabled {
            return Err(report_transport_error(
                "approved-check reporting lane is disabled",
            ));
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
    type ReportFrame = (Vec<u8>, Vec<u8>, String);

    #[cfg(feature = "client")]
    struct WireReports(std::cell::RefCell<std::collections::VecDeque<ReportFrame>>);

    #[cfg(feature = "client")]
    impl crate::collab_capnp::approved_check_reported_progress_stream::Server for WireReports {
        fn read(
            self: capnp::capability::Rc<Self>,
            _: crate::collab_capnp::approved_check_reported_progress_stream::ReadParams,
            mut results: crate::collab_capnp::approved_check_reported_progress_stream::ReadResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let Some((report, receipt, error)) = self.0.borrow_mut().pop_front() else {
                return std::future::ready(Err(capnp::Error::failed(
                    "unexpected extra read".into(),
                )));
            };
            let mut reply = results.get();
            reply.set_report(&report);
            reply.set_receipt(&receipt);
            reply.set_error(error.as_str());
            std::future::ready(Ok(()))
        }
    }

    #[cfg(feature = "client")]
    fn wire_reports(
        frames: Vec<(Vec<u8>, Vec<u8>, String)>,
    ) -> ApprovedCheckReportedProgressStream {
        ApprovedCheckReportedProgressStream {
            inner: capnp_rpc::new_client(WireReports(std::cell::RefCell::new(frames.into()))),
            request: Request {
                version: 1,
                invocation: ID.into(),
                check_id: "regression".into(),
            },
            sequence: 0,
            terminal: false,
            disabled: false,
        }
    }

    #[cfg(feature = "client")]
    #[tokio::test(flavor = "current_thread")]
    async fn approved_check_report_stream_keeps_terminal_receipt_separate_and_final() {
        let observed = report(1);
        let terminal = receipt();
        let mut stream = wire_reports(vec![
            (observed.encode().unwrap(), Vec::new(), String::new()),
            (Vec::new(), terminal.encode().unwrap(), String::new()),
            (report(2).encode().unwrap(), Vec::new(), String::new()),
        ]);
        assert_eq!(
            stream.read().await.unwrap(),
            ReportedProgressUpdate::Report(observed)
        );
        assert_eq!(
            stream.read().await.unwrap(),
            ReportedProgressUpdate::Receipt(Box::new(terminal))
        );
        assert!(
            stream.read().await.is_err(),
            "late observation cannot replace receipt"
        );
    }

    #[cfg(feature = "client")]
    #[tokio::test(flavor = "current_thread")]
    async fn approved_check_disabled_report_lane_accepts_only_terminal_receipt() {
        let disabled = Report {
            observation: ReportObservation::Disabled {
                reason: LaneRefusal::InvalidReport,
            },
            ..report(1)
        };
        let mut stream = wire_reports(vec![
            (disabled.encode().unwrap(), Vec::new(), String::new()),
            (Vec::new(), receipt().encode().unwrap(), String::new()),
        ]);
        assert_eq!(
            stream.read().await.unwrap(),
            ReportedProgressUpdate::Report(disabled.clone())
        );
        assert_eq!(
            stream.read().await.unwrap(),
            ReportedProgressUpdate::Receipt(Box::new(receipt()))
        );
        let mut stream = wire_reports(vec![
            (disabled.encode().unwrap(), Vec::new(), String::new()),
            (report(2).encode().unwrap(), Vec::new(), String::new()),
        ]);
        assert!(matches!(
            stream.read().await.unwrap(),
            ReportedProgressUpdate::Report(_)
        ));
        assert!(
            stream.read().await.is_err(),
            "disabled observation lane cannot restart"
        );
    }

    #[cfg(feature = "client")]
    #[tokio::test(flavor = "current_thread")]
    async fn approved_check_report_stream_refuses_ambiguous_foreign_stale_and_oversized() {
        let mut foreign = report(1);
        foreign.invocation = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into();
        for frame in [
            (Vec::new(), Vec::new(), String::new()),
            (
                report(1).encode().unwrap(),
                receipt().encode().unwrap(),
                String::new(),
            ),
            (foreign.encode().unwrap(), Vec::new(), String::new()),
            (vec![b' '; MAX_REPORT_BYTES + 1], Vec::new(), String::new()),
            (Vec::new(), Vec::new(), "x".repeat(MAX_REPORT_BYTES + 1)),
        ] {
            let mut stream = wire_reports(vec![frame]);
            assert!(stream.read().await.is_err());
            assert!(stream.read().await.is_err(), "refusal freezes lane");
        }
        let mut stream = wire_reports(vec![
            (report(3).encode().unwrap(), Vec::new(), String::new()),
            (report(3).encode().unwrap(), Vec::new(), String::new()),
        ]);
        assert!(matches!(
            stream.read().await.unwrap(),
            ReportedProgressUpdate::Report(_)
        ));
        assert!(stream.read().await.is_err());
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

    #[cfg(feature = "client")]
    struct WireAbi;

    #[cfg(feature = "client")]
    impl crate::collab_capnp::channel_handle::Server for WireAbi {
        fn local_file_clipboard(
            self: capnp::capability::Rc<Self>,
            _: crate::collab_capnp::channel_handle::LocalFileClipboardParams,
            mut results: crate::collab_capnp::channel_handle::LocalFileClipboardResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results.get().set_reply_json(b"clipboard56");
            std::future::ready(Ok(()))
        }

        fn audio_output_command(
            self: capnp::capability::Rc<Self>,
            _: crate::collab_capnp::channel_handle::AudioOutputCommandParams,
            mut results: crate::collab_capnp::channel_handle::AudioOutputCommandResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results.get().set_reply(b"audio57");
            std::future::ready(Ok(()))
        }

        fn approved_check(
            self: capnp::capability::Rc<Self>,
            _: crate::collab_capnp::channel_handle::ApprovedCheckParams,
            mut results: crate::collab_capnp::channel_handle::ApprovedCheckResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results.get().set_receipt(b"approved58");
            std::future::ready(Ok(()))
        }
    }

    #[cfg(feature = "client")]
    #[tokio::test(flavor = "current_thread")]
    async fn approved_check_reports_unsupported_broker_refuses_without_terminal_fallback() {
        let channel = crate::ChannelHandle {
            inner: std::rc::Rc::new(capnp_rpc::new_client(WireAbi)),
        };
        let request = Request {
            version: 1,
            invocation: ID.into(),
            check_id: "regression".into(),
        };
        assert!(
            channel
                .approved_check_reported_progress(&request)
                .await
                .is_err()
        );
    }

    /// Literal wire slots model an independently compiled existing caller,
    /// rather than allowing client and server renumbering to cancel each other.
    #[cfg(feature = "client")]
    #[tokio::test(flavor = "current_thread")]
    async fn approved_check_append_preserves_clipboard_and_audio_wire_slots() {
        use crate::collab_capnp::channel_handle;
        use capnp::capability::Request as RpcRequest;
        use capnp::traits::HasTypeId;

        let client: channel_handle::Client = capnp_rpc::new_client(WireAbi);
        let clipboard: RpcRequest<
            channel_handle::local_file_clipboard_params::Owned,
            channel_handle::local_file_clipboard_results::Owned,
        > = client
            .client
            .new_call(channel_handle::Client::TYPE_ID, 56, None);
        assert_eq!(
            clipboard
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_reply_json()
                .unwrap(),
            b"clipboard56"
        );
        let audio: RpcRequest<
            channel_handle::audio_output_command_params::Owned,
            channel_handle::audio_output_command_results::Owned,
        > = client
            .client
            .new_call(channel_handle::Client::TYPE_ID, 57, None);
        assert_eq!(
            audio
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_reply()
                .unwrap(),
            b"audio57"
        );
        let approved: RpcRequest<
            channel_handle::approved_check_params::Owned,
            channel_handle::approved_check_results::Owned,
        > = client
            .client
            .new_call(channel_handle::Client::TYPE_ID, 58, None);
        assert_eq!(
            approved
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_receipt()
                .unwrap(),
            b"approved58"
        );
        assert_eq!(
            client
                .approved_check_request()
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_receipt()
                .unwrap(),
            b"approved58"
        );
    }
}
