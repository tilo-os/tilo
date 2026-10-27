//! KMS signalling beyond the framebuffer: adaptive sync on the CRTC, and the
//! HDR signal on the connector (`max bpc`, `Colorspace`, `HDR_OUTPUT_METADATA`).
//!
//! Every write goes through the legacy `SETPROPERTY` ioctl, which atomic
//! drivers turn into a one-property atomic commit. None of these properties
//! is atomic-only, so this works whether an output's flips are atomic or
//! legacy ([`super::kms_flip`]), and an atomic flip writes none of them, so
//! it never undoes one (the same route xf86-video-amdgpu takes for
//! `VRR_ENABLED`). Legacy commits may
//! modeset, so entering or leaving HDR can blank the panel briefly.
//!
//! The decisions — which value to write, when to stop retrying — are kept in
//! plain state machines so they are tested without a DRM device.

use smithay::reexports::drm as drm_re;

use drm_re::control::{Device as ControlDevice, ResourceHandle, connector, property};
use settings_store::hardware::HdrUnavailable;

use crate::wayland::edid::HdrDisplayInfo;

/// Look up one named property on a KMS object, with its current value.
pub(super) fn find_property<D, T>(
    device: &D,
    object: T,
    name: &str,
) -> Option<(property::Handle, property::RawValue, property::Info)>
where
    D: ControlDevice,
    T: ResourceHandle,
{
    let properties = device.get_properties(object).ok()?;
    properties.iter().find_map(|(handle, value)| {
        let info = device.get_property(*handle).ok()?;
        (info.name().to_bytes() == name.as_bytes()).then_some((*handle, *value, info))
    })
}

/// What the presenter learned about one output's signalling at runtime. The
/// loop thread folds it into the Settings capability snapshot, so a feature a
/// driver refused is reported as unavailable, with the reason.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputSignalStatus {
    /// The driver refused to enable adaptive sync.
    pub adaptive_sync_rejected: bool,
    /// The output is being driven with an HDR (PQ BT.2020) signal.
    pub hdr_active: bool,
    /// HDR was requested but could not be driven, and why. The output fell
    /// back to 8-bit SDR.
    pub hdr_failure: Option<HdrUnavailable>,
    /// The output could not be lit, and why ([`super::output_link`]). The
    /// loop thread then drops it until a replug or a display settings
    /// change.
    pub link_failure: Option<settings_store::hardware::OutputUnavailable>,
}

/// Settings' view of HDR on one connector: usable, or why not.
pub(super) fn hdr_capability(
    sink_hdr: bool,
    driver_missing: Option<HdrUnavailable>,
    status: Option<&OutputSignalStatus>,
) -> (bool, Option<HdrUnavailable>) {
    if !sink_hdr {
        return (false, None);
    }
    if let Some(missing) = driver_missing {
        return (false, Some(missing));
    }
    if let Some(failure) = status.and_then(|status| status.hdr_failure) {
        return (false, Some(failure));
    }
    (true, None)
}

/// What the loop thread asks of an output that should run in HDR: the
/// sink's EDID description (for the metadata blob) and SDR white.
#[derive(Clone, Debug, PartialEq)]
pub struct HdrRequest {
    pub display: HdrDisplayInfo,
    pub sdr_white_nits: f32,
}

/// Length of `struct hdr_output_metadata` in `drm_mode.h`: a `u32` type and
/// the 26-byte `hdr_metadata_infoframe`, padded to the struct's 4-byte
/// alignment. The kernel refuses a blob of any other size.
pub(super) const HDR_OUTPUT_METADATA_BYTES: usize = 32;

/// CTA-861-G EOTF code for SMPTE ST 2084 (PQ).
const EOTF_SMPTE_ST2084: u8 = 2;

/// The `HDR_OUTPUT_METADATA` blob for a PQ signal, built from the sink's
/// EDID: its primaries and white point as the mastering display, its
/// desired peak and frame-average luminance as MaxCLL/MaxFALL. CTA-861.3
/// defines zero as "unknown", which is what a missing EDID value becomes.
///
/// Chromaticities are in units of 0.00002; the primaries go red, green,
/// blue, the order the kernel copies into the infoframe.
pub(super) fn hdr_output_metadata(info: &HdrDisplayInfo) -> [u8; HDR_OUTPUT_METADATA_BYTES] {
    let chroma = |micro: u32| -> u16 {
        u16::try_from(micro.checked_div(20).unwrap_or(0).min(50_000)).unwrap_or(50_000)
    };
    let mut fields: Vec<u8> = Vec::with_capacity(HDR_OUTPUT_METADATA_BYTES);
    // `metadata_type`: HDMI_STATIC_METADATA_TYPE1.
    fields.extend_from_slice(&0_u32.to_le_bytes());
    fields.push(EOTF_SMPTE_ST2084);
    // Infoframe `metadata_type`: static metadata type 1.
    fields.push(0);
    for (x, y) in info.display_primaries.iter().chain([&info.white_point]) {
        fields.extend_from_slice(&chroma(*x).to_le_bytes());
        fields.extend_from_slice(&chroma(*y).to_le_bytes());
    }
    // Mastering luminance: max in cd/m², min in 0.0001 cd/m².
    fields.extend_from_slice(&info.max_luminance_cd_m2.to_le_bytes());
    let min = u16::try_from(info.min_luminance_micro_cd_m2).unwrap_or(u16::MAX);
    fields.extend_from_slice(&min.to_le_bytes());
    // MaxCLL and MaxFALL.
    fields.extend_from_slice(&info.max_luminance_cd_m2.to_le_bytes());
    fields.extend_from_slice(&info.max_frame_average_luminance_cd_m2.to_le_bytes());
    let mut blob = [0_u8; HDR_OUTPUT_METADATA_BYTES];
    for (slot, byte) in blob.iter_mut().zip(fields) {
        *slot = byte;
    }
    blob
}

/// The `Colorspace` enum values for SDR (`Default`) and HDR (`BT2020_RGB`).
pub(super) fn colorspace_values<'a>(
    values: impl IntoIterator<Item = (u64, &'a [u8])>,
) -> Option<(u64, u64)> {
    let mut default = None;
    let mut bt2020 = None;
    for (value, name) in values {
        match name {
            b"Default" => default = Some(value),
            b"BT2020_RGB" => bt2020 = Some(value),
            _ => {}
        }
    }
    Some((default?, bt2020?))
}

/// The link depth HDR needs: ten bits a channel, or more when the connector
/// already runs deeper.
pub(super) fn hdr_max_bpc(current: u64, range_max: u64) -> Option<u64> {
    (range_max >= 10).then(|| current.max(10).min(range_max))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MaxBpc {
    handle: property::Handle,
    /// Value found at bring-up, restored when HDR ends.
    original: u64,
    range_max: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Colorspace {
    handle: property::Handle,
    default: u64,
    bt2020_rgb: u64,
}

/// The connector properties HDR signalling needs, and whether the signal is
/// on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HdrSignal {
    max_bpc: Option<MaxBpc>,
    colorspace: Option<Colorspace>,
    metadata: Option<property::Handle>,
    /// Blob attached to `HDR_OUTPUT_METADATA` while the signal is on.
    blob: Option<u64>,
    /// Whether the signal is on; `None` after a session resume, when
    /// another DRM master may have left the properties either way.
    enabled: Option<bool>,
}

impl HdrSignal {
    pub(super) fn probe<D: ControlDevice>(device: &D, connector: connector::Handle) -> Self {
        let max_bpc = find_property(device, connector, "max bpc").and_then(
            |(handle, value, info)| match info.value_type() {
                property::ValueType::UnsignedRange(_, range_max) => Some(MaxBpc {
                    handle,
                    original: value,
                    range_max,
                }),
                _ => None,
            },
        );
        let colorspace =
            find_property(device, connector, "Colorspace").and_then(|(handle, _, info)| match info
                .value_type()
            {
                property::ValueType::Enum(values) => {
                    let (raw, names) = values.values();
                    colorspace_values(
                        raw.iter()
                            .copied()
                            .zip(names.iter().map(|name| name.name().to_bytes())),
                    )
                    .map(|(default, bt2020_rgb)| Colorspace {
                        handle,
                        default,
                        bt2020_rgb,
                    })
                }
                _ => None,
            });
        let metadata =
            find_property(device, connector, "HDR_OUTPUT_METADATA").map(|(handle, _, _)| handle);
        Self {
            max_bpc,
            colorspace,
            metadata,
            blob: None,
            // A fresh session starts from the driver's SDR defaults.
            enabled: Some(false),
        }
    }

    /// The first missing driver piece, or `None` when HDR can be signalled.
    pub(super) fn missing(&self) -> Option<HdrUnavailable> {
        let deep_enough = self
            .max_bpc
            .is_some_and(|bpc| hdr_max_bpc(bpc.original, bpc.range_max).is_some());
        (self.metadata.is_none() || self.colorspace.is_none() || !deep_enough)
            .then_some(HdrUnavailable::DriverUnsupported)
    }

    pub(super) fn enabled(&self) -> bool {
        self.enabled == Some(true)
    }

    /// The kernel may not hold the `on` state: write it.
    pub(super) fn needs(&self, on: bool) -> bool {
        self.enabled != Some(on)
    }

    /// Turn the HDR signal on: link depth, colorimetry, then the metadata
    /// infoframe. A refused step undoes the ones before it.
    pub(super) fn enable<D: ControlDevice>(
        &mut self,
        device: &D,
        connector: connector::Handle,
        display: &HdrDisplayInfo,
    ) -> Result<(), String> {
        let (Some(bpc), Some(colorspace), Some(metadata)) =
            (self.max_bpc, self.colorspace, self.metadata)
        else {
            return Err("connector lacks an HDR property".to_owned());
        };
        let Some(depth) = hdr_max_bpc(bpc.original, bpc.range_max) else {
            return Err(format!("max bpc tops out at {}", bpc.range_max));
        };
        let blob = match device.create_property_blob(&hdr_output_metadata(display)) {
            Ok(property::Value::Blob(id)) => id,
            Ok(other) => return Err(format!("unexpected blob value {other:?}")),
            Err(error) => return Err(format!("create HDR_OUTPUT_METADATA blob: {error}")),
        };
        if let Some(stale) = self.blob.replace(blob) {
            let _ = device.destroy_property_blob(stale);
        }
        // Mark enabled first so a partial failure is undone by `disable`.
        self.enabled = Some(true);
        let steps = [
            ("max bpc", bpc.handle, depth),
            ("Colorspace", colorspace.handle, colorspace.bt2020_rgb),
            ("HDR_OUTPUT_METADATA", metadata, blob),
        ];
        for (name, handle, value) in steps {
            if let Err(error) = device.set_property(connector, handle, value) {
                let _ = self.disable(device, connector);
                return Err(format!("{name}: {error}"));
            }
        }
        Ok(())
    }

    /// Back to SDR signalling: no metadata, default colorimetry, the link
    /// depth found at bring-up. Best effort; the first refusal is returned
    /// after every step was tried.
    pub(super) fn disable<D: ControlDevice>(
        &mut self,
        device: &D,
        connector: connector::Handle,
    ) -> Result<(), String> {
        if self.enabled == Some(false) {
            return Ok(());
        }
        self.enabled = Some(false);
        let mut first_error = None;
        let mut write = |name: &str, handle: property::Handle, value: u64| {
            if let Err(error) = device.set_property(connector, handle, value) {
                first_error.get_or_insert_with(|| format!("{name}: {error}"));
            }
        };
        if let Some(metadata) = self.metadata {
            write("HDR_OUTPUT_METADATA", metadata, 0);
        }
        if let Some(colorspace) = self.colorspace {
            write("Colorspace", colorspace.handle, colorspace.default);
        }
        if let Some(bpc) = self.max_bpc {
            write("max bpc", bpc.handle, bpc.original);
        }
        if let Some(blob) = self.blob.take() {
            let _ = device.destroy_property_blob(blob);
        }
        first_error.map_or(Ok(()), Err)
    }

    /// After a session resume the kernel may hold another master's values:
    /// forget the signal so it is written again.
    pub(super) fn forget(&mut self) {
        self.enabled = None;
    }
}

/// Whether any CRTC of the device exposes `VRR_ENABLED`. Capability is
/// published before a connector is bound to a CRTC, and drivers expose the
/// property on every CRTC or none.
pub(super) fn device_has_adaptive_sync<D: ControlDevice>(device: &D) -> bool {
    device.resource_handles().is_ok_and(|resources| {
        resources
            .crtcs()
            .iter()
            .any(|&crtc| find_property(device, crtc, "VRR_ENABLED").is_some())
    })
}

/// Settings' view of adaptive sync on one connector: usable, or why not.
pub(super) fn adaptive_sync_capability(
    sink_capable: bool,
    driver_supports: bool,
    status: Option<&OutputSignalStatus>,
) -> (
    bool,
    Option<settings_store::hardware::AdaptiveSyncUnavailable>,
) {
    use settings_store::hardware::AdaptiveSyncUnavailable;
    if !sink_capable {
        return (false, None);
    }
    if !driver_supports {
        return (false, Some(AdaptiveSyncUnavailable::DriverUnsupported));
    }
    if status.is_some_and(|status| status.adaptive_sync_rejected) {
        return (false, Some(AdaptiveSyncUnavailable::DriverRejected));
    }
    (true, None)
}

/// Adaptive sync (VRR) on one CRTC: what the driver offers, what the
/// compositor wants, and what was last written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AdaptiveSync {
    /// The CRTC's `VRR_ENABLED` property, when the driver exposes one.
    property: Option<property::Handle>,
    /// The connector's `vrr_capable` property read 1: the sink accepts it.
    sink_capable: bool,
    wanted: bool,
    /// Value the kernel holds, as far as this process knows. `None` after a
    /// session resume, when another DRM master may have changed it.
    applied: Option<bool>,
    /// The driver refused a write. Latched until the next session resume so
    /// a broken driver is not asked again on every page flip.
    rejected: bool,
}

impl AdaptiveSync {
    pub(super) fn probe<D: ControlDevice>(
        device: &D,
        crtc: drm_re::control::crtc::Handle,
        connector: drm_re::control::connector::Handle,
    ) -> Self {
        let property = find_property(device, crtc, "VRR_ENABLED").map(|(handle, _, _)| handle);
        let sink_capable =
            find_property(device, connector, "vrr_capable").is_some_and(|(_, value, _)| value == 1);
        Self::new(property, sink_capable)
    }

    pub(super) fn new(property: Option<property::Handle>, sink_capable: bool) -> Self {
        Self {
            property,
            sink_capable,
            wanted: false,
            // The property resets to off on a fresh modeset; writing "off"
            // again would be a pointless commit.
            applied: Some(false),
            rejected: false,
        }
    }

    pub(super) fn property(&self) -> Option<property::Handle> {
        self.property
    }

    /// Both ends can do it: the sink says so and the driver has the knob.
    pub(super) fn supported(&self) -> bool {
        self.property.is_some() && self.sink_capable
    }

    pub(super) fn rejected(&self) -> bool {
        self.rejected
    }

    /// Adaptive sync is on in the kernel right now.
    pub(super) fn active(&self) -> bool {
        self.applied == Some(true)
    }

    pub(super) fn want(&mut self, on: bool) {
        self.wanted = on;
    }

    /// After a session resume the kernel state is unknown and a previous
    /// rejection may have been a transient master-less failure.
    pub(super) fn forget_applied(&mut self) {
        self.applied = None;
        self.rejected = false;
    }

    /// The value to write before the next flip, if the kernel differs from
    /// what is wanted.
    pub(super) fn pending_write(&self) -> Option<bool> {
        if self.property.is_none() || self.rejected {
            return None;
        }
        let target = self.wanted && self.sink_capable;
        (self.applied != Some(target)).then_some(target)
    }

    pub(super) fn record_write(&mut self, value: bool, succeeded: bool) {
        if succeeded {
            self.applied = Some(value);
        } else {
            // A refused write leaves the kernel value where it was. Turning
            // it on is what fails in practice, which leaves it off.
            self.rejected = true;
            if value {
                self.applied = Some(false);
            }
        }
    }
}

/// Software dimming in the CRTC's colour pipeline: the `GAMMA_LUT` the
/// display engine applies to every plane after composition, so a dimmed
/// display stays dimmed while a fullscreen client's buffer is scanned out
/// directly ([`crate::shell::software_dim`]). Like adaptive sync, it is
/// written between flips through the legacy `SETPROPERTY` ioctl, and the
/// decision of what to write is kept here, testable without a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CrtcGamma {
    /// The CRTC's `GAMMA_LUT` property, when the driver exposes one.
    property: Option<property::Handle>,
    /// Its `GAMMA_LUT_SIZE`: how many entries the LUT takes.
    size: usize,
    /// The light to let through (`f32` bits) and whether the signal is PQ.
    wanted: (u32, bool),
    /// What the kernel holds, as far as this process knows. `None` after a
    /// session resume, when another DRM master may have changed it.
    applied: Option<(u32, bool)>,
    /// The driver refused a LUT. Latched until the next session resume; the
    /// final pass dims meanwhile.
    rejected: bool,
}

/// The undimmed LUT: no LUT at all, whatever the signal.
const UNDIMMED: (u32, bool) = (0x3F80_0000, false);

impl CrtcGamma {
    pub(super) fn probe<D: ControlDevice>(device: &D, crtc: drm_re::control::crtc::Handle) -> Self {
        let property = find_property(device, crtc, "GAMMA_LUT").map(|(handle, _, _)| handle);
        let size = find_property(device, crtc, "GAMMA_LUT_SIZE")
            .map_or(0, |(_, value, _)| usize::try_from(value).unwrap_or(0));
        Self::new(property, size)
    }

    pub(super) fn new(property: Option<property::Handle>, size: usize) -> Self {
        Self {
            property,
            size,
            wanted: UNDIMMED,
            // A fresh session starts without a LUT, so undimmed costs no
            // commit.
            applied: Some(UNDIMMED),
            rejected: false,
        }
    }

    /// The CRTC can dim: it has a LUT of at least two entries, and the
    /// driver has not refused one.
    pub(super) fn carries(&self) -> bool {
        self.property.is_some() && self.size >= 2 && !self.rejected
    }

    /// Let `light` through (`1.0`: undimmed) on a signal that is PQ when
    /// `pq`, else sRGB.
    pub(super) fn want(&mut self, light: f32, pq: bool) {
        self.wanted = if crate::shell::software_dim::dims(light) {
            (light.to_bits(), pq)
        } else {
            UNDIMMED
        };
    }

    /// The signal changed between SDR and PQ: a dimmed LUT follows it.
    pub(super) fn set_pq(&mut self, pq: bool) {
        if self.wanted != UNDIMMED {
            self.wanted.1 = pq;
        }
    }

    /// The light and transfer to write before the next flip, if the kernel
    /// differs from what is wanted.
    pub(super) fn pending_write(&self) -> Option<(f32, bool)> {
        (self.carries() && self.applied != Some(self.wanted))
            .then(|| (f32::from_bits(self.wanted.0), self.wanted.1))
    }

    /// Write what is pending to `crtc`'s LUT: no LUT when undimmed, else the
    /// dimming curve at the LUT's full size and precision.
    pub(super) fn write<D: ControlDevice>(
        &mut self,
        device: &D,
        crtc: drm_re::control::crtc::Handle,
    ) -> Result<(), String> {
        use crate::shell::software_dim::{Transfer, gamma_lut, gamma_lut_blob};
        let (Some((light, pq)), Some(property)) = (self.pending_write(), self.property) else {
            return Ok(());
        };
        let blob = if self.wanted == UNDIMMED {
            None
        } else {
            let transfer = if pq { Transfer::Pq } else { Transfer::Srgb };
            let bytes = gamma_lut_blob(&gamma_lut(self.size, light, transfer));
            match create_blob(device, &bytes) {
                Ok(blob) => Some(blob),
                Err(error) => {
                    self.rejected = true;
                    return Err(format!("create GAMMA_LUT blob: {error}"));
                }
            }
        };
        let result = device.set_property(crtc, property, blob.map_or(0, u64::from));
        // The CRTC holds its own reference to a LUT it took.
        if let Some(blob) = blob {
            let _ = device.destroy_property_blob(u64::from(blob));
        }
        match result {
            Ok(()) => {
                self.applied = Some(self.wanted);
                Ok(())
            }
            // A commit still in flight: the write stays pending and goes
            // out before the next flip.
            Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {
                Err(format!("GAMMA_LUT busy: {error}"))
            }
            Err(error) => {
                self.rejected = true;
                Err(format!("GAMMA_LUT: {error}"))
            }
        }
    }

    /// The LUT as this process knows it, for the flight record: whether the
    /// CRTC carries one, the light wanted and what the kernel holds
    /// (`none` for no LUT, `unknown` after a resume).
    pub(super) fn describe(&self) -> String {
        let light = |(bits, pq): (u32, bool)| {
            if (bits, pq) == UNDIMMED {
                "none".to_owned()
            } else {
                format!("{:.3}{}", f32::from_bits(bits), if pq { "-pq" } else { "" })
            }
        };
        format!(
            "{}wanted={} applied={}",
            if self.carries() { "" } else { "uncarried," },
            light(self.wanted),
            self.applied.map_or_else(|| "unknown".to_owned(), light),
        )
    }

    /// After a session resume the kernel state is unknown and a previous
    /// refusal may have been a transient master-less failure.
    pub(super) fn forget_applied(&mut self) {
        self.applied = None;
        self.rejected = false;
    }
}

/// Create a property blob holding `bytes`: `DRM_IOCTL_MODE_CREATEPROPBLOB`.
/// The `drm` crate's own wrapper takes a sized value; a LUT's length is the
/// CRTC's.
fn create_blob<D: ControlDevice>(device: &D, bytes: &[u8]) -> std::io::Result<u32> {
    use std::os::fd::AsRawFd as _;
    /// `DRM_IOCTL_MODE_CREATEPROPBLOB`: `_IOWR('d', 0xBD, struct drm_mode_create_blob)`.
    const CREATE_BLOB: u32 = 0xC010_64BD;
    #[repr(C)]
    struct CreateBlob {
        data: u64,
        length: u32,
        blob_id: u32,
    }
    let invalid = || std::io::Error::from(std::io::ErrorKind::InvalidInput);
    let mut request = CreateBlob {
        data: u64::try_from(bytes.as_ptr().addr()).map_err(|_| invalid())?,
        length: u32::try_from(bytes.len()).map_err(|_| invalid())?,
        blob_id: 0,
    };
    // SAFETY: the request is the kernel's `drm_mode_create_blob` layout and
    // lives for the call; the kernel copies `length` bytes from `data`,
    // which `bytes` holds for the call.
    let result = unsafe {
        libc::ioctl(
            device.as_fd().as_raw_fd(),
            CREATE_BLOB.into(),
            &raw mut request,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(request.blob_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> property::Handle {
        property::Handle::from(std::num::NonZeroU32::new(7).expect("non-zero"))
    }

    fn hdr_sink() -> HdrDisplayInfo {
        HdrDisplayInfo {
            // BT.2020 red, green, blue and D65 in micro-CIE.
            display_primaries: [(708_000, 292_000), (170_000, 797_000), (131_000, 46_000)],
            white_point: (312_700, 329_000),
            max_luminance_cd_m2: 1015,
            max_frame_average_luminance_cd_m2: 403,
            min_luminance_micro_cd_m2: 500,
            ..HdrDisplayInfo::default_sdr()
        }
    }

    fn u16_at(blob: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([blob[offset], blob[offset.saturating_add(1)]])
    }

    /// Dim, blank, undim on the wake's input, wake: the dimmed LUT the CRTC
    /// held through the blank is replaced by no LUT before anything shows,
    /// and no dimming LUT is ever black. (The idle dim of a panel with a
    /// backlight never touches the LUT; this is the software-dimmed one.)
    #[test]
    fn a_wake_after_a_dimmed_blank_shows_no_lut_and_no_lut_is_ever_black() {
        use crate::shell::software_dim::{Transfer, gamma_lut};
        let mut gamma = CrtcGamma::new(Some(handle()), 1024);
        // Dimmed before the blank, and written while the CRTC scanned out.
        gamma.want(0.3, false);
        gamma.applied = Some(gamma.wanted);
        assert_eq!(gamma.describe(), "wanted=0.300 applied=0.300");
        // Blanked: nothing is written to a CRTC that is off. The wake's
        // input restores the brightness before the panel lights.
        gamma.want(1.0, false);
        assert_eq!(
            gamma.pending_write(),
            Some((1.0, false)),
            "the link-up write (on_link_up) takes the dimmed LUT off"
        );
        gamma.applied = Some(gamma.wanted);
        assert_eq!(gamma.pending_write(), None);
        assert_eq!(gamma.describe(), "wanted=none applied=none");
        // A resume makes the kernel's LUT unknown: it is written again.
        gamma.forget_applied();
        assert_eq!(gamma.describe(), "wanted=none applied=unknown");
        assert_eq!(gamma.pending_write(), Some((1.0, false)));
        let darkest = gamma_lut(
            1024,
            os_actions::brightness::SOFTWARE_DIM_FLOOR,
            Transfer::Srgb,
        );
        let top = darkest.last().copied().expect("a LUT");
        assert!(top > 0, "{top}");
    }

    #[test]
    fn the_crtc_lut_is_written_only_when_dimming_changes() {
        let mut gamma = CrtcGamma::new(Some(handle()), 1024);
        assert!(gamma.carries());
        assert_eq!(gamma.pending_write(), None, "undimmed costs no commit");
        gamma.want(0.5, false);
        assert_eq!(gamma.pending_write(), Some((0.5, false)));
        gamma.applied = Some(gamma.wanted);
        assert_eq!(gamma.pending_write(), None);
        gamma.set_pq(true);
        assert_eq!(
            gamma.pending_write(),
            Some((0.5, true)),
            "a PQ signal gets a PQ curve"
        );
        gamma.applied = Some(gamma.wanted);
        gamma.want(1.0, true);
        assert_eq!(gamma.pending_write(), Some((1.0, false)), "back to no LUT");
        gamma.forget_applied();
        gamma.applied = None;
        assert!(gamma.pending_write().is_some(), "a resume writes again");

        let mut none = CrtcGamma::new(None, 0);
        none.want(0.5, false);
        assert!(!none.carries(), "no LUT: the final pass dims");
        assert_eq!(none.pending_write(), None);
        let mut refused = CrtcGamma::new(Some(handle()), 256);
        refused.rejected = true;
        refused.want(0.5, false);
        assert!(!refused.carries());
    }

    #[test]
    fn hdr_metadata_blob_matches_the_kernel_struct_layout() {
        let blob = hdr_output_metadata(&hdr_sink());
        assert_eq!(blob.len(), 32, "sizeof(struct hdr_output_metadata)");
        assert_eq!(&blob[0..4], &[0, 0, 0, 0], "HDMI_STATIC_METADATA_TYPE1");
        assert_eq!(blob[4], 2, "EOTF SMPTE ST 2084");
        assert_eq!(blob[5], 0, "static metadata descriptor type 1");
        // Primaries in 0.00002 units: 0.708 → 35400, red first.
        assert_eq!(u16_at(&blob, 6), 35_400);
        assert_eq!(u16_at(&blob, 8), 14_600);
        assert_eq!(u16_at(&blob, 10), 8_500, "green x");
        assert_eq!(u16_at(&blob, 14), 6_550, "blue x");
        assert_eq!(u16_at(&blob, 18), 15_635, "white x 0.3127");
        assert_eq!(u16_at(&blob, 20), 16_450, "white y 0.329");
        assert_eq!(u16_at(&blob, 22), 1015, "mastering max, cd/m²");
        assert_eq!(u16_at(&blob, 24), 500, "mastering min, 0.0001 cd/m²");
        assert_eq!(u16_at(&blob, 26), 1015, "MaxCLL");
        assert_eq!(u16_at(&blob, 28), 403, "MaxFALL");
        assert_eq!(&blob[30..32], &[0, 0], "struct padding");

        // Unknown EDID luminance stays zero, CTA-861.3's "unknown".
        let unknown = HdrDisplayInfo {
            max_luminance_cd_m2: 0,
            max_frame_average_luminance_cd_m2: 0,
            min_luminance_micro_cd_m2: 0,
            ..hdr_sink()
        };
        let blob = hdr_output_metadata(&unknown);
        assert_eq!(u16_at(&blob, 22), 0);
        assert_eq!(u16_at(&blob, 26), 0);
        // Out-of-range chromaticity saturates at 1.0 (50000 units).
        let wild = HdrDisplayInfo {
            white_point: (2_000_000, 2_000_000),
            ..hdr_sink()
        };
        assert_eq!(u16_at(&hdr_output_metadata(&wild), 18), 50_000);
    }

    #[test]
    fn hdr_driver_properties_are_read_and_the_capability_explained() {
        // Phase `colorspace_needs_both_default_and_bt2020_rgb`.
        let named = [
            (0, b"Default".as_slice()),
            (9, b"BT2020_RGB".as_slice()),
            (10, b"BT2020_YCC".as_slice()),
        ];
        assert_eq!(colorspace_values(named), Some((0, 9)));
        assert_eq!(colorspace_values([(0, b"Default".as_slice())]), None);
        assert_eq!(hdr_max_bpc(8, 16), Some(10));
        assert_eq!(hdr_max_bpc(12, 16), Some(12));
        assert_eq!(hdr_max_bpc(8, 10), Some(10));
        assert_eq!(hdr_max_bpc(8, 8), None);
        assert_eq!(hdr_capability(false, None, None), (false, None));
        assert_eq!(
            hdr_capability(true, Some(HdrUnavailable::DriverUnsupported), None),
            (false, Some(HdrUnavailable::DriverUnsupported))
        );
        let failed = OutputSignalStatus {
            hdr_failure: Some(HdrUnavailable::DriverRejected),
            ..OutputSignalStatus::default()
        };
        assert_eq!(
            hdr_capability(true, None, Some(&failed)),
            (false, Some(HdrUnavailable::DriverRejected))
        );
        assert_eq!(hdr_capability(true, None, None), (true, None));
        let bare = HdrSignal {
            max_bpc: None,
            colorspace: None,
            metadata: None,
            blob: None,
            enabled: Some(false),
        };
        assert!(!bare.enabled());
        assert!(bare.needs(true) && !bare.needs(false));
        let mut resumed = bare.clone();
        resumed.forget();
        assert!(
            resumed.needs(true) && resumed.needs(false),
            "unknown after resume"
        );
        assert_eq!(bare.missing(), Some(HdrUnavailable::DriverUnsupported));
        let complete = HdrSignal {
            max_bpc: Some(MaxBpc {
                handle: handle(),
                original: 8,
                range_max: 16,
            }),
            colorspace: Some(Colorspace {
                handle: handle(),
                default: 0,
                bt2020_rgb: 9,
            }),
            metadata: Some(handle()),
            blob: None,
            enabled: Some(false),
        };
        assert_eq!(complete.missing(), None);
        let shallow = HdrSignal {
            max_bpc: Some(MaxBpc {
                handle: handle(),
                original: 8,
                range_max: 8,
            }),
            ..complete
        };
        assert_eq!(shallow.missing(), Some(HdrUnavailable::DriverUnsupported));
    }

    #[test]
    fn adaptive_sync_capability_reports_the_first_missing_piece() {
        use settings_store::hardware::AdaptiveSyncUnavailable;
        assert_eq!(adaptive_sync_capability(false, true, None), (false, None));
        assert_eq!(
            adaptive_sync_capability(true, false, None),
            (false, Some(AdaptiveSyncUnavailable::DriverUnsupported))
        );
        let refused = OutputSignalStatus {
            adaptive_sync_rejected: true,
            ..OutputSignalStatus::default()
        };
        assert_eq!(
            adaptive_sync_capability(true, true, Some(&refused)),
            (false, Some(AdaptiveSyncUnavailable::DriverRejected))
        );
        assert_eq!(
            adaptive_sync_capability(true, true, Some(&OutputSignalStatus::default())),
            (true, None)
        );
    }

    #[test]
    fn adaptive_sync_writes_only_changes_and_stops_after_a_refusal() {
        {
            let mut sync = AdaptiveSync::new(Some(handle()), true);
            assert!(sync.supported());
            assert_eq!(sync.pending_write(), None, "fresh modeset is already off");
            sync.want(true);
            assert_eq!(sync.pending_write(), Some(true));
            sync.record_write(true, true);
            assert!(sync.active());
            assert_eq!(sync.pending_write(), None);
            sync.want(false);
            assert_eq!(sync.pending_write(), Some(false));
            sync.record_write(false, true);
            assert!(!sync.active());
        }
        {
            let mut sink_cannot = AdaptiveSync::new(Some(handle()), false);
            sink_cannot.want(true);
            assert!(!sink_cannot.supported());
            assert_eq!(sink_cannot.pending_write(), None);
            let mut no_property = AdaptiveSync::new(None, true);
            no_property.want(true);
            assert!(!no_property.supported());
            assert_eq!(no_property.pending_write(), None);
        }
        {
            let mut sync = AdaptiveSync::new(Some(handle()), true);
            sync.want(true);
            sync.record_write(true, false);
            assert!(sync.rejected());
            assert!(!sync.active());
            assert_eq!(sync.pending_write(), None, "no retry on every flip");
            sync.forget_applied();
            assert!(!sync.rejected());
            assert_eq!(sync.pending_write(), Some(true), "resume tries once more");
        }
        {
            let mut sync = AdaptiveSync::new(Some(handle()), true);
            sync.forget_applied();
            assert_eq!(sync.pending_write(), Some(false));
        }
    }
}
