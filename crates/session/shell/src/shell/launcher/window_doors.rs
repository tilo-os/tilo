//! One process per app per room: the shell opens an app's later windows in
//! the process it started for the first.
//!
//! When the shell starts a Tilo app in a room it keeps one end of a window
//! door (`runtime_portal::windows`) and hands the other to the launch with
//! its release ([`launcher::registration_gate`]). Every later window of that
//! app in that room is then a request on the door instead of a second
//! process, which could never hold the app's records: one process owns them.
//! The request names the window's instance, the durable id its tab binds
//! (`attach_continuity_instance`), so the window it maps claims its tab just
//! as a launched process's first window does, and carries the window's own
//! connections to the session, admitted here as the launch's:
//!
//! - collaboration, recorded as the window's launch before the shell
//!   connects it (`register_in_process_window`), so the broker attests it,
//!   window instance included, as it attests a launched app;
//! - shell-ui, connected here and attributed to the process's launch
//!   ([`take_window_shell_ui`]).
//!
//! The door is reserved when the first launch is decided, before its process
//! exists, so windows asked for at once (a room's tabs restored together)
//! all reach the one process; the process reads them once it runs. It is
//! retired when the process ends.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::Mutex;

use super::{InstalledApp, RoomId, lock};

/// A reserved or running process's door, by `(package id, room)`.
struct Door {
    /// The shell's end.
    ours: OwnedFd,
    /// The launch's end, until the launch takes it with its release.
    theirs: Option<OwnedFd>,
    /// The launcher process, once it exists: the custody a window's
    /// connections are attributed to.
    launcher_pid: Option<u32>,
    /// Which reservation this is, so the end of an earlier launch never
    /// retires a later one's door.
    generation: u64,
    /// Launch attempts whose windows were routed here before the process
    /// existed: the process settles them once it does, by mapping them.
    waiting: Vec<u64>,
}

static DOORS: Mutex<Option<HashMap<(String, String), Door>>> = Mutex::new(None);

/// The shell-ui connections the shell is making for windows: the launcher
/// each is attributed to, by app id, until the shell-ui service reads it.
static WINDOW_SHELL_UI: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());

fn key(package_id: &str, room: RoomId) -> (String, String) {
    (package_id.to_owned(), room.to_string())
}

/// What became of a window asked for in a room where its app may run.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Routed {
    /// The running process opens it.
    Opened,
    /// No process of the app serves the room: start one.
    NotRunning,
}

/// Reserve the door of the process about to start for `package_id` in
/// `room`, unless one is reserved already. Later windows of the app there
/// are routed to it. The reservation is given up when it is dropped before
/// [`Reservation::launched`]: the launch did not start.
#[allow(
    clippy::significant_drop_tightening,
    reason = "one look at the doors decides and records a window's process, so two launches never both start one"
)]
pub(super) fn reserve(package_id: &str, room: RoomId) -> Option<Reservation> {
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let mut doors = lock(&DOORS);
    let doors = doors.get_or_insert_with(HashMap::new);
    if doors.contains_key(&key(package_id, room)) {
        return None;
    }
    let (ours, theirs) = door_pair()
        .map_err(|error| tracing::warn!(%error, package_id, "no window door for the launch"))
        .ok()?;
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    doors.insert(
        key(package_id, room),
        Door {
            ours,
            theirs: Some(theirs),
            launcher_pid: None,
            generation,
            waiting: Vec::new(),
        },
    );
    Some(Reservation {
        key: key(package_id, room),
        generation,
    })
}

/// The launch that reserved `door`, if it did, is on its way.
pub(super) fn launched(door: Option<Reservation>) {
    if let Some(door) = door {
        door.launched();
    }
}

/// A door reserved for a launch that has not started yet.
pub(super) struct Reservation {
    key: (String, String),
    generation: u64,
}

impl Reservation {
    /// The launch is on its way: it holds the door from its start
    /// ([`HeldDoor::hold`]).
    pub(super) fn launched(self) {
        std::mem::forget(self);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut doors = lock(&DOORS);
        if let Some(doors) = doors.as_mut()
            && doors
                .get(&self.key)
                .is_some_and(|door| door.generation == self.generation && door.theirs.is_some())
        {
            doors.remove(&self.key);
        }
    }
}

/// The door a launch of `package_id` in `room` serves, held for as long as
/// the launch runs: its end goes to the launcher with the release, and when
/// the launch is over (its process ended, or it never started) the door is
/// retired, so the app's next window there starts a process again.
pub(super) struct HeldDoor {
    key: (String, String),
    generation: u64,
    theirs: Option<OwnedFd>,
}

impl HeldDoor {
    /// Hold the door reserved for the launch of `package_id` in `room`, if
    /// one is reserved and no launch holds it yet.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the door is looked up and taken in one hold of the doors"
    )]
    pub(super) fn hold(package_id: &str, room: &str) -> Option<Self> {
        let key = (package_id.to_owned(), room.to_owned());
        let mut doors = lock(&DOORS);
        let door = doors.as_mut()?.get_mut(&key)?;
        let theirs = door.theirs.take()?;
        Some(Self {
            key,
            generation: door.generation,
            theirs: Some(theirs),
        })
    }

    /// The launcher `launcher_pid` serves the door: hand it its end, which
    /// it takes with its release, and the launch attempts routed to it so
    /// far, which its windows settle.
    pub(super) fn serve(&mut self, launcher_pid: u32) -> (Option<OwnedFd>, Vec<u64>) {
        let mut waiting = Vec::new();
        if let Some(door) = lock(&DOORS)
            .as_mut()
            .and_then(|doors| doors.get_mut(&self.key))
            .filter(|door| door.generation == self.generation)
        {
            door.launcher_pid = Some(launcher_pid);
            waiting = std::mem::take(&mut door.waiting);
        }
        (self.theirs.take(), waiting)
    }
}

impl Drop for HeldDoor {
    fn drop(&mut self) {
        let mut doors = lock(&DOORS);
        if let Some(doors) = doors.as_mut()
            && doors
                .get(&self.key)
                .is_some_and(|door| door.generation == self.generation)
        {
            doors.remove(&self.key);
        }
    }
}

/// Open a window of `app` in `room` in the process already serving it, if
/// one does.
#[allow(
    clippy::significant_drop_tightening,
    reason = "one look at the doors decides and records a window's process, so two launches never both start one"
)]
pub(super) fn open_in_running(app: &InstalledApp, room: RoomId) -> Routed {
    let mut doors = lock(&DOORS);
    let Some(door) = doors
        .as_mut()
        .and_then(|doors| doors.get_mut(&key(&app.package_id, room)))
    else {
        return Routed::NotRunning;
    };
    // Its last window closed and it is saving before it ends: the window
    // starts the next process, once this one is gone.
    if door.launcher_pid.is_some_and(super::launch_is_retiring) {
        if let Some(doors) = doors.as_mut() {
            doors.remove(&key(&app.package_id, room));
        }
        return Routed::NotRunning;
    }
    let instance = super::window_instance(app, room);
    let room_id = room.to_string();
    let collaboration = window_collaboration(&app.package_id, &room_id, instance)
        .map_err(|error| tracing::warn!(%error, "a routed window has no collaboration"))
        .ok();
    let shell_ui = door.launcher_pid.and_then(|pid| {
        window_shell_ui(&app.package_id, pid)
            .map_err(|error| tracing::warn!(%error, "a routed window has no shell-ui"))
            .ok()
    });
    let request = ui::window_door::WindowRequest {
        instance,
        collaboration: collaboration.is_some(),
        shell_ui: shell_ui.is_some(),
    };
    match ui::window_door::send(
        &door.ours,
        &request,
        collaboration.as_ref(),
        shell_ui.as_ref(),
    ) {
        Ok(()) => {
            tracing::info!(package_id = %app.package_id, instance, "window opened in the running app");
            // Whoever waits for this launch sees it settled when the process
            // maps the window, as a process's first window settles its own.
            if let Some(token) = super::super::launch_readiness::current() {
                match door.launcher_pid {
                    Some(pid) => super::LaunchSession::current()
                        .startup
                        .started(Some(token), pid),
                    None => door.waiting.push(token),
                }
            }
            Routed::Opened
        }
        Err(error) => {
            // The process is gone; the next launch starts another.
            tracing::info!(%error, package_id = %app.package_id, "the app's window door is closed");
            if let Some(doors) = doors.as_mut() {
                doors.remove(&key(&app.package_id, room));
            }
            Routed::NotRunning
        }
    }
}

/// Where a window of `app` in `room` opens: in the process already serving
/// the room, or in one this launch starts, whose door it reserves. Only a
/// `routable` launch, the app's own window with nothing named to open, can
/// open in a running process.
pub(super) fn route_or_reserve(
    app: &InstalledApp,
    room: RoomId,
    routable: bool,
) -> Result<Option<Reservation>, Routed> {
    if !routable {
        return Ok(None);
    }
    if open_in_running(app, room) == Routed::Opened {
        return Err(Routed::Opened);
    }
    let reserved = reserve(&app.package_id, room);
    // Another launch reserved it in between: its process opens this one.
    if reserved.is_none() && open_in_running(app, room) == Routed::Opened {
        return Err(Routed::Opened);
    }
    Ok(reserved)
}

/// The shell-ui connection being made for a window of `app_id`, if one is:
/// the launcher process its window belongs to. Read once by the shell-ui
/// service for a connection from the shell's own process.
pub(crate) fn take_window_shell_ui(app_id: &str) -> Option<u32> {
    let mut pending = lock(&WINDOW_SHELL_UI);
    let index = pending.iter().position(|(app, _)| app == app_id)?;
    Some(pending.remove(index).1)
}

fn door_pair() -> std::io::Result<(OwnedFd, OwnedFd)> {
    runtime_portal::windows::pair()
}

/// The window's own collaboration connection, attested as the launch of
/// `instance` of `app_id` in `room`.
fn window_collaboration(app_id: &str, room: &str, instance: u64) -> std::io::Result<OwnedFd> {
    super::register_in_process_window(app_id, room, Some(instance));
    let mut stream =
        std::os::unix::net::UnixStream::connect(::launcher::services::collab_socket())?;
    ::launcher::services::write_collab_handoff(&mut stream, app_id, room)?;
    Ok(stream.into())
}

/// The window's own shell-ui connection, attributed to the launch whose
/// process `launcher_pid` serves `app_id`.
fn window_shell_ui(app_id: &str, launcher_pid: u32) -> std::io::Result<OwnedFd> {
    use std::io::Write as _;
    lock(&WINDOW_SHELL_UI).push((app_id.to_owned(), launcher_pid));
    let connected = (|| {
        let mut stream = std::os::unix::net::UnixStream::connect(
            ::launcher::services::socket_for(::launcher::services::SHELL_UI_SERVICE),
        )?;
        let mut frame = serde_json::to_vec(&serde_json::json!({
            "protocol": ::launcher::services::SHELL_UI_LAUNCH_PROTOCOL,
            "app_id": app_id,
            // What the app may know of the network was decided for its
            // launch; the window does not widen it.
            "network_status_allowed": false,
        }))
        .map_err(std::io::Error::other)?;
        frame.push(b'\n');
        stream.write_all(&frame)?;
        stream.flush()?;
        Ok(stream.into())
    })();
    if connected.is_err() {
        take_window_shell_ui(app_id);
    }
    connected
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKAGE: &str = "so.tilo.window-door-test";

    fn room() -> RoomId {
        RoomId::parse("7a110d00-0000-4000-8000-000000000001").unwrap()
    }

    fn has_door(room: RoomId) -> bool {
        lock(&DOORS)
            .as_ref()
            .is_some_and(|doors| doors.contains_key(&key(PACKAGE, room)))
    }

    /// The first window of an app in a room starts its process and reserves
    /// its door; its launch holds the door and hands its end to the
    /// launcher; once the launch is over, a window starts a process again.
    #[test]
    fn a_rooms_app_keeps_one_door_until_its_launch_is_over() {
        let room = room();
        let room_id = room.to_string();
        reserve(PACKAGE, room).expect("reserved").launched();
        let mut held = HeldDoor::hold(PACKAGE, &room_id).expect("the launch holds the door");
        assert!(
            HeldDoor::hold(PACKAGE, &room_id).is_none(),
            "held by one launch"
        );
        assert!(
            reserve(PACKAGE, room).is_none(),
            "a second launch keeps the running process's door"
        );
        assert!(held.serve(4242).0.is_some(), "the launcher's end");
        assert!(held.serve(4242).0.is_none(), "handed over once");
        assert!(has_door(room));
        drop(held);
        assert!(!has_door(room), "the launch is over");
    }

    /// A launch that ends after its door was replaced leaves the new one.
    #[test]
    fn an_earlier_launch_never_retires_a_later_door() {
        let room = RoomId::parse("7a110d00-0000-4000-8000-000000000002").unwrap();
        let room_id = room.to_string();
        reserve(PACKAGE, room).expect("reserved").launched();
        let earlier = HeldDoor::hold(PACKAGE, &room_id).expect("held");
        lock(&DOORS).as_mut().unwrap().remove(&key(PACKAGE, room));
        reserve(PACKAGE, room).expect("reserved again").launched();
        drop(earlier);
        assert!(has_door(room), "the later launch's door stays");
        lock(&DOORS).as_mut().unwrap().remove(&key(PACKAGE, room));
    }

    /// A launch that never started gives its door up.
    #[test]
    fn a_launch_that_never_started_gives_its_door_up() {
        let room = RoomId::parse("7a110d00-0000-4000-8000-000000000003").unwrap();
        drop(reserve(PACKAGE, room).expect("reserved"));
        assert!(!has_door(room));
    }

    fn app(package_id: &str) -> InstalledApp {
        InstalledApp {
            package_id: package_id.into(),
            display_name: "Door".into(),
            localized_names: std::collections::BTreeMap::default(),
            keywords: Vec::new(),
            category: None,
            version: "0.1.0".into(),
            manifest_path: "/apps/door/manifest.toml".into(),
            payload_root: "/apps/door".into(),
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
            opens_secondary: false,
            offers: Vec::new(),
        }
    }

    /// One process per app per room: the second window of an app running in
    /// a room is a request on its door, naming the window's own instance,
    /// which is noted for the tab its window claims; in a room where the app
    /// does not run, or once its process is gone, a window starts a process.
    #[test]
    fn another_window_of_a_running_app_opens_in_its_process() {
        let package = "so.tilo.window-door-route";
        let app = app(package);
        let room = RoomId::parse("7a110d00-0000-4000-8000-000000000004").unwrap();
        let elsewhere = RoomId::parse("7a110d00-0000-4000-8000-000000000005").unwrap();
        assert_eq!(open_in_running(&app, room), Routed::NotRunning);
        reserve(package, room)
            .expect("the first window starts the process")
            .launched();
        let mut held = HeldDoor::hold(package, &room.to_string()).expect("held");
        let launcher_end = held
            .serve(std::process::id())
            .0
            .expect("the launcher's end");

        assert_eq!(open_in_running(&app, room), Routed::Opened);
        let opening = ui::window_door::receive(&launcher_end)
            .expect("a request")
            .expect("open");
        let first = opening.request.instance;
        assert_eq!(open_in_running(&app, room), Routed::Opened);
        let second = ui::window_door::receive(&launcher_end)
            .expect("a request")
            .expect("open")
            .request
            .instance;
        assert_ne!(first, second, "each window is an instance of its own");
        assert!(
            super::super::placement_and_instance_for_toplevel(package, 901)
                .is_some_and(|(_, instance)| instance == Some(first)),
            "the window's instance is noted for the tab it claims"
        );

        assert_eq!(open_in_running(&app, elsewhere), Routed::NotRunning);
        drop(launcher_end);
        drop(held);
        assert_eq!(
            open_in_running(&app, room),
            Routed::NotRunning,
            "the process is gone"
        );
    }

    #[test]
    fn a_window_shell_ui_is_attributed_once() {
        lock(&WINDOW_SHELL_UI).push(("so.tilo.attributed".to_owned(), 77));
        assert_eq!(take_window_shell_ui("so.tilo.attributed"), Some(77));
        assert_eq!(take_window_shell_ui("so.tilo.attributed"), None);
    }
}
