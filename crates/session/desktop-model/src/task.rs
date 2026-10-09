//! Room-owned tasks and effect receipts.

use serde::{Deserialize, Serialize};

/// Largest full instruction retained for one queued agent task.
pub const MAX_WORKSPACE_TASK_INSTRUCTION_BYTES: usize = 32 * 1024;
/// Largest direction a person may append to an active task.
pub const MAX_WORKSPACE_TASK_DIRECTION_BYTES: usize = 8 * 1024;
/// Largest terminal or abandonment summary retained in task history.
pub const MAX_WORKSPACE_TASK_SUMMARY_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTaskStatus {
    Queued,
    InProgress,
    Waiting,
    Done,
    Cancelled,
}

impl WorkspaceTaskStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::InProgress => "In progress",
            Self::Waiting => "Waiting",
            Self::Done => "Done",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTaskRoute {
    /// A person advances the task from the native task surface. No model is
    /// contacted, which keeps Task Center useful on an offline desktop.
    Manual,
    /// An agent may use exported actions and deterministic matching only.
    RulesOnly,
    /// An agent may ask its configured model before dispatching exported
    /// actions. The resulting action trace remains the source of truth.
    Model,
}

impl WorkspaceTaskRoute {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Manual => "Manual · no model",
            Self::RulesOnly => "Agent · rules only",
            Self::Model => "Agent · model assisted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WorkspaceTaskControl;

    #[test]
    fn legacy_task_control_decodes_as_explicit_title_only() {
        let control: WorkspaceTaskControl = serde_json::from_str(
            r#"{"run_epoch":3,"active_run":null,"directions":[],"receipts":[]}"#,
        )
        .unwrap();
        assert_eq!(control.run_epoch, 3);
        assert_eq!(control.pending_revision, 0);
        assert!(control.instruction.is_none());
        assert!(!control.instruction_required);
        assert!(control.execution_budget.is_none());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTaskResource {
    pub app_id: Option<String>,
    pub app_label: Option<String>,
    pub id: String,
    pub kind: String,
    pub label: String,
    /// Canonical direct-Chat request behind an explicitly queued agent task.
    /// The full instruction lives here rather than in the short Task Center
    /// display title, so an offline agent receives it byte-for-byte later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_request: Option<WorkspaceAgentRequest>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceAgentRequest {
    #[serde(default)]
    pub chat_record: String,
    pub channel_id: String,
    pub message_id: String,
    pub instruction: String,
    pub invocation: tilo_actions::InvocationId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTaskAuditEvent {
    pub at: i64,
    pub actor: String,
    pub summary: String,
}

/// One person-authored revision to a task's current instructions.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskDirection {
    pub revision: u64,
    pub invocation: tilo_actions::InvocationId,
    pub author: String,
    pub text: String,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied: Option<WorkspaceTaskDirectionApplied>,
}

impl WorkspaceTaskDirection {
    #[must_use]
    pub fn owner_admitted(
        revision: u64,
        invocation: uuid::Uuid,
        author: String,
        text: String,
        created_at: i64,
    ) -> Self {
        Self {
            revision,
            invocation: tilo_actions::InvocationId(invocation),
            author,
            text,
            created_at,
            applied: None,
        }
    }
}

/// Durable acknowledgement that the assigned agent incorporated a direction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskDirectionApplied {
    pub agent_id: String,
    pub run_epoch: u64,
    pub applied_at: i64,
}

/// The one run currently allowed to act for a task.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskActiveRun {
    pub epoch: u64,
    pub agent_id: String,
    pub started_at: i64,
    /// Highest contiguous direction revision durably incorporated by the agent.
    pub applied_direction_revision: u64,
}

/// Public execution milestones, not model reasoning or action output.
/// A host-created run lease alone is not any of these milestones.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTaskExecutionPhase {
    Accepted,
    Planning,
    Executing,
}

/// Latest authenticated phase of the exact active run. Zero is reserved for
/// the acceptance transaction; later sequences belong to that run's admitted
/// report stream, not the custody-history ordinal or a presentation timer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskExecution {
    pub run_epoch: u64,
    pub sequence: u64,
    pub phase: WorkspaceTaskExecutionPhase,
    pub updated_at: i64,
}

/// A person's durable request to stop one exact active run. The task remains
/// in progress until the leased Agent acknowledges cancellation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskCancellationRequest {
    pub run_epoch: u64,
    pub requested_by: String,
    pub requested_at: i64,
}

/// Result retained beside a task command so an exact retry is deterministic.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTaskMutationResult {
    Queued {
        task_id: String,
    },
    RunStarted {
        task_id: String,
        epoch: u64,
    },
    DirectionAdded {
        task_id: String,
        revision: u64,
    },
    DirectionApplied {
        task_id: String,
        revision: u64,
    },
    Finished {
        task_id: String,
    },
    Abandoned {
        task_id: String,
    },
    ExecutionReported {
        task_id: String,
        sequence: u64,
    },
    /// Appended to preserve the existing postcard result variant indices.
    ResourceAttached {
        task_id: String,
    },
    /// Appended to preserve existing encoded result indices.
    ExecutionBudgetSet {
        task_id: String,
    },
    // New instruction/cancellation results follow the published main indices.
    CancellationRequested {
        task_id: String,
        epoch: u64,
    },
    PendingAmended {
        task_id: String,
        revision: u64,
    },
    PendingCancelled {
        task_id: String,
        revision: u64,
    },
}

/// The pending frontier a person actually observed. This is correlation, not
/// authority; the owner checks it again under the same lock as run admission.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskPendingFrontier {
    pub status: WorkspaceTaskStatus,
    pub run_epoch: u64,
    pub revision: u64,
}

/// Exact replay record for one owner-admitted task mutation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskMutationReceipt {
    pub device: [u8; 32],
    pub invocation: uuid::Uuid,
    pub request_digest: [u8; 32],
    pub result: WorkspaceTaskMutationResult,
}

/// Owner-only task execution state, stored beside `tasks_v1` under the task id.
/// Keeping it as a sidecar leaves the long-lived public [`WorkspaceTask`] shape
/// backward compatible while preserving restart fences and exact replay.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskControl {
    pub run_epoch: u64,
    /// Owner-configured policy, frozen into a broker-minted run at dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_budget: Option<collab_types::task_budget::TaskExecutionBudget>,
    /// Revision of pending instruction edits, separate from active-run
    /// directions. Legacy controls start at zero.
    #[serde(default, skip_serializing_if = "pending_revision_is_zero")]
    pub pending_revision: u64,
    /// Owner admission order of the last enqueue, independent of wall-clock
    /// time and random task identity. Missing marks a legacy queue entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_ordinal: Option<u64>,
    /// Full person-authored instruction, independent of the short display
    /// title and any application-specific source resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// New queue commands require an exact retained instruction. False marks
    /// an explicitly legacy title-only task, whose title remains its prompt.
    #[serde(default)]
    pub instruction_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run: Option<WorkspaceTaskActiveRun>,
    /// Missing means no authenticated phase is known, not "working".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<WorkspaceTaskExecution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation: Option<WorkspaceTaskCancellationRequest>,
    #[serde(default)]
    pub directions: Vec<WorkspaceTaskDirection>,
    #[serde(default)]
    pub receipts: Vec<WorkspaceTaskMutationReceipt>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde skip_serializing_if requires a callback borrowing the field"
)]
fn pending_revision_is_zero(revision: &u64) -> bool {
    *revision == 0
}

/// Append-only, broker-authenticated record of an external side effect.
/// Arguments, output values, produced resource addresses, failure messages,
/// and failure details are deliberately absent from this durable shape.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceEffectReceipt {
    pub invocation: tilo_actions::InvocationId,
    pub actor: String,
    pub context: tilo_actions::ActionContext,
    pub status: tilo_actions::EffectReceiptStatus,
    pub completed_at: i64,
}

/// A durable unit of work in a collaboration room.
///
/// Tasks are deliberately shell-owned rather than chat messages: every app
/// can attach a resource, people can run them without a language model, and
/// the workspace room CRDT carries the same record to every desktop.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTask {
    pub id: String,
    pub title: String,
    pub created_by: String,
    pub assigned_agent: Option<String>,
    pub route: WorkspaceTaskRoute,
    pub status: WorkspaceTaskStatus,
    pub resource: Option<WorkspaceTaskResource>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "crate::history::omitted")]
    pub audit: Vec<WorkspaceTaskAuditEvent>,
}

impl WorkspaceTask {
    /// Legacy entries precede subsequently admitted entries. Within the new
    /// queue, only the owner's ordinal determines order; timestamps and IDs
    /// remain deterministic fallbacks for legacy documents.
    pub fn queue_order_key(
        &self,
        control: Option<&WorkspaceTaskControl>,
    ) -> (Option<u64>, i64, &str) {
        (
            control.and_then(|control| control.queue_ordinal),
            self.created_at,
            self.id.as_str(),
        )
    }

    pub fn action_context(&self) -> tilo_actions::ActionContext {
        let mut context = tilo_actions::ActionContext::default()
            .with_purpose(self.title.clone())
            .with_group(self.id.clone(), self.title.clone(), "task");
        if let Some(resource) = &self.resource {
            context.resource = Some(tilo_actions::ActionResource {
                id: resource.id.clone(),
                kind: resource.kind.clone(),
                label: resource.label.clone(),
            });
            if let (Some(id), Some(label)) = (&resource.app_id, &resource.app_label) {
                context = context.with_app(id.clone(), label.clone());
            }
        }
        context
    }
}
