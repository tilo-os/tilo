//! The session's services as a surface app reaches them. The bar, the desk, the panels
//! and the sheets are ordinary apps of the session that places their windows, and they
//! read what the session publishes the way every app does:
//! [`crate::collaboration::SessionResource`] over a connection the session's broker
//! admits for their launch. The session records the launch of the surface it starts
//! before the app may reach it, and releases it only then
//! (`runtime_portal::registration_gate`): [`connect`] waits for that release, so its
//! connection is never ahead of the record the broker attests it by.

#[cfg(feature = "broker-transport")]
use std::path::Path;

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

/// The connection this process holds to its session's services, once it has connected:
/// kept for the process's life, because what the published resources are read on ends
/// with it.
#[cfg(feature = "broker-transport")]
static SERVICES: std::sync::Mutex<Option<crate::collaboration::SessionServices>> =
    std::sync::Mutex::new(None);

/// Connect this surface app to its session's services, if it is one: the session's
/// published resources and the actions on them are reached from here on as any app
/// reaches them.
pub fn connect() {
    #[cfg(not(feature = "broker-transport"))]
    return;
    #[cfg(feature = "broker-transport")]
    {
        if !is_surface() {
            return;
        }
        // The session records this process's launch before it may reach it, and holds it
        // here until then: the gate is the session's release, sent once the record
        // exists, so the connection below is one the broker can attest and never arrives
        // ahead of its record.
        if std::env::var_os(runtime_portal::registration_gate::LAUNCH_REGISTRATION_STDIN_ENV)
            .is_some()
            && let Err(error) = runtime_portal::registration_gate::wait_on_stdin()
        {
            tracing::warn!(%error, "a surface app was not registered with its session");
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
