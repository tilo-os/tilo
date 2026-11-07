//! What Tasks does with the workspace beyond editing the tracker: opening
//! an issue, showing a place, writing a note.
//!
//! The vocabulary travels with the record so a typed reference to one of
//! these effects names the tracker wherever it is read from. The Tasks app
//! answers them; nothing here touches another app.

use tilo_resource::{Effect, Id};

/// The app that keeps the workspace's notes.
pub const NOTES: &str = "so.tilo.notes";

/// The kind of a note, an item of the notebook's pages.
pub const NOTE_KIND: &str = "notes.notebook.pages";

/// What Tasks does with other apps' resources.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "tasks.effect", app = "so.tilo.tasks")]
pub enum TasksEffect {
    /// Start a project
    ///
    /// Open the form a project or an area is made in. It changes nothing on
    /// its own: a script or an agent that means to make one invokes the
    /// tracker's own action with a name. It edits nothing, so a viewer may
    /// open it too.
    #[effect(kind = "read")]
    NewProject,

    /// Open issue
    ///
    /// Show an issue's page, as choosing it in the list does.
    #[effect(view, idempotent, item = issues)]
    #[effect(icon = "circle-dot")]
    OpenIssue {
        #[param("The issue")]
        issue: Id,
    },

    /// Write a note
    ///
    /// A note in Notes for the issue, titled after it and linked from it.
    /// Answers the note's resource.
    #[effect(job, kind = "undoable", item = issues)]
    WriteNote {
        #[param("The issue")]
        issue: Id,
    },

    /// Show the list
    ///
    /// Show every issue, sub-issues under their parent.
    #[effect(view, idempotent)]
    ShowList,

    /// Show the board
    ///
    /// Show a lane per status.
    #[effect(view, idempotent)]
    ShowBoard,

    /// Show the threads
    ///
    /// Show the tasks that have said something, as their conversations.
    #[effect(view, idempotent)]
    ShowThread,
}
