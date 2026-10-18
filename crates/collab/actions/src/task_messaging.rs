//! Private task-to-task context. These values confer no authority.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRunAddress {
    pub task_id: String,
    pub run_epoch: u64,
    pub agent_id: String,
}
impl TaskRunAddress {
    /// # Errors
    /// Refuses malformed task identities, agents and zero run epochs.
    pub fn validate(&self) -> Result<(), String> {
        if !self.task_id.starts_with("task:")
            || self.task_id.len() > 256
            || self.run_epoch == 0
            || self.agent_id.is_empty()
            || self.agent_id.len() > 512
            || self
                .task_id
                .chars()
                .chain(self.agent_id.chars())
                .any(char::is_control)
        {
            return Err("invalid task mailbox address".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMessageSend {
    pub run_epoch: u64,
    pub text: String,
}
impl TaskMessageSend {
    pub const MAX_TEXT_BYTES: usize = 8192;
    /// # Errors
    /// Refuses zero epochs and empty or oversized private text.
    pub fn validate(&self) -> Result<(), String> {
        if self.run_epoch == 0
            || self.text.trim().is_empty()
            || self.text.len() > Self::MAX_TEXT_BYTES
        {
            return Err("empty or oversized private task message".into());
        }
        Ok(())
    }
}

/// Read-only wait on the caller's own exact native task run. No sender or
/// authority can be supplied by an action argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMessageWait {
    pub run_epoch: u64,
    pub after_sequence: u64,
    pub timeout_ms: u32,
}
impl TaskMessageWait {
    pub const MAX_TIMEOUT_MS: u32 = 30_000;
    /// # Errors
    /// Refuses an invalid epoch or a wait beyond the bounded observation window.
    pub fn validate(&self) -> Result<(), String> {
        if self.run_epoch == 0 || self.timeout_ms == 0 || self.timeout_ms > Self::MAX_TIMEOUT_MS {
            return Err("invalid task mailbox wait epoch or timeout".into());
        }
        Ok(())
    }
}

/// Owner-authenticated inbox page, not model-authored content or a delivery ACK.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMessageWaitResult {
    pub task_run: TaskRunAddress,
    pub messages: Vec<TaskMessage>,
    pub next_sequence: u64,
    pub has_more: bool,
    pub timed_out: bool,
}
impl std::fmt::Debug for TaskMessageWaitResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TaskMessageWaitResult { private: redacted }")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskMessageStatus {
    Accepted,
    DeliveredToModel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMessage {
    pub version: u16,
    pub message_id: String,
    pub source: TaskRunAddress,
    pub recipient: TaskRunAddress,
    pub received_at: i64,
    pub sequence: u64,
    pub text: String,
    pub status: TaskMessageStatus,
}
impl TaskMessage {
    /// # Errors
    /// Refuses invalid run addresses, message identities, versions or text.
    pub fn validate(&self) -> Result<(), String> {
        self.source.validate()?;
        self.recipient.validate()?;
        let id = uuid::Uuid::parse_str(&self.message_id).map_err(|e| e.to_string())?;
        if self.version != 1
            || id.is_nil()
            || id.to_string() != self.message_id
            || self.sequence == 0
        {
            return Err("invalid private task message identity or version".into());
        }
        TaskMessageSend {
            run_epoch: self.recipient.run_epoch,
            text: self.text.clone(),
        }
        .validate()
    }
}

/// Exact owner-accepted prose for one automatic task run. Never public room state.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReport {
    pub invocation: uuid::Uuid,
    pub source_device: [u8; 32],
    pub role: TaskReportRole,
    /// Governor wall-clock acceptance time, nanoseconds since the UNIX epoch.
    pub accepted_at_unix_ns: u64,
    pub sequence: u64,
    pub task_run: TaskRunAddress,
    pub actor: String,
    pub kind: TaskReportKind,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<TaskReportPresentation>,
}

/// Exact private model-turn snapshot. Kept here to avoid a dependency cycle with
/// the collaboration wire types; its serialized fields match `TaskPresentation`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReportPresentation {
    pub run: String,
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<i64>,
    pub text: String,
    pub reasoning: String,
    pub finish: Option<TaskReportPresentationFinish>,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReportPresentationFinish {
    Complete,
    Truncated,
    Refused,
    Cancelled,
    Failed,
}

impl TaskReportPresentation {
    /// Combined UTF-8 prose budget; report body duplicates `text` and is not counted twice.
    pub const MAX_PROSE_BYTES: usize = 8 * 1024;

    /// # Errors
    /// Refuses invalid identities or oversized bodies without changing any byte.
    pub fn validate(&self) -> Result<(), String> {
        if self.run.is_empty()
            || self.run.len() > 192
            || self.run.chars().any(char::is_control)
            || self
                .text
                .len()
                .saturating_add(self.reasoning.len())
                .saturating_add(self.detail.len())
                > Self::MAX_PROSE_BYTES
            || self.detail.len() > 2 * 1024
        {
            return Err("invalid private task presentation".into());
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReportRole {
    Assistant,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskReportKind {
    Presentation,
    Progress,
    Finish,
    Failure,
}
impl std::fmt::Debug for TaskReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TaskReport { private: redacted }")
    }
}
impl TaskReport {
    /// # Errors
    /// Refuses malformed report identities or empty and oversized private prose.
    pub fn validate(&self) -> Result<(), String> {
        self.task_run.validate()?;
        if let Some(presentation) = &self.presentation {
            presentation.validate()?;
            if self.kind != TaskReportKind::Presentation || self.body != presentation.text {
                return Err("private task presentation body mismatch".into());
            }
        } else if self.kind == TaskReportKind::Presentation {
            return Err("private task presentation absent".into());
        }
        if self.invocation.is_nil()
            || self.accepted_at_unix_ns == 0
            || self.sequence == 0
            || (self.actor != "host" && self.actor != format!("agent:{}", self.task_run.agent_id))
            || self.body.len() > TaskReportPresentation::MAX_PROSE_BYTES
            || (self.presentation.is_none() && self.body.trim().is_empty())
        {
            return Err("invalid private task report".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;

    fn report() -> TaskReport {
        let text = format!("  exact answer\n{}", "é".repeat(3000));
        TaskReport {
            invocation: uuid::Uuid::new_v4(),
            source_device: [7; 32],
            role: TaskReportRole::Assistant,
            accepted_at_unix_ns: 1,
            sequence: 1,
            task_run: TaskRunAddress {
                task_id: "task:answer".into(),
                run_epoch: 1,
                agent_id: "scribe".into(),
            },
            actor: "agent:scribe".into(),
            kind: TaskReportKind::Presentation,
            body: text.clone(),
            presentation: Some(TaskReportPresentation {
                run: "turn:1".into(),
                sequence: 2,
                received_at: Some(123),
                text,
                reasoning: "  private reasoning\n".into(),
                finish: Some(TaskReportPresentationFinish::Complete),
                detail: "private detail".into(),
            }),
        }
    }

    #[test]
    fn presentation_report_preserves_complete_exact_answer_and_all_fields() {
        let report = report();
        report.validate().unwrap();
        let bytes = serde_json::to_vec(&report).unwrap();
        let decoded: TaskReport = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, report);
        assert_eq!(decoded.body, decoded.presentation.unwrap().text);
        assert_eq!(format!("{report:?}"), "TaskReport { private: redacted }");
    }

    #[test]
    fn presentation_report_preserves_each_finish_without_rewriting_metadata() {
        for finish in [
            None,
            Some(TaskReportPresentationFinish::Complete),
            Some(TaskReportPresentationFinish::Truncated),
            Some(TaskReportPresentationFinish::Refused),
            Some(TaskReportPresentationFinish::Cancelled),
            Some(TaskReportPresentationFinish::Failed),
        ] {
            let mut report = report();
            report.presentation.as_mut().unwrap().finish = finish;
            report.validate().unwrap();
            let decoded: TaskReport =
                serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
            assert_eq!(decoded, report);
        }
    }

    #[test]
    fn presentation_report_rejects_oversize_and_mismatched_bodies_without_truncation() {
        for field in ["run", "text", "reasoning", "detail"] {
            let mut value = serde_json::to_value(report()).unwrap();
            let limit = match field {
                "run" => 192,
                "text" | "reasoning" => TaskReportPresentation::MAX_PROSE_BYTES,
                _ => 2 * 1024,
            };
            value["presentation"][field] = "é".repeat(limit / 2 + 1).into();
            if field == "text" {
                value["body"] = value["presentation"][field].clone();
            }
            let oversized: TaskReport = serde_json::from_value(value).unwrap();
            let original = oversized.clone();
            assert!(oversized.validate().is_err(), "{field}");
            assert_eq!(oversized, original);
        }
        let mut mismatch = report();
        mismatch.body.push('!');
        assert!(mismatch.validate().is_err());
        mismatch.presentation = None;
        assert!(mismatch.validate().is_err());
    }

    #[test]
    fn presentation_prose_budget_counts_utf8_bytes_across_all_fields() {
        for field in ["text", "reasoning", "detail"] {
            let mut report = report();
            let presentation = report.presentation.as_mut().unwrap();
            presentation.text = "é".repeat(3072);
            presentation.reasoning = "é".repeat(512);
            presentation.detail = "é".repeat(512);
            report.body = presentation.text.clone();
            report.validate().unwrap(); // Exactly 8192 bytes, not 8192 characters.
            let before = serde_json::to_vec(&report).unwrap();
            let presentation = report.presentation.as_mut().unwrap();
            match field {
                "text" => {
                    presentation.text.push('x');
                    report.body = presentation.text.clone();
                }
                "reasoning" => presentation.reasoning.push('x'),
                _ => presentation.detail.push('x'),
            }
            let oversized = report.clone();
            assert!(report.validate().is_err(), "{field}");
            assert_eq!(report, oversized);
            assert_ne!(serde_json::to_vec(&report).unwrap(), before);
        }
    }

    #[test]
    fn legacy_reports_default_to_no_presentation_and_keep_original_bounds() {
        let mut value = serde_json::to_value(report()).unwrap();
        value.as_object_mut().unwrap().remove("presentation");
        value["kind"] = "finish".into();
        value["body"] = "exact legacy answer\n ".into();
        let legacy: TaskReport = serde_json::from_value(value).unwrap();
        legacy.validate().unwrap();
        assert!(legacy.presentation.is_none());
        for kind in [
            TaskReportKind::Progress,
            TaskReportKind::Finish,
            TaskReportKind::Failure,
        ] {
            let mut boundary = legacy.clone();
            boundary.kind = kind;
            boundary.body = "é".repeat(4096);
            boundary.validate().unwrap();
            boundary.body.push('x');
            let original = boundary.clone();
            assert!(boundary.validate().is_err());
            assert_eq!(boundary, original);
        }
    }
}

/// Only host-verified invocation receipts may construct a displayed output.
/// This is data, not an action argument or proof verifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOutputRef {
    pub version: u16,
    pub task_run: TaskRunAddress,
    pub invocation: String,
    pub provider: String,
    pub resource: String,
    pub open_action: String,
    pub durable_proof: String,
    pub status: TaskOutputStatus,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskOutputStatus {
    Created,
    Updated,
    Viewed,
    Returned,
}

impl TaskOutputRef {
    /// # Errors
    /// Refuses invalid versions, resource references or receipt identities.
    pub fn validate(&self) -> Result<(), String> {
        self.task_run.validate()?;
        let id = uuid::Uuid::parse_str(&self.invocation).map_err(|e| e.to_string())?;
        if self.version != 1
            || id.is_nil()
            || id.to_string() != self.invocation
            || self.open_action != "open"
            || self.provider.is_empty()
            || self.provider.len() > 2048
            || self.resource.is_empty()
            || self.resource.len() > 2048
            || self.durable_proof.len() != 64
            || !self
                .durable_proof
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid task output receipt".into());
        }
        Ok(())
    }
}
