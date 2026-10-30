//! The last frame a window drew, kept so its close can be animated.
//!
//! The desktop animates a closed window from the frame it last showed
//! ([`crate::Compositor::closing_windows`]). A client that closes its
//! window destroys its surface as it goes, and with the surface goes the
//! buffer and the textures the compositor drew from it; the frame is kept
//! here, as the window's own texture, while the window still stands, so
//! the animation has something to draw afterwards.
//!
//! One frame is kept per window per renderer, and only replaced when the
//! window commits a new one. It is the texture the surface itself holds,
//! so nothing is copied, and one frame's worth of buffer stays alive per
//! window until the desktop lets the window go.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::PoisonError;

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::texture::{TextureRenderBuffer, TextureRenderElement};
use smithay::backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData};
use smithay::backend::renderer::{ErasedContextId, Renderer};
use smithay::desktop::Window;
use smithay::utils::{Logical, Physical, Point, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus as _;

/// One window's last frame.
#[derive(Default)]
pub(crate) struct Snapshot {
    /// The window's logical size when it drew it.
    size: Size<i32, Logical>,
    /// The frame itself, one per renderer.
    frames: HashMap<ErasedContextId, Frame>,
}

/// A texture a window drew, and the commit it drew.
struct Frame {
    commit: CommitCounter,
    buffer: Box<dyn Any>,
}

impl Snapshot {
    /// The window's logical size.
    pub(crate) fn size(&self) -> Size<i32, Logical> {
        self.size
    }

    /// Keep the frame `window` just drew under `renderer`, replacing the
    /// one kept before; false when the window draws nothing (yet, or
    /// under this renderer).
    pub(crate) fn record<R>(&mut self, renderer: &R, window: &Window) -> bool
    where
        R: Renderer,
        R::TextureId: Clone + 'static,
    {
        let Some(surface) = window.wl_surface() else {
            return false;
        };
        let erased = renderer.context_id().erased();
        with_states(&surface, |states| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return false;
            };
            let state = data.lock().unwrap_or_else(PoisonError::into_inner);
            let commit = state.current_commit();
            if self
                .frames
                .get(&erased)
                .is_some_and(|frame| frame.commit == commit)
            {
                return true;
            }
            let Some(texture) = state.texture(renderer.context_id()).cloned() else {
                return false;
            };
            self.size = state.surface_size().unwrap_or_default();
            self.frames.insert(
                erased,
                Frame {
                    commit,
                    buffer: Box::new(TextureRenderBuffer::from_texture(
                        renderer,
                        texture,
                        state.buffer_scale(),
                        state.buffer_transform(),
                        None,
                    )),
                },
            );
            true
        })
    }

    /// The kept frame as an element at `location` (physical), for a
    /// renderer that drew the window; `None` when this renderer never did.
    pub(crate) fn element<R>(
        &self,
        renderer: &R,
        location: Point<f64, Physical>,
        alpha: f32,
    ) -> Option<TextureRenderElement<R::TextureId>>
    where
        R: Renderer,
        R::TextureId: Clone + 'static,
    {
        let frame = self.frames.get(&renderer.context_id().erased())?;
        let buffer = frame
            .buffer
            .downcast_ref::<TextureRenderBuffer<R::TextureId>>()?;
        Some(TextureRenderElement::from_texture_render_buffer(
            location,
            buffer,
            Some(alpha),
            None,
            None,
            Kind::Unspecified,
        ))
    }
}

/// The frames the windows last drew, by window id.
#[derive(Default)]
pub(crate) struct Snapshots(RefCell<HashMap<u64, Snapshot>>);

impl Snapshots {
    /// Keep the frame the window `id` just drew under `renderer`.
    pub(crate) fn record<R>(&self, id: u64, renderer: &R, window: &Window)
    where
        R: Renderer,
        R::TextureId: Clone + 'static,
    {
        self.0
            .borrow_mut()
            .entry(id)
            .or_default()
            .record(renderer, window);
    }

    /// Take the frame kept for `id`, if there is one.
    pub(crate) fn take(&self, id: u64) -> Option<Snapshot> {
        self.0.borrow_mut().remove(&id)
    }

    /// Forget `id`: no frame is kept for it.
    pub(crate) fn forget(&self, id: u64) {
        self.0.borrow_mut().remove(&id);
    }
}
