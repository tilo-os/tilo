//! What a surface hands the host that draws it, and what the host gives back.
//!
//! A surface draws its layers itself — the top bar, a panel, a sheet — and says which parts of them
//! changed: as premultiplied RGBA pixels the host copies into whatever it draws with (the page's
//! canvas, the compositor where no GPU draws), or, where a GPU draws them, into dma-bufs the host
//! composites straight from. It hands the surface the input that lands on them. Nothing here knows
//! how the pixels were made or where they are shown.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::{
    BackdropImage, BackdropRequest, CursorIcon, HostKey, HostedOutput, KeyClaim, Region,
    WindowPicture,
};

/// How many pixels a side of one coverage cell is: the granularity at which a frame says what it
/// shows.
pub const CELL: u32 = 8;

/// What one cell of a frame holds; a surface that reads its cells on the GPU hands them over as
/// these numbers ([`HostFrame::record_cells`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cell {
    /// Every pixel of it is transparent: nothing is shown there.
    Empty,
    /// Something is drawn in it.
    Partly,
    /// Every pixel of it is opaque: nothing under it shows through.
    Opaque,
}

/// What a frame shows and what of it nothing shows through, by cells: kept as the surface draws
/// into the frame, so a layer costs what it draws where it draws it rather than the whole frame it
/// draws in.
#[derive(Clone, Debug)]
struct Coverage {
    columns: u32,
    cells: Vec<Cell>,
}

impl Coverage {
    /// The coverage of a transparent frame of `size` pixels.
    fn new(size: (u32, u32)) -> Self {
        let columns = size.0.div_ceil(CELL);
        let rows = size.1.div_ceil(CELL);
        Self {
            columns,
            cells: vec![
                Cell::Empty;
                usize::try_from(columns)
                    .unwrap_or(0)
                    .saturating_mul(usize::try_from(rows).unwrap_or(0))
            ],
        }
    }

    /// Take the cells of `area` (frame pixels, its corner on a cell's) as a surface read them:
    /// `cells` row by row, `area.width.div_ceil(CELL)` a row, each a [`Cell`] number.
    fn set(&mut self, area: Region, cells: &[u8]) {
        let columns = usize::try_from(self.columns).unwrap_or(0).max(1);
        let first_row = (area.y / CELL) as usize;
        let first_column = (area.x / CELL) as usize;
        let across = area.width.div_ceil(CELL) as usize;
        for (row, read) in cells.chunks(across.max(1)).enumerate() {
            for (column, cell) in read.iter().enumerate() {
                let column = first_column.saturating_add(column);
                if column >= columns {
                    continue;
                }
                let slot = first_row
                    .saturating_add(row)
                    .saturating_mul(columns)
                    .saturating_add(column);
                if let Some(held) = self.cells.get_mut(slot) {
                    *held = match cell {
                        0 => Cell::Empty,
                        2 => Cell::Opaque,
                        _ => Cell::Partly,
                    };
                }
            }
        }
    }

    /// Read the cells `regions` (frame pixels) of `pixels` (premultiplied RGBA, `width` pixels a
    /// row) now hold.
    fn record(&mut self, pixels: &[u8], size: (u32, u32), regions: &[Region]) {
        let width = size.0;
        let columns = usize::try_from(self.columns).unwrap_or(0).max(1);
        let cells = &mut self.cells;
        for region in regions {
            let Some(area) = clip(region, size) else {
                continue;
            };
            let first_row = area.1 / CELL;
            let last_row = area.3.saturating_sub(1) / CELL;
            let first_column = area.0 / CELL;
            let last_column = area.2.saturating_sub(1) / CELL;
            for row in first_row..=last_row {
                let cell_top = row.saturating_mul(CELL);
                let cell_bottom = cell_top.saturating_add(CELL).min(size.1);
                for column in first_column..=last_column {
                    let cell_left = column.saturating_mul(CELL);
                    let cell_right = cell_left.saturating_add(CELL).min(size.0);
                    let drawn = (
                        area.0.max(cell_left),
                        area.1.max(cell_top),
                        area.2.min(cell_right),
                        area.3.min(cell_bottom),
                    );
                    let whole = drawn == (cell_left, cell_top, cell_right, cell_bottom);
                    let (mut shows, mut opaque) = (false, true);
                    for y in drawn.1..drawn.3 {
                        for x in drawn.0..drawn.2 {
                            let Some(alpha) = alpha_at(pixels, width, x, y) else {
                                continue;
                            };
                            shows |= alpha > 0;
                            opaque &= alpha == 255;
                        }
                    }
                    let Some(cell) = usize::try_from(row)
                        .ok()
                        .and_then(|row| row.checked_mul(columns))
                        .and_then(|slot| slot.checked_add(usize::try_from(column).ok()?))
                        .and_then(|slot| cells.get_mut(slot))
                    else {
                        continue;
                    };
                    *cell = if whole {
                        // The drawn part is the whole cell: what it holds now is what the cell
                        // holds.
                        if opaque {
                            Cell::Opaque
                        } else if shows {
                            Cell::Partly
                        } else {
                            Cell::Empty
                        }
                    } else if opaque && *cell == Cell::Opaque {
                        // Opaque pixels over part of an opaque cell leave it opaque.
                        Cell::Opaque
                    } else if !shows && *cell == Cell::Empty {
                        Cell::Empty
                    } else {
                        // Part of it is not the part drawn: the rest of what it holds is not known
                        // from this draw.
                        Cell::Partly
                    };
                }
            }
        }
    }

    /// The smallest rectangle of the frame holding anything it shows.
    fn shown(&self, size: (u32, u32)) -> Option<Region> {
        let columns = usize::try_from(self.columns).unwrap_or(0).max(1);
        let mut shown: Option<(u32, u32, u32, u32)> = None;
        for (row, cells) in self.cells.chunks(columns).enumerate() {
            let top = u32::try_from(row).unwrap_or(0).saturating_mul(CELL);
            let bottom = top.saturating_add(CELL).min(size.1);
            for (column, cell) in cells.iter().enumerate() {
                if *cell == Cell::Empty {
                    continue;
                }
                let left = u32::try_from(column).unwrap_or(0).saturating_mul(CELL);
                let right = left.saturating_add(CELL).min(size.0);
                shown = Some(match shown {
                    None => (left, top, right, bottom),
                    Some((was_left, was_top, was_right, was_bottom)) => (
                        was_left.min(left),
                        was_top.min(top),
                        was_right.max(right),
                        was_bottom.max(bottom),
                    ),
                });
            }
        }
        let (left, top, right, bottom) = shown?;
        Some(Region::new(
            left,
            top,
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        ))
    }

    /// The parts of the frame that are wholly opaque, as whole cells merged into as few rectangles
    /// as the rows give.
    fn opaque(&self, size: (u32, u32)) -> Vec<Region> {
        let columns = usize::try_from(self.columns).unwrap_or(0).max(1);
        let mut done: Vec<Region> = Vec::new();
        let mut running: Vec<Region> = Vec::new();
        for (row, cells) in self.cells.chunks(columns).enumerate() {
            let top = u32::try_from(row).unwrap_or(0).saturating_mul(CELL);
            let bottom = top.saturating_add(CELL).min(size.1);
            let height = bottom.saturating_sub(top);
            // Where the row's opaque cells start and stop, by column.
            let mut runs: Vec<(u32, u32)> = Vec::new();
            let mut start: Option<u32> = None;
            for (column, cell) in cells.iter().enumerate() {
                let column = u32::try_from(column).unwrap_or(0);
                match (*cell == Cell::Opaque, start) {
                    (true, None) => start = Some(column),
                    (false, Some(from)) => {
                        runs.push((from, column));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(from) = start {
                runs.push((from, self.columns));
            }
            // A run the row above also had goes on growing there.
            let mut next: Vec<Region> = Vec::with_capacity(runs.len());
            for (from, to) in runs {
                let left = from.saturating_mul(CELL);
                let right = to.saturating_mul(CELL).min(size.0);
                let width = right.saturating_sub(left);
                if let Some(above) = running
                    .iter_mut()
                    .find(|above| above.x == left && above.width == width)
                {
                    above.height = above.height.saturating_add(height);
                    next.push(*above);
                } else {
                    next.push(Region::new(left, top, width, height));
                }
            }
            done.extend(
                running
                    .iter()
                    .filter(|above| {
                        !next
                            .iter()
                            .any(|run| run.x == above.x && run.width == above.width)
                    })
                    .copied(),
            );
            running = next;
        }
        done.extend(running);
        done
    }
}

/// The part of `region` inside a frame of `size` pixels, as `(left, top, right, bottom)` : `None`
/// where it lies outside it.
fn clip(region: &Region, size: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let left = region.x.min(size.0);
    let top = region.y.min(size.1);
    let right = region.x.saturating_add(region.width).min(size.0);
    let bottom = region.y.saturating_add(region.height).min(size.1);
    (left < right && top < bottom).then_some((left, top, right, bottom))
}

/// The alpha of the pixel at `(x, y)` of `pixels` , `width` pixels a row.
fn alpha_at(pixels: &[u8], width: u32, x: u32, y: u32) -> Option<u8> {
    let index = usize::try_from(y)
        .ok()?
        .checked_mul(usize::try_from(width).ok()?)?
        .checked_add(usize::try_from(x).ok()?)?
        .checked_mul(4)?
        .checked_add(3)?;
    pixels.get(index).copied()
}

/// The dma-bufs a surface draws its frames into on the GPU: linear, one plane each, all of one
/// size and format. The host composites the one a frame shows straight from it, as it would a
/// client's.
#[cfg(unix)]
#[derive(Debug)]
pub struct HostBuffers {
    /// The DRM format.
    pub fourcc: u32,
    /// The DRM format modifier.
    pub modifier: u64,
    pub buffers: Vec<HostBuffer>,
}

/// One of a surface's [`HostBuffers`].
#[cfg(unix)]
#[derive(Debug)]
pub struct HostBuffer {
    pub fd: std::os::fd::OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

/// Where a frame drawn on the GPU stands: the buffer it shows, and the ones the host drew last,
/// which the GPU may still be reading.
#[cfg(unix)]
#[derive(Debug)]
struct OnGpu {
    buffers: Arc<HostBuffers>,
    shown: Option<usize>,
    /// The buffers the host took last, newest first.
    taken: [Option<usize>; 2],
}

/// What a frame holds besides its size: only the host that draws it and the surface that draws into
/// it read this, each through [`HostFrame`].
#[derive(Debug)]
struct Held {
    /// Premultiplied RGBA, `size.0 * 4` bytes a row; nothing for a frame drawn on the GPU.
    pixels: Vec<u8>,
    /// What it shows, cell by cell; `None` for a frame a surface handed over whole, whose drawing
    /// this side has not seen.
    coverage: Option<Coverage>,
    /// The parts drawn since a host last took them.
    damage: Vec<Region>,
    /// The buffers a frame drawn on the GPU is shown from.
    #[cfg(unix)]
    gpu: Option<OnGpu>,
}

/// The pixels a hosted surface shows: premultiplied RGBA, kept by the surface and taken by the host
/// that draws it, which copies only what changed.
///
/// It also keeps what it shows: which part of it holds anything, and which part is wholly opaque. A
/// layer the size of a display that paints a bar costs its bar, not the display, and nothing under
/// an opaque part of it is drawn.
#[derive(Clone, Debug)]
pub struct HostFrame {
    held: Arc<Mutex<Held>>,
    size: (u32, u32),
}

impl HostFrame {
    /// An empty frame of `size` physical pixels.
    #[must_use]
    pub fn new(size: (u32, u32)) -> Self {
        Self {
            held: Arc::new(Mutex::new(Held {
                pixels: vec![0; pixels_in(size)],
                coverage: Some(Coverage::new(size)),
                damage: Vec::new(),
                #[cfg(unix)]
                gpu: None,
            })),
            size,
        }
    }

    /// An empty frame of `size` physical pixels that a surface draws on the GPU into `buffers`
    /// ([`Self::show_buffer`]), reading what it shows there ([`Self::record_cells`]).
    #[cfg(unix)]
    #[must_use]
    pub fn on_gpu(size: (u32, u32), buffers: Arc<HostBuffers>) -> Self {
        Self {
            held: Arc::new(Mutex::new(Held {
                pixels: Vec::new(),
                coverage: Some(Coverage::new(size)),
                damage: Vec::new(),
                gpu: Some(OnGpu {
                    buffers,
                    shown: None,
                    taken: [None; 2],
                }),
            })),
            size,
        }
    }

    /// The frame now shows buffer `index` of its [`HostBuffers`], which changed in `regions` since
    /// the one it showed before.
    #[cfg(unix)]
    pub fn show_buffer(&self, index: usize, regions: &[Region]) {
        let mut held = self.held();
        if let Some(gpu) = held.gpu.as_mut() {
            gpu.shown = Some(index);
            held.damage.extend_from_slice(regions);
        }
    }

    /// The buffers the host drew from last, which the GPU may still be reading: a surface draws
    /// into neither.
    #[cfg(unix)]
    #[must_use]
    pub fn buffers_in_use(&self) -> [Option<usize>; 2] {
        self.held().gpu.as_ref().map_or([None; 2], |gpu| gpu.taken)
    }

    /// What the cells of `area` (frame pixels, its corner on a cell's corner) hold now, as a
    /// surface that draws on the GPU read them there: row by row, `area.width.div_ceil(CELL)` a
    /// row, `0` for an empty cell, `2` for an opaque one, `1` for one partly drawn.
    pub fn record_cells(&self, area: Region, cells: &[u8]) {
        if let Some(coverage) = self.held().coverage.as_mut() {
            coverage.set(area, cells);
        }
    }

    /// A frame showing `pixels` (premultiplied RGBA, `size` physical pixels), for a surface that
    /// draws its frames elsewhere. What it shows is taken to be all of it.
    #[must_use]
    pub fn from_pixels(size: (u32, u32), pixels: Vec<u8>) -> Self {
        Self {
            held: Arc::new(Mutex::new(Held {
                pixels,
                coverage: None,
                damage: vec![Region::new(0, 0, size.0, size.1)],
                #[cfg(unix)]
                gpu: None,
            })),
            size,
        }
    }

    /// The size of the surface in physical pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The pixels' identity: clones of one frame share it, and no live frame shares another's, so a
    /// host keeps what it drew of a frame by it.
    #[must_use]
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.held) as usize
    }

    /// A handle on the frame's pixels that says whether they still are the surface's: a host that
    /// keeps what it drew of a frame drops it once this stops being live.
    #[must_use]
    pub fn watch(&self) -> FrameWatch {
        FrameWatch(Arc::downgrade(&self.held))
    }

    /// The part of the frame it shows anything in, in its own pixels: nothing when the surface has
    /// drawn nothing (or drew it all transparent) and there is no layer to composite.
    #[must_use]
    pub fn shown(&self) -> Option<Region> {
        let held = self.held();
        match &held.coverage {
            Some(coverage) => coverage.shown(self.size),
            None => Some(Region::new(0, 0, self.size.0, self.size.1)),
        }
    }

    /// The alpha of one pixel of the frame, if it lies inside.
    #[must_use]
    pub fn alpha(&self, x: u32, y: u32) -> Option<u8> {
        if x >= self.size.0 || y >= self.size.1 {
            return None;
        }
        alpha_at(&self.held().pixels, self.size.0, x, y)
    }

    /// Copy the `regions` of `pixels` (premultiplied RGBA, `width * 4` bytes a row of this frame's
    /// surface) into the frame, and record what the frame now shows there.
    pub fn set_pixels(&self, pixels: &[u8], regions: &[Region]) {
        if regions.is_empty() {
            return;
        }
        let width = self.size.0 as usize;
        let size = self.size;
        let mut held = self.held();
        for region in regions {
            for row in region.y..region.y.saturating_add(region.height) {
                let start = (row as usize)
                    .saturating_mul(width)
                    .saturating_add(region.x as usize)
                    .saturating_mul(4);
                let end = start.saturating_add((region.width as usize).saturating_mul(4));
                if let (Some(to), Some(from)) =
                    (held.pixels.get_mut(start..end), pixels.get(start..end))
                {
                    to.copy_from_slice(from);
                }
            }
            held.damage.push(*region);
        }
        let Held {
            pixels: drawn,
            coverage,
            ..
        } = &mut *held;
        if let Some(coverage) = coverage.as_mut() {
            coverage.record(drawn, size, regions);
        }
        drop(held);
    }

    /// What changed since a host last took it, with the pixels it changed: `None` where nothing
    /// did. The parts are this call's: the host that takes them draws them and no other.
    #[must_use]
    pub fn take_changed(&self) -> Option<Changed<'_>> {
        let mut held = self.held();
        if held.damage.is_empty() {
            return None;
        }
        let regions = std::mem::take(&mut held.damage);
        #[cfg(unix)]
        if let Some(gpu) = held.gpu.as_mut()
            && gpu.shown != gpu.taken[0]
        {
            gpu.taken = [gpu.shown, gpu.taken[0]];
        }
        Some(Changed {
            held,
            size: self.size,
            regions,
        })
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether the pixels of a frame a host drew still stand: it stops being live once the surface that
/// drew them lets go of the frame.
#[derive(Clone, Debug)]
pub struct FrameWatch(std::sync::Weak<Mutex<Held>>);

impl FrameWatch {
    /// Whether the frame these pixels belong to still stands.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.0.strong_count() > 0
    }
}

/// What changed in a [`HostFrame`] since a host last took it: its pixels, the parts of them that
/// changed, and what it shows.#[derive(Debug)]
pub struct Changed<'a> {
    held: MutexGuard<'a, Held>,
    size: (u32, u32),
    regions: Vec<Region>,
}

impl Changed<'_> {
    /// The frame's pixels: premultiplied RGBA, `size().0 * 4` bytes a row; `None` for a frame drawn
    /// on the GPU, which shows [`Self::buffer`].
    #[must_use]
    pub fn pixels(&self) -> Option<&[u8]> {
        #[cfg(unix)]
        if self.held.gpu.is_some() {
            return None;
        }
        Some(&self.held.pixels)
    }

    /// The buffers of a frame drawn on the GPU, and the one it shows now.
    #[cfg(unix)]
    #[must_use]
    pub fn buffer(&self) -> Option<(&Arc<HostBuffers>, usize)> {
        let gpu = self.held.gpu.as_ref()?;
        Some((&gpu.buffers, gpu.shown?))
    }

    /// The parts of the frame that changed, in its own pixels.
    #[must_use]
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// The parts of the frame that are wholly opaque, in its own pixels: nothing under them is
    /// drawn. `None` where the frame was taken whole and its opacity is not known.
    #[must_use]
    pub fn opaque(&self) -> Option<Vec<Region>> {
        self.held
            .coverage
            .as_ref()
            .map(|coverage| coverage.opaque(self.size))
    }
}

/// The bytes of a `size` surface of premultiplied RGBA.
fn pixels_in(size: (u32, u32)) -> usize {
    (size.0 as usize)
        .saturating_mul(size.1 as usize)
        .saturating_mul(4)
}

/// One frame a hosted surface shows, at a position in the logical layout.
#[derive(Debug, Clone)]
pub struct HostLayer {
    /// The frame's top-left corner in the logical layout.
    pub location: (i32, i32),
    /// The size it covers in the logical layout: the frame is scaled to fit, so a surface draws at
    /// the output's (possibly fractional) scale.
    pub size: (i32, i32),
    /// The pixels.
    pub frame: HostFrame,
}

/// A drag a hosted surface starts: bytes per format (files as `text/uri-list` ) and, optionally, an
/// image under the pointer with its hotspot (logical pixels from its top left).
#[derive(Debug, Clone)]
pub struct HostDrag {
    pub offer: Vec<(String, Vec<u8>)>,
    pub icon: Option<(HostFrame, (i32, i32))>,
}

/// A text field a hosted surface draws, when it is the one the person types into: what an input
/// method serves it with — how to compose in it, and where its composition stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostTextInput {
    /// The field's content hint, as `text-input-v3` names it: what it takes and whether it may be
    /// shown, learned from or completed.
    pub hint: u32,
    /// The field's purpose, as `text-input-v3` names it: text, a name, an email, a URL, digits, a
    /// password, a terminal.
    pub purpose: u32,
    /// Where its caret stands, in the desk's logical coordinates: the candidate window and the
    /// composition go under it.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// What an input method sends a hosted surface's field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostIme {
    /// The composed text is in the field: insert it, replacing any composition shown before.
    Commit(String),
    /// The composition in progress (empty when it ended), with the byte offsets its cursor and
    /// selection stand at (`None` for no selection).
    Preedit(String, Option<(usize, usize)>),
    /// The field is the one an input method serves from now on.
    Enabled,
    /// The field stopped being served: composing in it is over.
    Disabled,
}

/// A hosted surface app, as the host that draws it sees it.
pub trait SurfaceHost {
    /// The layers this surface shows now, bottom first. Hosts draw above every window
    /// ([`Self::below_windows`] aside), later hosts above earlier ones.
    fn layers(&self) -> Vec<HostLayer>;

    /// Whether this surface lies under every window rather than above them: the desktop itself (its
    /// wallpaper, window bars and strips). It takes the pointer only where no window is.
    fn below_windows(&self) -> bool {
        false
    }

    /// Whether this surface shows over the session's lock. Only what the session itself stands up
    /// shows then (the hardware keys' readout, and the lock view when the lock screen's app is not
    /// there), and what wants the keyboard or the pointer of those takes them: a locked session
    /// gives every other surface nothing.
    fn above_lock(&self) -> bool {
        false
    }

    /// Whether this surface is the lock itself, drawn by the session where the lock screen's app
    /// cannot draw it: the lock is what the outputs show, so nothing waits for the app's frame, and
    /// a capture of a locked display is the placeholder as it is for the app's.
    fn drawn_over_lock(&self) -> bool {
        false
    }

    /// Whether a capture may not show what this surface draws now: it shows a secret, such as what
    /// the person typed into a password field. Every copy of the desktop's pixels — a screenshot, a
    /// recording, a share stream, the portal's screenshot — draws its layers as the placeholder
    /// instead, while the person's own display keeps showing them.
    fn withholds_captures(&self) -> bool {
        false
    }

    /// Whether the pointer at this logical point belongs to this surface rather than to what lies
    /// beneath it.
    fn accepts_pointer(&self, x: f64, y: f64) -> bool;

    /// The pointer moved to this logical point over the surface.
    fn pointer_motion(&mut self, x: f64, y: f64);

    /// The pointer left the surface.
    fn pointer_leave(&mut self) {}

    /// A pointer button (evdev code) changed state over the surface.
    fn pointer_button(&mut self, button: u32, pressed: bool);

    /// A pointer button was pressed at this logical point, somewhere other than this surface (a
    /// window, the desk, another surface): an open popover closes.
    fn pressed_elsewhere(&mut self, _x: f64, _y: f64) {}

    /// What the surface's owner closes when the pointer is pressed somewhere other than this
    /// surface. The handle the session shares with its host runs it before
    /// [`SurfaceHost::pressed_elsewhere`], and before holding the surface, since closing may ask the
    /// surface to draw again.
    fn press_elsewhere_closes(&self) -> Option<Rc<dyn Fn()>> {
        None
    }

    /// The pointer button that press came from is up again, at this logical point: the press is
    /// over, and so is everything it was still doing.
    fn press_ended(&mut self, _x: f64, _y: f64) {}

    /// Scroll over the surface, in logical pixels.
    fn pointer_axis(&mut self, _horizontal: f64, _vertical: f64) {}

    /// Whether keys should come to this surface instead of the focused window (an open launcher or
    /// menu).
    fn wants_keyboard(&self) -> bool {
        false
    }

    /// A key, while [`SurfaceHost::wants_keyboard`] holds, or one this surface claimed (its press,
    /// repeats and release).
    fn key(&mut self, _key: &HostKey) {}

    /// The text field this surface draws that the person types into now, when it has the focus: an
    /// input method serves it as it serves a client's field — composing into it, its candidate
    /// window under the caret — and keys it does not use come back to the field.
    fn text_input(&self) -> Option<HostTextInput> {
        None
    }

    /// What the input method serving [`SurfaceHost::text_input`] sent.
    fn ime(&mut self, _event: HostIme) {}

    /// Offered every press before whoever has the keyboard gets it, the topmost surface first: a
    /// desktop chord or a hardware key this surface answers wherever the keyboard is. `locked`
    /// while the session is locked, when only keys that reveal nothing (volume, brightness) may be
    /// taken. `inhibited` while a focused client holds the desktop's shortcuts, when only what must
    /// never be given up (hardware keys, locking the session) may be taken.
    fn claim_key(&mut self, _key: &HostKey, _locked: bool, _inhibited: bool) -> KeyClaim {
        KeyClaim::Pass
    }

    /// Other clients' windows this surface pictures (thumbnails, cards), bottom first: the host
    /// draws their live buffers there, in front of this surface's layers. A window the session
    /// keeps private is drawn as the placeholder a capture of it shows instead, since the desk a
    /// picture stands on is what a screenshot, a recording and a share show.
    fn window_pictures(&self) -> Vec<WindowPicture> {
        Vec::new()
    }

    /// What this surface draws over the windows, by the window each part is of, in the logical
    /// layout: a floating window's bar and body, whole and opaque. A surface below the windows draws
    /// nothing over them but this: the windows under it are drawn around it, and a point on it
    /// belongs to this surface rather than to the window beneath.
    fn drawn_over_windows(&self) -> Vec<(u64, (i32, i32, i32, i32))> {
        Vec::new()
    }

    /// The regions this surface draws translucent material in, which take a blur of what lies below
    /// them.
    fn backdrops(&self) -> Vec<BackdropRequest> {
        Vec::new()
    }

    /// Blurred images of what lies below [`SurfaceHost::backdrops`]' regions, in their order;
    /// `None` where nothing changed since the last.
    fn set_backdrops(&mut self, _images: Vec<Option<BackdropImage>>) {}

    /// A frame is about to be drawn: bring the layers up to date, and say whether this surface drew
    /// or changed what it shows. A host that did not leaves the frame to whoever else has something
    /// new; a host that never knows draws every frame (`true`).
    fn prepare_frame(&mut self) -> bool {
        true
    }

    /// Keep what it holds in step without drawing any of it: what a surface the shell stands in
    /// front of still does every frame, so the changes the person and the session ask of it take
    /// effect even though nothing of it shows. Whether any of it moved, so the frame it stands
    /// under is drawn again.
    fn advance(&mut self) -> bool {
        false
    }

    /// When this surface next needs a frame with no input to bring one: now when a wake it took has
    /// not drawn yet, else the earliest deadline its content sets (a banner's exit, a caret's
    /// blink). A surface whose content only moves draws on the display's cadence the host gave it
    /// ([`SurfaceHost::set_frame_interval`]), not once per turn of the loop. The loop waits no
    /// longer than this; `None` leaves it to the loop's own idle.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        None
    }

    /// How long the displays this surface is shown on hold each frame: a frame of its own is due
    /// once per one of these, so the screens show as many frames as they run at and not one per
    /// turn of the loop.
    fn set_frame_interval(&mut self, _interval: std::time::Duration) {}

    /// The outputs as they stand, now and whenever one is added, removed, moved or scaled: each
    /// one's logical rectangle and the scale it draws at. A surface covering the desktop (the top
    /// bar, the chrome above the windows) follows them, and draws at the scale of the output it is
    /// on so what it paints is as crisp as the display allows.
    fn outputs_changed(&mut self, _outputs: &[HostedOutput]) {}

    /// The pointer shape while the pointer is over the surface (a text beam over a field, a hand
    /// over a link).
    fn cursor(&self) -> CursorIcon {
        CursorIcon::Default
    }

    /// A drag (from a window or a hosted surface) is over the surface at this logical point,
    /// offering these formats (files as `text/uri-list` ): the format the surface takes if it is
    /// dropped here, or `None` to refuse it. Asked again as it moves.
    fn drop_offered(&mut self, _mimes: &[String], _x: f64, _y: f64) -> Option<String> {
        None
    }

    /// The drag left the surface without a drop.
    fn drop_left(&mut self) {}

    /// The drag was dropped on the surface: its data in the format [`SurfaceHost::drop_offered`]
    /// chose, once it has been read.
    fn drop_data(&mut self, _mime: &str, _data: Vec<u8>) {}

    /// A drag the app started while handling the input just routed to it (a press held and moved on
    /// something draggable); the host carries it as a drag of the surface's own.
    fn take_drag(&mut self) -> Option<HostDrag> {
        None
    }

    /// A drag this surface started ended: `dropped` when a target took it.
    fn drag_ended(&mut self, _dropped: bool) {}
}

/// Forwards a listed method of a shared surface to the surface, borrowing it for the call alone.
macro_rules! forwards {
    (read: $($name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
        $(fn $name(&self $(, $arg: $ty)*) $(-> $ret)? { self.borrow().$name($($arg),*) })*
    };
    (write: $($name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
        $(fn $name(&mut self $(, $arg: $ty)*) $(-> $ret)? { self.borrow_mut().$name($($arg),*) })*
    };
}

/// A surface the session hands to its host while keeping a handle of its own: the host drives the
/// same instance the role reads, and the owner closes what it has open before the surface hears the
/// press.
impl<T: SurfaceHost + ?Sized> SurfaceHost for Rc<RefCell<T>> {
    forwards! {
        read:
        layers() -> Vec<HostLayer>;
        below_windows() -> bool;
        above_lock() -> bool;
        drawn_over_lock() -> bool;
        withholds_captures() -> bool;
        accepts_pointer(x: f64, y: f64) -> bool;
        wants_keyboard() -> bool;
        text_input() -> Option<HostTextInput>;
        window_pictures() -> Vec<WindowPicture>;
        drawn_over_windows() -> Vec<(u64, (i32, i32, i32, i32))>;
        backdrops() -> Vec<BackdropRequest>;
        next_deadline() -> Option<std::time::Instant>;
        cursor() -> CursorIcon;
        press_elsewhere_closes() -> Option<Rc<dyn Fn()>>;
    }
    forwards! {
        write:
        pointer_motion(x: f64, y: f64);
        pointer_leave();
        pointer_button(button: u32, pressed: bool);
        press_ended(x: f64, y: f64);
        pointer_axis(horizontal: f64, vertical: f64);
        key(key: &HostKey);
        ime(event: HostIme);
        claim_key(key: &HostKey, locked: bool, inhibited: bool) -> KeyClaim;
        set_backdrops(images: Vec<Option<BackdropImage>>);
        prepare_frame() -> bool;
        advance() -> bool;
        set_frame_interval(interval: std::time::Duration);
        outputs_changed(outputs: &[HostedOutput]);
        drop_offered(mimes: &[String], x: f64, y: f64) -> Option<String>;
        drop_left();
        drop_data(mime: &str, data: Vec<u8>);
        take_drag() -> Option<HostDrag>;
        drag_ended(dropped: bool);
    }

    /// Before the surface hears the press, and with the surface let go of: what its owner closes
    /// may ask it to draw again.
    fn pressed_elsewhere(&mut self, x: f64, y: f64) {
        let closes = self.borrow().press_elsewhere_closes();
        if let Some(closes) = closes {
            closes();
        }
        self.borrow_mut().pressed_elsewhere(x, y);
    }
}

#[cfg(test)]
mod tests {
    //! What a frame says it shows and what changed in it, which is what a layer costs.

    use super::*;

    const SIZE: (u32, u32) = (64, 48);

    /// Pixels of a whole frame, `paint` giving each pixel.
    fn pixels(paint: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut pixels = Vec::new();
        for y in 0..SIZE.1 {
            for x in 0..SIZE.0 {
                pixels.extend_from_slice(&paint(x, y));
            }
        }
        pixels
    }

    /// `drawn` with a `width` by `height` block of `pixel` at `(x, y)`.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "a fixture's own pixel offsets, in whole numbers inside the frame it made"
    )]
    fn painted(
        mut drawn: Vec<u8>,
        (x, y): (u32, u32),
        (width, height): (u32, u32),
        pixel: [u8; 4],
    ) -> Vec<u8> {
        for row in y..y.saturating_add(height) {
            for column in x..x.saturating_add(width) {
                let start = (row as usize)
                    .saturating_mul(SIZE.0 as usize)
                    .saturating_add(column as usize)
                    .saturating_mul(4);
                drawn[start..start.saturating_add(4)].copy_from_slice(&pixel);
            }
        }
        drawn
    }

    const OPAQUE_RED: [u8; 4] = [255, 0, 0, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];
    const HALF_BLUE: [u8; 4] = [0, 0, 128, 128];

    #[test]
    fn a_frame_nothing_was_drawn_into_shows_nothing() {
        assert_eq!(
            HostFrame::new(SIZE).shown(),
            None,
            "an empty layer has no layer"
        );
    }

    #[test]
    fn a_frame_shows_the_cells_it_drew_in() {
        let frame = HostFrame::new(SIZE);
        let bar = Region::new(8, 16, 24, 8);
        frame.set_pixels(
            &painted(pixels(|_, _| CLEAR), (8, 16), (24, 8), OPAQUE_RED),
            &[bar],
        );
        assert_eq!(frame.shown(), Some(bar), "the bar it drew, and no more");
        let changed = frame.take_changed().expect("what it drew changed it");
        assert_eq!(changed.regions(), &[bar]);
        assert_eq!(
            changed.opaque(),
            Some(vec![bar]),
            "what it drew over is opaque"
        );
        drop(changed);
    }

    #[test]
    fn what_changed_is_the_hosts_once() {
        let frame = HostFrame::new(SIZE);
        let bar = Region::new(8, 16, 24, 8);
        frame.set_pixels(
            &painted(pixels(|_, _| CLEAR), (8, 16), (24, 8), OPAQUE_RED),
            &[bar],
        );
        assert!(
            frame.take_changed().is_some(),
            "the host takes what changed"
        );
        assert!(
            frame.take_changed().is_none(),
            "and the same parts are not drawn twice"
        );
        frame.set_pixels(
            &painted(pixels(|_, _| CLEAR), (8, 16), (4, 4), CLEAR),
            &[Region::new(8, 16, 4, 4)],
        );
        assert!(
            frame.take_changed().is_some(),
            "a later draw is the host's again"
        );
    }

    #[test]
    fn a_translucent_part_shows_but_is_not_opaque() {
        let frame = HostFrame::new(SIZE);
        let square = Region::new(0, 0, 8, 8);
        frame.set_pixels(
            &painted(pixels(|_, _| CLEAR), (0, 0), (8, 8), HALF_BLUE),
            &[square],
        );
        assert_eq!(frame.shown(), Some(square));
        assert_eq!(
            frame.take_changed().and_then(|changed| changed.opaque()),
            Some(Vec::new()),
            "a translucent cell is not opaque"
        );
    }

    #[test]
    fn a_cell_drawn_over_again_transparent_is_empty_and_no_longer_shown() {
        let frame = HostFrame::new(SIZE);
        let square = Region::new(0, 0, 8, 8);
        frame.set_pixels(
            &painted(pixels(|_, _| CLEAR), (0, 0), (8, 8), OPAQUE_RED),
            &[square],
        );
        assert_eq!(frame.shown(), Some(square));
        frame.set_pixels(&pixels(|_, _| CLEAR), &[square]);
        assert_eq!(
            frame.shown(),
            None,
            "drawing the cell transparent shows nothing there"
        );
    }

    #[test]
    fn the_layers_shown_rectangle_grows_with_what_it_draws_and_shrinks_with_what_it_erases() {
        let frame = HostFrame::new(SIZE);
        let corner = Region::new(48, 40, 8, 8);
        frame.set_pixels(
            &painted(pixels(|_, _| CLEAR), (48, 40), (8, 8), OPAQUE_RED),
            &[corner],
        );
        assert_eq!(frame.shown(), Some(corner));
        // Drawn over the whole cell: an erase of what was there.
        frame.set_pixels(&pixels(|_, _| CLEAR), &[corner]);
        assert_eq!(frame.shown(), None, "nothing is shown there any more");
    }

    #[test]
    fn a_frame_a_surface_draws_elsewhere_is_taken_whole() {
        let frame = HostFrame::from_pixels(SIZE, pixels(|_, _| HALF_BLUE));
        assert_eq!(
            frame.shown(),
            Some(Region::new(0, 0, SIZE.0, SIZE.1)),
            "all of it is what it shows"
        );
        let changed = frame.take_changed().expect("it changed first");
        assert_eq!(changed.regions(), &[Region::new(0, 0, SIZE.0, SIZE.1)]);
        assert_eq!(changed.opaque(), None, "what it shows through is not known");
        drop(changed);
    }

    #[test]
    fn copies_of_a_frame_are_one_frame_and_a_new_one_is_another() {
        let frame = HostFrame::new(SIZE);
        let copy = frame.clone();
        assert_eq!(
            frame.identity(),
            copy.identity(),
            "a clone is the same pixels"
        );
        copy.set_pixels(
            &painted(pixels(|_, _| CLEAR), (8, 16), (8, 8), OPAQUE_RED),
            &[Region::new(8, 16, 8, 8)],
        );
        assert_eq!(
            frame.shown(),
            Some(Region::new(8, 16, 8, 8)),
            "what one copy drew, the other shows"
        );
        assert!(frame.watch().alive());
        let other = HostFrame::new(SIZE);
        assert_ne!(frame.identity(), other.identity());
        let watch = other.watch();
        drop(other);
        assert!(!watch.alive(), "a frame its surface let go is gone");
    }

    #[test]
    fn a_pixel_of_the_frame_answers_its_alpha() {
        let frame = HostFrame::new(SIZE);
        assert_eq!(frame.alpha(0, 0), Some(0), "an empty frame is clear");
        assert_eq!(frame.alpha(SIZE.0, 0), None, "outside it there is no pixel");
        frame.set_pixels(
            &painted(pixels(|_, _| CLEAR), (8, 16), (1, 1), OPAQUE_RED),
            &[Region::new(8, 16, 1, 1)],
        );
        assert_eq!(frame.alpha(8, 16), Some(255));
    }

    /// A surface that records what it heard.
    struct Pressed {
        heard: Rc<RefCell<Vec<&'static str>>>,
        closes: Option<Rc<dyn Fn()>>,
    }

    impl SurfaceHost for Pressed {
        fn layers(&self) -> Vec<HostLayer> {
            Vec::new()
        }

        fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
            false
        }

        fn pointer_motion(&mut self, _x: f64, _y: f64) {}

        fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

        fn pressed_elsewhere(&mut self, _x: f64, _y: f64) {
            self.heard.borrow_mut().push("press");
        }

        fn press_elsewhere_closes(&self) -> Option<Rc<dyn Fn()>> {
            self.closes.clone()
        }
    }

    #[test]
    fn a_shared_surface_closes_what_its_owner_has_open_before_it_hears_the_press() {
        let heard: Rc<RefCell<Vec<&'static str>>> = Rc::default();
        // What the owner closes reaches back into the surface it stands on, as drawing that surface
        // again does: it must not find the surface held by whoever is delivering the press.
        let mut shared: Rc<RefCell<Pressed>> =
            Rc::new_cyclic(|placed: &std::rc::Weak<RefCell<Pressed>>| {
                let placed = placed.clone();
                let recorded = Rc::clone(&heard);
                RefCell::new(Pressed {
                    heard: Rc::clone(&heard),
                    closes: Some(Rc::new(move || {
                        placed
                            .upgrade()
                            .expect("the surface it was placed on")
                            .borrow_mut();
                        recorded.borrow_mut().push("closed");
                    })),
                })
            });
        SurfaceHost::pressed_elsewhere(&mut shared, 10.0, 20.0);
        assert_eq!(
            *heard.borrow(),
            ["closed", "press"],
            "what it has open closes before the press reaches it"
        );
    }

    /// Three buffers, as a surface that draws on the GPU delivers its frames in: their memory is
    /// never read here, so any file stands in for a dma-buf.
    #[cfg(unix)]
    fn buffers() -> Arc<HostBuffers> {
        Arc::new(HostBuffers {
            fourcc: 0,
            modifier: 0,
            buffers: (0..3)
                .map(|_| HostBuffer {
                    fd: std::fs::File::open("/dev/null").expect("/dev/null").into(),
                    offset: 0,
                    stride: 256,
                })
                .collect(),
        })
    }

    /// A frame drawn on the GPU hands the host the buffer it shows and what changed, and the
    /// surface never draws into the buffers the host drew from last, which the GPU may still be
    /// reading.
    #[cfg(unix)]
    #[test]
    fn the_host_shows_the_newest_buffer_and_the_surface_spares_the_ones_it_read() {
        let frame = HostFrame::on_gpu(SIZE, buffers());
        assert!(
            frame.take_changed().is_none(),
            "nothing drawn, nothing to show"
        );
        let bar = Region::new(0, 0, 64, 8);
        frame.show_buffer(0, &[bar]);
        let changed = frame.take_changed().expect("a frame was drawn");
        assert_eq!(changed.pixels(), None, "its pixels stay on the GPU");
        assert_eq!(changed.buffer().map(|(_, index)| index), Some(0));
        assert_eq!(changed.regions(), [bar]);
        drop(changed);
        assert_eq!(frame.buffers_in_use(), [Some(0), None]);
        // Two frames before the host draws again: it shows the newer, with both frames' changes.
        let glyph = Region::new(8, 16, 8, 8);
        frame.show_buffer(1, &[bar]);
        frame.show_buffer(2, &[glyph]);
        assert_eq!(frame.buffers_in_use(), [Some(0), None]);
        let changed = frame.take_changed().expect("frames were drawn");
        assert_eq!(changed.buffer().map(|(_, index)| index), Some(2));
        assert_eq!(changed.regions(), [bar, glyph]);
        drop(changed);
        assert_eq!(
            frame.buffers_in_use(),
            [Some(2), Some(0)],
            "the buffer drawn now and the one drawn before it"
        );
    }

    /// What a frame drawn on the GPU shows is read there, cell by cell, and it says so as one drawn
    /// in memory does: a layer costs what it draws, and nothing under its opaque part is drawn.
    #[cfg(unix)]
    #[test]
    fn a_frame_drawn_on_the_gpu_shows_the_cells_read_there() {
        let frame = HostFrame::on_gpu(SIZE, buffers());
        assert_eq!(frame.shown(), None);
        // A 32 by 16 area at (32, 8): its cells row by row, four a row.
        frame.record_cells(Region::new(32, 8, 32, 16), &[2, 2, 0, 0, 2, 2, 1, 0]);
        frame.show_buffer(0, &[Region::new(32, 8, 32, 16)]);
        assert_eq!(frame.shown(), Some(Region::new(32, 8, 24, 16)));
        let changed = frame.take_changed().expect("drawn");
        assert_eq!(
            changed.opaque(),
            Some(vec![Region::new(32, 8, 16, 16)]),
            "the opaque cells, merged down the rows"
        );
        drop(changed);
        // The same area read again, empty now: the layer shows nothing.
        frame.record_cells(Region::new(32, 8, 32, 16), &[0; 8]);
        assert_eq!(frame.shown(), None);
    }
}
