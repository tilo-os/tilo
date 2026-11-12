//! One process per app per workspace: the session opens an app's later windows in
//! the process it started for the first.
//!
//! When the session starts a Tilo app in a workspace it keeps one end of a window
//! door (`runtime_portal::windows`) and hands the other to the launch with
//! its release ([`launcher::registration_gate`]). Every later window of that
//! app in that workspace is then a request on the door instead of a second
//! process, which could never hold the app's records: one process owns them.
//! The request names the window's instance and carries the window's own
//! connections to the session ([`Connects`]), so the window it maps claims a
//! tab of its own and its data reaches the app it belongs to.
//!
//! The door is reserved when the first launch is decided, before its process
//! exists, so windows asked for at once (a workspace's tabs restored together) all
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

/// An app and a workspace: one process serves one of each.
type Key = (String, String);

/// How the session makes a window's connections to itself: the collaboration
/// socket and the shell-ui socket, each of the window's own, so a request the
/// window makes is answered for it alone.
pub trait Connects {
    /// The collaboration connection of the window `instance` of `app` in
    /// `workspace`, attested as that window of the launch whose process is
    /// `launcher_pid` while it runs.
    fn collaboration(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
        launcher_pid: Option<u32>,
    ) -> Option<OwnedFd>;

    /// The shell-ui connection of that window, attributed to its launch.
    fn shell_ui(&self, app: &str, instance: u64, launcher_pid: Option<u32>) -> Option<OwnedFd>;
}

/// A window of a tilolang app whose instance a tilolang host runs: the
/// window holds none of the app's data and shows what its instance sends over
/// its patch connection. The session hands the instance to the host of its
/// workspace and author with the host's end of that connection and the
/// window's own collaboration connection.
pub struct HostedWindow {
    pub app: String,
    pub workspace: String,
    /// The window's instance.
    pub instance: u64,
    /// The launch the window belongs to.
    pub launcher_pid: u32,
    /// The host's end of the window's patch connection.
    pub patch: OwnedFd,
    /// The instance's collaboration connection, attested as the window.
    pub collaboration: OwnedFd,
    /// What the window was asked to show, which its instance opens on.
    pub opens: Opens,
}

/// What a window is asked to show: the record a launch named, and the item the window opens on.
/// A window that holds its app's data opens the record itself, from its launch's command line,
/// and the item from its window request; a hosted window's instance is handed both.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opens {
    /// The record the launch named, by its id.
    pub record: Option<String>,
    /// The item the window opens on, by its anchor: a search hit's, an agent's `open`, the
    /// conversation a send landed in.
    pub reveal: Option<String>,
}

impl Opens {
    /// A window asked to open on the item `reveal`, when it names one.
    #[must_use]
    pub fn revealing(reveal: Option<&str>) -> Self {
        Self {
            record: None,
            reveal: reveal.map(str::to_owned),
        }
    }
}

/// What the session does with a hosted window's instance.
pub type HostsInstances = Box<dyn Fn(HostedWindow)>;

/// A reserved or running process's door, by `(app, workspace)`.
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
    /// A tilolang app whose instances a tilolang host runs: its process is
    /// its windows alone, each shown over a patch connection the door carries.
    hosted: bool,
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
    workspace: String,
    /// What the shell asked the window to show.
    opens: Opens,
    /// The window's instance: the tab the shell held for it, or one the door
    /// names when the shell named none.
    instance: Option<u64>,
    alone: StartAlone,
}

/// What became of a window asked for in a workspace where its app may run.
#[derive(Debug, PartialEq, Eq)]
pub enum Routed {
    /// The running process opens it.
    Opened,
    /// No process of the app serves the workspace: start one.
    NotRunning,
}

/// The doors of the apps this session started, by app and workspace.
pub struct Doors {
    connections: Box<dyn Connects>,
    /// Where a hosted window's instance goes.
    hosts: std::cell::RefCell<Option<HostsInstances>>,
    inner: Mutex<HashMap<Key, Door>>,
    generations: AtomicU64,
    instances: AtomicU64,
}

fn key(app: &str, workspace: &str) -> Key {
    (app.to_owned(), workspace.to_owned())
}

impl Doors {
    /// The doors of the apps `connections` makes windows for.
    #[must_use]
    pub fn new(connections: impl Connects + 'static) -> Rc<Self> {
        Rc::new(Self {
            connections: Box::new(connections),
            hosts: std::cell::RefCell::new(None),
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

    /// Reserve the door of the process about to start for `app` in `workspace`,
    /// unless one is reserved already. Later windows of the app there are
    /// routed to it. The reservation is given up when it is dropped
    /// before [`Reservation::launched`]: the launch did not start.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "one look at the doors decides and records a window's process, so two launches never both start one"
    )]
    fn reserve(self: &Rc<Self>, app: &str, workspace: &str) -> Option<Reservation> {
        let mut doors = self.doors();
        let key = key(app, workspace);
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
                hosted: false,
            },
        );
        Some(Reservation {
            doors: Rc::clone(self),
            key,
            generation,
        })
    }

    /// The door a launch of `app` in `workspace` serves, held for as long as the
    /// launch runs: its end goes to the launcher with the release, and when
    /// the launch is over (its process ended, or it never started) the door
    /// is retired, so the app's next window there starts a process again.
    #[must_use]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the door is looked up and taken in one hold of the doors"
    )]
    pub fn hold(self: &Rc<Self>, app: &str, workspace: &str) -> Option<HeldDoor> {
        let key = key(app, workspace);
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

    /// Open a window of `app` in `workspace` in the process already serving it, if
    /// one does: at once when its app opens windows, or once it has said
    /// whether it does. `alone` starts the window in a process of its own if
    /// the app turns out not to.
    fn open_in_running(
        &self,
        app: &str,
        workspace: &str,
        opens: &Opens,
        alone: &dyn Fn() -> StartAlone,
        instance: Option<u64>,
    ) -> Routed {
        let mut doors = self.doors();
        let Some(door) = doors.get_mut(&key(app, workspace)) else {
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
                    workspace: workspace.to_owned(),
                    opens: opens.clone(),
                    instance,
                    alone: alone(),
                });
                Routed::Opened
            }
            Serves::Yes => match send_window(self, door, app, workspace, opens, instance) {
                Ok(()) => Routed::Opened,
                Err(error) => {
                    // The process is gone; the next launch starts another.
                    tracing::info!(%error, app, "the app's window door is closed");
                    let gone = doors.remove(&key(app, workspace));
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
                let door = doors.get_mut(&key(&queued.app, &queued.workspace))?;
                send_window(
                    self,
                    door,
                    &queued.app,
                    &queued.workspace,
                    &queued.opens,
                    queued.instance,
                )
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
                        send_window(
                            self,
                            door,
                            &queued.app,
                            &queued.workspace,
                            &queued.opens,
                            queued.instance,
                        )
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

    /// Reserve the door of a process about to start for `app` in `workspace`
    /// with no window: `None` when a process of the app runs there already,
    /// or is starting, and serves the workspace in its place.
    ///
    /// # Errors
    /// No door could be made for the process.
    pub fn reserve_unless_running(
        self: &Rc<Self>,
        app: &str,
        workspace: &str,
    ) -> Result<Option<Reservation>, String> {
        if self.doors().contains_key(&key(app, workspace)) {
            return Ok(None);
        }
        self.reserve(app, workspace)
            .map(Some)
            .ok_or_else(|| format!("no window door for {app}"))
    }

    /// Where a window of `app` in `workspace` opens: in the process already
    /// serving the workspace, or in one this launch starts, whose door it
    /// reserves. Only a `routable` launch, the app's own window with nothing
    /// named to open, can open in a running process.
    ///
    /// #
    /// Errors [`Routed::Opened`]: the running process opens the window and nothing
    /// starts.
    pub fn route_or_reserve(
        self: &Rc<Self>,
        app: &str,
        workspace: &str,
        routable: bool,
        opens: &Opens,
        alone: &dyn Fn() -> StartAlone,
        instance: Option<u64>,
    ) -> Result<Option<Reservation>, Routed> {
        if !routable {
            return Ok(None);
        }
        if self.open_in_running(app, workspace, opens, alone, instance) == Routed::Opened {
            return Err(Routed::Opened);
        }
        let reserved = self.reserve(app, workspace);
        // Another launch reserved it in between: its process opens this one.
        if reserved.is_none()
            && self.open_in_running(app, workspace, opens, alone, instance) == Routed::Opened
        {
            return Err(Routed::Opened);
        }
        Ok(reserved)
    }

    /// Hand the instances of hosted windows to `hosts` from here on.
    pub fn hand_instances_to(&self, hosts: HostsInstances) {
        *self.hosts.borrow_mut() = Some(hosts);
    }

    /// The process reserved for `app` in `workspace` is a tilolang app's
    /// windows alone: a tilolang host runs their instances. Whether its door
    /// is reserved.
    pub fn host_instances(&self, app: &str, workspace: &str) -> bool {
        self.doors()
            .get_mut(&key(app, workspace))
            .map(|door| door.hosted = true)
            .is_some()
    }

    /// The first window of the hosted app `app` in `workspace`, which the
    /// launch whose door it serves started: send it its patch connection on
    /// the door, and hand its instance to its host.
    ///
    /// # Errors
    /// The door is not served, or the connections could not be made.
    pub fn first_hosted_window(
        &self,
        app: &str,
        workspace: &str,
        instance: u64,
        opens: &Opens,
    ) -> std::io::Result<()> {
        let hosted = self
            .doors()
            .get(&key(app, workspace))
            .filter(|door| door.hosted)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotConnected))
            .and_then(|door| hosted_window(self, door, app, workspace, instance, opens))?;
        self.hand_over(hosted);
        Ok(())
    }

    fn hand_over(&self, hosted: HostedWindow) {
        if let Some(hosts) = self.hosts.borrow().as_ref() {
            hosts(hosted);
        } else {
            tracing::error!(app = %hosted.app, "a tilolang window's instance has no host to go to");
        }
    }

    /// The instance a window asked for from here on is named by: one of its
    /// own, which its tab and the requests it makes about that tab keep.
    #[must_use]
    pub fn next_instance(&self) -> u64 {
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

/// The window `instance` of the hosted app `app` in `workspace`, whose
/// process is behind `entry`: its patch connection is sent on the door, before
/// the window is asked for, and the host's end goes with the instance, with
/// what the window `opens`.
fn hosted_window(
    door: &Doors,
    entry: &Door,
    app: &str,
    workspace: &str,
    instance: u64,
    opens: &Opens,
) -> std::io::Result<HostedWindow> {
    let launcher_pid = entry.launcher_pid.ok_or(std::io::ErrorKind::NotConnected)?;
    let collaboration = door
        .connections
        .collaboration(app, workspace, instance, Some(launcher_pid))
        .ok_or_else(|| std::io::Error::other("the instance has no collaboration connection"))?;
    let (window, host) = runtime_portal::tilolang_host::pair()?;
    runtime_portal::windows::send_patch(&entry.ours, instance, &window)?;
    Ok(HostedWindow {
        app: app.to_owned(),
        workspace: workspace.to_owned(),
        instance,
        launcher_pid,
        patch: host,
        collaboration,
        opens: opens.clone(),
    })
}

/// Ask the process behind `door` for a window of `app` in `workspace`.
fn send_window(
    door: &Doors,
    entry: &Door,
    app: &str,
    workspace: &str,
    opens: &Opens,
    instance: Option<u64>,
) -> std::io::Result<()> {
    // The shell names the tab a window takes, which is the window's instance;
    // a window asked for with no tab of its own gets one from the door.
    let instance = instance.unwrap_or_else(|| door.next_instance());
    // A tilolang host runs the window's instance: the window gets its patch
    // connection, and the instance the window's collaboration connection.
    let hosted = entry
        .hosted
        .then(|| hosted_window(door, entry, app, workspace, instance, opens))
        .transpose()?;
    let collaboration = if hosted.is_some() {
        None
    } else {
        door.connections
            .collaboration(app, workspace, instance, entry.launcher_pid)
    };
    let shell_ui = door.connections.shell_ui(app, instance, entry.launcher_pid);
    let request = ui::window_door::WindowRequest {
        instance,
        collaboration: collaboration.is_some(),
        shell_ui: shell_ui.is_some(),
        reveal: opens.reveal.clone(),
    };
    ui::window_door::send(
        &entry.ours,
        &request,
        collaboration.as_ref(),
        shell_ui.as_ref(),
    )?;
    tracing::info!(app, instance, "window opened in the running app");
    if let Some(hosted) = hosted {
        door.hand_over(hosted);
    }
    Ok(())
}

/// The process `pid` and the ones that started it, up to a few parents: a
/// window's process is the one the launcher started, or a child of it.
#[must_use]
pub fn ancestry(pid: u32) -> Vec<u32> {
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
        fn collaboration(
            &self,
            app: &str,
            _workspace: &str,
            _instance: u64,
            _launcher_pid: Option<u32>,
        ) -> Option<OwnedFd> {
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a socket");
            self.made.borrow_mut().push((app.to_owned(), theirs.into()));
            Some(ours.into())
        }

        fn shell_ui(
            &self,
            _app: &str,
            _instance: u64,
            _launcher_pid: Option<u32>,
        ) -> Option<OwnedFd> {
            None
        }
    }

    fn doors() -> (Rc<Doors>, Rc<TestConnections>) {
        let connections = Rc::new(TestConnections::default());
        (Doors::new(Rc::clone(&connections)), connections)
    }

    const APP: &str = "so.tilo.window-door-test";
    const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000001";

    fn has_door(doors: &Doors, app: &str, workspace: &str) -> bool {
        doors.doors().contains_key(&key(app, workspace))
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
        fn reserve_for_test(self: &Rc<Self>, app: &str, workspace: &str) {
            self.reserve(app, workspace).expect("reserved").launched();
        }
    }

    /// The first window of an app in a workspace starts its process and reserves
    /// its door; its launch holds the door and hands its end to the
    /// launcher; once the launch is over, a window starts a process again.
    #[test]
    fn a_workspaces_app_keeps_one_door_until_its_launch_is_over() {
        let (doors, _) = doors();
        doors.reserve_for_test(APP, WORKSPACE);
        let mut held = doors
            .hold(APP, WORKSPACE)
            .expect("the launch holds the door");
        assert!(doors.hold(APP, WORKSPACE).is_none(), "held by one launch");
        assert!(
            doors.reserve(APP, WORKSPACE).is_none(),
            "a second launch keeps the running process's door"
        );
        assert!(held.serve(4242).is_some(), "the launcher's end");
        assert!(held.serve(4242).is_none(), "handed over once");
        assert!(has_door(&doors, APP, WORKSPACE));
        drop(held);
        assert!(!has_door(&doors, APP, WORKSPACE), "the launch is over");
    }

    /// A launch that ends after its door was replaced leaves the new one.
    #[test]
    fn an_earlier_launch_never_retires_a_later_door() {
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000002";
        let (doors, _) = doors();
        doors.reserve_for_test(APP, WORKSPACE);
        let earlier = doors.hold(APP, WORKSPACE).expect("held");
        doors.doors().remove(&key(APP, WORKSPACE));
        doors.reserve_for_test(APP, WORKSPACE);
        drop(earlier);
        assert!(
            has_door(&doors, APP, WORKSPACE),
            "the later launch's door stays"
        );
    }

    /// A launch that never started gives its door up.
    #[test]
    fn a_launch_that_never_started_gives_its_door_up() {
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000003";
        let (doors, _) = doors();
        drop(doors.reserve(APP, WORKSPACE).expect("reserved"));
        assert!(!has_door(&doors, APP, WORKSPACE));
    }

    /// One process per app per workspace: the second window of an app running in
    /// a workspace, once the app said it opens windows, is a request on its door,
    /// naming the window's own instance and carrying its own connections; in
    /// a workspace where the app does not run, or once its process is gone, a
    /// window starts a process.
    #[test]
    fn another_window_of_a_running_app_opens_in_its_process() {
        const APP: &str = "so.tilo.window-door-route";
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000004";
        const ELSEWHERE: &str = "7a110d00-0000-4000-8000-000000000005";
        let (doors, _) = doors();
        assert_eq!(
            doors.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
            Routed::NotRunning
        );
        doors.reserve_for_test(APP, WORKSPACE);
        let mut held = doors.hold(APP, WORKSPACE).expect("held");
        let launcher_end = held.serve(4_100_001).expect("the launcher's end");
        runtime_portal::windows::send_serves(&launcher_end).unwrap();

        assert_eq!(
            doors.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
            Routed::Opened
        );
        let opening = ui::window_door::receive(&launcher_end)
            .expect("a request")
            .expect("open");
        let first = opening.request.instance;
        assert!(opening.request.collaboration, "the window's own connection");
        assert_eq!(
            doors.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
            Routed::Opened
        );
        let plain = ui::window_door::receive(&launcher_end)
            .expect("a request")
            .expect("open")
            .request;
        assert_eq!(
            plain.reveal, None,
            "a window asked for on nothing in particular says so"
        );
        assert_ne!(
            first, plain.instance,
            "each window is an instance of its own"
        );

        assert_eq!(
            doors.open_in_running(
                APP,
                WORKSPACE,
                &Opens::revealing(Some("chat/channels/c_ada")),
                &never_alone,
                None,
            ),
            Routed::Opened
        );
        let told = ui::window_door::receive(&launcher_end)
            .expect("a request")
            .expect("open")
            .request;
        assert_eq!(
            told.reveal.as_deref(),
            Some("chat/channels/c_ada"),
            "the window opens on what it was asked for"
        );
        assert_ne!(first, told.instance);
        assert_ne!(plain.instance, told.instance);

        assert_eq!(
            doors.open_in_running(APP, ELSEWHERE, &Opens::default(), &never_alone, None),
            Routed::NotRunning
        );
        drop(launcher_end);
        drop(held);
        assert_eq!(
            doors.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
            Routed::NotRunning,
            "the process is gone"
        );
    }

    /// A window the session starts takes the tab the desk made for it, and that
    /// tab is the instance it is known by, which its request names.
    #[test]
    fn a_window_takes_the_tab_the_desk_made_for_it_as_its_instance() {
        const APP: &str = "so.tilo.window-door-tab";
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-00000000000a";
        let (doors, _) = doors();
        doors.reserve_for_test(APP, WORKSPACE);
        let mut held = doors.hold(APP, WORKSPACE).expect("held");
        let launcher_end = held.serve(4_100_002).expect("the launcher's end");
        runtime_portal::windows::send_serves(&launcher_end).unwrap();

        assert_eq!(
            doors.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, Some(77)),
            Routed::Opened
        );
        let request = ui::window_door::receive(&launcher_end)
            .expect("a request")
            .expect("open")
            .request;
        assert_eq!(
            request.instance, 77,
            "the window is known by the tab it takes"
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
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000006";
        let (doors, _) = doors();
        doors.reserve_for_test(APP, WORKSPACE);
        let mut held = doors.hold(APP, WORKSPACE).expect("held");
        // The process that maps a window is the launcher itself here.
        let _launcher_end = held.serve(std::process::id()).expect("the launcher's end");
        let (started, waiting) = std::sync::mpsc::channel();
        let alone = alone_into(started);

        assert_eq!(
            doors.open_in_running(APP, WORKSPACE, &Opens::default(), &alone, None),
            Routed::Opened,
            "waits for the app to answer"
        );
        doors.window_mapped(std::process::id());
        waiting
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the waiting window starts a process of its own");
        assert_eq!(
            doors.open_in_running(
                APP,
                WORKSPACE,
                &Opens::default(),
                &alone_into(std::sync::mpsc::channel().0),
                None
            ),
            Routed::NotRunning,
            "a later window starts its own process at once"
        );
    }

    /// An app that reads its door says so before it maps its first window: a
    /// window asked for while it started opens in it.
    #[test]
    fn a_window_asked_for_while_its_app_starts_opens_in_it() {
        const APP: &str = "so.tilo.window-door-starting";
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000007";
        let (doors, _) = doors();
        doors.reserve_for_test(APP, WORKSPACE);
        let mut held = doors.hold(APP, WORKSPACE).expect("held");
        let launcher_end = held.serve(std::process::id()).expect("the launcher's end");
        assert_eq!(
            doors.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
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
            .route_or_reserve(APP, WORKSPACE, false, &Opens::default(), &never_alone, None)
            .expect("not routed");
        assert!(reserved.is_none(), "a process of its own, without a door");
        let reserved = doors
            .route_or_reserve(APP, WORKSPACE, true, &Opens::default(), &never_alone, None)
            .expect("reserved");
        assert!(reserved.is_some(), "the first window reserves the door");
        drop(reserved);
        assert!(
            !has_door(&doors, APP, WORKSPACE),
            "a launch that never started"
        );
    }

    /// A tilolang app whose instances a tilolang host runs: each window gets
    /// its patch connection on the door before it is asked for, and no
    /// collaboration connection of its own; its instance goes to the host
    /// with the other end of that connection and the window's collaboration.
    #[test]
    fn a_hosted_windows_instance_goes_to_its_host_with_its_patch_connection() {
        use std::io::{Read as _, Write as _};
        const APP: &str = "so.tilo.window-door-tilolang";
        const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000008";
        let (doors, _) = doors();
        let handed = Rc::new(RefCell::new(Vec::new()));
        let into = Rc::clone(&handed);
        doors.hand_instances_to(Box::new(move |hosted| into.borrow_mut().push(hosted)));
        doors.reserve_for_test(APP, WORKSPACE);
        assert!(doors.host_instances(APP, WORKSPACE));
        let mut held = doors.hold(APP, WORKSPACE).expect("held");
        let launcher_end = held.serve(4_100_008).expect("the launcher's end");
        runtime_portal::windows::send_serves(&launcher_end).unwrap();

        let first_opens = Opens {
            record: Some("uuid:7a110d00-0000-4000-8000-000000000009".to_owned()),
            reveal: None,
        };
        doors
            .first_hosted_window(APP, WORKSPACE, 5, &first_opens)
            .unwrap();
        let Some(runtime_portal::windows::Message::Patch(first, first_patch)) =
            runtime_portal::windows::receive_message(&launcher_end).unwrap()
        else {
            panic!("the first window's patch connection");
        };
        assert_eq!(first, 5);

        assert_eq!(
            doors.open_in_running(
                APP,
                WORKSPACE,
                &Opens::revealing(Some("tl.notes.Note:7")),
                &never_alone,
                Some(6)
            ),
            Routed::Opened
        );
        let Some(runtime_portal::windows::Message::Patch(patched, _)) =
            runtime_portal::windows::receive_message(&launcher_end).unwrap()
        else {
            panic!("the window's patch connection comes first");
        };
        let opening = ui::window_door::receive(&launcher_end)
            .expect("a request")
            .expect("open");
        assert_eq!(
            opening.request.instance, patched,
            "for the window asked for"
        );
        assert_eq!(patched, 6);
        assert!(
            !opening.request.collaboration && opening.collaboration.is_none(),
            "the window holds none of the app's data"
        );

        let handed = handed.borrow();
        let instances: Vec<(u64, u32)> = handed
            .iter()
            .map(|hosted| (hosted.instance, hosted.launcher_pid))
            .collect();
        assert_eq!(instances, [(5, 4_100_008), (6, 4_100_008)]);
        assert_eq!(
            handed[0].opens, first_opens,
            "the first window's instance opens the record its launch named"
        );
        assert_eq!(
            handed[1].opens.reveal.as_deref(),
            Some("tl.notes.Note:7"),
            "a later window's instance opens on the item it was asked for"
        );
        assert_eq!(
            opening.request.reveal.as_deref(),
            Some("tl.notes.Note:7"),
            "the window is told as any window is"
        );
        // The host's end and the window's are one connection.
        let mut host = std::os::unix::net::UnixStream::from(handed[0].patch.try_clone().unwrap());
        host.write_all(b"patch").unwrap();
        let mut window = std::os::unix::net::UnixStream::from(first_patch);
        let mut read = [0_u8; 5];
        window.read_exact(&mut read).unwrap();
        assert_eq!(&read, b"patch");
    }
}
