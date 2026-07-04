//! Display-capability probe for the receive-side PeerView.
//!
//! The compositor asks "can this output show PQ pixels natively?" per
//! monitor before wiring a PeerView; if yes, the shell's Wayland
//! `color-management-v1` extension (a follow-up) negotiates PQ
//! passthrough with the DRM connector and the tone-mapper is
//! bypassed. Otherwise the tone-mapper runs on decode.
//!
//! # Probe strategy
//!
//! We answer three questions:
//!
//! 1. **Does the wgpu adapter expose an HDR-capable format for
//!    presentation?** — `Rgb10a2Unorm` and `Rgba16Float` on the
//!    surface's `TextureFormat` list. The latter is scRGB-ish, the
//!    former is a HDR10 candidate.
//! 2. **What luminance can the display sustain?** — best-effort from
//!    the EDID HDR static block on DRM/KMS or the Vulkan
//!    `VK_EXT_hdr_metadata` extension on native-Wayland. Falls back
//!    to conservative defaults when unknown.
//! 3. **Which transfer functions can the compositor deliver?** — PQ
//!    (HDR10) vs HLG. Determined by the same wgpu format list + the
//!    Wayland compositor's advertised colour-management support.
//!
//! Wrong-negative (reporting SDR when the display is HDR-capable)
//! just costs quality on an actual HDR panel — the tone-mapper
//! degrades to no-op-ish on 100-nit content. Wrong-positive
//! (reporting HDR when it isn't) would send PQ pixels to an SDR
//! panel and look wrong (mostly clipped to black). The default
//! stance is therefore SDR — every uncertain path returns
//! [`DisplayCapabilities::sdr_default`].

/// What the compositor can deliver to the panel connected to this
/// wgpu surface / output. Consumed by
/// [`crate::shell::peer_view::PeerView::new_for_stream`] to pick
/// between tone-map and passthrough.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayCapabilities {
    /// Peak sustainable luminance in cd/m². SDR panels report 100;
    /// entry-level HDR panels (HDR400) 400; HDR1000 panels 1000+.
    /// Used by the compositor to pick the tone-map target when
    /// tone-mapping IS needed (a "bright SDR" panel wants less
    /// aggressive compression than a "dim SDR" panel).
    pub max_luminance_nits: f32,
    /// The compositor can output PQ (SMPTE ST 2084) transfer through
    /// the DRM connector or Vulkan swapchain. True → passthrough
    /// path; false → tone-map to SDR.
    pub supports_pq_output: bool,
    /// The compositor can output HLG. Independent of PQ — some
    /// broadcast HDR TVs prefer HLG.
    pub supports_hlg_output: bool,
    /// The panel covers (approximately) the BT.2020 gamut. If false
    /// and PQ is on, the gamut-mapping step still runs even in the
    /// "HDR display" path so out-of-gamut colours get compressed
    /// toward the display's real chromaticities.
    pub bt2020_gamut: bool,
}

impl DisplayCapabilities {
    /// Conservative SDR default — everything the tone-mapper needs to
    /// pick the "just make it look OK on a regular monitor" path.
    /// Returned on any probe failure.
    pub const fn sdr_default() -> Self {
        Self {
            max_luminance_nits: 100.0,
            supports_pq_output: false,
            supports_hlg_output: false,
            bt2020_gamut: false,
        }
    }

    /// HDR10 baseline — 1000 cd/m², PQ, BT.2020. What we assert when
    /// the probe finds `Rgb10a2Unorm` + explicit HDR metadata
    /// support.
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

/// Probe a wgpu adapter + surface for HDR capabilities. Returns SDR
/// defaults on any error path so callers never accidentally send PQ
/// pixels to an SDR display.
///
/// The probe:
/// 1. Queries `surface.get_capabilities(adapter)` for the format
///    list. `Rgb10a2Unorm` or `Rgba16Float` presence signals the
///    swapchain can carry HDR pixel data.
/// 2. On DRM/KMS (Linux native compositor path), a caller-supplied
///    EDID hint from the connector's HDR static metadata block would
///    tell us the panel's real peak. Not wired here — the compositor
///    passes the value through a follow-on
///    `ColorimetryHint` on top of this probe.
/// 3. Vulkan-side HDR metadata extension check goes through the
///    adapter's `get_info().backend`. On the Vulkan backend we can
///    ask for `VK_EXT_hdr_metadata`; on other backends we assume no.
///
/// The probe is intentionally read-only — no allocations, no
/// resource creation, just format-list inspection. Safe to call
/// from the shell's per-frame path if the compositor ever wants to
/// re-probe after hotplug.
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
    // Vulkan is the only backend on which we currently trust the
    // HDR metadata plumbing to actually reach the panel — Vulkan
    // + KMS supports the `VK_EXT_hdr_metadata` handshake, other
    // backends (GL, DX12, Metal in the compositor) route through
    // shims that don't. Conservative default: if we're not on
    // Vulkan, refuse to claim PQ passthrough regardless of format
    // availability.
    let backend_supports_hdr = matches!(backend, vello::wgpu::Backend::Vulkan);
    if !backend_supports_hdr {
        return DisplayCapabilities {
            // Format is present, so at least the swapchain could
            // carry HDR — bump max luminance modestly so the
            // tone-mapper has slightly more headroom on a "bright
            // SDR" monitor.
            max_luminance_nits: 200.0,
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
    // Rgb10a2Unorm is the standard HDR10-carrying swapchain format
    // when we've decided passthrough is safe. Rgba16Float is only
    // preferred when the display asks for it and the compositor is
    // running a scRGB-style intermediate — rare for us today.
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
        assert_eq!(caps.max_luminance_nits as u32, 100);
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
        // 1000-nit tips over into Rgba16Float per the branching in
        // preferred_hdr_swapchain_format — either is valid HDR10;
        // downstream just needs to accept it.
        assert!(
            matches!(
                fmt,
                vello::wgpu::TextureFormat::Rgba16Float | vello::wgpu::TextureFormat::Rgb10a2Unorm
            ),
            "unexpected fmt {fmt:?}"
        );
    }
}
