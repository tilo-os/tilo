//! `tilo_status`: how an invocation went, and the two things done to one after
//! it answered: stopping it (`cancel`) and taking it back (`undo`).
//! Named no invocation, it answers the caller's invocations and how each
//! stands, as the workspace's invocations record lists them, so an agent that
//! lost an id, or wonders what is still running, asks the same tool.

use super::{ActionProvider, Failure, McpServer, Reply, answer_of, invocation_id, link, outcome};
use serde::Deserialize;
use std::fmt::Write as _;
use tilo_action_services::workspace_invocations::Invocation;

/// What `tilo_status` takes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentStatusArgs {
    /// The invocation asked about; none asks for the caller's invocations.
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
            return self.own_listed(structured).await;
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

    /// The caller's invocations, newest first, each with the action it ran and how it stands,
    /// as the workspace's invocations record lists them.
    async fn own_listed(&self, structured: bool) -> Result<Reply, Failure> {
        let own = self.own_invocations().await?;
        let state = |item: &Invocation| super::watch::phase(item.status);
        let text = if own.is_empty() {
            "you ran nothing yet; tilo_do runs an action, and tilo_status {invocation_id} says how \
             one went"
                .to_owned()
        } else {
            let mut text = String::from("your invocations, newest first:");
            for item in &own {
                let _ = write!(
                    text,
                    "\n{} {} {}",
                    item.id.as_deref().unwrap_or("-"),
                    item.action.as_deref().unwrap_or("(calls)"),
                    state(item)
                );
            }
            text
        };
        let mut reply = Reply::ok(text);
        reply.structured = structured.then(|| {
            serde_json::json!({"invocations": own
                .iter()
                .map(|item| serde_json::json!({
                    "invocation_id": item.id,
                    "path": item.action,
                    "state": state(item),
                }))
                .collect::<Vec<_>>()})
        });
        Ok(reply)
    }
}
