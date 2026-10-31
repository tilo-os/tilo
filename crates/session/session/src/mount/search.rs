//! What search reads of the session, fed every pass: who is here, the
//! workspaces and the tabs they show, where each window was, and the history
//! the person typed and kept. Search is the session's own, so this is the one
//! place that hands it what only the host knows.

use std::rc::Rc;

use crate::Shared;
use crate::{search, tab_app};
use tilo_desktop_search::providers::places::Place;

impl Shared {
    /// Hand search what the session publishes when it changed: read after
    /// a frame, as the desk cannot read its own tree while it opens search.
    pub(crate) fn observe(&self) {
        // Who is here changes faster than what is published: people and
        // calls come and go between two door revisions, so the empty
        // palette's first group is read every pass.
        self.search.set_here(self.here_now());
        // The workspaces as this frame shows them, before the door revision
        // decides whether the rest is worth reading again: a record held by
        // another workspace is offered under that workspace's name.
        self.search.set_workspaces(self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| (workspace.room().to_string(), workspace.name.clone()))
                .collect()
        }));
        let now = self.doors.revision();
        if now.is_some() && *self.observed.borrow() == now {
            return;
        }
        self.observed.replace(now);
        let tabs = self.handle.read(|tree| {
            let mut tabs = Vec::new();
            for workspace in &tree.workspaces {
                let room = workspace.room().to_string();
                for tab in workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                {
                    tabs.push((tab.id, room.clone(), tab_app(&tab.tile), tab.title.clone()));
                }
            }
            tabs
        });
        self.record_places(&tabs);
        let open_tabs = tabs.iter().map(|(tab, ..)| *tab).collect();
        // The downloaded programs, as their registry publishes them: search
        // lists them from the one registry the app sheets read too.
        let downloaded = self
            .services
            .borrow()
            .read(tilo_app_launch::downloaded::RESOURCE)
            .and_then(|reading| serde_json::from_value(reading).ok());
        self.search
            .observe(&self.doors.resources(), open_tabs, downloaded);
        self.keep_history();
    }

    /// Where each app's window is now, for search's Continue: a window
    /// whose title changed since it was last seen was at a new place.
    pub(crate) fn record_places(&self, tabs: &[(u64, String, Option<String>, String)]) {
        let mut seen = self.places_seen.borrow_mut();
        seen.retain(|tab, _| tabs.iter().any(|(open, ..)| open == tab));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        for (tab, room, app, title) in tabs {
            let Some(app) = app else { continue };
            if title.trim().is_empty() || seen.get(tab) == Some(title) {
                continue;
            }
            seen.insert(*tab, title.clone());
            self.search.record_place(Place {
                app: app.clone(),
                room: room.clone(),
                tab: *tab,
                label: title.clone(),
                value: String::new(),
                anchor: None,
                at: now,
                device: None,
            });
        }
    }

    /// Keep what changed of search's history in its settings section,
    /// through the section's field edits; without a settings service it
    /// lasts as long as the session.
    pub(crate) fn keep_history(&self) {
        for edit in self.search.take_unkept() {
            let kept = self
                .services
                .borrow_mut()
                .invoke(search::HISTORY, "field-edit", &edit);
            if let Some(Err(error)) = kept {
                tracing::warn!(%error, "search history not kept");
            }
        }
    }

    /// Host the desktop's search, with its kept history and what the
    /// session publishes.
    pub(crate) fn start_search(&self) {
        self.handle
            .set_search_host(Rc::clone(&self.search) as Rc<dyn tilo_desktop_surface::SearchHost>);
        if let Some(kept) = self.services.borrow().read(search::HISTORY) {
            self.search.restore(&kept);
        }
        self.observe();
    }
}
