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
        let Ok(versions) = owner.with_current(room, |view| {
            let governance = view.peer().governance();
            let members = governance
                .governed_members(std::time::SystemTime::now())
                .unwrap_or_default();
            Ok(governance
                .app_versions()
                .into_iter()
                .map(|installed| {
                    let by = installer_name(&installed.installed_by, &members);
                    (installed.release, by)
                })
                .collect::<Vec<_>>())
        }) else {
            return;
        };
        let mut marks = DEV_BUILDS.lock_recover();
        marks.retain(|(of, _), _| *of != room);
        for (release, by) in &versions {
            if release.dev {
                marks.insert((room, release.app.clone()), by.clone());
            }
        }
        drop(marks);
        for (release, _) in versions {
            follow(&room.to_string(), &release);
        }
    }
}

/// The developer builds rooms run, by room and app: who installed each.
static DEV_BUILDS: LazyLock<
    Mutex<std::collections::HashMap<(crate::shell::state::RoomId, String), String>>,
> = LazyLock::new(Mutex::default);

/// Who installed the developer build of `app` that `room` runs, by the name
/// the room shows for them, when it runs one: what that app's windows in
/// the room say in their bar.
pub(crate) fn dev_build_by(room: crate::shell::state::RoomId, app: &str) -> Option<String> {
    DEV_BUILDS
        .lock_recover()
        .get(&(room, app.to_owned()))
        .cloned()
}

/// The name the room shows for the person `installed_by` (hex), or the
/// start of their key when it shows none.
fn installer_name(installed_by: &str, members: &[tilo_peer_state::GovernedMember]) -> String {
    members
        .iter()
        .find(|member| hex::encode(member.user_public_key) == installed_by)
        .and_then(|member| member.display_name.clone())
        .unwrap_or_else(|| installed_by.chars().take(8).collect())
}

/// The package root this device has installed of `app`, hex.
fn installed_package(app: &str) -> Option<String> {
    package_store::find_installed(app)
        .ok()
        .flatten()
        .map(|installed| installed.package_id_hex)
}

/// What following a room's version does on this device: what it has
/// installed, how it installs a checked package file, and how the app's
/// open windows start again on the new build.
#[derive(Clone, Copy)]
pub(crate) struct Device {
    pub installed: fn(&str) -> Option<String>,
    pub install: fn(&AppRelease, &[u8]) -> Result<(), String>,
    pub restart: fn(&str) -> bool,
}

/// This device: its package store, `app update --from`, and the launcher,
/// which ends the app's launches and puts their windows back in place.
const SESSION: Device = Device {
    installed: installed_package,
    install,
    restart: crate::shell::launcher::restart_on_new_build,
};

/// Follow `release`, the version of its app `room` runs: fetch and install
/// it once, when this device runs another and the room's host keeps it.
pub(crate) fn follow(room: &str, release: &AppRelease) {
    let Some(content) = CONTENT.get().and_then(Weak::upgrade) else {
        return;
    };
    drop(follow_on(SESSION, &content, room, release));
}

/// [`follow`] on `device`, fetching through `content`: the worker that
/// waits for the file, installs it and restarts the app's windows, if one
/// started.
pub(crate) fn follow_on(
    device: Device,
    content: &Arc<RouteContent>,
    room: &str,
    release: &AppRelease,
) -> Option<std::thread::JoinHandle<()>> {
    let file = release
        .file_to_fetch((device.installed)(&release.app).as_deref())?
        .to_owned();
    if !FOLLOWED
        .lock_recover()
        .insert((release.app.clone(), release.package.clone()))
    {
        return None;
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
                .and_then(|()| (device.install)(&release, &bytes))
            {
                tracing::warn!(app = %release.app, %error, "could not run the room's version of an app");
                FOLLOWED
                    .lock_recover()
                    .remove(&(release.app.clone(), release.package.clone()));
                return;
            }
            crate::shell::launcher::invalidate_app_cache();
            ui::app_surface::arrived();
            // Its open windows start again, in place, on the new build.
            let restarted = (device.restart)(&release.app);
            tracing::info!(
                app = %release.app,
                version = %release.version,
                restarted,
                "runs the room's version"
            );
        });
    spawned
        .map_err(|_| tracing::warn!("could not start fetching the room's version of an app"))
        .ok()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashMap};
    use tilo_action_broker::record_content::{
        ContentPeer, ContentPeers, ContentPiece, ContentScope, ContentShelf, ContentSources,
    };

    const APP: &str = "so.tilo.follow-fixture";
    const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    #[derive(Default)]
    struct Shelf(Mutex<HashMap<String, Vec<u8>>>);
    impl ContentShelf for Shelf {
        fn read(&self, _: &ContentScope, hash: &str) -> Option<Vec<u8>> {
            self.0.lock_recover().get(hash).cloned()
        }
        fn keep(&self, _: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
            self.0.lock_recover().insert(hash.into(), bytes.to_vec());
            Ok(())
        }
    }

    /// The room's host, which keeps the package, and what was asked of it.
    #[derive(Default)]
    struct Room(Mutex<Vec<String>>);
    impl ContentPeers for Room {
        fn want(&self, _: &CrdtRoute, hash: &str) {
            self.0.lock_recover().push(hash.into());
        }
        fn answer(&self, _: &CrdtRoute, _: ContentPeer, _: ContentPiece) {}
        fn lacks(&self, _: &CrdtRoute, _: ContentPeer, _: &str) {}
        fn sources(&self, _: &CrdtRoute) -> ContentSources {
            ContentSources {
                peers: BTreeSet::new(),
                hosts: BTreeSet::from([1]),
            }
        }
    }

    static INSTALLED: Mutex<Vec<(String, Vec<u8>)>> = Mutex::new(Vec::new());
    static RESTARTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn runs_v1(_: &str) -> Option<String> {
        Some(hex::encode([1; 32]))
    }
    fn install_for_test(release: &AppRelease, bytes: &[u8]) -> Result<(), String> {
        INSTALLED
            .lock_recover()
            .push((release.package.clone(), bytes.to_vec()));
        Ok(())
    }
    fn restart_for_test(app: &str) -> bool {
        RESTARTED.lock_recover().push(app.into());
        true
    }

    #[test]
    fn a_room_version_is_fetched_checked_installed_and_its_windows_restarted() {
        let package = b"the v2 build".repeat(100);
        let file = blake3::hash(&package).to_hex().to_string();
        let release = AppRelease {
            app: APP.into(),
            version: "2.0.0-dev.1".into(),
            surface: hex::encode([2; 32]),
            package: hex::encode([2; 32]),
            publisher: hex::encode([9; 32]),
            file: Some(file.clone()),
            dev: true,
        };
        let device = Device {
            installed: runs_v1,
            install: install_for_test,
            restart: restart_for_test,
        };
        let room = Arc::new(Room::default());
        let content = RouteContent::new(Arc::new(Shelf::default()), room.clone());

        // Already on the room's version: nothing is fetched.
        let current = Device {
            installed: |_| Some(hex::encode([2; 32])),
            ..device
        };
        assert!(follow_on(current, &content, ROOM, &release).is_none());

        let worker = follow_on(device, &content, ROOM, &release).expect("fetches the version");
        assert!(
            follow_on(device, &content, ROOM, &release).is_none(),
            "once"
        );
        assert_eq!(
            *room.0.lock_recover(),
            vec![file.clone()],
            "asks the room by hash"
        );
        let route = CrdtRoute::app(APP.to_owned(), ROOM.to_owned());
        content.peer_piece(
            &route,
            &ContentPiece {
                hash: file,
                total: package.len() as u64,
                offset: 0,
                bytes: package.clone(),
            },
        );
        worker.join().unwrap();
        assert_eq!(
            *INSTALLED.lock_recover(),
            vec![(release.package.clone(), package)],
            "the checked file is installed, bound to the version's package"
        );
        assert_eq!(*RESTARTED.lock_recover(), vec![APP.to_owned()]);
    }

    #[test]
    fn an_app_ended_for_its_new_build_counts_as_restarting() {
        assert!(!crate::shell::launcher::restarting_on_new_build(
            "so.tilo.other"
        ));
        crate::shell::launcher::mark_restarting_for_test("so.tilo.restarting");
        assert!(crate::shell::launcher::restarting_on_new_build(
            "so.tilo.restarting"
        ));
    }
}
