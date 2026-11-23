//! The overview's pictures of the shell's windows, and the clock they fly on.
//! The chrome draws the overview, but the windows it shows belong to other clients: only the
//! compositor holds their buffers. The chrome's panel names each window it pictures and where its
//! picture rests ([`WindowFlight::show`]); the compositor draws every named window's latest buffer
//! there. This is the host's side of that contract: it keeps the clock the pictures fly on when the
//! overview opens and closes, tells the panel, and hands the compositor each window's live picture
//! at the rect the panel would draw it in.

#![cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page mounts this with the chrome")
)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use tilo_clock::time::{Duration, Instant};

use tilo_desktop::window_flight::{WindowFlight, WindowPicture as Flying};
use tilo_desktop_model::TabId;
use tilo_session_core::surface::WindowPicture;
use ui::kurbo::Rect;
use ui::motion::{Presence, Zoom, input_source, unit_opacity};
use ui::tokens::EffectiveMotion;

/// The overview's flight, shared by the session, the chrome and the compositor it hands pictures
/// to.
#[derive(Clone)]
pub(crate) struct Overview {
    inner: Rc<Inner>,
}

struct Inner {
    flight: WindowFlight,
    /// How far the overview has arrived, or how far it has gone.
    zoom: RefCell<Presence>,
    /// When the zoom was last stepped while it moved; `None` while it rests.
    stepped: Cell<Option<Instant>>,
    /// Whether the chrome's overview is open, as it last drew: written by the chrome, read here to
    /// start the zoom.
    open: Rc<Cell<bool>>,
    /// The windows pictured now: their tab, the window it is, and the origin the chrome draws from
    /// (pictures are in its coordinates).
    shown: RefCell<Shown>,
    /// The surface that draws the overview, which its flight asks frames of.
    surface: Cell<Option<ui::motion::MotionSurface>>,
    /// The motion the shell moves with, and whether it flies at all.
    motion: EffectiveMotion,
    reduce_motion: bool,
}

#[derive(Default)]
struct Shown {
    windows: HashMap<TabId, u64>,
    origin: (i32, i32),
}

impl Overview {
    pub(crate) fn new(motion: EffectiveMotion, reduce_motion: bool) -> Self {
        Self {
            inner: Rc::new(Inner {
                flight: WindowFlight::new(),
                zoom: RefCell::new(Presence::closed()),
                stepped: Cell::new(None),
                open: Rc::default(),
                shown: RefCell::default(),
                surface: Cell::new(None),
                motion,
                reduce_motion,
            }),
        }
    }

    /// The pictures the chrome's overview panel draws with.
    pub(crate) fn flight(&self) -> WindowFlight {
        self.inner.flight.clone()
    }

    /// Set as the chrome draws, telling the session whether the overview is open.
    pub(crate) fn open_flag(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.inner.open)
    }

    /// The overview is drawn on `surface`: its flight asks frames of that surface alone, not of
    /// every surface on the thread.
    pub(crate) fn drawn_on(&self, surface: ui::motion::MotionSurface) {
        self.inner.surface.set(Some(surface));
    }

    /// The same flag, borrowed: for a reader that lives for one call.
    pub(crate) fn open_cell(&self) -> &Cell<bool> {
        &self.inner.open
    }

    /// A frame is about to be drawn: move the zoom by the clock, and tell the panel where the
    /// windows it pictures stand on the shell now.
    /// The shell says where they stand, in its own coordinates, and where those begin on the
    /// desktop, which the chrome shares.
    pub(crate) fn step(
        &self,
        handle: &tilo_desktop_surface::DesktopHandle,
        shell: &dyn crate::platform::surfaces::Shell,
        now: Instant,
    ) {
        let inner = &*self.inner;
        let open = inner.open.get();
        {
            let mut zoom = inner.zoom.borrow_mut();
            let mut show = || {
                let turned = zoom.is_open() != open;
                zoom.show(open, Zoom::OVERVIEW, inner.motion, input_source(), now);
                if turned {
                    inner.stepped.set(None);
                }
                hold_after_late_frame(&mut zoom, &inner.stepped, now);
            };
            match inner.surface.get() {
                Some(surface) => ui::motion::on_surface(surface, show),
                None => show(),
            }
            if !zoom.is_visible(now) && !open {
                // Nothing of it is on screen: no picture is owed.
                drop(zoom);
                inner.flight.clear();
                inner.shown.borrow_mut().windows.clear();
                inner.flight.set_clock(0.0, HashMap::new());
                return;
            }
        }
        let progress = inner.zoom.borrow().progress(now);
        // Where each window stands on the display it is shown on, while it is shown: the window
        // the overview flies from. A window of another strip or display is not there, so its
        // picture fades in where it rests instead.
        let shown = shell.shown();
        let origin = shell.origin();
        let surfaces = handle.read(crate::windows::window_surfaces);
        let (ox, oy) = origin;
        let mut pictured = HashMap::new();
        let mut sources = HashMap::new();
        // Every window the layout holds is pictured, not only the ones on the display in front:
        // the overview is the workspace's whole hierarchy, and a window on another strip or
        // another display shows its own pixels in its card as the windows in front do. Only a
        // window the compositor keeps nothing of is left to its app's mark.
        for (tab, window) in surfaces {
            pictured.insert(tab, window);
        }
        // Where the shown windows stand, so their pictures fly from there.
        let by_window: HashMap<u64, TabId> = pictured
            .iter()
            .map(|(tab, window)| (*window, *tab))
            .collect();
        for &(window, rect) in &shown {
            let Some(&tab) = by_window.get(&window) else {
                continue;
            };
            sources.insert(
                tab,
                Rect::new(
                    rect.x0 - f64::from(ox),
                    rect.y0 - f64::from(oy),
                    rect.x1 - f64::from(ox),
                    rect.y1 - f64::from(oy),
                ),
            );
        }
        inner.flight.set_clock(progress, sources);
        let mut state = inner.shown.borrow_mut();
        state.windows = pictured;
        state.origin = origin;
    }

    /// The pictures the compositor draws now: each pictured window's live buffer where the panel
    /// draws its picture at this moment of the flight.
    pub(crate) fn pictures(&self) -> Vec<WindowPicture> {
        let inner = &*self.inner;
        let flight = &inner.flight;
        let progress = flight.progress();
        if progress <= 0.0 && !inner.open.get() {
            return Vec::new();
        }
        let shown = inner.shown.borrow();
        let (ox, oy) = shown.origin;
        let whole = |rect: Rect| {
            (
                rect.x0 + f64::from(ox),
                rect.y0 + f64::from(oy),
                rect.width(),
                rect.height(),
            )
        };
        let clip = flight.clip().map(whole);
        flight
            .pictures()
            .iter()
            .filter_map(|picture: &Flying| {
                let window = *shown.windows.get(&picture.tab)?;
                let flown = picture.flies.then(|| flight.source(picture.tab)).flatten();
                let (rect, opacity) = match flown {
                    Some(from) if !inner.reduce_motion => {
                        (Zoom::rect_at(from, picture.rect, progress), picture.alpha)
                    }
                    _ => (picture.rect, picture.alpha * unit_opacity(progress)),
                };
                (opacity > 0.0).then(|| WindowPicture {
                    window,
                    rect: whole(rect),
                    opacity,
                    clip,
                })
            })
            .collect()
    }
}

/// The furthest one step moves the zoom: two refreshes of a 60 Hz display.
const LONGEST_STEP: Duration = Duration::from_millis(34);

/// Hold the moving `zoom` for what the loop took past [`LONGEST_STEP`] since its last step at
/// `stepped`, and note this step. A machine that draws its desktop on the CPU takes many refreshes
/// for one frame of the overview, the first one most: the zoom stands where its last frame showed
/// it instead of arriving in a jump, and is drawn at every step however slowly frames come.
fn hold_after_late_frame(zoom: &mut Presence, stepped: &Cell<Option<Instant>>, now: Instant) {
    if let Some(last) = stepped.get()
        && let Some(late) = now
            .saturating_duration_since(last)
            .checked_sub(LONGEST_STEP)
        && !late.is_zero()
    {
        zoom.defer(late);
    }
    stepped.set(zoom.is_moving(now).then_some(now));
}

impl crate::Shared {
    /// The windows the overview pictures as the compositor draws them now: each window's id and the
    /// rect its picture rests at, logical `(x, y, width, height)` , bottom first.
    pub(crate) fn overview_pictures(&self) -> Vec<(u64, (f64, f64, f64, f64))> {
        self.overview
            .pictures()
            .into_iter()
            .map(|picture| (picture.window, picture.rect))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tokens::MotionSource;

    /// The overview opened at `start`, its first step taken.
    fn opened(start: Instant) -> (Presence, Cell<Option<Instant>>) {
        let mut zoom = Presence::closed();
        let stepped = Cell::new(None);
        zoom.show(
            true,
            Zoom::OVERVIEW,
            EffectiveMotion::default(),
            MotionSource::Pointer,
            start,
        );
        hold_after_late_frame(&mut zoom, &stepped, start);
        (zoom, stepped)
    }

    tilo_test_env::scenario! {
        /// The overview's zoom on a machine that draws slowly.
        fn the_zoom_is_drawn_at_every_step_however_slowly_frames_come;

        phase a_late_frame_finds_the_zoom_where_the_last_one_left_it;
        // The first frame of the overview took most of a second to draw on the CPU: the zoom has
        // moved one step's worth when it is shown, not arrived.
        {
            let start = tilo_clock::now();
            let (mut zoom, stepped) = opened(start);
            let late = start + Duration::from_millis(870);
            hold_after_late_frame(&mut zoom, &stepped, late);
            let progress = zoom.progress(late);
            assert!(
                progress > 0.0 && progress < 0.5,
                "a frame 870 ms on shows the zoom one step on, at {progress}"
            );
            assert!(zoom.is_moving(late), "the zoom goes on from there");
        }

        phase frames_on_time_are_not_held;
        // A display that draws every refresh shows the zoom on its own clock.
        {
            let start = tilo_clock::now();
            let (mut zoom, stepped) = opened(start);
            let arrives = zoom.settles_at().expect("the zoom is under way");
            let mut now = start;
            while zoom.is_moving(now) {
                now += Duration::from_millis(16);
                hold_after_late_frame(&mut zoom, &stepped, now);
            }
            assert!(
                now <= arrives + Duration::from_millis(16),
                "the zoom arrives on its own clock: {:?} after it was due",
                now.saturating_duration_since(arrives)
            );
        }
    }
}
