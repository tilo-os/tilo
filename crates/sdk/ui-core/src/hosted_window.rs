//! The window around an app its host shows in its own process.
//!
//! An app in a process of its own reaches its window's bar and its tab over
//! its shell connection ([`crate::window_toolbar`], [`crate::shell_tabs`]),
//! which knows the asking app by its process. Apps a host mounts in its own
//! process (the desktop's Terminal tabs) share that process and its thread,
//! so they cannot use those: the host lends each of them the window it shows
//! it in, a [`HostedWindow`] (through
//! [`HostContext::lend`](crate::application::HostContext::lend)). On it the
//! app declares its bar's slots and asks to close its tab; the host draws the
//! slots in the bar, hands back what the person did with them, and closes
//! the tab the way the desktop closes any tab, the window with it when it was
//! the window's only tab.
//!
//! The app's side may be used from any thread: a shell that exits is seen
//! by the thread reading its terminal, and the tab closes even while it is
//! not the one the window shows.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::window_toolbar::{ToolbarEvent, WindowToolbar};

/// Upper bound on events the app has not read yet, as for an app in a
/// process of its own.
const MAX_PENDING: usize = 32;

/// One window a host shows an app in, shared by the host and the app.
/// Clones are the same window.
#[derive(Clone, Default)]
pub struct HostedWindow {
    inner: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    toolbar: Option<WindowToolbar>,
    /// Bumped by every declaration that changed the slots.
    revision: u64,
    events: VecDeque<ToolbarEvent>,
    close: bool,
    /// The session-supervisor session the tab runs, as the supervisor named
    /// it to the app: how the host knows which tab a program typed in a
    /// terminal belongs to.
    session: Option<String>,
    /// What the host runs when the app declared or asked to close, so it
    /// looks again even while nothing else is happening.
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for HostedWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state();
        f.debug_struct("HostedWindow")
            .field("revision", &state.revision)
            .field("events", &state.events.len())
            .field("close", &state.close)
            .finish_non_exhaustive()
    }
}

impl HostedWindow {
    /// A window with no slots, nothing to hear and no close asked.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // The state stays whole across a panic elsewhere: every write is
        // one assignment.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wake(wake: Option<Arc<dyn Fn() + Send + Sync>>) {
        if let Some(wake) = wake {
            wake();
        }
    }

    // --- the app's side

    /// Declare the window's slots (`None`: none), bounded as
    /// [`crate::window_toolbar::declare`] bounds them. Cheap when nothing
    /// changed.
    pub fn declare(&self, toolbar: Option<WindowToolbar>) {
        let toolbar = toolbar
            .map(WindowToolbar::bounded)
            .filter(|toolbar| !toolbar.is_empty());
        let wake = {
            let mut state = self.state();
            if state.toolbar == toolbar {
                return;
            }
            state.toolbar = toolbar;
            state.revision = state.revision.wrapping_add(1);
            state.wake.clone()
        };
        Self::wake(wake);
    }

    /// What the person did with the slots since the last call, oldest
    /// first.
    #[must_use]
    pub fn take_events(&self) -> Vec<ToolbarEvent> {
        self.state().events.drain(..).collect()
    }

    /// Close this window's tab, and the window with it when it is the only
    /// tab, the way the desktop closes any tab. From any thread.
    pub fn close(&self) {
        let wake = {
            let mut state = self.state();
            if state.close {
                return;
            }
            state.close = true;
            state.wake.clone()
        };
        Self::wake(wake);
    }

    /// Name the session-supervisor session this tab runs (`None`: none
    /// now), as the supervisor answered when it opened. From any thread.
    pub fn set_session(&self, session: Option<String>) {
        self.state().session = session;
    }

    // --- the host's side

    /// The session-supervisor session the tab runs, as the app named it.
    #[must_use]
    pub fn session(&self) -> Option<String> {
        self.state().session.clone()
    }

    /// What the host runs when the app declares new slots or asks to close.
    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.state().wake = Some(Arc::new(wake));
    }

    /// The slots and their revision, when the revision is not `seen` (the
    /// one the host last showed; `None`: it showed none yet).
    #[must_use]
    pub fn toolbar_since(&self, seen: Option<u64>) -> Option<(u64, Option<WindowToolbar>)> {
        let state = self.state();
        (seen != Some(state.revision)).then(|| (state.revision, state.toolbar.clone()))
    }

    /// Hand the app what the person did with its slots.
    pub fn deliver(&self, event: ToolbarEvent) {
        let mut state = self.state();
        if state.events.len() < MAX_PENDING {
            state.events.push_back(event.bounded());
        }
    }

    /// Whether the app asked to close its tab since the last call.
    #[must_use]
    pub fn take_close(&self) -> bool {
        std::mem::take(&mut self.state().close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_toolbar::ToolbarLocation;

    fn titled(label: &str) -> WindowToolbar {
        WindowToolbar {
            location: Some(ToolbarLocation {
                label: label.into(),
                ..ToolbarLocation::default()
            }),
            ..WindowToolbar::default()
        }
    }

    #[test]
    fn the_host_sees_each_declaration_once_and_the_app_hears_its_events() {
        let window = HostedWindow::new();
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&woken);
        window.set_wake(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let (first, slots) = window.toolbar_since(None).expect("nothing shown yet");
        assert_eq!(slots, None);
        window.declare(Some(titled("Your shell on this device")));
        let (second, slots) = window.toolbar_since(Some(first)).expect("declared");
        assert_eq!(slots, Some(titled("Your shell on this device")));
        window.declare(Some(titled("Your shell on this device")));
        assert_eq!(window.toolbar_since(Some(second)), None, "unchanged");
        window.deliver(ToolbarEvent::Commit {
            text: "target:host".into(),
        });
        assert_eq!(
            window.take_events(),
            vec![ToolbarEvent::Commit {
                text: "target:host".into()
            }]
        );
        assert!(window.take_events().is_empty());
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn a_close_asked_from_another_thread_reaches_the_host_once() {
        let window = HostedWindow::new();
        let app = window.clone();
        std::thread::spawn(move || app.close())
            .join()
            .expect("the reader thread");
        assert!(window.take_close());
        assert!(!window.take_close(), "one close per ask");
    }
}
