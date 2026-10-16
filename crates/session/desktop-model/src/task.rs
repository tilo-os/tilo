//! Room-owned tasks and effect receipts.

use serde::{Deserialize, Serialize};

/// Largest full instruction retained for one queued agent task.
pub const MAX_WORKSPACE_TASK_INSTRUCTION_BYTES: usize = 32 * 1024;
/// Largest direction a person may append to an active task.
pub const MAX_WORKSPACE_TASK_DIRECTION_BYTES: usize = 8 * 1024;
/// Maximum public progress notes retained per run.
pub const MAX_WORKSPACE_TASK_PUBLIC_UPDATES: usize = 128;
/// Largest public status/action update.
pub const MAX_WORKSPACE_TASK_PROGRESS_BYTES: usize = 8 * 1024;
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
    /// The status as task history records it.
    pub const fn key(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InProgress => "in-progress",
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTaskResource {
    pub app_id: Option<String>,
    pub app_label: Option<String>,
    pub id: String,
    pub kind: String,
    pub label: String,
    /// Canonical direct-Chat request behind an explicitly queued agent task.
    /// The full instruction lives here rather than in the short Task Center
    /// display title, so an offline agent receives it byte-for-byte later.
    #[serde(default)]
    pub agent_request: Option<WorkspaceAgentRequest>,
}

// An absent Chat request is left out of JSON, so the room document reads as
// it always has, but every part is written in a binary format: postcard
// reads fields by position and cannot skip one.
impl Serialize for WorkspaceTaskResource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename = "WorkspaceTaskResource")]
        struct Readable<'a> {
            app_id: Option<&'a str>,
            app_label: Option<&'a str>,
            id: &'a str,
            kind: &'a str,
            label: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            agent_request: Option<&'a WorkspaceAgentRequest>,
        }
        #[derive(Serialize)]
        #[serde(rename = "WorkspaceTaskResource")]
        struct Positional<'a> {
            app_id: Option<&'a str>,
            app_label: Option<&'a str>,
            id: &'a str,
            kind: &'a str,
            label: &'a str,
            agent_request: Option<&'a WorkspaceAgentRequest>,
        }
        if serializer.is_human_readable() {
            Readable {
                app_id: self.app_id.as_deref(),
                app_label: self.app_label.as_deref(),
                id: &self.id,
                kind: &self.kind,
                label: &self.label,
                agent_request: self.agent_request.as_ref(),
            }
            .serialize(serializer)
        } else {
            Positional {
                app_id: self.app_id.as_deref(),
                app_label: self.app_label.as_deref(),
                id: &self.id,
                kind: &self.kind,
                label: &self.label,
                agent_request: self.agent_request.as_ref(),
            }
            .serialize(serializer)
        }
    }
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

/// One event of a task's audit trail, as data: what was done to the task
/// (a verb of [`TASK_KIND`] with its plain arguments), and what its actor
/// wrote, when it wrote anything. Readers render the sentence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTaskAuditEvent {
    pub at: i64,
    pub actor: String,
    pub what: tilo_actions::Did,
    /// The actor's own words: an agent's outcome or progress, a reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The resource kind task audit verbs are published on.
pub const TASK_KIND: &str = "workspace-task";

impl WorkspaceTaskAuditEvent {
    /// `actor` did `action` (a task verb such as `create-task`) with the
    /// plain `args`, at `at` (Unix seconds).
    pub fn new(
        at: i64,
        actor: impl Into<String>,
        action: &str,
        args: impl IntoIterator<Item = (&'static str, tilo_actions::Scalar)>,
    ) -> Self {
        Self {
            at,
            actor: actor.into(),
            what: tilo_actions::Did::verb(
                tilo_actions::ActionKey::new(
                    crate::workspace::WORKSPACE_GOVERNANCE_APP_ID,
                    TASK_KIND,
                    action,
                ),
                args.into_iter()
                    .map(|(name, value)| (name.to_owned(), value)),
            ),
            note: None,
        }
    }

    /// With the actor's own words.
    #[must_use]
    pub fn noting(mut self, note: impl Into<String>) -> Self {
        let note = note.into();
        self.note = (!note.trim().is_empty()).then_some(note);
        self
    }

    /// Whether this note belongs to the exact run, independent of timestamps.
    pub fn for_run(&self, epoch: u64) -> bool {
        matches!(&self.what, tilo_actions::Did::Verb(verb)
            if verb.args.get("run_epoch") == Some(&tilo_actions::Scalar::text(&epoch.to_string())))
    }

    /// The verb the event records.
    pub fn action(&self) -> &str {
        match &self.what {
            tilo_actions::Did::Verb(verb) => &verb.action.action,
            _ => "",
        }
    }

    /// The event as a history entry a reader renders.
    pub fn entry(&self) -> tilo_actions::ActionContext {
        tilo_actions::ActionContext::for_app(
            crate::workspace::WORKSPACE_GOVERNANCE_APP_ID,
            "Workspace",
        )
        .with_described(tilo_actions::Described::new(self.what.clone()))
        .with_invoker(tilo_actions::ActionInvoker::actor(self.actor.clone()))
    }
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

/// Frozen starter authority, further capped to the task's attached resource or room.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTaskExecutionAuthority {
    pub epoch: u64,
    pub scope: tilo_action_policy::TreePath,
    pub starter: tilo_action_policy::Standing,
}

impl WorkspaceTaskExecutionAuthority {
    pub fn allows(
        &self,
        node: &tilo_action_policy::TreePath,
        kind: tilo_action_policy::ActionKind,
        now: web_time::SystemTime,
    ) -> bool {
        self.scope.covers(node)
            && self
                .starter
                .answer(node, kind, &tilo_action_policy::Context::at_time(now))
                == tilo_action_policy::Answer::Allow
    }
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
    /// Owner-minted ceiling for this run; never accepted from task arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_authority: Option<WorkspaceTaskExecutionAuthority>,
    /// Private instruction retained by the task owner. Public task projections
    /// expose the bounded title, never this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
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
    /// Private bounded model-turn snapshots for this run; never public audit.
    #[serde(default)]
    pub presentations:
        std::collections::BTreeMap<String, collab_types::task_presentation::TaskPresentation>,
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
    /// text is bounded and stripped of control characters, what was done
    /// keeps none of its arguments, and the context keeps only the receipt's
    /// own facets. Arbitrary application facets and arguments may carry
    /// private values, so none enter this audit.
    #[must_use]
    pub fn redacted(
        invocation: tilo_actions::InvocationId,
        actor: &str,
        mut context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
        completed_at: i64,
    ) -> Self {
        if let Some(what) = &mut context.what {
            what.forget_args();
        }
        Self::recorded(invocation, actor, context, status, completed_at)
    }

    /// [`Self::redacted`], keeping what was done with its arguments: for a
    /// receipt whose arguments are the record itself, such as the grant a
    /// person gave.
    #[must_use]
    pub fn recorded(
        invocation: tilo_actions::InvocationId,
        actor: &str,
        mut context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
        completed_at: i64,
    ) -> Self {
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

    /// Whether this receipt is exactly what [`Self::recorded`] produces for
    /// its own invocation.
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
    if let Some(item) = &mut context.item {
        item.id = bounded_receipt_text(&item.id, 512);
        item.label = bounded_receipt_text(&item.label, 200);
    }
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
    /// Default responsible person; distinct from the agent executing the work.
    pub created_by: String,
    pub assigned_agent: Option<String>,
    pub route: WorkspaceTaskRoute,
    pub status: WorkspaceTaskStatus,
    /// The scope-narrowing attachment: a task attached to a resource runs
    /// only within it.
    pub resource: Option<WorkspaceTaskResource>,
    /// What the request is about, when the person asked about a selection:
    /// the resource and exactly what was selected in it. Context only, kept
    /// apart from [`Self::resource`] so it never narrows (or widens) what
    /// the run may do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<tilo_actions::SelectionSubject>,
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

#[cfg(test)]
mod execution_authority_tests {
    use super::WorkspaceTaskExecutionAuthority;
    use tilo_action_policy::{ActionKind, RoomPolicy, Standing, TreePath};

    #[test]
    fn attached_scope_caps_even_a_workspace_owner() {
        let room = TreePath::room("room");
        let attached = room.clone().app("notes").kind("document").resource("one");
        let bound = WorkspaceTaskExecutionAuthority {
            epoch: 1,
            scope: attached.clone(),
            starter: Standing::person(RoomPolicy::new("room", "owner"), "owner"),
        };
        let now = web_time::SystemTime::now();
        assert!(bound.allows(&attached.action("append"), ActionKind::Undoable, now));
        assert!(
            !bound.allows(
                &room
                    .app("notes")
                    .kind("document")
                    .resource("two")
                    .action("append"),
                ActionKind::Undoable,
                now
            )
        );
        assert!(
            !bound.allows(
                &TreePath::room("other")
                    .app("notes")
                    .kind("document")
                    .resource("one"),
                ActionKind::Read,
                now
            )
        );
    }
}

#[cfg(test)]
mod effect_receipt_redaction_tests {
    #[test]
    fn only_broker_owned_terminal_facets_and_no_arguments_enter_public_receipts() {
        let invocation = tilo_actions::InvocationId::new();
        let context = tilo_actions::ActionContext {
            facets: vec![
                tilo_actions::ActionFacet::new("private.argument", "secret text"),
                tilo_actions::ActionFacet::new("tilo.invocation", "forged"),
                tilo_actions::ActionFacet::new("tilo.receipt", "forged"),
            ],
            what: Some(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new("so.tilo.notes", "note", "append"),
                [(
                    "text".to_owned(),
                    tilo_actions::Scalar::text("secret words"),
                )],
            )),
            ..Default::default()
        };
        let receipt = super::WorkspaceEffectReceipt::redacted(
            invocation,
            "person",
            context,
            tilo_actions::EffectReceiptStatus::Succeeded,
            0,
        );
        assert_eq!(
            receipt.context.facets,
            vec![
                tilo_actions::ActionFacet::new("tilo.receipt", "effect"),
                tilo_actions::ActionFacet::new("tilo.invocation", invocation.0.to_string()),
            ]
        );
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(!json.contains("secret text"));
        assert!(!json.contains("secret words"));
        assert!(receipt.is_redacted());
    }
}

#[cfg(test)]
mod selection_subject_tests {
    use super::{WorkspaceTask, WorkspaceTaskResource};
    use tilo_actions::{ActionResource, SelectionAnchor, SelectionPlace, SelectionSubject};

    fn resource() -> WorkspaceTaskResource {
        WorkspaceTaskResource {
            app_id: Some("so.tilo.word-processor".into()),
            app_label: Some("Writer".into()),
            id: "doc:1".into(),
            kind: "word-processor.document".into(),
            label: "Proposal".into(),
            agent_request: None,
        }
    }

    #[test]
    fn an_attachment_reads_and_writes_as_it_always_has() {
        let older = serde_json::json!({
            "app_id": "so.tilo.word-processor", "app_label": "Writer",
            "id": "doc:1", "kind": "word-processor.document", "label": "Proposal"
        });
        let read: WorkspaceTaskResource = serde_json::from_value(older.clone()).unwrap();
        assert_eq!(read, resource());
        assert_eq!(serde_json::to_value(&read).unwrap(), older);
        let encoded = postcard::to_allocvec(&read).unwrap();
        assert_eq!(
            postcard::from_bytes::<WorkspaceTaskResource>(&encoded).unwrap(),
            read,
            "a binary format writes the absent Chat request too"
        );
    }

    #[test]
    fn a_task_about_a_selection_keeps_it_apart_from_any_attachment() {
        let subject = SelectionSubject {
            app_id: "so.tilo.word-processor".into(),
            app_label: "Writer".into(),
            resource: ActionResource {
                id: "doc:1".into(),
                kind: "word-processor.document".into(),
                label: "Proposal".into(),
            },
            anchor: SelectionAnchor::of_text("the second paragraph")
                .at(SelectionPlace::range("block-7", 0, 20)),
        };
        let mut task: WorkspaceTask = serde_json::from_value(serde_json::json!({
            "id": "task:1", "title": "Tighten this", "created_by": "ada",
            "assigned_agent": "scribe", "route": "model", "status": "queued",
            "resource": null, "created_at": 1, "updated_at": 1
        }))
        .unwrap();
        assert!(task.subject.is_none(), "a task from before subjects reads");
        assert!(
            serde_json::to_value(&task)
                .unwrap()
                .get("subject")
                .is_none(),
            "and writes what it wrote"
        );
        task.subject = Some(subject.clone());
        let read: WorkspaceTask =
            serde_json::from_value(serde_json::to_value(&task).unwrap()).unwrap();
        assert_eq!(read.subject, Some(subject));
        assert!(read.resource.is_none(), "a selection is no attachment");
        let context = read.action_context();
        assert!(
            context.resource.is_none() && context.item.is_none(),
            "nor every change's resource or item"
        );
    }
}
