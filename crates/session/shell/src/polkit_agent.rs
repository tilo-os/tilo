//! The shell's side of the session's polkit authentication agent.
//!
//! The agent itself — polkit's D-Bus interface, the conversation with
//! `polkit-agent-helper-1` and the retry policy — is [`tilo_polkit_agent`].
//! The shell supplies only where it asks: a sheet the compositor draws
//! ([`crate::shell::portal_dialog`]), never a window an application could
//! imitate or read keystrokes from, in the shell's own words.

use std::sync::Arc;

use tilo_polkit_agent::service::{AgentHandle, SheetAnswer, Sheets};
use tilo_polkit_agent::{PromptAnswer, PromptView, StockNotice};

use crate::shell::portal_dialog::{
    AuthenticationDecision, AuthenticationRequest, PortalDialogQueue,
};

/// Authentication sheets in the compositor's dialog queue.
pub struct DialogSheets {
    queue: PortalDialogQueue,
}

impl DialogSheets {
    pub fn new(queue: PortalDialogQueue) -> Self {
        Self { queue }
    }
}

impl Sheets for DialogSheets {
    fn ask(&self, handle: &str, view: PromptView) -> SheetAnswer {
        let queue = self.queue.clone();
        let handle = handle.to_owned();
        Box::pin(async move {
            let receiver = queue
                .enqueue_authentication(AuthenticationRequest {
                    handle,
                    message: view.message,
                    action_id: view.action_id,
                    identities: view.identities,
                    identity: view.identity,
                    prompt: view.prompt,
                    echo: view.echo,
                    notice: view.notice,
                })
                .ok()?;
            match receiver.await.ok()? {
                AuthenticationDecision::Answered { identity, secret } => {
                    Some(PromptAnswer { identity, secret })
                }
                AuthenticationDecision::Cancelled => None,
            }
        })
    }

    fn cancel(&self, handle: &str) {
        self.queue.cancel(handle);
    }

    fn stock_notice(&self, notice: StockNotice) -> String {
        crate::shell::i18n::t(notice.message_key())
    }
}

/// Serve the agent on the system bus, asking in `queue`'s sheets, and
/// register it with polkit for this login session.
///
/// # Errors
///
/// When the system bus, logind or polkit cannot be reached.
pub async fn serve(queue: PortalDialogQueue) -> zbus::Result<AgentHandle> {
    tilo_polkit_agent::service::serve(Arc::new(DialogSheets::new(queue))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The agent names its own notices by Fluent key; the sheet must show
    /// the shell's words for each, never the key.
    #[test]
    fn every_stock_notice_reads_as_shell_copy() {
        let sheets = DialogSheets::new(PortalDialogQueue::default());
        for notice in StockNotice::ALL {
            let key = notice.message_key();
            let copy = sheets.stock_notice(notice);
            assert_ne!(copy, key, "{key} has no entry in the shell's catalogue");
            assert!(!copy.trim().is_empty(), "{key} resolves to nothing");
        }
    }
}
