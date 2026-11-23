//! The system's media controls, one service for every Playback of the workspace.
//!
//! An app never publishes a player itself: it writes its Playback resource, and this service
//! shows every active one to the desktop — MPRIS on the Linux session bus, the platform's own
//! now-playing surface on macOS — and turns what the desktop's media keys, its now-playing
//! widget and a script ask into the Playback's own actions. The hardware keys reach the
//! Playback the person touched last ([`Board`]).
//!
//! The board is the whole of the choice: what is active, which one a bare key reaches, and
//! what each control means. The platform half only carries it out, so a platform without a
//! now-playing surface still chooses correctly and a test reads the choice without a bus.

pub mod board;

pub use board::{Board, Change, ChangeKind, Command, Key, PlayerView, Status};

use std::sync::Arc;

/// What one control went to: the Playback's id and what it asks. The host runs it as the
/// Playback's action; the service never reaches a decoder or an app itself.
pub type OnCommand = Arc<dyn Fn(&str, Command) + Send + Sync>;

#[cfg(all(
    feature = "service",
    unix,
    not(target_os = "android"),
    not(target_os = "macos")
))]
mod linux;

#[cfg(all(feature = "service", target_os = "macos"))]
mod macos;

/// The system's media controls over every Playback it is shown.
pub struct MediaControls {
    board: Board,
    #[cfg(all(
        feature = "service",
        unix,
        not(target_os = "android"),
        not(target_os = "macos")
    ))]
    server: linux::Server,
    #[cfg(all(feature = "service", target_os = "macos"))]
    server: macos::Server,
}

impl MediaControls {
    /// Serve every Playback on this platform's now-playing surface, telling `on_command` what a
    /// control asks. A platform without one, or a build without the service, still boards what
    /// plays for [`Self::press`].
    #[must_use]
    pub fn start(runtime: &tokio::runtime::Handle, on_command: OnCommand) -> Self {
        Self::start_on(None, runtime, on_command)
    }

    /// [`Self::start`], serving on the bus at `address` (a test's own) instead of the session's.
    #[must_use]
    #[allow(
        unused_variables,
        clippy::needless_return,
        reason = "one branch per platform, each constructing and returning: only one compiles, so \
                  its `return` is the function's last statement"
    )]
    pub fn start_on(
        address: Option<String>,
        runtime: &tokio::runtime::Handle,
        on_command: OnCommand,
    ) -> Self {
        #[cfg(all(
            feature = "service",
            unix,
            not(target_os = "android"),
            not(target_os = "macos")
        ))]
        {
            return Self {
                board: Board::new(),
                server: linux::Server::start(address, runtime, on_command),
            };
        }
        #[cfg(all(feature = "service", target_os = "macos"))]
        {
            return Self {
                board: Board::new(),
                server: macos::Server::start(on_command),
            };
        }
        #[cfg(not(any(
            all(
                feature = "service",
                unix,
                not(target_os = "android"),
                not(target_os = "macos")
            ),
            all(feature = "service", target_os = "macos")
        )))]
        {
            let _ = on_command;
            let _ = address;
            let _ = runtime;
            Self {
                board: Board::new(),
            }
        }
    }

    /// The Playbacks active now, as the service is showing them.
    #[must_use]
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Show `players` now and stop showing the ones no longer listed.
    pub fn update(&mut self, players: Vec<PlayerView>) {
        let changes = self.board.update(players);
        #[cfg(any(
            all(
                feature = "service",
                unix,
                not(target_os = "android"),
                not(target_os = "macos")
            ),
            all(feature = "service", target_os = "macos")
        ))]
        self.server.apply(&self.board, &changes);
    }

    /// A hardware media key, as the Playback it reaches and what it asks. The caller runs the
    /// command as that Playback's action; a key with nothing to reach answers `None`.
    pub fn press(&mut self, key: Key) -> Option<(String, Command)> {
        self.board.press(key)
    }
}
