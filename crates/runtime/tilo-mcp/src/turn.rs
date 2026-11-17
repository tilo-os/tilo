//! The server an agent a host runs itself works through on one person's turn.

use tilo_actions::remote::{ActionHost, Caller};

use crate::{McpServer, Surface};

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// One turn of an agent the host runs itself, for the person whose ask it answers: `caller` is
    /// that person's device handing the session to the agent on the turn
    /// ([`Caller::delegated_to`], [`Caller::on_turn`]), as the host that admitted the ask built
    /// it. Every call is the agent's, held to the person's access and the agent's own bound, as
    /// an agent's session on the person's own device is. Its own fresh execution, retired when the
    /// turn's server drops.
    ///
    /// # Errors
    /// `caller` is not an agent acting for a person in a workspace.
    pub fn for_turn(host: H, caller: Caller) -> std::io::Result<Self> {
        let (Some(_), Some(person)) = (caller.acting().agent_id(), caller.acting_for_person())
        else {
            return Err(std::io::Error::other(
                "a turn's server is an agent's acting for a person",
            ));
        };
        let person = person.to_owned();
        let workspace = caller
            .workspace()
            .ok_or_else(|| std::io::Error::other("a turn names its workspace"))?
            .to_owned();
        Ok(
            Self::from_caller(host, workspace, person, caller.in_new_execution())
                .with_surface(Surface::Agent),
        )
    }
}
