//! What this device shows: the workspace in front, which display each window group is on and
//! whether one covers its display. It is the device's alone — a collaborator has their own
//! monitors and their own workspace in front — and is kept beside this device's other state, as one
//! small file written away from the frame. Nothing else of the layout is here: the strips, window
//! groups and tabs are the workspace's layout record ([`tilo_desktop_model::shared_record`]).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_desktop_model::partition::OutputPartition;
use tilo_desktop_model::{DesktopTree, GroupId, WorkspaceSlot};

/// The file this device's state is kept in, under `state`.
pub(crate) const FILE: &str = "shown.json";

/// Where the file lives under `state`, this device's state directory.
pub(crate) fn path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}

/// What this device shows of the desk.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Shown {
    /// The workspace in front.
    #[serde(default)]
    pub active: WorkspaceSlot,
    /// Which display each window group is shown on.
    #[serde(default, skip_serializing_if = "OutputPartition::is_empty")]
    pub outputs: OutputPartition,
    /// The window group covering a display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<GroupId>,
}

impl Shown {
    /// What `tree` shows.
    pub(crate) fn of(tree: &DesktopTree) -> Self {
        Self {
            active: tree.active,
            outputs: tree.outputs.clone(),
            fullscreen: tree.fullscreen,
        }
    }

    /// Show this in `tree`, as the person left it: the workspace in front where the desk still
    /// holds it, and the display partition. Whether anything changed.
    pub(crate) fn applied(&self, tree: &mut DesktopTree) -> bool {
        let mut changed = false;
        let holds = tree.workspaces.iter().any(|held| held.slot == self.active);
        let active = if holds || tree.workspaces.is_empty() {
            self.active
        } else {
            tree.workspaces
                .first()
                .map_or(self.active, |held| held.slot)
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
pub(crate) struct DeviceState {
    at: Option<PathBuf>,
    /// What was last written, so a pass that changed nothing writes nothing.
    kept: RefCell<Shown>,
    /// The desk revision the last look found, so a desk that did not move is not read.
    seen: Cell<Option<u64>>,
}

impl DeviceState {
    /// The state kept under `state`, and what this build made of it: bytes it does not read are
    /// left where they are, and the session starts on the desk it would show anyway.
    pub(crate) fn load(state: Option<&Path>) -> Self {
        let Some(at) = state.map(path) else {
            return Self::default();
        };
        let kept = read(&at);
        Self {
            at: Some(at),
            kept: RefCell::new(kept),
            seen: Cell::default(),
        }
    }

    /// Show what this device left in `tree`. Whether anything changed.
    pub(crate) fn applied(&self, tree: &mut DesktopTree) -> bool {
        self.kept.borrow().applied(tree)
    }

    /// Keep what `tree` shows, read at `revision` of the desk: a pass that finds the desk where
    /// the last one left it keeps nothing. `None` reads it now, whatever moved. Whether it changed
    /// since it was last kept.
    pub(crate) fn keep(&self, revision: Option<u64>, tree: impl FnOnce() -> DesktopTree) -> bool {
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
            *kept = shown.clone();
        }
        let Some(at) = &self.at else {
            return true;
        };
        write(at, &shown);
        true
    }
}

/// What the file at `at` holds, or nothing this build reads.
#[cfg(target_os = "linux")]
fn read(at: &Path) -> Shown {
    match std::fs::read(at) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            tracing::warn!(path = %at.display(), %error, "what this device shows was not read");
            Shown::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Shown::default(),
        Err(error) => {
            tracing::warn!(path = %at.display(), %error, "what this device shows was not read");
            Shown::default()
        }
    }
}

/// A page keeps no state of its own on a disk.
#[cfg(not(target_os = "linux"))]
fn read(_at: &Path) -> Shown {
    Shown::default()
}

/// Write `shown` where this device keeps it.
#[cfg(target_os = "linux")]
fn write(at: &Path, shown: &Shown) {
    match serde_json::to_vec(shown) {
        Ok(bytes) => tilo_durable_writer::keep(at.to_path_buf(), bytes),
        Err(error) => tracing::warn!(%error, "what this device shows was not written"),
    }
}

#[cfg(not(target_os = "linux"))]
fn write(_at: &Path, _shown: &Shown) {}

impl Default for DeviceState {
    fn default() -> Self {
        Self {
            at: None,
            kept: RefCell::default(),
            seen: Cell::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::{Strip, StripId, Workspace, WorkspaceId};

    fn desk(slot: u64) -> DesktopTree {
        let mut workspace =
            Workspace::in_workspace(WorkspaceSlot(slot), WorkspaceId::new(), "Garden");
        workspace.strips.push(Strip::new(StripId(2), "Main"));
        DesktopTree {
            workspaces: vec![workspace],
            active: WorkspaceSlot(slot),
            ..DesktopTree::default()
        }
    }

    /// The workspace in front is kept and put back, and a desk that no longer holds it shows its
    /// first one.
    #[test]
    fn the_workspace_in_front_is_kept_and_shown_where_the_desk_still_holds_it() {
        let state = DeviceState::default();
        let first = desk(1);
        assert!(
            state.keep(Some(1), || first.clone()),
            "what was never kept is written"
        );
        assert!(
            !state.keep(Some(1), || first.clone()),
            "a desk that did not move is not read"
        );
        assert!(
            !state.keep(Some(2), || first.clone()),
            "an unchanged desk writes nothing"
        );
        let mut second = desk(2);
        second.active = WorkspaceSlot(2);
        assert!(state.keep(Some(3), || second.clone()));
        let mut reopened = desk(2);
        reopened.active = WorkspaceSlot(0);
        assert!(state.applied(&mut reopened));
        assert_eq!(reopened.active, WorkspaceSlot(2));
        let mut gone = desk(9);
        gone.active = WorkspaceSlot(2);
        assert!(state.applied(&mut gone), "the desk holds no such workspace");
        assert_eq!(gone.active, WorkspaceSlot(9));
    }
}
