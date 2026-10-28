pub use tilo_model_client::*;

pub mod caller;
pub mod conversation;
#[cfg(all(unix, feature = "server"))]
pub mod http_service;
pub mod openai;
/// The model service a browser page runs for its own windows.
pub mod page_service;
#[cfg(all(unix, feature = "server"))]
pub mod service;
#[cfg(all(unix, feature = "host-backend"))]
pub use service::host_backend;
#[cfg(all(unix, feature = "server"))]
pub mod session_directory;

/// Read the person's settings as the session holds them, never holding them
/// itself: the gateway may start before the session, which must still be
/// the one that holds them. Its writes go to the session.
#[cfg(all(unix, feature = "server"))]
pub(crate) fn follow_session_settings() {
    let Some(root) = settings_store::config_root() else {
        return;
    };
    match settings_store::Store::follow(&root) {
        Ok(store) => settings_store::install_source(Some(store)),
        Err(error) => eprintln!("tilo-model-gateway: settings unreadable: {error}"),
    }
}
