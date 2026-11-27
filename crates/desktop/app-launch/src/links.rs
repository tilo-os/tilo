//! One process per app per workspace: the session opens an app's later windows in
//! the process it started for the first.
//!
//! When the session starts a Tilo app in a workspace it keeps one end of a window
//! link (`runtime_portal::process_link`) and hands the other to the launch with
//! its release ([`launcher::launch_hold`]). Every later window of that
//! app in that workspace is then a request on the link instead of a second
//! process, which could never hold the app's records: one process owns them.
//! The request names the window's instance and carries the window's own
//! connections to the session ([`Connects`]), so the window it maps claims a
//! tab of its own and its data reaches the app it belongs to.
//!
//! The link is reserved when the first launch is decided, before its process
//! exists, so windows asked for at once (a workspace's tabs restored together) all
//! reach the one process. It is retired when the process ends.
//!
//! The session and the apps are updated apart, so a window goes through the
//! link only once the app has said it reads it
//! (`runtime_portal::process_link::FromProcess::Serves`), which it does before it maps its
//! first window. Until the app has said it or mapped a window, windows asked
//! for wait at the link; an app that maps a window without having said it was
//! built before the link, and each of its windows starts a process of its own,
//! as it always did.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime_portal::process_link::{Binding, Packet as _, ToProcess, WindowRequest};

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

    /// The collaboration connection of a call of `app` in `workspace` that shows nothing:
    /// attested as that call, and handed no view to open.
    fn collaboration_with_no_view(&self, app: &str, workspace: &str, call: u64) -> Option<OwnedFd>;
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

/// What a window is asked to show: the record a launch named, the item the window opens on, and
/// the file or link it opens. A window that holds its app's data opens the record itself, from
/// its launch's command line, and the item and the file from its window request; a hosted
/// window's instance is handed the record, the item and the file it opens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opens {
    /// The record the launch named, by its id.
    pub record: Option<String>,
    /// The item the window opens on, by its anchor: a search hit's, an agent's `open`, the
    /// conversation a send landed in.
    pub reveal: Option<String>,
    /// The file (at the path the app's sandbox holds it at) or link the launch names.
    pub open: Option<String>,
    /// The link the launch names, of a scheme its app claims, for an instance the session hands
    /// to a tilolang host: the window's `open-link` shows what it names. `None` for a window
    /// opened with a file or with nothing.
    pub link: Option<String>,
    /// The file the launch opens, as content the instance reads from the content store: what an
    /// app with no path of its own is handed, the same `File` a chooser gives it.
    pub content: Option<ContentOpen>,
}

/// A file an app is opened with, by the hash of its content and the name it is known by: the
/// same `File` value a chooser gives the app, whose bytes the content store holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentOpen {
    pub hash: String,
    pub name: Option<String>,
}

/// What the session does with a hosted window's instance.
pub type HostsInstances = Box<dyn Fn(HostedWindow)>;

/// A reserved or running process's link, by `(app, workspace)`.
struct Link {
    /// The session's end.
    ours: OwnedFd,
    /// The launch's end, until the launch takes it with its release.
    theirs: Option<OwnedFd>,
    /// The launcher process, once it exists: the ownership a window's
    /// connections are attributed to.
    launcher_pid: Option<u32>,
    /// Which reservation this is, so the end of an earlier launch never
    /// retires a later one's link.
    generation: u64,
    /// What the app behind the link said about its windows.
    serves: Serves,
    /// Windows asked for before the app said whether it opens them.
    queued: Vec<Queued>,
    /// A tilolang app whose instances a tilolang host runs: its process is
    /// its windows alone, each shown over a patch connection the link carries.
    hosted: bool,
}

/// Whether the app behind a link opens the windows the session asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Serves {
    /// It has neither said so nor mapped a window yet.
    Unknown,
    /// It said so.
    Yes,
    /// It mapped a window without saying so: an app built before the link.
    No,
}

/// Starts a window in a process of its own, as a launch without a link does.
pub type StartAlone = Box<dyn FnOnce()>;

/// A window asked for while its app had not yet said whether it opens it.
struct Queued {
    app: String,
    workspace: String,
    /// What the shell asked the window to show.
    opens: Opens,
    /// The window's instance: the tab the shell held for it, or one the link
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

/// The links of the apps this session started, by app and workspace.
pub struct Links {
    connections: Box<dyn Connects>,
    /// Where a hosted window's instance goes.
    hosts: std::cell::RefCell<Option<HostsInstances>>,
    inner: Mutex<HashMap<Key, Link>>,
    generations: AtomicU64,
    instances: AtomicU64,
}

fn key(app: &str, workspace: &str) -> Key {
    (app.to_owned(), workspace.to_owned())
}

/// Whether `link`'s process can take invocations: it has not let go of its link, as a
/// process that stopped taking them does before it goes
/// ([`runtime_portal::process_link::hung_up`]). A link a launch reserved but whose process is
/// still starting can: what is asked of it waits for its first publish.
///
/// The broker asks the same of each of the process's connections before it picks one
/// (`open_stream`): a window that stopped taking invocations ends its stream, and the
/// process lets go of its link once none of its windows takes them.
fn serves_invocations(link: &Link) -> bool {
    !runtime_portal::process_link::hung_up(&link.ours)
}

impl Links {
    /// The links of the apps `connections` makes windows for.
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

    /// The connections this session makes for a window of the apps it starts.
    #[must_use]
    pub fn connections(&self) -> &dyn Connects {
        &*self.connections
    }

    fn links(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Link>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reserve the link of the process about to start for `app` in `workspace`,
    /// unless one is reserved already. Later windows of the app there are
    /// routed to it. The reservation is given up when it is dropped
    /// before [`Reservation::launched`]: the launch did not start.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "one look at the links decides and records a window's process, so two launches never both start one"
    )]
    fn reserve(self: &Rc<Self>, app: &str, workspace: &str) -> Option<Reservation> {
        let mut links = self.links();
        let key = key(app, workspace);
        if links.contains_key(&key) {
            return None;
        }
        let (ours, theirs) = runtime_portal::process_link::pair()
            .map_err(|error| tracing::warn!(%error, app, "no window link for the launch"))
            .ok()?;
        let generation = self.generations.fetch_add(1, Ordering::Relaxed);
        links.insert(
            key.clone(),
            Link {
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
            links: Rc::clone(self),
            key,
            generation,
        })
    }

    /// The link a launch of `app` in `workspace` serves, held for as long as the
    /// launch runs: its end goes to the launcher with the release, and when
    /// the launch is over (its process ended, or it never started) the link
    /// is retired, so the app's next window there starts a process again.
    #[must_use]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the link is looked up and taken in one hold of the links"
    )]
    pub fn hold(self: &Rc<Self>, app: &str, workspace: &str) -> Option<HeldLink> {
        let key = key(app, workspace);
        let mut links = self.links();
        let link = links.get_mut(&key)?;
        let theirs = link.theirs.take()?;
        Some(HeldLink {
            links: Some(Rc::clone(self)),
            key,
            generation: link.generation,
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
        let mut links = self.links();
        let here = key(app, workspace);
        // A process that stopped taking invocations, as it closes, takes no window: the
        // window starts a process of its own, and so do any that waited on it.
        if links
            .get(&here)
            .is_some_and(|link| !serves_invocations(link))
        {
            let gone = links.remove(&here);
            drop(links);
            if let Some(gone) = gone {
                start_alone(gone.queued);
            }
            return Routed::NotRunning;
        }
        let Some(link) = links.get_mut(&here) else {
            return Routed::NotRunning;
        };
        let mut late = Vec::new();
        if link.serves == Serves::Unknown
            && runtime_portal::process_link::take_serves(&link.ours).unwrap_or(false)
        {
            link.serves = Serves::Yes;
            late = std::mem::take(&mut link.queued);
        }
        let routed = match link.serves {
            Serves::No => Routed::NotRunning,
            Serves::Unknown => {
                link.queued.push(Queued {
                    app: app.to_owned(),
                    workspace: workspace.to_owned(),
                    opens: opens.clone(),
                    instance,
                    alone: alone(),
                });
                Routed::Opened
            }
            Serves::Yes => match send_window(self, link, app, workspace, opens, instance) {
                Ok(()) => Routed::Opened,
                Err(error) => {
                    // The process is gone; the next launch starts another.
                    tracing::info!(%error, app, "the app's window link is closed");
                    let gone = links.remove(&here);
                    drop(links);
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
                let link = links.get_mut(&key(&queued.app, &queued.workspace))?;
                send_window(
                    self,
                    link,
                    &queued.app,
                    &queued.workspace,
                    &queued.opens,
                    queued.instance,
                )
                .is_err()
                .then_some(queued)
            })
            .collect();
        drop(links);
        start_alone(failed);
        routed
    }

    /// A window of the process `pid` mapped. If the app behind its link has
    /// not said it opens windows by now it never will: it was built before
    /// the link. The windows that waited go where its answer says.
    pub fn window_mapped(&self, pid: u32) {
        let lineage = ancestry(pid);
        let (open, alone) = {
            let mut links = self.links();
            let Some(link) = links.values_mut().find(|link| {
                link.serves == Serves::Unknown
                    && link
                        .launcher_pid
                        .is_some_and(|launcher| lineage.contains(&launcher))
            }) else {
                return;
            };
            link.serves = if runtime_portal::process_link::take_serves(&link.ours).unwrap_or(false)
            {
                Serves::Yes
            } else {
                Serves::No
            };
            let queued = std::mem::take(&mut link.queued);
            if link.serves == Serves::Yes {
                let failed: Vec<Queued> = queued
                    .into_iter()
                    .filter_map(|queued| {
                        send_window(
                            self,
                            link,
                            &queued.app,
                            &queued.workspace,
                            &queued.opens,
                            queued.instance,
                        )
                        .is_err()
                        .then_some(queued)
                    })
                    .collect();
                drop(links);
                (true, failed)
            } else {
                drop(links);
                (false, queued)
            }
        };
        if !open {
            tracing::info!("an app built before the window link starts a process per window");
        }
        start_alone(alone);
    }

    /// Reserve the link of a process about to start for `app` in `workspace`
    /// with no window: `None` when a process of the app there can take
    /// invocations already ([`serves_invocations`]), running or starting, and
    /// serves the workspace in its place.
    ///
    /// # Errors
    /// No link could be made for the process.
    pub fn reserve_unless_running(
        self: &Rc<Self>,
        app: &str,
        workspace: &str,
    ) -> Result<Option<Reservation>, String> {
        let gone = {
            let mut links = self.links();
            let key = key(app, workspace);
            match links.get(&key) {
                Some(link) if serves_invocations(link) => return Ok(None),
                // The process stopped taking work as it goes: the work starts another.
                Some(_) => links.remove(&key),
                None => None,
            }
        };
        if let Some(gone) = gone {
            start_alone(gone.queued);
        }
        self.reserve(app, workspace)
            .map(Some)
            .ok_or_else(|| format!("no window link for {app}"))
    }

    /// Where a window of `app` in `workspace` opens: in the process already
    /// serving the workspace, or in one this launch starts, whose link it
    /// reserves. Only a `routable` launch, the app's own window, on nothing or
    /// on the file or link `opens` names, can open in a running process.
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

/// A link reserved for a launch that has not started yet.
pub struct Reservation {
    links: Rc<Links>,
    key: Key,
    generation: u64,
}

impl Reservation {
    /// The launch is on its way: it holds the link from its start
    /// ([`Links::hold`]).
    pub fn launched(self) {
        std::mem::forget(self);
    }

    /// Bind the spare window process whose link the session holds `spare` to the reserved app
    /// and workspace, for the window `instance`, showing what `opens` names: the spare's link
    /// takes the reservation's place as the app's process there, a tilolang app's whose instances
    /// a tilolang host runs, and opens the app's later windows there. The window's patch
    /// connection and then its binding (the app's verified `module`, and its `surface`, the file
    /// with its hash) go to the spare; the window's instance goes to its host. The link is held for as
    /// long as the spare runs.
    ///
    /// # Errors
    /// The reservation is gone, or the spare's link is closed: the spare is not bound.
    pub fn bind_spare(
        self,
        spare: Spare,
        instance: u64,
        opens: &Opens,
        module: OwnedFd,
        surface: Option<(OwnedFd, String)>,
    ) -> std::io::Result<HeldLink> {
        let (links, key, generation) = (Rc::clone(&self.links), self.key.clone(), self.generation);
        let hosted = {
            let mut map = links.links();
            let (app, workspace) = (&key.0, &key.1);
            let sent = map
                .get_mut(&key)
                .filter(|link| link.generation == generation)
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotConnected))
                .and_then(|link| {
                    link.ours = spare.link;
                    link.theirs = None;
                    link.launcher_pid = Some(spare.launcher_pid);
                    link.serves = Serves::Yes;
                    link.hosted = true;
                    let hosted = hosted_window(&links, link, app, workspace, instance, opens)?;
                    let (surface, surface_hash) = surface.unzip();
                    ToProcess::Bind {
                        binding: Box::new(Binding {
                            app: app.clone(),
                            workspace: workspace.clone(),
                            surface_hash,
                            window: WindowRequest {
                                instance,
                                reveal: opens.reveal.clone(),
                                open: opens.open.clone(),
                            },
                        }),
                        module,
                        shell_ui: links.connections.shell_ui(
                            app,
                            instance,
                            Some(spare.launcher_pid),
                        ),
                        surface,
                    }
                    .send(&link.ours)?;
                    Ok(hosted)
                });
            match sent {
                Ok(hosted) => {
                    drop(map);
                    hosted
                }
                Err(error) => {
                    // Nothing serves the app there: its next window reserves the link again.
                    if map
                        .get(&key)
                        .is_some_and(|link| link.generation == generation)
                    {
                        map.remove(&key);
                    }
                    drop(map);
                    return Err(error);
                }
            }
        };
        std::mem::forget(self);
        links.hand_over(hosted);
        tracing::info!(app = %key.0, instance, "a spare window opened the app's window");
        Ok(HeldLink {
            links: Some(links),
            key,
            generation,
            theirs: None,
        })
    }
}

/// A spare window process: a window of no app yet, which the session binds to the app whose
/// window is asked for ([`Reservation::bind_spare`]).
pub struct Spare {
    /// The session's end of its link.
    pub link: OwnedFd,
    /// Its launch, which its windows' connections are attributed to.
    pub launcher_pid: u32,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut links = self.links.links();
        if links
            .get(&self.key)
            .is_some_and(|link| link.generation == self.generation && link.theirs.is_some())
        {
            links.remove(&self.key);
        }
    }
}

/// The link a launch serves, held for as long as the launch runs.
pub struct HeldLink {
    /// The links it is one of; `None` for a spare window's, which serves no app until it is
    /// bound.
    links: Option<Rc<Links>>,
    key: Key,
    generation: u64,
    theirs: Option<OwnedFd>,
}

impl HeldLink {
    /// The link of a launch no app's windows are routed to yet, whose end `theirs` the launch is
    /// released with: a spare window process's.
    #[must_use]
    pub fn unrouted(theirs: OwnedFd) -> Self {
        Self {
            links: None,
            key: Key::default(),
            generation: 0,
            theirs: Some(theirs),
        }
    }

    /// The launcher `launcher_pid` serves the link: hand it its end, which it
    /// takes with its release.
    pub fn serve(&mut self, launcher_pid: u32) -> Option<OwnedFd> {
        if let Some(links) = &self.links
            && let Some(link) = links
                .links()
                .get_mut(&self.key)
                .filter(|link| link.generation == self.generation)
        {
            link.launcher_pid = Some(launcher_pid);
        }
        self.theirs.take()
    }
}

impl Drop for HeldLink {
    fn drop(&mut self) {
        let Some(links) = &self.links else {
            return;
        };
        let gone = {
            let mut links = links.links();
            if links
                .get(&self.key)
                .is_some_and(|link| link.generation == self.generation)
            {
                links.remove(&self.key)
            } else {
                None
            }
        };
        // Windows that waited for a process that never said it opens them
        // start their own.
        if let Some(link) = gone {
            start_alone(link.queued);
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
/// process is behind `entry`: its patch connection is sent on the link, before
/// the window is asked for, and the host's end goes with the instance, with
/// what the window `opens`.
fn hosted_window(
    link: &Links,
    entry: &Link,
    app: &str,
    workspace: &str,
    instance: u64,
    opens: &Opens,
) -> std::io::Result<HostedWindow> {
    let launcher_pid = entry.launcher_pid.ok_or(std::io::ErrorKind::NotConnected)?;
    let collaboration = link
        .connections
        .collaboration(app, workspace, instance, Some(launcher_pid))
        .ok_or_else(|| std::io::Error::other("the instance has no collaboration connection"))?;
    let (window, host) = runtime_portal::process_link::pair()?;
    ToProcess::Patch {
        instance,
        patch: window,
    }
    .send(&entry.ours)?;
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

/// Ask the process behind `link` for a window of `app` in `workspace`.
fn send_window(
    link: &Links,
    entry: &Link,
    app: &str,
    workspace: &str,
    opens: &Opens,
    instance: Option<u64>,
) -> std::io::Result<()> {
    // The shell names the tab a window takes, which is the window's instance;
    // a window asked for with no tab of its own gets one from the link.
    let instance = instance.unwrap_or_else(|| link.next_instance());
    // A tilolang host runs the window's instance: the window gets its patch
    // connection, and the instance the window's collaboration connection.
    let hosted = entry
        .hosted
        .then(|| hosted_window(link, entry, app, workspace, instance, opens))
        .transpose()?;
    let collaboration = if hosted.is_some() {
        None
    } else {
        link.connections
            .collaboration(app, workspace, instance, entry.launcher_pid)
    };
    let shell_ui = link.connections.shell_ui(app, instance, entry.launcher_pid);
    ToProcess::Window {
        request: WindowRequest {
            instance,
            reveal: opens.reveal.clone(),
            open: opens.open.clone(),
        },
        collaboration,
        shell_ui,
    }
    .send(&entry.ours)?;
    tracing::info!(app, instance, "window opened in the running app");
    if let Some(hosted) = hosted {
        link.hand_over(hosted);
    }
    Ok(())
}

/// The process `pid` and every one that started it, up to init: a window's process is the one
/// the launcher started or any process below it, however deep a program nests what it starts
/// (a shell, a runtime, a sandbox's own init).
#[must_use]
pub fn ancestry(pid: u32) -> Vec<u32> {
    let mut lineage = vec![pid];
    let mut current = pid;
    while let Some(parent) = parent_of(current) {
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

    /// A window request as the app reads it off its link.
    struct Opening {
        request: WindowRequest,
        collaboration: Option<OwnedFd>,
    }

    /// The next window request on `end`; `None` once the session closed it.
    fn receive_window(end: &OwnedFd) -> Option<Opening> {
        match ToProcess::receive(end).expect("a message") {
            Some(ToProcess::Window {
                request,
                collaboration,
                ..
            }) => Some(Opening {
                request,
                collaboration,
            }),
            Some(other) => panic!("a window request: {other:?}"),
            None => None,
        }
    }

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

        fn collaboration_with_no_view(
            &self,
            app: &str,
            _workspace: &str,
            _call: u64,
        ) -> Option<OwnedFd> {
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a socket");
            self.made.borrow_mut().push((app.to_owned(), theirs.into()));
            Some(ours.into())
        }
    }

    fn links() -> (Rc<Links>, Rc<TestConnections>) {
        let connections = Rc::new(TestConnections::default());
        (Links::new(Rc::clone(&connections)), connections)
    }

    const APP: &str = "so.tilo.window-link-test";
    const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000001";

    fn has_channel(links: &Links, app: &str, workspace: &str) -> bool {
        links.links().contains_key(&key(app, workspace))
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

    /// One `StartAlone` per window asked for, counting the ones that ran.
    fn alone_counting(ran: Rc<std::cell::Cell<usize>>) -> impl Fn() -> StartAlone {
        move || {
            let ran = Rc::clone(&ran);
            Box::new(move || ran.set(ran.get().saturating_add(1)))
        }
    }

    impl Links {
        fn reserve_for_test(self: &Rc<Self>, app: &str, workspace: &str) {
            self.reserve(app, workspace).expect("reserved").launched();
        }
    }

    tilo_test_env::scenario! {
        fn a_channel_is_reserved_held_and_given_up_with_its_launch;

        phase a_workspaces_app_keeps_one_channel_until_its_launch_is_over;
        // The first window of an app in a workspace starts its process and reserves
        // its link; its launch holds the link and hands its end to the
        // launcher; once the launch is over, a window starts a process again.
        {
            let (links, _) = links();
            links.reserve_for_test(APP, WORKSPACE);
            let mut held = links
                .hold(APP, WORKSPACE)
                .expect("the launch holds the link");
            assert!(links.hold(APP, WORKSPACE).is_none(), "held by one launch");
            assert!(
                links.reserve(APP, WORKSPACE).is_none(),
                "a second launch keeps the running process's link"
            );
            assert!(held.serve(4242).is_some(), "the launcher's end");
            assert!(held.serve(4242).is_none(), "handed over once");
            assert!(has_channel(&links, APP, WORKSPACE));
            drop(held);
            assert!(!has_channel(&links, APP, WORKSPACE), "the launch is over");
        }

        phase an_earlier_launch_never_retires_a_later_channel;
        // A launch that ends after its link was replaced leaves the new one.
        {
            const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000002";
            let (links, _) = links();
            links.reserve_for_test(APP, WORKSPACE);
            let earlier = links.hold(APP, WORKSPACE).expect("held");
            links.links().remove(&key(APP, WORKSPACE));
            links.reserve_for_test(APP, WORKSPACE);
            drop(earlier);
            assert!(
                has_channel(&links, APP, WORKSPACE),
                "the later launch's link stays"
            );
        }

        phase a_launch_that_never_started_gives_its_channel_up;
        // A launch that never started gives its link up.
        {
            const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000003";
            let (links, _) = links();
            drop(links.reserve(APP, WORKSPACE).expect("reserved"));
            assert!(!has_channel(&links, APP, WORKSPACE));
        }

        phase a_process_that_stopped_taking_work_leaves_the_next_to_another;
        // An app's process going away with its last window closes its end of the link first:
        // what is asked of the app from then on starts a process of its own, rather than
        // waiting on one that is about to exit.
        {
            const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000004";
            let (links, _) = links();
            links.reserve_for_test(APP, WORKSPACE);
            let mut held = links.hold(APP, WORKSPACE).expect("held");
            let theirs = held.serve(4242).expect("the launcher's end");
            assert!(
                links
                    .reserve_unless_running(APP, WORKSPACE)
                    .expect("asked")
                    .is_none(),
                "the running process serves the workspace"
            );
            runtime_portal::process_link::close(&theirs).expect("closed");
            let reserved = links
                .reserve_unless_running(APP, WORKSPACE)
                .expect("asked")
                .expect("a process of its own");
            reserved.launched();
            drop(held);
            assert!(
                links.links().contains_key(&key(APP, WORKSPACE)),
                "the going process leaves the new one's link"
            );
        }

        phase a_launch_that_names_what_it_opens_reserves_a_channel_and_starts;
        // A launch the link does not carry, such as a record, never opens in a
        // running process: the app's own command line carries it.
        {
            let (links, _) = links();
            let reserved = links
                .route_or_reserve(APP, WORKSPACE, false, &Opens::default(), &never_alone, None)
                .expect("not routed");
            assert!(reserved.is_none(), "a process of its own, without a link");
            let reserved = links
                .route_or_reserve(APP, WORKSPACE, true, &Opens::default(), &never_alone, None)
                .expect("reserved");
            assert!(reserved.is_some(), "the first window reserves the link");
            drop(reserved);
            assert!(
                !has_channel(&links, APP, WORKSPACE),
                "a launch that never started"
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_window_of_a_running_app_opens_through_its_channel;

        phase another_window_of_a_running_app_opens_in_its_process;
        // One process per app per workspace: the second window of an app running in
        // a workspace, once the app said it opens windows, is a request on its link,
        // naming the window's own instance and carrying its own connections; in
        // a workspace where the app does not run, or once its process is gone, a
        // window starts a process.
        {
            const APP: &str = "so.tilo.window-link-route";
            const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000004";
            const ELSEWHERE: &str = "7a110d00-0000-4000-8000-000000000005";
            let (links, _) = links();
            assert_eq!(
                links.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
                Routed::NotRunning
            );
            links.reserve_for_test(APP, WORKSPACE);
            let mut held = links.hold(APP, WORKSPACE).expect("held");
            let launcher_end = held.serve(4_100_001).expect("the launcher's end");
            runtime_portal::process_link::FromProcess::Serves.send(&launcher_end).unwrap();

            assert_eq!(
                links.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
                Routed::Opened
            );
            let opening = receive_window(&launcher_end).expect("open");
            let first = opening.request.instance;
            assert!(opening.collaboration.is_some(), "the window's own connection");
            assert_eq!(
                links.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
                Routed::Opened
            );
            let plain = receive_window(&launcher_end).expect("open")
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
                links.open_in_running(
                    APP,
                    WORKSPACE,
                    &Opens {
                        reveal: Some("chat/links/c_ada".to_owned()),
                        ..Opens::default()
                    },
                    &never_alone,
                    None,
                ),
                Routed::Opened
            );
            let told = receive_window(&launcher_end).expect("open")
                .request;
            assert_eq!(
                told.reveal.as_deref(),
                Some("chat/links/c_ada"),
                "the window opens on what it was asked for"
            );
            assert_ne!(first, told.instance);
            assert_ne!(plain.instance, told.instance);

            // A second file opened with the app reaches the process that runs it, at the path
            // the app's sandbox holds it at: it opens in a window of its own there.
            let budget = Opens {
                open: Some("/documents/Budget.txt".to_owned()),
                ..Opens::default()
            };
            assert_eq!(
                links.open_in_running(APP, WORKSPACE, &budget, &never_alone, None),
                Routed::Opened
            );
            let opened = receive_window(&launcher_end).expect("open")
                .request;
            assert_eq!(
                opened.open.as_deref(),
                Some("/documents/Budget.txt"),
                "the running app is handed the file"
            );
            assert!(
                ![first, plain.instance, told.instance].contains(&opened.instance),
                "the file opens in a window of its own"
            );

            assert_eq!(
                links.open_in_running(APP, ELSEWHERE, &Opens::default(), &never_alone, None),
                Routed::NotRunning
            );
            drop(launcher_end);
            drop(held);
            assert_eq!(
                links.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
                Routed::NotRunning,
                "the process is gone"
            );
        }

        phase a_window_takes_the_tab_the_shell_made_for_it_as_its_instance;
        // A window the session starts takes the tab the shell made for it, and that
        // tab is the instance it is known by, which its request names.
        {
            const APP: &str = "so.tilo.window-link-tab";
            const WORKSPACE: &str = "7a110d00-0000-4000-8000-00000000000a";
            let (links, _) = links();
            links.reserve_for_test(APP, WORKSPACE);
            let mut held = links.hold(APP, WORKSPACE).expect("held");
            let launcher_end = held.serve(4_100_002).expect("the launcher's end");
            runtime_portal::process_link::FromProcess::Serves.send(&launcher_end).unwrap();

            assert_eq!(
                links.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, Some(77)),
                Routed::Opened
            );
            let request = receive_window(&launcher_end).expect("open")
                .request;
            assert_eq!(
                request.instance, 77,
                "the window is known by the tab it takes"
            );
        }

        phase an_app_built_before_the_window_link_keeps_a_process_per_window;
        // The session and the apps are updated apart. An app built before the
        // window link never says it opens windows: a window asked for while it
        // starts waits until it maps its own, then starts a process of its own,
        // as every window of such an app always did, and so does every later
        // one.
        {
            const APP: &str = "so.tilo.window-link-older-app";
            const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000006";
            let (links, _) = links();
            links.reserve_for_test(APP, WORKSPACE);
            let mut held = links.hold(APP, WORKSPACE).expect("held");
            // The process that maps a window is the launcher itself here.
            let _launcher_end = held.serve(std::process::id()).expect("the launcher's end");
            let (started, waiting) = std::sync::mpsc::channel();
            let alone = alone_into(started);

            assert_eq!(
                links.open_in_running(APP, WORKSPACE, &Opens::default(), &alone, None),
                Routed::Opened,
                "waits for the app to answer"
            );
            links.window_mapped(std::process::id());
            waiting
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("the waiting window starts a process of its own");
            assert_eq!(
                links.open_in_running(
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

        phase a_window_asked_for_while_its_app_starts_opens_in_it;
        // An app that reads its link says so before it maps its first window: a
        // window asked for while it started opens in it.
        {
            const APP: &str = "so.tilo.window-link-starting";
            const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000007";
            let (links, _) = links();
            links.reserve_for_test(APP, WORKSPACE);
            let mut held = links.hold(APP, WORKSPACE).expect("held");
            let launcher_end = held.serve(std::process::id()).expect("the launcher's end");
            assert_eq!(
                links.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
                Routed::Opened
            );
            runtime_portal::process_link::FromProcess::Serves.send(&launcher_end).unwrap();
            links.window_mapped(std::process::id());
            assert!(
                receive_window(&launcher_end).is_some(),
                "the window that waited opens in the app"
            );
        }

        phase a_window_asked_of_a_process_that_let_go_of_its_link_starts_one;
        // A window the shell asked for while the app started waited at its link for the app
        // to say whether it opens windows. The process let go of the link before it said so:
        // it is closing, and it takes no windows. The window starts a process of its own,
        // rather than waiting on one that will never read it.
        {
            const APP: &str = "so.tilo.window-link-closing";
            const WORKSPACE: &str = "7a110d00-0000-4000-8000-00000000000b";
            let (links, _) = links();
            links.reserve_for_test(APP, WORKSPACE);
            let mut held = links.hold(APP, WORKSPACE).expect("held");
            let launcher_end = held.serve(4_100_003).expect("the launcher's end");
            let ran = Rc::new(std::cell::Cell::new(0));
            let alone = alone_counting(Rc::clone(&ran));
            assert_eq!(
                links.open_in_running(APP, WORKSPACE, &Opens::default(), &alone, None),
                Routed::Opened,
                "the window waits for the app to say whether it opens windows"
            );
            runtime_portal::process_link::close(&launcher_end).expect("let go");
            assert_eq!(
                links.open_in_running(APP, WORKSPACE, &Opens::default(), &never_alone, None),
                Routed::NotRunning,
                "the process that let go of its link takes no window"
            );
            assert_eq!(
                ran.get(),
                1,
                "the window that waited starts a process of its own"
            );
        }

        phase a_hosted_windows_instance_goes_to_its_host_with_its_patch_connection;
        // A tilolang app whose instances a tilolang host runs, in a spare window process
        // bound to it: each window gets its patch connection on the link before it is asked
        // for, and no collaboration connection of its own; its instance goes to the host with
        // the other end of that connection and the window's collaboration.
        {
            use std::io::{Read as _, Write as _};
            const APP: &str = "so.tilo.window-link-tilolang";
            const WORKSPACE: &str = "7a110d00-0000-4000-8000-000000000008";
            let (links, _) = links();
            let handed = Rc::new(RefCell::new(Vec::new()));
            let into = Rc::clone(&handed);
            links.hand_instances_to(Box::new(move |hosted| into.borrow_mut().push(hosted)));
            let reservation = links.reserve(APP, WORKSPACE).expect("reserved");
            let (session_end, launcher_end) = runtime_portal::process_link::pair().unwrap();

            let first_opens = Opens {
                record: Some("uuid:7a110d00-0000-4000-8000-000000000009".to_owned()),
                ..Opens::default()
            };
            let _held = reservation
                .bind_spare(
                    Spare {
                        link: session_end,
                        launcher_pid: 4_100_008,
                    },
                    5,
                    &first_opens,
                    std::fs::File::open("/dev/null").unwrap().into(),
                    None,
                )
                .unwrap();
            let Some(ToProcess::Patch {
                instance: first,
                patch: first_patch,
            }) = ToProcess::receive(&launcher_end).unwrap()
            else {
                panic!("the first window's patch connection");
            };
            let Some(ToProcess::Bind { binding, .. }) = ToProcess::receive(&launcher_end).unwrap()
            else {
                panic!("the spare is bound after its window's patch connection");
            };
            assert_eq!(
                (binding.app.as_str(), binding.workspace.as_str(), binding.window.instance),
                (APP, WORKSPACE, 5)
            );
            assert_eq!(first, 5);

            assert_eq!(
                links.open_in_running(
                    APP,
                    WORKSPACE,
                    &Opens {
                        reveal: Some("tl.notes.Note:7".to_owned()),
                        ..Opens::default()
                    },
                    &never_alone,
                    Some(6)
                ),
                Routed::Opened
            );
            let Some(ToProcess::Patch {
                instance: patched, ..
            }) = ToProcess::receive(&launcher_end).unwrap()
            else {
                panic!("the window's patch connection comes first");
            };
            let opening = receive_window(&launcher_end).expect("open");
            assert_eq!(
                opening.request.instance, patched,
                "for the window asked for"
            );
            assert_eq!(patched, 6);
            assert!(
                opening.collaboration.is_none(),
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
}
