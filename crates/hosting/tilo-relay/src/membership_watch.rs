//! Change notification for the membership files another process edits.
//!
//! `tilo-relay-admin` rewrites a room's `membership.json` and
//! `governed-access-v1.json` from outside the daemon. Rather than re-read and
//! parse them on every authorization, the daemon watches
//! `<state_root>/workspaces` and bumps a per-room generation whenever one of
//! those files, or the room directory itself, changes. `workspace_io`'s fresh
//! membership read touches the disk only when that generation moved since the
//! runtime last loaded it.
//!
//! The watch vouches for a room only while it is live. Before the first native
//! watch is established, after the watch fails, and once the daemon stops the
//! task, every room reads its files on each authorization, exactly as before
//! the watch existed: an unobservable change never leaves a stale grant.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use tilo_files::{ChangeKind, Dir, Location, VPath, Watch};
use tokio::task::JoinHandle;

use crate::store::{GOVERNED_ACCESS_FILE, MEMBERSHIP_FILE};

/// A room directory created after the native watch began is itself watched
/// only once the watcher has seen it appear, so a file written in between
/// reports no event. One more reload this long after the directory appears
/// covers that window.
const NEW_ROOM_SETTLE: Duration = Duration::from_secs(1);

/// One daemon's watch over `<state_root>/workspaces`.
pub(crate) struct MembershipWatch {
    workspaces: PathBuf,
    live: AtomicBool,
    rooms: Mutex<HashMap<u64, Arc<AtomicU64>>>,
}

impl MembershipWatch {
    fn room(&self, workspace_id: u64) -> Arc<AtomicU64> {
        self.rooms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(workspace_id)
            .or_default()
            .clone()
    }

    /// A room nobody has attached to yet needs no bump: its first read loads
    /// the disk anyway.
    fn bump(&self, workspace_id: u64) {
        let room = self
            .rooms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&workspace_id)
            .cloned();
        if let Some(room) = room {
            room.fetch_add(1, Ordering::Release);
            // Sessions in the room re-audit, and so read the change.
            crate::authority_epoch::bump();
        }
    }

    fn bump_all(&self) {
        let rooms = self.rooms.lock().unwrap_or_else(PoisonError::into_inner);
        for room in rooms.values() {
            room.fetch_add(1, Ordering::Release);
        }
        drop(rooms);
        crate::authority_epoch::bump();
    }

    /// Whether the watch vouches for its rooms. Either way sessions re-audit:
    /// one that stops being vouched for goes back to reading the files on
    /// every audit's cadence.
    fn set_live(&self, live: bool) {
        self.live.store(live, Ordering::Release);
        crate::authority_epoch::bump();
    }
}

/// A runtime's view of its room's on-disk generation.
#[derive(Clone)]
pub(crate) struct DiskGeneration {
    watch: Arc<MembershipWatch>,
    room: Arc<AtomicU64>,
}

impl DiskGeneration {
    /// The room's generation, or `None` while the watch cannot vouch for it.
    /// Read it before the files: a change during the read then leaves the
    /// loaded generation behind, and the next authorization reads again.
    pub(crate) fn current(&self) -> Option<u64> {
        let generation = self.room.load(Ordering::Acquire);
        self.watch
            .live
            .load(Ordering::Acquire)
            .then_some(generation)
    }

    pub(crate) fn is_live(&self) -> bool {
        self.watch.live.load(Ordering::Acquire)
    }

    /// What the watch does when it sees this room's files change.
    #[cfg(test)]
    pub(crate) fn bump(&self) {
        self.room.fetch_add(1, Ordering::Release);
    }
}

fn registry() -> &'static Mutex<Vec<Weak<MembershipWatch>>> {
    static WATCHES: OnceLock<Mutex<Vec<Weak<MembershipWatch>>>> = OnceLock::new();
    WATCHES.get_or_init(Mutex::default)
}

/// The generation of the room stored at `room_root`
/// (`<state_root>/workspaces/<id>`), when a running watch covers it.
pub(crate) fn attach(room_root: &Path) -> Option<DiskGeneration> {
    let workspace_id = room_root.file_name()?.to_str()?.parse().ok()?;
    let workspaces = room_root.parent()?;
    let watch = registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter_map(Weak::upgrade)
        .find(|watch| watch.workspaces == workspaces)?;
    let room = watch.room(workspace_id);
    Some(DiskGeneration { watch, room })
}

/// Unregisters the watch, and stops it vouching for any room, when its task
/// ends or is aborted.
struct Registration(Arc<MembershipWatch>);

impl Drop for Registration {
    fn drop(&mut self) {
        self.0.set_live(false);
        registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|watch| watch.strong_count() > 0 && watch.as_ptr() != Arc::as_ptr(&self.0));
    }
}

/// Watches `<state_root>/workspaces` and bumps a room's generation when its
/// membership or governed-access file changes. Abort the handle to stop it.
pub(crate) fn spawn_membership_watch(state_root: &Path) -> JoinHandle<()> {
    let watch = Arc::new(MembershipWatch {
        workspaces: state_root.join("workspaces"),
        live: AtomicBool::new(false),
        rooms: Mutex::default(),
    });
    {
        let mut watches = registry().lock().unwrap_or_else(PoisonError::into_inner);
        watches.retain(|watch| watch.strong_count() > 0);
        watches.push(Arc::downgrade(&watch));
    }
    let registration = Registration(watch.clone());
    tokio::spawn(async move {
        let _registration = registration;
        run(&watch).await;
    })
}

async fn run(watch: &Arc<MembershipWatch>) {
    let Some(mut native) = establish(watch).await else {
        return;
    };
    loop {
        let change = native.next().await;
        if change.kind == ChangeKind::Rescan {
            // Changes were missed, and a directory the native watcher could
            // not add stays unwatched: reload every room now, then watch the
            // tree afresh.
            watch.bump_all();
            let Some(next) = establish(watch).await else {
                return;
            };
            native = next;
            continue;
        }
        let Some((workspace_id, room_directory)) = changed_room(&change.path) else {
            continue;
        };
        watch.bump(workspace_id);
        if room_directory && change.kind == ChangeKind::Changed {
            let later = Arc::downgrade(watch);
            tokio::spawn(async move {
                tokio::time::sleep(NEW_ROOM_SETTLE).await;
                if let Some(watch) = later.upgrade() {
                    watch.bump(workspace_id);
                }
            });
        }
    }
}

/// Start a native watch over the whole tree, then reload every room so a
/// change made before it began is not missed. `None`, logged, when the
/// platform or filesystem cannot watch: the task ends and every room reads its
/// files on each authorization.
async fn establish(watch: &MembershipWatch) -> Option<Watch> {
    let workspaces = watch.workspaces.clone();
    let started = tokio::task::spawn_blocking(move || {
        // Below the existing state root only: a re-established watch must not
        // recreate a state root that was removed under it.
        if let Err(error) = std::fs::create_dir(&workspaces)
            && error.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(error.to_string());
        }
        Dir::open(Location::Native(workspaces))
            .and_then(|dir| dir.watch(&VPath::root()))
            .map_err(|error| error.to_string())
    })
    .await;
    let started = match started {
        Ok(started) => started,
        Err(error) => Err(error.to_string()),
    };
    match started {
        Ok(native) => {
            watch.live.store(true, Ordering::Release);
            // Announces the change of liveness too.
            watch.bump_all();
            Some(native)
        }
        Err(error) => {
            tracing::warn!(
                path = %watch.workspaces.display(),
                %error,
                "membership files cannot be watched; every authorization reads them from disk"
            );
            None
        }
    }
}

/// The room a change below `workspaces` concerns, and whether it is the room
/// directory itself. Other files in the room are not membership.
fn changed_room(path: &VPath) -> Option<(u64, bool)> {
    let mut components = path.components();
    let workspace_id = components.next()?.parse().ok()?;
    match (components.next(), components.next()) {
        (None, _) => Some((workspace_id, true)),
        (Some(MEMBERSHIP_FILE | GOVERNED_ACCESS_FILE), None) => Some((workspace_id, false)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> VPath {
        VPath::new(value).unwrap()
    }

    #[test]
    fn only_membership_files_and_room_directories_name_a_room() {
        assert_eq!(changed_room(&path("7/membership.json")), Some((7, false)));
        assert_eq!(
            changed_room(&path("7/governed-access-v1.json")),
            Some((7, false))
        );
        assert_eq!(changed_room(&path("7")), Some((7, true)));
        assert_eq!(changed_room(&path("7/snapshot.automerge")), None);
        assert_eq!(changed_room(&path("7/pending/membership.json")), None);
        assert_eq!(changed_room(&path("notes/membership.json")), None);
        assert_eq!(changed_room(&VPath::root()), None);
    }

    #[tokio::test]
    async fn a_stopped_watch_no_longer_vouches_for_its_rooms() {
        let tmp = tempfile::tempdir().unwrap();
        let task = spawn_membership_watch(tmp.path());
        let generation = attach(&tmp.path().join("workspaces/3")).unwrap();
        tilo_test_env::within("the watch to go live", async {
            while generation.current().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        task.abort();
        let _ = task.await;
        assert_eq!(generation.current(), None);
        assert!(attach(&tmp.path().join("workspaces/3")).is_none());
    }
}
