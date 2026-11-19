//! What Console does that changes no record: which run or agent a window shows, and what a
//! person asks of an agent from its card.

use tilo_resource::Effect;

/// Console's effects.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "console.effects")]
pub enum ConsoleEffect {
    #[effect(view, presence)]
    ViewSession {
        #[param("Id of the session")]
        session: String,
    },
    #[effect(view, presence)]
    ViewAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    #[effect(view, presence)]
    ViewConnect,
    #[effect(kind = "outside")]
    StopAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    #[effect(kind = "outside")]
    UndoAgentSession {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    #[effect(kind = "outside")]
    TakeOverAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    #[effect(kind = "outside")]
    RevokeAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
}
