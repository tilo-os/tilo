//! The session's services as a surface app reaches them. The bar, the shell, the panels
//! and the sheets are ordinary apps of the session that places their windows, and they
//! read what the session publishes the way every app does: the session's resources over a
//! connection the session's broker admits for their launch. The session records the launch
//! of the surface it starts before the app may reach it, and releases it only then
//! (`runtime_portal::launch_hold`): [`connect`] waits for that release, so its connection is
//! never ahead of the record the broker attests it by.

#[cfg(all(feature = "broker-transport", feature = "model"))]
use std::path::Path;

pub use tilo_ui_base::surface_services::*;

/// The connection this process holds to its session's services, once it has connected:
/// kept for the process's life, because what the published resources are read on ends
/// with it.
#[cfg(all(feature = "broker-transport", feature = "model"))]
static SERVICES: std::sync::Mutex<Option<collaboration::SessionServices>> =
    std::sync::Mutex::new(None);

/// Connect this surface app to its session's services, if it is one: the session's
/// published resources and the actions on them are reached from here on as any app
/// reaches them.
pub fn connect() {
    #[cfg(not(all(feature = "broker-transport", feature = "model")))]
    return;
    #[cfg(all(feature = "broker-transport", feature = "model"))]
    {
        if !is_surface() {
            return;
        }
        // The session records this process's launch before it may reach it, and holds it
        // here until then: the hold is the session's release, sent once the record
        // exists, so the connection below is one the broker can attest and never arrives
        // ahead of its record.
        if std::env::var_os(runtime_portal::launch_hold::LAUNCH_REGISTRATION_STDIN_ENV).is_some()
            && let Err(error) = runtime_portal::launch_hold::wait_on_stdin()
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
        match collaboration::SessionServices::connect(Path::new(&socket), &app_id, &workspace) {
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
