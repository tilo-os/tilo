//! The profile's admitted resources in the host operating system's search.
//!
//! The daemon is where resources are known, so it keeps the one index per
//! user: GNOME Shell and KRunner ask it over the session bus, and it walks
//! the catalogue again every [`REFRESH`] so what the owner may no longer see
//! leaves every host search.

use std::{io, sync::Arc, time::Duration};

use tilo_actions::remote::{ActionHost, Caller};
use tilo_os_search::OsSearch;

/// How long a revoked or closed resource may stay findable.
const REFRESH: Duration = Duration::from_secs(10);

/// Feed host search from `host`, as the profile's owner `actor` confined
/// to the profile's `room`: the caller MCP acts as.
///
/// # Errors
///
/// Returns an error when the app runtime cannot start.
pub(crate) fn start<H>(host: H, room: &str, actor: &str) -> io::Result<()>
where
    H: ActionHost + Send + Sync + 'static,
{
    let search = Arc::new(OsSearch::new(
        Arc::new(host),
        Caller::local_person(actor).in_room(room),
    ));
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    runtime.handle().spawn(async move {
        #[cfg(all(unix, not(target_os = "macos")))]
        let _providers = tilo_os_search::dbus::serve_on_session_bus(&search)
            .await
            .inspect_err(|error| {
                tracing::info!(%error, "GNOME Shell and KRunner cannot search this runtime");
            })
            .ok();
        loop {
            if let Err(error) = search.refresh().await {
                tracing::debug!(%error, "host search keeps its previous index");
            }
            tokio::time::sleep(REFRESH).await;
        }
    });
    Ok(())
}
