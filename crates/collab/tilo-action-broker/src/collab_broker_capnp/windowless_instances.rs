//! The instances of apps the host runs with no window: started for an
//! action that shows nothing, told apart from windows, and never handed an
//! `open`.

use tilo_actions::remote::{APPLICATION_OPEN_ACTION, ActionHostError};

use super::CapnpBrokerState;

impl CapnpBrokerState {
    /// Have the host run `app_id` with no window in `room`, for an action
    /// that shows nothing: how its start ends, where it does.
    pub(super) fn run_windowless(
        &self,
        app_id: &str,
        room: &str,
    ) -> Result<Option<crate::services::WindowlessStart>, ActionHostError> {
        let started = match (
            self.launchable.windowless(),
            self.services.launcher.as_ref(),
        ) {
            (Some(stand_in), _) => stand_in(app_id, room),
            (None, Some(launcher)) => launcher.windowless(app_id, room),
            (None, None) => Ok(None),
        };
        started.map_err(|why| ActionHostError::Unavailable {
            reason: format!("{app_id} could not run without a window: {why}"),
        })
    }

    /// Note whether peer `peer_id`, the connection of `local`, is an
    /// instance of its app the host runs with no window.
    pub(super) fn note_windowless(
        &self,
        peer_id: u64,
        local: Option<&super::native_media::LocalAppConnection>,
    ) {
        if local.is_some_and(|local| local.windowless)
            && let Ok(mut windowless) = self.launchable.windowless_peers.lock()
        {
            windowless.insert(peer_id);
        }
    }

    /// Peer `peer_id` disconnected: forget the window it was for, and that
    /// it had none.
    pub(super) fn forget_windows_of(&self, peer_id: u64) {
        if let Ok(mut windows) = self.window_instances.lock() {
            windows.remove(&peer_id);
        }
        if let Ok(mut windowless) = self.launchable.windowless_peers.lock() {
            windowless.remove(&peer_id);
        }
    }

    /// Whether peer `peer_id` may take an invocation of `action`: an
    /// instance with no window takes every action but `open`, which shows
    /// what it opens and so goes to a window.
    pub(super) fn shows(&self, peer_id: u64, action: &str) -> bool {
        action != APPLICATION_OPEN_ACTION
            || self
                .launchable
                .windowless_peers
                .lock()
                .is_ok_and(|windowless| !windowless.contains(&peer_id))
    }
}

/// Why the start of an instance with no window failed, once it does;
/// pending while there is none to wait on, or once it runs. Cancel-safe: a
/// wait dropped before the start ended leaves `starting` as it was.
pub(super) async fn start_failed(
    starting: &mut Option<crate::services::WindowlessStart>,
    app_id: &str,
) -> ActionHostError {
    let Some(start) = starting.as_mut() else {
        return std::future::pending().await;
    };
    let ended = start.await;
    *starting = None;
    let why = match ended {
        Ok(Ok(())) => return std::future::pending().await,
        Ok(Err(why)) => why,
        Err(_) => "the host stopped starting it".into(),
    };
    ActionHostError::Unavailable {
        reason: format!("{app_id} could not run without a window: {why}"),
    }
}
