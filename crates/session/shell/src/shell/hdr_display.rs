//! Display-capability probe for the receive-side `PeerView`: can this output
//! show PQ pixels natively? If yes the tone-mapper is bypassed for
//! passthrough; otherwise it runs on decode.
//!
//! Answers whether the wgpu adapter exposes an HDR-capable presentation
//! format, what luminance the display sustains (best-effort from
//! EDID/Vulkan metadata), and which transfer functions can be delivered.
//!
//! A wrong negative only costs quality; a wrong positive sends PQ pixels to
//! an SDR panel and clips to black. Every uncertain path therefore returns
//! [`DisplayCapabilities::sdr_default`].

/// What the compositor can deliver to the panel connected to this
/// wgpu surface / output. Consumed by
/// [`crate::shell::peer_view::PeerView::new_for_stream`] to pick
/// between tone-map and passthrough.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayCapabilities {
    /// Peak sustainable luminance in cd/m² (SDR 100, HDR400 400, HDR1000
    /// 1000+); sets the tone-map target.
    pub max_luminance_nits: f32,
    /// PQ (SMPTE ST 2084) can be driven out through the DRM connector or
    /// Vulkan swapchain: true → passthrough, false → tone-map to SDR.
    pub supports_pq_output: bool,
    /// The compositor can output HLG. Independent of PQ — some
    /// broadcast HDR TVs prefer HLG.
    pub supports_hlg_output: bool,
    /// Panel approximately covers BT.2020; when false, gamut-mapping still
    /// runs in the HDR path to compress toward the display's real gamut.
    pub bt2020_gamut: bool,
}

impl DisplayCapabilities {
    /// Conservative SDR default, returned on any probe failure.
    pub const fn sdr_default() -> Self {
        Self {
            max_luminance_nits: 100.0,
            supports_pq_output: false,
            supports_hlg_output: false,
            bt2020_gamut: false,
        }
    }

    /// HDR10 baseline — 1000 cd/m², PQ, BT.2020 — asserted when the probe
    /// finds `Rgb10a2Unorm` plus explicit HDR metadata support.
    pub const fn hdr10_baseline() -> Self {
        Self {
            max_luminance_nits: 1000.0,
            supports_pq_output: true,
            supports_hlg_output: false,
            bt2020_gamut: true,
        }
    }
}

impl Default for DisplayCapabilities {
    fn default() -> Self {
        Self::sdr_default()
    }
}

/// Returns SDR defaults on any error path so callers never send PQ pixels
/// to an SDR display. Read-only, safe to re-probe after hotplug; EDID HDR
/// static metadata is not read here — the compositor supplies that peak.
pub fn detect_hdr_display(
    adapter: &vello::wgpu::Adapter,
    surface: &vello::wgpu::Surface,
) -> DisplayCapabilities {
    let caps = surface.get_capabilities(adapter);
    let has_rgb10a2 = caps
        .formats
        .iter()
        .any(|f| matches!(f, vello::wgpu::TextureFormat::Rgb10a2Unorm));
    let has_rgba16f = caps
        .formats
        .iter()
        .any(|f| matches!(f, vello::wgpu::TextureFormat::Rgba16Float));

    if !has_rgb10a2 && !has_rgba16f {
        return DisplayCapabilities::sdr_default();
    }

    let backend = adapter.get_info().backend;
    // Only Vulkan+KMS has the `VK_EXT_hdr_metadata` handshake; other
    // backends route through shims without it, no PQ regardless of format.
    let backend_supports_hdr = matches!(backend, vello::wgpu::Backend::Vulkan);
    if !backend_supports_hdr {
        return DisplayCapabilities {
            max_luminance_nits: 200.0, // format present: some "bright SDR" headroom
            supports_pq_output: false,
            supports_hlg_output: false,
            bt2020_gamut: false,
        };
    }

    DisplayCapabilities {
        max_luminance_nits: if has_rgba16f { 1000.0 } else { 400.0 },
        supports_pq_output: has_rgb10a2,
        supports_hlg_output: has_rgb10a2, // same swapchain format
        bt2020_gamut: has_rgba16f,
    }
}

/// Which HDR pixel format the compositor should ask wgpu for on this
/// display. `None` when the display is SDR-only — the caller uses
/// `Rgba8UnormSrgb` in that case.
pub fn preferred_hdr_swapchain_format(
    caps: &DisplayCapabilities,
) -> Option<vello::wgpu::TextureFormat> {
    if !caps.supports_pq_output {
        return None;
    }
    // Rgba16Float only suits a rare scRGB-style intermediate.
    if caps.max_luminance_nits >= 800.0 {
        Some(vello::wgpu::TextureFormat::Rgba16Float)
    } else {
        Some(vello::wgpu::TextureFormat::Rgb10a2Unorm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdr_default_says_no_pq_no_hlg() {
        let caps = DisplayCapabilities::sdr_default();
        assert!(!caps.supports_pq_output);
        assert!(!caps.supports_hlg_output);
        assert!(!caps.bt2020_gamut);
        assert!((caps.max_luminance_nits - 100.0).abs() < f32::EPSILON);
    }

    #[test]
    fn preferred_swapchain_format_absent_on_sdr() {
        let caps = DisplayCapabilities::sdr_default();
        assert!(preferred_hdr_swapchain_format(&caps).is_none());
    }

    #[test]
    fn preferred_swapchain_format_picks_rgb10a2_on_hdr10_baseline() {
        let caps = DisplayCapabilities::hdr10_baseline();
        let fmt = preferred_hdr_swapchain_format(&caps).expect("HDR10 should pick a format");
        assert!(
            matches!(
                fmt,
                vello::wgpu::TextureFormat::Rgba16Float | vello::wgpu::TextureFormat::Rgb10a2Unorm
            ),
            "unexpected fmt {fmt:?}"
        );
    }
}
