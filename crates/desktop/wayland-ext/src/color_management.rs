//! `color-management-v1` staging protocol — server-side dispatch.
//!
//! smithay 0.7 ships no ready-made handler for this freedesktop staging
//! protocol, so the whole thing is implemented manually with
//! `wayland-server` `Dispatch` / `GlobalDispatch`, registered via
//! `delegate_dispatch!` / `delegate_global_dispatch!` at the bottom.
//!
//! Advertises the *parametric* creator path only — primaries, transfer
//! function, optional mastering-display/luminance hints, then `create`
//! for an immutable `wp_image_description_v1` attached via
//! `wp_color_management_surface_v1`, plus the Windows-scRGB description
//! (linear light, sRGB primaries, `1.0` at SDR white) that DirectX-style
//! games render into. A creator's requests are checked by
//! [`tilo_color_params`], the rules the app keeper checks its apps by too.
//! Deliberately unimplemented: ICC creators
//! (`unsupported_feature` error), and `get_information` on client-created descriptions (`no_information` —
//! they're surface-attach-only, not for round-tripping). Descriptions minted
//! by output and surface-feedback requests are queryable and return the exact
//! parametric values the compositor advertised.
//!
//! Per-surface attach is double-buffered per spec (pending on
//! `set_image_description`, applied on `wl_surface.commit`); the
//! pending and current values live in `SurfaceData::data_map` under
//! [`ColorManagementSurfaceData`], flipped by the commit hook in
//! the compositor state.

#[cfg(all(target_os = "linux", feature = "test-support"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    backend::{ClientId, GlobalId},
    protocol::{wl_output::WlOutput, wl_surface::WlSurface},
};

#[cfg(target_os = "linux")]
use smithay::wayland::compositor::{self as wl_compositor};

#[cfg(target_os = "linux")]
use wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1::{self, WpColorManagementOutputV1},
    wp_color_management_surface_feedback_v1::{self, WpColorManagementSurfaceFeedbackV1},
    wp_color_management_surface_v1::{self, WpColorManagementSurfaceV1},
    wp_color_manager_v1::{self, Primaries, TransferFunction, WpColorManagerV1},
    wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
    wp_image_description_creator_params_v1::{self, WpImageDescriptionCreatorParamsV1},
    wp_image_description_info_v1::WpImageDescriptionInfoV1,
    wp_image_description_v1::{self, Cause as ImageCause, WpImageDescriptionV1},
};

use neighbour_wire::colorimetry::{
    ColorPrimaries, ColorRange, Colorimetry, MatrixCoefficients, TransferCharacteristics,
};
use neighbour_wire::hdr_metadata::{ContentLightLevel, Hdr10Metadata, MasteringDisplayColorVolume};
use tilo_color_params::{Params, Primaries as ParamPrimaries, Support, Transfer, feature};

/// The maximum protocol version we support. Bump on staging churn.
///
/// Keep event emission below aligned with this value: `ready2` was added in
/// version 2 and sending it on a version-1 image-description object disconnects
/// clients with "has no event 2".
#[cfg(target_os = "linux")]
const MAX_VERSION: u32 = 1;

/// Advertised capability set for `wp_color_manager_v1`.
#[derive(Debug, Clone)]
pub struct ColorManagerState {
    /// What clients are told is supported, as `wp_color_manager_v1` enum values (not CICP).
    pub support: Support,
    /// Matrix hint used by the compositor for parametric image
    /// descriptions. `wp_color_manager_v1` does NOT itself have a
    /// "matrixes" event — matrix negotiation lives in the sibling
    /// `color-representation-v1` protocol. We keep the field around
    /// so the flatpak-share encoder can honour the compositor's
    /// preferred matrix on the wire; we don't advertise it through
    /// `color-management-v1`. See `wp_color_manager_v1.xml`.
    pub supported_matrixes: Vec<u32>,
    /// The global's id, held so the caller can revoke it when
    /// shutting down (currently unused, but the id is cheap to keep
    /// and it saves a `create_global` return dance).
    #[cfg(target_os = "linux")]
    pub global: Option<GlobalId>,
    /// Every live `wp_color_management_output_v1`, so a change of an
    /// output's signal reaches clients with `image_description_changed`.
    #[cfg(target_os = "linux")]
    outputs: Arc<Mutex<Vec<(WpColorManagementOutputV1, ColorManagementOutputData)>>>,
    /// How many output objects clients have created, so the compositor can
    /// skip [`Self::update_outputs`] while neither the outputs nor the
    /// objects describing them changed.
    #[cfg(target_os = "linux")]
    output_objects: Arc<std::sync::atomic::AtomicU64>,
    /// Every live surface feedback object, told when the preferred
    /// description changes.
    #[cfg(target_os = "linux")]
    feedbacks: Arc<Mutex<Vec<WpColorManagementSurfaceFeedbackV1>>>,
    /// The description surfaces are asked to render in.
    preferred: Arc<Mutex<OutputDescription>>,
    /// Whether a description a client makes has its `ready` held for a test to let go.
    #[cfg(all(target_os = "linux", feature = "test-support"))]
    holding: Arc<AtomicBool>,
    /// The descriptions held back, with the identity each will be told.
    #[cfg(all(target_os = "linux", feature = "test-support"))]
    held: Arc<Mutex<Vec<(WpImageDescriptionV1, u32)>>>,
}

#[cfg(all(target_os = "linux", feature = "test-support"))]
impl ColorManagerState {
    /// Hold back the `ready` of every image description a client makes from now on.
    pub fn hold_descriptions(&self) {
        self.holding.store(true, Ordering::Relaxed);
    }

    /// Tell every held description it is ready, and stop holding new ones.
    pub fn release_descriptions(&self) {
        self.holding.store(false, Ordering::Relaxed);
        let held = self
            .held
            .lock()
            .map(|mut held| std::mem::take(&mut *held))
            .unwrap_or_default();
        for (obj, identity_lo) in held {
            if obj.is_alive() {
                let () = obj.ready(identity_lo);
            }
        }
    }

    /// Tell every held description it failed, and stop holding new ones.
    pub fn fail_descriptions(&self, message: &str) {
        self.holding.store(false, Ordering::Relaxed);
        let held = self
            .held
            .lock()
            .map(|mut held| std::mem::take(&mut *held))
            .unwrap_or_default();
        for (obj, _) in held {
            if obj.is_alive() {
                let () = obj.failed(ImageCause::Unsupported, message.to_owned());
            }
        }
    }
}

impl Default for ColorManagerState {
    fn default() -> Self {
        Self {
            support: Support {
                // Perceptual (=0), Relative colorimetric (=1),
                // Media-relative w/ BPC (=4). Saturation (=2) + Absolute
                // (=3) aren't yet plumbed into the wgpu shader.
                intents: vec![0, 1, 4],
                // Parametric (=1) — must advertise or clients can't build
                // any parametric image description.
                // set_primaries (=2) — allows explicit CIE-1931 xy chroma.
                // set_tf_power (=3) — allows a pure power-curve transfer;
                //   fed straight into the wgpu shader.
                // set_luminances (=4) — primary volume min/max/reference.
                // set_mastering_display_primaries (=5) — SMPTE ST 2086
                //   mastering-display metadata for HDR pipelines.
                // windows_scrgb (=7) — the ready-made scRGB description.
                features: vec![1, 2, 3, 4, 5, 7],
                // Named input spaces the compositor's color pipeline accepts.
                // These are renderer capabilities, not a claim that every sink
                // physically covers each gamut: the output description below is
                // populated independently from that output's EDID/profile.
                primaries: vec![1, 6, 9],
                // BT.1886 (=1), gamma22 (=2), ext_linear (=5), ext_srgb
                // (=10), ST 2084 PQ (=11), HLG (=13). The v1 spec's `srgb`
                // (=9) is deprecated since v2; we don't advertise it — clients
                // get gamma22 or the sibling `ext_srgb` instead. `ext_linear` is
                // scRGB-style linear light, composited at full precision like PQ
                // and HLG.
                transfers: vec![1, 2, 5, 10, 11, 13],
            },
            // Compositor's preferred matrix set: Identity (=0),
            // BT.709 (=1), BT.2020-non-constant (=9). These are the
            // CICP MatrixCoefficients numbers, not `color-management-v1`
            // enum values.
            supported_matrixes: vec![0, 1, 9],
            #[cfg(target_os = "linux")]
            global: None,
            #[cfg(target_os = "linux")]
            outputs: Arc::default(),
            #[cfg(target_os = "linux")]
            output_objects: Arc::default(),
            #[cfg(target_os = "linux")]
            feedbacks: Arc::default(),
            preferred: Arc::new(Mutex::new(OutputDescription::sdr())),
            #[cfg(all(target_os = "linux", feature = "test-support"))]
            holding: Arc::new(AtomicBool::new(false)),
            #[cfg(all(target_os = "linux", feature = "test-support"))]
            held: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl ColorManagerState {
    /// Register the `wp_color_manager_v1` global on `display` and return
    /// the initialized state. Callers stash the returned value in the
    /// process-wide compositor state so the delegate dispatch impls
    /// route back to the right advertised-capability list.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<WpColorManagerV1, ()> + 'static,
    {
        let mut state = Self::default();
        let global = display.create_global::<D, WpColorManagerV1, _>(MAX_VERSION, ());
        state.global = Some(global);
        state
    }

    /// The description surfaces are asked to render in.
    pub fn preferred(&self) -> OutputDescription {
        self.preferred
            .lock()
            .map_or_else(|_| OutputDescription::sdr(), |preferred| *preferred)
    }

    /// Output objects created so far. A new value means some object has not
    /// been told its output's description yet.
    #[cfg(target_os = "linux")]
    pub fn output_objects(&self) -> u64 {
        self.output_objects
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Update what each output tells clients it displays. `describe` maps a
    /// client's `wl_output` to its description (`None`: an ordinary SDR
    /// output). Objects whose description changed get
    /// `image_description_changed`; objects of gone clients are dropped.
    #[cfg(target_os = "linux")]
    pub fn update_outputs(&self, describe: impl Fn(&WlOutput) -> Option<OutputDescription>) {
        let Ok(mut outputs) = self.outputs.lock() else {
            return;
        };
        outputs.retain(|(resource, _)| resource.is_alive());
        for (resource, data) in outputs.iter() {
            let description = data
                .output
                .as_ref()
                .and_then(&describe)
                .unwrap_or_else(OutputDescription::sdr);
            if data.set_description(description) {
                let () = resource.image_description_changed();
            }
        }
    }

    /// Change the description surfaces are asked to render in and tell
    /// every feedback object.
    #[cfg(target_os = "linux")]
    pub fn set_preferred(&self, description: OutputDescription) {
        let changed = self.preferred.lock().is_ok_and(|mut preferred| {
            let changed = *preferred != description;
            *preferred = description;
            changed
        });
        if !changed {
            return;
        }
        let Ok(mut feedbacks) = self.feedbacks.lock() else {
            return;
        };
        feedbacks.retain(Resource::is_alive);
        let identity = description_data(description).identity_lo();
        for feedback in feedbacks.iter() {
            let () = feedback.preferred_changed(identity);
        }
    }

    /// Zero-globals constructor for headless tests that never bind a
    /// display. Keeps the advertised-capability defaults but skips
    /// registering the Wayland global.
    #[must_use]
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::default()
    }
}

// -----------------------------------------------------------------------------
// UserData for each protocol object.
// -----------------------------------------------------------------------------

/// Builder state for `wp_image_description_creator_params_v1`: the parameters set so far, each
/// at most once. `create` destroys the creator, so nothing reaches it afterwards.
#[derive(Debug, Default)]
pub struct ParametricBuilder {
    params: Mutex<Params>,
}

impl ParametricBuilder {
    /// The settled image description, or `None` while primaries or transfer are missing.
    fn build(&self) -> Option<ImageDescriptionData> {
        let params = self.params.lock().ok()?.clone();
        // Required: primaries + transfer.
        let primaries = match params.primaries? {
            ParamPrimaries::Named(named) => primaries_named_to_color(named),
            ParamPrimaries::Chromaticities(xy) => guess_primaries_from_custom(xy),
        };
        let transfer = match params.transfer? {
            Transfer::Named(named) => transfer_named_to_color(named),
            // The common named power curve.
            Transfer::Power(22_000) => TransferCharacteristics::Bt709,
            Transfer::Power(_) => TransferCharacteristics::Unspecified,
        };
        // Matrix — a client that speaks `color-management-v1` alone
        // does not describe matrix; we default to BT.2020-nc for
        // BT.2020 primaries + a HDR transfer, and BT.709 otherwise.
        let matrix = if primaries == ColorPrimaries::Bt2020 && transfer.is_hdr() {
            MatrixCoefficients::Bt2020NonConstant
        } else if matches!(primaries, ColorPrimaries::Bt709) {
            MatrixCoefficients::Bt709
        } else {
            MatrixCoefficients::Unspecified
        };
        let colorimetry = Colorimetry {
            primaries,
            transfer,
            matrix,
            range: ColorRange::Limited,
        };

        // Optional HDR10 metadata from mastering primaries + luminances.
        // The two are independent requests: a client may describe the
        // luminance range it was mastered at without naming the
        // mastering primaries, so each falls back to the reference
        // volume on its own.
        let hdr =
            if transfer.is_hdr() {
                let reference = MasteringDisplayColorVolume::hdr10_reference();
                let volume = params.mastering_primaries.map_or(
                    reference,
                    |[rx, ry, gx, gy, bx, by, wx, wy]| MasteringDisplayColorVolume {
                        display_primaries: [
                            (chrom_micro_to_st2086(rx), chrom_micro_to_st2086(ry)),
                            (chrom_micro_to_st2086(gx), chrom_micro_to_st2086(gy)),
                            (chrom_micro_to_st2086(bx), chrom_micro_to_st2086(by)),
                        ],
                        white_point: (chrom_micro_to_st2086(wx), chrom_micro_to_st2086(wy)),
                        ..reference
                    },
                );
                let mastering = MasteringDisplayColorVolume {
                    max_display_mastering_luminance: params
                        .mastering_luminance
                        .map_or(volume.max_display_mastering_luminance, |(_, max)| {
                            max.saturating_mul(10_000)
                        }),
                    min_display_mastering_luminance: params
                        .mastering_luminance
                        .map_or(volume.min_display_mastering_luminance, |(min, _)| min),
                    ..volume
                };
                let light = ContentLightLevel {
                    max_content_light_level: params.max_cll.map_or(0, clamp_to_u16),
                    max_frame_average_light_level: params.max_fall.map_or(0, clamp_to_u16),
                };
                Some(Hdr10Metadata { mastering, light })
            } else {
                None
            };

        Some(ImageDescriptionData {
            colorimetry,
            hdr,
            identity: mint_identity(),
            queryable: false,
            reference_luminance: DEFAULT_REFERENCE_LUMINANCE,
        })
    }
}

/// Clamp a `u32` into `u16`'s range rather than truncate it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to 0..=u32::from(u16::MAX) immediately above, so it always fits u16"
)]
fn clamp_to_u16(v: u32) -> u16 {
    v.min(u32::from(u16::MAX)) as u16
}

/// A `wp_color_manager_v1.primaries` value as the colorimetry it names.
fn primaries_named_to_color(named: u32) -> ColorPrimaries {
    match named {
        1 => ColorPrimaries::Bt709,
        6 => ColorPrimaries::Bt2020,
        9 => ColorPrimaries::DisplayP3,
        // Uncommon primaries: unspecified, so the encoder falls back to BT.709 defaults.
        _ => ColorPrimaries::Unspecified,
    }
}

/// A `wp_color_manager_v1.transfer_function` value as the transfer it names.
fn transfer_named_to_color(named: u32) -> TransferCharacteristics {
    match named {
        1 | 2 => TransferCharacteristics::Bt709,
        5 => TransferCharacteristics::Linear,
        9 | 10 => TransferCharacteristics::Srgb,
        11 => TransferCharacteristics::SmpteSt2084,
        13 => TransferCharacteristics::AribStdB67,
        _ => TransferCharacteristics::Unspecified,
    }
}

/// The named primaries nearest to chromaticities (red, green, blue, white; x then y).
fn guess_primaries_from_custom([red_x, ..]: [i32; 8]) -> ColorPrimaries {
    // Red primary > 0.680 → almost certainly BT.2020 (0.708).
    if red_x > 680_000 {
        ColorPrimaries::Bt2020
    } else if red_x > 650_000 {
        ColorPrimaries::DisplayP3
    } else {
        ColorPrimaries::Bt709
    }
}

/// Wayland ships CIE 1931 xy as x × `1_000_000`; SMPTE ST 2086 wants
/// x × `50_000`. Divide by 20, saturating so a rogue negative from the
/// wire doesn't sign-flip.
fn chrom_micro_to_st2086(micro: i32) -> u16 {
    #[allow(
        clippy::cast_sign_loss,
        reason = "clamped to >= 0 immediately above, so it always fits a non-negative u32"
    )]
    let clamped = micro.max(0) as u32;
    let scaled = clamped.saturating_add(10).saturating_div(20);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to 0..=u32::from(u16::MAX) immediately above, so it always fits u16"
    )]
    let st2086 = scaled.min(u32::from(u16::MAX)) as u16;
    st2086
}

/// Monotonic image-description identity counter. Never recycled per
/// the protocol's requirement.
static NEXT_IDENTITY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn mint_identity() -> u64 {
    NEXT_IDENTITY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Sealed image description payload.
///
/// Handed out attached to a `wp_image_description_v1` as its
/// user-data. The compositor's paint + encoder paths read this
/// through the surface's [`ColorManagementSurfaceData`].
#[derive(Debug, Clone)]
pub struct ImageDescriptionData {
    pub colorimetry: Colorimetry,
    pub hdr: Option<Hdr10Metadata>,
    /// Compositor-wide unique id number. Kept as u64 even though the
    /// currently advertised v1 protocol exposes only the low 32 bits.
    pub identity: u64,
    /// Whether the compositor created this description from output/feedback
    /// state and therefore must answer `get_information`. Client-created
    /// parametric descriptions are deliberately attach-only.
    pub queryable: bool,
    /// Where SDR white sits, cd/m². For an HDR output this is the user's SDR
    /// content brightness, so HDR clients place their UI at the same level
    /// as the desktop around them.
    pub reference_luminance: u32,
}

/// BT.2408 reference white, the reference luminance unless an output says
/// otherwise.
pub const DEFAULT_REFERENCE_LUMINANCE: u32 = 203;

impl ImageDescriptionData {
    /// The Windows-scRGB description: linear light with sRGB primaries,
    /// `1.0` at SDR white and values above it for HDR highlights, full
    /// range RGB.
    pub fn windows_scrgb() -> Self {
        Self {
            colorimetry: Colorimetry {
                primaries: ColorPrimaries::Bt709,
                transfer: TransferCharacteristics::Linear,
                matrix: MatrixCoefficients::Identity,
                range: ColorRange::Full,
            },
            hdr: None,
            identity: mint_identity(),
            queryable: false,
            reference_luminance: DEFAULT_REFERENCE_LUMINANCE,
        }
    }
}

/// What the compositor tells clients one output displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputDescription {
    pub colorimetry: Colorimetry,
    pub hdr: Option<Hdr10Metadata>,
    pub reference_luminance: u32,
}

impl OutputDescription {
    /// An ordinary SDR output.
    pub fn sdr() -> Self {
        Self {
            colorimetry: Colorimetry::sdr_bt709(),
            hdr: None,
            reference_luminance: DEFAULT_REFERENCE_LUMINANCE,
        }
    }

    /// An output driven with a PQ BT.2020 signal: the sink's EDID primaries
    /// and luminance as the mastering volume, SDR white at `sdr_white_nits`.
    pub fn hdr_pq(display: &crate::edid::HdrDisplayInfo, sdr_white_nits: f32) -> Self {
        let to_st2086 = |micro: u32| -> u16 {
            u16::try_from(micro.checked_div(20).unwrap_or(0).min(50_000)).unwrap_or(50_000)
        };
        let [r, g, b] = display.display_primaries;
        let pair = |(x, y): (u32, u32)| (to_st2086(x), to_st2086(y));
        let mastering = MasteringDisplayColorVolume {
            display_primaries: [pair(r), pair(g), pair(b)],
            white_point: pair(display.white_point),
            max_display_mastering_luminance: u32::from(display.max_luminance_cd_m2)
                .saturating_mul(10_000),
            min_display_mastering_luminance: display.min_luminance_micro_cd_m2,
        };
        let light = ContentLightLevel {
            max_content_light_level: display.max_luminance_cd_m2,
            max_frame_average_light_level: display.max_frame_average_luminance_cd_m2,
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "SDR white is clamped to a small positive range before the cast"
        )]
        let reference_luminance = sdr_white_nits.clamp(1.0, 10_000.0).round() as u32;
        Self {
            colorimetry: Colorimetry::hdr10(),
            hdr: Some(Hdr10Metadata { mastering, light }),
            reference_luminance,
        }
    }
}

impl ImageDescriptionData {
    /// Low 32 bits of the identity for the pre-v2 `ready` event.
    #[must_use]
    pub fn identity_lo(&self) -> u32 {
        (self.identity & 0xFFFF_FFFF) as u32
    }

    fn information_colors(
        &self,
    ) -> Option<(Option<ColorPrimaries>, Option<TransferCharacteristics>)> {
        self.queryable.then(|| {
            let primaries = match self.colorimetry.primaries {
                ColorPrimaries::Bt709 => Some(ColorPrimaries::Bt709),
                ColorPrimaries::Bt2020 => Some(ColorPrimaries::Bt2020),
                ColorPrimaries::DisplayP3 => Some(ColorPrimaries::DisplayP3),
                ColorPrimaries::Unspecified => None,
            };
            let transfer = match self.colorimetry.transfer {
                TransferCharacteristics::Bt709 => Some(TransferCharacteristics::Bt709),
                TransferCharacteristics::Srgb => Some(TransferCharacteristics::Srgb),
                TransferCharacteristics::Linear => Some(TransferCharacteristics::Linear),
                TransferCharacteristics::SmpteSt2084 => Some(TransferCharacteristics::SmpteSt2084),
                TransferCharacteristics::AribStdB67 => Some(TransferCharacteristics::AribStdB67),
                TransferCharacteristics::Unspecified => None,
            };
            (primaries, transfer)
        })
    }

    #[cfg(target_os = "linux")]
    fn information(&self) -> Option<(Option<Primaries>, Option<TransferFunction>)> {
        let (primaries, transfer) = self.information_colors()?;
        Some((
            primaries.and_then(|value| match value {
                ColorPrimaries::Bt709 => Some(Primaries::Srgb),
                ColorPrimaries::Bt2020 => Some(Primaries::Bt2020),
                ColorPrimaries::DisplayP3 => Some(Primaries::DisplayP3),
                ColorPrimaries::Unspecified => None,
            }),
            transfer.and_then(|value| match value {
                TransferCharacteristics::Bt709 => Some(TransferFunction::Bt1886),
                TransferCharacteristics::Srgb => Some(TransferFunction::ExtSrgb),
                TransferCharacteristics::Linear => Some(TransferFunction::ExtLinear),
                TransferCharacteristics::SmpteSt2084 => Some(TransferFunction::St2084Pq),
                TransferCharacteristics::AribStdB67 => Some(TransferFunction::Hlg),
                TransferCharacteristics::Unspecified => None,
            }),
        ))
    }

    fn protocol_primaries(&self) -> (i32, i32, i32, i32, i32, i32, i32, i32) {
        // Fixed CIE 1931 coordinates for the named primary sets we expose.
        // The protocol requires the coordinate event even when the matching
        // named event is also sent.
        match self.colorimetry.primaries {
            ColorPrimaries::Bt2020 => (
                708_000, 292_000, 170_000, 797_000, 131_000, 46_000, 312_700, 329_000,
            ),
            ColorPrimaries::DisplayP3 => (
                680_000, 320_000, 265_000, 690_000, 150_000, 60_000, 312_700, 329_000,
            ),
            // BT.709 and the conservative fallback are the sRGB primaries.
            ColorPrimaries::Bt709 | ColorPrimaries::Unspecified => (
                640_000, 330_000, 300_000, 600_000, 150_000, 60_000, 312_700, 329_000,
            ),
        }
    }

    fn protocol_luminances(&self) -> (u32, u32, u32) {
        // BT.1886's protocol-defined defaults are 0.01/100/100 cd/m².
        self.hdr.as_ref().map_or((100, 100, 100), |hdr| {
            (
                hdr.mastering.min_display_mastering_luminance,
                hdr.mastering.max_display_mastering_luminance / 10_000,
                self.reference_luminance,
            )
        })
    }
}

/// Per-surface color-management state.
///
/// Sits in `SurfaceData::data_map`. The client attaches an image
/// description via `set_image_description`; the value goes into
/// `Self::pending`. On `wl_surface.commit` the compositor flips
/// `Self::pending` → [`Self::current`].
///
/// **Simplification vs the spec**: the protocol makes this
/// double-buffered state. In practice we snap the value straight to
/// `current` on set — the flatpak-share encoder reads `current` on
/// every frame and the difference is invisible for the streaming
/// path. If a future consumer (e.g. the wgpu backend's colour-space
/// picker) wants strict double-buffering, wire the flip through
/// `CompositorHandler::commit`.
#[derive(Debug, Default)]
pub struct ColorManagementSurfaceData {
    inner: Mutex<ColorManagementSurfaceInner>,
}

#[derive(Debug, Default)]
struct ColorManagementSurfaceInner {
    current: Option<ImageDescriptionData>,
    /// Guards `get_surface` from creating a second per-surface manager.
    /// Set the first time a `wp_color_management_surface_v1` is minted
    /// for this `wl_surface`.
    manager_attached: bool,
}

impl ColorManagementSurfaceData {
    /// Currently-applied image description, or `None` if the client
    /// never attached one (or explicitly unset it). Read by the
    /// flatpak-share encoder to override the config-driven default.
    pub fn current(&self) -> Option<ImageDescriptionData> {
        self.inner.lock().ok().and_then(|g| g.current.clone())
    }

    /// Convenience: yields the current colorimetry + HDR bundle.
    pub fn current_colorimetry(&self) -> Option<(Colorimetry, Option<Hdr10Metadata>)> {
        self.current().map(|d| (d.colorimetry, d.hdr))
    }

    #[cfg(target_os = "linux")]
    fn set_current(&self, data: ImageDescriptionData) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.current = Some(data);
        }
    }

    #[cfg(target_os = "linux")]
    fn clear_current(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.current = None;
        }
    }
}

/// Read the current image description off a `wl_surface`.
///
/// Public so the flatpak-share encoder can look up the client's
/// declared colorimetry without depending on the wayland protocol
/// module.
#[cfg(target_os = "linux")]
pub fn surface_current_colorimetry(
    surface: &WlSurface,
) -> Option<(Colorimetry, Option<Hdr10Metadata>)> {
    wl_compositor::with_states(surface, |states| {
        states
            .data_map
            .get::<ColorManagementSurfaceData>()
            .and_then(ColorManagementSurfaceData::current_colorimetry)
    })
}

/// User-data for `wp_color_management_output_v1` — remembers the
/// `wl_output` it was created for so subsequent
/// `get_image_description` requests can synthesise the display's
/// colorimetry.
#[derive(Debug, Clone)]
pub struct ColorManagementOutputData {
    // The `wl_output` this object describes. Tests of the pure descriptor
    // contract use `None`; live output resources always use `Some`, which
    // [`ColorManagerState::update_outputs`] maps back to the backend output.
    #[cfg(target_os = "linux")]
    output: Option<WlOutput>,
    /// Colorimetry the compositor tells clients this output actually
    /// displays. `default_sdr` on winit / headless; populated from the
    /// EDID HDR block on DRM. Held by `Arc<Mutex>` so the DRM
    /// backend can push updates without minting a new proxy.
    color: Arc<Mutex<Colorimetry>>,
    hdr: Arc<Mutex<Option<Hdr10Metadata>>>,
    reference_luminance: Arc<Mutex<u32>>,
}

impl ColorManagementOutputData {
    fn from_color(color: Colorimetry, hdr: Option<Hdr10Metadata>) -> Self {
        Self {
            #[cfg(target_os = "linux")]
            output: None,
            color: Arc::new(Mutex::new(color)),
            hdr: Arc::new(Mutex::new(hdr)),
            reference_luminance: Arc::new(Mutex::new(DEFAULT_REFERENCE_LUMINANCE)),
        }
    }

    /// The description this output object currently hands out.
    pub fn description(&self) -> OutputDescription {
        OutputDescription {
            colorimetry: self.color.lock().map(|c| *c).unwrap_or_default(),
            hdr: self.hdr.lock().ok().and_then(|h| *h),
            reference_luminance: self
                .reference_luminance
                .lock()
                .map_or(DEFAULT_REFERENCE_LUMINANCE, |r| *r),
        }
    }

    /// Replace the description; `true` when it changed.
    pub fn set_description(&self, description: OutputDescription) -> bool {
        if self.description() == description {
            return false;
        }
        self.set_display_color(description.colorimetry, description.hdr);
        if let Ok(mut reference) = self.reference_luminance.lock() {
            *reference = description.reference_luminance;
        }
        true
    }

    /// Attach a fresh output-side descriptor. Defaults to SDR BT.709;
    /// [`ColorManagerState::update_outputs`] sets the real description.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn new(output: WlOutput) -> Self {
        Self {
            output: Some(output),
            ..Self::from_color(Colorimetry::sdr_bt709(), None)
        }
    }

    /// Set the display's actual color space and optional HDR metadata.
    ///
    /// Called by the DRM backend when an EDID arrives or color space
    /// information is updated.
    pub fn set_display_color(&self, color: Colorimetry, hdr: Option<Hdr10Metadata>) {
        if let Ok(mut g) = self.color.lock() {
            *g = color;
        }
        if let Ok(mut g) = self.hdr.lock() {
            *g = hdr;
        }
    }

    fn snapshot(&self) -> ImageDescriptionData {
        description_data(self.description())
    }
}

/// A fresh queryable image description for `description`.
fn description_data(description: OutputDescription) -> ImageDescriptionData {
    ImageDescriptionData {
        colorimetry: description.colorimetry,
        hdr: description.hdr,
        identity: mint_identity(),
        queryable: true,
        reference_luminance: description.reference_luminance,
    }
}

/// User-data attached to a `wp_color_management_surface_v1`. Holds
/// the target `wl_surface` so subsequent set/unset requests know
/// which surface to mutate.
#[derive(Debug, Clone)]
#[cfg(target_os = "linux")]
pub struct ColorManagementSurfaceUserData {
    // Retains the protocol surface identity; pending/current color state lives
    // in the compositor surface data map and is applied on wl_surface.commit.
    #[allow(
        dead_code,
        reason = "holding the wl_surface preserves the protocol object's identity while pending color state lives in Smithay's surface data map"
    )]
    surface: WlSurface,
}

#[cfg(target_os = "linux")]
impl ColorManagementSurfaceUserData {
    /// Create a new user-data instance for a `wp_color_management_surface_v1`.
    #[must_use]
    pub fn new(surface: WlSurface) -> Self {
        Self { surface }
    }
}

/// User-data attached to a `wp_color_management_surface_feedback_v1`.
/// Minimal — carries the `wl_surface` so `get_preferred*` can hand
/// back the compositor's preferred description.
#[derive(Debug, Clone)]
#[cfg(target_os = "linux")]
pub struct ColorManagementSurfaceFeedbackData {
    // Retains the surface identity for preferred-description feedback.
    #[allow(
        dead_code,
        reason = "holding the wl_surface preserves feedback-resource identity for future preferred-description updates"
    )]
    surface: WlSurface,
}

#[cfg(target_os = "linux")]
impl ColorManagementSurfaceFeedbackData {
    /// Create a new user-data instance for a `wp_color_management_surface_feedback_v1`.
    #[must_use]
    pub fn new(surface: WlSurface) -> Self {
        Self { surface }
    }
}

// -----------------------------------------------------------------------------
// Dispatch impls
// -----------------------------------------------------------------------------

#[cfg(target_os = "linux")]
impl<D> GlobalDispatch<WpColorManagerV1, (), D> for ColorManagerState
where
    D: GlobalDispatch<WpColorManagerV1, ()>
        + Dispatch<WpColorManagerV1, ()>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParametricBuilder>>
        + Dispatch<WpImageDescriptionCreatorIccV1, ()>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceUserData>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData>
        + Dispatch<WpColorManagementOutputV1, ColorManagementOutputData>
        + AsRef<ColorManagerState>
        + 'static,
{
    fn bind(
        state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpColorManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());
        tilo_color_params::server::advertise(&manager, &state.as_ref().support);
    }
}

#[cfg(target_os = "linux")]
impl<D> Dispatch<WpColorManagerV1, (), D> for ColorManagerState
where
    D: Dispatch<WpColorManagerV1, ()>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParametricBuilder>>
        + Dispatch<WpImageDescriptionCreatorIccV1, ()>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceUserData>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData>
        + Dispatch<WpColorManagementOutputV1, ColorManagementOutputData>
        + AsRef<ColorManagerState>
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        manager: &WpColorManagerV1,
        request: <WpColorManagerV1 as Resource>::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_manager_v1::Request::CreateParametricCreator { obj } => {
                let builder = Arc::new(ParametricBuilder::default());
                data_init.init(obj, builder);
                if !state.as_ref().support.has(feature::PARAMETRIC) {
                    manager.post_error(
                        wp_color_manager_v1::Error::UnsupportedFeature,
                        "parametric image descriptions are not supported",
                    );
                }
            }
            wp_color_manager_v1::Request::CreateIccCreator { obj } => {
                // We don't advertise the icc_v2_v4 feature — clients
                // that call this are asking for something we said we
                // don't support. Post the mandated protocol error.
                // Initialise the request-created object before posting the
                // fatal error.  wayland-backend installs user data after the
                // callback returns; killing the client while this `new_id`
                // is still uninitialised removes it from the object map and
                // older backends then panic while completing dispatch.
                data_init.init(obj, ());
                manager.post_error(
                    wp_color_manager_v1::Error::UnsupportedFeature,
                    "ICC-based image descriptions are not supported",
                );
            }
            wp_color_manager_v1::Request::CreateWindowsScrgb { image_description } => {
                let data = ImageDescriptionData::windows_scrgb();
                let identity = data.identity;
                let obj = data_init.init(image_description, data);
                if !state.as_ref().support.has(feature::WINDOWS_SCRGB) {
                    manager.post_error(
                        wp_color_manager_v1::Error::UnsupportedFeature,
                        "the Windows scRGB description is not supported",
                    );
                    return;
                }
                // Version 1: `ready` is the only success event (see
                // `create_image_description`).
                tell_ready(state.as_ref(), &obj, (identity & 0xFFFF_FFFF) as u32);
            }
            wp_color_manager_v1::Request::GetOutput { id, output } => {
                let data = ColorManagementOutputData::new(output);
                let resource = data_init.init(id, data.clone());
                if let Ok(mut outputs) = state.as_ref().outputs.lock() {
                    outputs.push((resource, data));
                }
                state
                    .as_ref()
                    .output_objects
                    .fetch_add(1, std::sync::atomic::Ordering::Release);
            }
            wp_color_manager_v1::Request::GetSurface { id, surface } => {
                let already_taken = wl_compositor::with_states(&surface, |states| {
                    states
                        .data_map
                        .insert_if_missing_threadsafe(ColorManagementSurfaceData::default);
                    // `insert_if_missing_threadsafe` just above should guarantee this
                    // is present; if it's somehow missing, treat it as "already
                    // taken" so the client sees a protocol error rather than the
                    // compositor panicking (this process runs with `panic = "abort"`,
                    // so a panic here would kill every other client's session too).
                    let Some(d) = states.data_map.get::<ColorManagementSurfaceData>() else {
                        return true;
                    };
                    let mut inner = match d.inner.lock() {
                        Ok(g) => g,
                        // Poisoned mutex — treat as "already taken" so
                        // the client sees a protocol error instead of
                        // reusing garbage state.
                        Err(p) => p.into_inner(),
                    };
                    let taken = inner.manager_attached;
                    if !taken {
                        inner.manager_attached = true;
                    }
                    taken
                });
                if already_taken {
                    manager.post_error(
                        wp_color_manager_v1::Error::SurfaceExists,
                        "wl_surface already has a color management object",
                    );
                } else {
                    data_init.init(id, ColorManagementSurfaceUserData::new(surface));
                }
            }
            wp_color_manager_v1::Request::GetSurfaceFeedback { id, surface } => {
                let resource = data_init.init(id, ColorManagementSurfaceFeedbackData::new(surface));
                if let Ok(mut feedbacks) = state.as_ref().feedbacks.lock() {
                    feedbacks.push(resource);
                }
            }
            wp_color_manager_v1::Request::GetImageDescription {
                image_description,
                reference,
            } => {
                // We never hand out `wp_image_description_reference_v1`
                // objects so this request should never fire on a live
                // client; the compositor sends `failed` to make the
                // situation observable.
                let _ = reference;
                let desc = ImageDescriptionData {
                    colorimetry: Colorimetry::sdr_bt709(),
                    hdr: None,
                    identity: 0,
                    queryable: false,
                    reference_luminance: DEFAULT_REFERENCE_LUMINANCE,
                };
                let inited = data_init.init(image_description, desc);
                let () = inited.failed(
                    ImageCause::Unsupported,
                    "image_description references are unsupported".into(),
                );
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl<D> Dispatch<WpImageDescriptionCreatorIccV1, (), D> for ColorManagerState
where
    D: Dispatch<WpImageDescriptionCreatorIccV1, ()> + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _obj: &WpImageDescriptionCreatorIccV1,
        _request: <WpImageDescriptionCreatorIccV1 as Resource>::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, D>,
    ) {
        // The manager never advertises ICC support, so a conforming client is
        // disconnected by `create_icc_creator` before it can reach this object.
    }
}

#[cfg(target_os = "linux")]
impl<D> Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParametricBuilder>, D> for ColorManagerState
where
    D: Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParametricBuilder>>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + AsRef<ColorManagerState>
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        creator: &WpImageDescriptionCreatorParamsV1,
        request: <WpImageDescriptionCreatorParamsV1 as Resource>::Request,
        builder: &Arc<ParametricBuilder>,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        let set = match tilo_color_params::server::set_of(request) {
            Ok(set) => set,
            Err(wp_image_description_creator_params_v1::Request::Create { image_description }) => {
                create_image_description(
                    state.as_ref(),
                    creator,
                    builder,
                    image_description,
                    data_init,
                );
                return;
            }
            Err(_) => return,
        };
        let taken = builder
            .params
            .lock()
            .map_err(|_| tilo_color_params::ParamError::IncompleteSet)
            .and_then(|mut params| params.set(set, &state.as_ref().support));
        if let Err(error) = taken {
            creator.post_error(error.code(), error.message());
        }
    }
}

/// Tell a client its description is ready, or hold it back while a test says so.
#[cfg(target_os = "linux")]
fn tell_ready(manager: &ColorManagerState, obj: &WpImageDescriptionV1, identity_lo: u32) {
    #[cfg(feature = "test-support")]
    {
        if manager.holding.load(Ordering::Relaxed) {
            if let Ok(mut held) = manager.held.lock() {
                held.push((obj.clone(), identity_lo));
            }
            return;
        }
    }
    #[cfg(not(feature = "test-support"))]
    let _ = manager;
    let () = obj.ready(identity_lo);
}

/// `Create` request: the settled description, announced with the v1 `ready` event, or the
/// protocol error the parameters raise.
#[cfg(target_os = "linux")]
fn create_image_description<D>(
    manager: &ColorManagerState,
    creator: &WpImageDescriptionCreatorParamsV1,
    builder: &Arc<ParametricBuilder>,
    image_description: New<WpImageDescriptionV1>,
    data_init: &mut DataInit<'_, D>,
) where
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let checked = builder
        .params
        .lock()
        .map_err(|_| tilo_color_params::ParamError::IncompleteSet)
        .and_then(|params| params.check_complete());
    match checked.and_then(|()| {
        builder
            .build()
            .ok_or(tilo_color_params::ParamError::IncompleteSet)
    }) {
        Ok(data) => {
            let identity = data.identity;
            let obj = data_init.init(image_description, data);
            // The global is deliberately version 1, so `ready` is the only
            // valid success event. `ready2` has opcode 2 and exists only from
            // version 2 onward; emitting it disconnects v1 clients.
            tell_ready(manager, &obj, (identity & 0xFFFF_FFFF) as u32);
        }
        Err(error) => {
            // The request carried a `new_id`; initialise it before the fatal
            // error removes the client and all of its protocol objects.
            data_init.init(
                image_description,
                ImageDescriptionData {
                    colorimetry: Colorimetry::sdr_bt709(),
                    hdr: None,
                    identity: 0,
                    queryable: false,
                    reference_luminance: DEFAULT_REFERENCE_LUMINANCE,
                },
            );
            creator.post_error(error.code(), error.message());
        }
    }
}

#[cfg(target_os = "linux")]
impl<D> Dispatch<WpImageDescriptionV1, ImageDescriptionData, D> for ColorManagerState
where
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + Dispatch<WpImageDescriptionInfoV1, ImageDescriptionData>
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        obj: &WpImageDescriptionV1,
        request: <WpImageDescriptionV1 as Resource>::Request,
        data: &ImageDescriptionData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        if let wp_image_description_v1::Request::GetInformation { information } = request {
            let Some((primaries, transfer)) = data.information() else {
                obj.post_error(
                    wp_image_description_v1::Error::NoInformation,
                    "get_information is not allowed on this image description",
                );
                return;
            };
            let info = data_init.init(information, data.clone());
            let (r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y) = data.protocol_primaries();
            info.primaries(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y);
            if let Some(primaries) = primaries {
                info.primaries_named(primaries);
            }
            if let Some(transfer) = transfer {
                info.tf_named(transfer);
            }
            let (min_lum, max_lum, reference_lum) = data.protocol_luminances();
            info.luminances(min_lum, max_lum, reference_lum);
            // The target volume equals the primary volume for the profiles we
            // currently mint, so target_primaries is intentionally omitted.
            info.target_luminance(min_lum, max_lum);
            let () = info.done();
        }
    }
}

#[cfg(target_os = "linux")]
impl<D> Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceUserData, D>
    for ColorManagerState
where
    D: Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceUserData>
        + AsRef<ColorManagerState>
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        obj: &WpColorManagementSurfaceV1,
        request: <WpColorManagementSurfaceV1 as Resource>::Request,
        data: &ColorManagementSurfaceUserData,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, D>,
    ) {
        // If the wl_surface was destroyed the object is inert.
        if !data.surface.is_alive() {
            if matches!(request, wp_color_management_surface_v1::Request::Destroy) {
                return;
            }
            obj.post_error(
                wp_color_management_surface_v1::Error::Inert,
                "surface has been destroyed",
            );
            return;
        }

        match request {
            wp_color_management_surface_v1::Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                if !state.as_ref().support.intent(render_intent.into()) {
                    obj.post_error(
                        wp_color_management_surface_v1::Error::RenderIntent,
                        "the rendering intent was not advertised",
                    );
                    return;
                }
                let Some(desc) = image_description.data::<ImageDescriptionData>().cloned() else {
                    obj.post_error(
                        wp_color_management_surface_v1::Error::ImageDescription,
                        "image description is not ready",
                    );
                    return;
                };
                wl_compositor::with_states(&data.surface, |states| {
                    states
                        .data_map
                        .insert_if_missing_threadsafe(ColorManagementSurfaceData::default);
                    if let Some(d) = states.data_map.get::<ColorManagementSurfaceData>() {
                        d.set_current(desc);
                    }
                });
            }
            wp_color_management_surface_v1::Request::UnsetImageDescription => {
                wl_compositor::with_states(&data.surface, |states| {
                    if let Some(d) = states.data_map.get::<ColorManagementSurfaceData>() {
                        d.clear_current();
                    }
                });
            }
            wp_color_management_surface_v1::Request::Destroy => {
                // Per spec: destroy behaves like unset.
                wl_compositor::with_states(&data.surface, |states| {
                    if let Some(d) = states.data_map.get::<ColorManagementSurfaceData>() {
                        d.clear_current();
                        if let Ok(mut inner) = d.inner.lock() {
                            inner.manager_attached = false;
                        }
                    }
                });
            }
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut D,
        _client_id: ClientId,
        _obj: &WpColorManagementSurfaceV1,
        data: &ColorManagementSurfaceUserData,
    ) {
        if data.surface.is_alive() {
            wl_compositor::with_states(&data.surface, |states| {
                if let Some(d) = states.data_map.get::<ColorManagementSurfaceData>()
                    && let Ok(mut inner) = d.inner.lock()
                {
                    inner.manager_attached = false;
                }
            });
        }
    }
}

#[cfg(target_os = "linux")]
impl<D> Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData, D>
    for ColorManagerState
where
    D: Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + AsRef<ColorManagerState>
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _obj: &WpColorManagementSurfaceFeedbackV1,
        request: <WpColorManagementSurfaceFeedbackV1 as Resource>::Request,
        _data: &ColorManagementSurfaceFeedbackData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_surface_feedback_v1::Request::GetPreferred {
                image_description,
            }
            | wp_color_management_surface_feedback_v1::Request::GetPreferredParametric {
                image_description,
            } => {
                let desc = description_data(state.as_ref().preferred());
                let identity_lo = desc.identity_lo();
                let obj = data_init.init(image_description, desc);
                let () = obj.ready(identity_lo);
            }
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
impl<D> Dispatch<WpColorManagementOutputV1, ColorManagementOutputData, D> for ColorManagerState
where
    D: Dispatch<WpColorManagementOutputV1, ColorManagementOutputData>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _obj: &WpColorManagementOutputV1,
        request: <WpColorManagementOutputV1 as Resource>::Request,
        data: &ColorManagementOutputData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        if let wp_color_management_output_v1::Request::GetImageDescription { image_description } =
            request
        {
            let desc = data.snapshot();
            let identity_lo = desc.identity_lo();
            let obj = data_init.init(image_description, desc);
            let () = obj.ready(identity_lo);
        }
    }
}

// The interface has no client requests other than object destruction; the
// compositor emits its complete event sequence when get_information creates it.
#[cfg(target_os = "linux")]
impl<D> Dispatch<WpImageDescriptionInfoV1, ImageDescriptionData, D> for ColorManagerState
where
    D: Dispatch<WpImageDescriptionInfoV1, ImageDescriptionData> + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _obj: &WpImageDescriptionInfoV1,
        _request: <WpImageDescriptionInfoV1 as Resource>::Request,
        _data: &ImageDescriptionData,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, D>,
    ) {
        // Destroyed via `done` from the compositor; no client-side
        // requests are defined on this interface.
    }
}

// -----------------------------------------------------------------------------
// Type-level plumbing wiring the trait impls above onto the concrete
// compositor `State`.
// -----------------------------------------------------------------------------

/// Wire the `color-management-v1` dispatch impls onto a concrete
/// compositor state. `$ty` must own a `ColorManagerState` and
/// implement `AsRef<ColorManagerState>` for it.
#[cfg(target_os = "linux")]
#[macro_export]
macro_rules! delegate_color_management {
    ($ty:ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: ()
            ] => $crate::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1: ()
            ] => $crate::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: ()
            ] => $crate::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1:
                    std::sync::Arc<$crate::color_management::ParametricBuilder>
            ] => $crate::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_image_description_v1::WpImageDescriptionV1:
                    $crate::color_management::ImageDescriptionData
            ] => $crate::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_v1::WpColorManagementSurfaceV1:
                    $crate::color_management::ColorManagementSurfaceUserData
            ] => $crate::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1:
                    $crate::color_management::ColorManagementSurfaceFeedbackData
            ] => $crate::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_color_management_output_v1::WpColorManagementOutputV1:
                    $crate::color_management::ColorManagementOutputData
            ] => $crate::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_image_description_info_v1::WpImageDescriptionInfoV1:
                    $crate::color_management::ImageDescriptionData
            ] => $crate::color_management::ColorManagerState
        );
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A builder told `primaries` and `transfer` by name, and the mastering luminance range.
    fn builder(
        primaries: u32,
        transfer: u32,
        mastering_luminance: Option<(u32, u32)>,
    ) -> ParametricBuilder {
        ParametricBuilder {
            params: Mutex::new(Params {
                primaries: Some(ParamPrimaries::Named(primaries)),
                transfer: Some(Transfer::Named(transfer)),
                mastering_luminance,
                ..Params::default()
            }),
        }
    }

    #[test]
    fn color_management_advertises_builds_and_exposes_hdr_descriptions() {
        // Phase `advertised_capabilities_include_bt2020_and_pq`.
        {
            let cfg = ColorManagerState::default();
            assert!(cfg.support.primaries.contains(&(6)));
            assert!(cfg.support.primaries.contains(&(1)));
            assert!(cfg.support.primaries.contains(&(9)));
            assert!(cfg.support.transfers.contains(&(11)));
            assert!(cfg.support.transfers.contains(&(13)));
            assert!(cfg.support.has(feature::PARAMETRIC));
            assert!(cfg.support.has(feature::SET_PRIMARIES));
        }

        // Phase `builder_yields_bt2020_pq_when_configured`.
        {
            let b = builder(6, 11, None);
            let desc = b.build().expect("builder produces description");
            assert_eq!(desc.colorimetry.primaries, ColorPrimaries::Bt2020);
            assert_eq!(
                desc.colorimetry.transfer,
                TransferCharacteristics::SmpteSt2084
            );
            assert!(desc.hdr.is_some());
        }

        {
            let b = ParametricBuilder::default();
            assert!(b.build().is_none());
            b.params.lock().unwrap().primaries = Some(ParamPrimaries::Named(1));
            assert!(b.build().is_none());
        }

        {
            let b = builder(1, 1, None);
            let desc = b.build().unwrap();
            assert!(desc.hdr.is_none(), "SDR builder must not emit HDR metadata");
        }

        {
            let output =
                ColorManagementOutputData::from_color(Colorimetry::sdr_bt709(), None).snapshot();
            assert_eq!(
                output.information_colors(),
                Some((
                    Some(ColorPrimaries::Bt709),
                    Some(TransferCharacteristics::Bt709)
                ))
            );
            assert_eq!(
                output.protocol_primaries(),
                (
                    640_000, 330_000, 300_000, 600_000, 150_000, 60_000, 312_700, 329_000
                )
            );
            assert_eq!(output.protocol_luminances(), (100, 100, 100));

            let made = builder(6, 11, None);
            assert_eq!(made.build().unwrap().information_colors(), None);
        }
    }

    #[test]
    fn a_mastering_luminance_range_alone_is_kept() {
        let reference = MasteringDisplayColorVolume::hdr10_reference();
        let made = builder(6, 11, Some((500, 4_000)));
        let hdr = made.build().unwrap().hdr.expect("PQ carries metadata");
        assert_eq!(hdr.mastering.max_display_mastering_luminance, 40_000_000);
        assert_eq!(hdr.mastering.min_display_mastering_luminance, 500);
        assert_eq!(
            hdr.mastering.display_primaries, reference.display_primaries,
            "without mastering primaries the reference volume's primaries stand in"
        );
    }

    #[test]
    fn an_hdr_output_describes_pq_bt2020_with_its_edid_volume() {
        {
            let display = crate::edid::HdrDisplayInfo {
                display_primaries: [(708_000, 292_000), (170_000, 797_000), (131_000, 46_000)],
                white_point: (312_700, 329_000),
                max_luminance_cd_m2: 800,
                max_frame_average_luminance_cd_m2: 350,
                min_luminance_micro_cd_m2: 500,
                ..crate::edid::HdrDisplayInfo::default_sdr()
            };
            let description = OutputDescription::hdr_pq(&display, 250.0);
            assert_eq!(
                description.colorimetry.transfer,
                TransferCharacteristics::SmpteSt2084
            );
            assert_eq!(description.colorimetry.primaries, ColorPrimaries::Bt2020);
            let hdr = description.hdr.expect("PQ output carries HDR metadata");
            assert_eq!(hdr.mastering.display_primaries[0], (35_400, 14_600));
            assert_eq!(hdr.mastering.max_display_mastering_luminance, 8_000_000);
            assert_eq!(hdr.light.max_frame_average_light_level, 350);
            let output = ColorManagementOutputData::from_color(Colorimetry::sdr_bt709(), None);
            assert!(output.set_description(description));
            assert!(
                !output.set_description(description),
                "unchanged is no event"
            );
            let snapshot = output.snapshot();
            assert_eq!(snapshot.protocol_luminances(), (500, 800, 250));
            assert_eq!(
                snapshot.information_colors(),
                Some((
                    Some(ColorPrimaries::Bt2020),
                    Some(TransferCharacteristics::SmpteSt2084)
                ))
            );
            assert!(output.set_description(OutputDescription::sdr()));
            assert_eq!(output.snapshot().protocol_luminances(), (100, 100, 100));
            let state = ColorManagerState::for_test();
            assert_eq!(state.preferred(), OutputDescription::sdr());
        }
    }
}
