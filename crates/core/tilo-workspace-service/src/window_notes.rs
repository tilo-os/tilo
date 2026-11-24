//! What each window this device runs shows: the record its app holds, by the window instance the
//! shell launched it for, as the device's broker names it
//! ([`AppBrokerState::window_resources`](crate::apps::collab_app_broker::AppBrokerState::window_resources)).
//!
//! The desktop notes beside each window the record it shows, so a window's bar names where it is
//! and a place comes back on it. The broker that knows it runs in the device's daemon, so the
//! daemon publishes it here, to the person's shell on its own shell link alone: the desktop keeps
//! no broker of its own to read it from.

use crate::apps::route::CrdtRoute;

/// The resource the device's daemon publishes what its windows hold as, to the desktop on its shell
/// link alone.
pub const RESOURCE: &str = "workspaces.window-records";

/// The record one window's app holds, as a reader is shown it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WindowRecord {
    /// The window instance the shell launched: the tab it fills.
    pub window: u64,
    /// The app whose record it is.
    pub app: String,
    /// The workspace the record is in.
    pub workspace: String,
    /// The record's id.
    pub record: String,
}

/// What the windows this device runs show: a record for each window whose app holds one, as the
/// broker lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WindowNotes {
    pub windows: Vec<WindowRecord>,
}

impl WindowNotes {
    /// The notes `held` names: what the broker's windows show, by instance.
    #[must_use]
    pub fn of(held: &[(u64, CrdtRoute)]) -> Self {
        Self {
            windows: held
                .iter()
                .map(|(window, route)| WindowRecord {
                    window: *window,
                    app: route.app_id.clone(),
                    workspace: route.cluster_id.clone(),
                    record: route.document_id.clone(),
                })
                .collect(),
        }
    }

    /// What each window holds, as the broker names it: the instance and the record's route.
    #[must_use]
    pub fn routes(&self) -> Vec<(u64, CrdtRoute)> {
        self.windows
            .iter()
            .map(|window| {
                (
                    window.window,
                    CrdtRoute::record(
                        window.app.clone(),
                        window.workspace.clone(),
                        window.record.clone(),
                    ),
                )
            })
            .collect()
    }

    /// The notes as the wire carries them.
    #[must_use]
    pub fn reading(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// What a [`Self::reading`] of another device says, in place of what these showed.
    ///
    /// # Errors
    /// The reading does not read.
    pub fn take_reading(reading: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(reading)
            .map_err(|error| format!("what the windows hold does not read: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The notes' reading carries each window's record, and reads back as the routes the desktop
    /// notes.
    #[test]
    fn the_notes_read_back_as_the_routes_they_were_published_from() {
        let held = [
            (
                7,
                CrdtRoute::record("so.tilo.notes", "workspace-1", "note-1"),
            ),
            (
                8,
                CrdtRoute::record("so.tilo.chat", "workspace-1", "messages"),
            ),
        ];
        let notes = WindowNotes::of(&held);
        assert_eq!(notes.routes(), held, "each window's record reads back");
        let read = WindowNotes::take_reading(&notes.reading()).expect("the reading reads");
        assert_eq!(read, notes);
    }
}
