//! Private task delivery data and process-lifetime recipient custody.
//! These retained values do not restore the retired enrolled-task wire plane.

use crate::{TaskRunEnvelope, TaskRunPlanning};

/// Private managed delivery, independently versioned from the legacy task transport.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolledTaskEnvelope {
    pub version: u16,
    pub message_id: String,
    pub task_id: String,
    pub run_id: String,
    pub launch_id: String,
    pub runtime_epoch: String,
    pub recipient_agent: String,
    pub channel_lease: String,
    pub task: TaskRunEnvelope,
    /// The recipient agent's own earlier tasks (requests, steps, outcomes,
    /// replies) as compact JSON, composed by the host from its durable
    /// records. Private to that agent, display input only, never authority.
    /// Absent on older hosts; omitted from the wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<String>,
}
/// Most bytes of [`EnrolledTaskEnvelope::history`].
pub const MAX_ENROLLED_HISTORY_BYTES: usize = 12 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolledTaskAcceptance {
    pub version: u16,
    pub message_id: String,
    pub task_id: String,
    pub run_id: String,
    pub launch_id: String,
    pub runtime_epoch: String,
    pub recipient_agent: String,
    pub channel_lease: String,
    pub acceptance_sequence: u64,
}
impl EnrolledTaskEnvelope {
    pub const MAX_JSON_BYTES: usize = 128 * 1024;
    /// Check the envelope's version, labels and bounds.
    ///
    /// # Errors
    ///
    /// Returns a description of the first violated constraint.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.task.version != 1
            || self.task.run_id == 0
            || self.task.task_id != self.task_id
            || self.task.planning != TaskRunPlanning::Model
            || self.task.title.is_empty()
            || self.task.title.len() > 32 * 1024
            || self.task.title.chars().count() > 8000
            || self.task.instruction().trim().is_empty()
            || self.task.instruction().len() > 32 * 1024
            || self
                .history
                .as_ref()
                .is_some_and(|history| history.len() > MAX_ENROLLED_HISTORY_BYTES)
            || !enrolled_label(&self.task.lease)
            || [
                &self.message_id,
                &self.task_id,
                &self.run_id,
                &self.launch_id,
                &self.runtime_epoch,
                &self.recipient_agent,
                &self.channel_lease,
            ]
            .iter()
            .any(|s| !enrolled_label(s))
            || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > Self::MAX_JSON_BYTES
        {
            return Err("invalid enrolled task envelope".into());
        }
        if let Some(budget) = self.task.execution_budget {
            budget.validate().map_err(str::to_owned)?;
        }
        Ok(())
    }
    fn acceptance(&self, sequence: u64) -> EnrolledTaskAcceptance {
        EnrolledTaskAcceptance {
            version: 1,
            message_id: self.message_id.clone(),
            task_id: self.task_id.clone(),
            run_id: self.run_id.clone(),
            launch_id: self.launch_id.clone(),
            runtime_epoch: self.runtime_epoch.clone(),
            recipient_agent: self.recipient_agent.clone(),
            channel_lease: self.channel_lease.clone(),
            acceptance_sequence: sequence,
        }
    }
}
fn enrolled_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
impl EnrolledTaskAcceptance {
    pub fn matches(&self, envelope: &EnrolledTaskEnvelope) -> bool {
        self.acceptance_sequence != 0 && *self == envelope.acceptance(self.acceptance_sequence)
    }
}
/// Process-lifetime recipient deduplication. No eviction, persistence import or replay permission.
/// The host must retain the matching live process/channel custody; after restart it cannot resend.
pub struct EnrolledRecipientCustody {
    recipient: [String; 4],
    retained_bytes: usize,
    retained: std::collections::HashMap<String, (EnrolledTaskEnvelope, EnrolledTaskAcceptance)>,
}
pub struct EnrolledAcceptanceDecision {
    pub acceptance: EnrolledTaskAcceptance,
    /// True once, before the caller waits for the host's actual acceptance RPC reply.
    /// An ambiguous reply ends the task channel; it never rolls this reservation back.
    pub execute: bool,
}
impl EnrolledRecipientCustody {
    pub const CAPACITY: usize = 1024;
    /// Custody for one exact enrolled recipient.
    ///
    /// # Errors
    ///
    /// Refuses a recipient with an invalid label.
    pub fn new(agent: &str, launch: &str, epoch: &str, channel: &str) -> Result<Self, String> {
        let recipient = [agent, launch, epoch, channel].map(str::to_owned);
        if recipient.iter().any(|s| !enrolled_label(s)) {
            return Err("invalid enrolled recipient".into());
        }
        Ok(Self {
            recipient,
            retained_bytes: 0,
            retained: std::collections::HashMap::default(),
        })
    }
    /// Accept an envelope once, replaying the retained acceptance for an exact retry.
    ///
    /// # Errors
    ///
    /// Refuses invalid, misaddressed, changed or reused envelopes and full retention.
    pub fn accept(
        &mut self,
        envelope: &EnrolledTaskEnvelope,
    ) -> Result<EnrolledAcceptanceDecision, String> {
        envelope.validate()?;
        if self.recipient
            != [
                envelope.recipient_agent.clone(),
                envelope.launch_id.clone(),
                envelope.runtime_epoch.clone(),
                envelope.channel_lease.clone(),
            ]
        {
            return Err("wrong enrolled recipient custody".into());
        }
        if let Some((prior, acceptance)) = self.retained.get(&envelope.message_id) {
            if prior != envelope {
                return Err("enrolled message changed after acceptance".into());
            }
            return Ok(EnrolledAcceptanceDecision {
                acceptance: acceptance.clone(),
                execute: false,
            });
        }
        let bytes = serde_json::to_vec(envelope)
            .map_err(|e| e.to_string())?
            .len();
        let retained_bytes = self
            .retained_bytes
            .checked_add(bytes)
            .ok_or("enrolled retention overflow")?;
        if self.retained.len() >= Self::CAPACITY || retained_bytes > 8 * 1024 * 1024 {
            return Err("enrolled acceptance retention full".into());
        }
        if self.retained.values().any(|(prior, _)| {
            prior.task_id == envelope.task_id
                || prior.run_id == envelope.run_id
                || prior.task.run_id == envelope.task.run_id
        }) {
            return Err("enrolled task/run reused for another message".into());
        }
        let sequence = u64::try_from(self.retained.len())
            .map_err(|_| "acceptance sequence overflow")?
            .checked_add(1)
            .ok_or("acceptance sequence overflow")?;
        let acceptance = envelope.acceptance(sequence);
        self.retained_bytes = retained_bytes;
        self.retained.insert(
            envelope.message_id.clone(),
            (envelope.clone(), acceptance.clone()),
        );
        Ok(EnrolledAcceptanceDecision {
            acceptance,
            execute: true,
        })
    }
}
