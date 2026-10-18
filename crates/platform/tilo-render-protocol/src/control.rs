//! Messages on the control channel between an app and its render service.
//!
//! The channel is small and ordered: frames themselves travel through the
//! ring (`shm`), and the control channel says where ("frame N is at offset O")
//! and what became of them ("frame N is in buffer B", "frame N was
//! dropped, because …", "frame N was presented at T"). Descriptors ride along
//! where a message names one: the ring with the hello, an image's memfd with
//! its registration, the render buffers with their announcement.
//!
//! Messages are postcard-encoded; one message is one `SOCK_SEQPACKET` packet.

use serde::{Deserialize, Serialize};

use crate::{FontId, ImageId, Limits, SurfaceId, WireError};

/// Largest encoded message either side sends or accepts.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// Most descriptors one message carries.
pub const MAX_MESSAGE_FDS: usize = 4;

/// Longest font path an app may name.
pub const MAX_FONT_PATH: usize = 1024;

/// Pixel layout of an uploaded image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    Rgba8,
    Bgra8,
}

/// How an uploaded image's alpha is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlphaType {
    Straight,
    Premultiplied,
}

/// A buffer format both the compositor and the app's surface take: a DRM
/// fourcc and modifier, as the compositor advertised them to the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmabufFormat {
    pub fourcc: u32,
    pub modifier: u64,
}

/// What an app sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AppMessage {
    /// The first message, with the ring's memfd. `version` is the newest the
    /// app speaks.
    Hello {
        version: u16,
    },
    /// A new ring, with its memfd, replacing the current one; sent only when
    /// the service holds no frame of the old one (a frame outgrew it).
    Ring,
    /// A surface the app renders through the service. `formats` are the
    /// linear formats the compositor accepts from this app; `opaque` says
    /// whether the surface ignores alpha.
    CreateSurface {
        surface: SurfaceId,
        opaque: bool,
        formats: Vec<DmabufFormat>,
    },
    DestroySurface {
        surface: SurfaceId,
    },
    /// Frame `sequence` of a surface is the record at `offset..offset + len`
    /// in the ring.
    Frame {
        sequence: u64,
        offset: u64,
        len: u32,
    },
    /// The compositor released `buffer` of `surface`: the service may draw
    /// into it again.
    Released {
        surface: SurfaceId,
        generation: u32,
        buffer: u32,
    },
    /// An image's pixels, in the sealed memfd that comes with this message,
    /// tightly packed.
    RegisterImage {
        image: ImageId,
        width: u32,
        height: u32,
        format: ImageFormat,
        alpha: AlphaType,
    },
    DropImage {
        image: ImageId,
    },
    /// A font from the shared read-only view, by its path there.
    RegisterFontFile {
        font: FontId,
        path: String,
        index: u32,
    },
    /// A font the app holds in memory, in the sealed memfd that comes with
    /// this message.
    RegisterFontData {
        font: FontId,
        index: u32,
    },
    DropFont {
        font: FontId,
    },
    /// The compositor presented (or discarded) frame `sequence`; for the
    /// app's frame timeline in the service.
    Presented {
        surface: SurfaceId,
        sequence: u64,
        presented_ns: u64,
        refresh_ns: u64,
        discarded: bool,
    },
}

/// Why a frame or a request was not served.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// The record or a resource broke the protocol; nothing was drawn.
    Invalid(String),
    /// Over one of the app's limits; nothing was drawn.
    OverLimit(String),
    /// The GPU failed the frame; the app should send a full frame again.
    RenderFailed(String),
    /// The app used more GPU time than its share; the frame waited for the
    /// app's budget to recover and was superseded.
    Throttled,
}

impl From<WireError> for Refusal {
    fn from(error: WireError) -> Self {
        match error {
            WireError::OverLimit(what) => Self::OverLimit(what.to_owned()),
            other => Self::Invalid(other.to_string()),
        }
    }
}

/// Where a buffer's pixels live.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BufferKind {
    /// A dma-buf with this format; attach it with `zwp_linux_dmabuf_v1`.
    Dmabuf(DmabufFormat),
    /// Shared memory in `wl_shm` format `format` (`argb8888` = 0,
    /// `xrgb8888` = 1, or a fourcc); attach it with `wl_shm`. Used when the
    /// service has no GPU.
    Shm { format: u32 },
}

/// One buffer of a surface: the layout of its single plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plane {
    pub offset: u32,
    pub stride: u32,
}

/// What the service sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServiceMessage {
    /// Answer to the hello: the version both speak, the app's limits, and
    /// the font directories it may name fonts in by path (everything else is
    /// uploaded).
    Welcome {
        version: u16,
        limits: Limits,
        font_roots: Vec<String>,
    },
    /// The buffers `surface` is drawn into from now on, one descriptor each,
    /// in order. A new generation replaces the previous one (a resize).
    Buffers {
        surface: SurfaceId,
        generation: u32,
        width: u32,
        height: u32,
        kind: BufferKind,
        planes: Vec<Plane>,
    },
    /// Frame `sequence` is drawn in `buffer`: attach it with `damage` (in
    /// buffer pixels; empty means all of it).
    FrameReady {
        surface: SurfaceId,
        sequence: u64,
        generation: u32,
        buffer: u32,
        damage: Vec<[i32; 4]>,
        /// GPU time the frame took, in microseconds (0 when unmeasured).
        gpu_us: u32,
    },
    /// Frame `sequence` was not drawn; the ring space it used is free.
    FrameDropped { sequence: u64, why: Refusal },
    /// A request other than a frame was refused. The connection stays up.
    Refused { why: Refusal },
}

/// Encode `message` for the channel.
///
/// # Errors
///
/// When it does not fit [`MAX_MESSAGE_BYTES`].
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, WireError> {
    let bytes = postcard::to_allocvec(message).map_err(|_| WireError::Malformed("message"))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(WireError::OverLimit("message size"));
    }
    Ok(bytes)
}

/// Decode a message from the channel.
///
/// # Errors
///
/// When the bytes are not exactly one message.
pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, WireError> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(WireError::OverLimit("message size"));
    }
    let (message, rest) =
        postcard::take_from_bytes(bytes).map_err(|_| WireError::Malformed("message"))?;
    if !rest.is_empty() {
        return Err(WireError::Malformed("bytes after a message"));
    }
    Ok(message)
}
