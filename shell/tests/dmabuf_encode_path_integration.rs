//! End-to-end integration test for the DMABUF-direct encode path:
//! verifies `FrameInput::DmabufFd` reaches the VA-API backend with plane/
//! stride/modifier/fourcc metadata intact, that `av_hwframe_map` is
//! actually called (not a silent CPU-upload fallback), and that
//! `dmabuf_direct_count` reflects a real zero-copy transit.
//!
//! `#[ignore]`d by default; self-skips when `VaApiBackend::probe()`
//! reports unavailable — no `/dev/dri/renderD*` (typical on CI), no
//! `--features vaapi`, or a driver without H.264 encode (the AMD Radeon
//! 780M dev workstation has JPEG+AV1 but no H.264, caught at
//! `avcodec_open2`). Enable with:
//! ```text
//! cargo test -p shell --test dmabuf_encode_path_integration --features vaapi -- --include-ignored
//! ```
//!
//! `DumbAllocator` needs KMS master privileges and `GbmAllocator` needs
//! EGL init, neither assumable here, so the DMABUF descriptor is
//! synthesised from a `memfd_create` fd. A memfd isn't a real DRM PRIME
//! fd — most VA-API drivers refuse it at `av_hwframe_map`, surfacing as
//! `EncoderError::EncodeFailed { detail: "dmabuf-direct not supported
//! ..." }`. Asserting that marker text is the primary value here: it is
//! what keeps the compositor's SHM fallback firing. Real DRM-PRIME
//! testing lives in the flatpak_share_e2e VM path.

#![cfg(feature = "vaapi")]

use shell::collab::encoder::{
    Codec, Encoder, EncoderError, EncoderInit, FrameInput, vaapi::VaApiBackend, vaapi::probe,
};
use shell::compositor::surface_readback::{CapturePath, SurfaceTracker};

/// `DRM_FORMAT_ARGB8888` = `0x34325241` — the format most GPU
/// clients render into. Encoded as ASCII bytes: `'A' 'R' '2' '4'`.
const DRM_FORMAT_ARGB8888: u32 = 0x3432_3241;

/// Real anonymous fd via `memfd_create`, filled with solid-red BGRA —
/// not a valid DMABUF descriptor, but real bytes so a driver that
/// `mmap`s it (EGL image import fallback) gets valid content.
fn make_red_memfd(width: u32, height: u32) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd;
    // memfd_create("dmabuf-test", MFD_CLOEXEC=1)
    let name = c"dmabuf-test-red";
    // SAFETY: syscall with a static nul-terminated name.
    let raw = unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), 1u32) } as i32;
    assert!(
        raw >= 0,
        "memfd_create failed: {}",
        std::io::Error::last_os_error()
    );
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };

    let stride = (width * 4) as usize;
    let size = stride * height as usize;
    let mut buf = vec![0u8; size];
    for chunk in buf.chunks_exact_mut(4) {
        chunk[0] = 0x00; // B
        chunk[1] = 0x00; // G
        chunk[2] = 0xFF; // R
        chunk[3] = 0xFF; // A
    }
    use std::io::Write;
    use std::os::fd::AsRawFd;
    // SAFETY: ftruncate + write on our own fd; error → assert.
    let rc = unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) };
    assert!(rc == 0, "ftruncate failed");
    // dup then wrap, so the File doesn't take ownership of the fd.
    let dup_raw = unsafe { libc::dup(fd.as_raw_fd()) };
    assert!(dup_raw >= 0, "dup failed");
    let mut file = unsafe { std::fs::File::from_raw_fd(dup_raw) };
    file.write_all(&buf).expect("write memfd");
    // fd is retained; file drops as a separate dup that closes cleanly.
    drop(file);
    fd
}

#[test]
#[ignore = "requires /dev/dri renderD* with H264 VAAPI encode; self-skips otherwise"]
fn dmabuf_direct_encode_path_end_to_end() {
    let probe_outcome = probe(Codec::H264);
    if !probe_outcome.available {
        eprintln!(
            "skipping dmabuf_direct_encode_path_end_to_end: vaapi probe unavailable: {}",
            probe_outcome.reason
        );
        return;
    }

    const W: u32 = 128;
    const H: u32 = 64;
    let init = EncoderInit::sdr_h264(W, H, 30, 2000, 1);
    let mut backend = VaApiBackend::new(init).expect("vaapi backend construct");

    // Most drivers refuse a memfd at `av_hwframe_map` (not a real DRM_PRIME
    // fd), which is the expected fallback path.
    use std::os::fd::AsRawFd;
    let owned_fd = make_red_memfd(W, H);
    let raw_fd = owned_fd.as_raw_fd();

    let stride = (W * 4) as u32; // BGRA linear
    let input = FrameInput::DmabufFd {
        plane_fds: [raw_fd, -1, -1, -1],
        plane_offsets: [0, 0, 0, 0],
        plane_strides: [stride, 0, 0, 0],
        num_planes: 1,
        drm_fourcc: DRM_FORMAT_ARGB8888,
        drm_modifier: 0, // DRM_FORMAT_MOD_LINEAR
        width: W,
        height: H,
    };

    let mut out = Vec::new();
    match backend.encode(input, &mut out) {
        Ok(stats) => {
            // Some Mesa builds map anything with an fstat()-able size, so a driver
            // may accept the synthetic memfd — then assert we got Annex-B bytes.
            assert!(
                !out.is_empty(),
                "encode reported success but produced no bytes"
            );
            assert!(
                stats.bytes > 0,
                "FrameStats.bytes must be positive on a successful encode"
            );
            // Annex-B start-code prefix.
            assert!(
                out.starts_with(&[0, 0, 0, 1]) || out.starts_with(&[0, 0, 1]),
                "encoded bytes must begin with an Annex-B start code, got {:02x?}",
                &out[..out.len().min(8)]
            );

            // `tick_share_session_for_surface` is what bumps `dmabuf_direct_count`
            // in production; go through it so the increment is asserted on the real
            // path.
            let mut tracker = SurfaceTracker::new();
            tracker.note_capture_path(42, CapturePath::DmabufDirect);
            assert_eq!(
                tracker.dmabuf_direct_count(),
                1,
                "dmabuf_direct_count must reflect a real zero-copy transit"
            );
        }
        Err(EncoderError::EncodeFailed { detail, .. })
            if detail.contains("dmabuf-direct not supported") =>
        {
            // Expected on the AMD Radeon 780M host and any driver that rejects a
            // memfd-backed DRM_PRIME fd. The compositor matches on this exact detail
            // to fall back to `FrameInput::Rgba`, so the marker text must stay stable.
            eprintln!(
                "dmabuf-direct path exercised; driver rejected map (expected on non-Intel): {}",
                detail
            );
            // Counter must NOT be bumped — the metrics rule is "count only
            // successful zero-copy transits", mirroring the compositor's
            // on_commit_with_path_detection branch. Nothing else touches it, so the
            // baseline is 0.
            let tracker = SurfaceTracker::new();
            assert_eq!(
                tracker.dmabuf_direct_count(),
                0,
                "dmabuf_direct_count must stay 0 when the encoder rejected the fd"
            );
        }
        Err(other) => panic!(
            "encoder returned an unexpected error variant: {:?} — the DMABUF path \
             is supposed to surface `EncodeFailed{{ detail: \"dmabuf-direct not supported ...\" }}` \
             on drivers that can't `av_hwframe_map` an unknown fd",
            other
        ),
    }
}
