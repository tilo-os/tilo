//! The session's services as a surface app reaches them.
//!
//! The bar, the desk, the panels and the sheets are ordinary apps of the
//! session that places their windows, and they read what the session
//! publishes the way every app does: [`crate::collaboration::SessionResource`]
//! over a connection the session's broker admits for their launch. What
//! differs is only how the connection arrives — the launcher hands an app a
//! connected descriptor, while a surface app is started by the session
//! itself, which names the socket and the launch on its environment
//! ([`SOCKET_ENV`], [`APP_ID_ENV`], [`WORKSPACE_ENV`]). The app connects that
//! socket and says which launch it is, exactly as the launcher does on its
//! behalf, and the broker attests it by the process it started.
//!
//! [`connect`] runs once, before the app's window opens. A process that is no
//! surface connects nothing: an app started by the launcher already holds its
//! connection, and a process started by hand has no session to reach.

#[cfg(feature = "broker-transport")]
use std::path::Path;

/// The socket the session serves its services on, as a surface app reaches
/// it: what the session names when it serves one.
pub const SOCKET_ENV: &str = "TILO_SURFACE_COLLAB_SOCKET";

/// The app the session started this surface as: what the broker admits its
/// launch by, and what its catalogue rows are published under.
pub const APP_ID_ENV: &str = "TILO_SURFACE_APP_ID";

/// The workspace the session started this surface in: the room its launch
/// names.
pub const WORKSPACE_ENV: &str = "TILO_SURFACE_WORKSPACE";

/// Whether this process is a surface app's, as the session that started it
/// says: a surface is handed the actions socket the session answers on.
#[must_use]
pub fn is_surface() -> bool {
    std::env::var_os(crate::surface_peer::ACTIONS_FD_ENV).is_some()
}

/// The connection this process holds to its session's services, once it has
/// connected: kept for the process's life, because what the published
/// resources are read on ends with it.
#[cfg(feature = "broker-transport")]
static SERVICES: std::sync::Mutex<Option<crate::collaboration::SessionServices>> =
    std::sync::Mutex::new(None);

/// Connect this surface app to its session's services, if it is one: the
/// session's published resources and the actions on them are reached from
/// here on as any app reaches them.
///
/// A surface the session serves nothing to — it runs none of the services a
/// surface reads — connects nothing and says so in the log once.
pub fn connect() {
    #[cfg(not(feature = "broker-transport"))]
    return;
    #[cfg(feature = "broker-transport")]
    {
        if !is_surface() {
            return;
        }
        let Some(socket) = std::env::var_os(SOCKET_ENV) else {
            tracing::debug!("the session this surface belongs to serves no services");
            return;
        };
        let app_id = std::env::var(APP_ID_ENV).unwrap_or_default();
        let workspace = std::env::var(WORKSPACE_ENV).unwrap_or_default();
        match crate::collaboration::SessionServices::connect(
            Path::new(&socket),
            &app_id,
            &workspace,
        ) {
            Ok(services) => {
                *SERVICES
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(services);
            }
            Err(error) => {
                tracing::warn!(%error, "a surface app reaches none of its session's resources");
            }
        }
    }
}
