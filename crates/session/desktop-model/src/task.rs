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
    /// Exact person approval retained across host/agent reconnects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<WorkspaceTaskApproval>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTaskApproval {
    pub person: String,
    pub device: [u8; 32],
    pub approved_at: i64,
    pub instruction_revision: u64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run: Option<WorkspaceTaskActiveRun>,
    /// Missing means no authenticated phase is known, not "working".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<WorkspaceTaskExecution>,
    #[serde(default)]
    pub directions: Vec<WorkspaceTaskDirection>,
    #[serde(default)]
    pub receipts: Vec<WorkspaceTaskMutationReceipt>,
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

impl WorkspaceEffectReceipt {
    /// The bounded public receipt of `invocation`'s terminal `status`: every
    /// text is bounded and stripped of control characters, and the context
    /// keeps only the receipt's own facets. Arbitrary application facets may
    /// carry private argument summaries, so none enter this audit.
    #[must_use]
    pub fn redacted(
        invocation: tilo_actions::InvocationId,
        actor: &str,
        mut context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
        completed_at: i64,
    ) -> Self {
        let original_action = bounded_receipt_text(&context.action_label, 160);
        context.action_label = if original_action.is_empty() {
            status.label().to_string()
        } else {
            format!("{} · {original_action}", status.label())
        };
        context.facets = vec![
            tilo_actions::ActionFacet::new("tilo.receipt", "effect"),
            tilo_actions::ActionFacet::new("tilo.invocation", invocation.0.to_string()),
        ];
        Self {
            invocation,
            actor: bounded_receipt_text(actor, 160),
            context: bounded_receipt_context(context),
            status,
            completed_at,
        }
    }

    /// Whether this receipt is exactly what [`Self::redacted`] produces for
    /// its own invocation, apart from the status label it already carries.
    #[must_use]
    pub fn is_redacted(&self) -> bool {
        let facets = &self.context.facets;
        !self.invocation.0.is_nil()
            && !self.actor.is_empty()
            && self.actor == bounded_receipt_text(&self.actor, 160)
            && self.completed_at >= 0
            && self.context == bounded_receipt_context(self.context.clone())
            && facets.len() == 2
            && facets
                .iter()
                .any(|facet| facet.key == "tilo.receipt" && facet.value == "effect")
            && facets.iter().any(|facet| {
                facet.key == "tilo.invocation" && facet.value == self.invocation.0.to_string()
            })
    }
}

fn bounded_receipt_text(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

fn bounded_receipt_context(
    mut context: tilo_actions::ActionContext,
) -> tilo_actions::ActionContext {
    context.action_label = bounded_receipt_text(&context.action_label, 200);
    if let Some(app) = &mut context.app {
        app.id = bounded_receipt_text(&app.id, 160);
        app.label = bounded_receipt_text(&app.label, 160);
    }
    if let Some(resource) = &mut context.resource {
        resource.id = bounded_receipt_text(&resource.id, 512);
        resource.kind = bounded_receipt_text(&resource.kind, 160);
        resource.label = bounded_receipt_text(&resource.label, 200);
    }
    context.purpose = context
        .purpose
        .as_deref()
        .map(|value| bounded_receipt_text(value, 512))
        .filter(|value| !value.is_empty());
    if let Some(group) = &mut context.group {
        group.id = bounded_receipt_text(&group.id, 160);
        group.label = bounded_receipt_text(&group.label, 200);
        group.kind = bounded_receipt_text(&group.kind, 160);
    }
    for facet in &mut context.facets {
        facet.key = bounded_receipt_text(&facet.key, 96);
        facet.value = bounded_receipt_text(&facet.value, 160);
    }
    context
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
