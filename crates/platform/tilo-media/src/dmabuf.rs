//! The DMA-BUF vocabulary shared between the decoder and the GPU importer: the DRM
//! fourcc, the format modifier (tiling or compression layout), the per-plane fd,
//! offset and stride, and the colour convention the samples are in. Toolkit-free and
//! `GStreamer`-free, so `media` can produce it and `ui` import it.
//!
//! A [`DmabufImage`] owns its plane fds, and dropping it returns the decoder's buffer
//! to its pool: hold one exactly as long as the GPU reads from it. An importer that
//! hands the fd to Vulkan must `dup` it, because Vulkan takes ownership.

#[cfg(unix)]
use std::os::fd::OwnedFd;

#[cfg(unix)]
pub type DmabufHandle = OwnedFd;

#[cfg(not(unix))]
#[derive(Debug)]
pub enum DmabufHandle {}

/// Linear (untiled) layout — the only modifier a plain
/// `VK_EXT_external_memory_dma_buf` import can interpret.
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// "The producer did not say." Treated as linear by convention when a single
/// plane is present, because that is what every exporter that omits the
/// modifier actually produces.
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

pub const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

pub fn fourcc_str(v: u32) -> String {
    // A fourcc *is* its four little-endian bytes, so ask for them directly
    // rather than shifting and truncating a copy at a time.
    v.to_le_bytes()
        .iter()
        .map(|c| {
            if c.is_ascii_graphic() {
                *c as char
            } else {
                '?'
            }
        })
        .collect()
}

pub const DRM_FORMAT_NV12: u32 = fourcc(b'N', b'V', b'1', b'2');
pub const DRM_FORMAT_P010: u32 = fourcc(b'P', b'0', b'1', b'0');
/// 8-bit 4:2:2 packed as Y0 Cb Y1 Cr, single plane, 2 bytes per pixel.
pub const DRM_FORMAT_YUYV: u32 = fourcc(b'Y', b'U', b'Y', b'V');
pub const DRM_FORMAT_ARGB8888: u32 = fourcc(b'A', b'R', b'2', b'4');
pub const DRM_FORMAT_XRGB8888: u32 = fourcc(b'X', b'R', b'2', b'4');
pub const DRM_FORMAT_ABGR8888: u32 = fourcc(b'A', b'B', b'2', b'4');
pub const DRM_FORMAT_XBGR8888: u32 = fourcc(b'X', b'B', b'2', b'4');
/// 10-bit RGB in one little-endian 32-bit word, red in the low bits, two
/// padding bits on top (DRM's `XB30`). The HDR10 scanout format.
pub const DRM_FORMAT_XBGR2101010: u32 = fourcc(b'X', b'B', b'3', b'0');
pub const DRM_FORMAT_ABGR2101010: u32 = fourcc(b'A', b'B', b'3', b'0');
pub const DRM_FORMAT_XRGB2101010: u32 = fourcc(b'X', b'R', b'3', b'0');
pub const DRM_FORMAT_ARGB2101010: u32 = fourcc(b'A', b'R', b'3', b'0');
pub const DRM_FORMAT_ABGR16161616F: u32 = fourcc(b'A', b'B', b'4', b'H');
pub const DRM_FORMAT_XBGR16161616F: u32 = fourcc(b'X', b'B', b'4', b'H');

/// The YCbCr→RGB matrix the samples were encoded with. `Identity` means the
/// samples are already RGB and need no conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
    /// Samples are RGB already.
    Identity,
}

impl YuvMatrix {
    /// The three luma coefficients `(kr, kg, kb)` this matrix is defined by.
    /// `Identity` reports BT.709's, which is never applied — the importer
    /// short-circuits RGB payloads before it reaches the matrix.
    pub fn luma_coefficients(self) -> (f32, f32, f32) {
        match self {
            YuvMatrix::Bt601 => (0.299, 0.587, 0.114),
            YuvMatrix::Bt709 | YuvMatrix::Identity => (0.2126, 0.7152, 0.0722),
            YuvMatrix::Bt2020 => (0.2627, 0.6780, 0.0593),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorRange {
    Limited,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colorimetry {
    pub matrix: YuvMatrix,
    pub range: ColorRange,
}

impl Colorimetry {
    pub const BT709_LIMITED: Self = Self {
        matrix: YuvMatrix::Bt709,
        range: ColorRange::Limited,
    };

    pub const RGB: Self = Self {
        matrix: YuvMatrix::Identity,
        range: ColorRange::Full,
    };
}

impl Default for Colorimetry {
    fn default() -> Self {
        Self::BT709_LIMITED
    }
}

#[derive(Debug)]
pub struct DmabufPlane {
    pub fd: DmabufHandle,
    pub offset: u32,
    pub stride: u32,
}

#[derive(Debug)]
pub struct DmabufImage {
    pub planes: Vec<DmabufPlane>,
    pub fourcc: u32,
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    pub colorimetry: Colorimetry,
}

impl DmabufImage {
    /// Whether the layout is one a plain external-memory import can read:
    /// linear, or a producer that declined to state a modifier.
    ///
    /// A tiled or compression-enabled modifier needs
    /// `VK_EXT_image_drm_format_modifier`, which `wgpu-hal` does not enable —
    /// see [`crate::dmabuf`]'s module docs and the importer in `ui`.
    pub fn is_linear(&self) -> bool {
        self.modifier == DRM_FORMAT_MOD_LINEAR || self.modifier == DRM_FORMAT_MOD_INVALID
    }

    /// A short human description for logs: `NV12 1920x1080 mod=0x0 planes=2`.
    pub fn describe(&self) -> String {
        format!(
            "{} {}x{} mod={:#x} planes={}",
            fourcc_str(self.fourcc),
            self.width,
            self.height,
            self.modifier,
            self.planes.len()
        )
    }
}

/// The number of bytes a dmabuf holds, from `lseek(fd, 0, SEEK_END)`. dmabufs
/// implement seek precisely so an importer can size the allocation it needs
/// when the producer only described plane offsets. `None` when the fd does
/// not support seeking.
#[cfg(unix)]
pub fn dmabuf_size(fd: &DmabufHandle) -> Option<u64> {
    use std::io::{Seek, SeekFrom};
    let mut file = std::fs::File::from(fd.try_clone().ok()?);
    file.seek(SeekFrom::End(0)).ok()
}

#[cfg(not(unix))]
pub fn dmabuf_size(_fd: &DmabufHandle) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_round_trips_through_its_characters() {
        assert_eq!(fourcc_str(DRM_FORMAT_NV12), "NV12");
        assert_eq!(fourcc_str(DRM_FORMAT_P010), "P010");
        assert_eq!(fourcc_str(DRM_FORMAT_YUYV), "YUYV");
        // AR24 is DRM's name for what Vulkan calls B8G8R8A8 — the byte order
        // the importer must map it to, so getting this wrong swaps red/blue.
        assert_eq!(fourcc_str(DRM_FORMAT_ARGB8888), "AR24");
        assert_eq!(fourcc_str(DRM_FORMAT_ABGR8888), "AB24");
    }

    #[test]
    #[cfg(unix)]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn dmabuf_descriptions_layouts_and_sizes_come_from_the_real_buffer() {
        {
            let img = |modifier| DmabufImage {
                planes: vec![],
                fourcc: DRM_FORMAT_NV12,
                modifier,
                width: 16,
                height: 16,
                colorimetry: Colorimetry::default(),
            };
            assert!(img(DRM_FORMAT_MOD_LINEAR).is_linear());
            assert!(
                img(DRM_FORMAT_MOD_INVALID).is_linear(),
                "unstated is treated as linear"
            );
            // The AMD GFX9 tiled modifier a VA-API decoder exports. This must be
            // rejected: importing it as linear would sample garbage.
            assert!(!img(0x0200_0000_1040_1b04).is_linear());
        }
        {
            let img = DmabufImage {
                planes: vec![],
                fourcc: DRM_FORMAT_NV12,
                modifier: 0,
                width: 1920,
                height: 1080,
                colorimetry: Colorimetry::default(),
            };
            assert_eq!(img.describe(), "NV12 1920x1080 mod=0x0 planes=0");
        }
        {
            let dir = std::env::temp_dir().join(format!("tilo-dmabuf-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("buf");
            std::fs::write(&path, vec![0u8; 4096]).unwrap();
            let f = std::fs::File::open(&path).unwrap();
            let fd = OwnedFd::from(f);
            assert_eq!(
                dmabuf_size(&fd),
                Some(4096),
                "SEEK_END reports the real size"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
