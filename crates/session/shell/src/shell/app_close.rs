//! Closing a running application at a broker action's request.
//!
//! The broker runs on its own thread; the tabs it wants closed live in the
//! chrome, on the compositor's. A request travels through a mailbox the
//! compositor installs at startup and drains each loop turn, closing every
//! tab of the application through the one tab lifecycle a person's close
//! takes: durable removal first, then the client's own close request.

use std::sync::OnceLock;

use tilo_desktop_model::{DesktopTree, RoomId, TileContent};

use crate::session_runtime::{Delivery, Mailbox, Wake, mailbox};
use crate::shell::state::TabId;

/// Close every window of `app_id` in the workspace hosting `room`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppCloseRequest {
    pub app_id: String,
    pub room: RoomId,
}

static REQUESTS: OnceLock<Delivery<AppCloseRequest>> = OnceLock::new();

/// Install the compositor's mailbox; later installs keep the first.
pub fn install(wake: Wake) -> Mailbox<AppCloseRequest> {
    let (delivery, requests) = mailbox(wake);
    let _ = REQUESTS.set(delivery);
    requests
}

/// Whether a compositor is draining close requests in this process.
pub fn installed() -> bool {
    REQUESTS.get().is_some()
}

/// Hand `request` to the compositor.
pub fn request(request: AppCloseRequest) -> Result<(), String> {
    let delivery = REQUESTS
        .get()
        .ok_or_else(|| "closing applications is unsupported on this host".to_string())?;
    delivery.deliver(request);
    Ok(())
}

/// The tabs showing a window of `request.app_id` in `request.room`'s workspace.
pub fn tabs_of(tree: &DesktopTree, request: &AppCloseRequest) -> Vec<TabId> {
    tree.workspaces
        .iter()
        .filter(|workspace| workspace.room() == request.room)
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .filter(|tab| {
            matches!(&tab.tile, TileContent::Foreign { app_id, .. }
                if app_id.eq_ignore_ascii_case(&request.app_id))
        })
        .map(|tab| tab.id)
        .collect()
}

#[cfg(test)]
mod tests;
