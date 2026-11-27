//! What this device shows: its workspaces in its own order, the one in front, which display each
//! window group is on and whether one covers its display. It is the device's alone — a member has their own
//! monitors and their own workspace in front — and is kept as one of the device's records
//! ([`DeviceRecords`]), written away from the frame. Nothing else of the layout is here: the strips, window
//! groups and tabs are the workspace's layout record ([`tilo_desktop_model::shared_resource`]).

use std::cell::{Cell, RefCell};

use serde::{Deserialize, Serialize};
use tilo_desktop_model::partition::OutputPartition;
use tilo_desktop_model::{GroupId, Layout, Workspace, WorkspaceId};
#[cfg(target_os = "linux")]
use tilo_session_client::device_records::DeviceRecord;
use tilo_session_client::device_records::DeviceRecords;

/// The device record what this device shows is kept in.
#[cfg(target_os = "linux")]
const KEPT: &str = tilo_session_client::device_records::SHOWN;

/// What this device shows of the session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Shown {
    /// The workspaces the session shows, in its order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<ShownWorkspace>,
    /// The workspace in front.
    #[serde(default)]
    pub active: WorkspaceId,
    /// Which display each window group is shown on.
    #[serde(default, skip_serializing_if = "OutputPartition::is_empty")]
    pub outputs: OutputPartition,
    /// The window group covering a display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<GroupId>,
}

/// A workspace the session shows, and the name it showed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ShownWorkspace {
    pub id: WorkspaceId,
    pub name: String,
}

impl Shown {
    /// What `tree` shows.
    pub(crate) fn of(tree: &Layout) -> Self {
        Self {
            workspaces: tree
                .workspaces
                .iter()
                .filter(|workspace| !workspace.id().is_nil())
                .map(|workspace| ShownWorkspace {
                    id: workspace.id(),
                    name: workspace.name.clone(),
                })
                .collect(),
            active: tree.active,
            outputs: tree.outputs.clone(),
            fullscreen: tree.fullscreen,
        }
    }

    /// Show this in `tree`, a session being opened, as the person left it: the workspaces it showed,
    /// whose layouts their records hold, the one in front where the session still holds it, and the
    /// display partition. Whether anything changed.
    pub(crate) fn applied(&self, tree: &mut Layout) -> bool {
        let mut changed = false;
        if !self.workspaces.is_empty() {
            let mut opened = std::mem::take(&mut tree.workspaces);
            tree.workspaces = self
                .workspaces
                .iter()
                .map(|shown| {
                    opened
                        .iter()
                        .position(|held| held.id() == shown.id)
                        .map_or_else(
                            || Workspace::new(shown.id, shown.name.clone()),
                            |at| opened.swap_remove(at),
                        )
                })
                .collect();
            changed = true;
        }
        let holds = tree.workspaces.iter().any(|held| held.id() == self.active);
        let active = if holds || tree.workspaces.is_empty() {
            self.active
        } else {
            tree.workspaces
                .first()
                .map_or(self.active, tilo_desktop_model::Workspace::id)
        };
        if tree.active != active {
            tree.active = active;
            changed = true;
        }
        if tree.outputs != self.outputs {
            tree.outputs.clone_from(&self.outputs);
            changed = true;
        }
        if tree.fullscreen != self.fullscreen {
            tree.fullscreen = self.fullscreen;
            changed = true;
        }
        changed
    }
}

/// The state this device keeps, and where it keeps it. A device with nowhere to keep it keeps it in
/// memory alone.
#[derive(Default)]
pub(crate) struct DeviceState {
    #[cfg(target_os = "linux")]
    at: Option<DeviceRecord>,
    /// What was last written, so a pass that changed nothing writes nothing.
    kept: RefCell<Shown>,
    /// The session revision the last look found, so a session that did not move is not read.
    seen: Cell<Option<u64>>,
}

impl DeviceState {
    /// The state kept in `records`, and what this build made of it: a record it does not read is
    /// left as it is, and the session starts on the shell it would show anyway.
    pub(crate) fn load(records: Option<&DeviceRecords>) -> Self {
        #[cfg(target_os = "linux")]
        {
            let Some(at) = records.map(|records| records.record(KEPT)) else {
                return Self::default();
            };
            let kept = read(&at);
            Self {
                at: Some(at),
                kept: RefCell::new(kept),
                seen: Cell::default(),
            }
        }
        // A page keeps no state of its own on a disk.
        #[cfg(not(target_os = "linux"))]
        {
            let _ = records;
            Self::default()
        }
    }

    /// Whether this device kept no workspace to show: a shell opened over it shows only the one it
    /// started with. The shell is the Linux session's ([`crate::platform::session`]).
    #[cfg(target_os = "linux")]
    pub(crate) fn keeps_no_workspace(&self) -> bool {
        self.kept.borrow().workspaces.is_empty()
    }

    /// Show what this device left in `tree`. Whether anything changed.
    pub(crate) fn applied(&self, tree: &mut Layout) -> bool {
        self.kept.borrow().applied(tree)
    }

    /// Keep what `tree` shows, read at `revision` of the session: a pass that finds the session where
    /// the last one left it keeps nothing. `None` reads it now, whatever moved. Whether it changed
    /// since it was last kept.
    pub(crate) fn keep(&self, revision: Option<u64>, tree: impl FnOnce() -> Layout) -> bool {
        if let Some(revision) = revision {
            if self.seen.get() == Some(revision) {
                return false;
            }
            self.seen.set(Some(revision));
        }
        let shown = Shown::of(&tree());
        {
            let mut kept = self.kept.borrow_mut();
            if *kept == shown {
                return false;
            }
            #[cfg(target_os = "linux")]
            if let Some(at) = &self.at {
                write(at, &shown);
            }
            *kept = shown;
        }
        true
    }
}

/// What `at` holds, or nothing this build reads.
#[cfg(target_os = "linux")]
fn read(at: &DeviceRecord) -> Shown {
    match at.read() {
        Ok(Some(value)) => serde_json::from_value(value).unwrap_or_else(|error| {
            tracing::warn!(%error, "what this device shows was not read");
            Shown::default()
        }),
        Ok(None) => Shown::default(),
        Err(error) => {
            tracing::warn!(%error, "what this device shows was not read");
            Shown::default()
        }
    }
}

/// Keep `shown` in `at`.
#[cfg(target_os = "linux")]
fn write(at: &DeviceRecord, shown: &Shown) {
    match serde_json::to_value(shown) {
        Ok(value) => at.set(value),
        Err(error) => tracing::warn!(%error, "what this device shows was not written"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{Strip, StripId};

    fn session(id: u128) -> Layout {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(id), "Garden");
        workspace.strips.push(Strip::new(StripId(2), "Main"));
        Layout {
            workspaces: vec![workspace],
            active: WorkspaceId::from_u128(id),
            ..Layout::default()
        }
    }

    /// The workspaces the session showed and the one in front are kept, and a session opened again shows
    /// them in place of the one it was opened with.
    #[test]
    fn the_workspaces_and_the_one_in_front_are_kept_and_shown_again() {
        let state = DeviceState::default();
        let first = session(1);
        assert!(
            state.keep(Some(1), || first.clone()),
            "what was never kept is written"
        );
        assert!(
            !state.keep(Some(1), || first.clone()),
            "a session that did not move is not read"
        );
        assert!(
            !state.keep(Some(2), || first.clone()),
            "an unchanged session writes nothing"
        );
        let second = session(2);
        assert!(state.keep(Some(3), || second.clone()));
        let mut reopened = session(1);
        assert!(state.applied(&mut reopened));
        assert_eq!(reopened.active, WorkspaceId::from_u128(2));
        let shown: Vec<_> = reopened
            .workspaces
            .iter()
            .map(|workspace| (workspace.id(), workspace.id(), workspace.name.clone()))
            .collect();
        let kept = &second.workspaces[0];
        assert_eq!(
            shown,
            [(WorkspaceId::from_u128(2), kept.id(), "Garden".to_owned())],
            "the session shows the workspace it showed, not the one it was opened with"
        );
    }
}
