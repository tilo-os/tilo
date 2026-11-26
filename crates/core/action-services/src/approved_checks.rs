//! Native project checks selected by ID. Commands and execution bounds are host-only.
//!
//! An app's `run-approved-check` action runs one by calling the device's [`RUN`] on [`RESOURCE`]
//! of [`PROVIDER`] as that action's work, naming only the check's id ([`ARGUMENT`]). The device
//! reads which app, record and resource the check is for from the invocation its dispatch
//! delivered to the app, never from the call.
use serde::{Deserialize, Serialize};

pub const MAX_RECEIPT_BYTES: usize = 16384;
pub const MAX_PREVIEW_BYTES: usize = 2048;
/// The app action whose work an approved check is.
pub const ACTION_ID: &str = "run-approved-check";
/// The device's provider that runs approved checks.
pub const PROVIDER: &str = "tilo.approved-checks";
/// The resource its action is on.
pub const RESOURCE: &str = "checks";
/// Run the approved check `{check_id}` as the work of the app's `run-approved-check`: answers
/// the [`Receipt`] of what the device observed.
pub const RUN: &str = "run";
/// The argument naming the check.
pub const ARGUMENT: &str = "check_id";

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
    value.len() == 36
        && uuid::Uuid::try_parse(value).is_ok()
        && !value.bytes().any(|b| b.is_ascii_uppercase())
        && value != "00000000-0000-0000-0000-000000000000"
}

/// Whether `value` is a check's id: what the device's setting names it by, never a command.
#[must_use]
pub fn is_check_id(value: &str) -> bool {
    identifier(value)
}

/// Closed reasons for disabling informational observation, never execution outcomes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointRefusal {
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
        reason: EndpointRefusal,
    },
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
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err("approved-check receipt too large".into());
        }
        let receipt: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        receipt.encode()?;
        Ok(receipt)
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

    tilo_test_env::scenario! {
        fn approved_check_messages_are_closed_and_bounded;

        phase approved_check_ids_name_a_check_never_a_command;
        {
            assert!(is_check_id("regression"));
            for check_id in ["../regression", "regression;id", "-option", "", "run test"] {
                assert!(!is_check_id(check_id), "{check_id}");
            }
        }

        phase approved_check_receipt_carries_a_bounded_supervision_failure_beside_empty_streams;
        {
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
}
