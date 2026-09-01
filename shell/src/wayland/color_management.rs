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
//! `wp_color_management_surface_v1`. Deliberately unimplemented: ICC
//! creators and Windows-scRGB (`unsupported_feature` error — the
//! parametric path already unblocks HDR clients like `mpv --hdr`), and
//! `get_information` on our own descriptions (`no_information` — they're
//! surface-attach-only, not for round-tripping; feedback-surface
//! descriptions do get info).
//!
//! Per-surface attach is double-buffered per spec (pending on
//! `set_image_description`, applied on `wl_surface.commit`); the
//! pending and current values live in `SurfaceData::data_map` under
//! [`ColorManagementSurfaceData`], flipped by the commit hook in
//! [`crate::compositor::state::State`].

use std::sync::{Arc, Mutex};

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    backend::{ClientId, GlobalId},
    protocol::{wl_output::WlOutput, wl_surface::WlSurface},
};

use smithay::wayland::compositor::{self as wl_compositor};

use wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1::{self, WpColorManagementOutputV1},
    wp_color_management_surface_feedback_v1::{self, WpColorManagementSurfaceFeedbackV1},
    wp_color_management_surface_v1::{self, WpColorManagementSurfaceV1},
    wp_color_manager_v1::{
        self, Feature, Primaries, RenderIntent, TransferFunction, WpColorManagerV1,
    },
    wp_image_description_creator_params_v1::{self, WpImageDescriptionCreatorParamsV1},
    wp_image_description_info_v1::WpImageDescriptionInfoV1,
    wp_image_description_v1::{self, Cause as ImageCause, WpImageDescriptionV1},
};

use crate::collab::colorimetry::{
    ColorPrimaries, ColorRange, Colorimetry, MatrixCoefficients, TransferCharacteristics,
};
use crate::collab::hdr_metadata::{ContentLightLevel, Hdr10Metadata, MasteringDisplayColorVolume};

/// The maximum protocol version we support. Bump on staging churn.
const MAX_VERSION: u32 = 1;

/// Advertised capability set for `wp_color_manager_v1`.
///
/// Populated at compositor startup from the intersection of what the
/// wgpu backend can render, what the OS display driver supports, and
/// what we've validated with real clients. Kept as raw `u32`s
/// mirroring the protocol enum values so callers can construct fixture
/// [`ColorManagerState`] instances in tests without dragging in
/// `wayland-protocols` (matches the task spec).
#[derive(Debug, Clone)]
pub struct ColorManagerState {
    /// Supported primaries (`wp_color_manager_v1::primaries` enum values,
    /// NOT CICP): srgb=1, bt2020=6, `display_p3=9`, …
    pub supported_primaries: Vec<u32>,
    /// Supported named transfer functions (`wp_color_manager_v1::transfer_function`
    /// enum values, NOT CICP): bt1886=1, gamma22=2, `st2084_pq=11`, hlg=13, …
    pub supported_transfers: Vec<u32>,
    /// Matrix hint used by the compositor for parametric image
    /// descriptions. `wp_color_manager_v1` does NOT itself have a
    /// "matrixes" event — matrix negotiation lives in the sibling
    /// `color-representation-v1` protocol. We keep the field around
    /// so the flatpak-share encoder can honour the compositor's
    /// preferred matrix on the wire; we don't advertise it through
    /// `color-management-v1`. See `wp_color_manager_v1.xml`.
    pub supported_matrixes: Vec<u32>,
    /// Advertised features (`wp_color_manager_v1::feature`): parametric=1,
    /// `set_primaries=2`, `set_tf_power=3`, `set_luminances=4`,
    /// `set_mastering_display_primaries=5`, ...
    pub supported_features: Vec<u32>,
    /// Advertised render intents. Perceptual (0) is mandatory per spec.
    pub supported_render_intents: Vec<u32>,
    /// The global's id, held so the caller can revoke it when
    /// shutting down (currently unused, but the id is cheap to keep
    /// and it saves a `create_global` return dance).
    pub global: Option<GlobalId>,
}

impl Default for ColorManagerState {
    fn default() -> Self {
        Self {
            // BT.709 / sRGB (=1) and BT.2020 (=6). Display-P3 (=9) is
            // covered by the wgpu backend but only enabled when the
            // output's EDID actually advertises P3 (see the follow-up
            // TODO in ColorManagementOutputData).
            supported_primaries: vec![1, 6],
            // BT.1886 (=1), gamma22 (=2), ST 2084 PQ (=11), HLG (=13).
            // The v1 spec's `srgb` (=9) is deprecated since v2; we
            // don't advertise it — clients get gamma22 or the sibling
            // `ext_srgb` (=10) instead.
            supported_transfers: vec![1, 2, 11, 13],
            // Compositor's preferred matrix set: Identity (=0),
            // BT.709 (=1), BT.2020-non-constant (=9). These are the
            // CICP MatrixCoefficients numbers, not `color-management-v1`
            // enum values.
            supported_matrixes: vec![0, 1, 9],
            // Parametric (=1) — must advertise or clients can't build
            // any parametric image description.
            // set_primaries (=2) — allows explicit CIE-1931 xy chroma.
            // set_tf_power (=3) — allows a pure power-curve transfer;
            //   fed straight into the wgpu shader.
            // set_luminances (=4) — primary volume min/max/reference.
            // set_mastering_display_primaries (=5) — SMPTE ST 2086
            //   mastering-display metadata for HDR pipelines.
            supported_features: vec![1, 2, 3, 4, 5],
            // Perceptual (=0), Relative colorimetric (=1),
            // Media-relative w/ BPC (=4). Saturation (=2) + Absolute
            // (=3) aren't yet plumbed into the wgpu shader.
            supported_render_intents: vec![0, 1, 4],
            global: None,
        }
    }
}

impl ColorManagerState {
    /// Register the `wp_color_manager_v1` global on `display` and return
    /// the initialized state. Callers stash the returned value in the
    /// process-wide compositor state so the delegate dispatch impls
    /// route back to the right advertised-capability list.
    #[must_use]
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<WpColorManagerV1, ()> + 'static,
    {
        let mut state = Self::default();
        let global = display.create_global::<D, WpColorManagerV1, _>(MAX_VERSION, ());
        state.global = Some(global);
        state
    }

    /// Zero-globals constructor for headless tests that never bind a
    /// display. Keeps the advertised-capability defaults but skips
    /// registering the Wayland global.
    #[must_use]
    #[cfg(any(test, feature = "hosted-test-control"))]
    pub fn for_test() -> Self {
        Self::default()
    }
}

// -----------------------------------------------------------------------------
// UserData for each protocol object.
// -----------------------------------------------------------------------------

/// Builder state for `wp_image_description_creator_params_v1`.
///
/// Mirrors the "set exactly once, then `create`" contract of the
/// protocol — every field is `Option` and set-requests reject a
/// second write with `already_set`. On `create` we validate the
/// completeness (primaries + transfer are required), stitch the
/// values into a [`Colorimetry`] + optional [`Hdr10Metadata`], and
/// hand them to the newly-minted `wp_image_description_v1`.
#[derive(Debug, Default)]
pub struct ParametricBuilder {
    inner: Mutex<ParametricBuilderInner>,
}

#[derive(Debug, Default)]
struct ParametricBuilderInner {
    primaries_named: Option<Primaries>,
    /// Explicit CIE 1931 xy primaries (R, G, B, W) — each i32 is
    /// x/y × `1_000_000` per the protocol wire encoding.
    primaries_custom: Option<CustomPrimaries>,
    tf_named: Option<TransferFunction>,
    /// Pure-power transfer exponent × `10_000`. Mutually exclusive with
    /// `tf_named` per the protocol's "set exactly once" rule.
    tf_power_eexp: Option<u32>,
    /// Primary color volume luminance range: (`min_lum` × `10_000`,
    /// `max_lum`, `reference_lum`) — cd/m².
    luminances: Option<(u32, u32, u32)>,
    /// Mastering display primaries in the same wire encoding as
    /// `primaries_custom`.
    mastering_primaries: Option<CustomPrimaries>,
    /// Mastering luminance range (min × `10_000`, max) — cd/m².
    mastering_luminance: Option<(u32, u32)>,
    /// Max content light level — cd/m².
    max_cll: Option<u32>,
    /// Max frame-average light level — cd/m².
    max_fall: Option<u32>,
    /// Set once `create` runs so a second `create` is caught cleanly.
    consumed: bool,
}

#[derive(Debug, Clone, Copy)]
struct CustomPrimaries {
    r: (i32, i32),
    g: (i32, i32),
    b: (i32, i32),
    w: (i32, i32),
}

impl ParametricBuilder {
    /// Assemble the pieces into a settled image description. Returns
    /// `None` when the required-field set is incomplete.
    fn build(&self) -> Option<ImageDescriptionData> {
        // Copy the (Copy-typed) fields out under the lock, then release it —
        // everything below reads only these local copies, not `inner`.
        let (
            primaries_named,
            primaries_custom,
            tf_named,
            tf_power_eexp,
            mastering_primaries,
            mastering_luminance,
            max_cll,
            max_fall,
        ) = {
            let inner = self.inner.lock().ok()?;
            (
                inner.primaries_named,
                inner.primaries_custom,
                inner.tf_named,
                inner.tf_power_eexp,
                inner.mastering_primaries,
                inner.mastering_luminance,
                inner.max_cll,
                inner.max_fall,
            )
        };
        // Required: primaries + transfer.
        let primaries = match (primaries_named, primaries_custom) {
            (Some(p), _) => primaries_named_to_color(p),
            (None, Some(c)) => guess_primaries_from_custom(c),
            (None, None) => return None,
        };
        let transfer = match (tf_named, tf_power_eexp) {
            (Some(t), _) => transfer_named_to_color(t),
            (None, Some(exp)) => match exp {
                // Common named power curves.
                22_000 => TransferCharacteristics::Bt709,
                _ => TransferCharacteristics::Unspecified,
            },
            (None, None) => return None,
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
        let hdr = if transfer.is_hdr() {
            let mastering = mastering_primaries.map_or_else(
                MasteringDisplayColorVolume::hdr10_reference,
                |m| MasteringDisplayColorVolume {
                    display_primaries: [
                        (chrom_micro_to_st2086(m.r.0), chrom_micro_to_st2086(m.r.1)),
                        (chrom_micro_to_st2086(m.g.0), chrom_micro_to_st2086(m.g.1)),
                        (chrom_micro_to_st2086(m.b.0), chrom_micro_to_st2086(m.b.1)),
                    ],
                    white_point: (chrom_micro_to_st2086(m.w.0), chrom_micro_to_st2086(m.w.1)),
                    max_display_mastering_luminance: mastering_luminance
                        .map_or(10_000_000, |(_, max)| max.saturating_mul(10_000)),
                    min_display_mastering_luminance: mastering_luminance
                        .map_or(500, |(min, _)| min),
                },
            );
            let light = ContentLightLevel {
                max_content_light_level: max_cll.map_or(0, clamp_to_u16),
                max_frame_average_light_level: max_fall.map_or(0, clamp_to_u16),
            };
            Some(Hdr10Metadata { mastering, light })
        } else {
            None
        };

        Some(ImageDescriptionData {
            colorimetry,
            hdr,
            identity: mint_identity(),
        })
    }

    /// Mark the builder as consumed so the second `create` request
    /// can be caught with `already_set`.
    fn mark_consumed(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.consumed = true;
        }
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

fn primaries_named_to_color(p: Primaries) -> ColorPrimaries {
    match p {
        Primaries::Srgb => ColorPrimaries::Bt709,
        Primaries::Bt2020 => ColorPrimaries::Bt2020,
        Primaries::DisplayP3 => ColorPrimaries::DisplayP3,
        // Unsupported / uncommon primaries — treat as unspecified so
        // the encoder falls back to BT.709 defaults rather than
        // crashing.
        _ => ColorPrimaries::Unspecified,
    }
}

fn transfer_named_to_color(t: TransferFunction) -> TransferCharacteristics {
    match t {
        TransferFunction::Bt1886 | TransferFunction::Gamma22 => TransferCharacteristics::Bt709,
        TransferFunction::St2084Pq => TransferCharacteristics::SmpteSt2084,
        TransferFunction::Hlg => TransferCharacteristics::AribStdB67,
        TransferFunction::Srgb | TransferFunction::ExtSrgb => TransferCharacteristics::Srgb,
        _ => TransferCharacteristics::Unspecified,
    }
}

fn guess_primaries_from_custom(c: CustomPrimaries) -> ColorPrimaries {
    // Red primary > 0.680 → almost certainly BT.2020 (0.708).
    if c.r.0 > 680_000 {
        ColorPrimaries::Bt2020
    } else if c.r.0 > 650_000 {
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
    /// Compositor-wide unique id number returned via `ready2`. Kept
    /// as u64 even though the pre-v2 `ready` event only exposes the
    /// low 32 bits — see below.
    pub identity: u64,
}

impl ImageDescriptionData {
    /// Low 32 bits of the identity for the pre-v2 `ready` event.
    #[must_use]
    pub fn identity_lo(&self) -> u32 {
        (self.identity & 0xFFFF_FFFF) as u32
    }

    /// High 32 bits of the identity for the `ready2` event.
    #[must_use]
    pub fn identity_hi(&self) -> u32 {
        (self.identity >> 32) as u32
    }
}

/// Per-surface color-management state.
///
/// Sits in `SurfaceData::data_map`. The client attaches an image
/// description via `set_image_description`; the value goes into
/// [`Self::pending`]. On `wl_surface.commit` the compositor flips
/// [`Self::pending`] → [`Self::current`].
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

    fn set_current(&self, data: ImageDescriptionData) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.current = Some(data);
        }
    }

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
    // Retained, not yet read: `get_image_description` still answers from
    // a synthesised profile pending `wayland::edid::parse_hdr_display_info`.
    #[allow(dead_code)]
    output: WlOutput,
    /// Colorimetry the compositor tells clients this output actually
    /// displays. `default_sdr` on winit / headless; populated from the
    /// EDID HDR block on DRM. Held by `Arc<Mutex>` so the DRM
    /// backend can push updates without minting a new proxy.
    color: Arc<Mutex<Colorimetry>>,
    hdr: Arc<Mutex<Option<Hdr10Metadata>>>,
}

impl ColorManagementOutputData {
    /// Attach a fresh output-side descriptor. Defaults to SDR BT.709;
    /// the DRM backend calls [`Self::set_display_color`] when an EDID
    /// arrives.
    #[must_use]
    pub fn new(output: WlOutput) -> Self {
        Self {
            output,
            color: Arc::new(Mutex::new(Colorimetry::sdr_bt709())),
            hdr: Arc::new(Mutex::new(None)),
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
        let color = self.color.lock().map(|c| *c).unwrap_or_default();
        let hdr = self.hdr.lock().ok().and_then(|h| *h);
        ImageDescriptionData {
            colorimetry: color,
            hdr,
            identity: mint_identity(),
        }
    }
}

/// User-data attached to a `wp_color_management_surface_v1`. Holds
/// the target `wl_surface` so subsequent set/unset requests know
/// which surface to mutate.
#[derive(Debug, Clone)]
pub struct ColorManagementSurfaceUserData {
    // Retained, not yet read: `get_image_description` still answers from
    // a synthesised profile pending `wayland::edid::parse_hdr_display_info`.
    #[allow(dead_code)]
    surface: WlSurface,
}

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
pub struct ColorManagementSurfaceFeedbackData {
    // Retained, not yet read: `get_image_description` still answers from
    // a synthesised profile pending `wayland::edid::parse_hdr_display_info`.
    #[allow(dead_code)]
    surface: WlSurface,
}

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

impl<D> GlobalDispatch<WpColorManagerV1, (), D> for ColorManagerState
where
    D: GlobalDispatch<WpColorManagerV1, ()>
        + Dispatch<WpColorManagerV1, ()>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParametricBuilder>>
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
        // Advertise the capability set. Order matters only in as much
        // as spec-mandated events (perceptual render intent) precede
        // optional ones.
        let cfg = state.as_ref().clone();
        for intent in cfg.supported_render_intents {
            if let Ok(ri) = RenderIntent::try_from(intent) {
                let () = manager.supported_intent(ri);
            }
        }
        for feat in cfg.supported_features {
            if let Ok(f) = Feature::try_from(feat) {
                let () = manager.supported_feature(f);
            }
        }
        for prim in cfg.supported_primaries {
            if let Ok(p) = Primaries::try_from(prim) {
                let () = manager.supported_primaries_named(p);
            }
        }
        for tf in cfg.supported_transfers {
            if let Ok(t) = TransferFunction::try_from(tf) {
                let () = manager.supported_tf_named(t);
            }
        }
        let () = manager.done();
    }
}

impl<D> Dispatch<WpColorManagerV1, (), D> for ColorManagerState
where
    D: Dispatch<WpColorManagerV1, ()>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParametricBuilder>>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceUserData>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData>
        + Dispatch<WpColorManagementOutputV1, ColorManagementOutputData>
        + 'static,
{
    fn request(
        _state: &mut D,
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
            }
            wp_color_manager_v1::Request::CreateIccCreator { obj } => {
                // We don't advertise the icc_v2_v4 feature — clients
                // that call this are asking for something we said we
                // don't support. Post the mandated protocol error.
                let _ = obj;
                manager.post_error(
                    wp_color_manager_v1::Error::UnsupportedFeature,
                    "ICC-based image descriptions are not supported",
                );
            }
            wp_color_manager_v1::Request::CreateWindowsScrgb { image_description } => {
                let _ = image_description;
                manager.post_error(
                    wp_color_manager_v1::Error::UnsupportedFeature,
                    "Windows-scRGB image descriptions are not supported",
                );
            }
            wp_color_manager_v1::Request::GetOutput { id, output } => {
                let data = ColorManagementOutputData::new(output);
                data_init.init(id, data);
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
                data_init.init(id, ColorManagementSurfaceFeedbackData::new(surface));
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

impl<D> Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParametricBuilder>, D> for ColorManagerState
where
    D: Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParametricBuilder>>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + 'static,
{
    // The local `set_once!` macro and the `Create` arm's body were already split
    // out below (`set_builder_field_once`/`create_image_description`); what's left
    // is one match arm per wire request variant, which stays a 1:1 dispatch table
    // rather than fragmenting further.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per WpImageDescriptionCreatorParamsV1 request variant; \
                  the two heaviest chunks are already split into named helpers"
    )]
    fn request(
        _state: &mut D,
        _client: &Client,
        creator: &WpImageDescriptionCreatorParamsV1,
        request: <WpImageDescriptionCreatorParamsV1 as Resource>::Request,
        builder: &Arc<ParametricBuilder>,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_image_description_creator_params_v1::Request::Create { image_description } => {
                create_image_description(creator, builder, image_description, data_init);
            }
            wp_image_description_creator_params_v1::Request::SetTfNamed { tf } => {
                let Ok(tf) = tf.into_result() else {
                    creator.post_error(
                        wp_image_description_creator_params_v1::Error::InvalidTf,
                        "unknown transfer_function enum value",
                    );
                    return;
                };
                set_builder_field_once(builder, creator, "transfer function already set", |i| {
                    set_if_none(&mut i.tf_named, tf)
                });
            }
            wp_image_description_creator_params_v1::Request::SetTfPower { eexp } => {
                set_builder_field_once(builder, creator, "transfer function already set", |i| {
                    set_if_none(&mut i.tf_power_eexp, eexp)
                });
            }
            wp_image_description_creator_params_v1::Request::SetPrimariesNamed { primaries } => {
                let Ok(primaries) = primaries.into_result() else {
                    creator.post_error(
                        wp_image_description_creator_params_v1::Error::InvalidPrimariesNamed,
                        "unknown primaries enum value",
                    );
                    return;
                };
                set_builder_field_once(builder, creator, "primaries already set", |i| {
                    set_if_none(&mut i.primaries_named, primaries)
                });
            }
            wp_image_description_creator_params_v1::Request::SetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                let custom = CustomPrimaries {
                    r: (r_x, r_y),
                    g: (g_x, g_y),
                    b: (b_x, b_y),
                    w: (w_x, w_y),
                };
                set_builder_field_once(builder, creator, "primaries already set", |i| {
                    set_if_none(&mut i.primaries_custom, custom)
                });
            }
            wp_image_description_creator_params_v1::Request::SetLuminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                if max_lum <= min_lum / 10_000 || reference_lum == 0 {
                    // Loose validation — spec's tighter check is left
                    // to the wire; we accept anything sensible.
                }
                set_builder_field_once(builder, creator, "luminances already set", |i| {
                    set_if_none(&mut i.luminances, (min_lum, max_lum, reference_lum))
                });
            }
            wp_image_description_creator_params_v1::Request::SetMasteringDisplayPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                let custom = CustomPrimaries {
                    r: (r_x, r_y),
                    g: (g_x, g_y),
                    b: (b_x, b_y),
                    w: (w_x, w_y),
                };
                set_builder_field_once(
                    builder,
                    creator,
                    "mastering display primaries already set",
                    |i| set_if_none(&mut i.mastering_primaries, custom),
                );
            }
            wp_image_description_creator_params_v1::Request::SetMasteringLuminance {
                min_lum,
                max_lum,
            } => {
                set_builder_field_once(builder, creator, "mastering luminance already set", |i| {
                    set_if_none(&mut i.mastering_luminance, (min_lum, max_lum))
                });
            }
            wp_image_description_creator_params_v1::Request::SetMaxCll { max_cll } => {
                set_builder_field_once(builder, creator, "max_cll already set", |i| {
                    set_if_none(&mut i.max_cll, max_cll)
                });
            }
            wp_image_description_creator_params_v1::Request::SetMaxFall { max_fall } => {
                set_builder_field_once(builder, creator, "max_fall already set", |i| {
                    set_if_none(&mut i.max_fall, max_fall)
                });
            }
            _ => {}
        }
    }
}

/// `true` and sets `*slot` if it was `None`; `false` (no write) if the
/// caller had already set it. The `bool` is what
/// [`set_builder_field_once`] uses to decide whether to post `AlreadySet`.
fn set_if_none<T>(slot: &mut Option<T>, val: T) -> bool {
    if slot.is_some() {
        return false;
    }
    *slot = Some(val);
    true
}

/// Guarded field-set for `ParametricBuilder`: `set` reports via its `bool`
/// return whether the field was free to take. Posts the matching protocol
/// error — `IncompleteSet` if the mutex is poisoned, `AlreadySet` for a
/// consumed builder or a field already written — instead of overwriting
/// what the client set first, mirroring the spec. Split out of the
/// `WpImageDescriptionCreatorParamsV1` dispatch (formerly a
/// `macro_rules!` defined inline) to keep that match short.
fn set_builder_field_once(
    builder: &ParametricBuilder,
    creator: &WpImageDescriptionCreatorParamsV1,
    msg: &str,
    set: impl FnOnce(&mut ParametricBuilderInner) -> bool,
) {
    let Ok(mut inner) = builder.inner.lock() else {
        creator.post_error(
            wp_image_description_creator_params_v1::Error::IncompleteSet,
            "builder poisoned",
        );
        return;
    };
    if inner.consumed {
        creator.post_error(
            wp_image_description_creator_params_v1::Error::AlreadySet,
            "builder already consumed",
        );
        return;
    }
    if !set(&mut inner) {
        creator.post_error(
            wp_image_description_creator_params_v1::Error::AlreadySet,
            msg,
        );
    }
}

/// `Create` request: builds the final [`ImageDescriptionData`] and
/// advertises it via `ready2`/`ready`, or posts `IncompleteSet` when a
/// required field is still missing. Split out of the
/// `WpImageDescriptionCreatorParamsV1` dispatch to keep that match short.
fn create_image_description<D>(
    creator: &WpImageDescriptionCreatorParamsV1,
    builder: &Arc<ParametricBuilder>,
    image_description: New<WpImageDescriptionV1>,
    data_init: &mut DataInit<'_, D>,
) where
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    match builder.build() {
        Some(data) => {
            let identity = data.identity;
            let obj = data_init.init(image_description, data);
            // Advertise the modern `ready2` identity + the
            // legacy `ready` identity so v1 clients also
            // observe the object as usable.
            let () = obj.ready2((identity >> 32) as u32, (identity & 0xFFFF_FFFF) as u32);
            builder.mark_consumed();
        }
        None => {
            creator.post_error(
                wp_image_description_creator_params_v1::Error::IncompleteSet,
                "primaries + transfer are required before create",
            );
        }
    }
}

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
        _data: &ImageDescriptionData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        if let wp_image_description_v1::Request::GetInformation { information } = request {
            // Parametric descriptions from our creator DO NOT
            // permit get_information per spec (see the request's
            // documentation in the XML). Follow the spec: post
            // the `no_information` error.
            let _ = information;
            obj.post_error(
                wp_image_description_v1::Error::NoInformation,
                "get_information is not allowed on this image description",
            );
        }
    }
}

impl<D> Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceUserData, D>
    for ColorManagerState
where
    D: Dispatch<WpColorManagementSurfaceV1, ColorManagementSurfaceUserData> + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        obj: &WpColorManagementSurfaceV1,
        request: <WpColorManagementSurfaceV1 as Resource>::Request,
        data: &ColorManagementSurfaceUserData,
        _dh: &DisplayHandle,
        _init: &mut DataInit<'_, D>,
    ) {
        // If the wl_surface was destroyed the object is inert.
        if !data.surface.is_alive() {
            if let wp_color_management_surface_v1::Request::Destroy = request {
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
                let _ = render_intent;
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

impl<D> Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData, D>
    for ColorManagerState
where
    D: Dispatch<WpColorManagementSurfaceFeedbackV1, ColorManagementSurfaceFeedbackData>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + 'static,
{
    fn request(
        _state: &mut D,
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
                let desc = ImageDescriptionData {
                    colorimetry: Colorimetry::sdr_bt709(),
                    hdr: None,
                    identity: mint_identity(),
                };
                let identity_hi = desc.identity_hi();
                let identity_lo = desc.identity_lo();
                let obj = data_init.init(image_description, desc);
                let () = obj.ready2(identity_hi, identity_lo);
            }
            _ => {}
        }
    }
}

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
            let identity_hi = desc.identity_hi();
            let identity_lo = desc.identity_lo();
            let obj = data_init.init(image_description, desc);
            let () = obj.ready2(identity_hi, identity_lo);
        }
    }
}

// `wp_image_description_info_v1` is only reachable if we permit
// `get_information` — which we don't (parametric descriptions raise
// `no_information`). Still provide the dispatch impl so the type
// system's delegation macros are satisfied.
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
#[macro_export]
macro_rules! delegate_color_management {
    ($ty:ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: ()
            ] => $crate::wayland::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: ()
            ] => $crate::wayland::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1:
                    std::sync::Arc<$crate::wayland::color_management::ParametricBuilder>
            ] => $crate::wayland::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_image_description_v1::WpImageDescriptionV1:
                    $crate::wayland::color_management::ImageDescriptionData
            ] => $crate::wayland::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_v1::WpColorManagementSurfaceV1:
                    $crate::wayland::color_management::ColorManagementSurfaceUserData
            ] => $crate::wayland::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1:
                    $crate::wayland::color_management::ColorManagementSurfaceFeedbackData
            ] => $crate::wayland::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_color_management_output_v1::WpColorManagementOutputV1:
                    $crate::wayland::color_management::ColorManagementOutputData
            ] => $crate::wayland::color_management::ColorManagerState
        );
        smithay::reexports::wayland_server::delegate_dispatch!($ty:
            [
                wayland_protocols::wp::color_management::v1::server::wp_image_description_info_v1::WpImageDescriptionInfoV1:
                    $crate::wayland::color_management::ImageDescriptionData
            ] => $crate::wayland::color_management::ColorManagerState
        );
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_capabilities_include_bt2020_and_pq() {
        let cfg = ColorManagerState::default();
        assert!(
            cfg.supported_primaries
                .contains(&(Primaries::Bt2020 as u32))
        );
        assert!(cfg.supported_primaries.contains(&(Primaries::Srgb as u32)));
        assert!(
            cfg.supported_transfers
                .contains(&(TransferFunction::St2084Pq as u32))
        );
        assert!(
            cfg.supported_transfers
                .contains(&(TransferFunction::Hlg as u32))
        );
        assert!(
            cfg.supported_features
                .contains(&(Feature::Parametric as u32))
        );
        assert!(
            cfg.supported_features
                .contains(&(Feature::SetPrimaries as u32))
        );
    }

    #[test]
    fn builder_yields_bt2020_pq_when_configured() {
        let b = ParametricBuilder::default();
        {
            let mut inner = b.inner.lock().unwrap();
            inner.primaries_named = Some(Primaries::Bt2020);
            inner.tf_named = Some(TransferFunction::St2084Pq);
        }
        let desc = b.build().expect("builder produces description");
        assert_eq!(desc.colorimetry.primaries, ColorPrimaries::Bt2020);
        assert_eq!(
            desc.colorimetry.transfer,
            TransferCharacteristics::SmpteSt2084
        );
        assert!(desc.hdr.is_some());
    }

    #[test]
    fn builder_returns_none_on_missing_required_fields() {
        let b = ParametricBuilder::default();
        assert!(b.build().is_none());
        {
            b.inner.lock().unwrap().primaries_named = Some(Primaries::Srgb);
        }
        // Still missing transfer.
        assert!(b.build().is_none());
    }

    #[test]
    fn hdr_metadata_only_populated_for_hdr_transfer() {
        let b = ParametricBuilder::default();
        {
            let mut inner = b.inner.lock().unwrap();
            inner.primaries_named = Some(Primaries::Srgb);
            inner.tf_named = Some(TransferFunction::Bt1886);
        }
        let desc = b.build().unwrap();
        assert!(desc.hdr.is_none(), "SDR builder must not emit HDR metadata");
    }
}
