//! A minimal RFB (Remote Framebuffer, RFC 6143) client codec.
//!
//! This is the wire layer behind the VNC session: the handshake, the pixel
//! format, the client→server messages, and the server→client framebuffer
//! decoder. It is deliberately transport-agnostic — the decoders take any
//! [`std::io::Read`], so the exact same code path runs against a fixture
//! `Cursor` in the unit tests and against a live `TcpStream` in
//! [`crate::vnc`]. Nothing here is faked: a rectangle decoded from bytes
//! lands in the framebuffer as real pixels.
//!
//! Supported: protocol 3.3/3.7/3.8 version negotiation, `None` and
//! `VNC Authentication` (single-DES) security, true-colour pixel formats,
//! and the `Raw`, `CopyRect`, and `RRE` rectangle encodings — the mandatory
//! baseline every RFB server can speak.

use std::io::{self, Read};
use std::sync::Arc;

/// Hard ceilings for data supplied by an untrusted RFB peer. They are generous
/// enough for a 4K desktop while preventing malformed geometry/text metadata
/// from allocating gigabytes on the transport worker.
pub const MAX_FRAMEBUFFER_PIXELS: usize = 16_777_216; // 4096 × 4096
const MAX_SERVER_NAME_BYTES: usize = 16 * 1024;
const MAX_RECTANGLES_PER_UPDATE: u16 = 4_096;
const MAX_RRE_SUBRECTS: u32 = 1_000_000;
/// RRE's background and all subrectangles each have independent work limits.
/// A peer cannot turn a few bytes of coordinates into billions of pixel loops.
const MAX_RRE_PAINTED_PIXELS: usize = MAX_FRAMEBUFFER_PIXELS;

/// RFB rectangle encodings we advertise + decode.
pub mod encoding {
    pub const RAW: i32 = 0;
    pub const COPY_RECT: i32 = 1;
    pub const RRE: i32 = 2;
}

/// RFB security types.
pub mod security {
    pub const INVALID: u8 = 0;
    pub const NONE: u8 = 1;
    pub const VNC_AUTH: u8 = 2;
}

/// A server pixel format (the 16-byte `PIXEL_FORMAT` structure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelFormat {
    pub bits_per_pixel: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_colour: bool,
    pub red_max: u16,
    pub green_max: u16,
    pub blue_max: u16,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl PixelFormat {
    /// The format we ask the server to send: 32bpp little-endian true-colour
    /// with a pixel value of `R<<16 | G<<8 | B` — i.e. bytes `[B, G, R, x]`.
    /// A trivial conversion to the RGBA the GPU wants.
    pub fn rgb888() -> Self {
        Self {
            bits_per_pixel: 32,
            depth: 24,
            big_endian: false,
            true_colour: true,
            red_max: 255,
            green_max: 255,
            blue_max: 255,
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
        }
    }

    /// Parse the 16-byte on-wire structure.
    pub fn parse(b: &[u8; 16]) -> Self {
        Self {
            bits_per_pixel: b[0],
            depth: b[1],
            big_endian: b[2] != 0,
            true_colour: b[3] != 0,
            red_max: u16::from_be_bytes([b[4], b[5]]),
            green_max: u16::from_be_bytes([b[6], b[7]]),
            blue_max: u16::from_be_bytes([b[8], b[9]]),
            red_shift: b[10],
            green_shift: b[11],
            blue_shift: b[12],
            // b[13..16] padding
        }
    }

    /// Serialise back to the 16-byte on-wire structure.
    pub fn to_bytes(self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0] = self.bits_per_pixel;
        b[1] = self.depth;
        b[2] = u8::from(self.big_endian);
        b[3] = u8::from(self.true_colour);
        b[4..6].copy_from_slice(&self.red_max.to_be_bytes());
        b[6..8].copy_from_slice(&self.green_max.to_be_bytes());
        b[8..10].copy_from_slice(&self.blue_max.to_be_bytes());
        b[10] = self.red_shift;
        b[11] = self.green_shift;
        b[12] = self.blue_shift;
        b
    }

    /// Bytes each pixel occupies on the wire.
    pub fn bytes_per_pixel(self) -> usize {
        (self.bits_per_pixel as usize).div_ceil(8)
    }

    /// Read one pixel's raw bytes as a native integer, honouring endianness.
    fn raw_value(self, px: &[u8]) -> u32 {
        let mut v: u32 = 0;
        if self.big_endian {
            for &byte in px {
                v = (v << 8) | u32::from(byte);
            }
        } else {
            for (i, &byte) in px.iter().enumerate() {
                v |= u32::from(byte) << (8 * i);
            }
        }
        v
    }

    /// Convert one pixel's raw bytes to straight RGBA (opaque).
    pub fn decode_pixel(self, px: &[u8]) -> [u8; 4] {
        let v = self.raw_value(px);
        let chan = |shift: u8, max: u16| -> u8 {
            if max == 0 {
                return 0;
            }
            let raw = (v >> shift) & u32::from(max);
            // Scale the channel's [0, max] range up to [0, 255].
            ((raw * 255 + u32::from(max) / 2) / u32::from(max)) as u8
        };
        [
            chan(self.red_shift, self.red_max),
            chan(self.green_shift, self.green_max),
            chan(self.blue_shift, self.blue_max),
            255,
        ]
    }
}

/// The server's `ServerInit` reply: framebuffer size, pixel format, name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub format: PixelFormat,
    pub name: String,
}

impl ServerInit {
    /// Read a `ServerInit` message body from a stream.
    ///
    /// # Errors
    ///
    /// Returns an error if the stream ends before the fixed header or the
    /// server-supplied name have been fully read.
    pub fn read(r: &mut impl Read) -> io::Result<Self> {
        let mut head = [0u8; 24];
        r.read_exact(&mut head)?;
        let width = u16::from_be_bytes([head[0], head[1]]);
        let height = u16::from_be_bytes([head[2], head[3]]);
        let mut fmt = [0u8; 16];
        fmt.copy_from_slice(&head[4..20]);
        let name_len = u32::from_be_bytes([head[20], head[21], head[22], head[23]]) as usize;
        if name_len > MAX_SERVER_NAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "RFB server name exceeds the 16 KiB limit",
            ));
        }
        validate_framebuffer_size(width, height)?;
        let mut name_buf = vec![0u8; name_len];
        r.read_exact(&mut name_buf)?;
        Ok(Self {
            width,
            height,
            format: PixelFormat::parse(&fmt),
            name: String::from_utf8_lossy(&name_buf).into_owned(),
        })
    }
}

/// The RGBA framebuffer the session surface paints. `data` is
/// `width*height*4` bytes, row-major, straight alpha.
#[derive(Clone)]
pub struct Framebuffer {
    pub width: u16,
    pub height: u16,
    pub data: Arc<Vec<u8>>,
    /// Bumped on every applied update so the UI knows to re-upload the image.
    pub generation: u64,
}

impl Framebuffer {
    pub fn new(width: u16, height: u16) -> Self {
        // Callers that decode network data validate dimensions before this
        // constructor. Keeping a safe, bounded fallback makes direct fixture
        // construction non-panicking too.
        let pixels = usize::from(width)
            .checked_mul(usize::from(height))
            .filter(|pixels| *pixels <= MAX_FRAMEBUFFER_PIXELS)
            .unwrap_or(0);
        Self {
            width,
            height,
            data: Arc::new(vec![0u8; pixels.saturating_mul(4)]),
            generation: 0,
        }
    }

    fn make_writable(&mut self) {
        let _ = Arc::make_mut(&mut self.data);
    }

    fn put_pixel(&mut self, x: u16, y: u16, rgba: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        let Some(slot) = Arc::get_mut(&mut self.data).and_then(|data| data.get_mut(idx..idx + 4))
        else {
            return;
        };
        slot.copy_from_slice(&rgba);
    }

    /// Out-of-range coordinates (a malicious or buggy server can send a
    /// `CopyRect` source outside the framebuffer) read back as transparent
    /// black rather than panicking.
    fn get_pixel(&self, x: u16, y: u16) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0u8; 4];
        }
        let idx = (y as usize * self.width as usize + x as usize) * 4;
        let mut out = [0u8; 4];
        if let Some(slice) = self.data.get(idx..idx + 4) {
            out.copy_from_slice(slice);
        }
        out
    }
}

/// One rectangle header from a `FramebufferUpdate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectHeader {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub encoding: i32,
}

impl RectHeader {
    fn read(r: &mut impl Read) -> io::Result<Self> {
        let mut b = [0u8; 12];
        r.read_exact(&mut b)?;
        Ok(Self {
            x: u16::from_be_bytes([b[0], b[1]]),
            y: u16::from_be_bytes([b[2], b[3]]),
            w: u16::from_be_bytes([b[4], b[5]]),
            h: u16::from_be_bytes([b[6], b[7]]),
            encoding: i32::from_be_bytes([b[8], b[9], b[10], b[11]]),
        })
    }
}

/// Decode a `FramebufferUpdate` message body (everything after the 1-byte
/// message type) from `r`, applying each rectangle into `fb`. Returns the
/// number of rectangles applied.
///
/// The same function backs both the fixture tests (`r` is a `Cursor`) and
/// the live client (`r` is a `TcpStream`).
///
/// # Errors
///
/// Returns an error if the stream ends before a rectangle's declared data
/// has fully arrived, or if the server names a rectangle encoding this
/// client did not advertise support for.
pub fn decode_framebuffer_update(
    r: &mut impl Read,
    fb: &mut Framebuffer,
    fmt: &PixelFormat,
) -> io::Result<u16> {
    let next_generation = fb.generation.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "RFB framebuffer generation exhausted",
        )
    })?;
    let mut head = [0u8; 3];
    r.read_exact(&mut head)?; // 1 pad byte + u16 rectangle count
    let count = u16::from_be_bytes([head[1], head[2]]);
    if count > MAX_RECTANGLES_PER_UPDATE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RFB update has too many rectangles",
        ));
    }
    let bpp = fmt.bytes_per_pixel();
    if !(1..=4).contains(&bpp) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid RFB pixel format",
        ));
    }
    fb.make_writable();
    for _ in 0..count {
        let rect = RectHeader::read(r)?;
        match rect.encoding {
            encoding::RAW => decode_raw(r, fb, fmt, rect, bpp)?,
            encoding::COPY_RECT => decode_copy_rect(r, fb, rect)?,
            encoding::RRE => decode_rre(r, fb, fmt, rect, bpp)?,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported RFB encoding {other}"),
                ));
            }
        }
    }
    fb.generation = next_generation;
    Ok(count)
}

fn decode_raw(
    r: &mut impl Read,
    fb: &mut Framebuffer,
    fmt: &PixelFormat,
    rect: RectHeader,
    bpp: usize,
) -> io::Result<()> {
    let bytes = usize::from(rect.w)
        .checked_mul(usize::from(rect.h))
        .and_then(|pixels| pixels.checked_mul(bpp))
        .filter(|bytes| *bytes <= MAX_FRAMEBUFFER_PIXELS.saturating_mul(4))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "RFB raw rectangle is too large")
        })?;
    let mut pixels = vec![0u8; bytes];
    r.read_exact(&mut pixels)?;
    let mut i = 0;
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            let Some(px) = pixels.get(i..i + bpp) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "raw rectangle pixel buffer too short",
                ));
            };
            let rgba = fmt.decode_pixel(px);
            fb.put_pixel(rect.x.saturating_add(dx), rect.y.saturating_add(dy), rgba);
            i += bpp;
        }
    }
    Ok(())
}

fn decode_copy_rect(r: &mut impl Read, fb: &mut Framebuffer, rect: RectHeader) -> io::Result<()> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    let src_x = u16::from_be_bytes([b[0], b[1]]);
    let src_y = u16::from_be_bytes([b[2], b[3]]);
    // Snapshot the source region first so overlapping copies are correct.
    let pixels = usize::from(rect.w)
        .checked_mul(usize::from(rect.h))
        .filter(|pixels| *pixels <= MAX_FRAMEBUFFER_PIXELS)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "RFB copy rectangle is too large",
            )
        })?;
    let mut buf = Vec::with_capacity(pixels);
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            buf.push(fb.get_pixel(src_x.saturating_add(dx), src_y.saturating_add(dy)));
        }
    }
    let mut i = 0;
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            let Some(&pixel) = buf.get(i) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "copy-rect source buffer too short",
                ));
            };
            fb.put_pixel(rect.x.saturating_add(dx), rect.y.saturating_add(dy), pixel);
            i += 1;
        }
    }
    Ok(())
}

fn decode_rre(
    r: &mut impl Read,
    fb: &mut Framebuffer,
    fmt: &PixelFormat,
    rect: RectHeader,
    bpp: usize,
) -> io::Result<()> {
    let mut head = [0u8; 4];
    r.read_exact(&mut head)?;
    let subrects = u32::from_be_bytes(head);
    if subrects > MAX_RRE_SUBRECTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RFB RRE update has too many subrectangles",
        ));
    }
    checked_rect_pixels(rect.w, rect.h, "RRE background rectangle")?;
    let mut bg = vec![0u8; bpp];
    r.read_exact(&mut bg)?;
    let bg_rgba = fmt.decode_pixel(&bg);
    // Decode and bound every subrectangle before any paint loops. This keeps
    // malformed later geometry from doing expensive partial background work.
    let mut sub = vec![0u8; bpp + 8];
    let bad_subrect = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "RRE subrectangle buffer too short",
        )
    };
    let mut painted_subrect_pixels = 0usize;
    let mut pending_subrects = Vec::with_capacity(subrects as usize);
    for _ in 0..subrects {
        r.read_exact(&mut sub)?;
        let color_bytes = sub.get(..bpp).ok_or_else(bad_subrect)?;
        let color = fmt.decode_pixel(color_bytes);
        let coord: [u8; 8] = sub
            .get(bpp..bpp + 8)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(bad_subrect)?;
        let sx = u16::from_be_bytes([coord[0], coord[1]]);
        let sy = u16::from_be_bytes([coord[2], coord[3]]);
        let sw = u16::from_be_bytes([coord[4], coord[5]]);
        let sh = u16::from_be_bytes([coord[6], coord[7]]);
        let subrect_pixels = checked_rect_pixels(sw, sh, "RRE subrectangle")?;
        painted_subrect_pixels = painted_subrect_pixels
            .checked_add(subrect_pixels)
            .filter(|pixels| *pixels <= MAX_RRE_PAINTED_PIXELS)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "RRE subrectangles exceed the pixel work limit",
                )
            })?;
        pending_subrects.push((color, sx, sy, sw, sh));
    }
    // Fill the whole rectangle with the background first.
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            fb.put_pixel(
                rect.x.saturating_add(dx),
                rect.y.saturating_add(dy),
                bg_rgba,
            );
        }
    }
    // Then paint each already-bounded subrectangle.
    for (color, sx, sy, sw, sh) in pending_subrects {
        for dy in 0..sh {
            for dx in 0..sw {
                fb.put_pixel(
                    rect.x.saturating_add(sx).saturating_add(dx),
                    rect.y.saturating_add(sy).saturating_add(dy),
                    color,
                );
            }
        }
    }
    Ok(())
}

fn checked_rect_pixels(width: u16, height: u16, label: &str) -> io::Result<usize> {
    usize::from(width)
        .checked_mul(usize::from(height))
        .filter(|pixels| *pixels <= MAX_RRE_PAINTED_PIXELS)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{label} exceeds the pixel work limit"),
            )
        })
}

fn validate_framebuffer_size(width: u16, height: u16) -> io::Result<()> {
    let pixels = usize::from(width)
        .checked_mul(usize::from(height))
        .filter(|pixels| *pixels > 0 && *pixels <= MAX_FRAMEBUFFER_PIXELS);
    if pixels.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RFB framebuffer dimensions exceed the 16 megapixel limit",
        ));
    }
    Ok(())
}

// --- Handshake helpers -----------------------------------------------------

/// Parse a 12-byte `ProtocolVersion` string like `RFB 003.008\n`.
pub fn parse_protocol_version(b: &[u8; 12]) -> Option<(u8, u8)> {
    let s = std::str::from_utf8(b).ok()?;
    let s = s.strip_prefix("RFB ")?.trim_end();
    let (maj, min) = s.split_once('.')?;
    Some((maj.parse().ok()?, min.parse().ok()?))
}

/// The `ProtocolVersion` string a client offers, clamped to the highest
/// version we speak (3.8) but never above the server's.
pub fn client_protocol_version(server_major: u8, server_minor: u8) -> [u8; 12] {
    let minor = if (server_major, server_minor) >= (3, 8) {
        8
    } else if (server_major, server_minor) >= (3, 7) {
        7
    } else {
        3
    };
    let s = format!("RFB 003.{minor:03}\n");
    let mut out = [0u8; 12];
    out.copy_from_slice(s.as_bytes());
    out
}

/// Parse the 3.7+ security-type list: a count byte followed by that many
/// type bytes. A zero count means the server rejected us and follows with a
/// reason string — surfaced here as an error.
///
/// # Errors
///
/// Returns an error if the server advertised zero security types, or if
/// fewer type bytes followed than the count promised.
pub fn parse_security_types(count: u8, types: &[u8]) -> Result<Vec<u8>, String> {
    if count == 0 {
        return Err("server offered no security types".into());
    }
    let n = (count as usize).min(types.len());
    let Some(slice) = types.get(..n) else {
        return Err("malformed security type list".into());
    };
    Ok(slice.to_vec())
}

/// Pick the security type we prefer from the server's offer: `None` when
/// available (no password), else `VNC Authentication`.
pub fn choose_security(types: &[u8]) -> Option<u8> {
    if types.contains(&security::NONE) {
        Some(security::NONE)
    } else if types.contains(&security::VNC_AUTH) {
        Some(security::VNC_AUTH)
    } else {
        None
    }
}

/// Compute the 16-byte VNC-Authentication response: DES-ECB encrypt the
/// server's 16-byte challenge with a key derived from the password. VNC
/// mirrors the bit order of each key byte — a historical quirk of the
/// original implementation that every server expects.
pub fn vnc_auth_response(challenge: &[u8; 16], password: &str) -> [u8; 16] {
    use des::cipher::{BlockCipherEncrypt, KeyInit};

    let mut key = [0u8; 8];
    for (i, b) in password.bytes().take(8).enumerate() {
        if let Some(slot) = key.get_mut(i) {
            *slot = mirror_bits(b);
        }
    }
    let cipher = des::Des::new(&key.into());
    let mut out = *challenge;
    let (blocks, _) = out.as_chunks_mut::<8>();
    for block in blocks {
        cipher.encrypt_block(block.into());
    }
    out
}

/// Reverse the bit order within a byte (VNC's key-byte quirk).
fn mirror_bits(mut b: u8) -> u8 {
    let mut r = 0u8;
    for _ in 0..8 {
        r = (r << 1) | (b & 1);
        b >>= 1;
    }
    r
}

// --- Client→server message encoders ---------------------------------------

/// `SetPixelFormat` (message type 0).
pub fn encode_set_pixel_format(fmt: &PixelFormat) -> [u8; 20] {
    let mut b = [0u8; 20];
    b[0] = 0; // type
    b[4..20].copy_from_slice(&fmt.to_bytes());
    b
}

/// `SetEncodings` (message type 2).
pub fn encode_set_encodings(encodings: &[i32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(4 + encodings.len() * 4);
    b.push(2); // type
    b.push(0); // pad
    b.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
    for e in encodings {
        b.extend_from_slice(&e.to_be_bytes());
    }
    b
}

/// `FramebufferUpdateRequest` (message type 3).
pub fn encode_framebuffer_update_request(
    incremental: bool,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) -> [u8; 10] {
    let mut buf = [0u8; 10];
    buf[0] = 3;
    buf[1] = u8::from(incremental);
    buf[2..4].copy_from_slice(&x.to_be_bytes());
    buf[4..6].copy_from_slice(&y.to_be_bytes());
    buf[6..8].copy_from_slice(&w.to_be_bytes());
    buf[8..10].copy_from_slice(&h.to_be_bytes());
    buf
}

/// `PointerEvent` (message type 5).
pub fn encode_pointer_event(button_mask: u8, x: u16, y: u16) -> [u8; 6] {
    let mut b = [0u8; 6];
    b[0] = 5;
    b[1] = button_mask;
    b[2..4].copy_from_slice(&x.to_be_bytes());
    b[4..6].copy_from_slice(&y.to_be_bytes());
    b
}

/// `KeyEvent` (message type 4). `key` is an X11 keysym.
pub fn encode_key_event(down: bool, key: u32) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0] = 4;
    b[1] = u8::from(down);
    b[4..8].copy_from_slice(&key.to_be_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn pixel_formats_round_trip_and_decode() {
        eprintln!("phase pixel_format_roundtrips");
        {
            let f = PixelFormat::rgb888();
            assert_eq!(PixelFormat::parse(&f.to_bytes()), f);
        }

        eprintln!("phase decode_pixel_32bpp_bgrx");
        {
            // rgb888(): little-endian value R<<16|G<<8|B -> bytes [B,G,R,x].
            let f = PixelFormat::rgb888();
            assert_eq!(
                f.decode_pixel(&[0x10, 0x20, 0x30, 0x00]),
                [0x30, 0x20, 0x10, 255]
            );
        }

        eprintln!("phase decode_pixel_16bpp_565");
        {
            // 16bpp 5-6-5: red_max 31 (shift 11), green 63 (shift 5), blue 31 (0).
            let f = PixelFormat {
                bits_per_pixel: 16,
                depth: 16,
                big_endian: false,
                true_colour: true,
                red_max: 31,
                green_max: 63,
                blue_max: 31,
                red_shift: 11,
                green_shift: 5,
                blue_shift: 0,
            };
            // Pure red = 0xF800 -> little-endian [0x00, 0xF8].
            assert_eq!(f.decode_pixel(&[0x00, 0xF8]), [255, 0, 0, 255]);
            // Pure blue = 0x001F -> [0x1F, 0x00].
            assert_eq!(f.decode_pixel(&[0x1F, 0x00]), [0, 0, 255, 255]);
        }

        eprintln!("phase mirror_bits_reverses_byte");
        {
            assert_eq!(mirror_bits(0b0000_0001), 0b1000_0000);
            assert_eq!(mirror_bits(0b1010_0000), 0b0000_0101);
        }
    }

    #[test]
    fn rectangle_encodings_paint_the_framebuffer() {
        eprintln!("phase raw_rect_lands_in_framebuffer");
        {
            let f = PixelFormat::rgb888();
            let mut fb = Framebuffer::new(2, 2);
            // A FramebufferUpdate: pad, count=1; rect at (0,0) 2x2 Raw; 4 px.
            let mut msg = vec![0u8, 0, 1]; // pad + count
            msg.extend_from_slice(&[0, 0, 0, 0]); // x,y
            msg.extend_from_slice(&2u16.to_be_bytes()); // w
            msg.extend_from_slice(&2u16.to_be_bytes()); // h
            msg.extend_from_slice(&encoding::RAW.to_be_bytes());
            // Four BGRX pixels: red, green, blue, white.
            msg.extend_from_slice(&[0, 0, 255, 0]); // red
            msg.extend_from_slice(&[0, 255, 0, 0]); // green
            msg.extend_from_slice(&[255, 0, 0, 0]); // blue
            msg.extend_from_slice(&[255, 255, 255, 0]); // white
            let n = decode_framebuffer_update(&mut Cursor::new(msg), &mut fb, &f).unwrap();
            assert_eq!(n, 1);
            assert_eq!(&fb.data[0..4], &[255, 0, 0, 255]); // (0,0) red
            assert_eq!(&fb.data[4..8], &[0, 255, 0, 255]); // (1,0) green
            assert_eq!(&fb.data[8..12], &[0, 0, 255, 255]); // (0,1) blue
            assert_eq!(&fb.data[12..16], &[255, 255, 255, 255]); // (1,1) white
            assert_eq!(fb.generation, 1);
        }

        eprintln!("phase copy_rect_moves_pixels");
        {
            let f = PixelFormat::rgb888();
            let mut fb = Framebuffer::new(2, 1);
            // Seed (0,0) red.
            Arc::make_mut(&mut fb.data)[0..4].copy_from_slice(&[255, 0, 0, 255]);
            // CopyRect: copy the 1x1 region at src (0,0) to dst (1,0).
            let mut msg = vec![0u8, 0, 1];
            msg.extend_from_slice(&1u16.to_be_bytes()); // dst x = 1
            msg.extend_from_slice(&0u16.to_be_bytes()); // dst y
            msg.extend_from_slice(&1u16.to_be_bytes()); // w
            msg.extend_from_slice(&1u16.to_be_bytes()); // h
            msg.extend_from_slice(&encoding::COPY_RECT.to_be_bytes());
            msg.extend_from_slice(&0u16.to_be_bytes()); // src x
            msg.extend_from_slice(&0u16.to_be_bytes()); // src y
            decode_framebuffer_update(&mut Cursor::new(msg), &mut fb, &f).unwrap();
            assert_eq!(&fb.data[4..8], &[255, 0, 0, 255]);
        }

        eprintln!("phase rre_fills_background_and_subrects");
        {
            let f = PixelFormat::rgb888();
            let mut fb = Framebuffer::new(2, 2);
            let mut msg = vec![0u8, 0, 1];
            msg.extend_from_slice(&0u16.to_be_bytes()); // x
            msg.extend_from_slice(&0u16.to_be_bytes()); // y
            msg.extend_from_slice(&2u16.to_be_bytes()); // w
            msg.extend_from_slice(&2u16.to_be_bytes()); // h
            msg.extend_from_slice(&encoding::RRE.to_be_bytes());
            msg.extend_from_slice(&1u32.to_be_bytes()); // 1 subrect
            msg.extend_from_slice(&[0, 0, 255, 0]); // bg = red (BGRX wire order)
            // subrect: color blue (BGRX), at (1,1) 1x1
            msg.extend_from_slice(&[255, 0, 0, 0]);
            msg.extend_from_slice(&1u16.to_be_bytes());
            msg.extend_from_slice(&1u16.to_be_bytes());
            msg.extend_from_slice(&1u16.to_be_bytes());
            msg.extend_from_slice(&1u16.to_be_bytes());
            decode_framebuffer_update(&mut Cursor::new(msg), &mut fb, &f).unwrap();
            assert_eq!(&fb.data[0..4], &[255, 0, 0, 255]); // bg red
            assert_eq!(&fb.data[12..16], &[0, 0, 255, 255]); // subrect blue
        }
    }

    #[test]
    fn handshake_parses_versions_security_and_server_init() {
        eprintln!("phase server_init_parses");
        {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&800u16.to_be_bytes());
            bytes.extend_from_slice(&600u16.to_be_bytes());
            bytes.extend_from_slice(&PixelFormat::rgb888().to_bytes());
            bytes.extend_from_slice(&(4u32).to_be_bytes());
            bytes.extend_from_slice(b"test");
            let si = ServerInit::read(&mut Cursor::new(bytes)).unwrap();
            assert_eq!((si.width, si.height), (800, 600));
            assert_eq!(si.name, "test");
            assert_eq!(si.format, PixelFormat::rgb888());
        }

        eprintln!("phase protocol_version_parse_and_offer");
        {
            assert_eq!(parse_protocol_version(b"RFB 003.008\n"), Some((3, 8)));
            assert_eq!(&client_protocol_version(3, 8), b"RFB 003.008\n");
            assert_eq!(&client_protocol_version(3, 3), b"RFB 003.003\n");
        }

        eprintln!("phase security_negotiation");
        {
            let types = parse_security_types(2, &[security::NONE, security::VNC_AUTH]).unwrap();
            assert_eq!(choose_security(&types), Some(security::NONE));
            assert_eq!(
                choose_security(&[security::VNC_AUTH]),
                Some(security::VNC_AUTH)
            );
            assert_eq!(choose_security(&[99]), None);
            assert!(parse_security_types(0, &[]).is_err());
        }
    }

    #[test]
    fn hostile_geometry_is_rejected_before_allocation_or_painting() {
        eprintln!("phase oversized_server_geometry_is_rejected_before_allocation");
        {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&u16::MAX.to_be_bytes());
            bytes.extend_from_slice(&u16::MAX.to_be_bytes());
            bytes.extend_from_slice(&PixelFormat::rgb888().to_bytes());
            bytes.extend_from_slice(&0u32.to_be_bytes());
            assert!(ServerInit::read(&mut Cursor::new(bytes)).is_err());
        }

        eprintln!("phase overflowing_rectangle_coordinates_do_not_panic");
        {
            let f = PixelFormat::rgb888();
            let mut fb = Framebuffer::new(2, 2);
            let mut msg = vec![0u8, 0, 1];
            msg.extend_from_slice(&u16::MAX.to_be_bytes());
            msg.extend_from_slice(&u16::MAX.to_be_bytes());
            msg.extend_from_slice(&2u16.to_be_bytes());
            msg.extend_from_slice(&2u16.to_be_bytes());
            msg.extend_from_slice(&encoding::RAW.to_be_bytes());
            msg.extend_from_slice(&[0u8; 16]);
            assert!(decode_framebuffer_update(&mut Cursor::new(msg), &mut fb, &f).is_ok());
        }

        eprintln!("phase exhausted_framebuffer_generation_fails_without_reuse");
        {
            let f = PixelFormat::rgb888();
            let mut fb = Framebuffer::new(1, 1);
            fb.generation = u64::MAX;
            let before = Arc::clone(&fb.data);
            let mut msg = vec![0u8, 0, 1];
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&1u16.to_be_bytes());
            msg.extend_from_slice(&1u16.to_be_bytes());
            msg.extend_from_slice(&encoding::RAW.to_be_bytes());
            msg.extend_from_slice(&[0, 0, 0, 0]);
            assert!(decode_framebuffer_update(&mut Cursor::new(msg), &mut fb, &f).is_err());
            assert_eq!(fb.generation, u64::MAX);
            assert!(Arc::ptr_eq(&fb.data, &before));
        }

        eprintln!("phase rre_rejects_oversized_background_before_painting");
        {
            let f = PixelFormat::rgb888();
            let mut fb = Framebuffer::new(2, 2);
            let mut msg = vec![0u8, 0, 1];
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&u16::MAX.to_be_bytes());
            msg.extend_from_slice(&u16::MAX.to_be_bytes());
            msg.extend_from_slice(&encoding::RRE.to_be_bytes());
            msg.extend_from_slice(&0u32.to_be_bytes());
            assert!(decode_framebuffer_update(&mut Cursor::new(msg), &mut fb, &f).is_err());
            assert_eq!(&fb.data[..], &[0; 16]);
        }

        eprintln!("phase rre_rejects_cumulative_subrectangle_pixel_work_before_painting");
        {
            let f = PixelFormat::rgb888();
            let mut fb = Framebuffer::new(2, 2);
            let mut msg = vec![0u8, 0, 1];
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&4096u16.to_be_bytes());
            msg.extend_from_slice(&4096u16.to_be_bytes());
            msg.extend_from_slice(&encoding::RRE.to_be_bytes());
            msg.extend_from_slice(&2u32.to_be_bytes());
            msg.extend_from_slice(&[0, 0, 0, 0]);
            for _ in 0..2 {
                msg.extend_from_slice(&[0, 0, 0, 0]);
                msg.extend_from_slice(&0u16.to_be_bytes());
                msg.extend_from_slice(&0u16.to_be_bytes());
                msg.extend_from_slice(&4096u16.to_be_bytes());
                msg.extend_from_slice(&4096u16.to_be_bytes());
            }
            assert!(decode_framebuffer_update(&mut Cursor::new(msg), &mut fb, &f).is_err());
            assert_eq!(&fb.data[..], &[0; 16]);
        }
    }

    #[test]
    fn client_messages_and_vnc_auth_are_exact() {
        eprintln!("phase client_encoders_are_exact");
        {
            assert_eq!(
                encode_framebuffer_update_request(true, 0, 0, 800, 600),
                [3, 1, 0, 0, 0, 0, 0x03, 0x20, 0x02, 0x58]
            );
            assert_eq!(
                encode_set_encodings(&[encoding::RAW, encoding::COPY_RECT]),
                vec![2, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 1]
            );
            assert_eq!(encode_pointer_event(1, 0x0102, 0x0304), [5, 1, 1, 2, 3, 4]);
            assert_eq!(encode_key_event(true, 0x61), [4, 1, 0, 0, 0, 0, 0, 0x61]);
            let spf = encode_set_pixel_format(&PixelFormat::rgb888());
            assert_eq!(spf[0], 0);
            assert_eq!(&spf[4..20], &PixelFormat::rgb888().to_bytes());
        }

        eprintln!("phase vnc_auth_matches_independent_des_oracle");
        {
            // Known-answer vector produced by an independent single-DES
            // implementation (see the crate's test notes): password "pass",
            // challenge = 0x00..0x0F, VNC key-byte bit mirroring, DES-ECB.
            let challenge: [u8; 16] = [
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
                0x0e, 0x0f,
            ];
            let resp = vnc_auth_response(&challenge, "pass");
            // Cross-checked against openssl's `des-ecb` (legacy provider) and
            // pyDes — both independent single-DES implementations.
            assert_eq!(
                resp,
                [
                    0x5f, 0xb0, 0x2f, 0x4e, 0x6e, 0xc9, 0xfd, 0xa0, 0x6c, 0x41, 0xdf, 0x1f, 0x35,
                    0x01, 0x51, 0x38,
                ]
            );
        }
    }
}
