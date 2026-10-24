//! Whose documents a step may land in when it names none.
//!
//! A written document (a letter, an evidence pack) is its writer's: several
//! agents of a room given tasks in the same Word Processor each write their
//! own. A step naming no record, an open the host makes for a task and the
//! documents a task is offered never put an agent into a document another
//! member is writing unless the task names that document (its title or its
//! id). Records that are filled in together (a workbook, a form, a channel)
//! are not written documents, and a step naming none still finds the room's
//! one record of their app.

use tilo_actions::door::ActionDescriptor;
use tilo_browser_peer::DocumentRoute;

use super::AgentParticipant;
use super::unopened::Reading;

/// The kind of the rich text a record publishes as its body: an app whose
/// new record publishes it at the root (`word-processor.document/body`)
/// keeps written documents. Rich text further in (a notebook's pages) is
/// not a document of its own.
const DOCUMENT_TEXT: &str = "tilo.rich-text";

/// The fewest characters a title must have to count as named by a task: a
/// one- or two-letter heading is in any text.
const SHORTEST_NAMED_TITLE: usize = 3;

impl AgentParticipant {
    /// Whether the records of `app` are written documents
    /// ([`DOCUMENT_TEXT`] at their root), read from what a new record of it
    /// publishes.
    pub(super) fn writes_documents(&mut self, app: &str) -> bool {
        self.preview(app).is_some_and(|descriptors| {
            descriptors.iter().any(|descriptor| {
                descriptor.resource_kind == DOCUMENT_TEXT
                    && descriptor.address.resource.split('/').count() == 2
            })
        })
    }

    /// Whether the record at `route` is a written document another member
    /// of the room wrote in (or opened new for a task of theirs), which this
    /// agent never changed and `task` does not name: a step naming no record
    /// does not land in it, and it is not opened for the task.
    pub(super) fn others_document(&mut self, route: &DocumentRoute, task: &str) -> bool {
        self.others_wrote.contains(route)
            && !self.own_wrote.contains(route)
            && self.writes_documents(&route.app_id)
            && !self.names_record(route, task)
    }

    /// Whether `task` names the record at `route`: by its id, or by its
    /// title (a document's first line).
    fn names_record(&mut self, route: &DocumentRoute, task: &str) -> bool {
        let lower = task.to_lowercase();
        let id = route.record.trim_start_matches("uuid:").to_lowercase();
        if !id.is_empty() && lower.contains(&id) {
            return true;
        }
        self.document_title(route)
            .map(|title| title.trim().to_lowercase())
            .filter(|title| title.chars().count() >= SHORTEST_NAMED_TITLE)
            .is_some_and(|title| lower.contains(&title))
    }

    /// What the record at `route` is called: its app's name for it when it
    /// is not open, else its first line of text (a document's heading).
    pub(super) fn document_title(&mut self, route: &DocumentRoute) -> Option<String> {
        match self.apps.get(route) {
            Some(replica) => self
                .unopened
                .open_text(route, replica, Reading::Plain)
                .and_then(|text| {
                    text.lines()
                        .map(|line| line.trim_start_matches('#').trim())
                        .find(|line| !line.is_empty())
                        .map(str::to_owned)
                }),
            None => self.record_title(route),
        }
    }

    /// Set aside, of `open` (each with what it publishes) and `known`
    /// (each with its title), the written documents of other members that
    /// `task` does not name and its steps (`touched`) never acted on: they
    /// are offered to open by their record like any the room has, not with
    /// their operations, as a write or an import planned there would land
    /// in their document. Returns them.
    pub(super) fn set_aside_others_documents(
        &mut self,
        task: &str,
        touched: &[DocumentRoute],
        open: &mut Vec<(DocumentRoute, Vec<ActionDescriptor>)>,
        known: &mut Vec<(DocumentRoute, Option<String>)>,
    ) -> Vec<DocumentRoute> {
        let candidates: Vec<DocumentRoute> = open
            .iter()
            .map(|(route, _)| route)
            .chain(known.iter().map(|(route, _)| route))
            .cloned()
            .collect();
        let mut others: Vec<DocumentRoute> = Vec::new();
        for route in candidates {
            if !touched.contains(&route)
                && !self.task_read.contains(&route)
                && self.others_document(&route, task)
            {
                others.push(route);
            }
        }
        for (route, _) in open.iter().filter(|(route, _)| others.contains(route)) {
            let title = self.document_title(route);
            known.push((route.clone(), title));
        }
        open.retain(|(route, _)| !others.contains(route));
        others
    }

    /// The text the running task was given, which names the documents it
    /// may act in; empty when no task runs.
    pub(super) fn running_request(&self) -> String {
        self.running
            .as_ref()
            .map(|running| running.request.clone())
            .unwrap_or_default()
    }
}
