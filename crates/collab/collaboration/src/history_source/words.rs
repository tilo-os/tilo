//! How a history verb names steps and people in what it says: a refusal
//! names the step it could not take back and who changed it since.
//!
//! The words are the reader's: their locale, the app's catalogue and the
//! labels of the actions it publishes. The toolkit says them on the calling
//! thread (`ui::history_source::ThreadWords`); a process without the toolkit
//! brings its own.

use tilo_actions::ActionContext;

use super::authors::ResolveAuthor;

/// The words a history verb speaks in. See the module docs.
pub trait HistoryWords: ResolveAuthor {
    /// What a step did, as a sentence ("Renamed Draft to Report").
    fn sentence(&self, entry: &ActionContext) -> String;
}
