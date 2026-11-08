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
//!
//! A window whose surface carries an HDR signal is drawn as the
//! tone-mapped rendition of its content, not as its own buffer, so its
//! close animates that rendition: the same bytes on screen, kept by the
//! HDR path ([`crate::hdr`]) rather than imported from the surface.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::PoisonError;

use smithay::backend::renderer::ImportMem;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
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
    /// The frame of a surface that carries an HDR signal: its rendition,
    /// which is what the desktop drew.
    rendition: Option<MemoryRenderBuffer>,
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

    /// Keep the frame `window` just drew under `renderer` , replacing the one kept before; false
    /// when the window draws nothing (yet, or under this renderer).
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
            self.rendition = None;
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

    /// Keep the rendition `buffer` of a surface that carries an HDR signal as the window's last
    /// frame: its logical size is `size` .
    pub(crate) fn record_rendition(
        &mut self,
        buffer: MemoryRenderBuffer,
        size: Size<i32, Logical>,
    ) {
        self.size = size;
        self.frames.clear();
        self.rendition = Some(buffer);
    }

    /// The kept frame as an element at `location` (physical), for a renderer that drew the window;
    /// `None` when this renderer never did.
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

    /// The kept rendition at `location` (physical), when the window's surface carried an HDR
    /// signal.
    pub(crate) fn rendition<R>(
        &self,
        renderer: &mut R,
        location: Point<f64, Physical>,
        alpha: f32,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Clone + Send + 'static,
    {
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location,
            self.rendition.as_ref()?,
            Some(alpha),
            None,
            Some(self.size),
            Kind::Unspecified,
        )
        .ok()
    }
}

/// The frames the windows last drew, by window id.
#[derive(Default)]
pub(crate) struct Snapshots(RefCell<HashMap<u64, Snapshot>>);

impl Snapshots {
    /// Keep the frame the window `id` just drew under `renderer` .
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

    /// Keep what the window `id` of `window` drew this frame: the texture of its surface, or, when
    /// its surface carries an HDR signal, the rendition the desktop drew it as.
    pub(crate) fn record_shown<R>(
        &self,
        id: u64,
        hdr: &crate::hdr::HdrSurfaces,
        renderer: &R,
        window: &Window,
    ) where
        R: Renderer,
        R::TextureId: Clone + 'static,
    {
        match hdr.shown(id) {
            Some((rendition, size)) => self.record_rendition(id, rendition, size),
            None => self.record(id, renderer, window),
        }
    }

    /// Keep the rendition `buffer` as the frame of the window `id` , of logical size `size` .
    pub(crate) fn record_rendition(
        &self,
        id: u64,
        buffer: MemoryRenderBuffer,
        size: Size<i32, Logical>,
    ) {
        self.0
            .borrow_mut()
            .entry(id)
            .or_default()
            .record_rendition(buffer, size);
    }

    /// Take the frame kept for `id` , if there is one.
    pub(crate) fn take(&self, id: u64) -> Option<Snapshot> {
        self.0.borrow_mut().remove(&id)
    }

    /// Forget `id` : no frame is kept for it.
    pub(crate) fn forget(&self, id: u64) {
        self.0.borrow_mut().remove(&id);
    }
}
