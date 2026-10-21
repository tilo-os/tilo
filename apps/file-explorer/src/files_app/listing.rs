//! Reading a folder without freezing the window.
//!
//! A folder is read in place when that is quick: within
//! [`LISTING_IN_PLACE`], the time under which the window's loading line would
//! not show anyway ([`ui::motion::Loading::DELAY`]), so a small folder opens
//! in the frame that asked for it with no loading state at all. A folder that
//! takes longer (a large one, one on a slow or remote filesystem) goes on
//! being read on its own thread. Meanwhile the window shows the folder
//! loading, its bar's loading line says it is busy ([`ui::window_loading`]),
//! and the listing is shown once the thread hands it over and wakes the
//! window.
//!
//! Only a plain directory's listing is read this way. Recent, the hosted
//! Trash and an archive's contents are read in place, as before.
#[cfg(test)]
#[path = "listing_tests.rs"]
mod tests;

#[allow(
    clippy::wildcard_imports,
    reason = "this file continues the parent module's widget impl; it and the tests under it use the parent's items as the parent does, under every cfg"
)]
use super::*;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, channel};

/// How long a folder's listing is waited for in place before the window
/// shows it loading: the loading line's own delay, so a folder read in place
/// is one that would never have shown a loading state.
pub(super) const LISTING_IN_PLACE: Duration = ui::motion::Loading::DELAY;

/// A listing that outlasted [`LISTING_IN_PLACE`], still being read.
pub(super) struct PendingListing {
    /// The folder being read.
    path: PathBuf,
    /// Where the thread hands the listing over.
    receiver: Receiver<Result<Vec<Entry>, String>>,
}

/// Why a listing never arrived: its thread stopped before handing it over.
fn read_stopped() -> String {
    std::io::Error::from(std::io::ErrorKind::Interrupted).to_string()
}

impl FileExplorerWidget {
    /// How a plain directory's listing of `path` is read, off this widget,
    /// so it can run on a thread of its own. `None` for listings that are
    /// not one directory's entries read as they are (Recent, the hosted
    /// Trash), which are read in place.
    fn directory_reader(
        &mut self,
        path: &Path,
    ) -> Option<impl FnOnce() -> Result<Vec<Entry>, String> + Send + 'static> {
        if crate::recent::is_location(path)
            || (self.hosted_clipboard() && fileops::is_trash_dir(path))
        {
            return None;
        }
        let path = path.to_path_buf();
        let roots = self
            .startup_accounts
            .is_some()
            .then(|| self.workspace_roots.clone());
        // A test holds the read back to see the folder while it loads.
        #[cfg(test)]
        let hold = self.listing_hold.take();
        Some(move || {
            #[cfg(test)]
            if let Some(hold) = hold {
                let _ = hold.recv();
            }
            match roots {
                Some(roots) => crate::model::read_dir_with_workspace_roots(&path, &roots),
                None => read_dir_sync(&path),
            }
        })
    }

    /// Read `path`'s listing: in place when it comes within the budget,
    /// else go on reading it on its own thread and return `None`; the
    /// listing then arrives through [`Self::poll_listing`].
    pub(super) fn read_listing(&mut self, path: &Path) -> Option<Result<Vec<Entry>, String>> {
        let Some(read) = self.directory_reader(path) else {
            return Some(self.read_directory(path));
        };
        let Some(budget) = self.listing_budget else {
            return Some(read());
        };
        let (sender, receiver) = channel();
        let wake = self.listing_wake.clone();
        let spawned = std::thread::Builder::new()
            .name("files-listing".into())
            .spawn(move || {
                // The window may have moved on; then nobody is listening.
                if sender.send(read()).is_ok() {
                    wake.wake();
                }
            });
        if let Err(error) = spawned {
            return Some(Err(error.to_string()));
        }
        match receiver.recv_timeout(budget) {
            Ok(listing) => Some(listing),
            Err(RecvTimeoutError::Disconnected) => Some(Err(read_stopped())),
            Err(RecvTimeoutError::Timeout) => {
                self.pending_listing = Some(PendingListing {
                    path: path.to_path_buf(),
                    receiver,
                });
                None
            }
        }
    }

    /// Take a listing read on its own thread, if it arrived: the folder
    /// shows it. Returns whether it did.
    pub(super) fn poll_listing(&mut self) -> bool {
        let Some(pending) = self.pending_listing.as_ref() else {
            return false;
        };
        let listing = match pending.receiver.try_recv() {
            Ok(listing) => listing,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err(read_stopped()),
        };
        let Some(pending) = self.pending_listing.take() else {
            return false;
        };
        self.finish_load(pending.path, listing);
        self.structure_dirty = true;
        self.report_window_loading();
        true
    }

    /// A read still on its thread is no longer wanted (another folder was
    /// asked for): whatever it finds is dropped with its channel.
    pub(super) fn abandon_listing(&mut self) {
        self.pending_listing = None;
    }

    /// Tell the window's loading line what Files is doing: busy while a
    /// folder (or a cloud folder's page) is being read. An unchanged report
    /// costs nothing.
    pub(super) fn report_window_loading(&self) {
        if self.is_loading || self.cloud.pending.is_some() {
            ui::window_loading::busy();
        } else {
            ui::window_loading::done();
        }
    }
}
