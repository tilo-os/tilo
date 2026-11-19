//! What Console does that changes no record: which run or agent a window shows, and what a
//! person asks of an agent from its card.

use tilo_resource::Effect;

use crate::{Agent, Session};

/// Console's effects.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "console.effects")]
pub enum ConsoleEffect {
    #[effect(view, presence, on = Session)]
    ViewSession {
        #[param("Id of the session")]
        session: String,
    },
    #[effect(view, presence, on = Agent)]
    ViewAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    #[effect(view, presence)]
    ViewConnect,
    #[effect(kind = "outside", on = Agent)]
    StopAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    #[effect(kind = "outside", on = Agent)]
    UndoAgentSession {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    #[effect(kind = "outside", on = Agent)]
    TakeOverAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    #[effect(kind = "outside", on = Agent)]
    RevokeAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
}
