//! What Console does that changes no record: which run a window shows.
//!
//! A run is the workspaces' history, so a run's page is the window's own
//! view state: opening one here shows it for the person at this window and
//! changes nothing for a collaborator. That is also what a link to a run
//! opens, so the run's published view is opened by its own effect.

// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::Effect;

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
}
