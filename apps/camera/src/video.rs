//! Video recording — a self-contained Motion-JPEG / AVI encoder.
//!
//! Each RGBA frame handed to [`Recorder`] is JPEG-encoded (via the `image`
//! crate) and the finished clip is muxed
//! into a standards-compliant RIFF/AVI container with an `MJPG` video stream
//! and an `idx1` index. The result is a playable `.avi` file rather than a
//! directory of PNG frames.
//!
//! This path is deliberately codec-in-Rust and source-agnostic: the same
//! [`Recorder`] records frames coming off a webcam (the `live-capture`
//! pipeline hands it RGBA buffers) and the procedural frames used by the
//! headless demo/tests. No `GStreamer`, no ffmpeg, no device required to
//! produce or verify an encoded video.
//!
//! MJPEG (one JPEG per frame) is chosen over an inter-frame codec on purpose:
//! it needs no motion estimation or bitstream state, so the encoder is small
//! and fully deterministic, while remaining a widely-supported video codec.

use std::io;
use std::path::Path;

use crate::frame::{self, FrameBuf};

/// Frames per second written into the container timing. A clip records at
/// whatever rate frames actually arrive; 30 is the standard assumption used
/// for the `dwRate/dwScale` header and duration math.
pub const FPS: u32 = 30;

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
}

impl Recorder {
    /// A recorder that will tag the clip at `fps` frames per second.
    pub fn new(fps: u32) -> Self {
        Self {
            frames: Vec::new(),
            w: 0,
            h: 0,
            fps: fps.max(1),
            buffered_bytes: 0,
        }
    }

    /// JPEG-encode one frame and append it. The first frame fixes the clip
    /// dimensions; frames of a different size are skipped (defensive — real
    /// sources keep a constant size for the duration of a clip). Returns
    /// `true` if the frame was recorded.
    pub fn push(&mut self, fb: &FrameBuf) -> bool {
        if self.frames.len() >= MAX_FRAMES || fb.w == 0 || fb.h == 0 {
            return false;
        }
        if self.frames.is_empty() {
            self.w = fb.w;
            self.h = fb.h;
        } else if fb.w != self.w || fb.h != self.h {
            return false;
        }
        let rgb = frame::rgba_to_rgb(&fb.rgba);
        match encode_jpeg(&rgb, fb.w, fb.h) {
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

    /// Clip duration in milliseconds implied by the frame count and fps.
    pub fn duration_ms(&self) -> i64 {
        (self.frames.len() as i64 * 1000) / i64::from(self.fps)
    }

    /// Mux the buffered frames into a complete AVI byte stream. Returns
    /// `None` if nothing was recorded.
    pub fn finish(self) -> Option<Vec<u8>> {
        if self.frames.is_empty() {
            return None;
        }
        Some(encode_avi(&self.frames, self.w, self.h, self.fps))
    }
}

/// JPEG-encode a tightly-packed RGB8 buffer.
fn encode_jpeg(rgb: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    ui::jpeg::encode_rgb8(w, h, rgb, QUALITY).ok()
}

// ---- RIFF / AVI muxer ------------------------------------------------

fn push_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn push_tag(buf: &mut Vec<u8>, tag: [u8; 4]) {
    buf.extend_from_slice(&tag);
}

/// Build the 56-byte `avih` main-header body.
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

/// Build the 56-byte `strh` video stream-header body.
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

/// Build the 40-byte `strf` BITMAPINFOHEADER body for an MJPEG stream.
fn strf(w: u32, h: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(40);
    push_u32(&mut b, 40); // biSize
    push_u32(&mut b, w); // biWidth
    push_u32(&mut b, h); // biHeight
    push_u16(&mut b, 1); // biPlanes
    push_u16(&mut b, 24); // biBitCount
    push_tag(&mut b, *b"MJPG"); // biCompression
    push_u32(&mut b, w * h * 3); // biSizeImage
    push_u32(&mut b, 0); // biXPelsPerMeter
    push_u32(&mut b, 0); // biYPelsPerMeter
    push_u32(&mut b, 0); // biClrUsed
    push_u32(&mut b, 0); // biClrImportant
    b
}

/// Wrap `body` as `tag <size> body` with the size field preceding it.
fn chunk(tag: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(body.len() + 8);
    push_tag(&mut b, tag);
    push_u32(&mut b, body.len() as u32);
    b.extend_from_slice(body);
    b
}

/// Mux JPEG `frames` into a complete AVI byte stream.
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

// ---- probe -----------------------------------------------------------

/// The largest clip [`probe_avi`] reads (whole; the header is its first
/// 512 bytes).
#[cfg(test)]
const PROBE_READ_LIMIT: usize = 256 * 1024 * 1024;

/// Read the leading AVI headers of `path` and return `(width, height,
/// duration_ms)`: `None` if the file is not a readable AVI. Tests use it to
/// check what the recorder wrote.
#[cfg(test)]
pub fn probe_avi(path: &Path) -> Option<(u32, u32, i64)> {
    // The `avih` header lives near the top of the file; a small prefix is
    // enough. Read up to 512 bytes.
    let file = crate::files::read_bounded(path, PROBE_READ_LIMIT)?;
    let head = file.get(..file.len().min(512))?;
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

#[cfg(test)]
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Persist an encoded clip to `dir` as a timestamped `.avi`, creating the
/// directory on first use. Returns the written file name.
///
/// # Errors
///
/// Returns an error if `dir` cannot be created or the clip cannot be
/// written to it.
pub fn write_clip(dir: &Path, bytes: &[u8], stamp: &str) -> io::Result<String> {
    let name = format!("VID_{stamp}.avi");
    crate::files::write(&dir.join(&name), bytes)?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(width: u32, height: u32, red: u8, green: u8, blue: u8) -> FrameBuf {
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for px in rgba.chunks_exact_mut(4) {
            px[0] = red;
            px[1] = green;
            px[2] = blue;
            px[3] = 255;
        }
        FrameBuf {
            rgba,
            w: width,
            h: height,
        }
    }

    #[test]
    fn recorder_writes_probeable_avi_of_real_jpeg_frames_and_rejects_bad_input() {
        // Phase `recorder_encodes_real_jpeg_frames`.
        {
            let mut rec = Recorder::new(FPS);
            assert!(rec.is_empty());
            for i in 0..5u8 {
                assert!(rec.push(&solid_frame(64, 48, i * 40, 100, 200)));
            }
            assert_eq!(rec.frame_count(), 5);
            // 5 frames at 30fps ≈ 166 ms.
            assert_eq!(rec.duration_ms(), 166);
            let avi = rec.finish().expect("non-empty clip encodes");

            // Real RIFF/AVI container framing.
            assert_eq!(&avi[0..4], b"RIFF");
            assert_eq!(&avi[8..12], b"AVI ");
            // RIFF size field matches the actual byte count.
            let riff_size = u32::from_le_bytes([avi[4], avi[5], avi[6], avi[7]]) as usize;
            assert_eq!(riff_size, avi.len() - 8);
            // The movi list and a per-frame index are present.
            assert!(find(&avi, b"movi").is_some());
            assert!(find(&avi, b"idx1").is_some());
            // Exactly five "00dc" frame chunks were written into movi.
            let dc = avi.windows(4).filter(|w| *w == b"00dc").count();
            // Each frame appears once in movi and once in idx1.
            assert_eq!(dc, 5 * 2);
        }
        // Phase `recorded_frame_is_a_decodable_jpeg`.
        {
            // Prove the embedded frames are genuine JPEGs by decoding one back.
            let mut rec = Recorder::new(FPS);
            rec.push(&solid_frame(80, 60, 10, 220, 30));
            let avi = rec.finish().unwrap();

            // Locate the first "00dc" chunk and pull its payload.
            let movi = find(&avi, b"movi").unwrap();
            let dc = find(&avi[movi..], b"00dc").unwrap() + movi;
            let size =
                u32::from_le_bytes([avi[dc + 4], avi[dc + 5], avi[dc + 6], avi[dc + 7]]) as usize;
            let payload = &avi[dc + 8..dc + 8 + size];
            assert_eq!(&payload[0..2], &[0xFF, 0xD8], "JPEG SOI marker");

            let decoded = image::load_from_memory_with_format(payload, image::ImageFormat::Jpeg)
                .expect("embedded frame decodes as JPEG");
            assert_eq!((decoded.width(), decoded.height()), (80, 60));
        }
        // Phase `write_and_probe_round_trips`.
        {
            let mut rec = Recorder::new(FPS);
            for _ in 0..12 {
                rec.push(&solid_frame(48, 32, 90, 90, 90));
            }
            let bytes = rec.finish().unwrap();

            let dir = std::env::temp_dir().join(format!("camera-vid-test-{}", std::process::id()));
            let name = write_clip(&dir, &bytes, "20260101_000000_000").unwrap();
            assert!(
                name.starts_with("VID_")
                    && Path::new(&name)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("avi"))
            );

            let (w, h, dur) = probe_avi(&dir.join(&name)).expect("probe the written clip");
            assert_eq!((w, h), (48, 32));
            // 12 frames at 30 fps ≈ 400 ms.
            assert_eq!(dur, 400);
            std::fs::remove_dir_all(&dir).ok();
        }
        // Phase `empty_recorder_finishes_none`.
        {
            assert!(Recorder::new(FPS).finish().is_none());
        }
        // Phase `frames_of_a_different_size_are_skipped`.
        {
            let mut rec = Recorder::new(FPS);
            assert!(rec.push(&solid_frame(32, 32, 1, 2, 3)));
            assert!(
                !rec.push(&solid_frame(64, 64, 4, 5, 6)),
                "size change rejected"
            );
            assert_eq!(rec.frame_count(), 1);
        }
    }
}
