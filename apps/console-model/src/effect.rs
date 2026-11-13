//! What Console does that changes no record: which run or agent a window
//! shows, and what a person asks of an agent from its card.
//!
//! A run is the workspaces' history, so a run's page is the window's own
//! view state: opening one here shows it for the person at this window and
//! changes nothing for a collaborator. That is also what a link to a run
//! opens, so the run's published view is opened by its own effect.
//!
//! An agent's card is the same kind of view, of every run of one client
//! ("Claude Code"). What the card does (stop it, undo its run, take over,
//! revoke it) goes through the agent door and the history as the card's
//! buttons do; asked from elsewhere (its face in the workspace's presence),
//! each opens the card too, so nothing done to an agent happens out of
//! sight.

use tilo_resource::Effect;

/// Console's effects. See the [module docs](self).
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "console.effects")]
pub enum ConsoleEffect {
    /// Open run
    ///
    /// Show this run's page in this window: what it was asked, what it did
    /// and what can be done with it. It changes only the view of whoever
    /// opens it, so an agent may open a run for itself and is shown the run
    /// and what it offers (its link) like a person is.
    #[effect(view, participant)]
    ViewRun {
        #[param("Id of the run")]
        run: String,
    },
    /// Open agent card
    ///
    /// Show this agent's card in this window: what it is doing now, its
    /// runs, what it may reach, and what can be done with it.
    #[effect(view, participant)]
    ViewAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    /// Show how an agent connects in this window: the commands that add Tilo to a client, and
    /// every agent already let in. It changes only the view of whoever opens it.
    #[effect(view, participant)]
    ViewConnect,
    /// Stop agent
    ///
    /// End this agent's working run now, keeping what it may reach so it
    /// can come back, and show its card.
    StopAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    /// Undo agent's run
    ///
    /// Take back everything this agent's newest run changed, and show its
    /// card.
    UndoAgentRun {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    /// Take over from agent
    ///
    /// Stop this agent and open where it was working, and show its card.
    TakeOverAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
    /// Revoke agent
    ///
    /// Take back what this agent may reach: its runs end now and it is let
    /// in no more. Shows its card.
    RevokeAgent {
        #[param("The agent's client, by the name it goes by")]
        agent: String,
    },
}
