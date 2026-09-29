//! The DMA-BUF vocabulary shared between the decoder and the GPU importer.
//!
//! A hardware decoder hands back a picture that already lives in GPU memory.
//! Describing it takes four things — the DRM fourcc, the DRM format modifier
//! (tiling / compression layout), the per-plane fd + offset + stride, and the
//! colour convention the samples are in. Everything here is toolkit-free and
//! GStreamer-free so `media` can produce it and `ui` can import it without
//! either crate learning about the other's stack.
//!
//! # Ownership
//!
//! A [`DmabufImage`] owns its plane fds. Dropping it closes them, which
//! releases the decoder's buffer back to its pool — so hold one exactly as
//! long as the GPU still reads from it, and no longer. An importer that hands
//! the fd to Vulkan must `dup` it: Vulkan takes ownership of what it is given.

#[cfg(unix)]
use std::os::fd::OwnedFd;

/// Owned descriptor for a Linux DMA-BUF plane.
#[cfg(unix)]
pub type DmabufHandle = OwnedFd;

/// Uninhabited placeholder on platforms that cannot receive Linux DMA-BUFs.
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

/// Build a DRM fourcc from its four characters, little-endian as DRM defines.
pub const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

/// Render a fourcc back to its four characters, for logs and error text.
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

/// 8-bit 4:2:0, two planes: Y then interleaved `CbCr`. What essentially every
/// hardware H.264/HEVC decoder produces.
pub const DRM_FORMAT_NV12: u32 = fourcc(b'N', b'V', b'1', b'2');
/// 10-bit 4:2:0 in 16-bit containers, two planes. HDR / 10-bit HEVC + AV1.
pub const DRM_FORMAT_P010: u32 = fourcc(b'P', b'0', b'1', b'0');
/// 8-bit 4:2:2 packed as Y0 Cb Y1 Cr, single plane, 2 bytes per pixel.
pub const DRM_FORMAT_YUYV: u32 = fourcc(b'Y', b'U', b'Y', b'V');
/// 8-bit BGRA in memory order (DRM's `AR24`).
pub const DRM_FORMAT_ARGB8888: u32 = fourcc(b'A', b'R', b'2', b'4');
/// 8-bit BGRX in memory order (DRM's `XR24`).
pub const DRM_FORMAT_XRGB8888: u32 = fourcc(b'X', b'R', b'2', b'4');
/// 8-bit RGBA in memory order (DRM's `AB24`).
pub const DRM_FORMAT_ABGR8888: u32 = fourcc(b'A', b'B', b'2', b'4');
/// 8-bit RGBX in memory order (DRM's `XB24`).
pub const DRM_FORMAT_XBGR8888: u32 = fourcc(b'X', b'B', b'2', b'4');
/// 10-bit RGB in one little-endian 32-bit word, red in the low bits, two
/// padding bits on top (DRM's `XB30`). The HDR10 scanout format.
pub const DRM_FORMAT_XBGR2101010: u32 = fourcc(b'X', b'B', b'3', b'0');
/// `XB30` with the top two bits as alpha (DRM's `AB30`).
pub const DRM_FORMAT_ABGR2101010: u32 = fourcc(b'A', b'B', b'3', b'0');
/// 10-bit RGB, blue in the low bits (DRM's `XR30`).
pub const DRM_FORMAT_XRGB2101010: u32 = fourcc(b'X', b'R', b'3', b'0');
/// `XR30` with the top two bits as alpha (DRM's `AR30`).
pub const DRM_FORMAT_ARGB2101010: u32 = fourcc(b'A', b'R', b'3', b'0');
/// Half-float RGBA, red first in memory (DRM's `AB4H`).
pub const DRM_FORMAT_ABGR16161616F: u32 = fourcc(b'A', b'B', b'4', b'H');
/// Half-float RGBX, red first in memory (DRM's `XB4H`).
pub const DRM_FORMAT_XBGR16161616F: u32 = fourcc(b'X', b'B', b'4', b'H');

/// The YCbCr→RGB matrix the samples were encoded with. `Identity` means the
/// samples are already RGB and need no conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YuvMatrix {
    /// SD / JPEG-era coefficients.
    Bt601,
    /// HD coefficients — what 1080p H.264 almost always carries.
    Bt709,
    /// UHD / HDR coefficients.
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

/// How the sample values use their numeric range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorRange {
    /// "TV" / limited: luma 16..235, chroma 16..240 at 8 bits.
    Limited,
    /// "PC" / full: 0..255 at 8 bits.
    Full,
}

/// The colour convention a frame's samples follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colorimetry {
    pub matrix: YuvMatrix,
    pub range: ColorRange,
}

impl Colorimetry {
    /// The safe assumption for HD video when the stream says nothing:
    /// BT.709 coefficients over a limited range.
    pub const BT709_LIMITED: Self = Self {
        matrix: YuvMatrix::Bt709,
        range: ColorRange::Limited,
    };

    /// Straight RGB — no conversion applies.
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

/// One plane of a [`DmabufImage`]: its fd plus where the plane starts inside
/// it and how wide a row is. Several planes may name the *same* buffer at
/// different offsets (the usual single-fd NV12 export) or each carry its own.
#[derive(Debug)]
pub struct DmabufPlane {
    pub fd: DmabufHandle,
    pub offset: u32,
    pub stride: u32,
}

/// A decoded picture living in GPU memory, described well enough to import.
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

    /// Whether every plane names the same underlying buffer. When true, an
    /// importer allocates one `VkDeviceMemory` and binds each plane image at
    /// its own offset; when false, each plane is imported separately.
    ///
    /// Compares every plane's fd against the first by identity (`fstat`
    /// device + inode). An image with fewer than two planes has nothing to
    /// disagree with and reports `true`.
    pub fn planes_share_one_buffer(&self) -> bool {
        // `split_first` rather than `planes[0]` / `planes[1..]`: no index is
        // computed, so the empty case is a branch of the pattern instead of an
        // invariant someone has to remember.
        let Some((first, rest)) = self.planes.split_first() else {
            return true;
        };
        rest.iter().all(|p| same_file(first, p))
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

/// Whether two planes' fds refer to the same underlying file description.
/// dmabufs are anonymous, so device+inode is the only identity available.
#[cfg(unix)]
fn same_file(a: &DmabufPlane, b: &DmabufPlane) -> bool {
    use std::os::unix::fs::MetadataExt;
    let stat = |fd: &OwnedFd| -> Option<(u64, u64)> {
        // Let std select the target's actual stat ABI. The temporary File owns
        // a duplicate, so metadata inspection cannot close the producer's fd.
        let file = std::fs::File::from(fd.try_clone().ok()?);
        let metadata = file.metadata().ok()?;
        Some((metadata.dev(), metadata.ino()))
    };
    match (stat(&a.fd), stat(&b.fd)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_a: &DmabufPlane, _b: &DmabufPlane) -> bool {
    false
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
        // Phase `linearity_covers_the_unstated_modifier_but_not_a_tiled_one`.
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
        // Phase `describe_names_the_format_and_layout`.
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
        // Phase `dmabuf_size_reads_the_end_offset_of_a_real_fd`.
        {
            // A plain file stands in for a dmabuf here: both answer SEEK_END, and
            // this proves the binding returns the byte length rather than -1.
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
        // Phase `planes_sharing_one_fd_are_detected_as_one_buffer`.
        {
            let dir =
                std::env::temp_dir().join(format!("tilo-dmabuf-share-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let a = dir.join("a");
            let b = dir.join("b");
            std::fs::write(&a, vec![0u8; 64]).unwrap();
            std::fs::write(&b, vec![0u8; 64]).unwrap();

            let fd_a = OwnedFd::from(std::fs::File::open(&a).unwrap());
            let fd_a2 = fd_a.try_clone().unwrap();
            let shared = DmabufImage {
                planes: vec![
                    DmabufPlane {
                        fd: fd_a,
                        offset: 0,
                        stride: 8,
                    },
                    DmabufPlane {
                        fd: fd_a2,
                        offset: 32,
                        stride: 8,
                    },
                ],
                fourcc: DRM_FORMAT_NV12,
                modifier: 0,
                width: 8,
                height: 4,
                colorimetry: Colorimetry::default(),
            };
            assert!(
                shared.planes_share_one_buffer(),
                "same inode → one buffer, bind at offsets"
            );

            let split = DmabufImage {
                planes: vec![
                    DmabufPlane {
                        fd: OwnedFd::from(std::fs::File::open(&a).unwrap()),
                        offset: 0,
                        stride: 8,
                    },
                    DmabufPlane {
                        fd: OwnedFd::from(std::fs::File::open(&b).unwrap()),
                        offset: 0,
                        stride: 8,
                    },
                ],
                fourcc: DRM_FORMAT_NV12,
                modifier: 0,
                width: 8,
                height: 4,
                colorimetry: Colorimetry::default(),
            };
            assert!(
                !split.planes_share_one_buffer(),
                "distinct inodes → import each plane"
            );

            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
