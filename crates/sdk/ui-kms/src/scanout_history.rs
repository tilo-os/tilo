//! What each dumb buffer holds of the canvas, so a frame copies into a buffer only what that
//! buffer lacks.
//!
//! The canvas is drawn once per frame; the buffers it is copied into take turns on screen. A
//! buffer that last held frame `n` lacks the damage of every frame after `n`. The history keeps
//! the damage of the last [`FRAMES`] frames, at most [`MAX_REGIONS`] regions each, so it stays
//! bounded however long the display runs and however busy a frame was: a frame whose damage does
//! not fit, or covers half the canvas or more, counts as the whole canvas, and a buffer whose
//! frame left the history is copied whole.
//!
//! A buffer's claim names its own identity (its size and row pitch) besides the frame it holds:
//! a buffer made again, or one of another layout, is never taken to hold anything. The claim is
//! dropped before the copy starts and made again only once every byte of it was written, so a
//! copy that fails leaves a buffer that is copied whole next time, never one taken to be current.

use std::collections::VecDeque;

use tilo_cpu_raster::Region;

/// Frames of damage kept.
pub(crate) const FRAMES: usize = 8;

/// Regions one frame, or one copy, may name before it counts as the whole canvas.
pub(crate) const MAX_REGIONS: usize = 64;

/// What a buffer is laid out as: a claim on it holds only while it is the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub width: u32,
    pub height: u32,
    pub pitch: usize,
}

/// The frame a buffer holds, and the layout it held it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Holds {
    frame: u64,
    identity: Identity,
}

/// The damage of the last frames drawn into the canvas.
#[derive(Debug, Default)]
pub(crate) struct History {
    /// Newest last, by frame index; `None` is a frame that changed everything.
    frames: VecDeque<(u64, Option<Vec<Region>>)>,
    latest: u64,
}

impl History {
    /// A frame was drawn into the `width` × `height` canvas, changing `damage` (`None`: all of it).
    pub(crate) fn push(&mut self, damage: Option<&[Region]>, (width, height): (u32, u32)) {
        self.latest = self.latest.saturating_add(1);
        let regions = damage.and_then(|damage| bounded(damage, width, height));
        self.frames.push_back((self.latest, regions));
        while self.frames.len() > FRAMES {
            self.frames.pop_front();
        }
    }

    /// The regions a buffer that `holds` a frame lacks; `None` when it must be copied whole.
    pub(crate) fn lacking(&self, holds: Option<Holds>, identity: Identity) -> Option<Vec<Region>> {
        let holds = holds.filter(|holds| holds.identity == identity)?;
        let oldest = self.frames.front()?.0;
        if holds.frame.saturating_add(1) < oldest {
            return None;
        }
        let mut lacking = Vec::new();
        for (_, regions) in self.frames.iter().filter(|(frame, _)| *frame > holds.frame) {
            lacking.extend_from_slice(regions.as_deref()?);
        }
        bounded(&lacking, identity.width, identity.height)
    }

    /// Bring the buffer `target` (rows of `identity.pitch` bytes) up to date with `canvas`
    /// (premultiplied RGBA, `identity.width * 4` bytes a row), and record what it holds in
    /// `holds`. Returns whether every byte was written; when not, the buffer claims nothing.
    pub(crate) fn bring_up_to_date(
        &self,
        holds: &mut Option<Holds>,
        identity: Identity,
        canvas: &[u8],
        target: &mut [u8],
    ) -> bool {
        let whole = [Region::new(0, 0, identity.width, identity.height)];
        let lacking = self.lacking(holds.take(), identity);
        let regions = lacking.as_deref().unwrap_or(&whole);
        let written =
            crate::display::copy_regions(canvas, identity.width, target, identity.pitch, regions);
        if written {
            *holds = Some(Holds {
                frame: self.latest,
                identity,
            });
        }
        written
    }
}

/// `damage` clipped to the canvas, empty regions dropped; `None` when that is too many regions to
/// keep or half the canvas or more, which a whole copy does no slower.
fn bounded(damage: &[Region], width: u32, height: u32) -> Option<Vec<Region>> {
    let mut regions: Vec<Region> = Vec::with_capacity(damage.len().min(MAX_REGIONS));
    for region in damage {
        let x1 = region.x.saturating_add(region.width).min(width);
        let y1 = region.y.saturating_add(region.height).min(height);
        if x1 <= region.x || y1 <= region.y {
            continue;
        }
        let clipped = Region::new(
            region.x,
            region.y,
            x1.saturating_sub(region.x),
            y1.saturating_sub(region.y),
        );
        if regions.iter().any(|kept| contains(*kept, clipped)) {
            continue;
        }
        regions.retain(|kept| !contains(clipped, *kept));
        if regions.len() >= MAX_REGIONS {
            return None;
        }
        regions.push(clipped);
    }
    let area = |region: &Region| u64::from(region.width).saturating_mul(u64::from(region.height));
    let dirty = regions.iter().map(area).fold(0_u64, u64::saturating_add);
    let full = u64::from(width).saturating_mul(u64::from(height));
    (dirty.saturating_mul(2) < full).then_some(regions)
}

fn contains(outer: Region, inner: Region) -> bool {
    outer.x <= inner.x
        && outer.y <= inner.y
        && right(outer) >= right(inner)
        && bottom(outer) >= bottom(inner)
}

fn right(region: Region) -> u32 {
    region.x.saturating_add(region.width)
}

fn bottom(region: Region) -> u32 {
    region.y.saturating_add(region.height)
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test fixtures: small pixel sizes, an overflow is the failing test"
)]
mod tests {
    use super::*;

    const W: u32 = 64;
    const H: u32 = 48;
    const PITCH: usize = 64 * 4 + 16;
    const IDENTITY: Identity = Identity {
        width: W,
        height: H,
        pitch: PITCH,
    };

    /// A canvas whose every pixel is `value`.
    fn canvas(value: u8) -> Vec<u8> {
        vec![value; (W * H * 4) as usize]
    }

    fn buffer() -> Vec<u8> {
        vec![0; PITCH * H as usize]
    }

    /// The blue byte of the buffer's pixel at (x, y): XRGB8888 is B, G, R, X in memory.
    fn blue(buffer: &[u8], x: u32, y: u32) -> u8 {
        buffer[y as usize * PITCH + x as usize * 4]
    }

    #[test]
    fn a_buffer_gets_what_it_missed_and_nothing_else() {
        let mut history = History::default();
        let (mut front, mut back) = (buffer(), buffer());
        let (mut front_holds, mut back_holds) = (None, None);
        history.push(None, (W, H));
        assert!(history.bring_up_to_date(&mut front_holds, IDENTITY, &canvas(1), &mut front));
        assert_eq!(
            blue(&front, 63, 47),
            1,
            "a buffer that held nothing is copied whole"
        );

        let square = [Region::new(4, 4, 4, 4)];
        history.push(Some(&square), (W, H));
        assert!(history.bring_up_to_date(&mut back_holds, IDENTITY, &canvas(2), &mut back));
        assert_eq!(
            blue(&back, 63, 47),
            2,
            "the back buffer held nothing: whole"
        );

        // The next frame changes another square; the front buffer lacks both squares and
        // gets exactly those.
        let other = [Region::new(20, 20, 4, 4)];
        history.push(Some(&other), (W, H));
        assert_eq!(
            history.lacking(front_holds, IDENTITY),
            Some(vec![square[0], other[0]])
        );
        assert!(history.bring_up_to_date(&mut front_holds, IDENTITY, &canvas(3), &mut front));
        assert_eq!(blue(&front, 5, 5), 3);
        assert_eq!(blue(&front, 21, 21), 3);
        assert_eq!(
            blue(&front, 40, 40),
            1,
            "outside what it missed it is untouched"
        );
    }

    #[test]
    fn the_history_stays_bounded_and_a_buffer_older_than_it_is_copied_whole() {
        let mut history = History::default();
        let mut buffer = buffer();
        let mut holds = None;
        history.push(None, (W, H));
        assert!(history.bring_up_to_date(&mut holds, IDENTITY, &canvas(1), &mut buffer));
        for frame in 0..1000_u32 {
            let damage: Vec<Region> = (0..200)
                .map(|index| Region::new(index % W, (frame + index) % H, 1, 1))
                .collect();
            history.push(Some(&damage), (W, H));
            assert!(history.frames.len() <= FRAMES, "frames stay bounded");
            assert!(
                history
                    .frames
                    .iter()
                    .all(|(_, regions)| regions.as_ref().is_none_or(|r| r.len() <= MAX_REGIONS)),
                "regions per frame stay bounded"
            );
        }
        assert_eq!(
            history.lacking(holds, IDENTITY),
            None,
            "a buffer whose frame left the history is copied whole"
        );
        let mut recent = None;
        history.push(Some(&[Region::new(1, 1, 2, 2)]), (W, H));
        assert!(history.bring_up_to_date(&mut recent, IDENTITY, &canvas(2), &mut buffer));
        for _ in 0..=FRAMES {
            history.push(Some(&[Region::new(0, 0, 1, 1)]), (W, H));
        }
        assert_eq!(
            history.lacking(recent, IDENTITY),
            None,
            "more frames behind than the history keeps: whole"
        );
    }

    #[test]
    fn a_claim_holds_only_for_the_same_buffer_and_only_after_the_write() {
        let mut history = History::default();
        let mut buffer = buffer();
        let mut holds = None;
        history.push(None, (W, H));
        assert!(history.bring_up_to_date(&mut holds, IDENTITY, &canvas(1), &mut buffer));
        history.push(Some(&[Region::new(2, 2, 2, 2)]), (W, H));
        let repitched = Identity {
            pitch: PITCH + 64,
            ..IDENTITY
        };
        assert_eq!(
            history.lacking(holds, repitched),
            None,
            "a buffer of another layout holds nothing"
        );

        // A write that cannot complete (the buffer ends inside the region) claims nothing: the next
        // frame copies the whole buffer rather than trusting a half-written one.
        let mut short = vec![0; PITCH * 3];
        let mut short_holds = holds;
        assert!(!history.bring_up_to_date(&mut short_holds, IDENTITY, &canvas(2), &mut short));
        assert_eq!(short_holds, None, "a failed write commits no claim");
        history.push(Some(&[Region::new(2, 2, 2, 2)]), (W, H));
        assert_eq!(history.lacking(short_holds, IDENTITY), None);

        // Damage of half the canvas or more is kept as the whole of it.
        history.push(Some(&[Region::new(0, 0, W, H / 2)]), (W, H));
        assert_eq!(history.lacking(holds, IDENTITY), None);
    }
}
