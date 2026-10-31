//! The desk's action log: what the desk stood at after each change, and who
//! changed it when.
//!
//! The desk is kept as the tree it is ([`crate::desk_layout`]); this keeps
//! the changes that made it, beside it, as one Automerge log where each
//! change carries its author and its time ([`history_log::record`]). Reading
//! a workspace's history replays that log into the workspace's room
//! ([`history_log::room_history`]), which is what the Activity app shows: the
//! windows opened and closed, the strips renamed, the layouts restored, and
//! who did each.
//!
//! The log is this device's own, like the desk file beside it: it is written
//! away from the frame that made the change ([`Kept`]) and never leaves the
//! device.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use automerge::AutoCommit;
use tilo_desktop_surface::history_log;

use crate::kept::Kept;

/// The log beside the rest of this device's state.
const FILE: &str = "desk-history.automerge";

/// Where the log lives under `state`, this device's state directory.
fn path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}

/// The desk's changes as this device recorded them.
pub(crate) struct DeskHistory {
    log: Rc<RefCell<AutoCommit>>,
    /// Where the log is kept; a session without a state directory keeps it
    /// in memory alone.
    file: Option<Kept>,
}

impl DeskHistory {
    /// The log kept under `state`, empty when this device has none yet.
    pub(crate) fn load(state: Option<&Path>) -> Self {
        let file = state.map(|state| Kept::new(path(state)));
        let log = file
            .as_ref()
            .and_then(Kept::read)
            .and_then(|bytes| {
                AutoCommit::load(&bytes)
                    .map_err(|error| {
                        tracing::warn!(%error, "the desk's history is not this build's");
                    })
                    .ok()
            })
            .unwrap_or_default();
        Self {
            log: Rc::new(RefCell::new(log)),
            file,
        }
    }

    /// The log, for reading a workspace's history out of it.
    pub(crate) fn log(&self) -> Rc<RefCell<AutoCommit>> {
        Rc::clone(&self.log)
    }

    /// Record `desk` as it stands now, by `author` at `time` (seconds since
    /// the epoch), and keep the log. A desk the log already holds is no
    /// change; whether this one was.
    pub(crate) fn record(&self, desk: &serde_json::Value, author: &str, time: i64) -> bool {
        let mut log = self.log.borrow_mut();
        let wrote = history_log::record(&mut log, desk, author, time);
        if wrote && let Some(file) = &self.file {
            file.save(log.save());
        }
        wrote
    }
}
