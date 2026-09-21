//! What changes when a display is plugged in or pulled out.
//!
//! The udev source hands the backend three edges — a DRM device appeared,
//! changed, or went away — and each means "re-ask the hardware what is
//! connected, then reconcile": which connectors are new (modeset), which
//! live outputs are gone (tear down), which are the same connector at a
//! different mode (re-modeset), where every surviving output sits in the
//! combined coordinate space, and where the pointer ends up when that
//! space shrank. This module computes it; [`super::runtime`] performs it.
//!
//! Tilo does not pin a window to an output — a workspace holds strips, a
//! strip holds groups, and the shell places those groups across whatever
//! displays it is told about. So removing a display re-flows the same
//! groups onto the remaining ones on the next layout pass rather than
//! leaving anything at coordinates with no panel behind them. Only the
//! pointer ([`clamp_pointer_to_outputs`]) and the surviving outputs' packing
//! ([`layout`]) need computing explicitly.

use crate::compositor::backend::OutputId;

use super::outputs::DrmOutputKey;

/// An output the backend currently has a live scanout for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LiveOutput {
    pub key: DrmOutputKey,
    pub id: OutputId,
    pub size: (u32, u32),
}

/// A connector the hardware currently reports as connected, with the mode
/// the backend would drive it at.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ConnectedOutput {
    pub key: DrmOutputKey,
    pub size: (u32, u32),
    /// Physical panel size in millimetres, when the connector reports one.
    /// Feeds [`super::scale::derive_scale`].
    pub mm: Option<(u32, u32)>,
}

/// The work one hotplug edge implies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HotplugPlan {
    /// Connectors that became connected: modeset and register them.
    pub added: Vec<ConnectedOutput>,
    /// Outputs whose connector went away: tear the scanout down and
    /// destroy the `wl_output` global.
    pub removed: Vec<OutputId>,
    /// Outputs still present but at a different mode: re-modeset in place
    /// (keeping the `OutputId`, so clients see a mode change rather than a
    /// disappearance followed by a new display).
    pub remoded: Vec<(OutputId, ConnectedOutput)>,
}

impl HotplugPlan {
    /// Whether anything at all has to happen. A `Changed` udev event for a
    /// connector whose state did not actually move is the common case —
    /// every DPMS transition and every EDID re-read raises one — and
    /// re-modesetting on those would flicker the panel for nothing.
    pub fn is_noop(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.remoded.is_empty()
    }
}

/// Diff the live outputs against what the hardware now reports.
///
/// Matching is by `(device, connector)` — the same key the
/// [`OutputRegistry`](super::outputs::OutputRegistry) interns — so an
/// output that survives a hotplug keeps its `OutputId` and, with it, its
/// `wl_output` global and its identity to clients.
pub fn plan(live: &[LiveOutput], connected: &[ConnectedOutput]) -> HotplugPlan {
    let mut out = HotplugPlan::default();
    for c in connected {
        match live.iter().find(|l| l.key == c.key) {
            None => out.added.push(*c),
            Some(l) if l.size != c.size => out.remoded.push((l.id, *c)),
            Some(_) => {}
        }
    }
    for l in live {
        if !connected.iter().any(|c| c.key == l.key) {
            out.removed.push(l.id);
        }
    }
    out
}

/// Lay output sizes out left-to-right at `y = 0`: each output's left edge
/// is the sum of the prior outputs' widths.
///
/// Re-packing (rather than leaving the survivors where they were) is what
/// closes the hole an unplugged middle monitor leaves. A gap in the
/// combined space is a rectangle the compositor paints, no panel shows,
/// and the pointer can get lost in.
pub fn layout(sizes: &[(u32, u32)]) -> Vec<(i32, i32)> {
    let mut next_x: i32 = 0;
    let mut out = Vec::with_capacity(sizes.len());
    for &(w, _h) in sizes {
        out.push((next_x, 0));
        // Widths come from the connector's mode list; clamp rather than
        // wrap or panic on the (unreachable in practice) case of a
        // driver-reported width past `i32::MAX`.
        let w = i32::try_from(w).unwrap_or(i32::MAX);
        next_x = next_x.saturating_add(w);
    }
    out
}

/// The bounding box of a laid-out output set — the extent of the single
/// scene the compositor paints.
pub fn combined_extent(positions: &[(i32, i32)], sizes: &[(u32, u32)]) -> (i32, i32) {
    positions
        .iter()
        .zip(sizes)
        .fold((0i32, 0i32), |(w, h), (&(x, y), &(sw, sh))| {
            let sw = i32::try_from(sw).unwrap_or(i32::MAX);
            let sh = i32::try_from(sh).unwrap_or(i32::MAX);
            (w.max(x.saturating_add(sw)), h.max(y.saturating_add(sh)))
        })
}

/// Keep a pointer on the union of the live output rectangles.
///
/// Clamping only to the combined bounding box is insufficient once display
/// settings can describe vertical offsets or an L-shaped monitor island: the
/// bounding box contains dead rectangles which no connector scans out.  A
/// point already on an output is preserved.  Otherwise it is projected onto
/// the nearest addressable pixel of any output, with input order breaking an
/// exact tie (the runtime orders the configured primary first).
///
/// Empty and zero-sized output sets park the pointer at the origin.  A
/// non-finite coordinate is treated the same way rather than allowing NaN to
/// poison cursor routing and Wayland input events.
pub fn clamp_pointer_to_outputs(
    pointer: (f64, f64),
    outputs: impl IntoIterator<Item = ((i32, i32), (u32, u32))>,
) -> (f64, f64) {
    let pointer = if pointer.0.is_finite() && pointer.1.is_finite() {
        pointer
    } else {
        (0.0, 0.0)
    };

    let mut nearest: Option<((f64, f64), f64)> = None;
    for ((x, y), (width, height)) in outputs {
        if width == 0 || height == 0 {
            continue;
        }
        let min_x = f64::from(x);
        let min_y = f64::from(y);
        let max_x = min_x + f64::from(width.saturating_sub(1));
        let max_y = min_y + f64::from(height.saturating_sub(1));
        let candidate = (pointer.0.clamp(min_x, max_x), pointer.1.clamp(min_y, max_y));
        if candidate == pointer {
            return pointer;
        }
        let dx = pointer.0 - candidate.0;
        let dy = pointer.1 - candidate.1;
        let distance_squared = dx.mul_add(dx, dy * dy);
        if nearest
            .as_ref()
            .is_none_or(|(_, nearest_distance)| distance_squared < *nearest_distance)
        {
            nearest = Some((candidate, distance_squared));
        }
    }
    nearest.map_or((0.0, 0.0), |(point, _)| point)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(device: u64, connector: u32) -> DrmOutputKey {
        DrmOutputKey { device, connector }
    }

    fn live(device: u64, connector: u32, id: u32, size: (u32, u32)) -> LiveOutput {
        LiveOutput {
            key: key(device, connector),
            id: OutputId(id),
            size,
        }
    }

    fn conn(device: u64, connector: u32, size: (u32, u32)) -> ConnectedOutput {
        ConnectedOutput {
            key: key(device, connector),
            size,
            mm: None,
        }
    }

    #[test]
    fn nothing_changed_is_a_noop() {
        // The common `Changed` edge: a DPMS cycle or an EDID re-read. Any
        // work here would flicker the panel for no reason.
        let l = [live(1, 10, 0, (1920, 1080))];
        let c = [conn(1, 10, (1920, 1080))];
        let p = plan(&l, &c);
        assert!(p.is_noop(), "unchanged hardware must imply no work: {p:?}");
    }

    #[test]
    fn plugging_in_an_external_display_is_an_addition() {
        // The laptop panel is live; a monitor is plugged into HDMI.
        let l = [live(1, 10, 0, (1920, 1080))];
        let c = [conn(1, 10, (1920, 1080)), conn(1, 11, (2560, 1440))];
        let p = plan(&l, &c);
        assert_eq!(p.added, vec![conn(1, 11, (2560, 1440))]);
        assert!(p.removed.is_empty());
        assert!(p.remoded.is_empty());
    }

    #[test]
    fn unplugging_a_display_removes_exactly_that_output_id() {
        let l = [live(1, 10, 0, (1920, 1080)), live(1, 11, 1, (2560, 1440))];
        let c = [conn(1, 10, (1920, 1080))];
        let p = plan(&l, &c);
        assert_eq!(p.removed, vec![OutputId(1)]);
        assert!(
            p.added.is_empty(),
            "the surviving panel must not be re-added"
        );
        assert!(p.remoded.is_empty());
    }

    #[test]
    fn a_mode_change_keeps_the_output_id() {
        // A monitor renegotiating its mode must read as the same display
        // at a new size, not as an unplug followed by a plug — otherwise
        // its `wl_output` global churns and every client re-lays-out from
        // scratch.
        let l = [live(1, 10, 7, (1920, 1080))];
        let c = [conn(1, 10, (3840, 2160))];
        let p = plan(&l, &c);
        assert!(p.added.is_empty());
        assert!(p.removed.is_empty());
        assert_eq!(p.remoded, vec![(OutputId(7), conn(1, 10, (3840, 2160)))]);
    }

    #[test]
    fn a_second_gpu_does_not_collide_with_the_first_on_connector_ids() {
        // Connector handles restart per card, so keying on the handle
        // alone would make card1's connector 10 look like card0's.
        let l = [live(1, 10, 0, (1920, 1080))];
        let c = [conn(1, 10, (1920, 1080)), conn(2, 10, (1280, 1024))];
        let p = plan(&l, &c);
        assert_eq!(p.added, vec![conn(2, 10, (1280, 1024))]);
        assert!(p.removed.is_empty());
    }

    #[test]
    fn fake_hybrid_cards_replug_without_disturbing_the_other_gpu() {
        // Deterministic fake connector snapshots for a normal hybrid laptop:
        // the internal panel belongs to one card and the dock connector to
        // another.  Both happen to use DRM connector handle 10, which is why
        // the `(device, connector)` identity is load-bearing.
        let panel = conn(101, 10, (1920, 1080));
        let dock = conn(202, 10, (2560, 1440));
        let mut registry = super::super::outputs::OutputRegistry::new();
        let panel_id = registry.intern(panel.key);
        let dock_id = registry.intern(dock.key);
        assert_eq!((panel_id, dock_id), (OutputId(0), OutputId(1)));
        let live_outputs = [
            live(101, 10, panel_id.0, (1920, 1080)),
            live(202, 10, dock_id.0, (2560, 1440)),
        ];

        let unplug = plan(&live_outputs, &[panel]);
        assert_eq!(unplug.removed, vec![OutputId(1)]);
        assert!(unplug.added.is_empty());
        assert!(unplug.remoded.is_empty());

        // The panel remains live throughout the dock cycle, so its wl_output
        // identity is untouched.  When the connector returns, the runtime
        // asks its OutputRegistry for the existing key and restores id 1.
        assert_eq!(registry.intern(dock.key), dock_id);
        let after_unplug = [live(101, 10, panel_id.0, (1920, 1080))];
        let replug = plan(&after_unplug, &[panel, dock]);
        assert_eq!(replug.added, vec![dock]);
        assert!(replug.removed.is_empty());
        assert!(replug.remoded.is_empty());
    }

    #[test]
    fn usb_c_dp_alt_mode_replug_keeps_its_output_identity() {
        // The kernel exposes DP Alt Mode as an ordinary DP connector. Its
        // connector handle survives a cable cycle on the same DRM card, so
        // the output registry must restore the same wl_output identity.
        let panel = conn(101, 10, (1920, 1080));
        let usb_c = conn(101, 42, (2560, 1440));
        let mut registry = super::super::outputs::OutputRegistry::new();
        let panel_id = registry.intern(panel.key);
        let usb_c_id = registry.intern(usb_c.key);
        let live_outputs = [
            live(101, 10, panel_id.0, panel.size),
            live(101, 42, usb_c_id.0, usb_c.size),
        ];

        let unplug = plan(&live_outputs, &[panel]);
        assert_eq!(unplug.removed, vec![usb_c_id]);
        assert_eq!(registry.intern(usb_c.key), usb_c_id);

        let replug = plan(&[live(101, 10, panel_id.0, panel.size)], &[panel, usb_c]);
        assert_eq!(replug.added, vec![usb_c]);
        assert!(replug.removed.is_empty());
    }

    #[test]
    fn everything_unplugged_removes_everything() {
        // A dock pulled out. All outputs go; the compositor keeps serving
        // its socket headless rather than exiting.
        let l = [live(1, 10, 0, (1920, 1080)), live(1, 11, 1, (2560, 1440))];
        let p = plan(&l, &[]);
        assert_eq!(p.removed, vec![OutputId(0), OutputId(1)]);
        assert!(p.added.is_empty());
    }

    #[test]
    fn the_first_display_of_a_headless_session_is_an_addition() {
        let p = plan(&[], &[conn(1, 10, (1920, 1080))]);
        assert_eq!(p.added.len(), 1);
        assert!(p.removed.is_empty());
    }

    #[test]
    fn layout_places_a_single_output_at_the_origin() {
        assert_eq!(layout(&[(1920, 1080)]), vec![(0, 0)]);
    }

    #[test]
    fn layout_repacks_after_a_middle_output_is_removed() {
        // Three monitors at 0 / 1920 / 3200. Pull the middle one and the
        // third must slide left to 1920 — leaving it at 3200 would open a
        // 1280 px hole in the combined space that no panel displays.
        let before = layout(&[(1920, 1080), (1280, 1024), (2560, 1440)]);
        assert_eq!(before, vec![(0, 0), (1920, 0), (3200, 0)]);
        let after = layout(&[(1920, 1080), (2560, 1440)]);
        assert_eq!(after, vec![(0, 0), (1920, 0)]);
    }

    #[test]
    fn layout_of_nothing_is_nothing() {
        assert!(layout(&[]).is_empty());
    }

    #[test]
    fn combined_extent_spans_every_output() {
        let sizes = [(1920u32, 1080u32), (1280, 1024)];
        let pos = layout(&sizes);
        assert_eq!(combined_extent(&pos, &sizes), (3200, 1080));
    }

    #[test]
    fn combined_extent_of_nothing_is_zero() {
        assert_eq!(combined_extent(&[], &[]), (0, 0));
    }

    #[test]
    fn the_pointer_comes_back_onto_the_surviving_display() {
        // This is the concrete "stranded off-screen" case: the pointer was
        // on the external monitor when it was unplugged.
        let clamped = clamp_pointer_to_outputs((3000.0, 900.0), [((0, 0), (1920, 1080))]);
        assert_eq!(clamped, (1919.0, 900.0));
    }

    #[test]
    fn a_pointer_already_in_bounds_is_left_alone() {
        assert_eq!(
            clamp_pointer_to_outputs((100.0, 200.0), [((0, 0), (1920, 1080))]),
            (100.0, 200.0)
        );
    }

    #[test]
    fn the_pointer_is_clamped_vertically_too() {
        // Unplugging a taller monitor shrinks the height of the space.
        assert_eq!(
            clamp_pointer_to_outputs((10.0, 1400.0), [((0, 0), (1920, 1080))]),
            (10.0, 1079.0)
        );
    }

    #[test]
    fn a_pointer_in_a_space_with_no_outputs_parks_at_the_origin() {
        assert_eq!(
            clamp_pointer_to_outputs((3000.0, 900.0), std::iter::empty()),
            (0.0, 0.0)
        );
    }

    #[test]
    fn pointer_is_projected_out_of_an_l_shaped_topology_gap() {
        let outputs = [((0, 0), (1920, 1080)), ((1920, 360), (1280, 720))];
        assert_eq!(
            clamp_pointer_to_outputs((2200.0, 100.0), outputs),
            (1919.0, 100.0),
            "the empty rectangle above the offset monitor is not addressable"
        );
        assert_eq!(
            clamp_pointer_to_outputs((2200.0, 500.0), outputs),
            (2200.0, 500.0),
            "a point on the offset monitor must be preserved"
        );
    }

    #[test]
    fn pointer_can_cross_the_shared_edge_of_vertically_offset_outputs() {
        let outputs = [((0, 0), (1920, 1080)), ((1920, 360), (1280, 720))];
        assert_eq!(
            clamp_pointer_to_outputs((1924.0, 500.0), outputs),
            (1924.0, 500.0)
        );
    }

    #[test]
    fn nearest_output_wins_across_a_real_topology_gap() {
        let outputs = [((0, 0), (800, 600)), ((1000, 0), (800, 600))];
        assert_eq!(
            clamp_pointer_to_outputs((850.0, 300.0), outputs),
            (799.0, 300.0)
        );
        assert_eq!(
            clamp_pointer_to_outputs((950.0, 300.0), outputs),
            (1000.0, 300.0)
        );
    }

    #[test]
    fn mirrored_outputs_and_total_disconnect_are_deterministic() {
        let mirrors = [((0, 0), (1920, 1080)), ((0, 0), (1920, 1080))];
        assert_eq!(
            clamp_pointer_to_outputs((900.0, 500.0), mirrors),
            (900.0, 500.0)
        );
        assert_eq!(
            clamp_pointer_to_outputs((10.0, 20.0), std::iter::empty()),
            (0.0, 0.0)
        );
        assert_eq!(
            clamp_pointer_to_outputs((f64::NAN, 20.0), mirrors),
            (0.0, 0.0)
        );
    }
}
