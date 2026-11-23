//! A window brought into view for the person without taking their keyboard: the window their own
//! agent opened or works in, so they see what it does. The strip scrolls to it as it scrolls to a
//! window the person opens, and the window the person was typing in keeps the keyboard. While the
//! person is in the middle of input (typing, scrolling, dragging) the strip stays still under
//! their hands and comes to the window once they pause ([`INPUT_QUIET`]).

use tilo_desktop_model::TabId;
use ui::time::{Duration, Instant};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

use super::{DesktopHandle, navigation, route_of};

/// How long the person's hands rest before a window is brought into view for them: a strip that
/// moved between two keystrokes would move the field they type in from under their eyes.
pub const INPUT_QUIET: Duration = Duration::from_secs(2);

/// The window waiting to come into view, and how recently the person used the shell.
#[derive(Debug, Default)]
pub struct Revealing {
    /// The window to bring into view ([`DesktopHandle::reveal_tab`]); the newest ask wins.
    pub asked: Option<TabId>,
    /// When the person last typed, scrolled or moved a held pointer on the shell.
    pub last_input: Option<Instant>,
    /// The person holds the primary button down: a drag in progress, however still.
    pub pressed: bool,
}

impl DesktopHandle {
    /// Bring the window showing `tab` into view on the next pass, without moving the keyboard: the
    /// strip scrolls to it, the focused window stays focused. While the person types, scrolls or
    /// drags it waits until they pause ([`INPUT_QUIET`]). Only a window in the shown strip moves
    /// into view; one on another strip stays where it is. False when no window shows `tab`.
    pub fn reveal_tab(&self, tab: TabId) -> bool {
        let mut shared = self.shared.borrow_mut();
        if route_of(&shared.tree, tab).is_none() {
            return false;
        }
        shared.revealing.asked = Some(tab);
        shared.wake.wake();
        true
    }

    /// The window waiting to come into view until the person pauses, if any.
    #[must_use]
    pub fn reveal_waiting(&self) -> Option<TabId> {
        self.shared.borrow().revealing.asked
    }

    /// The window of `app_id` in the active workspace: the focused one when it is that app's, else
    /// its first. `None` when the workspace shows none.
    #[must_use]
    pub fn tab_of_app(&self, app_id: &str) -> Option<TabId> {
        self.read(|tree| {
            let shows = |tab: &tilo_desktop_model::Tab| {
                !tilo_desktop_model::apps::shows_a_view(tab)
                    && matches!(&tab.tile, tilo_desktop_model::TileContent::LocalApp { app_id: id, .. } if id == app_id)
            };
            if let Some(active) = navigation::active_route(tree)
                && super::find_tab(tree, active.strip, active.tab).is_some_and(|(_, tab)| shows(tab))
            {
                return Some(active.tab);
            }
            tree.active_workspace()?
                .tabs()
                .find(|tab| shows(tab))
                .map(|tab| tab.id)
        })
    }
}

impl Revealing {
    /// Whether the person is in the middle of input at `now`.
    pub fn busy(&self, now: Instant) -> bool {
        self.pressed
            || self
                .last_input
                .is_some_and(|at| now.saturating_duration_since(at) < INPUT_QUIET)
    }

    /// Note what `event` says of the person's hands at `now`.
    pub fn note(&mut self, event: &WindowEvent, now: Instant) {
        match event {
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::MouseWheel { .. } => {
                self.last_input = Some(now);
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.pressed = *state == ElementState::Pressed;
                self.last_input = Some(now);
            }
            WindowEvent::CursorMoved { .. } if self.pressed => self.last_input = Some(now),
            _ => {}
        }
    }

    /// Whether a window waits to come into view and may now.
    pub fn ready(&self, now: Instant) -> bool {
        self.asked.is_some() && !self.busy(now)
    }

    /// Whether a window waits to come into view at all, and so the strip has
    /// yet to move for it.
    pub fn waiting(&self) -> bool {
        self.asked.is_some()
    }

    /// When a waiting window may come into view: once the person's hands have rested.
    pub fn due(&self) -> Option<Instant> {
        self.asked?;
        self.last_input?.checked_add(INPUT_QUIET)
    }
}
