//! Every window of an app in a room, served by one process.
//!
//! The shell starts an app once per room. When the person opens another
//! window of it there (another tab of Chat, the Lounge's copy of a window
//! someone else opened), the shell opens it in the running process instead
//! of starting a second one: it writes a [`WindowRequest`] on the window door
//! the launcher handed the process (`runtime_portal::windows`), with the
//! window's own connections to the session attached, and the process builds
//! the window beside the ones it has. Every window then reads and writes the
//! app's records through the one store the process holds; a second process
//! could never take that store's writer lock.
//!
//! The window is built as the process's first was, from the same launch: the
//! request only says which window it is (its instance, the durable id its
//! tab keeps), and carries the connections that are the window's own. While
//! it is built, those stand in for the ones the launcher handed the process
//! ([`instance`], [`take_collaboration`], [`take_shell_ui`]).

use serde::{Deserialize, Serialize};

/// The shell's request to open one more window of the app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRequest {
    /// The window's instance: the durable id its tab keeps, which names its
    /// saved view and the record a window of its own opens
    /// (`TILO_CONTINUITY_INSTANCE` for the process's first window).
    pub instance: u64,
    /// The window's own collaboration connection is attached (first).
    #[serde(default)]
    pub collaboration: bool,
    /// The window's own shell-ui connection is attached (after the
    /// collaboration one, when both are).
    #[serde(default)]
    pub shell_ui: bool,
}

#[cfg(feature = "local-daemons")]
pub use transport::*;

/// Build the window the shell asked for (`pending`), as the process's first
/// window was built from its launch (`spec`, `args`), but with the window's
/// own instance and connections, and nothing the launch named to open: that
/// was the first window's.
#[cfg(feature = "native")]
pub(crate) fn open_window(
    spec: &crate::launch::LaunchSpec,
    args: &crate::launch::LaunchArgs,
    pending: PendingWindow,
) -> Option<crate::app::App> {
    let opening = pending.take()?;
    building(opening, || crate::launch::dispatch_window(spec, args))
        .map_err(|error| tracing::error!(%error, "the window the shell asked for could not open"))
        .ok()
        .flatten()
}

/// The instance of the window being built for the shell's request, while one
/// is; `None` while the process's first window is built, and where there is
/// no window door.
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
    }

    /// One [`WindowOpening`] carried through the event loop, taken once by
    /// the runner that builds its window.
    #[derive(Clone, Debug)]
    pub struct PendingWindow(Arc<Mutex<Option<WindowOpening>>>);

    impl PendingWindow {
        #[must_use]
        pub fn new(opening: WindowOpening) -> Self {
            Self(Arc::new(Mutex::new(Some(opening))))
        }

        #[must_use]
        pub fn take(&self) -> Option<WindowOpening> {
            self.0.lock().map_or_else(
                |poisoned| poisoned.into_inner().take(),
                |mut slot| slot.take(),
            )
        }
    }

    /// Write `request` on the shell's end of `door`, with the connections it
    /// says it carries.
    ///
    /// # Errors
    ///
    /// The door's error, or a request whose flags disagree with what is
    /// attached.
    pub fn send(
        door: &Descriptor,
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
        runtime_portal::windows::send_open(door, &encoded, &fds)
    }

    /// The next request on the app's end of `door`; `None` once the shell
    /// closed it.
    ///
    /// # Errors
    ///
    /// The door's error, or a request that is malformed or does not carry
    /// what it says.
    pub fn receive(door: &Descriptor) -> std::io::Result<Option<WindowOpening>> {
        let Some((encoded, fds)) = runtime_portal::windows::receive_open(door)? else {
            return Ok(None);
        };
        let request: WindowRequest = serde_json::from_slice(&encoded)
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
        Ok(Some(WindowOpening {
            request,
            collaboration,
            shell_ui,
        }))
    }

    /// The door the launcher handed this process, once (see
    /// [`runtime_portal::windows::take_door`]).
    #[must_use]
    pub fn take_door() -> Option<Descriptor> {
        runtime_portal::windows::take_door()
    }

    /// Read `door` on a thread of its own, handing each request to `open`,
    /// until the shell closes it or `open` refuses (its loop is gone).
    ///
    /// # Errors
    ///
    /// The thread could not be started.
    pub fn serve(
        door: Descriptor,
        open: impl Fn(PendingWindow) -> bool + Send + 'static,
    ) -> std::io::Result<()> {
        tilo_rt::spawn_thread("tilo-window-door", move || {
            loop {
                match receive(&door) {
                    Ok(Some(opening)) => {
                        if !open(PendingWindow::new(opening)) {
                            return;
                        }
                    }
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

    thread_local! {
        /// The window being built for a request, while it is built.
        static BUILDING: RefCell<Option<WindowOpening>> = const { RefCell::new(None) };
    }

    /// Build the window `opening` asks for with `build`: while it runs, the
    /// window's instance and connections stand in for the process's own.
    /// What `build` did not take is closed after.
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

    /// The instance of the window being built for a request, if one is.
    #[must_use]
    pub fn instance() -> Option<u64> {
        BUILDING.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|opening| opening.request.instance)
        })
    }

    /// Whether a window is being built for a request: its connections are
    /// its own, never the ones the launcher handed the process.
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

        /// Where the platform has window doors at all.
        fn door() -> Option<(Descriptor, Descriptor)> {
            runtime_portal::windows::pair().ok()
        }

        fn connection() -> Option<Descriptor> {
            door().map(|(one, _)| one)
        }

        #[test]
        fn a_window_request_arrives_with_its_own_connections() {
            let Some((shell, app)) = door() else { return };
            let (Some(collaboration), Some(shell_ui)) = (connection(), connection()) else {
                return;
            };
            let request = WindowRequest {
                instance: 41,
                collaboration: true,
                shell_ui: true,
            };
            send(&shell, &request, Some(&collaboration), Some(&shell_ui)).unwrap();
            let opening = receive(&app).unwrap().unwrap();
            assert_eq!(opening.request, request);
            assert!(opening.collaboration.is_some() && opening.shell_ui.is_some());

            let bare = WindowRequest {
                instance: 42,
                collaboration: false,
                shell_ui: false,
            };
            send(&shell, &bare, None, None).unwrap();
            let opening = receive(&app).unwrap().unwrap();
            assert_eq!(opening.request.instance, 42);
            assert!(opening.collaboration.is_none() && opening.shell_ui.is_none());
            drop(shell);
            assert!(receive(&app).unwrap().is_none());
        }

        #[test]
        fn a_request_that_does_not_carry_what_it_names_is_refused() {
            let Some((shell, app)) = door() else { return };
            let request = WindowRequest {
                instance: 1,
                collaboration: true,
                shell_ui: false,
            };
            assert!(send(&shell, &request, None, None).is_err());
            runtime_portal::windows::send_open(&shell, &serde_json::to_vec(&request).unwrap(), &[])
                .unwrap();
            assert!(receive(&app).is_err());
        }

        /// While a window is built for a request its instance and
        /// connections stand in for the process's, once each, and only
        /// while it is built.
        #[test]
        fn a_window_is_built_with_its_own_instance_and_connections() {
            let opening = WindowOpening {
                request: WindowRequest {
                    instance: 9,
                    collaboration: true,
                    shell_ui: false,
                },
                collaboration: connection(),
                shell_ui: None,
            };
            let carried = opening.collaboration.is_some();
            assert_eq!(instance(), None);
            building(opening, || {
                assert_eq!(instance(), Some(9));
                assert!(is_building());
                assert_eq!(take_collaboration().is_some(), carried);
                assert!(take_collaboration().is_none(), "taken once");
                assert!(take_shell_ui().is_none());
            });
            assert_eq!(instance(), None);
            assert!(!is_building());
        }
    }
}
