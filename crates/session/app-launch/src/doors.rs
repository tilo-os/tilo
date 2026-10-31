//! One process per app per room: the session opens an app's later windows in
//! the process it started for the first.
//!
//! When the session starts a Tilo app in a room it keeps one end of a window
//! door (`runtime_portal::windows`) and hands the other to the launch with
//! its release ([`launcher::registration_gate`]). Every later window of that
//! app in that room is then a request on the door instead of a second
//! process, which could never hold the app's records: one process owns them.
//! The request names the window's instance and carries the window's own
//! connections to the session ([`Connects`]), so the window it maps claims a
//! tab of its own and its data reaches the app it belongs to.
//!
//! The door is reserved when the first launch is decided, before its process
//! exists, so windows asked for at once (a room's tabs restored together) all
//! reach the one process. It is retired when the process ends.
//!
//! The session and the apps are updated apart, so a window goes through the
//! door only once the app has said it reads it
//! (`runtime_portal::windows::SERVES_VERB`), which it does before it maps its
//! first window. Until the app has said it or mapped a window, windows asked
//! for wait at the door; an app that maps a window without having said it was
//! built before the door, and each of its windows starts a process of its own,
//! as it always did.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// An app and a room: one process serves one of each.
type Key = (String, String);

/// The connections a window of a running app holds of its own.
#[derive(Default)]
pub struct WindowConnections {
    /// The window's collaboration connection, attested as its own launch.
    pub collaboration: Option<OwnedFd>,
    /// The window's shell-ui connection, attributed to the app's launch.
    pub shell_ui: Option<OwnedFd>,
}

/// How the session makes a window's connections to itself: the collaboration
/// socket and the shell-ui socket, each of the window's own, so a request the
/// window makes is answered for it alone.
pub trait Connects {
    /// The connections of the window `instance` of `app` in `room`.
    fn window(&self, app: &str, room: &str, instance: u64) -> WindowConnections;
}

/// A reserved or running process's door, by `(app, room)`.
struct Door {
    /// The session's end.
    ours: OwnedFd,
    /// The launch's end, until the launch takes it with its release.
    theirs: Option<OwnedFd>,
    /// The launcher process, once it exists: the custody a window's
    /// connections are attributed to.
    launcher_pid: Option<u32>,
    /// Which reservation this is, so the end of an earlier launch never
    /// retires a later one's door.
    generation: u64,
    /// What the app behind the door said about its windows.
    serves: Serves,
    /// Windows asked for before the app said whether it opens them.
    queued: Vec<Queued>,
}

/// Whether the app behind a door opens the windows the session asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Serves {
    /// It has neither said so nor mapped a window yet.
    Unknown,
    /// It said so.
    Yes,
    /// It mapped a window without saying so: an app built before the door.
    No,
}

/// Starts a window in a process of its own, as a launch without a door does.
pub type StartAlone = Box<dyn FnOnce()>;

/// A window asked for while its app had not yet said whether it opens it.
struct Queued {
    app: String,
    room: String,
    alone: StartAlone,
}

/// What became of a window asked for in a room where its app may run.
#[derive(Debug, PartialEq, Eq)]
pub enum Routed {
    /// The running process opens it.
    Opened,
    /// No process of the app serves the room: start one.
    NotRunning,
}

/// The doors of the apps this session started, by app and room.
pub struct Doors {
    connections: Box<dyn Connects>,
    inner: Mutex<HashMap<Key, Door>>,
    generations: AtomicU64,
    instances: AtomicU64,
}

fn key(app: &str, room: &str) -> Key {
    (app.to_owned(), room.to_owned())
}

impl Doors {
    /// The doors of the apps `connections` makes windows for.
    #[must_use]
    pub fn new(connections: impl Connects + 'static) -> Rc<Self> {
        Rc::new(Self {
            connections: Box::new(connections),
            inner: Mutex::new(HashMap::new()),
            generations: AtomicU64::new(1),
            instances: AtomicU64::new(1),
        })
    }

    fn doors(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Door>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether a process of `app` already serves `room` and opens the
    /// windows asked of it: another window of the app there is a new window
    /// in that process, not a second copy.
    #[must_use]
    pub fn opens_more_windows(&self, app: &str, room: &str) -> bool {
        self.doors()
            .get(&key(app, room))
            .is_some_and(|door| door.serves == Serves::Yes)
    }

    /// Reserve the door of the process about to start for `app` in `room`,
    /// unless one is reserved already. Later windows of the app there are
    /// routed to it. The reservation is given up when it is dropped before
    /// [`Reservation::launched`]: the launch did not start.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "one look at the doors decides and records a window's process, so two launches never both start one"
    )]
    fn reserve(self: &Rc<Self>, app: &str, room: &str) -> Option<Reservation> {
        let mut doors = self.doors();
        let key = key(app, room);
        if doors.contains_key(&key) {
            return None;
        }
        let (ours, theirs) = runtime_portal::windows::pair()
            .map_err(|error| tracing::warn!(%error, app, "no window door for the launch"))
            .ok()?;
        let generation = self.generations.fetch_add(1, Ordering::Relaxed);
        doors.insert(
            key.clone(),
            Door {
                ours,
                theirs: Some(theirs),
                launcher_pid: None,
                generation,
                serves: Serves::Unknown,
                queued: Vec::new(),
            },
        );
        Some(Reservation {
            doors: Rc::clone(self),
            key,
            generation,
        })
    }

    /// The door a launch of `app` in `room` serves, held for as long as the
    /// launch runs: its end goes to the launcher with the release, and when
    /// the launch is over (its process ended, or it never started) the door
    /// is retired, so the app's next window there starts a process again.
    #[must_use]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the door is looked up and taken in one hold of the doors"
    )]
    pub fn hold(self: &Rc<Self>, app: &str, room: &str) -> Option<HeldDoor> {
        let key = key(app, room);
        let mut doors = self.doors();
        let door = doors.get_mut(&key)?;
        let theirs = door.theirs.take()?;
        Some(HeldDoor {
            doors: Rc::clone(self),
            key,
            generation: door.generation,
            theirs: Some(theirs),
        })
    }

    /// Open a window of `app` in `room` in the process already serving it, if
    /// one does: at once when its app opens windows, or once it has said
    /// whether it does. `alone` starts the window in a process of its own if
    /// the app turns out not to.
    fn open_in_running(&self, app: &str, room: &str, alone: &dyn Fn() -> StartAlone) -> Routed {
        let mut doors = self.doors();
        let Some(door) = doors.get_mut(&key(app, room)) else {
            return Routed::NotRunning;
        };
        let mut late = Vec::new();
        if door.serves == Serves::Unknown
            && runtime_portal::windows::take_serves(&door.ours).unwrap_or(false)
        {
            door.serves = Serves::Yes;
            late = std::mem::take(&mut door.queued);
        }
        let routed = match door.serves {
            Serves::No => Routed::NotRunning,
            Serves::Unknown => {
                door.queued.push(Queued {
                    app: app.to_owned(),
                    room: room.to_owned(),
                    alone: alone(),
                });
                Routed::Opened
            }
            Serves::Yes => match send_window(self, door, app, room) {
                Ok(()) => Routed::Opened,
                Err(error) => {
                    // The process is gone; the next launch starts another.
                    tracing::info!(%error, app, "the app's window door is closed");
                    let gone = doors.remove(&key(app, room));
                    drop(doors);
                    if let Some(gone) = gone {
                        start_alone(gone.queued);
                    }
                    start_alone(late);
                    return Routed::NotRunning;
                }
            },
        };
        let failed = late
            .into_iter()
            .filter_map(|queued| {
                let door = doors.get_mut(&key(&queued.app, &queued.room))?;
                send_window(self, door, &queued.app, &queued.room)
                    .is_err()
                    .then_some(queued)
            })
            .collect();
        drop(doors);
        start_alone(failed);
        routed
    }

    /// A window of the process `pid` mapped. If the app behind its door has
    /// not said it opens windows by now it never will: it was built before
    /// the door. The windows that waited go where its answer says.
    pub fn window_mapped(&self, pid: u32) {
        let lineage = ancestry(pid);
        let (open, alone) = {
            let mut doors = self.doors();
            let Some(door) = doors.values_mut().find(|door| {
                door.serves == Serves::Unknown
                    && door
                        .launcher_pid
                        .is_some_and(|launcher| lineage.contains(&launcher))
            }) else {
                return;
            };
            door.serves = if runtime_portal::windows::take_serves(&door.ours).unwrap_or(false) {
                Serves::Yes
            } else {
                Serves::No
            };
            let queued = std::mem::take(&mut door.queued);
            if door.serves == Serves::Yes {
                let failed: Vec<Queued> = queued
                    .into_iter()
                    .filter_map(|queued| {
                        send_window(self, door, &queued.app, &queued.room)
                            .is_err()
                            .then_some(queued)
                    })
                    .collect();
                drop(doors);
                (true, failed)
            } else {
                drop(doors);
                (false, queued)
            }
        };
        if !open {
            tracing::info!("an app built before the window door starts a process per window");
        }
        start_alone(alone);
    }

    /// Where a window of `app` in `room` opens: in the process already
    /// serving the room, or in one this launch starts, whose door it
    /// reserves. Only a `routable` launch, the app's own window with nothing
    /// named to open, can open in a running process.
    ///
    /// # Errors
    /// [`Routed::Opened`]: the running process opens the window and nothing
    /// starts.
    pub fn route_or_reserve(
        self: &Rc<Self>,
        app: &str,
        room: &str,
        routable: bool,
        alone: &dyn Fn() -> StartAlone,
    ) -> Result<Option<Reservation>, Routed> {
        if !routable {
            return Ok(None);
        }
        if self.open_in_running(app, room, alone) == Routed::Opened {
            return Err(Routed::Opened);
        }
        let reserved = self.reserve(app, room);
        // Another launch reserved it in between: its process opens this one.
        if reserved.is_none() && self.open_in_running(app, room, alone) == Routed::Opened {
            return Err(Routed::Opened);
        }
        Ok(reserved)
    }

    /// The instance a window asked for from here on is named by: one of its
    /// own, which its tab and the requests it makes about that tab keep.
    fn next_instance(&self) -> u64 {
        self.instances.fetch_add(1, Ordering::Relaxed)
    }
}

/// A door reserved for a launch that has not started yet.
pub struct Reservation {
    doors: Rc<Doors>,
    key: Key,
    generation: u64,
}

impl Reservation {
    /// The launch is on its way: it holds the door from its start
    /// ([`Doors::hold`]).
    pub fn launched(self) {
        std::mem::forget(self);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut doors = self.doors.doors();
        if doors
            .get(&self.key)
            .is_some_and(|door| door.generation == self.generation && door.theirs.is_some())
        {
            doors.remove(&self.key);
        }
    }
}

/// The door a launch serves, held for as long as the launch runs.
pub struct HeldDoor {
    doors: Rc<Doors>,
    key: Key,
    generation: u64,
    theirs: Option<OwnedFd>,
}

impl HeldDoor {
    /// The launcher `launcher_pid` serves the door: hand it its end, which it
    /// takes with its release.
    pub fn serve(&mut self, launcher_pid: u32) -> Option<OwnedFd> {
        if let Some(door) = self
            .doors
            .doors()
            .get_mut(&self.key)
            .filter(|door| door.generation == self.generation)
        {
            door.launcher_pid = Some(launcher_pid);
        }
        self.theirs.take()
    }
}

impl Drop for HeldDoor {
    fn drop(&mut self) {
        let gone = {
            let mut doors = self.doors.doors();
            if doors
                .get(&self.key)
                .is_some_and(|door| door.generation == self.generation)
            {
                doors.remove(&self.key)
            } else {
                None
            }
        };
        // Windows that waited for a process that never said it opens them
        // start their own.
        if let Some(door) = gone {
            start_alone(door.queued);
        }
    }
}

/// Start each of `queued` in a process of its own, on the pass that is
/// running: a window never waits for another.
fn start_alone(queued: Vec<Queued>) {
    for queued in queued {
        (queued.alone)();
    }
}

/// Ask the process behind `door` for a window of `app` in `room`.
fn send_window(door: &Doors, entry: &Door, app: &str, room: &str) -> std::io::Result<()> {
    let instance = door.next_instance();
    let connections = door.connections.window(app, room, instance);
    let request = ui::window_door::WindowRequest {
        instance,
        collaboration: connections.collaboration.is_some(),
        shell_ui: connections.shell_ui.is_some(),
    };
    ui::window_door::send(
        &entry.ours,
        &request,
        connections.collaboration.as_ref(),
        connections.shell_ui.as_ref(),
    )?;
    tracing::info!(app, instance, "window opened in the running app");
    Ok(())
}

/// The process `pid` and the ones that started it, up to a few parents: a
/// window's process is the one the launcher started, or a child of it.
fn ancestry(pid: u32) -> Vec<u32> {
    const DEPTH: usize = 8;
    let mut lineage = vec![pid];
    let mut current = pid;
    for _ in 0..DEPTH {
        let Some(parent) = parent_of(current) else {
            break;
        };
        if parent == 0 || lineage.contains(&parent) {
            break;
        }
        lineage.push(parent);
        current = parent;
    }
    lineage
}

/// The process that started `pid`, from `/proc/<pid>/stat`.
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm is parenthesised and may hold spaces and parentheses: the fields
    // after the last ')' start with the state, so the parent is the second.
    let rest = stat.rsplit_once(") ")?.1;
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// The connections a test's windows are given: pairs whose far ends the
    /// test reads the request from.
    #[derive(Default)]
    struct TestConnections {
        made: RefCell<Vec<(String, OwnedFd)>>,
    }

    impl Connects for Rc<TestConnections> {
        fn window(&self, app: &str, _room: &str, _instance: u64) -> WindowConnections {
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a socket");
            self.made.borrow_mut().push((app.to_owned(), theirs.into()));
            WindowConnections {
                collaboration: Some(ours.into()),
                shell_ui: None,
            }
        }
    }

    fn doors() -> (Rc<Doors>, Rc<TestConnections>) {
        let connections = Rc::new(TestConnections::default());
        (Doors::new(Rc::clone(&connections)), connections)
    }

    const APP: &str = "so.tilo.window-door-test";
    const ROOM: &str = "7a110d00-0000-4000-8000-000000000001";

    fn has_door(doors: &Doors, app: &str, room: &str) -> bool {
        doors.doors().contains_key(&key(app, room))
    }

    fn never_alone() -> StartAlone {
        Box::new(|| panic!("no window starts a process of its own here"))
    }

    /// One `StartAlone` per window asked for, telling `started` when it ran.
    fn alone_into(started: std::sync::mpsc::Sender<()>) -> impl Fn() -> StartAlone {
        move || {
            let started = started.clone();
            Box::new(move || {
                let _ = started.send(());
            })
        }
    }

    impl Doors {
        fn reserve_for_test(self: &Rc<Self>, app: &str, room: &str) {
            self.reserve(app, room).expect("reserved").launched();
        }
    }

    /// The first window of an app in a room starts its process and reserves
    /// its door; its launch holds the door and hands its end to the
    /// launcher; once the launch is over, a window starts a process again.
    #[test]
    fn a_rooms_app_keeps_one_door_until_its_launch_is_over() {
        let (doors, _) = doors();
        doors.reserve_for_test(APP, ROOM);
        let mut held = doors.hold(APP, ROOM).expect("the launch holds the door");
        assert!(doors.hold(APP, ROOM).is_none(), "held by one launch");
        assert!(
            doors.reserve(APP, ROOM).is_none(),
            "a second launch keeps the running process's door"
        );
        assert!(held.serve(4242).is_some(), "the launcher's end");
        assert!(held.serve(4242).is_none(), "handed over once");
        assert!(has_door(&doors, APP, ROOM));
        drop(held);
        assert!(!has_door(&doors, APP, ROOM), "the launch is over");
    }

    /// A launch that ends after its door was replaced leaves the new one.
    #[test]
    fn an_earlier_launch_never_retires_a_later_door() {
        const ROOM: &str = "7a110d00-0000-4000-8000-000000000002";
        let (doors, _) = doors();
        doors.reserve_for_test(APP, ROOM);
        let earlier = doors.hold(APP, ROOM).expect("held");
        doors.doors().remove(&key(APP, ROOM));
        doors.reserve_for_test(APP, ROOM);
        drop(earlier);
        assert!(has_door(&doors, APP, ROOM), "the later launch's door stays");
    }

    /// A launch that never started gives its door up.
    #[test]
    fn a_launch_that_never_started_gives_its_door_up() {
        const ROOM: &str = "7a110d00-0000-4000-8000-000000000003";
        let (doors, _) = doors();
        drop(doors.reserve(APP, ROOM).expect("reserved"));
        assert!(!has_door(&doors, APP, ROOM));
    }

    /// One process per app per room: the second window of an app running in
    /// a room, once the app said it opens windows, is a request on its door,
    /// naming the window's own instance and carrying its own connections; in
    /// a room where the app does not run, or once its process is gone, a
    /// window starts a process.
    #[test]
    fn another_window_of_a_running_app_opens_in_its_process() {
        const APP: &str = "so.tilo.window-door-route";
        const ROOM: &str = "7a110d00-0000-4000-8000-000000000004";
        const ELSEWHERE: &str = "7a110d00-0000-4000-8000-000000000005";
        let (doors, _) = doors();
        assert_eq!(
            doors.open_in_running(APP, ROOM, &never_alone),
            Routed::NotRunning
        );
        doors.reserve_for_test(APP, ROOM);
        let mut held = doors.hold(APP, ROOM).expect("held");
        let launcher_end = held.serve(4_100_001).expect("the launcher's end");
        runtime_portal::windows::send_serves(&launcher_end).unwrap();

        assert_eq!(
            doors.open_in_running(APP, ROOM, &never_alone),
            Routed::Opened
        );
        let opening = ui::window_door::receive(&launcher_end)
            .expect("a request")
            .expect("open");
        let first = opening.request.instance;
        assert!(opening.request.collaboration, "the window's own connection");
        assert_eq!(
            doors.open_in_running(APP, ROOM, &never_alone),
            Routed::Opened
        );
        let second = ui::window_door::receive(&launcher_end)
            .expect("a request")
            .expect("open")
            .request
            .instance;
        assert_ne!(first, second, "each window is an instance of its own");

        assert_eq!(
            doors.open_in_running(APP, ELSEWHERE, &never_alone),
            Routed::NotRunning
        );
        drop(launcher_end);
        drop(held);
        assert_eq!(
            doors.open_in_running(APP, ROOM, &never_alone),
            Routed::NotRunning,
            "the process is gone"
        );
    }

    /// The session and the apps are updated apart. An app built before the
    /// window door never says it opens windows: a window asked for while it
    /// starts waits until it maps its own, then starts a process of its own,
    /// as every window of such an app always did, and so does every later
    /// one.
    #[test]
    fn an_app_built_before_the_window_door_keeps_a_process_per_window() {
        const APP: &str = "so.tilo.window-door-older-app";
        const ROOM: &str = "7a110d00-0000-4000-8000-000000000006";
        let (doors, _) = doors();
        doors.reserve_for_test(APP, ROOM);
        let mut held = doors.hold(APP, ROOM).expect("held");
        // The process that maps a window is the launcher itself here.
        let _launcher_end = held.serve(std::process::id()).expect("the launcher's end");
        let (started, waiting) = std::sync::mpsc::channel();
        let alone = alone_into(started);

        assert_eq!(
            doors.open_in_running(APP, ROOM, &alone),
            Routed::Opened,
            "waits for the app to answer"
        );
        doors.window_mapped(std::process::id());
        waiting
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the waiting window starts a process of its own");
        assert_eq!(
            doors.open_in_running(APP, ROOM, &alone_into(std::sync::mpsc::channel().0)),
            Routed::NotRunning,
            "a later window starts its own process at once"
        );
    }

    /// An app that reads its door says so before it maps its first window: a
    /// window asked for while it started opens in it.
    #[test]
    fn a_window_asked_for_while_its_app_starts_opens_in_it() {
        const APP: &str = "so.tilo.window-door-starting";
        const ROOM: &str = "7a110d00-0000-4000-8000-000000000007";
        let (doors, _) = doors();
        doors.reserve_for_test(APP, ROOM);
        let mut held = doors.hold(APP, ROOM).expect("held");
        let launcher_end = held.serve(std::process::id()).expect("the launcher's end");
        assert_eq!(
            doors.open_in_running(APP, ROOM, &never_alone),
            Routed::Opened
        );
        runtime_portal::windows::send_serves(&launcher_end).unwrap();
        doors.window_mapped(std::process::id());
        assert!(
            ui::window_door::receive(&launcher_end)
                .expect("a request")
                .is_some(),
            "the window that waited opens in the app"
        );
    }

    /// A launch with something named to open never opens in a running
    /// process: the app's own command line carries it.
    #[test]
    fn a_launch_that_names_what_it_opens_reserves_a_door_and_starts() {
        let (doors, _) = doors();
        let reserved = doors
            .route_or_reserve(APP, ROOM, false, &never_alone)
            .expect("not routed");
        assert!(reserved.is_none(), "a process of its own, without a door");
        let reserved = doors
            .route_or_reserve(APP, ROOM, true, &never_alone)
            .expect("reserved");
        assert!(reserved.is_some(), "the first window reserves the door");
        drop(reserved);
        assert!(!has_door(&doors, APP, ROOM), "a launch that never started");
    }
}
