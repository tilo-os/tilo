//! `wp_color_management_v1`: what the outputs display and what a client's
//! content is in.
//!
//! smithay 0.7 ships no handler for this staging protocol, so its server side
//! is hand-rolled in `tilo_wayland_ext::color_management`; this module
//! advertises the global from the compositor, tells clients what each output
//! displays, and leaves the description a client attached to a surface where
//! the render pass finds it.
//!
//! A client learns an output's description (`wp_color_management_output_v1`)
//! and the one its surfaces are asked to render in
//! (`wp_color_management_surface_feedback_v1`), and attaches the description
//! its own content is encoded in (`wp_color_management_surface_v1`). The
//! attached description stays on the surface as
//! [`tilo_wayland_ext::color_management::ColorManagementSurfaceData`], which
//! [`colorimetry_of`] reads back for whoever draws that surface.
//!
//! Nothing here converts pixels yet: the description a window carries is kept
//! for the colour pass and for what a share stream says about the frames it
//! sends.

use neighbour_wire::colorimetry::Colorimetry;
use neighbour_wire::hdr_metadata::Hdr10Metadata;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use tilo_wayland_ext::color_management::{ColorManagerState, OutputDescription};
use tilo_wayland_ext::edid::HdrDisplayInfo;

use crate::config::Hdr;
use crate::state::State;

impl AsRef<ColorManagerState> for State {
    fn as_ref(&self) -> &ColorManagerState {
        &self.protocols.color_manager
    }
}

// `wp_color_manager_v1` and its objects are hand-rolled outside smithay's
// delegate wrappers; this wires their `Dispatch`/`GlobalDispatch` impls onto
// the compositor's state.
tilo_wayland_ext::delegate_color_management!(State);

/// The image description the client of `surface` attached: the colour its
/// content is encoded in, which a 10-bit or YUV buffer is converted from.
pub(crate) fn colorimetry_of(surface: &WlSurface) -> Option<(Colorimetry, Option<Hdr10Metadata>)> {
    tilo_wayland_ext::color_management::surface_current_colorimetry(surface)
}

/// The description matching the HDR signal `hdr`: PQ with the volume the
/// display reports, SDR white where the person put it.
fn description(hdr: &Hdr) -> OutputDescription {
    OutputDescription::hdr_pq(
        &HdrDisplayInfo {
            display_primaries: hdr.primaries,
            white_point: hdr.white_point,
            max_luminance_cd_m2: hdr.max_nits,
            max_frame_average_luminance_cd_m2: hdr.max_frame_average_nits,
            min_luminance_micro_cd_m2: hdr.min_luminance_micro_nits,
            ..HdrDisplayInfo::default_sdr()
        },
        hdr.sdr_white_nits,
    )
}

impl State {
    /// The colour the client of the window `id` says its content is in: the
    /// image description it attached through
    /// `wp_color_management_surface_v1`, or `None` when it attached none.
    pub(crate) fn window_color(&self, id: u64) -> Option<(Colorimetry, Option<Hdr10Metadata>)> {
        let window = self.space.elements().find(|window| {
            crate::state::describe(&self.display, &self.icons, window)
                .is_some_and(|info| info.id == id)
        })?;
        colorimetry_of(window.toplevel()?.wl_surface())
    }
}

impl crate::Compositor {
    /// Tell colour-managed clients what each output displays: the HDR signal
    /// of an output driven in HDR, SDR otherwise. Surfaces are asked to
    /// render in HDR only while every output is HDR, since an SDR output can
    /// show an HDR surface only tone-mapped.
    ///
    /// Clients are told only what changed: an output's description (hotplug,
    /// an HDR toggle) or a description a new output object has not heard yet.
    pub(crate) fn publish_output_colors(&self) {
        let state = &self.data.state;
        let described: Vec<(Output, Option<OutputDescription>)> = state
            .space
            .outputs()
            .map(|output| {
                (
                    output.clone(),
                    self.data
                        .backend
                        .output_hdr(output)
                        .map(|hdr| description(&hdr)),
                )
            })
            .collect();
        let manager = &state.protocols.color_manager;
        manager.update_outputs(|resource| {
            let output = Output::from_resource(resource)?;
            described
                .iter()
                .find_map(|(known, told)| (*known == output).then_some(*told))
                .flatten()
        });
        let preferred = match described.as_slice() {
            [(_, Some(first)), rest @ ..] if rest.iter().all(|(_, told)| told.is_some()) => *first,
            _ => OutputDescription::sdr(),
        };
        manager.set_preferred(preferred);
    }
}
