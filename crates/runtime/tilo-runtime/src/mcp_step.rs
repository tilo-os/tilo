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
    /// The name the run that step belongs to goes by, when the call named
    /// one: what history and presence show instead of the connection's own
    /// label.
    pub(super) static STEP_TITLE: Option<String>;
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

    /// The run the call being answered is a step of: its undo step and the
    /// name it goes by, when the call names one (`_meta["so.tilo.mcp"]`
    /// `run`).
    pub(super) fn grouped_run() -> Option<(tilo_actions::ActionId, String)> {
        let label = STEP_TITLE.try_with(Clone::clone).ok().flatten()?;
        let step = STEP.try_with(|step| *step).ok()?;
        Some((step, label))
    }

    /// The run the tool call `request` was a step of, by the name history shows its changes
    /// under: the title its `_meta` run names, else this session's own run. What a window the
    /// call opened says it was opened in.
    #[must_use]
    pub fn run_of(&self, request: &Value) -> Option<tilo_actions::WorkGroup> {
        let mut run = self.agent_run()?;
        if let Ok(Some(title)) = Self::step_title(request.pointer("/params/_meta"))
            && !title.is_empty()
        {
            run.label = title;
        }
        Some(run)
    }

    /// The name the run a tool call joins goes by, when its
    /// `_meta["so.tilo.mcp"].run` names one: what the changes of the call are
    /// attributed to, as history shows them. `Some("")` for a call that
    /// names a run and no title for it; `None` for a call that names no run.
    pub(super) fn step_title(meta: Option<&Value>) -> Result<Option<String>, String> {
        let names_run = meta.is_some_and(|meta| {
            ["/so.tilo.mcp/run/id", "/so.tilo.mcp/run"]
                .iter()
                .any(|key| meta.pointer(key).is_some_and(Value::is_string))
        });
        if !names_run {
            return Ok(None);
        }
        let Some(title) = meta.and_then(|meta| meta.pointer(SURFACE_RUN_TITLE)) else {
            return Ok(Some(String::new()));
        };
        let title: String = title
            .as_str()
            .filter(|title| !title.trim().is_empty())
            .ok_or("_meta so.tilo.mcp run title must be a non-empty string")?
            .chars()
            .filter(|character| !character.is_control())
            .take(MAX_RUN_TITLE)
            .collect();
        Ok(Some(title.trim().to_owned()))
    }
}
