//! What Console does that changes no record: which run or agent a window shows, and what a
//! person asks of an agent from its card.

use tilo_resource::Effect;

/// Console's effects.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "console.effects")]
pub enum ConsoleEffect {
    #[effect(view, participant)]
    ViewRun {
        #[param("Id of the run")]
        run: String,
    },
    #[effect(view, participant)]
    ViewAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    #[effect(view, participant)]
    ViewConnect,
    StopAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    UndoAgentRun {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    TakeOverAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    RevokeAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
}
