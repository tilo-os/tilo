//! A page's frames as the rects that changed. A caret that blinks or a
//! spinner that turns repaints a few pixels; the host sends just those rects
//! (packet kind 4, `--tilo-frame-damage`) and this process applies them to
//! the tab's frame it keeps whole ([`FrameStore`]), so the frame is neither
//! sent, converted nor compared whole again, and the view repaints and
//! uploads only where it changed ([`crate::webview::FrameDamage`]).

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::Arc;

use super::protocol::{self, MAX_PACKET, invalid};
use crate::webview::{Frame, FrameDamage, PixelRect};

/// The most rects one damage packet carries: the host sends at most 16, an
/// owner passing a slow client's on folds several into one.
const MAX_RECTS: usize = 256;

/// Generations of rects a frame carries for a view that missed some.
const RECENT: usize = 8;

/// The rects of a tab's frame that changed, as the host sends them: BGRA,
/// each rect's rows in turn, for the frame of these dimensions it sent last.
pub(super) struct WireDamage {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) rects: Vec<PixelRect>,
    pub(super) pixels: Vec<u8>,
}

impl WireDamage {
    /// Writes the rects, in turn, into `frame`, a whole frame of the same
    /// dimensions and pixel format.
    pub(super) fn apply_to(&self, frame: &mut [u8]) {
        let stride = self.width as usize * 4;
        let mut at = 0;
        for rect in &self.rects {
            let row = rect.width as usize * 4;
            for y in rect.y..rect.y + rect.height {
                let start = y as usize * stride + rect.x as usize * 4;
                if let (Some(to), Some(from)) = (
                    frame.get_mut(start..start + row),
                    self.pixels.get(at..at + row),
                ) {
                    to.copy_from_slice(from);
                }
                at += row;
            }
        }
    }

    /// Whether `frame` already shows every rect as it is here.
    fn shown_in(&self, frame: &[u8]) -> bool {
        let stride = self.width as usize * 4;
        let mut at = 0;
        self.rects.iter().all(|rect| {
            let row = rect.width as usize * 4;
            (rect.y..rect.y + rect.height).all(|y| {
                let start = y as usize * stride + rect.x as usize * 4;
                let same = frame.get(start..start + row) == self.pixels.get(at..at + row);
                at += row;
                same
            })
        })
    }

    /// `later` folded into this, as applying both in turn would change the
    /// frame. Hands it back, and leaves this as it was, when they are for
    /// frames of other dimensions or together pass a whole frame's size.
    pub(super) fn absorb(&mut self, later: Self) -> Result<(), Self> {
        let whole = self.width as usize * self.height as usize * 4;
        if (later.width, later.height) != (self.width, self.height)
            || self.rects.len() + later.rects.len() > MAX_RECTS
            || self.pixels.len() + later.pixels.len() > whole
        {
            return Err(later);
        }
        self.rects.extend_from_slice(&later.rects);
        self.pixels.extend_from_slice(&later.pixels);
        Ok(())
    }
}

/// Reads the body of a damage packet whose length (counting its kind byte)
/// is `length`: the tab, the frame's dimensions, the rects and their pixels.
pub(super) fn read_body(reader: &mut impl Read, length: u32) -> io::Result<(u32, WireDamage)> {
    let truncated = || invalid("Truncated Chromium damage packet");
    let mut header = [0; 16];
    let body = length.checked_sub(1 + 16).ok_or_else(truncated)? as usize;
    reader.read_exact(&mut header)?;
    let word = |at: usize| {
        header
            .get(at..at + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .map_or(0, u32::from_le_bytes)
    };
    let (tab, width, height, count) = (word(0), word(4), word(8), word(12) as usize);
    if tab == 0 || tab > i32::MAX as u32 {
        return Err(invalid("Invalid tab ID"));
    }
    if !(1..=8192).contains(&width) || !(1..=8192).contains(&height) {
        return Err(invalid("Invalid Chromium frame dimensions"));
    }
    if !(1..=MAX_RECTS).contains(&count) || count * 16 > body {
        return Err(invalid("Invalid Chromium damage rects"));
    }
    let mut encoded = vec![0; count * 16];
    reader.read_exact(&mut encoded)?;
    let mut rects = Vec::with_capacity(count);
    let mut size = 0usize;
    for rect in encoded.chunks_exact(16) {
        let field = |at: usize| {
            rect.get(at..at + 4)
                .and_then(|bytes| bytes.try_into().ok())
                .map_or(0, u32::from_le_bytes)
        };
        let rect = PixelRect {
            x: field(0),
            y: field(4),
            width: field(8),
            height: field(12),
        };
        if rect.width == 0
            || rect.height == 0
            || rect.x >= width
            || rect.y >= height
            || rect.width > width - rect.x
            || rect.height > height - rect.y
        {
            return Err(invalid("Chromium damage rect outside its frame"));
        }
        size += rect.width as usize * rect.height as usize * 4;
        rects.push(rect);
    }
    if size != body - count * 16 {
        return Err(invalid("Chromium damage pixels do not match its rects"));
    }
    let mut pixels = vec![0; size];
    reader.read_exact(&mut pixels)?;
    Ok((
        tab,
        WireDamage {
            width,
            height,
            rects,
            pixels,
        },
    ))
}

/// Writes `damage` as the host would: a damage packet for `tab`, its pixels
/// as they are.
pub(super) fn write(writer: &mut impl Write, tab: u32, damage: &WireDamage) -> io::Result<()> {
    let length = 1 + 16 + damage.rects.len() * 16 + damage.pixels.len();
    let length = u32::try_from(length)
        .ok()
        .filter(|length| *length <= MAX_PACKET)
        .ok_or_else(|| invalid("Chromium damage exceeds packet limit"))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&[4])?;
    let count = damage.rects.len() as u32;
    for word in [tab, damage.width, damage.height, count] {
        writer.write_all(&word.to_le_bytes())?;
    }
    for rect in &damage.rects {
        for word in [rect.x, rect.y, rect.width, rect.height] {
            writer.write_all(&word.to_le_bytes())?;
        }
    }
    writer.write_all(&damage.pixels)?;
    writer.flush()
}

/// A tab's frame as the host last painted it, kept whole so the rects it
/// sends change only themselves.
#[derive(Default)]
pub(super) struct FrameStore {
    whole: Option<Whole>,
    /// Rises with every frame, across whole ones too, so a view never takes
    /// one frame's rects for another's.
    generation: u64,
}

struct Whole {
    width: u32,
    height: u32,
    rgba: Arc<Vec<u8>>,
    /// What each recent generation changed, newest first.
    recent: VecDeque<(u64, Arc<[PixelRect]>)>,
    /// An earlier generation's pixels, reused for the next frame once no
    /// view holds them, so a small change copies only what changed since.
    spare: Option<(Arc<Vec<u8>>, u64)>,
}

/// What a damage packet did to a [`FrameStore`].
pub(super) enum Applied {
    /// The frame changed: the frame to show.
    Changed(Frame),
    /// Every rect already showed what it carries.
    Unchanged,
    /// There is no frame of its dimensions to change: the host has to send
    /// the frame whole.
    Missing,
}

impl FrameStore {
    /// Makes `frame`, a whole frame, the store's, and gives it its damage.
    /// Returns `false` when it shows exactly what the store has.
    pub(super) fn replace(&mut self, frame: &mut Frame) -> bool {
        if let Some(whole) = &self.whole
            && (whole.width, whole.height) == (frame.width, frame.height)
            && whole.rgba == frame.rgba
        {
            frame.damage = Some(self.damage());
            return false;
        }
        self.generation += 1;
        self.whole = Some(Whole {
            width: frame.width,
            height: frame.height,
            rgba: frame.rgba.clone(),
            recent: VecDeque::new(),
            spare: None,
        });
        frame.damage = Some(self.damage());
        true
    }

    /// The store's frame, for a view that has none.
    pub(super) fn frame(&self, trace_id: Option<u64>) -> Option<Frame> {
        let whole = self.whole.as_ref()?;
        Some(Frame {
            trace_id,
            rgba: whole.rgba.clone(),
            width: whole.width,
            height: whole.height,
            damage: Some(self.damage()),
        })
    }

    fn damage(&self) -> FrameDamage {
        FrameDamage {
            generation: self.generation,
            recent: self
                .whole
                .as_ref()
                .map(|whole| whole.recent.iter().cloned().collect())
                .unwrap_or_else(|| Arc::from([])),
        }
    }

    /// Applies `damage`, sent for tab `tab`, to the store's frame.
    pub(super) fn apply(&mut self, tab: u32, mut damage: WireDamage) -> Applied {
        let Some(whole) = self
            .whole
            .as_mut()
            .filter(|whole| (whole.width, whole.height) == (damage.width, damage.height))
        else {
            return Applied::Missing;
        };
        protocol::bgra_to_rgba(&mut damage.pixels);
        if damage.shown_in(&whole.rgba) {
            return Applied::Unchanged;
        }
        let mut next = whole.take_spare(self.generation);
        damage.apply_to(&mut next);
        let next = Arc::new(next);
        let previous = std::mem::replace(&mut whole.rgba, next);
        whole.spare = Some((previous, self.generation));
        self.generation += 1;
        whole
            .recent
            .push_front((self.generation, Arc::from(damage.rects)));
        whole.recent.truncate(RECENT);
        let (width, height) = (whole.width, whole.height);
        let rgba = whole.rgba.clone();
        Applied::Changed(Frame {
            trace_id: crate::webview::frame_trace::received(tab, &rgba, width, height),
            rgba,
            width,
            height,
            damage: Some(self.damage()),
        })
    }
}

impl Whole {
    /// Pixels to write generation `current + 1` into: the spare brought up
    /// to `current` when no view holds it any more, a copy otherwise.
    fn take_spare(&mut self, current: u64) -> Vec<u8> {
        if let Some((spare, at)) = self.spare.take()
            && let Some(changed) = crate::webview::changed_since(&self.recent, current, at)
            && let Ok(mut pixels) = Arc::try_unwrap(spare)
        {
            let stride = self.width as usize * 4;
            for rect in changed {
                let row = rect.width as usize * 4;
                for y in rect.y..rect.y + rect.height {
                    let start = y as usize * stride + rect.x as usize * 4;
                    if let (Some(to), Some(from)) = (
                        pixels.get_mut(start..start + row),
                        self.rgba.get(start..start + row),
                    ) {
                        to.copy_from_slice(from);
                    }
                }
            }
            return pixels;
        }
        self.rgba.as_ref().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    /// A whole frame of `width`x`height` BGRA pixels, each `[b, g, r, 255]`.
    fn whole(width: u32, height: u32, bgr: [u8; 3]) -> Frame {
        let pixels = [bgr[2], bgr[1], bgr[0], 255].repeat((width * height) as usize);
        Frame {
            trace_id: None,
            rgba: Arc::new(pixels),
            width,
            height,
            damage: None,
        }
    }

    fn damage(width: u32, height: u32, rects: Vec<PixelRect>, bgr: [u8; 3]) -> WireDamage {
        let area: u32 = rects.iter().map(|r| r.width * r.height).sum();
        WireDamage {
            width,
            height,
            rects,
            pixels: [bgr[0], bgr[1], bgr[2], 255].repeat(area as usize),
        }
    }

    fn pixel(frame: &Frame, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * frame.width + x) * 4) as usize;
        frame.rgba[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn a_small_rect_changes_only_its_pixels_and_says_where() {
        let mut store = FrameStore::default();
        let mut first = whole(64, 48, [10, 20, 30]);
        assert!(store.replace(&mut first));
        let drawn = first.damage.clone().unwrap().generation;

        // A caret, 2x16 pixels, blinks on.
        let caret = rect(20, 10, 2, 16);
        let mut packet = Vec::new();
        write(&mut packet, 3, &damage(64, 48, vec![caret], [0, 0, 0])).unwrap();
        assert_eq!(
            packet.len(),
            4 + 1 + 16 + 16 + 2 * 16 * 4,
            "the rect, not the frame"
        );
        let Some(protocol::Packet::TabDamage(tab, sent)) =
            protocol::read_packet(&mut Cursor::new(&packet)).unwrap()
        else {
            panic!("expected damage")
        };
        assert_eq!(tab, 3);
        let Applied::Changed(frame) = store.apply(tab, sent) else {
            panic!("the caret changed the frame")
        };
        assert_eq!(pixel(&frame, 20, 10), [0, 0, 0, 255]);
        assert_eq!(pixel(&frame, 21, 25), [0, 0, 0, 255]);
        for (x, y) in [(19, 10), (22, 10), (20, 9), (20, 26), (0, 0), (63, 47)] {
            assert_eq!(pixel(&frame, x, y), [30, 20, 10, 255], "({x}, {y}) kept");
        }
        let damage_of = frame.damage.clone().unwrap();
        assert_eq!(damage_of.since(drawn), Some(vec![caret]));
        assert_eq!(damage_of.since(damage_of.generation), Some(Vec::new()));
        assert!(
            !Arc::ptr_eq(&frame.rgba, &first.rgba),
            "the frame a view holds is never written"
        );
        assert_eq!(pixel(&first, 20, 10), [30, 20, 10, 255]);

        // The same caret again is no change at all.
        assert!(matches!(
            store.apply(3, damage(64, 48, vec![caret], [0, 0, 0])),
            Applied::Unchanged
        ));

        // Off again, once no view holds the first frame: its pixels are
        // reused, and a view that missed a frame repaints both changes.
        let reused = first.rgba.as_ptr();
        drop(first);
        let other = rect(40, 30, 4, 4);
        let Applied::Changed(next) = store.apply(3, damage(64, 48, vec![other], [1, 2, 3])) else {
            panic!("changed")
        };
        assert_eq!(
            pixel(&next, 20, 10),
            [0, 0, 0, 255],
            "the earlier rect is carried"
        );
        assert_eq!(pixel(&next, 40, 30), [3, 2, 1, 255]);
        assert_eq!(pixel(&next, 0, 0), [30, 20, 10, 255]);
        assert_eq!(
            next.rgba.as_ptr(),
            reused,
            "the first frame's pixels, reused"
        );
        assert_eq!(
            pixel(&frame, 40, 30),
            [30, 20, 10, 255],
            "the shown frame kept"
        );
        assert_eq!(next.damage.unwrap().since(drawn), Some(vec![other, caret]));
    }

    #[test]
    fn damage_without_its_frame_asks_for_the_frame_whole() {
        let mut store = FrameStore::default();
        assert!(matches!(
            store.apply(1, damage(8, 8, vec![rect(0, 0, 1, 1)], [0, 0, 0])),
            Applied::Missing
        ));
        let mut first = whole(8, 8, [0, 0, 0]);
        store.replace(&mut first);
        assert!(matches!(
            store.apply(1, damage(8, 9, vec![rect(0, 0, 1, 1)], [0, 0, 0])),
            Applied::Missing
        ));
        // A whole frame says nothing of what changed before it.
        let mut second = whole(8, 8, [9, 9, 9]);
        assert!(store.replace(&mut second));
        let drawn = first.damage.unwrap().generation;
        assert_eq!(second.damage.unwrap().since(drawn), None);
        // The same pixels again are no new frame.
        let mut again = whole(8, 8, [9, 9, 9]);
        assert!(!store.replace(&mut again));
    }

    #[test]
    fn damage_packets_are_validated_and_fold_in_order() {
        let mut packet = Vec::new();
        write(
            &mut packet,
            2,
            &damage(4, 4, vec![rect(3, 3, 1, 1)], [1, 1, 1]),
        )
        .unwrap();
        assert!(protocol::read_packet(&mut Cursor::new(&packet)).is_ok());
        // A rect past its frame, and pixels that do not match the rects.
        let mut outside = packet.clone();
        outside[21..25].copy_from_slice(&4u32.to_le_bytes());
        assert!(protocol::read_packet(&mut Cursor::new(outside)).is_err());
        let mut short = packet.clone();
        short.pop();
        let length = short.len() as u32 - 4;
        short[..4].copy_from_slice(&length.to_le_bytes());
        assert!(protocol::read_packet(&mut Cursor::new(short)).is_err());
        let mut no_tab = packet;
        no_tab[5..9].copy_from_slice(&0u32.to_le_bytes());
        assert!(protocol::read_packet(&mut Cursor::new(no_tab)).is_err());

        // Two packets folded give what applying both in turn gives.
        let mut earlier = damage(4, 4, vec![rect(0, 0, 2, 2)], [1, 1, 1]);
        earlier
            .absorb(damage(4, 4, vec![rect(1, 1, 2, 2)], [2, 2, 2]))
            .ok()
            .unwrap();
        let mut frame = vec![0; 4 * 4 * 4];
        earlier.apply_to(&mut frame);
        assert_eq!(&frame[0..4], &[1, 1, 1, 255]);
        assert_eq!(&frame[(4 + 1) * 4..(4 + 1) * 4 + 4], &[2, 2, 2, 255]);
        assert!(
            earlier
                .absorb(damage(4, 5, vec![rect(0, 0, 1, 1)], [0, 0, 0]))
                .is_err()
        );
        assert!(
            earlier
                .absorb(damage(4, 4, vec![rect(0, 0, 4, 4)], [0, 0, 0]))
                .is_err(),
            "past a whole frame's size the frame goes whole"
        );
    }
}
