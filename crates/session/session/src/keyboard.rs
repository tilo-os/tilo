//! The on-screen keyboard, as the session keeps it: the app that draws it is
//! started while the person may want it, stands on the display they type on,
//! and shows itself while a text field waits for typing on the glass.
//!
//! The keyboard is a process of its own, so nothing here crosses an `Rc`
//! between the two: the session keeps the state, publishes it as [`KEYBOARD`],
//! and the app reads it as any app of the session reads a resource. Whether
//! it belongs on screen is [`crate::a11y_settings::keyboard_visible`], from
//! the person's own choice and what the compositor sees of their typing;
//! whether it runs at all is [`crate::a11y_settings::keyboard_runs`].
//!
//! Its keys are typed into the field through the compositor's input method,
//! as any keyboard's are: what it draws is the only thing the session places.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_service::{Service, Started, reading};

use crate::surfaces::{
    Anchor, Keyboard as Interactivity, Layer, StartError, Surface, program_path,
};

/// The resource the keyboard's app reads.
pub const KEYBOARD: &str = "keyboard";

/// What the session calls the keyboard's app: its name in the log, the key it stops by, and the
/// program it is installed as.
pub const SURFACE: &str = "osk";

/// Its program, as it is installed beside the session's own.
const PROGRAM: &str = "tilo-osk";

/// Its kind, as a reader with no words of its own knows it.
const KEYBOARD_KIND: &str = "keyboard.on-screen";

/// The margin between the keyboard and the bottom and sides of the display,
/// in logical pixels.
const MARGIN: i32 = 20;
/// How wide the keyboard grows at most before it stops widening with the display.
const WIDEST: u32 = 960;
/// How tall the keyboard is at least, on a short display, and how tall it grows at most: it takes
/// two fifths of the display's height between them.
const LOWEST: u32 = 220;
const TALLEST: u32 = 380;

/// How many times the keyboard's app is started for the same display before it is given up on: one
/// that will not stay up leaves the person without a keyboard rather than starting for ever.
const TRIES: u32 = 3;

/// The keyboard's surface app, when one is installed: a window of its own
/// that the session places at the bottom of a display, and that never takes
/// the keyboard from the field its taps are typed into.
///
/// # Errors
/// No on-screen keyboard is installed, or its program is not there.
pub fn surface() -> Result<Surface, StartError> {
    Ok(surface_of(program_path(PROGRAM)?))
}

/// The keyboard's surface app, run from `program` .
#[must_use]
pub fn surface_of(program: impl Into<std::path::PathBuf>) -> Surface {
    Surface::new(SURFACE, program)
        .layer(Layer::Top)
        .anchor(Anchor::BOTTOM)
        // Its taps type into the field it stands over: the keyboard it takes would be the field's,
        // and taking it would close the field.
        .keyboard(Interactivity::None)
}

/// Where the keyboard stands on a display of `width` by `height` logical pixels: the size its
/// window takes, and the margin from the edges it is pinned to.
/// At the bottom, centred, no wider than [`WIDEST`], between [`LOWEST`] and [`TALLEST`] tall with
/// [`MARGIN`] to the display's edge: wide enough to type on with two thumbs on a phone, and narrow
/// enough to leave the window it types into in view on a monitor.
#[must_use]
pub fn band(width: i32, height: i32) -> ((u32, u32), (i32, i32, i32, i32)) {
    let (width, height) = (i64::from(width), i64::from(height));
    let margin = i64::from(MARGIN).min(width.saturating_div(20));
    let widest = width
        .saturating_sub(margin.saturating_mul(2))
        .max(1)
        .min(i64::from(WIDEST));
    let tallest = height
        .saturating_mul(2)
        .saturating_div(5)
        .clamp(i64::from(LOWEST), i64::from(TALLEST));
    let shortest = height.saturating_sub(margin.saturating_mul(2)).max(1);
    let band = (
        u32::try_from(widest).unwrap_or(1),
        u32::try_from(tallest.min(shortest)).unwrap_or(1),
    );
    // Only the bottom edge is pinned, so only its margin places the keyboard: it stands that far
    // above the display's edge.
    (band, (0, 0, i32::try_from(margin).unwrap_or(0), 0))
}

/// What the on-screen keyboard does now. The session's loop says whether it belongs on screen and
/// where it stands; the service hands that to the app.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shown {
    /// The keyboard belongs on screen now.
    pub visible: bool,
    /// The display it stands on, while it runs.
    pub display: Option<String>,
}

/// Where the on-screen keyboard's app stands, as the session's loop keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Run {
    /// It does not run: nobody may want it now.
    #[default]
    Down,
    /// It does not run, and is not asked again this session: its program is not installed, and does
    /// not appear by waiting.
    Failed,
    /// It runs on `display` , `size` logical pixels wide and tall.
    Standing { display: String, size: (u32, u32) },
}

/// The keyboard's state, as the session's loop keeps it.
#[derive(Clone, Default)]
pub struct Keyboard {
    shown: Rc<RefCell<Shown>>,
}

impl Keyboard {
    /// Say what the keyboard does now, when it is not what it did.
    pub fn show(&self, shown: Shown) {
        if *self.shown.borrow() == shown {
            return;
        }
        *self.shown.borrow_mut() = shown;
    }

    /// What the keyboard does now.
    #[must_use]
    pub fn shown(&self) -> Shown {
        self.shown.borrow().clone()
    }
}

/// The keyboard's state, as the session's services publish it.
pub struct KeyboardService {
    state: Keyboard,
    /// What was last published, so a change of it is one revision.
    shown: Option<Shown>,
}

impl KeyboardService {
    /// The service publishing `state` .
    #[must_use]
    pub fn new(state: Keyboard) -> Self {
        let shown = Some(state.shown());
        Self { state, shown }
    }
}

impl Service for KeyboardService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![ResourceSpec::new(
            KEYBOARD,
            KEYBOARD_KIND,
            "On-screen keyboard",
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == KEYBOARD).then(|| reading(&*self.state.shown.borrow()))
    }

    /// The keyboard is read, never asked: it types through the compositor's input method, and shows
    /// or hides itself by what is published here.
    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &serde_json::Map<String, Value>,
    ) -> Result<Started, DoorError> {
        Err(DoorError::Unknown(ActionAddress::new(resource, action)))
    }

    fn poll(&mut self) -> bool {
        let shown = self.state.shown();
        if self.shown.as_ref() == Some(&shown) {
            return false;
        }
        self.shown = Some(shown);
        true
    }
}

impl crate::Session {
    /// Follow the on-screen keyboard: run its app while the person may want it, on the display they
    /// are working on, and publish whether it shows and where it stands.
    /// A field's focus reaches an input method only once one is connected, so the app runs from
    /// before the tap into a field rather than from it: what it does about showing itself is
    /// decided each pass, from the person's choice and what the compositor sees of their typing.
    pub(crate) fn follow_keyboard(&mut self) {
        let keyboard = self.shared.keyboard.clone();
        let entry = self.compositor.text_entry();
        let preference = self.keyboard_preference;
        // The lock screen draws its own keyboard over a locked session, and takes every key:
        // nothing of the session's stands over it.
        let unlocked = !self.compositor.locked();
        // An engine of the active source owns the seat's input method: its
        // keys compose characters, which a keyboard's committed text cannot,
        // and one field broken between two input methods belongs to neither.
        let composing = crate::input_method::composes(&self.sources.active);
        let runs = unlocked && !composing && crate::a11y_settings::keyboard_runs(preference, entry);
        if !runs {
            self.stop_keyboard();
            // Wanted again later, the keyboard is asked afresh: its tries are the ones since it was
            // last not wanted at all.
            self.keyboard_run = Run::Down;
            self.keyboard_starts = 0;
            keyboard.show(Shown::default());
            return;
        }
        if self.keyboard_run == Run::Failed {
            keyboard.show(Shown::default());
            return;
        }
        // The app may have exited since the last pass: the keyboard stands on nothing then, and is
        // started again where the person types.
        if matches!(self.keyboard_run, Run::Standing { .. }) && !self.surfaces.running(SURFACE) {
            self.keyboard_run = Run::Down;
            self.keyboard_starts = self.keyboard_starts.saturating_add(1);
            if self.keyboard_starts >= TRIES {
                tracing::error!(
                    starts = self.keyboard_starts,
                    "the on-screen keyboard would not stay up: it is not started again this session"
                );
                self.keyboard_run = Run::Failed;
                keyboard.show(Shown::default());
                return;
            }
        }
        let Some((screen, (_, _, width, height))) = self.keyboard_display() else {
            // No display to stand on, and nothing to say about one.
            self.stop_keyboard();
            keyboard.show(Shown::default());
            return;
        };
        let (size, margin) = band(width, height);
        let started = Run::Standing {
            display: screen.clone(),
            size,
        };
        if self.keyboard_run != started {
            if matches!(self.keyboard_run, Run::Standing { .. }) {
                // Another display, or one that changed size: a fresh start,
                // and its tries with it.
                self.keyboard_starts = 0;
            }
            self.stop_keyboard();
            let Some(surface) = self
                .surface_apps
                .iter()
                .find(|surface| surface.name == SURFACE)
            else {
                tracing::warn!("no on-screen keyboard is installed");
                self.keyboard_run = Run::Failed;
                keyboard.show(Shown::default());
                return;
            };
            let placed = surface.clone().on(screen.clone()).size(size).margin(margin);
            if let Err(error) = self.start_surface(&placed) {
                tracing::warn!(
                    display = %screen,
                    %error,
                    "the on-screen keyboard did not start: no keyboard is shown"
                );
                self.keyboard_run = Run::Failed;
                keyboard.show(Shown::default());
                return;
            }
            tracing::debug!(display = %screen, ?size, "the on-screen keyboard stands");
            self.keyboard_run = started;
        }
        keyboard.show(Shown {
            visible: crate::a11y_settings::keyboard_visible(preference, entry),
            display: Some(screen),
        });
    }

    /// Stop the on-screen keyboard's app, if it runs.
    pub(crate) fn stop_keyboard(&mut self) {
        if matches!(self.keyboard_run, Run::Standing { .. }) {
            self.keyboard_run = Run::Down;
            self.surfaces.stop(SURFACE);
        }
    }

    /// The display the person is working on, with its rectangle: the one holding the focused
    /// window, and the first display there is otherwise.
    fn keyboard_display(&self) -> Option<(String, (i32, i32, i32, i32))> {
        let displays: Vec<(String, (i32, i32, i32, i32))> = self
            .compositor
            .outputs()
            .into_iter()
            .filter(|output| output.enabled)
            .filter_map(|output| Some((output.name, output.rect?)))
            .collect();
        let focused = self
            .compositor
            .focused_window()
            .and_then(|focused| {
                self.compositor
                    .windows()
                    .into_iter()
                    .find(|(window, _)| window.id == focused.id)
                    .map(|(_, rect)| rect)
            })
            .map(|(x, y, w, h)| {
                (
                    i64::from(x).saturating_add(i64::from(w).saturating_div(2)),
                    i64::from(y).saturating_add(i64::from(h).saturating_div(2)),
                )
            });
        let holding = focused.and_then(|(cx, cy)| {
            displays
                .iter()
                .find(|(_, (x, y, w, h))| {
                    let (x, y) = (i64::from(*x), i64::from(*y));
                    let (w, h) = (i64::from(*w), i64::from(*h));
                    cx >= x && cx < x.saturating_add(w) && cy >= y && cy < y.saturating_add(h)
                })
                .cloned()
        });
        holding.or_else(|| displays.first().cloned())
    }
}
