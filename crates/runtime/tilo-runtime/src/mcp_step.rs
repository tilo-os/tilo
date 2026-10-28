//! The undo step a door call joins: one per task step a program names, one
//! of its own for any other call.
use super::{ActionHost, MAX_RUN_ID, McpServer, Value};

/// The undo step a tool call joins, in its `_meta`: `{"so.tilo.mcp":
/// {"step": "…"}}`. Every call of this session naming the same step is
/// taken back as one step (one task step: all the calls one program makes);
/// a call naming none is a step of its own.
pub(super) const SURFACE_STEP: &str = "/so.tilo.mcp/step";
/// A tool call's ask for typed answers, in its `_meta`: `{"so.tilo.mcp":
/// {"structured": true}}`. A program reads with it; a model reads text.
pub(super) const SURFACE_STRUCTURED: &str = "/so.tilo.mcp/structured";

tokio::task_local! {
    /// The undo step the invocations of the tool call being answered join:
    /// every change they make records it as its gesture, so history takes
    /// them back as one step.
    pub(super) static STEP: tilo_actions::ActionId;
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// The undo step a tool call's invocations join: the one its
    /// `_meta["so.tilo.mcp"].step` names, which every call naming the same
    /// step in this session joins (one task step: one program's calls), or
    /// one of the call's own: the same again for a call retried under its
    /// `invocation`, which must be the same request. Derived from this
    /// session's execution and run, never taken from the client, so a step
    /// it names can only ever group its own changes.
    pub(super) fn step_gesture(
        &self,
        meta: Option<&Value>,
        invocation: Option<&str>,
    ) -> Result<tilo_actions::ActionId, String> {
        let (domain, step) = match meta.and_then(|meta| meta.pointer(SURFACE_STEP)) {
            Some(step) => (
                "tilo door step v1",
                step.as_str()
                    .filter(|step| !step.is_empty() && step.len() <= MAX_RUN_ID)
                    .ok_or("_meta so.tilo.mcp step must be a string of 1 to 96 bytes")?,
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
}
