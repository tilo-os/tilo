//! The mark a rebuilt row goes on from: a host that builds a row anew for what it holds now
//! drops the old one, and the row it puts in its place picks the mark up where it was.

use super::ListRow;
use crate::widget::checkbox::CheckMotion;

/// What a row leaves for the one a rebuild puts in its place.
pub(super) struct KeptRow {
    pub(super) check: CheckMotion,
}

impl ListRow {
    /// What names the row across rebuilds: the id of the item it shows, so a successor
    /// standing somewhere else (a row a person added or removed) still picks its own mark
    /// up.
    pub(super) fn identity(&self) -> Option<String> {
        Some(self.activation.id().to_owned())
    }

    /// Goes on from the row a rebuild dropped.
    pub(super) fn take_up(&mut self, kept: KeptRow) {
        self.check = kept.check;
    }
}

impl Drop for ListRow {
    fn drop(&mut self) {
        let check = std::mem::take(&mut self.check);
        self.hand_on.leave(KeptRow { check });
    }
}
