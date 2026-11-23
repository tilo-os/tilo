//! The person's MCP server, as the overview of their Tilo follows it: the
//! server's live read ([`mcp_server::SERVER`]), asked of the host that serves it
//! and followed as agents connect, work and leave. It is the read Console's
//! agent card shows, not a second one: the same server, asked of the same host
//! ([`ActionProvider`]), and each host's answer is what its own run names.
//!
//! A host that refuses the read, or a session that reaches none, leaves the
//! rows as they are: the overview says nothing of agents it cannot hear
//! about, and asks again a while later.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{
    ActionProvider, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget, mcp_server,
};
use tilo_actions::{InvocationId, InvocationUpdate};

/// How long the MCP server is given to answer before its read is asked again: a
/// refused read, or a session that reached no host, is worth another try
/// later, never a frame spent waiting for it.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(15);

/// The MCP server's read, as the overview follows it.
#[derive(Default)]
pub struct ApiRead {
    /// The read being followed, once asked.
    read: Option<InvocationId>,
    /// When it was asked, so one that never answers is asked again.
    asked: Option<ui::time::Instant>,
    /// What the MCP server last answered.
    answer: Option<mcp_server::Server>,
    /// Whether a wake is out for the answer to move.
    armed: Arc<AtomicBool>,
}

impl ApiRead {
    /// Follow the MCP server `host` serves for `caller`, waking `wake` when the
    /// answer may have moved. Whether the MCP server's answer changed, so the rows
    /// are to be drawn again.
    pub fn follow(
        &mut self,
        host: &dyn ActionProvider,
        caller: &Caller,
        wake: &Arc<dyn Fn() + Send + Sync>,
    ) -> bool {
        let now = ui::time::now();
        let overdue = self
            .asked
            .is_some_and(|asked| now.saturating_duration_since(asked) >= ASK_AGAIN);
        let id = match self.read {
            Some(id) if self.answer.is_some() || !overdue => id,
            _ => {
                let id = InvocationId::new();
                self.read = Some(id);
                self.asked = Some(now);
                // The answer is the read's own state, followed below: a host
                // that refuses the ask answers here, with nothing to show.
                drop(host.invoke(
                    caller,
                    request(id, mcp_server::SERVER, InvokeArgument::None),
                ));
                id
            }
        };
        let answer = host
            .observe(caller, &id)
            .and_then(|state| server_of(&state));
        let changed = answer.is_some() && answer != self.answer;
        if changed {
            self.answer = answer;
        }
        // One wake out at a time: a host that holds its answer says when it
        // moved, and one that cannot is asked again on the next frame.
        if !self.armed.load(Ordering::Relaxed)
            && let Some(change) = host.next_change(caller, &id)
        {
            self.armed.store(true, Ordering::Release);
            let armed = Arc::clone(&self.armed);
            let wake = Arc::clone(wake);
            change.then(move |_| {
                armed.store(false, Ordering::Release);
                wake();
            });
        }
        changed
    }

    /// The MCP server as it last answered, for the overview's rows.
    #[must_use]
    pub fn server(&self) -> Option<&mcp_server::Server> {
        self.answer.as_ref()
    }
}

/// The invocation of server `action`, with `argument`.
fn request(invocation: InvocationId, action: &str, argument: InvokeArgument) -> InvokeRequest {
    InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: mcp_server::PROVIDER.to_owned(),
            address: ActionAddress::new(mcp_server::RESOURCE, action),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// What a read's state says of the MCP server.
fn server_of(state: &tilo_actions::remote::InvocationState) -> Option<mcp_server::Server> {
    let InvocationUpdate::Item { value, .. } = &state.update else {
        return None;
    };
    serde_json::from_value(value.clone()).ok()
}

#[cfg(test)]
mod tests;
