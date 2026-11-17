//! Every window of an app in a workspace, served by one process. The shell starts an app
//! once per workspace. Every window then reads and writes the app's records through the
//! one store the process holds; a second process could never take that store's writer
//! lock.

use serde::{Deserialize, Serialize};

/// The shell's request to open one more window of the app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRequest {
    pub instance: u64,
    #[serde(default)]
    pub collaboration: bool,
    #[serde(default)]
    pub shell_ui: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
}

#[cfg(feature = "local-daemons")]
pub use transport::*;

/// Say on the window link the launcher handed this process that it opens the windows the shell
/// asks for, before the launch's first window is made: the shell takes a mapped window as
/// proof the app cannot, so an app that answers only once its loop runs keeps a process
/// per window.
pub fn announce() {
    #[cfg(feature = "local-daemons")]
    {
        transport::announce();
    }
}

/// Build the window the shell asked for (`pending`), as the process's first window was
/// built from its launch (`spec`, `args`), but with the window's own instance and
/// connections, opening what the request names (a file opened with the running app) and
/// nothing its launch named: that was the first window's.
#[cfg(feature = "native")]
pub(crate) fn open_window(
    spec: &crate::launch::LaunchSpec,
    args: &crate::launch::LaunchArgs,
    pending: PendingWindow,
) -> Option<crate::app::App> {
    let mut opening = pending.take()?;
    if let Some(arriving) = opening.arriving.take() {
        return building(opening, || {
            crate::launch::dispatch_window_arrived(spec, args, arriving)
        })
        .map_err(|error| tracing::error!(%error, "the window the shell asked for could not open"))
        .ok()
        .flatten();
    }
    let reopen = pending.reopen.clone();
    building(opening, || {
        match crate::launch::dispatch_window(spec, args) {
            Ok(crate::launch::WindowBuild::Built(app)) => app,
            // The window's record is not on this device yet: its join, which brings it,
            // is waited for off this thread, so the process's other windows go on
            // meanwhile. The window is built when the join is over, handed back to this
            // thread as it first arrived.
            Ok(crate::launch::WindowBuild::Arriving(arriving)) => {
                let (Some(mut opening), Some(reopen)) = (keep_building(), reopen) else {
                    tracing::error!("the window the shell asked for cannot wait for its workspace");
                    return None;
                };
                let back = PendingWindow {
                    slot: std::sync::Arc::new(std::sync::Mutex::new(None)),
                    reopen: Some(std::sync::Arc::clone(&reopen)),
                };
                // Told on the join's thread, or at once when it is already over; either
                // way the window comes back to this thread only after it is in `back`,
                // below.
                arriving.when_done({
                    let back = back.clone();
                    move || {
                        if !reopen(back) {
                            tracing::warn!("the window's loop ended before its join did");
                        }
                    }
                });
                opening.arriving = Some(arriving);
                back.put(opening);
                None
            }
            Err(error) => {
                tracing::error!(%error, "the window the shell asked for could not open");
                None
            }
        }
    })
}

/// The instance of the window being built for the shell's request, while one is; `None`
/// while the process's first window is built, and where there is no window link.
#[must_use]
pub fn requested_instance() -> Option<u64> {
    #[cfg(feature = "local-daemons")]
    {
        transport::instance()
    }
    #[cfg(not(feature = "local-daemons"))]
    {
        None
    }
}

/// The variable a launch names the item its window opens on in, for a process the shell
/// starts for that window rather than one it asks for it: a window link request cannot
/// reach a window that is already being built.
pub const REVEAL_ENV: &str = crate::host::REVEAL_ENV;

/// The variable the session sets on a launch it makes with no window: the app answers its
/// actions and serves no window link, and its first window is the first one the shell asks for here.
pub const WINDOWLESS_ENV: &str = crate::host::WINDOWLESS_ENV;

/// The item the window being built opens on: what the shell's request named for a window
/// of a process already running, or, for the process's first window, the item its launch
/// named it.
#[must_use]
pub fn requested_reveal() -> Option<String> {
    #[cfg(feature = "local-daemons")]
    {
        if transport::is_building() {
            return transport::requested_reveal();
        }
        let environment = crate::host::environment();
        environment
            .reveal
            .as_deref()
            .map(str::trim)
            .map(str::to_owned)
            .filter(|value| !value.is_empty())
    }
    #[cfg(not(feature = "local-daemons"))]
    {
        None
    }
}

#[cfg(feature = "local-daemons")]
mod transport {
    use super::WindowRequest;
    use runtime_portal::windows::Descriptor;
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};

    /// A request as it arrived, with the connections it carries.
    #[derive(Debug)]
    pub struct WindowOpening {
        pub request: WindowRequest,
        pub collaboration: Option<Descriptor>,
        pub shell_ui: Option<Descriptor>,
        /// The window's record arrives with its workspace's join, waited for off the
        /// window's thread; the window is built once it is over.
        #[cfg(feature = "native")]
        pub(crate) arriving: Option<crate::launch::Arriving>,
    }

    /// Hand a [`PendingWindow`] to the loop that builds windows; `false` once that loop
    /// is gone.
    pub(crate) type Reopen = Arc<dyn Fn(PendingWindow) -> bool + Send + Sync>;

    /// One [`WindowOpening`] carried through the event loop, taken once by the runner
    /// that builds its window.
    #[derive(Clone)]
    pub struct PendingWindow {
        pub(crate) slot: Arc<Mutex<Option<WindowOpening>>>,
        pub(crate) reopen: Option<Reopen>,
    }

    impl std::fmt::Debug for PendingWindow {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("PendingWindow")
                .field("slot", &self.slot)
                .finish_non_exhaustive()
        }
    }

    impl PendingWindow {
        #[must_use]
        pub fn new(opening: WindowOpening) -> Self {
            Self {
                slot: Arc::new(Mutex::new(Some(opening))),
                reopen: None,
            }
        }

        #[must_use]
        pub fn take(&self) -> Option<WindowOpening> {
            self.slot.lock().map_or_else(
                |poisoned| poisoned.into_inner().take(),
                |mut slot| slot.take(),
            )
        }

        /// Carry `opening` again.
        pub(crate) fn put(&self, opening: WindowOpening) {
            match self.slot.lock() {
                Ok(mut slot) => *slot = Some(opening),
                Err(poisoned) => *poisoned.into_inner() = Some(opening),
            }
        }
    }

    /// Write `request` on the shell's end of `link`, with the connections it says it
    /// carries.
    pub fn send(
        link: &Descriptor,
        request: &WindowRequest,
        collaboration: Option<&Descriptor>,
        shell_ui: Option<&Descriptor>,
    ) -> std::io::Result<()> {
        if request.collaboration != collaboration.is_some()
            || request.shell_ui != shell_ui.is_some()
        {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let encoded = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        let fds: Vec<&Descriptor> = collaboration.into_iter().chain(shell_ui).collect();
        runtime_portal::windows::send_open(link, &encoded, &fds)
    }

    /// The next request on the app's end of `link`; `None` once the shell closed it.
    pub fn receive(link: &Descriptor) -> std::io::Result<Option<WindowOpening>> {
        let Some((encoded, fds)) = runtime_portal::windows::receive_open(link)? else {
            return Ok(None);
        };
        opening(&encoded, fds).map(Some)
    }

    fn opening(encoded: &[u8], fds: Vec<Descriptor>) -> std::io::Result<WindowOpening> {
        let request: WindowRequest = serde_json::from_slice(encoded)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let expected = usize::from(request.collaboration) + usize::from(request.shell_ui);
        if fds.len() != expected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a window request does not carry the connections it names",
            ));
        }
        let mut fds = fds.into_iter();
        let collaboration = if request.collaboration {
            fds.next()
        } else {
            None
        };
        let shell_ui = if request.shell_ui { fds.next() } else { None };
        Ok(WindowOpening {
            request,
            collaboration,
            shell_ui,
            #[cfg(feature = "native")]
            arriving: None,
        })
    }

    #[derive(Default)]
    struct Patches {
        arrived: std::collections::HashMap<u64, Descriptor>,
        waiting: std::collections::HashMap<u64, crate::WakeHandle>,
    }

    static PATCHES: Mutex<Option<Patches>> = Mutex::new(None);

    fn patches<T>(with: impl FnOnce(&mut Patches) -> T) -> T {
        let mut patches = PATCHES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        with(patches.get_or_insert_with(Patches::default))
    }

    /// The patch connection of the window `instance`, once the shell sent it; until then,
    /// `wake` is woken when it arrives.
    #[must_use]
    pub fn take_patch(instance: u64, wake: crate::WakeHandle) -> Option<Descriptor> {
        patches(|patches| {
            let patch = patches.arrived.remove(&instance);
            if patch.is_none() {
                patches.waiting.insert(instance, wake);
            } else {
                patches.waiting.remove(&instance);
            }
            patch
        })
    }

    fn patch_arrived(instance: u64, patch: Descriptor) {
        let waiting = patches(|patches| {
            patches.arrived.insert(instance, patch);
            patches.waiting.remove(&instance)
        });
        if let Some(wake) = waiting {
            wake.wake();
        }
    }

    /// The window link the launcher handed this process, taken once: saying so tells the
    /// shell this app serves windows, and it routes the app's later windows to a process that
    /// said so before it mapped one.
    #[must_use]
    pub fn take_link() -> Option<Descriptor> {
        link().take()
    }

    static LINK: Mutex<Option<Descriptor>> = Mutex::new(None);

    static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// The link, taken once and announced once: whoever wants it first ([`announce`]
    /// before the launch's first window, [`take_link`] to serve it) takes it and tells
    /// the shell so, and later callers get it until it is handed out.
    fn link() -> std::sync::MutexGuard<'static, Option<Descriptor>> {
        let mut slot = LINK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !TAKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
            *slot = runtime_portal::windows::take_link();
            if let Some(link) = slot.as_ref()
                && let Err(error) = runtime_portal::windows::send_serves(link)
            {
                tracing::warn!(%error, "the shell was not told this app opens its windows");
            }
        }
        slot
    }

    /// Take the link and tell the shell about it, keeping it for serving.
    pub fn announce() {
        drop(link());
    }

    /// Read `link` on a thread of its own, handing each request to `open`, until the
    /// shell closes it or `open` refuses (its loop is gone). The thread could not be
    /// started.
    pub fn serve(
        link: Descriptor,
        open: impl Fn(PendingWindow) -> bool + Send + 'static,
    ) -> std::io::Result<()> {
        *served() = link.try_clone().ok();
        let open = Arc::new(Mutex::new(open));
        let reopen: Reopen = Arc::new(move |pending| match open.lock() {
            Ok(open) => open(pending),
            Err(poisoned) => (poisoned.into_inner())(pending),
        });
        tilo_rt::spawn_thread("tilo-window-link", move || {
            loop {
                let received =
                    runtime_portal::windows::receive_message(&link).and_then(|message| {
                        Ok(match message {
                            Some(runtime_portal::windows::Message::Open(encoded, fds)) => {
                                Some(Some(opening(&encoded, fds)?))
                            }
                            Some(runtime_portal::windows::Message::Patch(instance, patch)) => {
                                patch_arrived(instance, patch);
                                Some(None)
                            }
                            None => None,
                        })
                    });
                match received {
                    Ok(Some(Some(opening))) => {
                        let mut pending = PendingWindow::new(opening);
                        pending.reopen = Some(Arc::clone(&reopen));
                        if !reopen(pending) {
                            return;
                        }
                    }
                    Ok(Some(None)) => {}
                    Ok(None) => return,
                    Err(error) if error.kind() == std::io::ErrorKind::Unsupported => return,
                    Err(error) => {
                        tracing::warn!(%error, "the shell's window request was refused");
                    }
                }
            }
        })
        .map(drop)
    }

    /// The API being served, kept to close it when the loop that builds windows ends.
    static SERVED: Mutex<Option<Descriptor>> = Mutex::new(None);

    fn served() -> std::sync::MutexGuard<'static, Option<Descriptor>> {
        SERVED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Take no more windows: the loop that builds them is ending, and what the process does
    /// before it exits (saving, letting its renderer go) can take seconds. A window the shell
    /// asks for from now on is refused at once, so the shell starts it in a process of its own,
    /// rather than handing it to this one, which would drop it.
    pub fn stop_serving() {
        if let Some(link) = served().take()
            && let Err(error) = runtime_portal::windows::close_link(&link)
        {
            tracing::warn!(%error, "the window API stayed open as the loop ended");
        }
    }

    thread_local! {
        static BUILDING: RefCell<Option<WindowOpening>> = const { RefCell::new(None) };
    }

    /// Build the window `opening` asks for with `build`: while it runs, the window's
    /// instance and connections stand in for the process's own.
    pub fn building<T>(opening: WindowOpening, build: impl FnOnce() -> T) -> T {
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                BUILDING.with(|slot| slot.borrow_mut().take());
            }
        }
        BUILDING.with(|slot| *slot.borrow_mut() = Some(opening));
        let _done = Done;
        build()
    }

    /// The item the window being built for a request opened on, if one is.
    #[must_use]
    pub fn requested_reveal() -> Option<String> {
        BUILDING.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|opening| opening.request.reveal.clone())
        })
    }

    /// What the window being built for a request opens, if it names one; `None` for the
    /// process's first window, which opens what its launch named.
    #[must_use]
    pub fn requested_open() -> Option<String> {
        BUILDING.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|opening| opening.request.open.clone())
        })
    }

    /// The instance of the window being built for a request, if one is.
    #[must_use]
    pub fn instance() -> Option<u64> {
        BUILDING.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|opening| opening.request.instance)
        })
    }

    /// Whether a window is being built for a request: its connections are its own, never
    /// the ones the launcher handed the process.
    #[must_use]
    pub fn is_building() -> bool {
        BUILDING.with(|slot| slot.borrow().is_some())
    }

    /// The collaboration connection of the window being built, once.
    #[must_use]
    pub fn take_collaboration() -> Option<Descriptor> {
        BUILDING.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .and_then(|opening| opening.collaboration.take())
        })
    }

    /// Take back the opening of the window being built, with what it has not given yet
    /// (its shell-ui connection), to build it later: its record arrives with its join.
    #[cfg(feature = "native")]
    pub(crate) fn keep_building() -> Option<WindowOpening> {
        BUILDING.with(|slot| slot.borrow_mut().take())
    }

    /// The shell-ui connection of the window being built, once.
    #[must_use]
    pub fn take_shell_ui() -> Option<Descriptor> {
        BUILDING.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .and_then(|opening| opening.shell_ui.take())
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn link_pair() -> Option<(Descriptor, Descriptor)> {
            runtime_portal::windows::pair().ok()
        }

        fn connection() -> Option<Descriptor> {
            link_pair().map(|(one, _)| one)
        }

        tilo_test_env::scenario! {
            /// A window request sent over the link and received at the app's end.
            /// Each phase was a test of its own and builds its own fixture.
            fn a_window_request_carries_exactly_the_connections_it_names;

            phase a_window_request_arrives_with_its_own_connections;
            {
                let Some((shell, app)) = link_pair() else { return };
                let (Some(collaboration), Some(shell_ui)) = (connection(), connection()) else {
                    return;
                };
                let request = WindowRequest {
                    instance: 41,
                    collaboration: true,
                    shell_ui: true,
                    reveal: Some("chat/links/c_ada".into()),
                    open: None,
                };
                send(&shell, &request, Some(&collaboration), Some(&shell_ui)).unwrap();
                let opening = receive(&app).unwrap().unwrap();
                assert_eq!(opening.request, request);
                assert!(opening.collaboration.is_some() && opening.shell_ui.is_some());

                let bare = WindowRequest {
                    instance: 42,
                    collaboration: false,
                    shell_ui: false,
                    reveal: None,
                    open: Some("/documents/Budget.txt".into()),
                };
                send(&shell, &bare, None, None).unwrap();
                let opening = receive(&app).unwrap().unwrap();
                assert_eq!(opening.request, bare, "the file it opens comes with it");
                assert!(opening.collaboration.is_none() && opening.shell_ui.is_none());
                drop(shell);
                assert!(receive(&app).unwrap().is_none());
            }

            phase a_request_that_does_not_carry_what_it_names_is_refused;
            {
                let Some((shell, app)) = link_pair() else { return };
                let request = WindowRequest {
                    instance: 1,
                    collaboration: true,
                    shell_ui: false,
                    reveal: None,
                    open: None,
                };
                assert!(send(&shell, &request, None, None).is_err());
                runtime_portal::windows::send_open(&shell, &serde_json::to_vec(&request).unwrap(), &[])
                    .unwrap();
                assert!(receive(&app).is_err());
            }
        }

        /// While a window is built for a request its instance and connections stand in
        /// for the process's, once each, and only while it is built.
        #[test]
        fn a_window_is_built_with_its_own_instance_and_connections() {
            let opening = WindowOpening {
                request: WindowRequest {
                    instance: 9,
                    collaboration: true,
                    shell_ui: false,
                    reveal: Some("chat/links/c_ada".into()),
                    open: Some("/documents/Budget.txt".into()),
                },
                collaboration: connection(),
                shell_ui: None,
                #[cfg(feature = "native")]
                arriving: None,
            };
            let carried = opening.collaboration.is_some();
            assert_eq!(instance(), None);
            assert_eq!(requested_reveal(), None);
            building(opening, || {
                assert_eq!(instance(), Some(9));
                assert_eq!(
                    requested_reveal().as_deref(),
                    Some("chat/links/c_ada"),
                    "the window opens on the item the shell named"
                );
                assert_eq!(
                    requested_open().as_deref(),
                    Some("/documents/Budget.txt"),
                    "the window opens the file the shell named"
                );
                assert!(is_building());
                assert_eq!(take_collaboration().is_some(), carried);
                assert!(take_collaboration().is_none(), "taken once");
                assert!(take_shell_ui().is_none());
            });
            assert_eq!(instance(), None);
            assert_eq!(requested_reveal(), None);
            assert_eq!(requested_open(), None);
            assert!(!is_building());
        }
    }
}
