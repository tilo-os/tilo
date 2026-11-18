//! `tilo_status`: how an invocation went, and the two things done to one after
//! it answered: stopping it (`cancel`) and taking it back (`undo`).
//! Named no invocation, it answers this session's invocations and how each
//! stands, so an agent that lost an id, or wonders what is still running,
//! asks the same tool.

use super::{ActionProvider, Failure, McpServer, Reply, answer_of, invocation_id, link, outcome};
use serde::Deserialize;
use std::fmt::Write as _;
use tilo_lock::MutexRecover as _;

/// What `tilo_status` takes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentStatusArgs {
    /// The invocation asked about; none asks for this session's invocations.
    #[serde(default)]
    invocation_id: Option<String>,
    #[serde(default)]
    cancel: bool,
    /// Take back what the invocation changed ([`McpServer::undo_invocation`]).
    #[serde(default)]
    undo: bool,
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// [`AgentStatusArgs`] as one answer.
    pub(super) async fn agent_status(
        &self,
        args: &AgentStatusArgs,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let Some(id) = args.invocation_id.as_deref() else {
            if args.cancel || args.undo {
                return Err("cancel and undo act on one invocation: name its invocation_id".into());
            }
            return Ok(self.session_invocations(structured));
        };
        let invocation = invocation_id(id)?;
        if args.undo {
            if args.cancel {
                return Err(
                    "cancel stops an invocation and undo takes one back: ask one of them".into(),
                );
            }
            return self.undo_invocation(id, structured).await;
        }
        if args.cancel {
            self.host()
                .cancel(&self.caller(), &invocation)
                .await
                .ok_or("the host dropped the request")??;
        }
        let value = self.status(invocation).await?;
        let mut reply = outcome(&value);
        let link = self.changed_link_of(id, &value).await;
        let typed = structured.then(|| answer_of(&value));
        reply.structured = link::linked(&mut reply, typed, link);
        Ok(reply)
    }

    /// This session's invocations, oldest first, each with the path it ran
    /// and how it stands as last told.
    fn session_invocations(&self, structured: bool) -> Reply {
        let invocations = self.invocations.lock_recover().clone();
        let text = if invocations.is_empty() {
            "this session ran nothing yet; tilo_do runs an action, and tilo_status \
             {invocation_id} says how one went"
                .to_owned()
        } else {
            let mut text = String::from("this session's invocations, oldest first:");
            for watched in &invocations {
                let path = if watched.path.is_empty() {
                    "(calls)"
                } else {
                    &watched.path
                };
                let _ = write!(text, "\n{} {path} {}", watched.invocation, watched.phase);
            }
            text
        };
        let typed = structured.then(|| {
            serde_json::json!({"invocations": invocations
                .iter()
                .map(|watched| serde_json::json!({
                    "invocation_id": watched.invocation,
                    "path": watched.path,
                    "state": watched.phase,
                }))
                .collect::<Vec<_>>()})
        });
        let mut reply = Reply::ok(text);
        reply.structured = typed;
        reply
    }
}
