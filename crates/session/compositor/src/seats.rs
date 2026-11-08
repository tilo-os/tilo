//! Seats beside the person's: a pointer and a keyboard of its own.
//! The compositor drives one app-facing seat: clients see one pointer and one keyboard. Everyone
//! else who acts in a window — an agent through its computer tools, a person watching a shared
//! window from another device — acts at a [`Seat`] of their own: its own pointer position, its own
//! keyboard focus, and its own held keys and buttons. While one of its events is delivered the
//! app-facing seat stands where the seat left its pointer, types into the window it aims at, and
//! holds exactly the keys and buttons the seat holds; afterwards the person's pointer, focus and
//! holds are back. A `Shift` an agent holds never shifts the person's typing, a drag an agent began
//! never ends with the person's click, and neither one moves the window the person is working in to
//! the front.

use std::collections::BTreeSet;

use smithay::input::pointer::MotionEvent;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Serial};

use crate::input::InputEvent;
use crate::state::{PointerTarget, State};

/// A seat of its own, opened by [`crate::Compositor::open_seat`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seat(u64);

/// What a seat aims at: the window whose pixels its coordinates are.
#[derive(Clone, Debug, PartialEq)]
pub struct Aim {
    /// The window it acts in, when it aims at one.
    pub window: Option<WindowAim>,
    /// The layout's extent, width and height.
    pub screen: (f64, f64),
}

impl Aim {
    /// The rectangle the seat acts in, `(x, y, width, height)` in the layout: its window's, or the
    /// whole layout's.
    #[must_use]
    pub fn viewport(&self) -> (f64, f64, f64, f64) {
        self.window
            .as_ref()
            .map_or((0.0, 0.0, self.screen.0, self.screen.1), |window| {
                window.rect
            })
    }
}

/// The window a seat aims at, as the compositor knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowAim {
    /// The window's id, as [`crate::Compositor::windows`] names it.
    pub id: u64,
    /// Its foreign-toplevel identifier, which captures name it by.
    pub identifier: String,
    /// Its place in the layout, `(x, y, width, height)` .
    pub rect: (f64, f64, f64, f64),
}

/// What one seat holds and where it left its pointer.
#[derive(Clone, Debug, Default)]
pub(crate) struct Held {
    /// Where its pointer last was, in the layout.
    pub(crate) pointer: Option<(f64, f64)>,
    /// The window its pointer and keys act in.
    pub(crate) aim: Option<WindowAim>,
    /// The keys it holds down (evdev codes).
    pub(crate) keys: BTreeSet<u32>,
    /// The pointer buttons it holds down.
    pub(crate) buttons: BTreeSet<u32>,
}

/// Every seat beside the person's, and what the app-facing seat holds now.
#[derive(Debug, Default)]
pub(crate) struct Seats {
    next: u64,
    held: std::collections::BTreeMap<u64, Held>,
    /// The keys the app-facing keyboard holds down: the person's, between a seat's events.
    shown_keys: BTreeSet<u32>,
    /// The pointer buttons this machine's own pointer holds down.
    person_buttons: BTreeSet<u32>,
    /// How many seats and the person hold each pointer button down. One pointer serves them all: a
    /// press reaches the window at 0→1 and its release at 1→0, so a drag one began never ends on
    /// another's click.
    button_holders: std::collections::BTreeMap<u32, usize>,
}

impl Seats {
    pub(crate) fn open(&mut self) -> Seat {
        self.next = self.next.wrapping_add(1);
        self.held.insert(self.next, Held::default());
        Seat(self.next)
    }

    pub(crate) fn has(&self, seat: Seat) -> bool {
        self.held.contains_key(&seat.0)
    }

    pub(crate) fn get(&self, seat: Seat) -> Option<&Held> {
        self.held.get(&seat.0)
    }

    pub(crate) fn get_mut(&mut self, seat: Seat) -> Option<&mut Held> {
        self.held.get_mut(&seat.0)
    }

    /// Forget the seat, answering what it still held.
    pub(crate) fn close(&mut self, seat: Seat) -> Option<Held> {
        self.held.remove(&seat.0)
    }

    /// Fold one of the person's own keys into what the app-facing keyboard holds, for the next seat
    /// event to put aside and put back.
    pub(crate) fn hold_person_key(&mut self, keycode: u32, pressed: bool) {
        if pressed {
            self.shown_keys.insert(keycode);
        } else {
            self.shown_keys.remove(&keycode);
        }
    }

    /// Fold one of the person's own pointer buttons into the one pointer the windows see: whether
    /// the app-facing pointer hears it.
    pub(crate) fn hold_person_button(&mut self, button: u32, pressed: bool) -> bool {
        if !pressed
            && !self.person_buttons.contains(&button)
            && !self.button_holders.contains_key(&button)
        {
            // A button nobody here pressed is let go: a drop the desktop ends on a release it never
            // saw pressed, as a hosted app's drag does.
            return true;
        }
        let Self {
            person_buttons,
            button_holders,
            ..
        } = self;
        fold(person_buttons, button_holders, button, pressed)
    }

    /// The same for a seat's own button.
    pub(crate) fn hold_button(&mut self, seat: Seat, button: u32, pressed: bool) -> bool {
        let Some(buttons) = self.held.get_mut(&seat.0).map(|held| &mut held.buttons) else {
            return false;
        };
        fold(buttons, &mut self.button_holders, button, pressed)
    }
}

/// Fold one holder's button transition into the shared count: `true` when the app-facing pointer
/// must hear it.
fn fold(
    held: &mut BTreeSet<u32>,
    holders: &mut std::collections::BTreeMap<u32, usize>,
    button: u32,
    pressed: bool,
) -> bool {
    if pressed {
        if !held.insert(button) {
            return false;
        }
        let count = holders.entry(button).or_default();
        *count = count.saturating_add(1);
        *count == 1
    } else {
        if !held.remove(&button) {
            return false;
        }
        let Some(count) = holders.get_mut(&button) else {
            return false;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            holders.remove(&button);
            true
        } else {
            false
        }
    }
}

/// What the person had in front while a seat's event is delivered, put back when the event is done.
pub(crate) struct Person {
    pointer: Point<f64, Logical>,
    focus: Option<WlSurface>,
    keys: BTreeSet<u32>,
}

impl State {
    /// Put the person's pointer, focus and holds aside and stand the app-facing seat in `seat` 's
    /// place, for one event or a run of them. `None` for a seat this compositor does not have, or
    /// while the session is locked: the lock screen is not a way into the machine.
    pub(crate) fn seat_begin(&mut self, seat: Seat) -> Option<Person> {
        if !self.seats.has(seat) || self.locked() {
            return None;
        }
        let held = self.seats.get(seat)?.clone();
        let person = Person {
            pointer: self.pointer,
            focus: self
                .seat
                .get_keyboard()
                .and_then(|keyboard| keyboard.current_focus()),
            keys: std::mem::take(&mut self.seats.shown_keys),
        };
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        let time = self.now_ms();
        // The person lets go of what this seat does not hold, while the person's own focus still
        // hears the release.
        let released: Vec<u32> = person.keys.difference(&held.keys).copied().collect();
        keys(self, &released, false, serial, time);
        // The seat stands where it left its pointer, and types into its window.
        if let Some((x, y)) = held.pointer {
            self.seat_pointer((x, y), serial, time);
        }
        if let Some(aim) = &held.aim {
            self.focus_window_id(aim.id, serial);
        }
        self.seat_keys_only = true;
        let pressed: Vec<u32> = held.keys.difference(&person.keys).copied().collect();
        keys(self, &pressed, true, serial, time);
        self.seats.shown_keys = held.keys;
        Some(person)
    }

    /// Forget `keycode` among what `seat` holds: a release the seat sent lets go of it before the
    /// event begins, so the app-facing keyboard is not pressed into it again.
    pub(crate) fn seat_forget_key(&mut self, seat: Seat, keycode: u32) {
        if let Some(held) = self.seats.get_mut(seat) {
            held.keys.remove(&keycode);
        }
    }

    /// Deliver one event as `seat`: a pointer and a keyboard, in its window,
    /// with none of the desktop's own rules (a press focuses nothing, a
    /// gesture resizes nothing, no hosted surface takes it).
    pub(crate) fn seat_deliver(&mut self, seat: Seat, event: &InputEvent) {
        if !self.seats.has(seat) {
            return;
        }
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        let time = self.now_ms();
        self.mark_redraw_all();
        match *event {
            InputEvent::PointerMotion { x, y } => {
                if let Some(held) = self.seats.get_mut(seat) {
                    held.pointer = Some((x, y));
                }
                self.seat_pointer((x, y), serial, time);
            }
            InputEvent::PointerMotionRelative { dx, dy } => {
                let from = self
                    .seats
                    .get(seat)
                    .and_then(|held| held.pointer)
                    .unwrap_or((self.pointer.x, self.pointer.y));
                let to = (from.0 + dx, from.1 + dy);
                if let Some(held) = self.seats.get_mut(seat) {
                    held.pointer = Some(to);
                }
                self.seat_pointer(to, serial, time);
            }
            InputEvent::PointerButton { button, pressed } => {
                if self.seats.hold_button(seat, button, pressed) {
                    self.seat_button(button, pressed, serial, time);
                }
            }
            InputEvent::PointerAxis {
                horizontal,
                vertical,
            } => {
                self.pointer_target = PointerTarget::Clients;
                self.pointer_axis(horizontal, vertical, time);
            }
            InputEvent::Key { keycode, pressed } => {
                let Some(held) = self.seats.get_mut(seat) else {
                    return;
                };
                // A key the seat held was pressed when the event began, and one it does not hold is
                // the person's: only the transition the app-facing keyboard has not heard yet is
                // delivered.
                let had = held.keys.contains(&keycode);
                if pressed {
                    held.keys.insert(keycode);
                } else {
                    held.keys.remove(&keycode);
                }
                if pressed != had {
                    self.key(keycode, pressed, serial, time);
                }
            }
            // Touches, tablets, gestures and switches belong to the person's own devices, and no
            // seat carries them.
            _ => {}
        }
    }

    /// Put the person back: their pointer, their focus, and exactly the keys and buttons they held
    /// before the seat's event.
    pub(crate) fn seat_end(&mut self, seat: Seat, person: Person) {
        let held = self.seats.get(seat).cloned().unwrap_or_default();
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        let time = self.now_ms();
        // The seat lets go of what the person does not hold, while the seat's own window still
        // hears the release.
        let released: Vec<u32> = held.keys.difference(&person.keys).copied().collect();
        keys(self, &released, false, serial, time);
        self.seat_keys_only = false;
        // The person's pointer and focus come back.
        self.seat_pointer((person.pointer.x, person.pointer.y), serial, time);
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, person.focus.clone(), serial);
        }
        let pressed: Vec<u32> = person.keys.difference(&held.keys).copied().collect();
        keys(self, &pressed, true, serial, time);
        self.seats.shown_keys = person.keys;
    }

    /// End `seat`: a button it still held goes up in the window it aimed at,
    /// then the seat is gone.
    pub(crate) fn close_seat(&mut self, seat: Seat) {
        let Some(held) = self.seats.get(seat).cloned() else {
            return;
        };
        if !held.buttons.is_empty()
            && !self.locked()
            // Releasing them re-presses nothing of the seat's own.
            && let Some(stored) = self.seats.get_mut(seat)
        {
            stored.keys.clear();
            if let Some(person) = self.seat_begin(seat) {
                let serial = smithay::utils::SERIAL_COUNTER.next_serial();
                let time = self.now_ms();
                for button in held.buttons {
                    if self.seats.hold_button(seat, button, false) {
                        self.seat_button(button, false, serial, time);
                    }
                }
                self.seat_end(seat, person);
            }
        }
        self.seats.close(seat);
    }

    /// The pointer at `at` in the layout, delivered to the surface under it and to no hosted
    /// surface.
    fn seat_pointer(&mut self, at: (f64, f64), serial: Serial, time: u32) {
        let location = self.clamp_to_outputs(Point::from(at));
        self.pointer = location;
        self.pointer_target = PointerTarget::Clients;
        let focus = self.pointer_surface_under(location);
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }
}

/// Press or release `keys` on the app-facing keyboard.
fn keys(state: &mut State, keys: &[u32], pressed: bool, serial: Serial, time: u32) {
    for &keycode in keys {
        state.key(keycode, pressed, serial, time);
    }
}

impl State {
    /// Aim `seat` at the window of `app` (the topmost one, the last that
    /// opened), or at nothing for a seat that acts on the whole screen.
    ///
    /// # Errors
    /// The window of that app is not open, or the compositor has no screen:
    /// the message says which, for the person and the agent's own log.
    pub(crate) fn seat_aim_app(&mut self, seat: Seat, app: Option<&str>) -> Result<Aim, String> {
        if !self.seats.has(seat) {
            return Err(NO_SEAT.to_owned());
        }
        let Some(app) = app else {
            if let Some(held) = self.seats.get_mut(seat) {
                held.aim = None;
            }
            return self.seat_view(seat).ok_or_else(|| NO_SEAT.to_owned());
        };
        let identifier = self
            .capture
            .shareable_windows()
            .into_iter()
            .filter(|(_, info)| info.app_id.as_deref() == Some(app))
            // A dialog stands in its window's place: the window itself is what an agent aims at.
            .filter(|(_, info)| info.parent.is_none())
            .map(|(identifier, _)| identifier)
            .next_back()
            .ok_or_else(|| {
                let mut open: Vec<String> = self
                    .capture
                    .shareable_windows()
                    .into_iter()
                    .filter_map(|(_, info)| info.app_id)
                    .collect();
                open.sort();
                open.dedup();
                format!(
                    "no window of {app} is open; open windows: [{}] (or none, for the whole \
                     screen)",
                    open.join(", ")
                )
            })?;
        self.seat_aim_window(seat, &identifier)
    }

    /// Aim `seat` at the window a foreign-toplevel `identifier` names, as a share of that window
    /// does.
    pub(crate) fn seat_aim_window(&mut self, seat: Seat, identifier: &str) -> Result<Aim, String> {
        let window = self
            .capture
            .window(identifier)
            .ok_or_else(|| format!("no window is open by the identifier {identifier:?}"))?;
        let rect = self
            .space
            .element_geometry(window)
            .ok_or_else(|| format!("the window {identifier:?} is not on the desk"))?;
        let info = window
            .toplevel()
            .map(|toplevel| crate::state::window_info(&self.display, &self.icons, toplevel));
        let id = info.map_or(0, |info| info.id);
        let aim = WindowAim {
            id,
            identifier: identifier.to_owned(),
            rect: (
                f64::from(rect.loc.x),
                f64::from(rect.loc.y),
                f64::from(rect.size.w),
                f64::from(rect.size.h),
            ),
        };
        let Some(held) = self.seats.get_mut(seat) else {
            return Err(NO_SEAT.to_owned());
        };
        if held.pointer.is_none() {
            // A seat that has not moved yet stands in the window's top-left corner, where its first
            // coordinate is a pixel of the window.
            held.pointer = Some((aim.rect.0, aim.rect.1));
        }
        held.aim = Some(aim);
        self.seat_view(seat).ok_or_else(|| NO_SEAT.to_owned())
    }

    /// What `seat` aims at now.
    pub(crate) fn seat_view(&self, seat: Seat) -> Option<Aim> {
        self.seats.get(seat).map(|held| self.seat_view_of(held))
    }

    fn seat_view_of(&self, held: &Held) -> Aim {
        let geometries = self.output_geometries();
        let screen = geometries.iter().fold((0, 0), |(right, bottom), rect| {
            (
                right.max(rect.loc.x.saturating_add(rect.size.w)),
                bottom.max(rect.loc.y.saturating_add(rect.size.h)),
            )
        });
        Aim {
            window: held.aim.clone(),
            screen: (f64::from(screen.0), f64::from(screen.1)),
        }
    }
}

/// What an op is told when its seat has gone: no op answers for a seat the person's Stop ended.
const NO_SEAT: &str = "this seat has ended";
