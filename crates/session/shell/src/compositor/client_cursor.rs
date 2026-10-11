//! The cursor the pointer-focused client asked for.
//!
//! A client names a shape (`wp_cursor_shape_v1`), hands over a cursor
//! surface (`wl_pointer.set_cursor`), or hides the pointer. Smithay reports
//! each request through `SeatHandler::cursor_image` and resets to the default
//! arrow whenever pointer focus leaves the client; this keeps the latest
//! request and turns a cursor surface into the bitmap every render path
//! draws.

use std::sync::Arc;

use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{self as wl_compositor, BufferAssignment, SurfaceAttributes};

use crate::compositor::cursor::{CursorBitmap, CursorShape, PointerImage};

pub struct ClientCursor {
    status: CursorImageStatus,
    /// The converted cursor surface, refreshed on each of its commits.
    bitmap: Option<Arc<CursorBitmap>>,
}

impl Default for ClientCursor {
    fn default() -> Self {
        Self {
            status: CursorImageStatus::default_named(),
            bitmap: None,
        }
    }
}

impl ClientCursor {
    /// Adopt a client's request; `output_scale` sizes a cursor surface.
    pub fn set(&mut self, status: CursorImageStatus, output_scale: f64) {
        self.status = status;
        self.refresh(output_scale);
    }

    /// Re-read the cursor surface after `surface` committed, if it is the
    /// one in use (animated cursors commit a new frame each step).
    pub fn surface_committed(&mut self, surface: &WlSurface, output_scale: f64) {
        if matches!(&self.status, CursorImageStatus::Surface(current) if current == surface) {
            self.refresh(output_scale);
        }
    }

    fn refresh(&mut self, output_scale: f64) {
        self.bitmap = match &self.status {
            CursorImageStatus::Surface(surface) => read_cursor_surface(surface, output_scale),
            CursorImageStatus::Hidden | CursorImageStatus::Named(_) => None,
        };
    }

    /// What the pointer should show while this client has pointer focus.
    /// A cursor surface without a buffer hides the pointer, as the protocol
    /// specifies.
    pub fn image(&self) -> PointerImage {
        match &self.status {
            CursorImageStatus::Hidden => PointerImage::Hidden,
            CursorImageStatus::Named(icon) => PointerImage::Shape(CursorShape::from_icon(*icon)),
            CursorImageStatus::Surface(_) => {
                self.bitmap.as_ref().map_or(PointerImage::Hidden, |bitmap| {
                    PointerImage::Client(Arc::clone(bitmap))
                })
            }
        }
    }

    /// Release the cursor surface's frame throttle once a frame showing it
    /// was presented, so animated cursors keep animating.
    pub fn send_frame_callbacks(&self, time_ms: u32) {
        let CursorImageStatus::Surface(surface) = &self.status else {
            return;
        };
        if !surface.is_alive() {
            return;
        }
        wl_compositor::with_states(surface, |states| {
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            for callback in attrs.current().frame_callbacks.drain(..) {
                callback.done(time_ms);
            }
        });
    }
}

fn read_cursor_surface(surface: &WlSurface, output_scale: f64) -> Option<Arc<CursorBitmap>> {
    if !surface.is_alive() {
        return None;
    }
    wl_compositor::with_states(surface, |states| {
        let hotspot = states
            .data_map
            .get::<CursorImageSurfaceData>()
            .map(|data| {
                data.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .hotspot
            })
            .unwrap_or_default();
        let scale = crate::compositor::state::committed_surface_scale(states);
        let mut attrs = states.cached_state.get::<SurfaceAttributes>();
        let current = attrs.current();
        let buffer = match &current.buffer {
            Some(BufferAssignment::NewBuffer(buffer)) => Some(buffer.clone()),
            Some(BufferAssignment::Removed) | None => None,
        };
        drop(attrs);
        let buffer = buffer?;
        let mut rgba = Vec::new();
        let (width, height) =
            match crate::compositor::surface_readback::buffer_to_rgba(&buffer, &mut rgba) {
                Ok(size) => size,
                Err(error) => {
                    tracing::debug!(%error, "client cursor surface is not readable; hiding it");
                    return None;
                }
            };
        // Buffer pixels per logical pixel: the buffer scale, or what a
        // viewport makes of the buffer (a fractionally scaled client).
        let density = scale.density(width, height).map_or(1.0, |(x, _)| x);
        CursorBitmap::from_client(
            &rgba,
            width,
            height,
            (hotspot.x, hotspot.y),
            density,
            output_scale,
        )
        .map(Arc::new)
    })
}
