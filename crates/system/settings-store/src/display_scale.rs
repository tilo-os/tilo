//! Per-output display scale, derived from the panel's physical size (which
//! DRM reports per connector in millimetres) against its resolution — the
//! only signal that separates a 1080p 24" desktop monitor (≈92 dpi, wants
//! 1.0) from a 1080p 14" laptop panel (≈157 dpi, wants 1.5). A user setting
//! overrides the derivation outright.
//!
//! Pure arithmetic over plain numbers, so the policy is unit-tested without
//! a connector.

/// Physical dots-per-inch a scale factor of 1.0 is meant to look right at.
/// Calibrated so a 24" 1080p desktop monitor (≈92 dpi) stays at 1.0 and a
/// 13.3" 1080p laptop panel (≈166 dpi) lands on 1.5 — 110 puts both on the
/// correct quarter step with margin either side.
pub const REFERENCE_DPI: f64 = 110.0;

/// Scale factors snap to quarter steps. `wp_fractional_scale_v1` transports
/// 1/120 units so finer steps would survive the wire, but a continuum of
/// scales is a continuum of slightly-different text rasterisations across
/// monitors.
pub const SCALE_STEP: f64 = 0.25;

/// Lowest scale the derivation will produce. Below 1.0 the compositor
/// would be asking clients to render *fewer* pixels than the panel has.
pub const MIN_SCALE: f64 = 1.0;

/// Highest scale the derivation will produce, matching the range the
/// Settings app's display-scale control offers (100 %–200 %).
pub const MAX_SCALE: f64 = 2.0;

/// Derived scale is reduced until the output is at least this wide in
/// logical pixels — guards a small, very dense panel (a 4K 10" tablet
/// screen) from scaling itself down to a phone.
pub const MIN_LOGICAL_WIDTH: f64 = 1024.0;

/// Diagonal dots-per-inch of a panel `px` pixels across whose physical size
/// is `mm`. `None` when the connector reports no usable physical size —
/// normal for virtual connectors (virtio-gpu reports 0×0 mm) and some
/// projectors.
pub fn dpi_from_physical(px: (u32, u32), mm: (u32, u32)) -> Option<f64> {
    if px.0 == 0 || px.1 == 0 || mm.0 == 0 || mm.1 == 0 {
        return None;
    }
    let diag_px = (f64::from(px.0).powi(2) + f64::from(px.1).powi(2)).sqrt();
    let diag_mm = (f64::from(mm.0).powi(2) + f64::from(mm.1).powi(2)).sqrt();
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

/// The scale this output should run at. [`MIN_SCALE`] when the physical
/// size is unknown — a connector that will not say how big it is gets the
/// safe answer, not a guess from resolution alone.
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
    while scale > MIN_SCALE && (f64::from(px.0) / scale) < MIN_LOGICAL_WIDTH {
        scale -= SCALE_STEP;
    }
    scale.max(MIN_SCALE)
}

/// The `display.scale` the user set, if any. Distinguishing "unset" from
/// "set to 1.0" is the point: unset derives each output's scale from its
/// panel, 1.0 means the user chose 100% on a `HiDPI` panel. Values outside
/// the Settings control's range (100%-200%, with margin) read as unset so an
/// out-of-band value can't wedge the UI.
pub fn user_scale(display: &serde_json::Map<String, serde_json::Value>) -> Option<f64> {
    display
        .get("scale")
        .and_then(serde_json::Value::as_f64)
        .filter(|scale| (0.5..=3.0).contains(scale))
}

/// [`user_scale`] of the persisted display section.
pub fn load_user_scale() -> Option<f64> {
    user_scale(&crate::load_section(crate::hardware::DISPLAY_SECTION))
}

/// The scale for one output: the user's setting when expressed, otherwise
/// the derivation. `override_scale` is the `display.scale` value from the
/// shared settings store, already validated; `None` means not configured.
pub fn resolve_scale(px: (u32, u32), mm: Option<(u32, u32)>, override_scale: Option<f64>) -> f64 {
    match override_scale {
        Some(s) if s > 0.0 => s,
        _ => derive_scale(px, mm),
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact scale conversion results are the behavior under test"
)]
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
    fn output_scale_derives_from_panel_dpi_snaps_and_honours_the_setting() {
        // Phase `dpi_is_none_without_a_physical_size`.
        // virtio-gpu reports 0×0 mm; there is nothing to derive from.
        assert_eq!(dpi_from_physical((1920, 1080), (0, 0)), None);
        assert_eq!(dpi_from_physical((0, 0), (300, 200)), None);
        // Phase `dpi_matches_the_panel_spec`.
        {
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
        // Phase `a_desktop_monitor_stays_at_one`.
        // The regression that matters most: adding scale derivation must
        // not silently zoom every existing single-monitor desktop.
        assert_eq!(
            derive_scale(DESKTOP_24IN_1080P.0, Some(DESKTOP_24IN_1080P.1)),
            1.0
        );
        // Phase `a_hidpi_laptop_panel_scales_up`.
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
        // Phase `four_k_panels_land_on_sensible_steps`.
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
        // Phase `an_unknown_physical_size_falls_back_to_one`.
        assert_eq!(derive_scale((1920, 1080), None), 1.0);
        assert_eq!(derive_scale((1920, 1080), Some((0, 0))), 1.0);
        // Phase `the_scale_never_leaves_a_desktop_narrower_than_a_window`.
        {
            // A very dense small panel must still clear the logical-width floor.
            let tiny_dense = ((1600, 1200), (120u32, 90u32)); // ~339 dpi, 5.9in
            let scale = derive_scale(tiny_dense.0, Some(tiny_dense.1));
            assert!(
                (f64::from(tiny_dense.0.0) / scale) >= MIN_LOGICAL_WIDTH,
                "scale {scale} leaves a {}px logical desktop, below the floor",
                f64::from(tiny_dense.0.0) / scale
            );
        }
        // Phase `snapping_lands_on_quarter_steps_and_clamps`.
        assert_eq!(snap_scale(0.4), 1.0, "below 1.0 clamps up");
        assert_eq!(snap_scale(1.1), 1.0);
        assert_eq!(snap_scale(1.2), 1.25);
        assert_eq!(snap_scale(1.6), 1.5);
        assert_eq!(snap_scale(1.9), 2.0);
        assert_eq!(snap_scale(4.0), 2.0, "above the ceiling clamps down");
        // Phase `a_user_setting_overrides_the_derivation`.
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
}
