//! The page's doors to the devices it asks actions of.
//!
//! The page asks an action as one `RemoteActionMessage` and hears its answer: the session it holds
//! with the workspace's host carries the request to that host, whose broker decides it and answers
//! over the same session. The page's own Rust asks these doors directly, and its JavaScript asks
//! them through the runtime, so nothing in the page speaks the frames itself.

use std::cell::RefCell;
use std::collections::HashMap;

use tilo_browser_peer::PeerHandle;
use tilo_browser_peer::session_actions::SessionActions;

/// The doors a page holds, by workspace and device: one session path to each device it asks.
#[derive(Default)]
pub(crate) struct Doors(RefCell<HashMap<(u64, [u8; 32]), SessionActions>>);

impl Doors {
    /// The door to `device`'s actions in `workspace`, through the session `peer` carries.
    pub(crate) fn of(&self, peer: &PeerHandle, workspace: u64, device: [u8; 32]) -> SessionActions {
        self.0
            .borrow_mut()
            .entry((workspace, device))
            .or_insert_with(|| SessionActions::new(peer, workspace, device))
            .clone()
    }
}
