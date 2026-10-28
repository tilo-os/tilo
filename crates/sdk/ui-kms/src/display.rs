//! One display lit through KMS: a connected connector at its preferred mode,
//! two dumb buffers it flips between, and a hardware cursor where the driver
//! has one.
//!
//! The frame is kept in a canvas in ordinary memory ([`crate::canvas`]):
//! dumb buffers are often write-combined, and a rasterizer that reads what
//! it blends over would crawl on them. Each frame copies into the buffer not
//! on screen what that buffer lacks, which is this frame's damage and the
//! damage of the frame before, which went to the other buffer.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;

use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::{
    Device as _, Event, Mode, ModeTypeFlags, PageFlipFlags, connector, crtc, dumbbuffer,
    framebuffer,
};
use tilo_cpu_raster::Region;

/// The card the host was handed, open and the master of its display.
pub(crate) struct Card(OwnedFd);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}
impl drm::control::Device for Card {}

/// A dumb buffer mapped for the host's lifetime.
struct Mapping {
    data: NonNull<u8>,
    len: usize,
}

impl Mapping {
    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: `data` is a live shared mapping of `len` bytes, unmapped
        // only on drop, and borrowed mutably only through `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.data.as_ptr(), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the mapping was made by `map` with this address and length
        // and nothing borrows it any more.
        let _ = unsafe { rustix::mm::munmap(self.data.as_ptr().cast(), self.len) };
    }
}

struct Scanout {
    buffer: dumbbuffer::DumbBuffer,
    framebuffer: framebuffer::Handle,
    map: Mapping,
    /// What this buffer lacks of the canvas; `None` when it holds none of it.
    stale: Option<Vec<Region>>,
}

fn map(card: &Card, buffer: &dumbbuffer::DumbBuffer) -> io::Result<Mapping> {
    let info = drm_ffi::mode::dumbbuffer::map(card.as_fd(), buffer.handle().into(), 0, 0)?;
    let len = (buffer.pitch() as usize).saturating_mul(buffer.size().1 as usize);
    let offset = info.offset;
    // SAFETY: the kernel handed out `offset` for exactly this buffer, whose
    // length is `len`; the mapping is shared with the scanout and lives in
    // `Mapping`, which unmaps it.
    let data = unsafe {
        rustix::mm::mmap(
            std::ptr::null_mut(),
            len,
            rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE,
            rustix::mm::MapFlags::SHARED,
            card.as_fd(),
            offset,
        )?
    };
    let data = NonNull::new(data.cast::<u8>()).ok_or_else(|| io::Error::other("null mapping"))?;
    Ok(Mapping { data, len })
}

/// What the host lights.
pub struct Display {
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    size: (u32, u32),
    physical_mm: Option<(u32, u32)>,
    scanouts: Vec<Scanout>,
    /// The scanout on screen, once the first frame is.
    front: Option<usize>,
    flip_pending: bool,
    cursor: Option<Cursor>,
}

struct Cursor {
    buffer: dumbbuffer::DumbBuffer,
    shown: bool,
}

/// Why the display could not be lit.
#[derive(Debug)]
pub enum DisplayError {
    /// The card has no connected connector with a mode.
    NoOutput,
    /// The kernel refused a request.
    Io(io::Error),
}

impl std::fmt::Display for DisplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOutput => f.write_str("no connected display"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for DisplayError {}

impl From<io::Error> for DisplayError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Built-in panels first: a laptop with a monitor attached signs in on its
/// own screen, as the login screen puts the card on the built-in panel.
fn internal(kind: connector::Interface) -> bool {
    matches!(
        kind,
        connector::Interface::EmbeddedDisplayPort
            | connector::Interface::LVDS
            | connector::Interface::DSI
            | connector::Interface::DPI
    )
}

impl Display {
    /// Find the display on `card` and prepare its buffers. Nothing changes
    /// on screen until the first [`Self::present`].
    pub fn open(card: OwnedFd) -> Result<Self, DisplayError> {
        let card = Card(card);
        // Universal planes are not needed; the cursor goes through the
        // legacy ioctl, which every KMS driver with a cursor answers.
        let resources = card.resource_handles()?;
        let mut outputs: Vec<connector::Info> = resources
            .connectors()
            .iter()
            .filter_map(|handle| card.get_connector(*handle, true).ok())
            .filter(|info| info.state() == connector::State::Connected && !info.modes().is_empty())
            .collect();
        outputs.sort_by_key(|info| !internal(info.interface()));
        for info in outputs {
            let Some(mode) = info
                .modes()
                .iter()
                .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
                .or_else(|| info.modes().first())
                .copied()
            else {
                continue;
            };
            let Some(crtc) = crtc_for(&card, &resources, &info) else {
                continue;
            };
            let (width, height) = mode.size();
            let size = (u32::from(width), u32::from(height));
            let mut scanouts = Vec::with_capacity(2);
            for _ in 0..2 {
                let buffer = card.create_dumb_buffer(size, DrmFourcc::Xrgb8888, 32)?;
                let framebuffer = card.add_framebuffer(&buffer, 24, 32)?;
                let map = map(&card, &buffer)?;
                scanouts.push(Scanout {
                    buffer,
                    framebuffer,
                    map,
                    stale: None,
                });
            }
            let physical_mm = info
                .size()
                .filter(|(width, height)| *width > 0 && *height > 0);
            let cursor = Cursor::new(&card).ok();
            return Ok(Self {
                card,
                crtc,
                connector: info.handle(),
                mode,
                size,
                physical_mm,
                scanouts,
                front: None,
                flip_pending: false,
                cursor,
            });
        }
        Err(DisplayError::NoOutput)
    }

    /// The mode's size in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The panel's size in millimetres, when the display reports it.
    pub fn physical_size(&self) -> Option<(u32, u32)> {
        self.physical_mm
    }

    /// The time between two refreshes of the mode.
    pub fn refresh_interval(&self) -> std::time::Duration {
        let hertz = self.mode.vrefresh().clamp(24, 240);
        std::time::Duration::from_secs(1)
            .checked_div(hertz)
            .unwrap_or_default()
    }

    pub(crate) fn fd(&self) -> BorrowedFd<'_> {
        self.card.as_fd()
    }

    /// A flip was asked for and has not landed yet: the next frame waits.
    pub fn flip_pending(&self) -> bool {
        self.flip_pending
    }

    /// Copy `canvas` (premultiplied RGBA, `width * 4` bytes a row) into the
    /// buffer not on screen where it differs, and show it at the next
    /// vertical blank. `damage` is what changed in the canvas since the last
    /// frame presented; `None` is all of it.
    pub fn present(&mut self, canvas: &[u8], damage: Option<&[Region]>) -> io::Result<()> {
        let (width, height) = self.size;
        let whole = [Region::new(0, 0, width, height)];
        let damage = damage.unwrap_or(&whole);
        let back = self.front.map_or(0, |front| front ^ 1);
        // The other buffer will lack what this frame changed.
        if let Some(front) = self.front
            && let Some(other) = self.scanouts.get_mut(front)
            && let Some(stale) = other.stale.as_mut()
        {
            stale.extend_from_slice(damage);
        }
        let Some(target) = self.scanouts.get_mut(back) else {
            return Err(io::Error::other("no scanout buffer"));
        };
        let pitch = target.buffer.pitch() as usize;
        let regions = match target.stale.take() {
            Some(mut lacking) => {
                lacking.extend_from_slice(damage);
                lacking
            }
            None => whole.to_vec(),
        };
        copy_regions(canvas, width, target.map.bytes(), pitch, &regions);
        target.stale = Some(Vec::new());
        let framebuffer = target.framebuffer;
        if self.front.is_none() {
            // The first frame sets the mode; every later one flips.
            self.card.set_crtc(
                self.crtc,
                Some(framebuffer),
                (0, 0),
                &[self.connector],
                Some(self.mode),
            )?;
            self.front = Some(back);
            if let Some(other) = self.scanouts.get_mut(back ^ 1) {
                other.stale = None;
            }
            return Ok(());
        }
        match self
            .card
            .page_flip(self.crtc, framebuffer, PageFlipFlags::EVENT, None)
        {
            Ok(()) => self.flip_pending = true,
            Err(error) => {
                // A driver that cannot flip still takes the buffer whole;
                // the frame then shows at once, unpaced.
                tracing::debug!(%error, "page flip refused; setting the buffer instead");
                self.card.set_crtc(
                    self.crtc,
                    Some(framebuffer),
                    (0, 0),
                    &[self.connector],
                    Some(self.mode),
                )?;
                self.front = Some(back);
            }
        }
        Ok(())
    }

    /// Read what the kernel says about the display. Returns whether a flip
    /// landed.
    pub fn dispatch(&mut self) -> io::Result<bool> {
        let mut flipped = false;
        for event in self.card.receive_events()? {
            if let Event::PageFlip(flip) = event
                && flip.crtc == self.crtc
                && self.flip_pending
            {
                self.flip_pending = false;
                self.front = self.front.map(|front| front ^ 1);
                flipped = true;
            }
        }
        Ok(flipped)
    }

    /// Show the pointer at `position` (physical pixels), or hide it.
    pub fn set_cursor(&mut self, position: Option<(f64, f64)>) {
        let Some(cursor) = self.cursor.as_mut() else {
            return;
        };
        match position {
            Some((x, y)) => {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "a pointer position on a display is a small pixel count"
                )]
                let at = (x.round() as i32, y.round() as i32);
                if !cursor.shown {
                    #[allow(deprecated, reason = "the legacy cursor works on every KMS driver")]
                    let shown = self
                        .card
                        .set_cursor2(self.crtc, Some(&cursor.buffer), (0, 0))
                        .is_ok();
                    cursor.shown = shown;
                }
                #[allow(deprecated, reason = "the legacy cursor works on every KMS driver")]
                let _ = self.card.move_cursor(self.crtc, at);
            }
            None if cursor.shown => {
                #[allow(deprecated, reason = "the legacy cursor works on every KMS driver")]
                let _ = self
                    .card
                    .set_cursor2::<dumbbuffer::DumbBuffer>(self.crtc, None, (0, 0));
                cursor.shown = false;
            }
            None => {}
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        for scanout in self.scanouts.drain(..) {
            let _ = self.card.destroy_framebuffer(scanout.framebuffer);
            drop(scanout.map);
            let _ = self.card.destroy_dumb_buffer(scanout.buffer);
        }
        if let Some(cursor) = self.cursor.take() {
            let _ = self.card.destroy_dumb_buffer(cursor.buffer);
        }
    }
}

/// A CRTC that can drive `info`: the one already driving it, else the first
/// free one its encoders can use.
fn crtc_for(
    card: &Card,
    resources: &drm::control::ResourceHandles,
    info: &connector::Info,
) -> Option<crtc::Handle> {
    let current = info
        .current_encoder()
        .and_then(|encoder| card.get_encoder(encoder).ok())
        .and_then(|encoder| encoder.crtc());
    current.or_else(|| {
        info.encoders()
            .iter()
            .filter_map(|encoder| card.get_encoder(*encoder).ok())
            .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
            .next()
    })
}

/// The arrow the pointer is, 64 pixels square as every driver's legacy
/// cursor takes it: white with a dark outline, its tip at the origin.
const CURSOR_SIDE: u32 = 64;

impl Cursor {
    fn new(card: &Card) -> io::Result<Self> {
        let mut buffer =
            card.create_dumb_buffer((CURSOR_SIDE, CURSOR_SIDE), DrmFourcc::Argb8888, 32)?;
        let pitch = buffer.pitch() as usize;
        let mut mapping = card.map_dumb_buffer(&mut buffer)?;
        draw_arrow(mapping.as_mut(), pitch);
        drop(mapping);
        Ok(Self {
            buffer,
            shown: false,
        })
    }
}

/// An arrow 20 pixels tall: inside the outline when `x <= y / 2 + 1` on the
/// upper rows and a stem below.
fn draw_arrow(bytes: &mut [u8], pitch: usize) {
    const TALL: u32 = 20;
    for y in 0..CURSOR_SIDE {
        for x in 0..CURSOR_SIDE {
            let edge = y.saturating_mul(3) / 5;
            let inside = y < TALL && x <= edge;
            let outline = inside && (x == 0 || x == edge || y == TALL.saturating_sub(1));
            let pixel: u32 = if outline {
                0xff20_2020
            } else if inside {
                0xffff_ffff
            } else {
                0
            };
            let offset = (y as usize)
                .saturating_mul(pitch)
                .saturating_add((x as usize).saturating_mul(4));
            if let Some(slot) = bytes.get_mut(offset..offset.saturating_add(4)) {
                slot.copy_from_slice(&pixel.to_le_bytes());
            }
        }
    }
}

/// Copy `regions` of a premultiplied RGBA canvas `width` pixels wide into an
/// XRGB8888 buffer with rows of `pitch` bytes. The display shows opaque
/// pixels, so the colour channels are taken as they are.
pub(crate) fn copy_regions(
    canvas: &[u8],
    width: u32,
    target: &mut [u8],
    pitch: usize,
    regions: &[Region],
) {
    let stride = (width as usize).saturating_mul(4);
    for region in regions {
        let x0 = (region.x.min(width) as usize).saturating_mul(4);
        let x1 = (region.x.saturating_add(region.width).min(width) as usize).saturating_mul(4);
        if x1 <= x0 {
            continue;
        }
        for row in region.y..region.y.saturating_add(region.height) {
            let row = row as usize;
            let Some(source) = canvas.get(row.saturating_mul(stride).saturating_add(x0)..) else {
                break;
            };
            let Some(source) = source.get(..x1.saturating_sub(x0)) else {
                break;
            };
            let start = row.saturating_mul(pitch).saturating_add(x0);
            let Some(dest) = target.get_mut(start..start.saturating_add(source.len())) else {
                break;
            };
            for (rgba, xrgb) in source.chunks_exact(4).zip(dest.chunks_exact_mut(4)) {
                if let ([r, g, b, _], [db, dg, dr, dx]) = (rgba, xrgb) {
                    *db = *b;
                    *dg = *g;
                    *dr = *r;
                    *dx = 0xff;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_copy_as_xrgb_and_nothing_else_is_touched() {
        let (width, height) = (4_u32, 3_u32);
        let canvas: Vec<u8> = (0..width * height)
            .flat_map(|i| [u8::try_from(i).unwrap(), 100, 200, 255])
            .collect();
        let pitch = 32;
        let mut target = vec![7_u8; pitch * height as usize];
        copy_regions(
            &canvas,
            width,
            &mut target,
            pitch,
            &[Region::new(1, 1, 2, 1), Region::new(3, 2, 9, 9)],
        );
        // Row 1, pixels 1 and 2: canvas pixels 5 and 6, as B, G, R, X.
        assert_eq!(
            &target[pitch + 4..pitch + 12],
            &[200, 100, 5, 255, 200, 100, 6, 255]
        );
        // Row 2, pixel 3, clipped to the canvas.
        assert_eq!(
            &target[2 * pitch + 12..2 * pitch + 16],
            &[200, 100, 11, 255]
        );
        // Untouched elsewhere.
        assert!(target[..pitch].iter().all(|byte| *byte == 7));
        assert!(target[pitch..pitch + 4].iter().all(|byte| *byte == 7));
        assert!(target[2 * pitch + 16..].iter().all(|byte| *byte == 7));
    }
}
