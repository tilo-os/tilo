//! Per-output display scale.
//!
//! Every output used to be handed `scale: 1.0`, which is wrong the moment
//! a laptop has a HiDPI panel: the toolkit already renders at whatever
//! fractional factor it is told (`ui::scale`, `wp_fractional_scale_v1`),
//! so a hard-coded 1.0 makes a 14" 4K panel draw 7 pt text.
//!
//! The factor is derived from the panel's *physical* size, which DRM
//! reports per connector in millimetres, against its resolution. That is
//! the only signal that distinguishes a 1080p 24" desktop monitor (≈92
//! dpi, wants 1.0) from a 1080p 14" laptop panel (≈157 dpi, wants 1.5) —
//! resolution alone cannot.
//!
//! A user setting overrides the derivation outright. Auto-detection is a
//! good default and a bad mandate: someone who has decided their display
//! should be at 125 % is not asking for a second opinion.
//!
//! Everything here is pure arithmetic over plain numbers, so the policy is
//! unit-tested without a connector.

/// Physical dots-per-inch a scale factor of 1.0 is meant to look right at.
///
/// Calibrated against the two anchors that matter: a 24" 1080p desktop
/// monitor (≈92 dpi) must stay at 1.0, and a 13.3" 1080p laptop panel
/// (≈166 dpi) must land on 1.5. 110 is the value that puts both on the
/// correct quarter step with margin on either side.
pub const REFERENCE_DPI: f64 = 110.0;

/// Scale factors are snapped to quarter steps. `wp_fractional_scale_v1`
/// transports 1/120 units so finer steps would survive the wire, but a
/// continuum of scales is a continuum of slightly-different text
/// rasterisations across monitors; quarters are what every other
/// compositor's UI offers and what the Settings control writes.
pub const SCALE_STEP: f64 = 0.25;

/// Lowest scale the derivation will produce. Below 1.0 the compositor
/// would be asking clients to render *fewer* pixels than the panel has.
pub const MIN_SCALE: f64 = 1.0;

/// Highest scale the derivation will produce, matching the range the
/// Settings app's display-scale control offers (100 %–200 %).
pub const MAX_SCALE: f64 = 2.0;

/// The desktop stops being usable below this logical width, so the
/// derived scale is reduced until the output is at least this wide in
/// logical pixels. Guards the pathological case of a small, very dense
/// panel (a 4K 10" tablet screen) scaling itself down to a phone.
pub const MIN_LOGICAL_WIDTH: f64 = 1024.0;

/// Diagonal dots-per-inch of a panel `px` pixels across whose physical
/// size is `mm`. `None` when the connector reports no usable physical
/// size — which is the normal case for virtual connectors (virtio-gpu
/// reports 0×0 mm) and for some projectors.
pub fn dpi_from_physical(px: (u32, u32), mm: (u32, u32)) -> Option<f64> {
    if px.0 == 0 || px.1 == 0 || mm.0 == 0 || mm.1 == 0 {
        return None;
    }
    let diag_px = ((px.0 as f64).powi(2) + (px.1 as f64).powi(2)).sqrt();
    let diag_mm = ((mm.0 as f64).powi(2) + (mm.1 as f64).powi(2)).sqrt();
    let diag_in = diag_mm / 25.4;
    if diag_in <= 0.0 {
        return None;
    }
    Some(diag_px / diag_in)
}

/// Snap `raw` to the nearest [`SCALE_STEP`] and clamp it into
/// `[MIN_SCALE, MAX_SCALE]`.
pub fn snap_scale(raw: f64) -> f64 {
    let stepped = (raw / SCALE_STEP).round() * SCALE_STEP;
    stepped.clamp(MIN_SCALE, MAX_SCALE)
}

/// The scale this output should run at, given its resolution and physical
/// size. Returns [`MIN_SCALE`] when the physical size is unknown — a
/// connector that will not say how big it is gets the safe answer, not a
/// guess derived from resolution alone.
pub fn derive_scale(px: (u32, u32), mm: Option<(u32, u32)>) -> f64 {
    let Some(mm) = mm else {
        return MIN_SCALE;
    };
    let Some(dpi) = dpi_from_physical(px, mm) else {
        return MIN_SCALE;
    };
    let mut scale = snap_scale(dpi / REFERENCE_DPI);
    // Back off while the logical desktop would be narrower than a usable
    // window. One step at a time so a panel that only marginally trips the
    // guard keeps most of its density.
    while scale > MIN_SCALE && (px.0 as f64 / scale) < MIN_LOGICAL_WIDTH {
        scale -= SCALE_STEP;
    }
    scale.max(MIN_SCALE)
}

/// The scale for one output: the user's setting when they have expressed
/// one, otherwise the derivation.
///
/// `override_scale` is the `display.scale` value from the shared settings
/// store, already validated by the caller; `None` means "not configured".
pub fn resolve_scale(px: (u32, u32), mm: Option<(u32, u32)>, override_scale: Option<f64>) -> f64 {
    match override_scale {
        Some(s) if s > 0.0 => s,
        _ => derive_scale(px, mm),
    }
}

/// The scale a surface at global point `(x, y)` should render at.
///
/// Outputs are `(position, size, scale)` triples in the compositor's
/// combined coordinate space. A surface sitting on a given output gets
/// that output's scale; one that lands in a gap between mismatched
/// monitors — or before any output is known — gets the *largest* scale in
/// play, because rendering too sharp costs bandwidth while rendering too
/// soft is visible.
pub fn preferred_scale_at(outputs: &[((i32, i32), (u32, u32), f64)], x: f64, y: f64) -> f64 {
    for &((ox, oy), (ow, oh), scale) in outputs {
        let (ox, oy) = (ox as f64, oy as f64);
        if x >= ox && x < ox + ow as f64 && y >= oy && y < oy + oh as f64 {
            return scale;
        }
    }
    outputs
        .iter()
        .map(|&(_, _, s)| s)
        .fold(f64::NEG_INFINITY, f64::max)
        .max(MIN_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Panel geometries taken from real hardware, so the calibration is
    /// pinned against displays that exist rather than round numbers.
    const DESKTOP_24IN_1080P: ((u32, u32), (u32, u32)) = ((1920, 1080), (527, 296));
    const LAPTOP_13IN_1080P: ((u32, u32), (u32, u32)) = ((1920, 1080), (294, 165));
    const LAPTOP_156IN_1080P: ((u32, u32), (u32, u32)) = ((1920, 1080), (344, 194));
    const DESKTOP_27IN_4K: ((u32, u32), (u32, u32)) = ((3840, 2160), (597, 336));
    const LAPTOP_14IN_4K: ((u32, u32), (u32, u32)) = ((3840, 2160), (309, 174));

    #[test]
    fn dpi_is_none_without_a_physical_size() {
        // virtio-gpu reports 0×0 mm; there is nothing to derive from.
        assert_eq!(dpi_from_physical((1920, 1080), (0, 0)), None);
        assert_eq!(dpi_from_physical((0, 0), (300, 200)), None);
    }

    #[test]
    fn dpi_matches_the_panel_spec() {
        let dpi = dpi_from_physical(DESKTOP_24IN_1080P.0, DESKTOP_24IN_1080P.1)
            .expect("a 24in 1080p panel has a physical size");
        assert!(
            (90.0..95.0).contains(&dpi),
            "24in 1080p is ~92 dpi, computed {dpi}"
        );
        let dpi = dpi_from_physical(LAPTOP_13IN_1080P.0, LAPTOP_13IN_1080P.1)
            .expect("a 13in 1080p panel has a physical size");
        assert!(
            (160.0..172.0).contains(&dpi),
            "13.3in 1080p is ~166 dpi, computed {dpi}"
        );
    }

    #[test]
    fn a_desktop_monitor_stays_at_one() {
        // The regression that matters most: adding scale derivation must
        // not silently zoom every existing single-monitor desktop.
        assert_eq!(
            derive_scale(DESKTOP_24IN_1080P.0, Some(DESKTOP_24IN_1080P.1)),
            1.0
        );
    }

    #[test]
    fn a_hidpi_laptop_panel_scales_up() {
        assert_eq!(
            derive_scale(LAPTOP_13IN_1080P.0, Some(LAPTOP_13IN_1080P.1)),
            1.5,
            "a 13.3in 1080p panel needs 150%"
        );
        assert_eq!(
            derive_scale(LAPTOP_156IN_1080P.0, Some(LAPTOP_156IN_1080P.1)),
            1.25,
            "a 15.6in 1080p panel needs 125%"
        );
    }

    #[test]
    fn four_k_panels_land_on_sensible_steps() {
        assert_eq!(
            derive_scale(DESKTOP_27IN_4K.0, Some(DESKTOP_27IN_4K.1)),
            1.5,
            "a 27in 4K monitor at 150% gives a 2560x1440 logical desktop"
        );
        assert_eq!(
            derive_scale(LAPTOP_14IN_4K.0, Some(LAPTOP_14IN_4K.1)),
            2.0,
            "a 14in 4K panel is clamped to the 200% the settings control offers"
        );
    }

    #[test]
    fn an_unknown_physical_size_falls_back_to_one() {
        assert_eq!(derive_scale((1920, 1080), None), 1.0);
        assert_eq!(derive_scale((1920, 1080), Some((0, 0))), 1.0);
    }

    #[test]
    fn the_scale_never_leaves_a_desktop_narrower_than_a_window() {
        // A very dense small panel: 2560x1600 in a 10.5" tablet screen is
        // ~287 dpi, which would snap to 2.5 (clamped 2.0) and leave a
        // 1280-wide logical desktop. That is still above the floor, so it
        // stands. Shrink the panel further and the guard has to bite.
        let tiny_dense = ((1600, 1200), (120u32, 90u32)); // ~423 dpi, 5in
        let scale = derive_scale(tiny_dense.0, Some(tiny_dense.1));
        assert!(
            (tiny_dense.0.0 as f64 / scale) >= MIN_LOGICAL_WIDTH,
            "scale {scale} leaves a {}px logical desktop, below the floor",
            tiny_dense.0.0 as f64 / scale
        );
    }

    #[test]
    fn snapping_lands_on_quarter_steps_and_clamps() {
        assert_eq!(snap_scale(0.4), 1.0, "below 1.0 clamps up");
        assert_eq!(snap_scale(1.1), 1.0);
        assert_eq!(snap_scale(1.2), 1.25);
        assert_eq!(snap_scale(1.6), 1.5);
        assert_eq!(snap_scale(1.9), 2.0);
        assert_eq!(snap_scale(4.0), 2.0, "above the ceiling clamps down");
    }

    #[test]
    fn a_user_setting_overrides_the_derivation() {
        // Someone who set 125% on a 4K panel gets 125%, not the 150% the
        // physical size argues for.
        assert_eq!(
            resolve_scale(DESKTOP_27IN_4K.0, Some(DESKTOP_27IN_4K.1), Some(1.25)),
            1.25
        );
        // And with no setting, the derivation stands.
        assert_eq!(
            resolve_scale(DESKTOP_27IN_4K.0, Some(DESKTOP_27IN_4K.1), None),
            1.5
        );
        // A nonsense override is ignored rather than propagated.
        assert_eq!(
            resolve_scale(DESKTOP_24IN_1080P.0, Some(DESKTOP_24IN_1080P.1), Some(0.0)),
            1.0
        );
    }

    #[test]
    fn a_surface_gets_the_scale_of_the_output_it_is_on() {
        // The laptop panel at 1.5 on the left, an external 1080p monitor
        // at 1.0 to its right. Dragging a window across must change the
        // preferred scale it is told to render at.
        let outputs = [
            ((0, 0), (1920u32, 1080u32), 1.5),
            ((1920, 0), (1920u32, 1080u32), 1.0),
        ];
        assert_eq!(preferred_scale_at(&outputs, 10.0, 10.0), 1.5);
        assert_eq!(preferred_scale_at(&outputs, 1919.0, 500.0), 1.5);
        assert_eq!(preferred_scale_at(&outputs, 1920.0, 500.0), 1.0);
        assert_eq!(preferred_scale_at(&outputs, 3000.0, 500.0), 1.0);
    }

    #[test]
    fn a_surface_off_every_output_gets_the_sharpest_scale() {
        // Mismatched heights leave a gap. A surface there is about to land
        // somewhere; too sharp costs bandwidth, too soft is visible.
        let outputs = [
            ((0, 0), (1920u32, 1080u32), 1.0),
            ((1920, 0), (1280u32, 720u32), 2.0),
        ];
        assert_eq!(preferred_scale_at(&outputs, 2000.0, 900.0), 2.0);
    }

    #[test]
    fn with_no_outputs_at_all_the_scale_is_one() {
        // Before the first modeset there is nothing to ask; 1.0 is the
        // only answer that cannot be wrong by a factor.
        assert_eq!(preferred_scale_at(&[], 0.0, 0.0), 1.0);
    }
}
