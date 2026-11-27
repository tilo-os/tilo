//! Every window of an app in a workspace, served by one process. The shell starts an app
//! once per workspace. Every window then reads and writes the app's records through the
//! one store the process holds; a second process could never take that store's writer
//! lock.

/// The session's request to open one more window of the app.
#[cfg(feature = "local-daemons")]
pub use runtime_portal::process_link::WindowRequest;

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

/// How many windows of this process take invocations: the one its launch made, the
/// windowless endpoint it starts with, and each window the shell asks for later, until it
/// stops taking them as it closes ([`window_stops_taking_invocations`]). A process with none
/// left lets go of its link while it is still alive to finish them, so the shell starts its
/// next window, and the work that comes after, in a process of its own.
#[cfg(feature = "local-daemons")]
static TAKING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// The process's loop starts, with one window taking invocations.
#[cfg(feature = "native")]
pub fn process_starts() {
    TAKING.store(1, std::sync::atomic::Ordering::Release);
}

/// The shell asked the process for one more window: it takes invocations too.
#[cfg(feature = "native")]
pub fn window_takes_invocations() {
    TAKING.fetch_add(1, std::sync::atomic::Ordering::Release);
}

/// This window takes no more invocations: it answers the ones it was handed and goes. One
/// that was the process's last to take them lets go of its link, so what is asked of the app
/// from now on reaches a process that takes it, rather than one that is on its way out.
pub fn window_stops_taking_invocations() {
    #[cfg(feature = "local-daemons")]
    {
        use std::sync::atomic::Ordering;
        let last = TAKING
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |taking| {
                taking.checked_sub(1)
            })
            .is_ok_and(|before| before == 1);
        if last {
            transport::stop_serving();
        }
    }
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

/// The instance of the window a bound spare process opens first ([`adopt`]), until it is built.
#[must_use]
pub fn first_instance() -> Option<u64> {
    #[cfg(feature = "local-daemons")]
    {
        transport::first_instance()
    }
    #[cfg(not(feature = "local-daemons"))]
    {
        None
    }
}

/// Build the process's first window with `build`: as the window the session bound this process
/// for ([`adopt`]), with its instance and connections, when it was; else as its launch names it.
#[cfg(feature = "native")]
pub fn build_first<T>(build: impl FnOnce() -> T) -> T {
    #[cfg(feature = "local-daemons")]
    if let Some(first) = transport::take_first() {
        return transport::building(first, build);
    }
    build()
}

/// The variable a launch names the item its window opens on in, for a process the shell
/// starts for that window rather than one it asks for it: a window link request cannot
/// reach a window that is already being built.
pub const REVEAL_ENV: &str = crate::process::REVEAL_ENV;

/// The variable the session sets on a launch it makes with no window: the app answers its
/// actions and serves no window link, and its first window is the first one the shell asks for here.
pub const NO_WINDOW_ENV: &str = crate::process::NO_WINDOW_ENV;

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
        let environment = crate::process::environment();
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

#[cfg(all(feature = "native", feature = "model"))]
/// A launch whose record is not on this device yet: the workspace's join, waited for off the window's
/// thread, and the wake scope the app built after it takes, which the join's manager wakes.
pub struct Arriving {
    pub join: ::collaboration::NativeJoin,
    pub scope: crate::wake::WakeScope,
}

#[cfg(all(feature = "native", feature = "model"))]
impl std::fmt::Debug for Arriving {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Arriving").finish_non_exhaustive()
    }
}

#[cfg(feature = "local-daemons")]
mod transport {
    use super::WindowRequest;
    use runtime_portal::process_link::{Descriptor, FromProcess, Packet as _, ToProcess};
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
        #[cfg(all(feature = "native", feature = "model"))]
        pub arriving: Option<super::Arriving>,
    }

    /// Hand a [`PendingWindow`] to the loop that builds windows; `false` once that loop
    /// is gone.
    pub(crate) type Reopen = Arc<dyn Fn(PendingWindow) -> bool + Send + Sync>;

    /// One [`WindowOpening`] carried through the event loop, taken once by the runner
    /// that builds its window.
    #[derive(Clone)]
    pub struct PendingWindow {
        pub slot: Arc<Mutex<Option<WindowOpening>>>,
        pub reopen: Option<Reopen>,
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
        pub fn put(&self, opening: WindowOpening) {
            match self.slot.lock() {
                Ok(mut slot) => *slot = Some(opening),
                Err(poisoned) => *poisoned.into_inner() = Some(opening),
            }
        }
    }

    fn opening(
        request: WindowRequest,
        collaboration: Option<Descriptor>,
        shell_ui: Option<Descriptor>,
    ) -> WindowOpening {
        WindowOpening {
            request,
            collaboration,
            shell_ui,
            #[cfg(all(feature = "native", feature = "model"))]
            arriving: None,
        }
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

    /// The patch connection of the window `instance` arrived: a spare process reads it off its
    /// link before it serves it.
    pub fn patch_arrived(instance: u64, patch: Descriptor) {
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
            *slot = runtime_portal::process_link::take();
            if let Some(link) = slot.as_ref()
                && let Err(error) = FromProcess::Serves.send(link)
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

    /// The first window of a spare process the session bound to an app, until it is built.
    static FIRST: Mutex<Option<WindowOpening>> = Mutex::new(None);

    fn first() -> std::sync::MutexGuard<'static, Option<WindowOpening>> {
        FIRST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Serve `link`, which a spare window process read its display and its binding from, as the
    /// link of the app the session bound it to, and open `request` with `shell_ui` as the
    /// process's first window. The session counts the app as serving its windows from the
    /// binding on, so nothing is announced.
    pub fn adopt(link: Descriptor, request: WindowRequest, shell_ui: Option<Descriptor>) {
        *first() = Some(opening(request, None, shell_ui));
        let mut slot = LINK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        TAKEN.store(true, std::sync::atomic::Ordering::Release);
        *slot = Some(link);
    }

    pub(super) fn first_instance() -> Option<u64> {
        first().as_ref().map(|opening| opening.request.instance)
    }

    #[cfg(feature = "native")]
    pub(super) fn take_first() -> Option<WindowOpening> {
        first().take()
    }

    /// Read `link` on a thread of its own, handing each request to `open`, until the
    /// shell closes it or `open` refuses (its loop is gone). The thread could not be
    /// started.
    pub fn serve(
        link: Descriptor,
        open: impl Fn(PendingWindow) -> bool + Send + 'static,
    ) -> std::io::Result<()> {
        *served() = runtime_portal::process_link::try_clone(&link).ok();
        let open = Arc::new(Mutex::new(open));
        let reopen: Reopen = Arc::new(move |pending| match open.lock() {
            Ok(open) => open(pending),
            Err(poisoned) => (poisoned.into_inner())(pending),
        });
        tilo_rt::spawn_thread("tilo-window-link", move || {
            loop {
                let received = ToProcess::receive(&link).map(|message| match message {
                    Some(ToProcess::Window {
                        request,
                        collaboration,
                        shell_ui,
                    }) => Some(Some(opening(request, collaboration, shell_ui))),
                    Some(ToProcess::Patch { instance, patch }) => {
                        patch_arrived(instance, patch);
                        Some(None)
                    }
                    Some(other) => {
                        tracing::warn!(
                            ?other,
                            "the session's link carried what no window asks for"
                        );
                        Some(None)
                    }
                    None => None,
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
            && let Err(error) = runtime_portal::process_link::close(&link)
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
    #[cfg(all(feature = "native", feature = "model"))]
    pub fn keep_building() -> Option<WindowOpening> {
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
            runtime_portal::process_link::pair().ok()
        }

        fn connection() -> Option<Descriptor> {
            link_pair().map(|(one, _)| one)
        }

        /// While a window is built for a request its instance and connections stand in
        /// for the process's, once each, and only while it is built.
        #[test]
        fn a_window_is_built_with_its_own_instance_and_connections() {
            let opening = WindowOpening {
                request: WindowRequest {
                    instance: 9,
                    reveal: Some("chat/links/c_ada".into()),
                    open: Some("/documents/Budget.txt".into()),
                },
                collaboration: connection(),
                shell_ui: None,
                #[cfg(all(feature = "native", feature = "model"))]
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

        /// A process serves `link` as the launcher handed it one.
        fn serves(link: Descriptor) {
            *served() = runtime_portal::process_link::try_clone(&link).ok();
        }

        /// The windows of a process that are still leaving: the windowless endpoint it
        /// started with, and the window a send opened in its place.
        ///
        /// As the last of them stops taking invocations the process lets go of its link,
        /// though it is still alive to finish what it was handed: the shell starts the next
        /// window, and the work that comes after, in a process of its own. Before, the count
        /// was of windows, and the windowless endpoint still counted as one after it stopped,
        /// so a process with nothing left to take invocations kept its link, and what was
        /// asked of the app waited on it until it went.
        #[test]
        fn a_process_with_no_window_left_taking_invocations_lets_go_of_its_link() {
            let Some((ours, theirs)) = link_pair() else {
                return;
            };
            serves(theirs);
            crate::window_link::process_starts();
            crate::window_link::window_takes_invocations();
            // The windowless endpoint stops as the window opens.
            crate::window_link::window_stops_taking_invocations();
            assert!(
                !runtime_portal::process_link::hung_up(&ours),
                "a window still takes them"
            );
            // That window closes; the process stays to save what it holds.
            crate::window_link::window_stops_taking_invocations();
            assert!(
                runtime_portal::process_link::hung_up(&ours),
                "it lets go of its link once no window of it takes invocations"
            );
            *served() = None;
        }
    }
}
