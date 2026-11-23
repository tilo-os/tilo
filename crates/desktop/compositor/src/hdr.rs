//! Surfaces that say their content is HDR.
//!
//! A client can attach an image description to its surface
//! (`wp_color_management_v1`); when that description is an HDR signal (PQ,
//! HLG or scRGB-style linear light) the buffer holds values no 8-bit desktop
//! can show as they are. Such a surface is drawn two ways at once: the scene
//! gets the tone-mapped SDR rendition of its buffer, in place of the surface
//! itself and exactly where it is, while an [`HdrLayer`] carrying the signal
//! at the buffer's precision is recorded for the output pass that composites
//! it ([`tilo_color_pipeline::hdr_surface`]).
//!
//! The layer is found the way the SDR desktop finds anything else: by drawing
//! the surface's rendition as part of the scene. A presenter renders the
//! scene twice with the rendition replaced by opaque black ([`black`]) and by
//! the layer's weight channel colour ([`lit`]), and the difference between
//! the two is how much of the layer reaches each pixel — the window's rounded
//! corners, its effects and anything drawn over it included. Anything that
//! draws the scene without substituting it (captures, screenshots, a stream,
//! the desktop on an SDR output) shows the tone-mapped rendition, so no path
//! can show a hole.
//!
//! A surface whose description is absent or is 8-bit SDR keeps today's way:
//! it is drawn by its own buffer, untouched.
//!
//! An SDR desktop draws the rendition, so the luminance range the content was
//! mastered in decides how its highlights are compressed; on an HDR output
//! the layer carries the signal itself, so the same light is shown at its own
//! precision whatever it was mastered for.
//!
//! The plane is read from the surface's committed buffer, so only buffers the
//! CPU can read reach it: `wl_shm` 10-bit and half-float buffers, and
//! single-plane linear dma-bufs. A buffer stored in another layout keeps its
//! SDR rendition too, since the plane has to hold the same texels the
//! rendition draws, in the same order.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical and physical coordinates of outputs and windows, far from i32 limits"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "buffer extents, sample values and pixel counts: small, checked where they matter"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::PoisonError;

use smithay::backend::allocator::{Buffer as _, Fourcc};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData, SurfaceView};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::{PopupManager, Window};
use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use smithay::wayland::compositor::{TraversalAction, with_states, with_surface_tree_downward};
use smithay::wayland::seat::WaylandFocus as _;

use neighbour_wire::colorimetry::Colorimetry;
use neighbour_wire::hdr_metadata::Hdr10Metadata;
use tilo_color_pipeline::hdr_surface::{
    Affine, CompositeTarget, HdrLayer, HdrSurface, Rect, SurfaceSignal, composite_hdr_pixel,
    encode_srgb8, half_to_f32,
};
use tilo_color_pipeline::hdr_tonemap::HdrTonemapConfig;

use crate::render::OutputRenderElement;

/// One drawn piece of a window that carries an HDR signal: a surface as usual, or its rendition
/// where the described surface itself would be.
pub(crate) enum Piece<R: Renderer> {
    Surface(WaylandSurfaceRenderElement<R>),
    Rendered(MemoryRenderBufferRenderElement<R>),
}

impl<R: Renderer> From<WaylandSurfaceRenderElement<R>> for Piece<R> {
    fn from(element: WaylandSurfaceRenderElement<R>) -> Self {
        Self::Surface(element)
    }
}

impl<R> Piece<R>
where
    R: Renderer + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    /// The piece as the output's own scene element.
    pub(crate) fn into_output(self) -> OutputRenderElement<R> {
        match self {
            Self::Surface(element) => OutputRenderElement::Surface(element),
            Self::Rendered(element) => OutputRenderElement::Memory(element),
        }
    }
}

/// What a pass draws where an HDR surface's rendition goes.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Appearance {
    /// The rendition itself: the desktop as people see it.
    #[default]
    Rendered,
    /// Opaque black, to find what each layer covers.
    Black,
    /// The layer's weight channel, to find how much of it reaches a pixel.
    Lit,
}

#[derive(Clone, Default)]
struct Pass {
    appearance: Appearance,
    /// Which weight channel each plane is drawn in, by plane id.
    channels: Vec<(u64, u8)>,
}

thread_local! {
    static PASS: RefCell<Pass> = RefCell::new(Pass::default());
}

/// A pass that draws every HDR surface as something other than its rendition; the thread draws as
/// usual again where it ends.
#[must_use = "the pass lasts while the guard lives"]
pub(crate) struct Substitute {
    previous: Pass,
}

impl Drop for Substitute {
    fn drop(&mut self) {
        PASS.with(|pass| *pass.borrow_mut() = self.previous.clone());
    }
}

/// A pass that draws every HDR surface's rendition as opaque black: the SDR desktop with the layers
/// left out.
pub(crate) fn black() -> Substitute {
    substitute(Appearance::Black, &[])
}

/// A pass that draws each surface's rendition in its layer's weight channel colour (`channels`:
/// plane id and channel, from the recorded layers).
pub(crate) fn lit(channels: &[(u64, u8)]) -> Substitute {
    substitute(Appearance::Lit, channels)
}

fn substitute(appearance: Appearance, channels: &[(u64, u8)]) -> Substitute {
    let previous = PASS.with(|pass| pass.borrow().clone());
    PASS.with(|pass| {
        *pass.borrow_mut() = Pass {
            appearance,
            channels: channels.to_vec(),
        };
    });
    Substitute { previous }
}

fn appearance() -> Appearance {
    PASS.with(|pass| pass.borrow().appearance)
}

fn channel_of(plane: u64) -> Option<u8> {
    PASS.with(|pass| {
        pass.borrow()
            .channels
            .iter()
            .find(|(id, _)| *id == plane)
            .map(|(_, channel)| *channel)
    })
}

/// One window's HDR plane, kept until it commits again.
struct Kept {
    commit: CommitCounter,
    colorimetry: Colorimetry,
    /// The volume the content was mastered for, which its SDR rendition is tone-mapped from.
    tonemap: HdrTonemapConfig,
    /// The signal at the buffer's precision, and the SDR rendition.
    surface: Arc<HdrSurface>,
    /// The rendition as a buffer a renderer draws, built once per commit.
    rendition: MemoryRenderBuffer,
    /// Its logical size on screen.
    logical: Size<i32, Logical>,
}

/// The HDR planes of the windows that carry one, by window id.
#[derive(Default)]
pub(crate) struct HdrSurfaces(RefCell<HashMap<u64, Kept>>);

impl HdrSurfaces {
    /// The window's HDR plane: the one kept for its latest commit, or one built from the buffer
    /// that commit attached.
    pub(crate) fn surface(
        &self,
        id: u64,
        window: &Window,
        description: (Colorimetry, Option<Hdr10Metadata>),
    ) -> Option<Arc<HdrSurface>> {
        let (colorimetry, metadata) = description;
        // The luminance range the content was mastered in decides how it is compressed for an SDR
        // output; a client that named none gets the HDR10 baseline.
        let tonemap = HdrTonemapConfig::from_metadata(metadata);
        // An SDR description is what the 8-bit desktop already shows.
        let signal = SurfaceSignal::for_colorimetry(colorimetry, tonemap)?;
        let surface = window.wl_surface()?;
        let commit = with_states(&surface, |states| {
            let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
            let state = data.lock().unwrap_or_else(PoisonError::into_inner);
            Some(state.current_commit())
        })?;
        let mut known = self.0.borrow_mut();
        if let Some(kept) = known.get(&id)
            && kept.commit == commit
            && kept.colorimetry == colorimetry
            && kept.tonemap == tonemap
        {
            return Some(Arc::clone(&kept.surface));
        }
        let (surface, scale) = read(&surface, signal)?;
        let size = Size::<i32, Buffer>::from((
            i32::try_from(surface.width()).ok()?,
            i32::try_from(surface.height()).ok()?,
        ));
        let rendition = MemoryRenderBuffer::from_slice(
            surface.sdr(),
            Fourcc::Abgr8888,
            size,
            scale,
            Transform::Normal,
            Some(vec![Rectangle::from_size(size)]),
        );
        let logical = Size::<i32, Logical>::from((
            i32::try_from(surface.width()).ok()? / scale.max(1),
            i32::try_from(surface.height()).ok()? / scale.max(1),
        ));
        let surface = Arc::new(surface);
        known.insert(
            id,
            Kept {
                commit,
                colorimetry,
                tonemap,
                surface: Arc::clone(&surface),
                rendition,
                logical,
            },
        );
        Some(surface)
    }

    /// The rendition buffer kept for the plane `plane` a window draws, when it is that window's
    /// latest commit.
    fn rendition(&self, id: u64, plane: u64) -> Option<MemoryRenderBuffer> {
        let known = self.0.borrow();
        known
            .get(&id)
            .filter(|kept| kept.surface.plane().id() == plane)
            .map(|kept| kept.rendition.clone())
    }

    /// The rendition of the window `id` 's plane and its logical size: what the window shows on
    /// screen, for its close to animate.
    pub(crate) fn shown(&self, id: u64) -> Option<(MemoryRenderBuffer, Size<i32, Logical>)> {
        let known = self.0.borrow();
        let kept = known.get(&id)?;
        Some((kept.rendition.clone(), kept.logical))
    }

    /// Forget `id` : no plane is kept for it.
    pub(crate) fn forget(&self, id: u64) {
        self.0.borrow_mut().remove(&id);
    }
}

/// `surface` 's committed buffer as an HDR plane, and the buffer's scale.
fn read(surface: &WlSurface, signal: SurfaceSignal) -> Option<(HdrSurface, i32)> {
    with_states(surface, |states| {
        let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
        let state = data.lock().unwrap_or_else(PoisonError::into_inner);
        // The plane sits where the rendition is drawn, which a rotated buffer would move.
        if state.buffer_transform() != Transform::Normal {
            return None;
        }
        let scale = state.buffer_scale();
        let buffer = state.buffer()?;
        let (width, height, rgb) = pixels(buffer)?;
        drop(state);
        HdrSurface::from_signal_rgb(width, height, rgb, signal).map(|plane| (plane, scale))
    })
}

/// The buffer's samples as normalised signal values, when it holds them at a precision beyond 8
/// bits.
fn pixels(buffer: &WlBuffer) -> Option<(u32, u32, Vec<[f32; 3]>)> {
    if let Ok(read) = smithay::wayland::shm::with_buffer_contents(buffer, from_shm)
        && let Some(read) = read
    {
        return Some(read);
    }
    let dmabuf = smithay::wayland::dmabuf::get_dmabuf(buffer).ok()?;
    from_dmabuf(dmabuf)
}

/// How a buffer's words hold a signal.
#[derive(Clone, Copy)]
enum Layout {
    /// One word per pixel, ten bits each, red in the low bits (`Xbgr2101010`) or in the high ones
    /// (`Xrgb2101010`).
    Rgb10 { red_low: bool },
    /// Four IEEE half floats per pixel, red first.
    Half,
}

impl Layout {
    /// The layout of a 10-bit or half-float buffer format; `None` for a format whose samples are
    /// 8-bit or packed differently.
    fn of_fourcc(fourcc: Fourcc) -> Option<Self> {
        match fourcc {
            Fourcc::Xbgr2101010 | Fourcc::Abgr2101010 => Some(Self::Rgb10 { red_low: true }),
            Fourcc::Xrgb2101010 | Fourcc::Argb2101010 => Some(Self::Rgb10 { red_low: false }),
            Fourcc::Xrgb16161616f | Fourcc::Abgr16161616f => Some(Self::Half),
            _ => None,
        }
    }

    fn of_shm(format: wl_shm::Format) -> Option<Self> {
        match format {
            wl_shm::Format::Xbgr2101010 | wl_shm::Format::Abgr2101010 => {
                Some(Self::Rgb10 { red_low: true })
            }
            wl_shm::Format::Xrgb2101010 | wl_shm::Format::Argb2101010 => {
                Some(Self::Rgb10 { red_low: false })
            }
            wl_shm::Format::Xrgb16161616f | wl_shm::Format::Abgr16161616f => Some(Self::Half),
            _ => None,
        }
    }

    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb10 { .. } => 4,
            Self::Half => 8,
        }
    }

    /// The signal one pixel's bytes hold.
    fn signal(self, pixel: &[u8]) -> [f32; 3] {
        /// A 10-bit sample's full-scale value.
        const MAX_10: f32 = 1023.0;
        match self {
            Self::Rgb10 { red_low } => {
                let word = u32::from_le_bytes(pixel.try_into().unwrap_or_default());
                let low = |word: u32| (word & 0x3FF) as f32 / MAX_10;
                let middle = |word: u32| ((word >> 10) & 0x3FF) as f32 / MAX_10;
                let high = |word: u32| ((word >> 20) & 0x3FF) as f32 / MAX_10;
                if red_low {
                    [low(word), middle(word), high(word)]
                } else {
                    [high(word), middle(word), low(word)]
                }
            }
            Self::Half => [0_usize, 2, 4].map(|offset| {
                let bits = pixel
                    .get(offset..offset.saturating_add(2))
                    .and_then(|half| half.try_into().ok())
                    .map_or(0, u16::from_le_bytes);
                half_to_f32(bits)
            }),
        }
    }
}

/// Rows `stride` apart from `offset` , decoded as `layout` says.
fn decode(
    layout: Layout,
    data: &[u8],
    (width, height): (u32, u32),
    stride: usize,
    offset: usize,
) -> Option<(u32, u32, Vec<[f32; 3]>)> {
    let bpp = layout.bytes_per_pixel();
    let row = (width as usize).checked_mul(bpp)?;
    if width == 0 || height == 0 || row > stride {
        return None;
    }
    let end = offset
        .checked_add((height as usize).saturating_sub(1).checked_mul(stride)?)?
        .checked_add(row)?;
    if end > data.len() {
        return None;
    }
    let mut rgb = Vec::with_capacity((width as usize).checked_mul(height as usize)?);
    for row_start in (offset..end).step_by(stride.max(1)).take(height as usize) {
        let bytes = data.get(row_start..row_start.saturating_add(row))?;
        rgb.extend(bytes.chunks_exact(bpp).map(|pixel| layout.signal(pixel)));
    }
    Some((width, height, rgb))
}

/// A `wl_shm` buffer's samples; `None` for one that is not 10-bit or half-float.
fn from_shm(
    ptr: *const u8,
    len: usize,
    data: smithay::wayland::shm::BufferData,
) -> Option<(u32, u32, Vec<[f32; 3]>)> {
    let layout = Layout::of_shm(data.format)?;
    if data.width <= 0 || data.height <= 0 {
        return None;
    }
    // SAFETY: the shm pool hands out a readable mapping of `len` bytes for
    // the duration of the closure, which `decode` reads only within.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    decode(
        layout,
        bytes,
        (
            u32::try_from(data.width).ok()?,
            u32::try_from(data.height).ok()?,
        ),
        usize::try_from(data.stride).ok()?,
        usize::try_from(data.offset).ok()?,
    )
}

/// A single-plane linear dma-buf's samples, read through a mapping; `None` for a buffer the CPU
/// cannot read in place (tiled, inverted, multi-plane).
fn from_dmabuf(
    dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf,
) -> Option<(u32, u32, Vec<[f32; 3]>)> {
    let format = dmabuf.format();
    let layout = Layout::of_fourcc(format.code)?;
    if dmabuf.num_planes() != 1 || dmabuf.has_modifier() || dmabuf.y_inverted() {
        return None;
    }
    let size = dmabuf.size();
    if size.w <= 0 || size.h <= 0 {
        return None;
    }
    let (width, height) = (u32::try_from(size.w).ok()?, u32::try_from(size.h).ok()?);
    let offset = usize::try_from(dmabuf.offsets().next()?).ok()?;
    let stride = usize::try_from(dmabuf.strides().next()?).ok()?;
    let len = offset
        .checked_add(usize::try_from(size.h).ok()?.checked_mul(stride)?)?
        .max((width as usize).checked_mul(layout.bytes_per_pixel())?);
    let fd = dmabuf.handles().next()?;
    // SAFETY: a fresh private read-only mapping of the plane's fd at an
    // address the kernel chooses, unmapped exactly once below. `decode`
    // reads only within its length.
    let ptr = unsafe {
        rustix::mm::mmap(
            std::ptr::null_mut(),
            len,
            rustix::mm::ProtFlags::READ,
            rustix::mm::MapFlags::PRIVATE,
            fd,
            0,
        )
    }
    .ok()?;
    let decoded = {
        // SAFETY: `mmap` returned a mapping of `len` bytes.
        let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
        decode(layout, bytes, (width, height), stride, offset)
    };
    // SAFETY: the mapping was made just above and is not read again.
    unsafe {
        let _ = rustix::mm::munmap(ptr, len);
    }
    decoded
}

/// The window's pieces when it carries an HDR signal: its described surface drawn as its rendition
/// (or as the pass being built draws there) where it stands, and its popups and subsurfaces as
/// usual. `None` for an ordinary window, which the caller draws as it always has.
pub(crate) fn pieces<R>(
    renderer: &mut R,
    state: &crate::state::State,
    window: &Window,
    origin: Point<i32, Physical>,
    output_scale: f64,
    opacity: f32,
) -> Option<Vec<Piece<R>>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let surface = window.wl_surface()?;
    let description = crate::color_management::colorimetry_of(&surface)?;
    let id = crate::state::window_id(state, window)?;
    let hdr = &state.hdr;
    let plane = hdr.surface(id, window, description)?;
    let scale = Scale::from(output_scale);
    let mut pieces = Vec::new();
    // A popup is a surface of its own: it is drawn as usual, in its place over the window. Only the
    // window's own surface carries a layer here.
    for (popup, popup_offset) in PopupManager::popups_for_surface(&surface) {
        let offset = (window.geometry().loc + popup_offset - popup.geometry().loc)
            .to_physical_precise_round(scale);
        let popup = popup.wl_surface().clone();
        pieces.extend(render_elements_from_surface_tree::<R, Piece<R>>(
            renderer,
            &popup,
            origin + offset,
            scale,
            opacity,
            Kind::Unspecified,
        ));
    }
    let root = surface.clone();
    let mut described = Vec::new();
    with_surface_tree_downward(
        &surface,
        origin.to_f64(),
        |_, states, location| {
            let mut location = *location;
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return TraversalAction::SkipChildren;
            };
            let view = data.lock().unwrap_or_else(PoisonError::into_inner).view();
            match view {
                Some(view) => {
                    location += view.offset.to_f64().to_physical(scale);
                    TraversalAction::DoChildren(location)
                }
                None => TraversalAction::SkipChildren,
            }
        },
        |surface, states, location| {
            let mut location = *location;
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return;
            };
            let state = data.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(view) = state.view() else {
                return;
            };
            let buffer_scale = state.buffer_scale();
            location += view.offset.to_f64().to_physical(scale);
            drop(state);
            if surface.id() == root.id() {
                let drawn = Drawn {
                    id,
                    plane: &plane,
                    output_scale,
                    opacity,
                };
                if let Some(element) = piece(renderer, hdr, drawn, view, location, buffer_scale) {
                    described.push(element);
                }
            } else if let Ok(Some(element)) = WaylandSurfaceRenderElement::from_surface(
                renderer,
                surface,
                states,
                location,
                opacity,
                Kind::Unspecified,
            ) {
                described.push(Piece::Surface(element));
            }
        },
        |_, _, _| true,
    );
    pieces.extend(described);
    Some(pieces)
}

/// A described surface as a pass draws it: the window it belongs to, its signal at the buffer's
/// precision, and the pass's scale and opacity.
#[derive(Clone, Copy)]
struct Drawn<'a> {
    /// The window whose surface it is.
    id: u64,
    /// The signal at the buffer's precision.
    plane: &'a Arc<HdrSurface>,
    /// Physical pixels per logical pixel.
    output_scale: f64,
    /// How opaque the window is drawn.
    opacity: f32,
}

/// The described surface itself: its rendition where it stands, or whatever the pass being built
/// draws there, registered as the layer this frame composites.
fn piece<R>(
    renderer: &mut R,
    hdr: &HdrSurfaces,
    drawn: Drawn<'_>,
    view: SurfaceView,
    location: Point<f64, Physical>,
    buffer_scale: i32,
) -> Option<Piece<R>>
where
    R: Renderer + ImportMem,
    R::TextureId: Clone + Send + 'static,
{
    let Drawn {
        id,
        plane,
        output_scale,
        opacity,
    } = drawn;
    let channel = channel_of(plane.plane().id());
    // The rendition covers the surface's logical extent; a substitute is a single pixel of its
    // colour stretched over the same extent.
    let (buffer, src) = match appearance() {
        Appearance::Rendered => (hdr.rendition(id, plane.plane().id())?, view.src),
        Appearance::Black => (
            solid([0, 0, 0, 255]),
            Rectangle::from_size(Size::from((1.0, 1.0))),
        ),
        Appearance::Lit => (
            solid(channel_colour(channel?)),
            Rectangle::from_size(Size::from((1.0, 1.0))),
        ),
    };
    let factor = output_scale / f64::from(buffer_scale.max(1));
    let scale_x = factor * f64::from(view.dst.w) / view.src.size.w.max(1.0);
    let scale_y = factor * f64::from(view.dst.h) / view.src.size.h.max(1.0);
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "Affine composition is floating-point matrix multiplication; it cannot panic"
    )]
    let image_to_scene = Affine::translate((location.x, location.y))
        * Affine::scale_non_uniform(scale_x, scale_y)
        * Affine::translate((-view.src.loc.x, -view.src.loc.y));
    let bounds = Rect::new(
        location.x,
        location.y,
        location.x + f64::from(view.dst.w) * output_scale,
        location.y + f64::from(view.dst.h) * output_scale,
    );
    plane.record(image_to_scene, bounds);
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        location,
        &buffer,
        (opacity < 1.0).then_some(opacity),
        Some(src),
        Some(view.dst),
        Kind::Unspecified,
    )
    .ok()
    .map(Piece::Rendered)
}

/// A one-pixel buffer of `colour` (R, G, B, A), drawn stretched over a surface's extent: what a
/// substitution pass puts in its place.
fn solid(colour: [u8; 4]) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        &colour,
        Fourcc::Abgr8888,
        (1, 1),
        1,
        Transform::Normal,
        Some(vec![Rectangle::from_size(Size::from((1, 1)))]),
    )
}

/// The opaque colour a layer's weight channel is drawn in.
fn channel_colour(channel: u8) -> [u8; 4] {
    let mut colour = [0, 0, 0, 255];
    if let Some(slot) = colour.get_mut(usize::from(channel.min(2))) {
        *slot = 255;
    }
    colour
}

/// The composited frame: `first` (the scene with every layer black) with each HDR layer's light
/// from `second` (the same scene with the layers in their channel colours), as the 8-bit SDR
/// desktop shows it.
/// Both are the output's pixels in R, G, B, A order, `width` × `height` . A pixel no layer covers
/// is left as `first` has it.
pub(crate) fn composite(
    first: &[u8],
    second: &[u8],
    (width, height): (u32, u32),
    layers: &[HdrLayer],
) -> Vec<u8> {
    let mut rgba = first.to_vec();
    for y in 0..height {
        for x in 0..width {
            if !layers.iter().any(|layer| layer.covers(x, y)) {
                continue;
            }
            let at = (y as usize)
                .saturating_mul(width as usize)
                .saturating_add(x as usize)
                .saturating_mul(4);
            let pixel = |source: &[u8]| -> [u8; 4] {
                source
                    .get(at..at.saturating_add(4))
                    .and_then(|pixel| pixel.try_into().ok())
                    .unwrap_or_default()
            };
            let light = composite_hdr_pixel(
                pixel(first),
                pixel(second),
                x,
                y,
                layers,
                CompositeTarget::Sdr,
            );
            let [r, g, b] = encode_srgb8(light);
            if let Some(slot) = rgba.get_mut(at..at.saturating_add(4)) {
                slot.copy_from_slice(&[r, g, b, pixel(first)[3]]);
            }
        }
    }
    rgba
}
