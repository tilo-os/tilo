//! The default environment's runtime, imported into the user's store ahead of its first
//! session.

use crate::records::Records;
use crate::store::Store;

/// Import the default environment's runtime into the user's store on a thread of its own,
/// once, as soon as the supervisor runs. The first session of the default environment
/// imports it too when it is not there yet: a system store image has no copy in the user's
/// store until its first use, and the copy reads and checks thousands of files. Starting it
/// when the supervisor starts (which an app opening Terminal does) overlaps that copy with
/// the app's own start, and the store's lock lets the session wait for the copy under way
/// instead of making a second one. The journal records how long it took.
pub(crate) fn start(store: &Store) {
    let records = Records::at(store.records_root());
    let spawned = std::thread::Builder::new()
        .name("ush-runtime-import".into())
        .spawn(move || {
            let images = crate::images::on_this_device();
            let runtime = match records.load(tilo_environment::DEFAULT_RECORD) {
                Ok(Some(environment)) => environment
                    .runtime_image(tilo_environment::host_arch())
                    .ok(),
                Ok(None) => images.current_runtime().ok().map(|(_, id)| id),
                Err(_) => None,
            };
            let Some(runtime) = runtime else { return };
            if images.is_local(&runtime) {
                return;
            }
            let started = std::time::Instant::now();
            tracing::info!(%runtime, "ush: importing the default runtime for its first use");
            match images.tree(&runtime) {
                Ok(_) => tracing::info!(
                    %runtime,
                    elapsed_ms = started.elapsed().as_millis(),
                    "ush: imported the default runtime for its first use"
                ),
                Err(error) => tracing::warn!(
                    %runtime,
                    %error,
                    elapsed_ms = started.elapsed().as_millis(),
                    "ush: importing the default runtime failed; its first session tries again"
                ),
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "ush: could not start the runtime import");
    }
}
