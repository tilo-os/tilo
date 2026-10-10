//! GLES composition of CPU-rasterized retained desktop layers.
//!
//! Adapted from the preserved Orange Pi consumer (ee13ba84). Heap uploads
//! keep CPU memory private. The caller waits for the entire GPU submission
//! before any subsequent canvas write; per-canvas fences are not used.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "pixel rectangles and byte counts on a screen whose sides fit i32 and f64 exactly"
)]

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::{Dmabuf, DmabufFlags};
use smithay::backend::allocator::gbm::{GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{Bind, Frame, ImportMem, Renderer};
use smithay::reexports::drm as drm_re;
use smithay::utils::{Buffer as BufferCoord, Physical, Rectangle, Size, Transform};

use drm_re::control::{Device as ControlDevice, FbCmd2Flags, framebuffer};
use vello::FineDamageRect;

use crate::compositor::cursor::CursorBitmap;

/// A dup of the display card's fd, for GBM and framebuffer registration.
#[derive(Clone)]
struct CardFd(Arc<OwnedFd>);

impl AsFd for CardFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm_re::Device for CardFd {}
impl ControlDevice for CardFd {}

/// One layer as the compositor draws it.
pub struct LayerView<'a> {
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// Bytes per row of the tightly packed heap canvas.
    pub stride: usize,
    /// Bumped whenever `pixels` is reallocated.
    pub generation: u64,
    /// The layer covers every pixel it is drawn on.
    pub opaque: bool,
    /// Screen rectangles and how far right in the layer their pixels are.
    pub pieces: Vec<(FineDamageRect, u32)>,
    /// Per piece, the screen rectangles within it worth drawing: where the
    /// layer is not transparent, or for the bottom layer, where no opaque
    /// layer above hides it.
    pub draw: Vec<Vec<FineDamageRect>>,
    /// Layer regions written since the compositor last saw this layer, for
    /// uploads of heap pixels.
    pub written: &'a [FineDamageRect],
}

/// What one composite cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct ComposeStats {
    pub draws: usize,
    pub upload_px: u64,
    /// CPU time issuing the draws.
    pub record: Duration,
}

struct Target {
    dmabuf: Dmabuf,
    fb: framebuffer::Handle,
    _bo: smithay::reexports::gbm::BufferObject<()>,
    card: CardFd,
}

impl Drop for Target {
    fn drop(&mut self) {
        let _ = self.card.destroy_framebuffer(self.fb);
    }
}

struct LayerTexture {
    generation: u64,
    texture: GlesTexture,
}

/// The GLES compositor for one output.
pub struct GlCompositor {
    renderer: GlesRenderer,
    gbm: GbmDevice<CardFd>,
    card: CardFd,
    targets: Vec<Target>,
    size: (u32, u32),
    /// Slot KMS scans out, and the one queued to replace it.
    state: super::scanout::DirectScanoutState,
    composed: Option<usize>,
    layers: Vec<Option<LayerTexture>>,
    cursor: Option<(Arc<CursorBitmap>, GlesTexture)>,
    max_texture: u32,
}

impl std::fmt::Debug for GlCompositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlCompositor")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// Clear the whole bound buffer. The layers cover every pixel; clearing
/// first tells a tiling GPU not to load the buffer's old contents into its
/// tiles before drawing, a full extra read of the frame.
fn clear_whole(
    frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
) -> Result<(), String> {
    frame
        .with_context(|gl| {
            use smithay::backend::renderer::gles::ffi;
            // SAFETY: plain state calls on the context `render` made
            // current, with the scanout buffer bound.
            unsafe {
                gl.Disable(ffi::SCISSOR_TEST);
                gl.ClearColor(0.0, 0.0, 0.0, 1.0);
                gl.Clear(ffi::COLOR_BUFFER_BIT);
            }
        })
        .map_err(|error| format!("clear frame: {error}"))
}

/// The parts of piece `piece_index` of `view` to draw, relative to `dest`.
fn dest_parts(
    view: &LayerView<'_>,
    piece_index: usize,
    piece: FineDamageRect,
    dest: Rectangle<i32, Physical>,
) -> Vec<Rectangle<i32, Physical>> {
    match view.draw.get(piece_index) {
        Some(parts) => parts
            .iter()
            .map(|part| rect(part.x - piece.x, part.y - piece.y, part.width, part.height))
            .collect(),
        None => vec![Rectangle::from_size(dest.size)],
    }
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> Rectangle<i32, Physical> {
    Rectangle::new(
        (x as i32, y as i32).into(),
        (width as i32, height as i32).into(),
    )
}

impl GlCompositor {
    /// Open GLES on the display card `card` (a dup of its fd) and allocate
    /// two `size` scanout buffers.
    ///
    /// # Errors
    ///
    /// Fails when EGL, GLES or GBM scanout allocation is unavailable.
    pub fn new(card: OwnedFd, size: (u32, u32)) -> Result<Self, String> {
        let card = CardFd(Arc::new(card));
        let gbm =
            GbmDevice::new(card.clone()).map_err(|error| format!("gbm_create_device: {error}"))?;
        // SAFETY: the GBM device outlives the display: the display holds a clone.
        let display = unsafe { EGLDisplay::new(gbm.clone()) }
            .map_err(|error| format!("EGL display: {error}"))?;
        let context = EGLContext::new(&display).map_err(|error| format!("EGL context: {error}"))?;
        // One unshared context draws everything, so textures need no fence
        // per draw to order them against other contexts. Such a fence makes
        // panfrost submit every draw as its own batch, each reloading and
        // writing back the whole framebuffer.
        // SAFETY: the context is current only on this thread, which owns the renderer.
        let capabilities = unsafe { GlesRenderer::supported_capabilities(&context) }
            .map_err(|error| format!("GLES capabilities: {error}"))?
            .into_iter()
            .filter(|capability| {
                *capability != smithay::backend::renderer::gles::Capability::Fencing
            });
        // SAFETY: as above.
        let mut renderer = unsafe { GlesRenderer::with_capabilities(context, capabilities) }
            .map_err(|error| format!("GLES renderer: {error}"))?;
        let max_texture = renderer
            .with_context(|gl| {
                let mut max = 0;
                // SAFETY: a single integer query on the current context.
                unsafe {
                    gl.GetIntegerv(
                        smithay::backend::renderer::gles::ffi::MAX_TEXTURE_SIZE,
                        &raw mut max,
                    );
                };
                u32::try_from(max).unwrap_or(0)
            })
            .map_err(|error| format!("GLES query: {error}"))?;
        let mut compositor = Self {
            renderer,
            gbm,
            card,
            targets: Vec::new(),
            size: (0, 0),
            state: super::scanout::DirectScanoutState::new(0),
            composed: None,
            layers: Vec::new(),
            cursor: None,
            max_texture,
        };
        let allocated = compositor.allocate_targets(size);
        let released = compositor
            .renderer
            .egl_context()
            .unbind()
            .map_err(|error| format!("release GLES context: {error}"));
        allocated?;
        released?;
        Ok(compositor)
    }

    /// Largest texture side this compositor can sample.
    pub fn max_layer_width(&self) -> u32 {
        self.max_texture
    }

    fn allocate_targets(&mut self, size: (u32, u32)) -> Result<(), String> {
        let mut targets = Vec::with_capacity(2);
        for index in 0..2 {
            let bo = self
                .gbm
                .create_buffer_object::<()>(
                    size.0,
                    size.1,
                    Fourcc::Xrgb8888,
                    GbmBufferFlags::SCANOUT | GbmBufferFlags::RENDERING,
                )
                .map_err(|error| format!("GBM scanout buffer[{index}]: {error}"))?;
            let fd = bo
                .fd_for_plane(0)
                .map_err(|error| format!("export scanout buffer[{index}]: {error}"))?;
            let modifier = bo.modifier();
            let mut builder = Dmabuf::builder(
                (size.0 as i32, size.1 as i32),
                Fourcc::Xrgb8888,
                modifier,
                DmabufFlags::empty(),
            );
            builder.add_plane(fd, 0, bo.offset(0), bo.stride_for_plane(0));
            let dmabuf = builder
                .build()
                .ok_or_else(|| format!("scanout buffer[{index}] has no plane"))?;
            let flags = if modifier == Modifier::Invalid {
                FbCmd2Flags::empty()
            } else {
                FbCmd2Flags::MODIFIERS
            };
            let fb = self
                .gbm
                .add_planar_framebuffer(&bo, flags)
                .map_err(|error| format!("framebuffer for scanout buffer[{index}]: {error}"))?;
            targets.push(Target {
                dmabuf,
                fb,
                _bo: bo,
                card: self.card.clone(),
            });
        }
        self.release_targets();
        self.targets = targets;
        self.size = size;
        self.state = super::scanout::DirectScanoutState::new(0);
        self.composed = None;
        Ok(())
    }

    fn release_targets(&mut self) {
        self.targets.clear();
    }

    /// KMS shows a buffer of its own again: none of ours is on screen.
    pub fn forget_scanout(&mut self) {
        self.state = super::scanout::DirectScanoutState::new(0);
        self.composed = None;
    }

    /// The flip queued by [`Self::compose`]'s caller completed.
    pub fn flip_completed(&mut self) {
        self.state.complete_flip();
    }

    /// The queued flip was refused: the slot is free again.
    pub fn flip_refused(&mut self) {
        self.composed = None;
    }

    /// Transfer the completed slot only after the kernel accepted its flip.
    pub fn flip_accepted(&mut self) -> bool {
        self.composed
            .take()
            .is_some_and(|slot| self.state.submit(slot))
    }

    fn free_slot(&self) -> Option<usize> {
        self.composed
            .is_none()
            .then(|| self.state.render_slot())
            .flatten()
    }

    fn layer_texture(
        &mut self,
        index: usize,
        view: &LayerView<'_>,
        stats: &mut ComposeStats,
    ) -> Result<GlesTexture, String> {
        if self.layers.len() <= index {
            self.layers.resize_with(index + 1, || None);
        }
        if let Some(layer) = self.layers.get(index).and_then(Option::as_ref)
            && layer.generation == view.generation
        {
            let texture = layer.texture.clone();
            for written in view.written {
                let region = Rectangle::<i32, BufferCoord>::new(
                    (written.x as i32, written.y as i32).into(),
                    (written.width as i32, written.height as i32).into(),
                );
                self.renderer
                    .update_memory(&texture, view.pixels, region)
                    .map_err(|error| format!("layer upload: {error}"))?;
                stats.upload_px += u64::from(written.width) * u64::from(written.height);
            }
            return Ok(texture);
        }
        stats.upload_px += u64::from(view.width) * u64::from(view.height);
        let padded: Size<i32, BufferCoord> = ((view.stride / 4) as i32, view.height as i32).into();
        let texture = self
            .renderer
            .import_memory(view.pixels, Fourcc::Abgr8888, padded, false)
            .map_err(|error| format!("layer upload: {error}"))?;
        if let Some(slot) = self.layers.get_mut(index) {
            *slot = Some(LayerTexture {
                generation: view.generation,
                texture: texture.clone(),
            });
        }
        Ok(texture)
    }

    fn cursor_texture(&mut self, bitmap: &Arc<CursorBitmap>) -> Result<GlesTexture, String> {
        if let Some((cached, texture)) = &self.cursor
            && Arc::ptr_eq(cached, bitmap)
        {
            return Ok(texture.clone());
        }
        let texture = self
            .renderer
            .import_memory(
                bitmap.rgba(),
                Fourcc::Abgr8888,
                (bitmap.width as i32, bitmap.height as i32).into(),
                false,
            )
            .map_err(|error| format!("cursor upload: {error}"))?;
        self.cursor = Some((bitmap.clone(), texture.clone()));
        Ok(texture)
    }

    /// Composite `layers` bottom to top, then the pointer, into a free
    /// scanout buffer. Returns the framebuffer to flip to and the point the
    /// GPU will have finished at. KMS waits for the drawing itself (the
    /// buffer carries the GPU's implicit fence), so the flip can be queued
    /// at once; the layers must stay unchanged until the GPU is done.
    ///
    /// # Errors
    ///
    /// Fails when a texture cannot be made or the GPU work fails.
    pub fn compose(
        &mut self,
        layers: &[LayerView<'_>],
        cursor: Option<(&Arc<CursorBitmap>, i32, i32)>,
    ) -> Result<(framebuffer::Handle, ComposeStats, SyncPoint), String> {
        let result = self.compose_current(layers, cursor);
        let released = self
            .renderer
            .egl_context()
            .unbind()
            .map_err(|error| format!("release GLES context: {error}"));
        released?;
        result
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "`finish` consumes the frame; only an error returns before it, dropping the frame there"
    )]
    fn compose_current(
        &mut self,
        layers: &[LayerView<'_>],
        cursor: Option<(&Arc<CursorBitmap>, i32, i32)>,
    ) -> Result<(framebuffer::Handle, ComposeStats, SyncPoint), String> {
        let started = Instant::now();
        let mut stats = ComposeStats::default();
        let slot = self.free_slot().ok_or("no free scanout buffer")?;
        let mut textures = Vec::with_capacity(layers.len());
        for (index, view) in layers.iter().enumerate() {
            textures.push(self.layer_texture(index, view, &mut stats)?);
        }
        let pointer = match cursor {
            Some((bitmap, x, y)) => Some((
                self.cursor_texture(bitmap)?,
                bitmap.width,
                bitmap.height,
                x,
                y,
            )),
            None => None,
        };
        let (width, height) = self.size;
        let Self {
            renderer, targets, ..
        } = self;
        let target = targets.get_mut(slot).ok_or("scanout slot vanished")?;
        let fb = target.fb;
        let mut bound = renderer
            .bind(&mut target.dmabuf)
            .map_err(|error| format!("bind scanout buffer: {error}"))?;
        let mut frame = renderer
            .render(
                &mut bound,
                (width as i32, height as i32).into(),
                Transform::Normal,
            )
            .map_err(|error| format!("begin frame: {error}"))?;
        clear_whole(&mut frame)?;
        for (index, (view, texture)) in layers.iter().zip(&textures).enumerate() {
            for (piece_index, (piece, offset)) in view.pieces.iter().enumerate() {
                if piece.width == 0 || piece.height == 0 {
                    continue;
                }
                let dest = rect(piece.x, piece.y, piece.width, piece.height);
                let src = Rectangle::<f64, BufferCoord>::new(
                    (f64::from(piece.x + offset), f64::from(piece.y)).into(),
                    (f64::from(piece.width), f64::from(piece.height)).into(),
                );
                let parts = dest_parts(view, piece_index, *piece, dest);
                if parts.is_empty() {
                    continue;
                }
                let opaque: &[Rectangle<i32, Physical>] = if view.opaque { &parts } else { &[] };
                frame
                    .render_texture_from_to(
                        texture,
                        src,
                        dest,
                        &parts,
                        opaque,
                        Transform::Normal,
                        1.0,
                        None,
                        &[],
                    )
                    .map_err(|error| format!("draw layer {index}: {error}"))?;
                stats.draws += 1;
            }
        }
        if let Some((texture, cw, ch, x, y)) = pointer {
            let dest =
                Rectangle::<i32, Physical>::new((x, y).into(), (cw as i32, ch as i32).into());
            let src =
                Rectangle::<f64, BufferCoord>::from_size((f64::from(cw), f64::from(ch)).into());
            frame
                .render_texture_from_to(
                    &texture,
                    src,
                    dest,
                    &[Rectangle::from_size(dest.size)],
                    &[],
                    Transform::Normal,
                    1.0,
                    None,
                    &[],
                )
                .map_err(|error| format!("draw pointer: {error}"))?;
            stats.draws += 1;
        }
        let sync = frame
            .finish()
            .map_err(|error| format!("finish frame: {error}"))?;
        drop(bound);
        stats.record = started.elapsed();
        self.composed = Some(slot);
        Ok((fb, stats, sync))
    }
}

impl Drop for GlCompositor {
    fn drop(&mut self) {
        self.release_targets();
    }
}
