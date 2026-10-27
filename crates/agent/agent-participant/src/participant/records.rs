//! Which record a step lands in when it names none, or names one an
//! import cannot go to: a blank record the task made, the latest it acted
//! on, the room's one written record of the app, else a new one; never a
//! document another agent is writing ([`super::authors`]).

use automerge::ReadDoc as _;
use tilo_browser_peer::DocumentRoute;

use super::AgentParticipant;
use super::running::imports;
use crate::planner::Step;

impl AgentParticipant {
    /// Whether the record `record` of `app` is known to hold more than a
    /// new record of its app starts with. Not known is not written.
    pub(super) fn written(&mut self, app: &str, record: &str) -> bool {
        let route = DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: self.room.clone(),
            record: record.to_owned(),
        };
        let Some(genesis) = self.genesis_changes(app) else {
            return false;
        };
        let changes = match self.apps.get(&route) {
            Some(replica) => replica.read(|doc| doc.stats().num_changes),
            None => self.unopened.changes(&route),
        };
        changes.is_some_and(|changes| changes > genesis)
    }

    /// The record an open of `app` naming none opens: the plan asked for a
    /// new document, so a blank one this task made (the host opened it for
    /// the task), else a new one; never one already open with content (a
    /// plan offered this open beside an open document wants a second one).
    pub(super) fn new_record(&mut self, app: &str) -> Result<DocumentRoute, String> {
        let made: Vec<DocumentRoute> = self
            .running
            .iter()
            .flat_map(|running| running.made.iter().rev())
            .filter(|route| route.app_id == app && self.apps.contains_key(route))
            .cloned()
            .collect();
        for route in made {
            if self.blank(&route) {
                return Ok(route);
            }
        }
        let route = self.open(app, None)?;
        if let Some(running) = self.running.as_mut()
            && !running.made.contains(&route)
        {
            running.made.push(route.clone());
        }
        Ok(route)
    }

    /// The record `step`, which names none, acts on
    /// ([`Running::record_for`]): one open, else (unless it imports) the
    /// room's only record of its app that holds anything, else a new one
    /// the task made. A blank record the agent merely holds gives way to
    /// that written one: it is what another agent filled (the workbook it
    /// imported), where a new record would hold none of it.
    pub(super) fn unnamed_record(&mut self, step: &Step) -> Result<DocumentRoute, String> {
        if let Some(route) = self.existing_record(step)? {
            return Ok(route);
        }
        let route = self.open(&step.app, None)?;
        if let Some(running) = self.running.as_mut()
            && !running.made.contains(&route)
        {
            running.made.push(route.clone());
        }
        Ok(route)
    }

    /// The record of the room [`Self::unnamed_record`] finds for `step`
    /// without making one: `None` where it would open a new record.
    pub(super) fn existing_record(&mut self, step: &Step) -> Result<Option<DocumentRoute>, String> {
        let open: Vec<DocumentRoute> = self
            .apps
            .keys()
            .filter(|route| route.app_id == step.app)
            .cloned()
            .collect();
        // Another member's written document is never where a step naming
        // no record lands, unless the task names it.
        let request = self.running_request();
        let mut foreign: Vec<DocumentRoute> = Vec::new();
        let touched: Vec<DocumentRoute> = self
            .running
            .iter()
            .flat_map(|running| running.touched.iter())
            .cloned()
            .collect();
        for route in open.iter().chain(&touched) {
            if route.app_id == step.app
                && !foreign.contains(route)
                && self.others_document(route, &request)
            {
                foreign.push(route.clone());
            }
        }
        let open: Vec<DocumentRoute> = open
            .into_iter()
            .filter(|route| !foreign.contains(route))
            .collect();
        let blank: Vec<DocumentRoute> = open
            .iter()
            .filter(|route| self.blank(route))
            .cloned()
            .collect();
        let found = match self.running.as_ref() {
            Some(running) => running.record_for(
                step,
                &open,
                |route| blank.contains(route),
                |route| foreign.contains(route),
            ),
            None => open
                .iter()
                .find(|route| !blank.contains(route))
                .or_else(|| open.first())
                .cloned(),
        };
        let found = found.filter(|route| self.apps.contains_key(route));
        let holds_nothing = found.as_ref().is_none_or(|route| {
            blank.contains(route)
                && !self
                    .running
                    .as_ref()
                    .is_some_and(|running| running.holds(route))
        });
        if holds_nothing && !imports(step) {
            let mut known: Vec<DocumentRoute> = Vec::new();
            for route in self.unopened.routes() {
                if route.app_id == step.app
                    && !super::parts::is_part(&route.record)
                    && !self.others_document(&route, &request)
                {
                    known.push(route);
                }
            }
            if let Some(written) = self.only_written(known) {
                return self.open(&step.app, Some(&written.record)).map(Some);
            }
        }
        Ok(found)
    }

    /// The one of `routes`, records of one app the agent has not opened,
    /// that holds anything, when exactly one does.
    pub(super) fn only_written(&mut self, routes: Vec<DocumentRoute>) -> Option<DocumentRoute> {
        let mut written = routes.into_iter().filter(|route| !self.blank(route));
        match (written.next(), written.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }

    /// Whether the record at `route` (open, or seen and not opened) holds
    /// only what a new record of its app starts with: nobody changed it
    /// since its genesis. Not known (the app could not be previewed, the
    /// record not read) is not blank.
    pub(super) fn blank(&mut self, route: &DocumentRoute) -> bool {
        let Some(genesis) = self.genesis_changes(&route.app_id) else {
            return false;
        };
        let changes = match self.apps.get(route) {
            Some(app) => app.read(|doc| doc.stats().num_changes),
            None => self.unopened.changes(route),
        };
        changes.is_some_and(|changes| changes <= genesis)
    }

    /// How many changes a new record of `app` holds ([`Self::preview`]).
    pub(super) fn genesis_changes(&mut self, app: &str) -> Option<u64> {
        if !self.genesis_changes.contains_key(app) {
            self.preview(app)?;
        }
        self.genesis_changes.get(app).copied()
    }
}
