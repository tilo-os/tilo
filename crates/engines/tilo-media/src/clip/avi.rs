//! The desktop recorder: a self-contained Motion-JPEG / AVI encoder.
//!
//! Each RGBA frame handed to [`Recorder`] is JPEG-encoded and the clip muxed
//! into a RIFF/AVI container with an `MJPG` stream and an `idx1` index, so the
//! result is a playable `.avi` file. Frames come from a webcam or from the
//! procedural frames of the headless demo and tests alike, with no `GStreamer`,
//! ffmpeg or device needed. MJPEG needs no motion estimation or bitstream
//! state, so the encoder stays small and deterministic.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "the AVI and WebM writers do byte arithmetic on sizes the capture bounds (a 63 MiB clip cap, frame extents no wider than a u16) and index the buffers they build; every site is a container field, not a computed decision"
)]

use std::io::Read as _;
use std::path::Path;

use super::{FPS, Frame, JpegFrame};

/// JPEG quality for each recorded frame (0–100). 82 keeps files reasonable
/// while staying visually clean.
const QUALITY: u8 = 82;

/// Hard cap on buffered frames (~30 s at [`FPS`]) so a forgotten recording
/// can't grow without bound. Frames past the cap are dropped.
const MAX_FRAMES: usize = FPS as usize * 30;
/// JPEG entropy varies enormously. The frame-count ceiling alone can still
/// retain hundreds of MiB for noisy 4K input, so cap the actual compressed
/// working set as well. The AVI assembly is done only after recording stops.
/// Leave a full MiB for the AVI header/index so every admitted clip remains
/// below the 64 MiB portable media limit after worker-side muxing.
const MAX_BUFFERED_BYTES: usize = 63 * 1024 * 1024;

/// Accumulates JPEG-encoded frames and muxes them into an AVI on [`finish`].
///
/// [`finish`]: Recorder::finish
pub struct Recorder {
    frames: Vec<Vec<u8>>,
    w: u32,
    h: u32,
    fps: u32,
    buffered_bytes: usize,
    /// The JPEG encoder this build encodes frames with, handed in by the app
    /// that owns the clip: the platform layer compiles no codec of its own.
    jpeg: JpegFrame,
}

impl Recorder {
    pub fn new(fps: u32, jpeg: JpegFrame) -> Self {
        Self {
            frames: Vec::new(),
            w: 0,
            h: 0,
            fps: fps.max(1),
            buffered_bytes: 0,
            jpeg,
        }
    }

    /// JPEG-encode one frame and append it. The first frame fixes the clip
    /// dimensions; frames of a different size are skipped (defensive — real
    /// sources keep a constant size for the duration of a clip). Returns
    /// `true` if the frame was recorded.
    pub fn push(&mut self, frame: &Frame<'_>) -> bool {
        let (width, height) = (frame.width, frame.height);
        if self.frames.len() >= MAX_FRAMES || width == 0 || height == 0 {
            return false;
        }
        if self.frames.is_empty() {
            self.w = width;
            self.h = height;
        } else if width != self.w || height != self.h {
            return false;
        }
        let rgb = rgba_to_rgb(frame.rgba);
        match (self.jpeg)(&rgb, width, height, QUALITY) {
            Some(jpeg) => {
                let Some(next) = self.buffered_bytes.checked_add(jpeg.len()) else {
                    return false;
                };
                if next > MAX_BUFFERED_BYTES {
                    return false;
                }
                self.buffered_bytes = next;
                self.frames.push(jpeg);
                true
            }
            None => false,
        }
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    pub fn duration_ms(&self) -> i64 {
        (self.frames.len() as i64 * 1000) / i64::from(self.fps)
    }

    /// Mux the buffered frames into a complete AVI byte stream, with its
    /// duration, and hand it to `done` here and now: this recorder encodes as
    /// frames arrive, so nothing is left to flush.
    pub fn finish(self, done: Box<dyn FnOnce(super::ClipResult)>) {
        if self.frames.is_empty() {
            done(Err("nothing recorded".to_string()));
            return;
        }
        let duration = self.duration_ms();
        done(Ok((
            encode_avi(&self.frames, self.w, self.h, self.fps),
            duration,
        )));
    }
}

/// Drop the alpha channel: JPEG keeps none, and each pixel keeps its colour
/// values uncomposited.
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.chunks_exact(4) {
        if let [r, g, b, _a] = px {
            out.push(*r);
            out.push(*g);
            out.push(*b);
        }
    }
    out
}

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_tag(buf: &mut Vec<u8>, tag: [u8; 4]) {
    buf.extend_from_slice(&tag);
}

fn avih(w: u32, h: u32, fps: u32, frames: u32, max_frame: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(56);
    push_u32(&mut b, 1_000_000 / fps.max(1)); // dwMicroSecPerFrame
    push_u32(&mut b, max_frame.saturating_mul(fps)); // dwMaxBytesPerSec (approx)
    push_u32(&mut b, 0); // dwPaddingGranularity
    push_u32(&mut b, 0x10); // dwFlags = AVIF_HASINDEX
    push_u32(&mut b, frames); // dwTotalFrames
    push_u32(&mut b, 0); // dwInitialFrames
    push_u32(&mut b, 1); // dwStreams
    push_u32(&mut b, max_frame); // dwSuggestedBufferSize
    push_u32(&mut b, w); // dwWidth
    push_u32(&mut b, h); // dwHeight
    for _ in 0..4 {
        push_u32(&mut b, 0); // dwReserved[4]
    }
    b
}

fn strh(fps: u32, frames: u32, max_frame: u32, w: u32, h: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(56);
    push_tag(&mut b, *b"vids"); // fccType
    push_tag(&mut b, *b"MJPG"); // fccHandler
    push_u32(&mut b, 0); // dwFlags
    push_u16(&mut b, 0); // wPriority
    push_u16(&mut b, 0); // wLanguage
    push_u32(&mut b, 0); // dwInitialFrames
    push_u32(&mut b, 1); // dwScale
    push_u32(&mut b, fps.max(1)); // dwRate → rate/scale = fps
    push_u32(&mut b, 0); // dwStart
    push_u32(&mut b, frames); // dwLength (frames)
    push_u32(&mut b, max_frame); // dwSuggestedBufferSize
    push_u32(&mut b, 0xFFFF_FFFF); // dwQuality (default)
    push_u32(&mut b, 0); // dwSampleSize
    push_u16(&mut b, 0); // rcFrame.left
    push_u16(&mut b, 0); // rcFrame.top
    push_u16(&mut b, w as u16); // rcFrame.right
    push_u16(&mut b, h as u16); // rcFrame.bottom
    b
}

fn strf(w: u32, h: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(40);
    push_u32(&mut b, 40); // biSize
    push_u32(&mut b, w); // biWidth
    push_u32(&mut b, h); // biHeight
    push_u16(&mut b, 1); // one surface, as MJPG decoders read it
    push_u16(&mut b, 24); // biBitCount
    push_tag(&mut b, *b"MJPG"); // biCompression
    push_u32(&mut b, w * h * 3); // biSizeImage
    push_u32(&mut b, 0); // biXPelsPerMeter
    push_u32(&mut b, 0); // biYPelsPerMeter
    push_u32(&mut b, 0); // biClrUsed
    push_u32(&mut b, 0); // biClrImportant
    b
}

fn chunk(tag: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(body.len() + 8);
    push_tag(&mut b, tag);
    push_u32(&mut b, body.len() as u32);
    b.extend_from_slice(body);
    b
}

fn encode_avi(frames: &[Vec<u8>], w: u32, h: u32, fps: u32) -> Vec<u8> {
    let n = frames.len() as u32;
    let max_frame = frames.iter().map(std::vec::Vec::len).max().unwrap_or(0) as u32;

    // --- movi list body: the sequence of "00dc" frame chunks. Track each
    // chunk's offset relative to the "movi" fourcc for the index. ---
    let mut movi = Vec::new();
    let mut index: Vec<(u32, u32)> = Vec::with_capacity(frames.len()); // (offset, size)
    for jpeg in frames {
        // Offset of this chunk's fourcc relative to the "movi" fourcc: the
        // chunks begin 4 bytes after it (right past the fourcc itself).
        let offset = movi.len() as u32 + 4;
        index.push((offset, jpeg.len() as u32));
        push_tag(&mut movi, *b"00dc");
        push_u32(&mut movi, jpeg.len() as u32);
        movi.extend_from_slice(jpeg);
        if jpeg.len() % 2 == 1 {
            movi.push(0); // pad chunks to an even boundary
        }
    }

    // --- idx1 body ---
    let mut idx = Vec::with_capacity(frames.len() * 16);
    for (offset, size) in &index {
        push_tag(&mut idx, *b"00dc");
        push_u32(&mut idx, 0x10); // AVIIF_KEYFRAME
        push_u32(&mut idx, *offset);
        push_u32(&mut idx, *size);
    }

    // --- hdrl list: avih + (strl: strh + strf) ---
    let mut strl = Vec::new();
    push_tag(&mut strl, *b"LIST");
    // size filled below
    let strl_children = {
        let mut c = Vec::new();
        c.extend_from_slice(&chunk(*b"strh", &strh(fps, n, max_frame, w, h)));
        c.extend_from_slice(&chunk(*b"strf", &strf(w, h)));
        c
    };
    push_u32(&mut strl, (4 + strl_children.len()) as u32); // "strl" + children
    push_tag(&mut strl, *b"strl");
    strl.extend_from_slice(&strl_children);

    let mut hdrl = Vec::new();
    push_tag(&mut hdrl, *b"LIST");
    let hdrl_children = {
        let mut c = Vec::new();
        c.extend_from_slice(&chunk(*b"avih", &avih(w, h, fps, n, max_frame)));
        c.extend_from_slice(&strl);
        c
    };
    push_u32(&mut hdrl, (4 + hdrl_children.len()) as u32); // "hdrl" + children
    push_tag(&mut hdrl, *b"hdrl");
    hdrl.extend_from_slice(&hdrl_children);

    // --- movi list wrapper ---
    let mut movi_list = Vec::new();
    push_tag(&mut movi_list, *b"LIST");
    push_u32(&mut movi_list, (4 + movi.len()) as u32); // "movi" + chunks
    push_tag(&mut movi_list, *b"movi");
    movi_list.extend_from_slice(&movi);

    // --- assemble RIFF ---
    let mut body = Vec::new();
    push_tag(&mut body, *b"AVI ");
    body.extend_from_slice(&hdrl);
    body.extend_from_slice(&movi_list);
    body.extend_from_slice(&chunk(*b"idx1", &idx));

    let mut out = Vec::with_capacity(body.len() + 8);
    push_tag(&mut out, *b"RIFF");
    push_u32(&mut out, body.len() as u32);
    out.extend_from_slice(&body);
    out
}

/// Read the `avih` header of an AVI clip: its size and duration, or `None`
/// when the file is not one. The header lives in the first 512 bytes.
pub fn probe_avi(path: &Path) -> Option<(u32, u32, i64)> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 512];
    let mut filled = 0;
    while filled < head.len() {
        let read = file.read(&mut head[filled..]).ok()?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    let head = head.get(..filled)?;
    if head.len() < 12
        || head.get(0..4) != Some(&b"RIFF"[..])
        || head.get(8..12) != Some(&b"AVI "[..])
    {
        return None;
    }
    let pos = find(head, b"avih")?;
    let body = pos + 8; // skip "avih" + size
    if body + 40 > head.len() {
        return None;
    }
    let rd = |off: usize| -> u32 {
        head.get(off..off + 4)
            .and_then(|s| s.try_into().ok())
            .map_or(0, u32::from_le_bytes)
    };
    let usec_per_frame = rd(body);
    let total_frames = rd(body + 16);
    let w = rd(body + 32);
    let h = rd(body + 36);
    // Recover the frame rate from the (integer-microsecond) frame period and
    // derive the duration as frames ÷ fps — how a player times an AVI — so it
    // matches `Recorder::duration_ms` exactly rather than drifting by the
    // microsecond-truncation of `dwMicroSecPerFrame`.
    let fps = (1_000_000 + usec_per_frame / 2)
        .checked_div(usec_per_frame)
        .map_or(FPS, |v| v.max(1));
    let dur_ms = (i64::from(total_frames) * 1000) / i64::from(fps);
    Some((w, h, dur_ms))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
