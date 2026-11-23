//! What an agent's run is doing now, as the steps every run takes.
//!
//! A run reports its progress as a note on the task, and says each step once
//! rather than on every poll. The steps are this vocabulary — looking
//! something up on the web, reading a document, changing one — and the words
//! a person reads are the agent's own catalogue's, in their language
//! ("Editing Notes…", "Ändert Notizen…"), so nothing here is text on screen.

/// One step of a run, as the person is told about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskStep {
    /// Searching the web, for what.
    Searching(String),
    /// Reading what an application holds, or a web page at that address: the
    /// application's name or the page's address.
    Reading(String),
    /// Changing what an application holds; the application's name.
    Editing(String),
    /// Finding out what an application can do; the application's name.
    Exploring(String),
}
