//! Where a secret a person copies from the workspace's vault goes: the clipboard of the session
//! they sit at, which the daemon reaches at the session's clipboard service as the person's own
//! apps do. The copy is marked a secret, so the clipboard's history and the person's other
//! devices keep none of it, and it is taken off again after a while unless they copied something
//! else meanwhile. The secret never passes through an app: the vault's `copy` runs here.

use std::sync::Arc;
use std::time::Duration;

use tilo_workspace_service::workspaces::SecretClipboard;

/// How long a copied secret stays on the clipboard: what a copy an app makes waits too.
const CLEARS_AFTER: Duration = Duration::from_secs(45);

/// The session's clipboard, connected for each copy: a session that started after the daemon
/// is reached as much as one that was there first.
pub(super) struct SessionClipboard;

impl SecretClipboard for SessionClipboard {
    fn copy_secret(&self, secret: &str) -> bool {
        let socket = tilo_paths::xdg::service_socket(runtime_portal::session_services::CLIPBOARD);
        let Some(stream) = tilo_clipboard::service::connect_session(&socket) else {
            return false;
        };
        let Some(clipboard) = tilo_clipboard::service::Connected::connect(stream) else {
            return false;
        };
        Arc::new(clipboard).copy_secret(secret, CLEARS_AFTER)
    }
}
