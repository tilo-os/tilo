//! The profile's admitted resources in the host operating system's search.
//!
//! The daemon is where resources are known, so it keeps the one index per
//! user: GNOME Shell and `KRunner` ask it over the session bus, Spotlight is
//! handed each change (and a chosen result comes back over the native
//! socket), and it walks the catalogue again every [`REFRESH`] so
//! what the owner may no longer see leaves every host search.

use std::{
    io,
    sync::{Arc, Weak},
    time::Duration,
};

use tilo_actions::remote::{ActionProvider, Caller};
use tilo_os_search::{Changes, OsSearch};

/// How long a revoked or closed resource may stay findable.
const REFRESH: Duration = Duration::from_secs(10);

/// Feed host search from `host`, as the profile's owner `actor` confined
/// to the profile's `workspace`: the caller MCP acts as. The feed lasts as long as `part`,
/// the person's part `host` was admitted under: a host outlives its admission when the part
/// ends, and asking it then is refused every time.
pub(crate) fn start<H, P>(
    host: H,
    workspace: &str,
    actor: &str,
    part: &Arc<P>,
) -> io::Result<Arc<OsSearch>>
where
    H: ActionProvider + Send + Sync + 'static,
    P: Send + Sync + 'static,
{
    let search = Arc::new(OsSearch::new(
        Arc::new(host),
        Caller::local_person(actor).in_workspace(workspace),
    ));
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    let feeding = Arc::clone(&search);
    let part = Arc::downgrade(part);
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
        feed(&feeding, &part, REFRESH).await;
    });
    Ok(search)
}

/// Walk the catalogue into `search` every `every` while `part` lives, and end with it.
pub(crate) async fn feed<P: Send + Sync>(search: &OsSearch, part: &Weak<P>, every: Duration) {
    while part.strong_count() > 0 {
        match search.refresh().await {
            Ok(changes) => publish(&changes),
            Err(error) => tracing::debug!(%error, "host search keeps its previous index"),
        }
        tokio::time::sleep(every).await;
    }
}

/// Hand the host searches that keep their own copy what changed.
fn publish(changes: &Changes) {
    #[cfg(target_os = "macos")]
    tilo_os_search::spotlight::apply(changes);
    #[cfg(not(target_os = "macos"))]
    let _ = changes;
}
