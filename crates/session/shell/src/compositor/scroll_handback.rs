//! Sideways scrolls a window has no use for move its strip.
//!
//! A two-finger sideways swipe or a tilted wheel over a window reaches that
//! window first, as any scroll does: a spreadsheet, a map or a wide table
//! scrolls with it, and a browser goes back with it. Only the application
//! knows whether it used one, so a client that binds
//! `tilo_scroll_manager_v1` (every app built on Tilo's toolkit, through
//! `ui::gesture::SidewaysScroll`) says so: it hands back the first event of a
//! sideways finger scroll, or a sideways wheel notch, that nothing in it
//! used. The compositor then ends the scroll on that window
//! (`wl_pointer.axis_stop`) and pans the window's strip with the whole of it,
//! the same direct manipulation as a three-finger swipe; a handed-back notch
//! steps along the strip a window, as Alt+Shift+wheel does.
//!
//! Every other Wayland client (Chromium, GTK, Qt, Electron) keeps every
//! horizontal scroll. Nothing on the wire says whether such a client used
//! one, and taking sideways scrolls from an app that scrolls or navigates
//! with them would break it; three fingers pan the strip over any window.
//!
//! The decision is made once per scroll and the scroll then belongs to one
//! owner to its end: it never moves the window and the strip at once.

use std::collections::HashMap;

use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use winit::event::{MouseScrollDelta, TouchPhase};

use super::state::State;
use crate::compositor::frame::FrameBuilder;
use crate::shell::state::StripId;

pub mod protocol {
    use smithay::reexports::wayland_server;
    use wayland_server::protocol::wl_surface;
    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        use smithay::reexports::wayland_server::protocol::__interfaces::{
            WL_SURFACE_INTERFACE, wl_surface_interface,
        };
        wayland_scanner::generate_interfaces!(
            "../../../vendor/winit/src/platform_impl/linux/wayland/types/tilo_scroll.xml"
        );
    }
    use self::__interfaces::{TILO_SCROLL_MANAGER_V1_INTERFACE, TILO_SCROLL_SURFACE_V1_INTERFACE};
    wayland_scanner::generate_server_code!(
        "../../../vendor/winit/src/platform_impl/linux/wayland/types/tilo_scroll.xml"
    );
}
use protocol::tilo_scroll_manager_v1::{self, TiloScrollManagerV1};
use protocol::tilo_scroll_surface_v1::{self, TiloScrollSurfaceV1};

/// Who a sideways finger scroll in progress belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Owner {
    /// The window under the pointer, which may still hand it back.
    Window,
    /// The strip, which the scroll pans.
    Strip,
}

/// One finger (or continuous) scroll over a window, from its first event to
/// the fingers lifting.
#[derive(Clone, Copy, Debug)]
struct Scroll {
    surface: u64,
    strip: Option<StripId>,
    owner: Owner,
    /// Horizontal travel so far, positive toward the strip's later windows.
    travel: f64,
    /// When its last event arrived.
    last: std::time::Instant,
}

/// A pause this long between finger-scroll events starts a new scroll, for
/// a source that reports no phases; the toolkit applies the same rule.
const IDLE: std::time::Duration = std::time::Duration::from_millis(200);

/// The surfaces that hand back sideways scrolls, and the scroll in progress.
#[derive(Default)]
pub struct ScrollHandback {
    /// Surface protocol id by the `tilo_scroll_surface_v1` speaking for it.
    surfaces: HashMap<ObjectId, u64>,
    /// The finger scroll in progress over a window that hands back.
    scroll: Option<Scroll>,
    /// A sideways wheel notch delivered to a window that hands back, not yet
    /// answered: its surface, strip and direction along the strip.
    notch: Option<(u64, Option<StripId>, i32)>,
    /// Surfaces that handed back since the last turn of the run loop.
    handed_back: Vec<u64>,
}

impl ScrollHandback {
    pub(crate) fn register(display: &DisplayHandle) {
        display.create_global::<State, TiloScrollManagerV1, _>(1, ());
    }

    fn hands_back(&self, surface: u64) -> bool {
        self.surfaces.values().any(|&id| id == surface)
    }
}

/// Travel toward the strip's later windows for a scroll of `delta`. Winit's
/// deltas are negative for fingers moving content toward its later columns,
/// the way `Super+Right` goes.
fn travel_of(delta: MouseScrollDelta) -> (f64, f64) {
    match delta {
        MouseScrollDelta::PixelDelta(p) => (-p.x, -p.y),
        MouseScrollDelta::LineDelta(x, y) => (-f64::from(x), -f64::from(y)),
    }
}

/// What to do with one scroll event over the window `surface` in `strip`.
pub(crate) enum Route {
    /// Forward it to the window.
    Window,
    /// The strip took it: forward nothing.
    Strip,
}

impl State {
    /// Route one scroll event over the window `surface`, which sits in
    /// `strip`. A scroll the strip owns pans it here; anything else goes to
    /// the window, and a sideways scroll over a window that hands back is
    /// remembered so its answer can be acted on.
    pub(crate) fn route_window_scroll(
        &mut self,
        fb: &mut FrameBuilder,
        surface: u64,
        strip: Option<StripId>,
        delta: MouseScrollDelta,
        phase: TouchPhase,
    ) -> Route {
        let (dx, dy) = travel_of(delta);
        let handback = &mut self.scroll_handback;
        if let MouseScrollDelta::LineDelta(..) = delta {
            handback.notch = (handback.hands_back(surface) && dx.abs() > dy.abs())
                .then(|| (surface, strip, if dx > 0.0 { 1 } else { -1 }));
            return Route::Window;
        }
        let now = std::time::Instant::now();
        let begins = phase == TouchPhase::Started
            || handback.scroll.is_none_or(|scroll| {
                scroll.surface != surface || now.saturating_duration_since(scroll.last) > IDLE
            });
        if begins {
            // A scroll the strip took and whose end never came ends now.
            if let Some(previous) = handback.scroll.take()
                && previous.owner == Owner::Strip
            {
                fb.host_mut().end_strip_pan_of(previous.strip, now);
            }
            handback.scroll = (handback.hands_back(surface)
                && !matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled))
            .then_some(Scroll {
                surface,
                strip,
                owner: Owner::Window,
                travel: 0.0,
                last: now,
            });
        }
        let Some(scroll) = handback.scroll.as_mut() else {
            return Route::Window;
        };
        scroll.travel += dx;
        scroll.last = now;
        let ends = matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled);
        let owner = scroll.owner;
        let (strip, travel) = (scroll.strip, scroll.travel);
        if ends {
            handback.scroll = None;
        }
        match owner {
            Owner::Window => Route::Window,
            Owner::Strip => {
                let host = fb.host_mut();
                if ends {
                    host.end_strip_pan_of(strip, now);
                } else {
                    host.update_strip_pan_of(strip, travel, false);
                }
                Route::Strip
            }
        }
    }

    /// Act on the scrolls windows handed back since the last turn: take a
    /// finger scroll in progress from its window and pan the strip with all
    /// of it, or step along the strip for a notch. Returns whether the strip
    /// moved.
    pub fn take_handed_back_scrolls(&mut self, fb: &mut FrameBuilder) -> bool {
        let handed_back = std::mem::take(&mut self.scroll_handback.handed_back);
        let mut moved = false;
        for surface in handed_back {
            if let Some(scroll) = self.scroll_handback.scroll.as_mut()
                && scroll.surface == surface
                && scroll.owner == Owner::Window
            {
                scroll.owner = Owner::Strip;
                let (strip, travel) = (scroll.strip, scroll.travel);
                // The window's scroll ends here; the rest is the strip's.
                self.stop_client_pointer_axis();
                moved |= fb.host_mut().update_strip_pan_of(strip, travel, true);
                continue;
            }
            if let Some((notch_surface, strip, step)) = self.scroll_handback.notch
                && notch_surface == surface
            {
                self.scroll_handback.notch = None;
                moved |= fb.host_mut().step_strip_of(strip, step);
            }
        }
        moved
    }
}

impl GlobalDispatch<TiloScrollManagerV1, ()> for State {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<TiloScrollManagerV1>,
        (): &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<TiloScrollManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        manager: &TiloScrollManagerV1,
        request: <TiloScrollManagerV1 as Resource>::Request,
        (): &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let tilo_scroll_manager_v1::Request::GetScrollSurface { id, surface } = request {
            // A client speaks only for its own surfaces.
            if surface
                .client()
                .is_none_or(|owner| owner.id() != client.id())
            {
                manager.post_error(0_u32, "a scroll surface must be the client's own");
                return;
            }
            let resource = init.init(id, ());
            state
                .scroll_handback
                .surfaces
                .insert(resource.id(), crate::compositor::state::surface_id(&surface));
        }
    }
}

impl Dispatch<TiloScrollSurfaceV1, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        resource: &TiloScrollSurfaceV1,
        request: <TiloScrollSurfaceV1 as Resource>::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let tilo_scroll_surface_v1::Request::HorizontalUnused = request
            && let Some(&surface) = state.scroll_handback.surfaces.get(&resource.id())
        {
            state.scroll_handback.handed_back.push(surface);
        }
    }

    fn destroyed(
        state: &mut Self,
        _: smithay::reexports::wayland_server::backend::ClientId,
        resource: &TiloScrollSurfaceV1,
        (): &(),
    ) {
        state.scroll_handback.surfaces.remove(&resource.id());
    }
}
