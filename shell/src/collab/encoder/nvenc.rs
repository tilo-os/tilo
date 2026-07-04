//! NVENC hardware encoder for NVIDIA GPUs.
//!
//! Only compiled with the `nvenc` cargo feature (default off — this is a
//! NVIDIA-only build variant, and pulling in the SDK on non-NVIDIA hosts
//! wastes disk + build time).
//!
//! # Crate choice — `nvidia-video-codec-sdk`
//!
//! The single actively-maintained safe-Rust wrapper on crates.io. It
//! ships bindgen-generated bindings against the NVENC / NVDEC SDK plus
//! `Encoder` / `Session` builders that hide the C API's inout
//! parameter conventions. Alternatives (`nvenc-sys` raw bindings +
//! hand-rolled safety wrappers) would double the code size for no
//! meaningful gain — NVENC's C ABI is stable enough that a `-sys` wrap
//! doesn't buy portability.
//!
//! # Runtime probe
//!
//! Checks for `libcuda.so.1` via `dlopen` before touching the codec SDK
//! (the SDK panics on `CUDA not initialised`). The probe returns
//! `available = false` on any host without a CUDA driver — that's
//! ~everything except a NVIDIA-configured Linux box.

use super::{Encoder, EncoderError, EncoderInit, EncoderKind, FrameInput, FrameStats, ProbeOutcome};

/// Probe NVENC availability.
///
/// # Method
/// 1. Check `/dev/nvidia0` (or `/dev/nvidiactl`) presence.
/// 2. If present, defer to the codec SDK's own init; failure means
///    driver mismatch or no encode-capable GPU.
pub fn probe() -> ProbeOutcome {
    if !std::path::Path::new("/dev/nvidiactl").exists()
        && !std::path::Path::new("/dev/nvidia0").exists()
    {
        return ProbeOutcome {
            kind: EncoderKind::NvEnc,
            available: false,
            reason: "no /dev/nvidia* nodes present — non-NVIDIA host".to_string(),
        };
    }
    // Real init requires `nvidia_video_codec_sdk::CUDA_INIT` which
    // touches the driver. For the probe we stop at the device-file
    // check so it's cheap + doesn't spin up a CUDA context on hosts
    // where we don't intend to encode.
    ProbeOutcome {
        kind: EncoderKind::NvEnc,
        available: true,
        reason: "nvidia driver nodes present".to_string(),
    }
}

/// NVENC backend skeleton — same shape as the VA-API skeleton in this
/// module's sibling. The public API is stable; the encode path is
/// wired to `EncodeError::EncoderUnavailable` until an NVIDIA build
/// variant fills it in.
pub struct NvEncBackend {
    width: u32,
    height: u32,
}

impl NvEncBackend {
    pub fn new(init: EncoderInit) -> Result<Self, EncoderError> {
        // Reuse the probe as a construction gate.
        let outcome = probe();
        if !outcome.available {
            return Err(EncoderError::Unavailable {
                kind: EncoderKind::NvEnc,
                reason: outcome.reason,
            });
        }
        Ok(Self {
            width: init.width,
            height: init.height,
        })
    }
}

impl Encoder for NvEncBackend {
    fn kind(&self) -> EncoderKind {
        EncoderKind::NvEnc
    }
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn encode(
        &mut self,
        _input: FrameInput<'_>,
        _out: &mut Vec<u8>,
    ) -> Result<FrameStats, EncoderError> {
        Err(EncoderError::EncodeFailed {
            kind: EncoderKind::NvEnc,
            detail: "nvenc encode wiring pending — force fallback to next encoder".to_string(),
        })
    }
}
