//! The profile's admitted resources in the host operating system's search.
//!
//! The daemon is where resources are known, so it keeps the one index per
//! user: GNOME Shell and `KRunner` ask it over the session bus, Spotlight is
//! handed each change (and a chosen result comes back over the native
//! socket), and it walks the catalogue again every [`REFRESH`] so
//! what the owner may no longer see leaves every host search.

use std::{io, sync::Arc, time::Duration};

use tilo_actions::remote::{ActionHost, Caller};
use tilo_os_search::{Changes, OsSearch};

/// How long a revoked or closed resource may stay findable.
const REFRESH: Duration = Duration::from_secs(10);

/// Feed host search from `host`, as the profile's owner `actor` confined
/// to the profile's `room`: the caller MCP acts as.
///
/// # Errors
///
/// Returns an error when the app runtime cannot start.
pub(crate) fn start<H>(host: H, room: &str, actor: &str) -> io::Result<Arc<OsSearch>>
where
    H: ActionHost + Send + Sync + 'static,
{
    let search = Arc::new(OsSearch::new(
        Arc::new(host),
        Caller::local_person(actor).in_room(room),
    ));
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let feeding = Arc::clone(&search);
    runtime.handle().spawn(async move {
        #[cfg(all(unix, not(target_os = "macos")))]
        let _providers = tilo_os_search::dbus::serve_on_session_bus(&feeding)
            .await
            .inspect_err(|error| {
                tracing::info!(%error, "GNOME Shell and KRunner cannot search this runtime");
            })
            .ok();
        #[cfg(target_os = "macos")]
        if tilo_os_search::spotlight::bundled() {
            tilo_os_search::spotlight::reset();
        } else {
            tracing::info!("Spotlight indexes for an app bundle; this runtime runs without one");
        }
        loop {
            match feeding.refresh().await {
                Ok(changes) => publish(&changes),
                Err(error) => tracing::debug!(%error, "host search keeps its previous index"),
            }
            tokio::time::sleep(REFRESH).await;
        }
    });
    Ok(search)
}

/// Hand the host searches that keep their own copy what changed.
fn publish(changes: &Changes) {
    #[cfg(target_os = "macos")]
    tilo_os_search::spotlight::apply(changes);
    #[cfg(not(target_os = "macos"))]
    let _ = changes;
}
