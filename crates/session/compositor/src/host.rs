//! Surface apps the compositor hosts in its own process.
//!
//! The top bar, launcher, quick settings and every other shell surface are
//! apps; the compositor holds none of their code. A [`SurfaceHost`] is how one
//! of them reaches the screen: it hands the compositor buffers to show above
//! the windows and receives the input that lands on them. The in-process host
//! drives an ordinary toolkit app and draws its frames into those buffers;
//! nothing here knows how the pixels were made.

use std::sync::Arc;

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Buffer, Rectangle};

use tilo_session_core::surface::{
    BackdropImage, BackdropRequest, CursorIcon, HostKey, HostedOutput, KeyClaim, Region,
    WindowPicture,
};

/// How many pixels a side of one coverage cell is: the granularity at which a frame says what it
/// shows.
const CELL: u32 = 8;

/// What one cell of a frame holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cell {
    /// Every pixel of it is transparent: nothing is shown there.
    Empty,
    /// Something is drawn in it.
    Partly,
    /// Every pixel of it is opaque: nothing under it shows through.
    Opaque,
}

/// What a frame shows and what of it nothing shows through, by cells: kept as the host draws into
/// the frame, so a layer costs what it draws where it draws it rather than the whole frame it draws
/// in.
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

/// The pixels a hosted surface shows: premultiplied RGBA, kept as the buffer the compositor draws,
/// so a host only hands over what changed.
/// It also keeps what it shows: which part of it holds anything, and which part is wholly opaque. A
/// layer the size of a display that paints a bar costs its bar, not the display, and nothing under
/// an opaque part of it is drawn.
#[derive(Clone, Debug)]
pub struct HostFrame {
    buffer: MemoryRenderBuffer,
    size: (u32, u32),
    /// The cell map of the frames written into it; `None` for a buffer this side did not see drawn,
    /// whose frame is taken whole.
    coverage: Option<Arc<Coverage>>,
    /// The part of the frame it shows anything in, in its own pixels.
    shown: Option<Region>,
}

impl HostFrame {
    /// An empty frame of `size` physical pixels.
    #[must_use]
    pub fn new(size: (u32, u32)) -> Self {
        Self {
            buffer: MemoryRenderBuffer::new(
                smithay::backend::allocator::Fourcc::Abgr8888,
                (
                    i32::try_from(size.0).unwrap_or(i32::MAX),
                    i32::try_from(size.1).unwrap_or(i32::MAX),
                ),
                1,
                smithay::utils::Transform::Normal,
                None,
            ),
            size,
            coverage: Some(Arc::new(Coverage::new(size))),
            shown: None,
        }
    }

    /// The frame as the compositor draws it.
    #[must_use]
    pub const fn buffer(&self) -> &MemoryRenderBuffer {
        &self.buffer
    }

    /// A frame showing `buffer` , a surface of `size` physical pixels: for a host that made its
    /// own. What it shows is taken to be all of it, as this side has not seen it drawn.
    #[must_use]
    pub const fn from_buffer(buffer: MemoryRenderBuffer, size: (u32, u32)) -> Self {
        Self {
            buffer,
            size,
            coverage: None,
            shown: Some(Region::new(0, 0, size.0, size.1)),
        }
    }

    /// The size of the surface in physical pixels.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The part of the frame it shows anything in, in its own pixels: nothing when the host has
    /// drawn nothing (or drew it all transparent) and there is no layer to composite.
    #[must_use]
    pub const fn shown(&self) -> Option<Region> {
        self.shown
    }

    /// Copy the `regions` of `pixels` (premultiplied RGBA, `width * 4` bytes a row of this frame's
    /// surface) into the frame, telling it and the compositor what changed, and what the frame now
    /// shows.
    pub fn set_pixels(&mut self, pixels: &[u8], regions: &[Region]) {
        if regions.is_empty() {
            return;
        }
        let width = self.size.0 as usize;
        let size = self.size;
        let Self {
            buffer,
            coverage,
            shown,
            ..
        } = self;
        let mut context = buffer.render();
        let drawn = context.draw(|memory: &mut [u8]| {
            for region in regions {
                for row in region.y..region.y.saturating_add(region.height) {
                    let start = (row as usize)
                        .saturating_mul(width)
                        .saturating_add(region.x as usize)
                        .saturating_mul(4);
                    let end = start.saturating_add((region.width as usize).saturating_mul(4));
                    if let (Some(to), Some(from)) =
                        (memory.get_mut(start..end), pixels.get(start..end))
                    {
                        to.copy_from_slice(from);
                    }
                }
            }
            let coverage = coverage.as_mut().map(Arc::make_mut);
            if let Some(coverage) = coverage {
                coverage.record(memory, size, regions);
                *shown = coverage.shown(size);
            }
            Ok::<_, std::convert::Infallible>(
                regions
                    .iter()
                    .filter_map(|region| {
                        Some(Rectangle::<i32, Buffer>::new(
                            (i32::try_from(region.x).ok()?, i32::try_from(region.y).ok()?).into(),
                            (
                                i32::try_from(region.width).ok()?,
                                i32::try_from(region.height).ok()?,
                            )
                                .into(),
                        ))
                    })
                    .collect(),
            )
        });
        if let Err(never) = drawn {
            match never {}
        }
        // What nothing under shows through: the compositor draws no layer beneath it and clears no
        // pixel it covers.
        if let Some(coverage) = coverage {
            let opaque = coverage
                .opaque(size)
                .into_iter()
                .filter_map(|region| {
                    Some(Rectangle::<i32, Buffer>::new(
                        (i32::try_from(region.x).ok()?, i32::try_from(region.y).ok()?).into(),
                        (
                            i32::try_from(region.width).ok()?,
                            i32::try_from(region.height).ok()?,
                        )
                            .into(),
                    ))
                })
                .collect();
            context.update_opaque_regions(Some(opaque));
        }
    }
}

/// One buffer a hosted surface shows, at a position in the logical layout.
#[derive(Debug, Clone)]
pub struct HostLayer {
    /// The buffer's top-left corner in the logical layout.
    pub location: (i32, i32),
    /// The size it covers in the logical layout: the buffer is scaled to fit, so a host draws at
    /// the output's (possibly fractional) scale.
    pub size: (i32, i32),
    /// The pixels; the buffer's own render context records what changed.
    pub frame: HostFrame,
}

/// A drag a hosted app starts: bytes per format (files as `text/uri-list` ) and, optionally, an
/// image under the pointer with its hotspot (logical pixels from its top left).
#[derive(Debug, Clone)]
pub struct HostDrag {
    pub offer: Vec<(String, Vec<u8>)>,
    pub icon: Option<(
        HostFrame,
        smithay::utils::Point<i32, smithay::utils::Logical>,
    )>,
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

/// A hosted surface app, as the compositor sees it.
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
    /// instead, while the person's own display keeps showing them (see
    /// [`crate::capture::protect`]).
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

    /// Offered every press before whoever has the keyboard gets it, the
    /// topmost surface first: a desktop chord or a hardware key this
    /// surface answers wherever the keyboard is. `locked` while the session
    /// is locked, when only keys that reveal nothing (volume, brightness)
    /// may be taken. `inhibited` while a focused client holds the desktop's
    /// shortcuts, when only what must never be given up (hardware keys,
    /// locking the session) may be taken.
    fn claim_key(&mut self, _key: &HostKey, _locked: bool, _inhibited: bool) -> KeyClaim {
        KeyClaim::Pass
    }

    /// Other clients' windows this surface pictures (thumbnails, cards),
    /// bottom first: the compositor draws their live buffers there, in
    /// front of this surface's layers. A window the session keeps private
    /// ([`crate::capture::protect`]) is drawn as the placeholder a capture
    /// of it shows instead, since the desk a picture stands on is what a
    /// screenshot, a recording and a share show.
    fn window_pictures(&self) -> Vec<WindowPicture> {
        Vec::new()
    }

    /// What this surface draws over the windows, by the window each part is of, in the logical
    /// layout: a floating window's bar and body, whole and opaque. A surface below the windows
    /// draws nothing over them but this: the windows under it are drawn around it, and a point on
    /// it belongs to this surface rather than to the window beneath.
    fn drawn_over_windows(&self) -> Vec<(u64, crate::LayoutRect)> {
        Vec::new()
    }

    /// The regions this surface draws translucent material in, which take a blur of what lies below
    /// them ([`crate::backdrop`]).
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
    /// blink). A surface whose content only moves draws on the display's cadence the compositor
    /// gave it ([`SurfaceHost::set_frame_interval`]), not once per turn of the loop. The loop waits
    /// no longer than this; `None` leaves it to the loop's own idle.
    fn next_deadline(&self) -> Option<std::time::Instant> {
        None
    }

    /// How long the displays this surface is shown on hold each frame: a frame of its own is due
    /// once per one of these, so the screens show as many frames as they run at and not one per
    /// turn of the loop.
    fn set_frame_interval(&mut self, _interval: std::time::Duration) {}

    /// The outputs as they stand, now and whenever one is added, removed,
    /// moved or scaled: each one's logical rectangle and the scale it draws
    /// at. A surface covering the desktop (the top bar, the chrome above the
    /// windows) follows them, and draws at the scale of the output it is on
    /// so what it paints is as crisp as the display allows.
    fn outputs_changed(&mut self, _outputs: &[HostedOutput]) {}

    /// The pointer shape while the pointer is over the surface (a text beam over a field, a hand
    /// over a link).
    fn cursor(&self) -> CursorIcon {
        CursorIcon::Default
    }

    /// A drag (from a Wayland window, an X11 window or a hosted surface) is over the surface at
    /// this logical point, offering these formats (files as `text/uri-list` ): the format the
    /// surface takes if it is dropped here, or `None` to refuse it. Asked again as it moves.
    fn drop_offered(&mut self, _mimes: &[String], _x: f64, _y: f64) -> Option<String> {
        None
    }

    /// The drag left the surface without a drop.
    fn drop_left(&mut self) {}

    /// The drag was dropped on the surface: its data in the format [`SurfaceHost::drop_offered`]
    /// chose, once it has been read.
    fn drop_data(&mut self, _mime: &str, _data: Vec<u8>) {}

    /// A drag the app started while handling the input just routed to it (a press held and moved on
    /// something draggable); the compositor carries it like [`crate::Compositor::start_host_drag`].
    fn take_drag(&mut self) -> Option<HostDrag> {
        None
    }

    /// A drag this surface started ([`crate::Compositor::start_drag`]) ended: `dropped` when a
    /// target took it.
    fn drag_ended(&mut self, _dropped: bool) {}
}

#[cfg(test)]
mod tests {
    //! What a frame says it shows, which is what a layer costs.

    use super::*;

    const SIZE: (u32, u32) = (64, 48);

    /// A transparent frame of [`SIZE`].
    fn frame() -> HostFrame {
        HostFrame::new(SIZE)
    }

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

    const OPAQUE_RED: [u8; 4] = [255, 0, 0, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];
    const HALF_BLUE: [u8; 4] = [0, 0, 128, 128];

    #[test]
    fn a_frame_nothing_was_drawn_into_shows_nothing() {
        assert_eq!(frame().shown(), None, "an empty layer has no layer");
    }

    #[test]
    fn a_frame_shows_the_cells_it_drew_in() {
        let mut frame = frame();
        let mut drawn = pixels(|_, _| CLEAR);
        for y in 16..24 {
            for x in 8..32 {
                let start = ((y * SIZE.0 + x) * 4) as usize;
                drawn[start..start + 4].copy_from_slice(&OPAQUE_RED);
            }
        }
        frame.set_pixels(&drawn, &[Region::new(8, 16, 24, 8)]);
        assert_eq!(
            frame.shown(),
            Some(Region::new(8, 16, 24, 8)),
            "the bar it drew, and no more"
        );
    }

    #[test]
    fn what_is_wholly_opaque_is_said_to_be() {
        let mut frame = frame();
        frame.set_pixels(&pixels(|_, _| OPAQUE_RED), &[Region::new(0, 0, 64, 48)]);
        let coverage = frame.coverage.clone().expect("a frame this side drew");
        assert_eq!(
            coverage.opaque(SIZE),
            vec![Region::new(0, 0, 64, 48)],
            "the whole frame, as one rectangle"
        );
    }

    #[test]
    fn a_translucent_part_shows_but_is_not_opaque() {
        let mut frame = frame();
        let mut drawn = pixels(|_, _| CLEAR);
        for y in 0..8 {
            for x in 0..8 {
                let start = ((y * SIZE.0 + x) * 4) as usize;
                drawn[start..start + 4].copy_from_slice(&HALF_BLUE);
            }
        }
        frame.set_pixels(&drawn, &[Region::new(0, 0, 8, 8)]);
        assert_eq!(frame.shown(), Some(Region::new(0, 0, 8, 8)));
        let coverage = frame.coverage.clone().expect("a frame this side drew");
        assert_eq!(
            coverage.opaque(SIZE),
            Vec::new(),
            "a translucent cell is not opaque"
        );
    }

    #[test]
    fn a_cell_drawn_over_again_transparent_is_empty_and_no_longer_shown() {
        let mut frame = frame();
        let mut drawn = pixels(|_, _| CLEAR);
        for y in 0..8 {
            for x in 0..8 {
                let start = ((y * SIZE.0 + x) * 4) as usize;
                drawn[start..start + 4].copy_from_slice(&OPAQUE_RED);
            }
        }
        frame.set_pixels(&drawn, &[Region::new(0, 0, 8, 8)]);
        assert_eq!(frame.shown(), Some(Region::new(0, 0, 8, 8)));
        frame.set_pixels(&pixels(|_, _| CLEAR), &[Region::new(0, 0, 8, 8)]);
        assert_eq!(
            frame.shown(),
            None,
            "drawing the cell transparent shows nothing there"
        );
    }

    #[test]
    fn the_layers_shown_rectangle_grows_with_what_it_draws_and_shrinks_with_what_it_erases() {
        let mut frame = frame();
        let mut drawn = pixels(|_, _| CLEAR);
        for y in 40..48 {
            for x in 48..56 {
                let start = ((y * SIZE.0 + x) * 4) as usize;
                drawn[start..start + 4].copy_from_slice(&OPAQUE_RED);
            }
        }
        frame.set_pixels(&drawn, &[Region::new(48, 40, 8, 8)]);
        assert_eq!(frame.shown(), Some(Region::new(48, 40, 8, 8)));
        // Drawn over the whole cell: an erase of what was there.
        frame.set_pixels(&pixels(|_, _| CLEAR), &[Region::new(48, 40, 8, 8)]);
        assert_eq!(frame.shown(), None, "nothing is shown there any more");
    }

    #[test]
    fn a_frame_from_a_hosts_own_buffer_is_taken_whole() {
        let buffer = MemoryRenderBuffer::new(
            smithay::backend::allocator::Fourcc::Abgr8888,
            (64, 48),
            1,
            smithay::utils::Transform::Normal,
            None,
        );
        let frame = HostFrame::from_buffer(buffer, SIZE);
        assert_eq!(
            frame.shown(),
            Some(Region::new(0, 0, 64, 48)),
            "what it shows is unknown, so all of it is drawn"
        );
    }
}
