//! The undo step a door call joins: one per run a client names, one per task
//! step a program names, one of its own for any other call.
use super::{ActionHost, MAX_RUN_ID, McpServer, Value};

/// The undo step a tool call joins, in its `_meta`: `{"so.tilo.mcp":
/// {"step": "…"}}`. Every call of this session naming the same step is
/// taken back as one step (one task step: all the calls one program makes);
/// a call naming none is a step of its own.
pub(super) const SURFACE_STEP: &str = "/so.tilo.mcp/step";
/// The run a tool call is a step of, in its `_meta`: `{"so.tilo.mcp":
/// {"run": {"id": "…", "title": "…"}}}`. Every call of this session naming
/// the same run id is taken back as one step, and its changes are attributed
/// to the run by `title`, which is what people read where they see history.
pub(super) const SURFACE_RUN_ID: &str = "/so.tilo.mcp/run/id";
/// The same run, named by its id alone: `{"so.tilo.mcp": {"run": "…"}}`.
pub(super) const SURFACE_RUN: &str = "/so.tilo.mcp/run";
/// The name the run's changes go by, in its `_meta` ([`SURFACE_RUN_ID`]).
pub(super) const SURFACE_RUN_TITLE: &str = "/so.tilo.mcp/run/title";
/// A tool call's ask for typed answers, in its `_meta`: `{"so.tilo.mcp":
/// {"structured": true}}`. A program reads with it; a model reads text.
pub(super) const SURFACE_STRUCTURED: &str = "/so.tilo.mcp/structured";

/// The longest run title a client may name, as a label other tools show.
const MAX_RUN_TITLE: usize = 96;

tokio::task_local! {
    /// The undo step the invocations of the tool call being answered join:
    /// every change they make records it as its gesture, so history takes
    /// them back as one step.
    pub(super) static STEP: tilo_actions::ActionId;
    /// The run that step belongs to, when the call named one: its id, which
    /// history keeps with each change, and the name history and presence
    /// show instead of the connection's own label.
    pub(super) static STEP_RUN: Option<DeclaredRun>;
}

/// The run a tool call says it is a step of (`_meta["so.tilo.mcp"].run`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeclaredRun {
    /// The id the client gave the run: what tells its runs apart in history.
    pub(super) id: String,
    /// What the client called it; empty when it called it nothing.
    pub(super) title: String,
}

impl DeclaredRun {
    /// The declared run of the tool call being answered.
    pub(super) fn current() -> Option<Self> {
        STEP_RUN.try_with(Clone::clone).ok().flatten()
    }

    /// `run`, this connection's own, as the client declared this step of it:
    /// under its title, and naming its id.
    pub(super) fn onto(&self, run: &mut tilo_actions::WorkGroup) {
        if !self.title.is_empty() {
            run.label.clone_from(&self.title);
            run.title = Some(self.title.clone());
        }
        run.declared_id = Some(self.id.clone());
    }
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// The undo step a tool call's invocations join: the run its
    /// `_meta["so.tilo.mcp"].run.id` names (or the task step its `step`
    /// names), which every call naming the same one in this session joins,
    /// or one of the call's own: the same again for a call retried under its
    /// `invocation`, which must be the same request. Derived from this
    /// session's execution and run, never taken from the client, so a step
    /// it names can only ever group its own changes.
    pub(super) fn step_gesture(
        &self,
        meta: Option<&Value>,
        invocation: Option<&str>,
    ) -> Result<tilo_actions::ActionId, String> {
        let named = meta.and_then(|meta| {
            [SURFACE_RUN_ID, SURFACE_RUN, SURFACE_STEP]
                .into_iter()
                .find_map(|key| {
                    let named = meta.pointer(key)?;
                    (key != SURFACE_RUN || named.is_string()).then_some((key, named))
                })
        });
        let (domain, step) = match named {
            Some((key, step)) => (
                "tilo door step v1",
                step.as_str()
                    .filter(|step| !step.is_empty() && step.len() <= MAX_RUN_ID)
                    .ok_or_else(|| {
                        let named = match key {
                            SURFACE_RUN_ID | SURFACE_RUN => "run id",
                            _ => "step",
                        };
                        format!("_meta so.tilo.mcp {named} must be a string of 1 to 96 bytes")
                    })?,
            ),
            None => match invocation {
                Some(invocation) => ("tilo door call v1", invocation),
                None => return Ok(tilo_actions::ActionId::new()),
            },
        };
        let mut hasher = blake3::Hasher::new_derive_key(domain);
        if let Some(execution) = self.caller.execution_id() {
            hasher.update(execution.as_bytes());
        }
        let run = self.agent_run().map(|run| run.id).unwrap_or_default();
        hasher.update(&u64::try_from(run.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(run.as_bytes());
        hasher.update(step.as_bytes());
        let bytes = hasher.finalize();
        let id = *bytes.as_bytes().first_chunk::<16>().unwrap_or(&[0; 16]);
        Ok(tilo_actions::ActionId(
            uuid::Builder::from_custom_bytes(id).into_uuid(),
        ))
    }

    /// The undo step the invocations of the call being answered join.
    pub(super) fn call_step() -> Option<tilo_actions::ActionId> {
        STEP.try_with(|step| *step).ok()
    }

    /// The run the call being answered is a step of: its undo step and the
    /// name it goes by, when the call names one (`_meta["so.tilo.mcp"]`
    /// `run`).
    pub(super) fn grouped_run() -> Option<(tilo_actions::ActionId, String)> {
        let label = DeclaredRun::current()?.title;
        let step = STEP.try_with(|step| *step).ok()?;
        Some((step, label))
    }

    /// The run a tool call joins, when its `_meta["so.tilo.mcp"].run` names
    /// one: its id, and the name the changes of the call are attributed to,
    /// as history shows them (empty when it names none). `None` for a call
    /// that names no run.
    pub(super) fn step_run(meta: Option<&Value>) -> Result<Option<DeclaredRun>, String> {
        let Some(id) = meta.and_then(|meta| {
            [SURFACE_RUN_ID, SURFACE_RUN]
                .iter()
                .find_map(|key| meta.pointer(key).and_then(Value::as_str))
        }) else {
            return Ok(None);
        };
        if id.is_empty() || id.len() > MAX_RUN_ID {
            return Err("_meta so.tilo.mcp run id must be a string of 1 to 96 bytes".into());
        }
        let id = id.to_owned();
        let Some(title) = meta.and_then(|meta| meta.pointer(SURFACE_RUN_TITLE)) else {
            return Ok(Some(DeclaredRun {
                id,
                title: String::new(),
            }));
        };
        let title: String = title
            .as_str()
            .filter(|title| !title.trim().is_empty())
            .ok_or("_meta so.tilo.mcp run title must be a non-empty string")?
            .chars()
            .filter(|character| !character.is_control())
            .take(MAX_RUN_TITLE)
            .collect();
        Ok(Some(DeclaredRun {
            id,
            title: title.trim().to_owned(),
        }))
    }
}
