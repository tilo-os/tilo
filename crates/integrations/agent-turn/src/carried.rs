//! The session's actions, as a process that reaches them over a link of its own sees them.
//!
//! An agent answering a turn in its own process (the Agent app on a desktop) asks the session's
//! action API over its connection to the host: each request is one remote-action message, and the
//! host answers on the same connection, deciding every request as the turn's. [`carried`] is that
//! connection as an [`ActionProvider`](tilo_actions::remote::ActionProvider)
//! ([`SessionActions`]), so an [`McpServer`](crate::McpServer) serves the agent's tools over it:
//! what the provider asks waits in the [`Carried`] outbox until the process sends it on its link,
//! and what the host answers is handed back with [`Carried::hear`].

use std::sync::{Arc, Mutex};

use collab_types::WorkspaceId;
use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
use tilo_lock::MutexRecover as _;
use tilo_session_actions::{Heard, RemoteSession, SessionActions};

/// The device the carried messages are said to go to: the link reaches one host only.
const HOST: [u8; 32] = [0; 32];

/// What waits to go out on a process's link, and what came back on it.
pub struct Carried {
    workspace: WorkspaceId,
    outbox: Mutex<Vec<RemoteActionMessage>>,
    heard: Mutex<Vec<RemoteActionMessage>>,
    /// Wakes the provider when the host answered.
    woken: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Tells the process its outbox holds something.
    ring: Box<dyn Fn() + Send + Sync>,
}

impl Carried {
    /// Everything the provider asked since the last call, to send on the link in order.
    pub fn take_outbox(&self) -> Vec<RemoteActionMessage> {
        std::mem::take(&mut *self.outbox.lock_recover())
    }

    /// The host answered `message` on the link.
    pub fn hear(&self, message: RemoteActionMessage) {
        self.heard.lock_recover().push(message);
        let woken = self.woken.lock_recover().clone();
        if let Some(wake) = woken {
            wake();
        }
    }
}

/// The session as [`SessionActions`] reaches it.
struct Link(Arc<Carried>);

impl RemoteSession for Link {
    fn send_remote_action(
        &self,
        _: WorkspaceId,
        _: [u8; 32],
        frame: Vec<u8>,
    ) -> Result<(), String> {
        let frame = RemoteActionFrame::decode(&frame).map_err(|error| error.to_string())?;
        self.0.outbox.lock_recover().push(frame.message);
        (self.0.ring)();
        Ok(())
    }

    fn take_heard(&self) -> Vec<Heard> {
        let heard = std::mem::take(&mut *self.0.heard.lock_recover());
        heard
            .into_iter()
            .filter_map(|message| {
                Some(Heard::Action {
                    workspace_id: self.0.workspace,
                    user: HOST,
                    from: HOST,
                    frame: RemoteActionFrame::new(message).encode().ok()?,
                })
            })
            .collect()
    }

    fn on_change(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.0.woken.lock_recover() = Some(wake);
    }
}

/// The actions of `workspace` asked over a process's own link: the provider, and the messages it
/// leaves for the link to carry. `ring` is called whenever a message waits to go out.
#[must_use]
pub fn carried(
    workspace: WorkspaceId,
    ring: impl Fn() + Send + Sync + 'static,
) -> (SessionActions, Arc<Carried>) {
    let carried = Arc::new(Carried {
        workspace,
        outbox: Mutex::default(),
        heard: Mutex::default(),
        woken: Mutex::default(),
        ring: Box::new(ring),
    });
    let actions = SessionActions::new(
        Arc::new(Link(Arc::clone(&carried))) as Arc<dyn RemoteSession>,
        workspace,
        HOST,
    );
    (actions, carried)
}
