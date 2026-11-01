//! The overview's pictures of the desk's windows, and the clock they fly on.
//!
//! The chrome draws the overview, but the windows it shows belong to other
//! clients: only the compositor holds their buffers. The chrome's panel
//! names each window it pictures and where its picture rests
//! ([`WindowFlight::show`]); the compositor draws every named window's
//! latest buffer there. This is the host's side of that contract: it keeps
//! the clock the pictures fly on when the overview opens and closes, tells
//! the panel, and hands the compositor each window's live picture at the
//! rect the panel would draw it in.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use tilo_compositor::WindowPicture;
use tilo_desktop::window_flight::{WindowFlight, WindowPicture as Flying};
use tilo_desktop_model::{TabId, TileContent};
use ui::kurbo::Rect;
use ui::motion::{Presence, Zoom, input_source, unit_opacity};
use ui::tokens::EffectiveMotion;

/// The overview's flight, shared by the session, the chrome and the
/// compositor it hands pictures to.
#[derive(Clone)]
pub(crate) struct Overview {
    inner: Rc<Inner>,
}

struct Inner {
    flight: WindowFlight,
    /// How far the overview has arrived, or how far it has gone.
    zoom: RefCell<Presence>,
    /// Whether the chrome's overview is open, as it last drew: written by
    /// the chrome, read here to start the zoom.
    open: Rc<Cell<bool>>,
    /// The windows pictured now: their tab, the window it is, and the
    /// origin the chrome draws from (pictures are in its coordinates).
    shown: RefCell<Shown>,
    /// The motion the desk moves with, and whether it flies at all.
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
                open: Rc::default(),
                shown: RefCell::default(),
                motion,
                reduce_motion,
            }),
        }
    }

    /// The pictures the chrome's overview panel draws with.
    pub(crate) fn flight(&self) -> WindowFlight {
        self.inner.flight.clone()
    }

    /// Set as the chrome draws, telling the session whether the overview is
    /// open.
    pub(crate) fn open_flag(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.inner.open)
    }

    /// A frame is about to be drawn: move the zoom by the clock, and tell
    /// the panel where the windows it pictures stand on the desk now.
    ///
    /// The desk says where they stand, in its own coordinates, and where
    /// those begin on the desktop, which the chrome shares.
    pub(crate) fn step(&self, shared: &crate::Shared, now: Instant) {
        let inner = &*self.inner;
        let open = inner.open.get();
        {
            let mut zoom = inner.zoom.borrow_mut();
            zoom.show(open, Zoom::OVERVIEW, inner.motion, input_source(), now);
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
        let shown = shared.desktop.with_policy(|policy| policy.layout().shown);
        let origin = shared.desktop.with_widgets(|host| host.placement().origin);
        let windows = shared.handle.read(window_tabs);
        let (ox, oy) = origin;
        let mut pictured = HashMap::new();
        let mut sources = HashMap::new();
        for &(window, rect) in &shown {
            let Some(&tab) = windows.get(&window) else {
                continue;
            };
            pictured.insert(tab, window);
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

    /// The pictures the compositor draws now: each pictured window's live
    /// buffer where the panel draws its picture at this moment of the
    /// flight.
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

/// Every window the tree holds a tab of, by the window's id.
fn window_tabs(tree: &tilo_desktop_model::DesktopTree) -> HashMap<u64, TabId> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .filter_map(|tab| match &tab.tile {
            TileContent::Foreign { surface_id, .. } => Some((*surface_id, tab.id)),
            _ => None,
        })
        .collect()
}

impl crate::Shared {
    /// The windows the overview pictures as the compositor draws them now:
    /// each window's id and the rect its picture rests at, logical
    /// `(x, y, width, height)`, bottom first.
    pub(crate) fn overview_pictures(&self) -> Vec<(u64, (f64, f64, f64, f64))> {
        self.overview
            .pictures()
            .into_iter()
            .map(|picture| (picture.window, picture.rect))
            .collect()
    }
}
