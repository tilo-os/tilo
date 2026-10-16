//! Change notification for the membership files another process edits.
//!
//! `tilo-relay-admin` rewrites a room's `membership.json` and
//! `governed-access-v1.json` from outside the daemon. Rather than re-read and
//! parse them on every authorization, the daemon watches the entries of
//! `<state_root>/workspaces` (room directories appearing and going) and the
//! entries of each room directory, and bumps a per-room generation whenever
//! one of those files, or the room directory itself, changes.
//! `workspace_io`'s fresh membership read touches the disk only when that
//! generation moved since the runtime last loaded it.
//!
//! Nothing below a room directory is watched. The room's store creates,
//! writes and removes directories and files for every delta; watching them
//! recursively made each one an inotify watch or event the watcher thread
//! had to handle, about a fifth of a loaded relay's CPU.
//!
//! The watch vouches for a room only while it is live. Before the first native
//! watch is established, after the watch fails, and once the daemon stops the
//! task, every room reads its files on each authorization, exactly as before
//! the watch existed: an unobservable change never leaves a stale grant.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tilo_files::{Change, ChangeKind, FsError, VPath, Watch};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::store::{GOVERNED_ACCESS_FILE, MEMBERSHIP_FILE};
use tilo_lock::MutexRecover as _;

/// Changes queued from the native watches to the watch task.
const QUEUED_CHANGES: usize = 256;

/// One daemon's watch over `<state_root>/workspaces`.
pub(crate) struct MembershipWatch {
    workspaces: PathBuf,
    live: AtomicBool,
    rooms: Mutex<HashMap<u64, Arc<AtomicU64>>>,
    /// Changes the native watches delivered, for tests that the store's
    /// churn is not watched at all.
    #[cfg(test)]
    delivered: AtomicU64,
}

impl MembershipWatch {
    fn room(&self, workspace_id: u64) -> Arc<AtomicU64> {
        self.rooms
            .lock_recover()
            .entry(workspace_id)
            .or_default()
            .clone()
    }

    /// A room nobody has attached to yet needs no bump: its first read loads
    /// the disk anyway.
    fn bump(&self, workspace_id: u64) {
        let room = self.rooms.lock_recover().get(&workspace_id).cloned();
        if let Some(room) = room {
            room.fetch_add(1, Ordering::Release);
            // Sessions in the room re-audit, and so read the change.
            crate::authority_epoch::bump();
        }
    }

    /// A change to `file` of `workspace_id`: the room reads its files again
    /// and its sessions re-audit, unless the file holds exactly what this
    /// process last wrote there itself ([`wrote_own`]), which the room
    /// already holds. The relay rewrites a governed room's access file
    /// whenever it admits someone, after it has applied the change to the
    /// room and announced whatever it narrowed; announcing each rewrite
    /// again woke every session in a public room to re-audit, and to read
    /// the file back, for every newcomer.
    async fn changed(&self, workspace_id: u64, file: &str) {
        let path = self.workspaces.join(workspace_id.to_string()).join(file);
        let own = match tokio::fs::read(&path).await {
            Ok(bytes) => is_own(&path, &bytes),
            Err(_) => false,
        };
        if !own {
            self.bump(workspace_id);
        }
    }

    fn bump_all(&self) {
        let rooms = self.rooms.lock_recover();
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
        .lock_recover()
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
            .lock_recover()
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
        #[cfg(test)]
        delivered: AtomicU64::new(0),
    });
    {
        let mut watches = registry().lock_recover();
        watches.retain(|watch| watch.strong_count() > 0);
        watches.push(Arc::downgrade(&watch));
    }
    let registration = Registration(watch.clone());
    tokio::spawn(async move {
        let _registration = registration;
        run(&watch).await;
    })
}

/// Which native watch a change came from: the one on `workspaces`, or the one
/// on a room's directory.
#[derive(Clone, Copy, Debug)]
enum Origin {
    Rooms,
    Room(u64),
}

/// The native watches of one established watch, each forwarding its changes
/// into one queue. Dropping it stops them all.
struct Watches {
    changes: mpsc::Receiver<(Origin, Change)>,
    sender: mpsc::Sender<(Origin, Change)>,
    rooms: HashMap<u64, JoinHandle<()>>,
    forwards: Vec<JoinHandle<()>>,
}

impl Watches {
    fn forward(&self, origin: Origin, native: Watch) -> JoinHandle<()> {
        let sender = self.sender.clone();
        tokio::spawn(async move {
            loop {
                let change = native.next().await;
                if sender.send((origin, change)).await.is_err() {
                    return;
                }
            }
        })
    }

    fn watch_room(&mut self, workspace_id: u64, native: Watch) {
        let forward = self.forward(Origin::Room(workspace_id), native);
        if let Some(previous) = self.rooms.insert(workspace_id, forward) {
            previous.abort();
        }
    }

    fn unwatch_room(&mut self, workspace_id: u64) {
        if let Some(forward) = self.rooms.remove(&workspace_id) {
            forward.abort();
        }
    }
}

impl Drop for Watches {
    fn drop(&mut self) {
        for forward in self.forwards.iter().chain(self.rooms.values()) {
            forward.abort();
        }
    }
}

async fn run(watch: &Arc<MembershipWatch>) {
    let Some(mut watches) = establish(watch).await else {
        return;
    };
    // The queue never closes: `watches` holds a sender.
    while let Some((origin, change)) = watches.changes.recv().await {
        #[cfg(test)]
        watch.delivered.fetch_add(1, Ordering::Relaxed);
        let rescan = change.kind == ChangeKind::Rescan
            || match origin {
                Origin::Rooms => room_changed(watch, &mut watches, &change).await.is_err(),
                Origin::Room(workspace_id) => {
                    if let Some(file) = membership_file(&change.path) {
                        watch.changed(workspace_id, file).await;
                    }
                    false
                }
            };
        if rescan {
            // Changes were missed, or a room could not be watched: reload
            // every room now, then watch afresh.
            watch.bump_all();
            drop(watches);
            let Some(next) = establish(watch).await else {
                return;
            };
            watches = next;
        }
    }
}

/// A change among the room directories: watch a room that appeared, stop
/// watching one that went, and reload it either way. A room's own watch
/// starts only once its directory exists, so the reload that follows covers
/// whatever was written before. `Err` when a room that exists cannot be
/// watched.
async fn room_changed(
    watch: &MembershipWatch,
    watches: &mut Watches,
    change: &Change,
) -> Result<(), ()> {
    let Some(workspace_id) = room_directory(&change.path) else {
        return Ok(());
    };
    if change.kind == ChangeKind::Removed {
        watches.unwatch_room(workspace_id);
    } else {
        let room = watch.workspaces.join(workspace_id.to_string());
        match tokio::task::spawn_blocking(move || tilo_files::native::watch_entries(&room)).await {
            Ok(Ok(native)) => watches.watch_room(workspace_id, native),
            // Gone again already: its removal follows.
            Ok(Err(FsError::NotFound)) => {}
            Ok(Err(error)) => {
                tracing::warn!(workspace_id, %error, "a room's membership files cannot be watched");
                return Err(());
            }
            Err(_) => return Err(()),
        }
    }
    watch.bump(workspace_id);
    Ok(())
}

/// Start the native watches — the entries of `workspaces`, and of each room
/// directory in it — then reload every room so a change made before they
/// began is not missed. `None`, logged, when the platform or filesystem
/// cannot watch: the task ends and every room reads its files on each
/// authorization.
async fn establish(watch: &MembershipWatch) -> Option<Watches> {
    let workspaces = watch.workspaces.clone();
    let started = tokio::task::spawn_blocking(move || {
        // Below the existing state root only: a re-established watch must not
        // recreate a state root that was removed under it.
        if let Err(error) = std::fs::create_dir(&workspaces)
            && error.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(error.to_string());
        }
        // The room list first, so a room created while the others are being
        // watched is reported by it.
        let rooms_watch =
            tilo_files::native::watch_entries(&workspaces).map_err(|error| error.to_string())?;
        let mut rooms = Vec::new();
        for entry in std::fs::read_dir(&workspaces).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let Some(workspace_id) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u64>().ok())
            else {
                continue;
            };
            match tilo_files::native::watch_entries(&entry.path()) {
                Ok(native) => rooms.push((workspace_id, native)),
                Err(FsError::NotFound) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok((rooms_watch, rooms))
    })
    .await;
    let started = match started {
        Ok(started) => started,
        Err(error) => Err(error.to_string()),
    };
    match started {
        Ok((rooms_watch, rooms)) => {
            let (sender, changes) = mpsc::channel(QUEUED_CHANGES);
            let mut watches = Watches {
                changes,
                sender,
                rooms: HashMap::new(),
                forwards: Vec::new(),
            };
            let forward = watches.forward(Origin::Rooms, rooms_watch);
            watches.forwards.push(forward);
            for (workspace_id, native) in rooms {
                watches.watch_room(workspace_id, native);
            }
            watch.live.store(true, Ordering::Release);
            // Announces the change of liveness too.
            watch.bump_all();
            Some(watches)
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

/// The room a change among the entries of `workspaces` names, if it names
/// one.
fn room_directory(path: &VPath) -> Option<u64> {
    let mut components = path.components();
    let workspace_id = components.next()?.parse().ok()?;
    components.next().is_none().then_some(workspace_id)
}

/// Whether a change among a room directory's entries is one of its
/// membership files. Other files in the room are not membership.
#[cfg(test)]
fn names_membership(path: &VPath) -> bool {
    membership_file(path).is_some()
}

/// The membership file a change among a room directory's entries names, if
/// it names one.
fn membership_file(path: &VPath) -> Option<&'static str> {
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(MEMBERSHIP_FILE), None) => Some(MEMBERSHIP_FILE),
        (Some(GOVERNED_ACCESS_FILE), None) => Some(GOVERNED_ACCESS_FILE),
        _ => None,
    }
}

/// What this process last wrote to each membership file through a path
/// that applies the same change to the room's runtime, by digest.
fn own_writes() -> &'static Mutex<HashMap<PathBuf, [u8; 32]>> {
    static OWN: OnceLock<Mutex<HashMap<PathBuf, [u8; 32]>>> = OnceLock::new();
    OWN.get_or_init(Mutex::default)
}

/// Record that this process wrote `bytes` to the membership file at
/// `path`, and applied them to the room's runtime with every narrowing in
/// them announced: the watch then does not announce the file holding them.
/// Only the latest write of each file is remembered, so the file holding
/// anything else (an earlier write, another process's) is announced.
pub(crate) fn wrote_own(path: &Path, bytes: &[u8]) {
    own_writes()
        .lock_recover()
        .insert(path.to_path_buf(), *blake3::hash(bytes).as_bytes());
}

/// Whether `bytes`, read from `path`, are what this process last wrote
/// there ([`wrote_own`]).
fn is_own(path: &Path, bytes: &[u8]) -> bool {
    own_writes()
        .lock_recover()
        .get(path)
        .is_some_and(|own| own == blake3::hash(bytes).as_bytes())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn path(value: &str) -> VPath {
        VPath::new(value).unwrap()
    }

    #[test]
    fn only_membership_files_and_room_directories_name_a_room() {
        assert_eq!(room_directory(&path("7")), Some(7));
        assert_eq!(room_directory(&path("notes")), None);
        assert_eq!(room_directory(&path("7/membership.json")), None);
        assert_eq!(room_directory(&VPath::root()), None);
        assert!(names_membership(&path("membership.json")));
        assert!(names_membership(&path("governed-access-v1.json")));
        assert!(!names_membership(&path("snapshot.automerge")));
        assert!(!names_membership(&path("pending/membership.json")));
        assert!(!names_membership(&VPath::root()));
    }

    /// A running watch over `state_root` and room 7's generation once the
    /// watch vouches for it.
    async fn live_room(state_root: &Path, workspace_id: u64) -> (JoinHandle<()>, DiskGeneration) {
        let task = spawn_membership_watch(state_root);
        let generation = attach(&state_root.join(format!("workspaces/{workspace_id}"))).unwrap();
        tilo_test_env::within("the watch to go live", async {
            while generation.current().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        (task, generation)
    }

    async fn moves_from(generation: &DiskGeneration, before: Option<u64>, what: &str) {
        tilo_test_env::within(what, async {
            while generation.current() == before {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn a_membership_file_change_bumps_its_room() {
        let tmp = tempfile::tempdir().unwrap();
        let room = tmp.path().join("workspaces/7");
        std::fs::create_dir_all(&room).unwrap();
        let (task, generation) = live_room(tmp.path(), 7).await;
        for file in [MEMBERSHIP_FILE, GOVERNED_ACCESS_FILE] {
            let before = generation.current();
            std::fs::write(room.join(file), b"[]").unwrap();
            moves_from(&generation, before, "the room to bump").await;
        }
        task.abort();
    }

    /// The relay's own rewrite of a room's access file, already applied to
    /// the room, is not announced again; anything else in the file is.
    #[tokio::test]
    async fn the_relays_own_access_write_is_not_announced_again() {
        let tmp = tempfile::tempdir().unwrap();
        let room = tmp.path().join("workspaces/7");
        std::fs::create_dir_all(&room).unwrap();
        let (task, generation) = live_room(tmp.path(), 7).await;
        // Let the establishing events settle before comparing.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let file = room.join(GOVERNED_ACCESS_FILE);
        let own = br#"{"version":1,"member_devices":[],"member_user_roots":[]}"#;
        wrote_own(&file, own);
        let before = generation.current();
        // Written as the store writes it: staged, then renamed into place.
        tilo_files::native::atomic::write(&file, own).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            generation.current(),
            before,
            "the relay's own write must not move the room's generation"
        );
        // What the relay did not write itself is announced, so the watch was
        // looking all along.
        tilo_files::native::atomic::write(&file, br#"{"version":1,"member_devices":[]}"#).unwrap();
        moves_from(&generation, before, "a foreign write to bump the room").await;
        task.abort();
    }

    #[tokio::test]
    async fn a_room_created_after_the_watch_began_is_watched() {
        let tmp = tempfile::tempdir().unwrap();
        let (task, generation) = live_room(tmp.path(), 9).await;
        let before = generation.current();
        let room = tmp.path().join("workspaces/9");
        std::fs::create_dir(&room).unwrap();
        // Bumped once its own watch exists: anything written before is read.
        moves_from(&generation, before, "the new room to bump").await;
        let before = generation.current();
        std::fs::write(room.join(MEMBERSHIP_FILE), b"[]").unwrap();
        moves_from(&generation, before, "the new room's membership to bump").await;
        task.abort();
    }

    #[tokio::test]
    async fn store_churn_below_a_room_is_not_watched() {
        let tmp = tempfile::tempdir().unwrap();
        let room = tmp.path().join("workspaces/7");
        let channel = room.join("retained/channel");
        std::fs::create_dir_all(&channel).unwrap();
        let (task, generation) = live_room(tmp.path(), 7).await;
        // Let the establishing events settle before counting.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let before = generation.current();
        let delivered = generation.watch.delivered.load(Ordering::Relaxed);
        // What retained deltas do to the store: directories made, written,
        // renamed out of and removed, below the room directory.
        for round in 0..50 {
            let staging = channel.join(format!("staging-{round}"));
            std::fs::create_dir_all(staging.join("sorter")).unwrap();
            std::fs::write(staging.join("delta"), b"delta").unwrap();
            std::fs::write(staging.join(MEMBERSHIP_FILE), b"not the room's").unwrap();
            std::fs::rename(staging.join("delta"), channel.join(format!("{round}"))).unwrap();
            std::fs::remove_dir_all(&staging).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            generation.watch.delivered.load(Ordering::Relaxed),
            delivered,
            "no change below a room directory may reach the watch"
        );
        assert_eq!(
            generation.current(),
            before,
            "store churn below a room must not bump its membership generation"
        );
        // The watch is still there and still sees membership.
        std::fs::write(room.join(MEMBERSHIP_FILE), b"[]").unwrap();
        moves_from(&generation, before, "the room to bump").await;
        task.abort();
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
