//! Screen capture for trusted tools: screenshots, recorders and the
//! desktop portal's screen sharing.
//!
//! Two protocols ask for frames: `zwlr_screencopy_manager_v1` (grim,
//! wf-recorder, older portal backends) and `ext_image_copy_capture_v1` with
//! output sources (newer portal backends). Both become a [`Frame`]: a part of
//! one output to draw into a client's buffer, with or without the pointer,
//! now or once that part changed. Screen contents are private, so only
//! clients the session trusts see either global (as with output management).
//!
//! A frame is served after the next [`Compositor::render`](crate::Compositor::render):
//! the backend draws the output's scene again into the client's buffer (a
//! dma-buf is drawn into directly; shared memory through an offscreen image
//! read back), see [`copy`]. A locked session's outputs show only the lock
//! surface, so captures do too.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: physical coordinates of outputs and buffers, far from i32 limits"
)]

mod image_copy;
mod screencopy;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, Format, Fourcc};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::{Bind, ExportMem, ImportAll, ImportMem, Offscreen, Renderer};
use smithay::output::Output;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::{wl_buffer::WlBuffer, wl_shm};
use smithay::utils::{Buffer, Logical, Monotonic, Physical, Point, Rectangle, Size, Transform};

use crate::backend::BackendError;
use crate::render::{BACKGROUND, output_elements};
use crate::state::State;

/// The shared-memory formats frames are written in: what every capture
/// client reads.
const SHM_FORMATS: [wl_shm::Format; 2] = [wl_shm::Format::Xrgb8888, wl_shm::Format::Argb8888];

/// The dma-buf formats offered, when the backend can draw into dma-bufs.
const DMABUF_CODES: [Fourcc; 2] = [Fourcc::Xrgb8888, Fourcc::Argb8888];

/// Where a frame's answer goes.
pub(crate) enum Reply {
    Screencopy(screencopy::Frame),
    ImageCopy(image_copy::Frame),
}

impl Reply {
    fn alive(&self) -> bool {
        match self {
            Self::Screencopy(frame) => frame.alive(),
            Self::ImageCopy(frame) => frame.alive(),
        }
    }

    fn ready(&self, damage: &[Rectangle<i32, Buffer>], transform: Transform) {
        let time = std::time::Duration::from(smithay::utils::Clock::<Monotonic>::new().now());
        match self {
            Self::Screencopy(frame) => frame.ready(damage, time),
            Self::ImageCopy(frame) => frame.ready(damage, transform, time),
        }
    }

    fn failed(&self, why: Failure) {
        match self {
            Self::Screencopy(frame) => frame.failed(),
            Self::ImageCopy(frame) => frame.failed(why),
        }
    }
}

/// Why a frame was not copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The buffer no longer fits the output (its mode changed).
    Constraints,
    /// The output is gone.
    Stopped,
    /// The renderer failed.
    Unknown,
}

/// Whose earlier copies a frame's damage is counted from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Watcher {
    /// A screencopy client watching one output.
    Client(ClientId, String),
    /// An image-copy session.
    Session(ObjectId),
}

/// One requested copy of a part of an output.
pub struct Frame {
    reply: Reply,
    output: String,
    /// The captured part of the output, in its physical pixels as shown
    /// (after its transform).
    region: Rectangle<i32, Physical>,
    buffer: WlBuffer,
    cursor: bool,
    /// Copy only once the region changed since the watcher's last copy.
    wait_for_damage: bool,
    watcher: Watcher,
}

impl Frame {
    /// The name of the output the frame copies.
    pub fn output(&self) -> &str {
        &self.output
    }
}

/// The outcome of [`copy`].
pub enum Copied {
    /// Nothing changed since the watcher's last copy: the frame waits.
    Waiting,
    /// The buffer holds the region; these parts of it changed since the
    /// watcher's last copy (buffer coordinates).
    Done(Vec<Rectangle<i32, Buffer>>),
}

/// Why [`copy`] failed.
#[derive(Debug)]
pub enum CaptureError {
    /// The client's buffer does not fit the frame.
    Buffer(String),
    /// The backend or renderer failed.
    Backend(BackendError),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Buffer(why) => write!(f, "capture buffer: {why}"),
            Self::Backend(error) => write!(f, "capture: {error}"),
        }
    }
}

impl From<BackendError> for CaptureError {
    fn from(error: BackendError) -> Self {
        Self::Backend(error)
    }
}

/// What a watcher saw at its last copy.
struct Watch {
    watcher: Watcher,
    size: Size<i32, Physical>,
    scale: f64,
    transform: Transform,
    tracker: OutputDamageTracker,
}

/// The dma-bufs the backend can draw into.
#[derive(Clone, Debug)]
pub(crate) struct DmabufTargets {
    /// The device they are allocated on (`dev_t`).
    pub(crate) device: u64,
    pub(crate) formats: Vec<Format>,
}

/// Capture's state: the globals, frames waiting and what each watcher saw.
pub(crate) struct Capture {
    pending: Vec<Frame>,
    watches: Vec<Watch>,
    dmabuf: Option<DmabufTargets>,
    sessions: Vec<image_copy::Session>,
}

impl Capture {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        screencopy::create_global(display);
        image_copy::create_globals(display);
        Self {
            pending: Vec::new(),
            watches: Vec::new(),
            dmabuf: None,
            sessions: Vec::new(),
        }
    }

    /// Offer dma-buf targets of `formats` (those the renderer draws into)
    /// on `device`, besides shared memory.
    pub(crate) fn set_dmabuf_targets(
        &mut self,
        device: u64,
        formats: impl IntoIterator<Item = Format>,
    ) {
        let formats: Vec<Format> = formats
            .into_iter()
            .filter(|format| DMABUF_CODES.contains(&format.code))
            .collect();
        self.dmabuf = (!formats.is_empty()).then_some(DmabufTargets { device, formats });
    }

    fn queue(&mut self, frame: Frame) {
        self.pending.push(frame);
    }

    /// Frames to try now.
    pub(crate) fn take_pending(&mut self) -> Vec<Frame> {
        std::mem::take(&mut self.pending)
    }

    /// The damage of `elements` since `watcher`'s last copy, in the
    /// region's physical pixels; all of it at a watcher's first copy or
    /// when the region's size or the output's scale or transform changed.
    fn damage_since<E: smithay::backend::renderer::element::Element>(
        &mut self,
        watcher: &Watcher,
        size: Size<i32, Physical>,
        scale: f64,
        transform: Transform,
        elements: &[E],
    ) -> Vec<Rectangle<i32, Physical>> {
        if !self.watches.iter().any(|watch| &watch.watcher == watcher) {
            self.watches.push(Watch {
                watcher: watcher.clone(),
                size,
                scale,
                transform,
                tracker: OutputDamageTracker::new(size, scale, transform),
            });
        }
        let Some(watch) = self
            .watches
            .iter_mut()
            .find(|watch| &watch.watcher == watcher)
        else {
            return Vec::new();
        };
        #[allow(
            clippy::float_cmp,
            reason = "the scale is compared with the one stored from the same output, not computed"
        )]
        let changed = watch.size != size || watch.scale != scale || watch.transform != transform;
        if changed {
            *watch = Watch {
                watcher: watcher.clone(),
                size,
                scale,
                transform,
                tracker: OutputDamageTracker::new(size, scale, transform),
            };
        }
        match watch.tracker.damage_output(1, elements) {
            Ok((Some(damage), _)) => damage.clone(),
            Ok((None, _)) => Vec::new(),
            Err(_) => vec![Rectangle::from_size(
                transform.invert().transform_size(size),
            )],
        }
    }

    /// Forget watchers that are gone: disconnected clients, destroyed
    /// sessions.
    fn retain_watchers(&mut self, display: &DisplayHandle) {
        self.watches.retain(|watch| match &watch.watcher {
            Watcher::Client(client, _) => display
                .backend_handle()
                .get_client_data(client.clone())
                .is_ok(),
            Watcher::Session(session) => self
                .sessions
                .iter()
                .any(|live| live.alive() && &live.id() == session),
        });
    }
}

impl State {
    /// Offer capture clients dma-buf targets the renderer can draw into.
    pub(crate) fn enable_dmabuf_capture(
        &mut self,
        device: u64,
        formats: impl IntoIterator<Item = Format>,
    ) {
        self.capture.set_dmabuf_targets(device, formats);
    }

    /// The physical size `output` shows (its mode after its transform).
    fn shown_size(&self, name: &str) -> Option<(Output, Size<i32, Physical>)> {
        let output = self.space.outputs().find(|output| output.name() == name)?;
        let mode = output.current_mode()?;
        let size = output.current_transform().transform_size(mode.size);
        Some((output.clone(), size))
    }
}

impl dyn crate::Backend {
    /// Copy the frames that can be copied now and tell their clients; keep
    /// those waiting for damage.
    pub(crate) fn serve_captures(&mut self, state: &mut State) {
        image_copy::refresh_sessions(state);
        let display = state.display.clone();
        state.capture.retain_watchers(&display);
        for frame in state.capture.take_pending() {
            if !frame.reply.alive() {
                continue;
            }
            if state.shown_size(&frame.output).is_none() {
                frame.reply.failed(Failure::Stopped);
                continue;
            }
            match self.capture(state, &frame) {
                Ok(Copied::Waiting) => state.capture.queue(frame),
                Ok(Copied::Done(damage)) => {
                    let transform = state
                        .shown_size(&frame.output)
                        .map_or(Transform::Normal, |(output, _)| output.current_transform());
                    frame.reply.ready(&damage, transform);
                }
                Err(CaptureError::Buffer(why)) => {
                    tracing::debug!(why, output = frame.output, "capture buffer refused");
                    frame.reply.failed(Failure::Constraints);
                }
                Err(CaptureError::Backend(error)) => {
                    tracing::warn!(%error, output = frame.output, "capture failed");
                    frame.reply.failed(Failure::Unknown);
                }
            }
        }
    }
}

/// The part of an output shown at `region` (logical, relative to the
/// output; all of it when `None`) in its physical pixels, clipped to the
/// `shown` size; `None` when nothing of it is on the output.
pub(crate) fn region_on_output(
    region: Option<Rectangle<i32, Logical>>,
    scale: f64,
    shown: Size<i32, Physical>,
) -> Option<Rectangle<i32, Physical>> {
    let whole = Rectangle::from_size(shown);
    let region = match region {
        None => whole,
        Some(region) => {
            if region.size.w <= 0 || region.size.h <= 0 {
                return None;
            }
            region.to_f64().to_physical(scale).to_i32_round()
        }
    };
    let clipped = region.intersection(whole)?;
    (clipped.size.w > 0 && clipped.size.h > 0).then_some(clipped)
}

/// The size of the buffer a region of an output with `transform` is copied
/// into (its sides swap for quarter turns).
pub(crate) fn buffer_size(
    region: Size<i32, Physical>,
    transform: Transform,
) -> Size<i32, Physical> {
    transform.invert().transform_size(region)
}

/// Damage in a region's physical pixels as rectangles of its buffer.
fn damage_in_buffer(
    damage: &[Rectangle<i32, Physical>],
    region: Size<i32, Physical>,
    transform: Transform,
) -> Vec<Rectangle<i32, Buffer>> {
    let render = transform.invert();
    damage
        .iter()
        .map(|rect| {
            let rect = render.transform_rect_in(*rect, &region);
            Rectangle::new(
                Point::from((rect.loc.x, rect.loc.y)),
                Size::from((rect.size.w, rect.size.h)),
            )
        })
        .collect()
}

/// A shared-memory buffer's layout, as the client created it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShmLayout {
    pub(crate) offset: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) stride: i32,
    pub(crate) format: wl_shm::Format,
}

impl From<smithay::wayland::shm::BufferData> for ShmLayout {
    fn from(data: smithay::wayland::shm::BufferData) -> Self {
        Self {
            offset: data.offset,
            width: data.width,
            height: data.height,
            stride: data.stride,
            format: data.format,
        }
    }
}

/// Whether a shared-memory buffer can take a frame of `size` from a pool
/// of `pool_len` bytes: an offered format, the frame's size, rows wide
/// enough and all inside the pool.
pub(crate) fn check_shm(
    layout: ShmLayout,
    pool_len: usize,
    size: Size<i32, Physical>,
) -> Result<(), String> {
    if !SHM_FORMATS.contains(&layout.format) {
        return Err(format!("format {:?} is not offered", layout.format));
    }
    if (layout.width, layout.height) != (size.w, size.h) {
        return Err(format!(
            "{}x{} buffer for a {}x{} frame",
            layout.width, layout.height, size.w, size.h
        ));
    }
    let row = usize::try_from(layout.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or("negative width")?;
    let stride = usize::try_from(layout.stride).map_err(|_| "negative stride")?;
    if stride < row {
        return Err(format!("stride {stride} below a row of {row} bytes"));
    }
    let end = usize::try_from(layout.offset)
        .ok()
        .zip(usize::try_from(layout.height).ok())
        .and_then(|(offset, height)| {
            offset.checked_add(
                stride
                    .checked_mul(height.checked_sub(1)?)?
                    .checked_add(row)?,
            )
        })
        .ok_or("buffer extent out of range")?;
    if end > pool_len {
        return Err(format!(
            "buffer ends at {end}, past its pool of {pool_len} bytes"
        ));
    }
    Ok(())
}

/// Whether a dma-buf can take a frame of `size`: an offered format and
/// modifier, the frame's size.
pub(crate) fn check_dmabuf(
    size: (i32, i32),
    format: Format,
    frame: Size<i32, Physical>,
    offered: Option<&DmabufTargets>,
) -> Result<(), String> {
    let offered = offered.ok_or("dma-buf targets are not offered")?;
    if !offered.formats.contains(&format) {
        return Err(format!("dma-buf format {format:?} is not offered"));
    }
    if size != (frame.w, frame.h) {
        return Err(format!(
            "{}x{} dma-buf for a {}x{} frame",
            size.0, size.1, frame.w, frame.h
        ));
    }
    Ok(())
}

/// Write rows of R, G, B, A bytes (`width` pixels each) into a
/// shared-memory buffer laid out as `layout` (XRGB8888 and ARGB8888 are B,
/// G, R, A in memory). `None` when the pixels or the pool are too short.
fn write_pixels(rgba: &[u8], layout: ShmLayout, pool: &mut [u8]) -> Option<()> {
    let width = usize::try_from(layout.width).ok()?;
    let height = usize::try_from(layout.height).ok()?;
    let stride = usize::try_from(layout.stride).ok()?;
    let offset = usize::try_from(layout.offset).ok()?;
    let row = width.checked_mul(4)?;
    for y in 0..height {
        let from = y.checked_mul(row)?;
        let source = rgba.get(from..from.checked_add(row)?)?;
        let start = offset.checked_add(y.checked_mul(stride)?)?;
        let target = pool.get_mut(start..start.checked_add(row)?)?;
        for (pixel, out) in source.chunks_exact(4).zip(target.chunks_exact_mut(4)) {
            if let ([r, g, b, a], [ob, og, or, oa]) = (pixel, out) {
                *ob = *b;
                *og = *g;
                *or = *r;
                *oa = *a;
            }
        }
    }
    Some(())
}

/// Draw `frame`'s region of `output` into its buffer with `renderer`,
/// through an offscreen image of type `T` for shared memory. Waits (draws
/// nothing) when the frame waits for damage and nothing changed.
pub fn copy<R, T>(
    renderer: &mut R,
    state: &mut State,
    output: &Output,
    frame: &Frame,
) -> Result<Copied, CaptureError>
where
    R: Renderer + ImportAll + ImportMem + Offscreen<T> + Bind<Dmabuf> + ExportMem,
    R::TextureId: Clone + Send + 'static,
{
    let transform = output.current_transform();
    let scale = output.current_scale().fractional_scale();
    let size = buffer_size(frame.region.size, transform);
    let mut elements = if frame.cursor {
        state.cursor_elements(renderer, output)
    } else {
        Vec::new()
    };
    elements.extend(output_elements(renderer, state, output));
    let offset = Point::<i32, Physical>::from((-frame.region.loc.x, -frame.region.loc.y));
    let elements: Vec<_> = elements
        .into_iter()
        .map(|element| RelocateRenderElement::from_element(element, offset, Relocate::Relative))
        .collect();
    let damage = state
        .capture
        .damage_since(&frame.watcher, size, scale, transform, &elements);
    if damage.is_empty() && frame.wait_for_damage {
        return Ok(Copied::Waiting);
    }
    let damage = damage_in_buffer(&damage, frame.region.size, transform);
    if let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(&frame.buffer) {
        let dmabuf_size = dmabuf.size();
        check_dmabuf(
            (dmabuf_size.w, dmabuf_size.h),
            dmabuf.format(),
            size,
            state.capture.dmabuf.as_ref(),
        )
        .map_err(CaptureError::Buffer)?;
        let mut dmabuf = dmabuf.clone();
        let mut target = renderer
            .bind(&mut dmabuf)
            .map_err(|error| crate::backend::failed("bind capture dma-buf", error))?;
        draw(renderer, &mut target, size, scale, transform, &elements)?;
        return Ok(Copied::Done(damage));
    }
    let rgba = render_to_memory::<R, T, _>(renderer, size, scale, transform, &elements)?;
    smithay::wayland::shm::with_buffer_contents_mut(&frame.buffer, |pointer, len, data| {
        let layout = ShmLayout::from(data);
        check_shm(layout, len, size)?;
        // SAFETY: smithay maps the client's pool for `len` bytes and keeps
        // it mapped for this call; the client may write it concurrently,
        // which can tear the image it reads but not touch our memory.
        let pool = unsafe { std::slice::from_raw_parts_mut(pointer, len) };
        write_pixels(&rgba, layout, pool).ok_or_else(|| "pixels out of range".to_owned())
    })
    .map_err(|error| CaptureError::Buffer(format!("{error:?}")))?
    .map_err(CaptureError::Buffer)?;
    Ok(Copied::Done(damage))
}

/// Draw `elements` over the background into a bound target of `size`.
fn draw<R, E>(
    renderer: &mut R,
    target: &mut R::Framebuffer<'_>,
    size: Size<i32, Physical>,
    scale: f64,
    transform: Transform,
    elements: &[E],
) -> Result<(), CaptureError>
where
    R: Renderer,
    E: RenderElement<R>,
{
    let mut tracker = OutputDamageTracker::new(size, scale, transform);
    let drawn = tracker
        .render_output(renderer, target, 0, elements, BACKGROUND)
        .map_err(|error| crate::backend::failed("render capture", error))?;
    // The client reads the buffer as soon as it is told: the GPU has to be
    // done with it.
    drawn
        .sync
        .wait()
        .map_err(|error| crate::backend::failed("wait for capture", error))?;
    Ok(())
}

/// Draw `elements` into an offscreen image of `size` and read it back as
/// rows of R, G, B, A.
fn render_to_memory<R, T, E>(
    renderer: &mut R,
    size: Size<i32, Physical>,
    scale: f64,
    transform: Transform,
    elements: &[E],
) -> Result<Vec<u8>, CaptureError>
where
    R: Renderer + Offscreen<T> + ExportMem,
    E: RenderElement<R>,
{
    let buffer_size = Size::<i32, Buffer>::from((size.w, size.h));
    let mut image: T = renderer
        .create_buffer(Fourcc::Abgr8888, buffer_size)
        .map_err(|error| crate::backend::failed("capture image", error))?;
    let mut target = renderer
        .bind(&mut image)
        .map_err(|error| crate::backend::failed("bind capture image", error))?;
    draw(renderer, &mut target, size, scale, transform, elements)?;
    // ABGR8888 is R, G, B, A in memory.
    let mapping = renderer
        .copy_framebuffer(&target, Rectangle::from_size(buffer_size), Fourcc::Abgr8888)
        .map_err(|error| crate::backend::failed("read capture back", error))?;
    let bytes = renderer
        .map_texture(&mapping)
        .map_err(|error| crate::backend::failed("map capture", error))?;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(width: i32, height: i32, stride: i32) -> ShmLayout {
        ShmLayout {
            offset: 0,
            width,
            height,
            stride,
            format: wl_shm::Format::Xrgb8888,
        }
    }

    #[test]
    fn a_whole_output_is_its_shown_size_and_regions_are_scaled_and_clipped() {
        let shown = Size::from((1920, 1080));
        assert_eq!(
            region_on_output(None, 1.0, shown),
            Some(Rectangle::from_size(shown))
        );
        let region = Rectangle::new((100, 50).into(), (200, 100).into());
        assert_eq!(
            region_on_output(Some(region), 2.0, shown),
            Some(Rectangle::new((200, 100).into(), (400, 200).into()))
        );
        let hanging = Rectangle::new((900, 500).into(), (200, 100).into());
        assert_eq!(
            region_on_output(Some(hanging), 2.0, shown),
            Some(Rectangle::new((1800, 1000).into(), (120, 80).into()))
        );
        let outside = Rectangle::new((2000, 0).into(), (10, 10).into());
        assert_eq!(region_on_output(Some(outside), 1.0, shown), None);
        let empty = Rectangle::new((0, 0).into(), (0, 10).into());
        assert_eq!(region_on_output(Some(empty), 1.0, shown), None);
    }

    #[test]
    fn rotated_outputs_copy_into_buffers_of_their_panel_orientation() {
        let region = Size::from((1080, 1920));
        assert_eq!(buffer_size(region, Transform::Normal), region);
        assert_eq!(
            buffer_size(region, Transform::_90),
            Size::from((1920, 1080))
        );
        let damage = [Rectangle::new((0, 0).into(), (10, 20).into())];
        let normal = damage_in_buffer(&damage, region, Transform::Normal);
        assert_eq!(normal, vec![Rectangle::new((0, 0).into(), (10, 20).into())]);
        let turned = damage_in_buffer(&damage, region, Transform::_90);
        assert_eq!(
            turned.first().map(|rect| rect.size),
            Some(Size::from((20, 10)))
        );
    }

    #[test]
    fn shm_buffers_must_fit_the_frame() {
        let size = Size::from((4, 2));
        assert_eq!(check_shm(layout(4, 2, 16), 32, size), Ok(()));
        // Padded rows, the last one need not be padded.
        assert_eq!(check_shm(layout(4, 2, 20), 36, size), Ok(()));
        assert!(
            check_shm(layout(4, 2, 12), 64, size).is_err(),
            "rows too narrow"
        );
        assert!(check_shm(layout(5, 2, 20), 64, size).is_err(), "wrong size");
        assert!(
            check_shm(layout(4, 2, 16), 31, size).is_err(),
            "past the pool"
        );
        let mut odd = layout(4, 2, 16);
        odd.format = wl_shm::Format::Rgb565;
        assert!(check_shm(odd, 64, size).is_err(), "format not offered");
        let mut shifted = layout(4, 2, 16);
        shifted.offset = 8;
        assert!(
            check_shm(shifted, 32, size).is_err(),
            "offset past the pool"
        );
        assert_eq!(check_shm(shifted, 40, size), Ok(()));
    }

    #[test]
    fn dmabufs_must_be_offered_and_fit_the_frame() {
        let format = Format {
            code: Fourcc::Xrgb8888,
            modifier: smithay::backend::allocator::Modifier::Linear,
        };
        let targets = DmabufTargets {
            device: 1,
            formats: vec![format],
        };
        let size = Size::from((8, 8));
        assert_eq!(check_dmabuf((8, 8), format, size, Some(&targets)), Ok(()));
        assert!(check_dmabuf((8, 8), format, size, None).is_err());
        assert!(check_dmabuf((8, 9), format, size, Some(&targets)).is_err());
        let other = Format {
            code: Fourcc::Abgr8888,
            ..format
        };
        assert!(check_dmabuf((8, 8), other, size, Some(&targets)).is_err());
    }

    #[test]
    fn pixels_land_as_bgra_rows_at_the_buffers_stride() {
        let rgba = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut pool = [0_u8; 2 + 6 + 4];
        let shm = ShmLayout {
            offset: 2,
            width: 1,
            height: 2,
            stride: 6,
            format: wl_shm::Format::Argb8888,
        };
        assert_eq!(write_pixels(&rgba, shm, &mut pool), Some(()));
        assert_eq!(pool, [0, 0, 3, 2, 1, 4, 0, 0, 7, 6, 5, 8]);
        assert_eq!(write_pixels(&rgba, shm, &mut [0_u8; 8]), None);
    }

    #[test]
    fn only_drawable_formats_are_offered_as_dmabuf_targets() {
        let mut capture = Capture {
            pending: Vec::new(),
            watches: Vec::new(),
            dmabuf: None,
            sessions: Vec::new(),
        };
        let linear = smithay::backend::allocator::Modifier::Linear;
        capture.set_dmabuf_targets(
            7,
            [
                Format {
                    code: Fourcc::Nv12,
                    modifier: linear,
                },
                Format {
                    code: Fourcc::Xrgb8888,
                    modifier: linear,
                },
            ],
        );
        let targets = capture.dmabuf.clone().expect("a drawable format");
        assert_eq!(targets.device, 7);
        assert_eq!(targets.formats.len(), 1);
        capture.set_dmabuf_targets(7, []);
        assert!(capture.dmabuf.is_none());
    }
}
