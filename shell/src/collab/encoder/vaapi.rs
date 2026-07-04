//! VA-API hardware encoder for Intel iGPU / AMD Radeon / ARM SoCs.
//!
//! Only compiled with the `vaapi` cargo feature.
//!
//! # Crate choice — device-file probe, not `cros-libva` (yet)
//!
//! The original plan was to link `cros-libva` (ChromeOS's upstream
//! safe wrapper over `libva.so.2`). That approach hit a workspace-
//! wide `log` feature conflict: `cros-libva` selects
//! `log/release_max_level_debug` while an existing transitive
//! (`ssip-client`) selects `log/release_max_level_info`, and the two
//! variants are mutually exclusive at compile time.
//!
//! Rather than patch either upstream to unlock the conflict (which
//! requires a fork of both and a `[patch.crates-io]` in the workspace
//! root), this backend probes the render node via the DRM device file
//! directly — `/dev/dri/renderD128` presence + a read-open smoke test
//! is enough to answer "would VA-API work here?". The actual encode
//! path is stubbed to `EncodeError::EncodeFailed` so `SharedFlatpakSession`
//! falls back to openh264 for real byte production. Wiring the coded-
//! buffer read-back is a follow-up once the log-feature conflict is
//! resolved.
//!
//! # DMABUF path
//!
//! Once the encoder path is wired, `FrameInput::DmabufFd` will map
//! straight to `vaCreateSurfaceFromDrmPrime` — that's the zero-copy
//! import point.

use super::{Encoder, EncoderError, EncoderInit, EncoderKind, FrameInput, FrameStats, ProbeOutcome};
use std::path::PathBuf;

/// Probe VA-API availability on this host.
///
/// Cheap probe: enumerate `/dev/dri/renderD*` nodes; require that at
/// least one is openable for reading. That gates on:
/// - the DRM subsystem being present in the kernel
/// - the user running the shell being in the `render`/`video` group
/// - a real GPU actually mapped to a render node
///
/// A follow-up should extend this to `vaQueryConfigProfiles` so we
/// only report available when the render node exposes an H.264 encode
/// profile — some hardware (AMD Phoenix iGPU) does JPEG+AV1 encode
/// but no H.264 encode.
pub fn probe() -> ProbeOutcome {
    let nodes = enumerate_render_nodes();
    if nodes.is_empty() {
        return ProbeOutcome {
            kind: EncoderKind::VaApi,
            available: false,
            reason: "no /dev/dri/renderD* nodes present on this host".to_string(),
        };
    }
    let mut opened = None;
    for node in &nodes {
        match try_open_render_node(node) {
            Ok(_) => {
                opened = Some(node.clone());
                break;
            }
            Err(err) => {
                tracing::debug!(
                    node = %node.display(),
                    error = %err,
                    "vaapi probe: node did not open, trying next"
                );
                continue;
            }
        }
    }
    let node = match opened {
        Some(n) => n,
        None => {
            return ProbeOutcome {
                kind: EncoderKind::VaApi,
                available: false,
                reason: format!(
                    "none of {} render node(s) opened; check `render` group membership",
                    nodes.len()
                ),
            };
        }
    };
    // Honesty gate: `encode()` is currently stubbed pending the
    // `cros-libva` log-feature conflict resolution. Until the encode
    // path actually produces NALs, we report `available = false` so
    // `SharedFlatpakSession` falls through to openh264 and never ships
    // an empty bitstream to viewers. The probe still walks the render
    // nodes so the log line surfaces the hardware that WOULD have been
    // picked once the encode path is wired.
    ProbeOutcome {
        kind: EncoderKind::VaApi,
        available: false,
        reason: format!(
            "found render node {} but vaapi encode wiring is pending; \
             falling through to openh264 for byte production",
            node.display()
        ),
    }
}

fn enumerate_render_nodes() -> Vec<PathBuf> {
    let dri = PathBuf::from("/dev/dri");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dri) {
        for e in entries.flatten() {
            let path = e.path();
            let file = match path.file_name().and_then(|f| f.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if file.starts_with("renderD") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn try_open_render_node(node: &std::path::Path) -> Result<String, String> {
    // Read-open is enough for the probe — it validates the fd table
    // and permissions without allocating a codec context.
    std::fs::OpenOptions::new()
        .read(true)
        .open(node)
        .map_err(|e| format!("{e}"))?;
    Ok("read-open succeeded".to_string())
}

/// VA-API H.264 backend — skeleton.
///
/// Construction succeeds when the probe passes so the runtime probe +
/// metrics + capture-path plumbing exercise the VA-API branch. Encode
/// currently returns `EncodeFailed` so `SharedFlatpakSession` falls
/// back to openh264 for byte production. Wiring the full VA-API
/// encoder is queued behind resolving the `log/release_max_level_*`
/// conflict with `cros-libva`.
pub struct VaApiBackend {
    width: u32,
    height: u32,
    render_node: PathBuf,
}

impl VaApiBackend {
    pub fn new(init: EncoderInit) -> Result<Self, EncoderError> {
        let nodes = enumerate_render_nodes();
        let mut chosen = None;
        for node in &nodes {
            if try_open_render_node(node).is_ok() {
                chosen = Some(node.clone());
                break;
            }
        }
        let render_node = chosen.ok_or_else(|| EncoderError::Unavailable {
            kind: EncoderKind::VaApi,
            reason: "no openable render node".to_string(),
        })?;
        tracing::info!(
            node = %render_node.display(),
            width = init.width,
            height = init.height,
            "vaapi backend constructed (encode stubbed pending upstream log-feature fix)"
        );
        Ok(Self {
            width: init.width,
            height: init.height,
            render_node,
        })
    }
}

impl Encoder for VaApiBackend {
    fn kind(&self) -> EncoderKind {
        EncoderKind::VaApi
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
            kind: EncoderKind::VaApi,
            detail: format!(
                "vaapi encode wiring pending — probe opened {}",
                self.render_node.display()
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The probe test asserts we correctly detect *available* on a host
    // with a render node. #[ignore]d because CI runners don't ship
    // one; enable manually with `--include-ignored` on a workstation.
    #[test]
    #[ignore = "requires /dev/dri render node access"]
    fn vaapi_probe_is_available_on_real_host() {
        let outcome = probe();
        assert!(
            outcome.available,
            "vaapi probe reported unavailable — reason: {}",
            outcome.reason
        );
    }
}
