//! What the kernel's V4L2 memory-to-memory codecs can actually do.
//!
//! `GStreamer`'s V4L2 elements advertise unbounded sizes in their pad
//! templates: `v4l2slh264dec` says it takes any H.264 up to 2³¹ pixels wide,
//! while the Hantro G1 behind it on an RK3566 stops at 1920×1088 and refuses
//! the stream only once caps arrive. The kernel knows the real limits, so this
//! module asks it directly — `VIDIOC_QUERYCAP`, `VIDIOC_ENUM_FMT` and
//! `VIDIOC_ENUM_FRAMESIZES` on every `/dev/video*` memory-to-memory node — and
//! the answers explain a software fallback ("this chip decodes H.264 only up to
//! 1920×1088") instead of leaving it a mystery.
//!
//! The probe only opens and queries nodes; it never allocates buffers or
//! starts streaming, so it is safe to run next to a live decoder.

/// What a memory-to-memory node does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V4l2Role {
    /// Compressed in, raw pictures out. `stateless` when the driver takes
    /// parsed slices plus per-frame controls (Hantro, rkvdec, Cedrus) rather
    /// than a raw bitstream (Venus, Amphion, the Raspberry Pi's HEVC block).
    Decoder { stateless: bool },
    /// Raw pictures in, compressed out.
    Encoder,
    /// Raw in, raw out: a scaler / colour converter such as Rockchip's RGA.
    Converter,
}

/// One compressed format a node takes or produces, with the largest picture
/// the kernel will configure for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodedFormat {
    /// The V4L2 pixel format, e.g. `S264`.
    pub fourcc: u32,
    /// The `GStreamer` media type this bitstream is, e.g. `video/x-h264`.
    pub media_type: &'static str,
    /// Largest coded size, when the driver reports one.
    pub max_size: Option<(u32, u32)>,
}

/// A V4L2 memory-to-memory codec or converter node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V4l2Device {
    pub path: String,
    pub driver: String,
    /// The driver's card name — on Rockchip the compatible string, e.g.
    /// `rockchip,rk3568-vpu-dec`.
    pub card: String,
    pub role: V4l2Role,
    /// Compressed formats: the input of a decoder, the output of an encoder.
    pub coded: Vec<CodedFormat>,
    /// Raw picture formats on the other side, as V4L2 fourccs.
    pub raw: Vec<u32>,
}

impl V4l2Device {
    /// The coded format for `media_type`, if this node handles it.
    pub fn coded_format(&self, media_type: &str) -> Option<&CodedFormat> {
        self.coded.iter().find(|c| c.media_type == media_type)
    }

    /// One line for logs: `rockchip,rk3568-vpu-dec (hantro-vpu, stateless
    /// decoder): H.264 ≤1920x1088, VP8 ≤3840x2160`.
    pub fn describe(&self) -> String {
        let role = match self.role {
            V4l2Role::Decoder { stateless: true } => "stateless decoder",
            V4l2Role::Decoder { stateless: false } => "stateful decoder",
            V4l2Role::Encoder => "encoder",
            V4l2Role::Converter => "converter",
        };
        let coded: Vec<String> = self
            .coded
            .iter()
            .map(|c| match c.max_size {
                Some((w, h)) => format!("{} ≤{w}x{h}", codec_label(c.media_type)),
                None => codec_label(c.media_type).to_string(),
            })
            .collect();
        if coded.is_empty() {
            format!("{} ({}, {role})", self.card, self.driver)
        } else {
            format!(
                "{} ({}, {role}): {}",
                self.card,
                self.driver,
                coded.join(", ")
            )
        }
    }
}

/// A human codec name for a media type.
pub fn codec_label(media_type: &str) -> &'static str {
    match media_type {
        MEDIA_H264 => "H.264",
        MEDIA_HEVC => "HEVC",
        MEDIA_VP8 => "VP8",
        MEDIA_VP9 => "VP9",
        MEDIA_AV1 => "AV1",
        MEDIA_MPEG2 => "MPEG-2",
        MEDIA_JPEG => "JPEG",
        _ => "unknown codec",
    }
}

pub const MEDIA_H264: &str = "video/x-h264";
pub const MEDIA_HEVC: &str = "video/x-h265";
pub const MEDIA_VP8: &str = "video/x-vp8";
pub const MEDIA_VP9: &str = "video/x-vp9";
pub const MEDIA_AV1: &str = "video/x-av1";
pub const MEDIA_MPEG2: &str = "video/mpeg, mpegversion=(int)2";
pub const MEDIA_JPEG: &str = "image/jpeg";

const fn v4l2_fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    crate::dmabuf::fourcc(a, b, c, d)
}

/// The bitstream a compressed V4L2 pixel format carries, and whether it is
/// the stateless (parsed-slice / per-frame-control) flavour.
///
/// Only compressed formats map; a raw format answers `None`.
pub fn coded_media_type(fourcc: u32) -> Option<(&'static str, bool)> {
    const TABLE: &[(u32, &str, bool)] = &[
        (v4l2_fourcc(b'S', b'2', b'6', b'4'), MEDIA_H264, true),
        (v4l2_fourcc(b'H', b'2', b'6', b'4'), MEDIA_H264, false),
        (v4l2_fourcc(b'A', b'V', b'C', b'1'), MEDIA_H264, false),
        (v4l2_fourcc(b'S', b'2', b'6', b'5'), MEDIA_HEVC, true),
        (v4l2_fourcc(b'H', b'E', b'V', b'C'), MEDIA_HEVC, false),
        (v4l2_fourcc(b'V', b'P', b'8', b'F'), MEDIA_VP8, true),
        (v4l2_fourcc(b'V', b'P', b'8', b'0'), MEDIA_VP8, false),
        (v4l2_fourcc(b'V', b'P', b'9', b'F'), MEDIA_VP9, true),
        (v4l2_fourcc(b'V', b'P', b'9', b'0'), MEDIA_VP9, false),
        (v4l2_fourcc(b'A', b'V', b'1', b'F'), MEDIA_AV1, true),
        (v4l2_fourcc(b'A', b'V', b'0', b'1'), MEDIA_AV1, false),
        (v4l2_fourcc(b'M', b'G', b'2', b'S'), MEDIA_MPEG2, true),
        (v4l2_fourcc(b'M', b'P', b'G', b'2'), MEDIA_MPEG2, false),
        (v4l2_fourcc(b'J', b'P', b'E', b'G'), MEDIA_JPEG, false),
        (v4l2_fourcc(b'M', b'J', b'P', b'G'), MEDIA_JPEG, false),
    ];
    TABLE
        .iter()
        .find(|(f, _, _)| *f == fourcc)
        .map(|(_, media, stateless)| (*media, *stateless))
}

/// The bitstream a `GStreamer` codec element handles, read from the codec
/// token in its factory or instance name (`v4l2slh264dec0`, `avdec_vp9`,
/// `dav1ddec`, `vah265enc`).
pub fn element_media_type(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    // `dav1ddec` and `rav1enc` carry the token inside a longer word.
    if n.contains("av1") {
        Some(MEDIA_AV1)
    } else if n.contains("h264") {
        Some(MEDIA_H264)
    } else if n.contains("h265") || n.contains("hevc") || n.contains("de265") {
        Some(MEDIA_HEVC)
    } else if n.contains("vp9") {
        Some(MEDIA_VP9)
    } else if n.contains("vp8") {
        Some(MEDIA_VP8)
    } else if n.contains("mpeg2") {
        Some(MEDIA_MPEG2)
    } else if n.contains("jpeg") {
        Some(MEDIA_JPEG)
    } else {
        None
    }
}

/// The largest size a `VIDIOC_ENUM_FRAMESIZES` walk reported. Discrete
/// entries contribute their own size; a stepwise or continuous range its
/// maximum.
pub fn max_frame_size(entries: &[FrameSizeEntry]) -> Option<(u32, u32)> {
    entries
        .iter()
        .map(|e| match *e {
            FrameSizeEntry::Discrete { width, height } => (width, height),
            FrameSizeEntry::Range {
                max_width,
                max_height,
            } => (max_width, max_height),
        })
        .max_by_key(|&(w, h)| u64::from(w).saturating_mul(u64::from(h)))
}

/// One `VIDIOC_ENUM_FRAMESIZES` answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameSizeEntry {
    Discrete { width: u32, height: u32 },
    Range { max_width: u32, max_height: u32 },
}

/// Classify a node from the formats on its two queues. `output_*` is the
/// queue the application writes (`V4L2_BUF_TYPE_VIDEO_OUTPUT*`), `capture_*`
/// the one it reads.
pub fn classify_node(output: &[(u32, bool)], capture: &[(u32, bool)]) -> Option<V4l2Role> {
    let coded_in = output.iter().any(|&(_, compressed)| compressed);
    let coded_out = capture.iter().any(|&(_, compressed)| compressed);
    match (coded_in, coded_out) {
        (true, false) => {
            let stateless = output
                .iter()
                .filter(|&&(_, compressed)| compressed)
                .any(|&(f, _)| coded_media_type(f).is_some_and(|(_, s)| s));
            Some(V4l2Role::Decoder { stateless })
        }
        (false, true) => Some(V4l2Role::Encoder),
        (false, false) if !output.is_empty() && !capture.is_empty() => Some(V4l2Role::Converter),
        _ => None,
    }
}

/// Whether some node on this machine decodes `media_type` in hardware, and
/// the largest picture it takes.
///
/// `Some(None)` is "a decoder exists but did not say how large".
pub fn decoder_limit(devices: &[V4l2Device], media_type: &str) -> Option<Option<(u32, u32)>> {
    devices
        .iter()
        .filter(|d| matches!(d.role, V4l2Role::Decoder { .. }))
        .filter_map(|d| d.coded_format(media_type))
        .map(|c| c.max_size)
        .max_by_key(|s| s.map_or(0, |(w, h)| u64::from(w).saturating_mul(u64::from(h))))
}

/// Whether a `width`×`height` picture fits a `(max_w, max_h)` limit. Either
/// orientation fits: decoders size their buffers by macroblock count, and a
/// portrait 1080×1920 stream decodes wherever 1920×1080 does.
pub fn fits(limit: (u32, u32), width: u32, height: u32) -> bool {
    let (max_w, max_h) = limit;
    (width <= max_w && height <= max_h) || (width <= max_h && height <= max_w)
}

/// Every memory-to-memory node on this machine, probed once per process.
pub fn devices() -> &'static [V4l2Device] {
    static DEVICES: std::sync::OnceLock<Vec<V4l2Device>> = std::sync::OnceLock::new();
    DEVICES.get_or_init(probe_all)
}

#[cfg(target_os = "linux")]
fn probe_all() -> Vec<V4l2Device> {
    let Ok(dir) = std::fs::read_dir("/dev") else {
        return Vec::new();
    };
    let mut paths: Vec<String> = dir
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            n.strip_prefix("video")
                .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|n| format!("/dev/{n}"))
        .collect();
    paths.sort();
    let out: Vec<V4l2Device> = paths.iter().filter_map(|p| sys::probe(p)).collect();
    for d in &out {
        tracing::debug!("media: v4l2 {} — {}", d.path, d.describe());
    }
    out
}

#[cfg(not(target_os = "linux"))]
fn probe_all() -> Vec<V4l2Device> {
    Vec::new()
}

#[cfg(target_os = "linux")]
mod sys {
    //! The three V4L2 ioctls the probe needs, bound directly: their layouts
    //! are stable kernel ABI, and a whole V4L2 crate for three queries would
    //! be weight without benefit.

    use super::{
        CodedFormat, FrameSizeEntry, V4l2Device, classify_node, coded_media_type, max_frame_size,
    };
    use std::os::fd::{AsRawFd, OwnedFd};

    const V4L2_CAP_VIDEO_M2M_MPLANE: u32 = 0x0000_4000;
    const V4L2_CAP_VIDEO_M2M: u32 = 0x0000_8000;
    const V4L2_CAP_DEVICE_CAPS: u32 = 0x8000_0000;
    const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
    const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
    const V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 9;
    const V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE: u32 = 10;
    const V4L2_FMT_FLAG_COMPRESSED: u32 = 0x0001;
    const V4L2_FRMSIZE_TYPE_DISCRETE: u32 = 1;

    #[repr(C)]
    struct Capability {
        driver: [u8; 16],
        card: [u8; 32],
        bus_info: [u8; 32],
        version: u32,
        capabilities: u32,
        device_caps: u32,
        reserved: [u32; 3],
    }

    #[repr(C)]
    struct FmtDesc {
        index: u32,
        kind: u32,
        flags: u32,
        description: [u8; 32],
        pixelformat: u32,
        mbus_code: u32,
        reserved: [u32; 3],
    }

    #[repr(C)]
    struct FrmSizeEnum {
        index: u32,
        pixel_format: u32,
        kind: u32,
        // Discrete: width, height. Stepwise / continuous: min_w, max_w,
        // step_w, min_h, max_h, step_h.
        size: [u32; 6],
        reserved: [u32; 2],
    }

    const fn ioc(dir: u32, nr: u32, size: usize) -> libc::c_ulong {
        // `size` is a struct size well under the 14-bit field.
        #[allow(clippy::cast_possible_truncation, reason = "ioctl structs are tiny")]
        let size = size as u32;
        ((dir << 30) | (size << 16) | ((b'V' as u32) << 8) | nr) as libc::c_ulong
    }
    const VIDIOC_QUERYCAP: libc::c_ulong = ioc(2, 0, std::mem::size_of::<Capability>());
    const VIDIOC_ENUM_FMT: libc::c_ulong = ioc(3, 2, std::mem::size_of::<FmtDesc>());
    const VIDIOC_ENUM_FRAMESIZES: libc::c_ulong = ioc(3, 74, std::mem::size_of::<FrmSizeEnum>());

    fn cstr(bytes: &[u8]) -> String {
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(bytes.get(..end).unwrap_or_default()).into_owned()
    }

    /// `ioctl` on a struct the kernel reads and writes in place.
    fn query<T>(fd: &OwnedFd, request: libc::c_ulong, arg: &mut T) -> bool {
        // SAFETY: `arg` is a live, exclusively borrowed `#[repr(C)]` struct
        // whose size is encoded in `request`, which is exactly what the
        // kernel reads and writes.
        unsafe { libc::ioctl(fd.as_raw_fd(), request as _, std::ptr::from_mut(arg)) == 0 }
    }

    fn formats(fd: &OwnedFd, kind: u32) -> Vec<(u32, bool)> {
        let mut out = Vec::new();
        for index in 0..64 {
            // SAFETY: an all-zero `FmtDesc` is a valid value of a plain-old-data struct.
            let mut desc: FmtDesc = unsafe { std::mem::zeroed() };
            desc.index = index;
            desc.kind = kind;
            if !query(fd, VIDIOC_ENUM_FMT, &mut desc) {
                break;
            }
            out.push((desc.pixelformat, desc.flags & V4L2_FMT_FLAG_COMPRESSED != 0));
        }
        out
    }

    fn frame_sizes(fd: &OwnedFd, fourcc: u32) -> Vec<FrameSizeEntry> {
        let mut out = Vec::new();
        for index in 0..64 {
            // SAFETY: an all-zero `FrmSizeEnum` is a valid value of a plain-old-data struct.
            let mut e: FrmSizeEnum = unsafe { std::mem::zeroed() };
            e.index = index;
            e.pixel_format = fourcc;
            if !query(fd, VIDIOC_ENUM_FRAMESIZES, &mut e) {
                break;
            }
            let [a, b, _, _, max_h, _] = e.size;
            if e.kind == V4L2_FRMSIZE_TYPE_DISCRETE {
                out.push(FrameSizeEntry::Discrete {
                    width: a,
                    height: b,
                });
            } else {
                // A range is reported once, at index 0.
                out.push(FrameSizeEntry::Range {
                    max_width: b,
                    max_height: max_h,
                });
                break;
            }
        }
        out
    }

    pub(super) fn probe(path: &str) -> Option<V4l2Device> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?;
        let fd = OwnedFd::from(file);
        // SAFETY: an all-zero `Capability` is a valid value of a plain-old-data struct.
        let mut cap: Capability = unsafe { std::mem::zeroed() };
        if !query(&fd, VIDIOC_QUERYCAP, &mut cap) {
            return None;
        }
        let caps = if cap.capabilities & V4L2_CAP_DEVICE_CAPS != 0 {
            cap.device_caps
        } else {
            cap.capabilities
        };
        let (out_kind, cap_kind) = if caps & V4L2_CAP_VIDEO_M2M_MPLANE != 0 {
            (
                V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
                V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
            )
        } else if caps & V4L2_CAP_VIDEO_M2M != 0 {
            (V4L2_BUF_TYPE_VIDEO_OUTPUT, V4L2_BUF_TYPE_VIDEO_CAPTURE)
        } else {
            return None;
        };
        let output = formats(&fd, out_kind);
        let capture = formats(&fd, cap_kind);
        let role = classify_node(&output, &capture)?;
        let (coded_side, raw_side) = match role {
            super::V4l2Role::Encoder => (&capture, &output),
            _ => (&output, &capture),
        };
        let coded = coded_side
            .iter()
            .filter(|&&(_, compressed)| compressed)
            .filter_map(|&(fourcc, _)| {
                let (media_type, _) = coded_media_type(fourcc)?;
                Some(CodedFormat {
                    fourcc,
                    media_type,
                    max_size: max_frame_size(&frame_sizes(&fd, fourcc)),
                })
            })
            .collect();
        Some(V4l2Device {
            path: path.to_string(),
            driver: cstr(&cap.driver),
            card: cstr(&cap.card),
            role,
            coded,
            raw: raw_side.iter().map(|&(f, _)| f).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S264: u32 = v4l2_fourcc(b'S', b'2', b'6', b'4');
    const VP8F: u32 = v4l2_fourcc(b'V', b'P', b'8', b'F');
    const MG2S: u32 = v4l2_fourcc(b'M', b'G', b'2', b'S');
    const NV12: u32 = v4l2_fourcc(b'N', b'V', b'1', b'2');
    const NM12: u32 = v4l2_fourcc(b'N', b'M', b'1', b'2');
    const JPEG: u32 = v4l2_fourcc(b'J', b'P', b'E', b'G');
    const H264: u32 = v4l2_fourcc(b'H', b'2', b'6', b'4');

    /// The nodes an Orange Pi 3B (RK3566, mainline 7.2) exposes, as probed.
    fn rk3566() -> Vec<V4l2Device> {
        vec![
            V4l2Device {
                path: "/dev/video0".into(),
                driver: "rockchip-rga".into(),
                card: "rga2".into(),
                role: V4l2Role::Converter,
                coded: vec![],
                raw: vec![NV12, NM12],
            },
            V4l2Device {
                path: "/dev/video1".into(),
                driver: "hantro-vpu".into(),
                card: "rockchip,rk3568-vpu-dec".into(),
                role: V4l2Role::Decoder { stateless: true },
                coded: vec![
                    CodedFormat {
                        fourcc: S264,
                        media_type: MEDIA_H264,
                        max_size: Some((1920, 1088)),
                    },
                    CodedFormat {
                        fourcc: MG2S,
                        media_type: MEDIA_MPEG2,
                        max_size: Some((1920, 1088)),
                    },
                    CodedFormat {
                        fourcc: VP8F,
                        media_type: MEDIA_VP8,
                        max_size: Some((3840, 2160)),
                    },
                ],
                raw: vec![NV12],
            },
            V4l2Device {
                path: "/dev/video2".into(),
                driver: "hantro-vpu".into(),
                card: "rockchip,rk3568-vepu-enc".into(),
                role: V4l2Role::Encoder,
                coded: vec![CodedFormat {
                    fourcc: JPEG,
                    media_type: MEDIA_JPEG,
                    max_size: Some((8192, 8192)),
                }],
                raw: vec![NM12],
            },
        ]
    }

    #[test]
    fn nodes_are_classified_by_which_queue_is_compressed() {
        // Hantro G1: parsed H.264 slices in, NV12 out.
        assert_eq!(
            classify_node(&[(S264, true), (VP8F, true)], &[(NV12, false)]),
            Some(V4l2Role::Decoder { stateless: true })
        );
        // A stateful decoder takes the raw bitstream.
        assert_eq!(
            classify_node(&[(H264, true)], &[(NV12, false)]),
            Some(V4l2Role::Decoder { stateless: false })
        );
        // VEPU: NV12 in, JPEG out.
        assert_eq!(
            classify_node(&[(NM12, false)], &[(JPEG, true)]),
            Some(V4l2Role::Encoder)
        );
        // RGA: raw on both sides.
        assert_eq!(
            classify_node(&[(NV12, false)], &[(NV12, false)]),
            Some(V4l2Role::Converter)
        );
        // A capture-only node (a camera, a loopback) is not memory-to-memory.
        assert_eq!(classify_node(&[], &[(NV12, false)]), None);
    }

    #[test]
    fn compressed_fourccs_map_to_bitstreams_and_raw_ones_do_not() {
        assert_eq!(coded_media_type(S264), Some((MEDIA_H264, true)));
        assert_eq!(coded_media_type(H264), Some((MEDIA_H264, false)));
        assert_eq!(coded_media_type(VP8F), Some((MEDIA_VP8, true)));
        assert_eq!(coded_media_type(MG2S), Some((MEDIA_MPEG2, true)));
        assert_eq!(coded_media_type(NV12), None);
        assert_eq!(
            coded_media_type(v4l2_fourcc(b'V', b'P', b'9', b'F')),
            Some((MEDIA_VP9, true))
        );
        assert_eq!(
            coded_media_type(v4l2_fourcc(b'S', b'2', b'6', b'5')),
            Some((MEDIA_HEVC, true))
        );
        assert_eq!(
            coded_media_type(v4l2_fourcc(b'A', b'V', b'1', b'F')),
            Some((MEDIA_AV1, true))
        );
    }

    #[test]
    fn element_names_resolve_to_their_bitstream() {
        for (name, want) in [
            ("v4l2slh264dec0", Some(MEDIA_H264)),
            ("v4l2slh265dec", Some(MEDIA_HEVC)),
            ("vah265enc", Some(MEDIA_HEVC)),
            ("openh264dec", Some(MEDIA_H264)),
            ("avdec_h264-0", Some(MEDIA_H264)),
            ("v4l2slvp8dec", Some(MEDIA_VP8)),
            ("vp9dec0", Some(MEDIA_VP9)),
            ("dav1ddec", Some(MEDIA_AV1)),
            ("v4l2slav1dec", Some(MEDIA_AV1)),
            ("v4l2slmpeg2dec", Some(MEDIA_MPEG2)),
            ("v4l2jpegenc", Some(MEDIA_JPEG)),
            ("videoconvert0", None),
        ] {
            assert_eq!(element_media_type(name), want, "{name}");
        }
    }

    #[test]
    fn frame_size_limits_come_from_the_largest_reported_entry() {
        assert_eq!(
            max_frame_size(&[FrameSizeEntry::Range {
                max_width: 1920,
                max_height: 1088
            }]),
            Some((1920, 1088))
        );
        assert_eq!(
            max_frame_size(&[
                FrameSizeEntry::Discrete {
                    width: 640,
                    height: 480
                },
                FrameSizeEntry::Discrete {
                    width: 1280,
                    height: 720
                },
            ]),
            Some((1280, 720))
        );
        assert_eq!(max_frame_size(&[]), None);
    }

    #[test]
    fn the_rk3566_limits_explain_which_streams_reach_hardware() {
        let devices = rk3566();
        let h264 = decoder_limit(&devices, MEDIA_H264).flatten().unwrap();
        assert!(fits(h264, 1920, 1080), "1080p H.264 decodes on the G1");
        assert!(fits(h264, 1080, 1920), "so does portrait 1080p");
        assert!(!fits(h264, 3840, 2160), "4K H.264 does not");
        assert_eq!(
            decoder_limit(&devices, MEDIA_HEVC),
            None,
            "mainline has no HEVC decoder for the RK3566"
        );
        assert_eq!(decoder_limit(&devices, MEDIA_VP9), None);
        assert_eq!(decoder_limit(&devices, MEDIA_AV1), None);
        assert!(fits(
            decoder_limit(&devices, MEDIA_VP8).flatten().unwrap(),
            3840,
            2160
        ));
        // An encoder that only produces JPEG is not an H.264 decoder.
        assert_eq!(decoder_limit(&devices, MEDIA_JPEG), None);
        assert_eq!(
            devices[1].describe(),
            "rockchip,rk3568-vpu-dec (hantro-vpu, stateless decoder): \
             H.264 ≤1920x1088, MPEG-2 ≤1920x1088, VP8 ≤3840x2160"
        );
    }
}
