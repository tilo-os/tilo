//! Bounded task-control observations on the existing authenticated task channel.
//! These values are instruction data, never an authority envelope or a grant.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDirection {
    pub revision: u64,
    pub author: String,
    pub text: String,
    pub issued_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRunControl {
    pub version: u16,
    pub task_id: String,
    pub run_epoch: u64,
    pub from_revision: u64,
    pub applied_revision: u64,
    pub latest_revision: u64,
    pub directions: Vec<TaskDirection>,
    pub has_more: bool,
    /// Exact-run untrusted context, never a direction or a capability grant.
    #[serde(default)]
    pub messages: Vec<crate::TaskMessage>,
}
impl TaskRunControl {
    pub const MAX_BYTES: usize = 64 * 1024;
    pub const MAX_DIRECTIONS: usize = 8;
    /// Validate a bounded, contiguous instruction page from the retained frontier.
    ///
    /// # Errors
    /// Rejects an invalid identity, revision gap, incomplete marker or oversized page.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.run_epoch == 0
            || self.task_id.is_empty()
            || self.task_id.len() > 256
            || self.from_revision > self.latest_revision
            || self.applied_revision > self.latest_revision
            || self.directions.len() > Self::MAX_DIRECTIONS
        {
            return Err("invalid native task control".into());
        }
        if self.messages.len() > 4 {
            return Err("oversized task inbox page".into());
        }
        for message in &self.messages {
            message.validate()?;
            if message.recipient.task_id != self.task_id
                || message.recipient.run_epoch != self.run_epoch
            {
                return Err("task inbox page belongs to another run".into());
            }
        }
        let mut prior = self.from_revision;
        for direction in &self.directions {
            if Some(direction.revision) != prior.checked_add(1)
                || direction.revision > self.latest_revision
                || direction.author.is_empty()
                || direction.author.len() > 1024
                || direction.text.is_empty()
                || direction.text.len() > 8192
            {
                return Err("invalid native task direction page".into());
            }
            prior = direction.revision;
        }
        if self.has_more != (prior < self.latest_revision)
            || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > Self::MAX_BYTES
        {
            return Err("incomplete or oversized native task direction page".into());
        }
        Ok(())
    }
}

/// Observation of the exact leased run; absent control preserves older hosts.
#[derive(Clone, Debug)]
pub struct TaskRunStatus {
    pub cancelled: bool,
    pub control: Option<TaskRunControl>,
}

/// A revision marker on a host-issued opaque run token, not proof of authority.
#[must_use]
pub fn native_task_revision(lease: &str) -> Option<(&str, u64)> {
    let (base, revision) = lease.rsplit_once(':')?;
    let digest = base.strip_prefix("native:")?;
    if digest.len() != 32 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some((base, revision.parse().ok()?))
}
