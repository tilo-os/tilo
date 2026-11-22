//! The server an agent a host runs itself works through on one person's turn.

use tilo_actions::remote::{ActionProvider, Caller};

use crate::McpServer;

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// One session of an agent the host admitted itself, for a person: `caller` is that person's
    /// device handing the session to the agent ([`Caller::delegated_to`]), as the host that
    /// admitted it built it. Every call is the agent's, held to the person's access and the
    /// agent's own bound, as an agent's session on the person's own device is. Its own fresh
    /// execution, retired when the server drops.
    ///
    /// # Errors
    /// `caller` is not an agent acting for a person in a workspace.
    pub fn for_delegated(host: H, caller: Caller) -> std::io::Result<Self> {
        if caller.acting().agent_id().is_none() || caller.acting_for_person().is_none() {
            return Err(std::io::Error::other(
                "a delegated server is an agent's acting for a person",
            ));
        }
        let workspace = caller
            .workspace()
            .ok_or_else(|| std::io::Error::other("a delegated agent names its workspace"))?
            .to_owned();
        Ok(Self::from_caller(
            host,
            workspace,
            caller.in_new_execution(),
        ))
    }

    /// One turn of an agent the host runs itself, for the person whose ask it answers: `caller` is
    /// that person's device handing the session to the agent on the turn
    /// ([`Caller::delegated_to`], [`Caller::on_turn`]), as the host that admitted the ask built
    /// it ([`Self::for_delegated`]).
    ///
    /// A turn's call waits for its tool until the tool has ended: nobody asked for a slow one to
    /// be answered `pending`, which costs the turn's model a second call for what one should
    /// have answered. A call that names `wait_ms` asked for it and is answered so.
    ///
    /// # Errors
    /// `caller` is not an agent acting for a person in a workspace.
    pub fn for_turn(host: H, caller: Caller) -> std::io::Result<Self> {
        let mut server = Self::for_delegated(host, caller)?;
        server.until_ended = true;
        Ok(server)
    }
}
