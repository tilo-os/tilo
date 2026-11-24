//! The hold on suspend a request from another of the person's devices takes: a logind suspend
//! inhibitor, kept while the request's lease is.

use tilo_runtime::device::capabilities::Awake;

/// The logind suspend inhibitor this machine understands, once logind answers.
pub(crate) async fn hold() -> Option<Awake> {
    use tilo_power_mgmt::LogindManager;
    let connection = match zbus::Connection::system().await {
        Ok(connection) => connection,
        Err(error) => {
            tracing::debug!(%error, "no system bus: a remote use cannot hold this machine awake");
            return None;
        }
    };
    let manager = match LogindManager::with_connection(&connection).await {
        Ok(manager) => manager,
        Err(error) => {
            tracing::debug!(%error, "no logind: a remote use cannot hold this machine awake");
            return None;
        }
    };
    match manager
        .inhibit(
            "sleep",
            "Tilo",
            "another of your devices is using this computer",
            "block",
        )
        .await
    {
        Ok(inhibitor) => Some(Box::new(inhibitor)),
        Err(error) => {
            tracing::debug!(%error, "logind refused the hold a remote use asked for");
            None
        }
    }
}
