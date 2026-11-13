//! `tilo_status`: how an invocation went, and the two things done to one after
//! it answered: stopping it (`cancel`) and taking its run back (`undo`).

use super::{ActionHost, McpServer, Reply, answer_of, invocation_id, link, outcome};
use serde::Deserialize;

/// What `tilo_status` takes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AgentStatusArgs {
    invocation_id: String,
    #[serde(default)]
    cancel: bool,
    /// Take back what the invocation's run changed ([`McpServer::undo_run`]).
    #[serde(default)]
    undo: bool,
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// [`AgentStatusArgs`] as one answer.
    pub(super) async fn agent_status(
        &self,
        args: &AgentStatusArgs,
        structured: bool,
    ) -> Result<Reply, String> {
        let invocation = invocation_id(&args.invocation_id)?;
        if args.undo {
            if args.cancel {
                return Err("cancel stops a run and undo takes one back: ask one of them".into());
            }
            return self.undo_run(&args.invocation_id, structured).await;
        }
        if args.cancel {
            self.host()
                .cancel(&self.caller(), &invocation)
                .await
                .ok_or("the host dropped the request")?
                .map_err(|error| error.to_string())?;
        }
        let value = self.status(invocation).await?;
        let mut reply = outcome(&value);
        let link = self.changed_link_of(&args.invocation_id, &value).await;
        let typed = structured.then(|| answer_of(&value));
        reply.structured = link::linked(&mut reply, typed, link);
        Ok(reply)
    }
}
