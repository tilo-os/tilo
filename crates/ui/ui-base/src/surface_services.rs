//! What a session names in the environment of a surface app it starts: the bar, the shell,
//! the panels and the sheets are ordinary apps of the session that places their windows.
//! `tilo_ui::surface_services::connect` reaches the session's services with them.

/// The socket the session serves its services on, as a surface app reaches it: what the
/// session names when it serves one.
pub const SOCKET_ENV: &str = "TILO_SURFACE_COLLAB_SOCKET";

/// The app the session started this surface as: what the broker admits its launch by, and
/// what its catalogue rows are published under.
pub const APP_ID_ENV: &str = "TILO_SURFACE_APP_ID";

/// The workspace the session started this surface in: the workspace its launch names.
pub const WORKSPACE_ENV: &str = "TILO_SURFACE_WORKSPACE";

/// The environment variable naming the descriptor on which a surface app talks to the
/// session that places it.
pub const ACTIONS_FD_ENV: &str = "TILO_SURFACE_ACTIONS_FD";

/// Whether this process is a surface app's, as the session that started it says: a
/// surface is handed the actions socket the session answers on. No session places a
/// window it did not start in a process of its own, so a build without the native runner
/// is never a surface and nothing names the variable there.
#[must_use]
pub fn is_surface() -> bool {
    std::env::var_os(ACTIONS_FD_ENV).is_some()
}
