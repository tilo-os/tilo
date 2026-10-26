//! Running the version of an app a room runs.
//!
//! A room's governance names the version each app runs in it
//! (`install-app`). When that is a package this device does not have
//! installed, and the room's host keeps its file, the device asks the room
//! for the file by its hash, on the app's record-content route like any
//! content a record references, checks the bytes hash to what the room
//! names, and updates the app from them with `app update --from`, bound to
//! the version's package root and publisher. The writes held for the
//! room's surface are then decided again, as after any app update
//! ([`crate::shell::app_versions::surface_named`]).

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex, OnceLock, Weak};

use tilo_action_broker::record_content::{ContentState, RouteContent};
use tilo_action_broker::route::CrdtRoute;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_membership::AppRelease;

/// The session's record-content service, once its broker made one.
static CONTENT: OnceLock<Weak<RouteContent>> = OnceLock::new();

/// Each `(app, package)` this device fetched or is fetching, once.
static FOLLOWED: LazyLock<Mutex<HashSet<(String, String)>>> = LazyLock::new(Mutex::default);

/// Fetch room versions through `content`.
pub(crate) fn fetch_through(content: &Arc<RouteContent>) {
    let _ = CONTENT.set(Arc::downgrade(content));
}

/// How often a room's versions are looked at.
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// When each room's versions were last looked at.
static CHECKED: LazyLock<
    Mutex<std::collections::HashMap<crate::shell::state::RoomId, std::time::Instant>>,
> = LazyLock::new(Mutex::default);

impl crate::shell::network::ShellNetwork {
    /// Follow the versions of apps `room` runs ([`follow`]). Cheap: looks
    /// at most every [`CHECK_EVERY`], and only at an owner already open.
    /// Call every tick.
    pub fn follow_room_app_versions(&self, room: crate::shell::state::RoomId) {
        let now = std::time::Instant::now();
        {
            let mut checked = CHECKED.lock_recover();
            if checked
                .get(&room)
                .is_some_and(|at| now.saturating_duration_since(*at) < CHECK_EVERY)
            {
                return;
            }
            checked.insert(room, now);
        }
        let Some(owner) = self.opened_native_owner() else {
            return;
        };
        let Ok(releases) = owner.with_current(room, |view| {
            Ok(view
                .peer()
                .governance()
                .app_versions()
                .into_iter()
                .map(|installed| installed.release)
                .collect::<Vec<_>>())
        }) else {
            return;
        };
        for release in releases {
            follow(&room.to_string(), &release);
        }
    }
}

/// The package root this device has installed of `app`, hex.
fn installed_package(app: &str) -> Option<String> {
    package_store::find_installed(app)
        .ok()
        .flatten()
        .map(|installed| installed.package_id_hex)
}

/// Follow `release`, the version of its app `room` runs: fetch and install
/// it once, when this device runs another and the room's host keeps it.
pub(crate) fn follow(room: &str, release: &AppRelease) {
    let Some(file) = release
        .file_to_fetch(installed_package(&release.app).as_deref())
        .map(str::to_owned)
    else {
        return;
    };
    let Some(content) = CONTENT.get().and_then(Weak::upgrade) else {
        return;
    };
    if !FOLLOWED
        .lock_recover()
        .insert((release.app.clone(), release.package.clone()))
    {
        return;
    }
    tracing::info!(
        app = %release.app,
        version = %release.version,
        room,
        "the room runs another version of an app: fetching it from the room"
    );
    let route = CrdtRoute::app(release.app.clone(), room.to_owned());
    let mut news = content.subscribe(&route);
    content.want(&route, &file);
    let release = release.clone();
    let spawned = std::thread::Builder::new()
        .name("room-app-version".into())
        .spawn(move || {
            let bytes = loop {
                match news.blocking_recv() {
                    Some(news) if news.hash == file && news.state == ContentState::Resolved => {
                        break news.bytes;
                    }
                    Some(_) => {}
                    None => return,
                }
            };
            if let Err(error) = release
                .check_file(&bytes)
                .and_then(|()| install(&release, &bytes))
            {
                tracing::warn!(app = %release.app, %error, "could not run the room's version of an app");
                FOLLOWED
                    .lock_recover()
                    .remove(&(release.app.clone(), release.package.clone()));
                return;
            }
            crate::shell::launcher::invalidate_app_cache();
            ui::app_surface::arrived();
            tracing::info!(app = %release.app, version = %release.version, "runs the room's version");
        });
    if spawned.is_err() {
        tracing::warn!("could not start fetching the room's version of an app");
    }
}

/// Update the app from `bytes`, the room's checked package file, bound to
/// the release's package root and publisher: the installer verifies the
/// signature and refuses any other package.
fn install(release: &AppRelease, bytes: &[u8]) -> Result<(), String> {
    let bin = crate::shell::catalogue::resolve_catalogue_bin()
        .ok_or("the app command is not installed")?;
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = dir.path().join(format!("{}.tapp", release.app));
    std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
    let output = std::process::Command::new(bin)
        .arg("update")
        .arg(&release.app)
        .arg("--from")
        .arg(&path)
        .args(["--require-package-root", &release.package])
        .args(["--require-publisher", &release.publisher])
        .arg("--yes")
        .output()
        .map_err(|error| error.to_string())?;
    crate::shell::catalogue::invalidate();
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}
