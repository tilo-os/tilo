//! Which output a touchscreen touches, and where on it.
//!
//! A touchscreen reports positions normalised to its own surface. They are
//! mapped onto its output, not onto the desktop's combined extent, or a
//! laptop panel's touches would spread across a docked monitor. udev names
//! the output of a touchscreen (`WL_OUTPUT`, libinput's `output_name`); one
//! without a name is the panel built into the machine.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "logical output coordinates, far from overflow"
)]

use smithay::utils::Transform;

/// One output's geometry, as touch mapping needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TouchOutput<'a> {
    /// The connector name (`eDP-1`), which udev's tag names.
    pub connector_name: &'a str,
    /// The output's unique name in the session; the connector name, or a
    /// GPU-qualified one when two GPUs have a connector of that name.
    pub key: &'a str,
    /// Top-left corner in the logical layout.
    pub position: (i32, i32),
    /// Logical size, after rotation.
    pub size: (i32, i32),
    pub transform: Transform,
}

/// Whether a connector is a panel built into the machine.
pub(crate) fn is_builtin_connector(connector_name: &str) -> bool {
    let upper = connector_name.to_ascii_uppercase();
    upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI")
}

/// Pick a touchscreen's output from udev's name for it. `outputs` come in
/// layout order, the primary first, so a missing or stale name falls back
/// to the same output every time. A name two outputs share (two GPUs'
/// `HDMI-A-1`) falls back too rather than picking one by order. A
/// touchscreen without a name is the built-in panel's, even when a docked
/// monitor is the primary.
pub(crate) fn touch_output_for_name<'a>(
    outputs: impl IntoIterator<Item = TouchOutput<'a>>,
    output_name: Option<&str>,
) -> Option<TouchOutput<'a>> {
    let mut fallback = None;
    let mut panel = None;
    let mut raw_match = None;
    let mut raw_match_count = 0usize;
    for output in outputs {
        if fallback.is_none() {
            fallback = Some(output);
        }
        if panel.is_none() && is_builtin_connector(output.connector_name) {
            panel = Some(output);
        }
        if output_name.is_some_and(|name| name == output.key) {
            return Some(output);
        }
        if output_name.is_some_and(|name| name == output.connector_name) {
            raw_match_count = raw_match_count.saturating_add(1);
            raw_match = Some(output);
        }
    }
    if raw_match_count == 1 {
        raw_match
    } else if output_name.is_none() {
        panel.or(fallback)
    } else {
        fallback
    }
}

/// A touch at `(normalized_x, normalized_y)` of the panel (0..=1 each) in
/// the logical layout. The rotation is undone so touch, pointer and picture
/// agree on a rotated panel, and the far edge stays inside the output.
pub(crate) fn calibrate_touch_position(
    output: TouchOutput<'_>,
    normalized_x: f64,
    normalized_y: f64,
) -> (f64, f64) {
    let x = normalized_x.clamp(0.0, 1.0);
    let y = normalized_y.clamp(0.0, 1.0);
    // Output rectangles end before their right and bottom edges: a touch at
    // exactly 1.0 must not land on the neighbouring monitor.
    let w = f64::from(output.size.0.saturating_sub(1).max(0));
    let h = f64::from(output.size.1.saturating_sub(1).max(0));
    let (local_x, local_y) = match output.transform {
        Transform::_90 => (y * w, (1.0 - x) * h),
        Transform::_180 => ((1.0 - x) * w, (1.0 - y) * h),
        Transform::_270 => ((1.0 - y) * w, x * h),
        Transform::Flipped => ((1.0 - x) * w, y * h),
        Transform::Flipped90 => ((1.0 - y) * w, (1.0 - x) * h),
        Transform::Flipped180 => (x * w, (1.0 - y) * h),
        Transform::Flipped270 => (y * w, x * h),
        Transform::Normal => (x * w, y * h),
    };
    (
        f64::from(output.position.0) + local_x,
        f64::from(output.position.1) + local_y,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(
        connector_name: &'static str,
        key: &'static str,
        position: (i32, i32),
        size: (i32, i32),
        transform: Transform,
    ) -> TouchOutput<'static> {
        TouchOutput {
            connector_name,
            key,
            position,
            size,
            transform,
        }
    }

    #[test]
    fn a_panel_touch_stays_on_the_panel_beside_a_monitor() {
        let outputs = [
            geometry("eDP-1", "eDP-1", (0, 0), (1920, 1080), Transform::Normal),
            geometry(
                "HDMI-A-1",
                "HDMI-A-1",
                (1920, 0),
                (2560, 1440),
                Transform::Normal,
            ),
        ];
        let panel = touch_output_for_name(outputs, Some("eDP-1")).expect("bound panel");
        assert_eq!(calibrate_touch_position(panel, 0.0, 0.0), (0.0, 0.0));
        assert_eq!(
            calibrate_touch_position(panel, 1.0, 1.0),
            (1919.0, 1079.0),
            "the panel corner remains inside eDP-1, not on HDMI's first pixel"
        );
        assert_eq!(calibrate_touch_position(panel, 0.5, 0.5), (959.5, 539.5));
    }

    #[test]
    fn a_named_output_is_used_wherever_it_sits_in_the_layout() {
        let outputs = [
            geometry("eDP-1", "eDP-1", (0, 360), (1920, 1080), Transform::Normal),
            geometry("DP-1", "DP-1", (1920, 0), (2560, 1440), Transform::Normal),
        ];
        let dock = touch_output_for_name(outputs, Some("DP-1")).expect("bound dock");
        assert_eq!(calibrate_touch_position(dock, 0.0, 0.0), (1920.0, 0.0));
        assert_eq!(calibrate_touch_position(dock, 1.0, 1.0), (4479.0, 1439.0));
    }

    #[test]
    fn a_missing_output_falls_back_to_the_primary_and_an_unnamed_one_to_the_panel() {
        let after_unplug = [geometry(
            "HDMI-A-1",
            "HDMI-A-1",
            (0, 0),
            (2560, 1440),
            Transform::Normal,
        )];
        let fallback = touch_output_for_name(after_unplug, Some("eDP-1")).expect("primary");
        assert_eq!(fallback.connector_name, "HDMI-A-1");
        assert_eq!(
            calibrate_touch_position(fallback, 0.5, 0.5),
            (1279.5, 719.5)
        );

        let docked = [
            geometry(
                "HDMI-A-1",
                "HDMI-A-1",
                (0, 0),
                (2560, 1440),
                Transform::Normal,
            ),
            geometry("eDP-1", "eDP-1", (2560, 0), (2944, 1840), Transform::Normal),
        ];
        let panel = touch_output_for_name(docked, None).expect("built-in panel");
        assert_eq!(panel.connector_name, "eDP-1");
        assert_eq!(calibrate_touch_position(panel, 0.0, 0.0), (2560.0, 0.0));
    }

    #[test]
    fn a_connector_name_two_gpus_share_never_picks_one_by_order() {
        let outputs = [
            geometry("eDP-1", "eDP-1", (0, 0), (1920, 1080), Transform::Normal),
            geometry(
                "HDMI-A-1",
                "card1/HDMI-A-1",
                (1920, 0),
                (2560, 1440),
                Transform::Normal,
            ),
            geometry(
                "HDMI-A-1",
                "card2/HDMI-A-1",
                (4480, 0),
                (2560, 1440),
                Transform::Normal,
            ),
        ];
        let fallback = touch_output_for_name(outputs, Some("HDMI-A-1")).expect("primary");
        assert_eq!(fallback.key, "eDP-1");
        let qualified = touch_output_for_name(outputs, Some("card2/HDMI-A-1")).expect("qualified");
        assert_eq!(qualified.position, (4480, 0));
    }

    #[test]
    fn a_rotated_panel_maps_its_physical_corner_to_the_turned_one() {
        let output = geometry("eDP-1", "eDP-1", (100, 50), (1080, 1920), Transform::_90);
        // The panel's physical top-left is the turned picture's bottom-left.
        assert_eq!(calibrate_touch_position(output, 0.0, 0.0), (100.0, 1969.0));
        assert_eq!(calibrate_touch_position(output, 0.0, 1.0), (1179.0, 1969.0));
    }
}
