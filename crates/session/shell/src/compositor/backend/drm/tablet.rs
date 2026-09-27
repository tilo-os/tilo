//! libinput tablet-tool events → [`TabletInput`], shaped by the tablet's
//! own saved settings ([`TabletConfig`], keyed by device identity): mapped
//! onto the display it is tied to (or the whole desktop when it is tied to
//! none or that display is unplugged), letterboxed to keep its aspect ratio,
//! turned for left-handed use, pressure through its curve, barrel buttons
//! and the eraser remapped. Pad buttons run shortcuts through
//! [`chord_keycodes`].

use smithay::backend::input::{Event, TabletToolCapabilities, TabletToolEvent, TabletToolType};
use smithay::backend::libinput::LibinputInputBackend;

use settings_store::hardware::{
    EraserAction, KeyChord, ShortcutAction, StylusButtonAction, TabletConfig,
};

use crate::compositor::backend::{
    TabletDevice, TabletInput, TabletPhase, TabletTool, TabletToolKind, map_tablet_position,
};
use crate::compositor::tablet::{
    BTN_EXTRA, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE, BTN_STYLUS, BTN_STYLUS2, BTN_STYLUS3,
};

/// Resolution used to normalise libinput's absolute position.
const NORMALISE: i32 = 1 << 16;

/// The global rect (`x, y, width, height`) the pen maps onto: the named
/// output when it is live, otherwise the bounding box of every output.
/// `outputs` are `(connector name, settings key, position, logical size)`.
pub(super) fn pen_target<'a>(
    outputs: impl Iterator<Item = (&'a str, &'a str, (i32, i32), (u32, u32))> + Clone,
    output: Option<&str>,
) -> Option<(f64, f64, f64, f64)> {
    let rect = |(x, y): (i32, i32), (w, h): (u32, u32)| {
        (f64::from(x), f64::from(y), f64::from(w), f64::from(h))
    };
    if let Some(wanted) = output
        && let Some((_, _, position, size)) = outputs
            .clone()
            .find(|(connector, key, _, _)| *connector == wanted || *key == wanted)
    {
        return Some(rect(position, size));
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (_, _, position, size) in outputs {
        let (x, y, w, h) = rect(position, size);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x + w);
        y1 = y1.max(y + h);
    }
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
}

/// One tool event as the compositor receives it, shaped by `config`.
pub(super) fn tablet_input<E>(
    event: &E,
    phase: TabletPhase,
    target: (f64, f64, f64, f64),
    config: &TabletConfig,
) -> TabletInput
where
    E: TabletToolEvent<LibinputInputBackend> + Event<LibinputInputBackend>,
{
    let device = event.device();
    let descriptor = event.tool();
    let normalised = shape_normalised(
        (
            event.x_transformed(NORMALISE) / f64::from(NORMALISE),
            event.y_transformed(NORMALISE) / f64::from(NORMALISE),
        ),
        device.size(),
        target,
        config.keep_aspect,
        config.left_handed,
    );
    let caps = descriptor.capabilities;
    TabletInput {
        device: TabletDevice {
            name: smithay::backend::input::Device::name(&device),
            usb_id: smithay::backend::input::Device::usb_id(&device),
            syspath: smithay::backend::input::Device::syspath(&device),
        },
        tool: TabletTool {
            kind: match descriptor.tool_type {
                TabletToolType::Pen => TabletToolKind::Pen,
                TabletToolType::Eraser => TabletToolKind::Eraser,
                TabletToolType::Brush => TabletToolKind::Brush,
                TabletToolType::Pencil => TabletToolKind::Pencil,
                TabletToolType::Airbrush => TabletToolKind::Airbrush,
                TabletToolType::Mouse => TabletToolKind::Mouse,
                TabletToolType::Lens => TabletToolKind::Lens,
                TabletToolType::Totem => TabletToolKind::Totem,
                TabletToolType::Unknown => TabletToolKind::Unknown,
            },
            serial: descriptor.hardware_serial,
            wacom_id: descriptor.hardware_id_wacom,
            pressure: caps.contains(TabletToolCapabilities::PRESSURE),
            distance: caps.contains(TabletToolCapabilities::DISTANCE),
            tilt: caps.contains(TabletToolCapabilities::TILT),
            rotation: caps.contains(TabletToolCapabilities::ROTATION),
            slider: caps.contains(TabletToolCapabilities::SLIDER),
            wheel: caps.contains(TabletToolCapabilities::WHEEL),
        },
        phase,
        position: map_tablet_position(normalised, target),
        pressure: apply_pressure_curve(config.pressure_curve, event.pressure()),
        distance: event.distance(),
        tilt: (event.tilt_x(), event.tilt_y()),
        rotation: event.rotation(),
        slider: event.slider_position(),
        wheel: (event.wheel_delta(), event.wheel_delta_discrete()),
        time_msec: event.time_msec(),
    }
}

/// Where on the tablet's active surface (`0.0 ..= 1.0` each way) a pen at
/// `normalised` counts as being, before [`map_tablet_position`] puts it on
/// `target` (`x, y, width, height`).
///
/// With `keep_aspect`, only the centred part of the tablet with the target's
/// shape is used (the rest letterboxed away), so a circle drawn on the
/// tablet stays a circle on screen and the whole target stays reachable.
/// `left_handed` turns the tablet half a turn, buttons on the right.
pub fn shape_normalised(
    normalised: (f64, f64),
    tablet_mm: Option<(f64, f64)>,
    target: (f64, f64, f64, f64),
    keep_aspect: bool,
    left_handed: bool,
) -> (f64, f64) {
    let unit = |v: f64| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.5
        }
    };
    let (mut nx, mut ny) = (unit(normalised.0), unit(normalised.1));
    if left_handed {
        nx = 1.0 - nx;
        ny = 1.0 - ny;
    }
    let (_, _, width, height) = target;
    if keep_aspect
        && let Some((tw, th)) = tablet_mm
        && tw > 0.0
        && th > 0.0
        && width > 0.0
        && height > 0.0
    {
        let tablet_aspect = tw / th;
        let target_aspect = width / height;
        if tablet_aspect > target_aspect {
            // Tablet wider than the display: use a centred band of its width.
            let used = target_aspect / tablet_aspect;
            nx = unit((nx - (1.0 - used) / 2.0) / used);
        } else if tablet_aspect < target_aspect {
            let used = tablet_aspect / target_aspect;
            ny = unit((ny - (1.0 - used) / 2.0) / used);
        }
    }
    (nx, ny)
}

/// Shape a raw pen pressure (`0.0 ..= 1.0`) through the tablet's pressure
/// curve: a cubic Bézier from `(0, 0)` to `(1, 1)` with control points
/// `(x1, y1)` and `(x2, y2)`, the same form GNOME and Wacom's drivers use.
/// Points above the diagonal feel soft (light pressure already draws
/// heavily); below it, firm.
pub fn apply_pressure_curve(curve: [f64; 4], pressure: f64) -> f64 {
    let p = if pressure.is_finite() {
        pressure.clamp(0.0, 1.0)
    } else {
        0.0
    };
    // The curve's ends are fixed; a pen resting or pressed flat stays exact.
    if p <= 0.0 || p >= 1.0 {
        return p;
    }
    let [x1, y1, x2, y2] = curve.map(|v| {
        if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            0.0
        }
    });
    let bezier = |a: f64, b: f64, t: f64| {
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    };
    // x(t) is monotonic for control points inside the unit square, so a
    // bisection finds the t whose x is the pressure.
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..40 {
        let mid = f64::midpoint(lo, hi);
        if bezier(x1, x2, mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bezier(y1, y2, f64::midpoint(lo, hi)).clamp(0.0, 1.0)
}

/// What one pen button press turns into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonOutcome {
    /// Report this button code: to a tablet-aware client as a tool button,
    /// and to everything else as the pointer button it emulates.
    Button(u32),
    /// Perform this desktop shortcut.
    Shortcut(ShortcutAction),
    /// Swallow the press.
    Ignore,
}

/// Resolve a pen barrel button against the tablet's saved mapping.
pub fn stylus_button(config: &TabletConfig, code: u32) -> ButtonOutcome {
    let action = match code {
        BTN_STYLUS => config.lower_button,
        BTN_STYLUS2 => config.upper_button,
        BTN_STYLUS3 => config.third_button,
        other => return ButtonOutcome::Button(other),
    };
    match action {
        StylusButtonAction::Default => ButtonOutcome::Button(code),
        StylusButtonAction::Left => ButtonOutcome::Button(BTN_LEFT),
        StylusButtonAction::Middle => ButtonOutcome::Button(BTN_MIDDLE),
        StylusButtonAction::Right => ButtonOutcome::Button(BTN_RIGHT),
        StylusButtonAction::Back => ButtonOutcome::Button(BTN_SIDE),
        StylusButtonAction::Forward => ButtonOutcome::Button(BTN_EXTRA),
        StylusButtonAction::Shortcut(action) => ButtonOutcome::Shortcut(action),
        StylusButtonAction::Disabled => ButtonOutcome::Ignore,
    }
}

/// Where the eraser end of a pen goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EraserRoute {
    /// As an eraser tool: tablet-aware apps erase, others see a pointer.
    Native,
    /// As a pointer everywhere, touching the surface pressing this button
    /// code.
    Pointer(u32),
    /// As a pointer that clicks nothing; touching the surface performs
    /// this shortcut.
    Shortcut(ShortcutAction),
}

/// Resolve the eraser against the tablet's saved mapping.
pub fn eraser_route(config: &TabletConfig) -> EraserRoute {
    match config.eraser {
        EraserAction::Native => EraserRoute::Native,
        EraserAction::Left => EraserRoute::Pointer(BTN_LEFT),
        EraserAction::Middle => EraserRoute::Pointer(BTN_MIDDLE),
        EraserAction::Right => EraserRoute::Pointer(BTN_RIGHT),
        EraserAction::Back => EraserRoute::Pointer(BTN_SIDE),
        EraserAction::Forward => EraserRoute::Pointer(BTN_EXTRA),
        EraserAction::Shortcut(action) => EraserRoute::Shortcut(action),
    }
}

/// The shortcut a pad button performs, if one is mapped.
pub fn pad_button(config: &TabletConfig, button: u32) -> Option<ShortcutAction> {
    config.pad_buttons.get(&button).copied()
}

/// evdev keycodes that press `chord`: its modifiers first, then the key.
/// A pen or pad button mapped to a shortcut presses exactly the chord the
/// user bound to it, so the one shortcut resolver decides what it does.
/// `char_keycode` finds a printable key on the active layout.
pub fn chord_keycodes(
    chord: &KeyChord,
    char_keycode: impl Fn(char) -> Option<u32>,
) -> Option<Vec<u32>> {
    const KEY_LEFTCTRL: u32 = 29;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_LEFTALT: u32 = 56;
    const KEY_LEFTMETA: u32 = 125;
    let mut codes = Vec::with_capacity(chord.modifiers.len().saturating_add(1));
    for modifier in &chord.modifiers {
        codes.push(match modifier.as_str() {
            "Super" => KEY_LEFTMETA,
            "Ctrl" => KEY_LEFTCTRL,
            "Alt" => KEY_LEFTALT,
            "Shift" => KEY_LEFTSHIFT,
            _ => return None,
        });
    }
    let key = match chord.key.as_str() {
        "space" => 57,
        "tab" => 15,
        "enter" => 28,
        "backspace" => 14,
        "left" => 105,
        "right" => 106,
        "up" => 103,
        "down" => 108,
        "home" => 102,
        "end" => 107,
        "pageup" => 104,
        "pagedown" => 109,
        "insert" => 110,
        "delete" => 111,
        "printscreen" => 99,
        "f11" => 87,
        "f12" => 88,
        name => {
            if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok())
                && (1..=10).contains(&n)
            {
                n.saturating_add(58)
            } else {
                let mut chars = name.chars();
                let (Some(ch), None) = (chars.next(), chars.next()) else {
                    return None;
                };
                char_keycode(ch)?
            }
        }
    };
    codes.push(key);
    Some(codes)
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact corners and midpoints are the contract under test"
)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const SECOND: (f64, f64, f64, f64) = (1920.0, 0.0, 1920.0, 1080.0);

    fn on(
        target: (f64, f64, f64, f64),
        n: (f64, f64),
        mm: Option<(f64, f64)>,
        aspect: bool,
        left: bool,
    ) -> (f64, f64) {
        map_tablet_position(shape_normalised(n, mm, target, aspect, left), target)
    }

    #[test]
    fn left_handed_turns_the_tablet_half_a_turn() {
        assert_eq!(on(SECOND, (0.0, 0.0), None, false, true), (3840.0, 1080.0));
        assert_eq!(
            on(SECOND, (0.25, 0.75), None, false, true),
            (1920.0 + 0.75 * 1920.0, 0.25 * 1080.0)
        );
        assert_eq!(
            on(SECOND, (f64::NAN, 2.0), None, false, false),
            (2880.0, 1080.0),
            "junk clamps"
        );
    }

    #[test]
    fn keeping_the_aspect_ratio_letterboxes_the_tablet_not_the_screen() {
        // A 4:3 tablet on a 16:9 display: the top and bottom bands of the
        // tablet go unused so strokes keep their shape.
        let tablet = Some((160.0, 120.0));
        let map = |n| on(SECOND, n, tablet, true, false);
        let used = (160.0 / 120.0) / (1920.0 / 1080.0);
        let top = (1.0 - used) / 2.0;
        assert_eq!(map((0.0, top)), (1920.0, 0.0));
        assert_eq!(map((1.0, 1.0 - top)), (3840.0, 1080.0));
        assert_eq!(map((0.5, 0.0)), (2880.0, 0.0), "the unused band clamps");
        assert!((map((0.5, 0.5)).1 - 540.0).abs() < 1e-9);
        assert_eq!(on(SECOND, (0.5, 0.0), tablet, false, false).1, 0.0);
        // A wide tablet on a tall display trims the sides instead.
        let portrait = (0.0, 0.0, 1080.0, 1920.0);
        assert!((on(portrait, (0.5, 0.5), tablet, true, false).0 - 540.0).abs() < 1e-9);
        assert_eq!(on(portrait, (0.0, 0.5), tablet, true, false).0, 0.0);
    }

    #[test]
    fn the_pen_maps_to_its_output_or_the_whole_desktop() {
        let outputs = [
            ("eDP-1", "eDP-1", (0, 0), (2560_u32, 1600_u32)),
            ("DP-2", "DP-2", (2560, 0), (1920, 1080)),
        ];
        assert_eq!(
            pen_target(outputs.iter().copied(), Some("DP-2")),
            Some((2560.0, 0.0, 1920.0, 1080.0))
        );
        assert_eq!(
            pen_target(outputs.iter().copied(), None),
            Some((0.0, 0.0, 4480.0, 1600.0))
        );
        // An unplugged output falls back to the desktop rather than
        // stranding the pen.
        assert_eq!(
            pen_target(outputs.iter().copied(), Some("HDMI-A-1")),
            Some((0.0, 0.0, 4480.0, 1600.0))
        );
        assert_eq!(pen_target(std::iter::empty(), None), None);
    }

    #[test]
    fn the_pressure_curve_shapes_pressure_and_keeps_its_ends() {
        let linear = [0.0, 0.0, 1.0, 1.0];
        for p in [0.0, 0.1, 0.5, 0.9, 1.0] {
            assert!((apply_pressure_curve(linear, p) - p).abs() < 1e-6);
        }
        let soft = [0.0, 0.5, 0.5, 1.0];
        let firm = [0.5, 0.0, 1.0, 0.5];
        assert!(apply_pressure_curve(soft, 0.3) > 0.3, "soft draws heavier");
        assert!(apply_pressure_curve(firm, 0.3) < 0.3, "firm draws lighter");
        for curve in [soft, firm] {
            assert!(apply_pressure_curve(curve, 0.0).abs() < 1e-6);
            assert!((apply_pressure_curve(curve, 1.0) - 1.0).abs() < 1e-6);
        }
        assert_eq!(apply_pressure_curve(soft, f64::NAN), 0.0);
        assert!(apply_pressure_curve(linear, 7.0) <= 1.0);
    }

    #[test]
    fn stylus_buttons_follow_the_saved_mapping() {
        let config = TabletConfig {
            lower_button: StylusButtonAction::Right,
            upper_button: StylusButtonAction::Shortcut(ShortcutAction::Search),
            third_button: StylusButtonAction::Disabled,
            ..TabletConfig::default()
        };
        assert_eq!(
            stylus_button(&config, BTN_STYLUS),
            ButtonOutcome::Button(BTN_RIGHT)
        );
        assert_eq!(
            stylus_button(&config, BTN_STYLUS2),
            ButtonOutcome::Shortcut(ShortcutAction::Search)
        );
        assert_eq!(stylus_button(&config, BTN_STYLUS3), ButtonOutcome::Ignore);
        let untouched = TabletConfig::default();
        assert_eq!(
            stylus_button(&untouched, BTN_STYLUS),
            ButtonOutcome::Button(BTN_STYLUS),
            "an unmapped button reaches tablet-aware apps as itself"
        );
    }

    #[test]
    fn the_eraser_follows_the_saved_mapping() {
        assert_eq!(
            eraser_route(&TabletConfig::default()),
            EraserRoute::Native,
            "a new tablet erases natively"
        );
        for (action, route) in [
            (EraserAction::Left, EraserRoute::Pointer(BTN_LEFT)),
            (EraserAction::Middle, EraserRoute::Pointer(BTN_MIDDLE)),
            (EraserAction::Right, EraserRoute::Pointer(BTN_RIGHT)),
            (EraserAction::Back, EraserRoute::Pointer(BTN_SIDE)),
            (EraserAction::Forward, EraserRoute::Pointer(BTN_EXTRA)),
            (
                EraserAction::Shortcut(ShortcutAction::ClipboardHistory),
                EraserRoute::Shortcut(ShortcutAction::ClipboardHistory),
            ),
        ] {
            let config = TabletConfig {
                eraser: action,
                ..TabletConfig::default()
            };
            assert_eq!(eraser_route(&config), route, "{action:?}");
        }
    }

    #[test]
    fn pad_buttons_run_only_mapped_shortcuts() {
        let mut config = TabletConfig::default();
        config
            .pad_buttons
            .insert(2, ShortcutAction::WorkspaceOverview);
        assert_eq!(
            pad_button(&config, 2),
            Some(ShortcutAction::WorkspaceOverview)
        );
        assert_eq!(pad_button(&config, 0), None);
    }

    #[test]
    fn a_chord_becomes_modifiers_then_its_key() {
        let chord = |mods: &[&str], key: &str| KeyChord {
            modifiers: mods
                .iter()
                .map(|m| (*m).to_string())
                .collect::<BTreeSet<_>>(),
            key: key.into(),
        };
        let us = |ch: char| match ch {
            'k' => Some(37),
            '/' => Some(53),
            _ => None,
        };
        assert_eq!(
            chord_keycodes(&chord(&["Super"], "k"), us),
            Some(vec![125, 37])
        );
        assert_eq!(
            chord_keycodes(&chord(&["Shift", "Super"], "/"), us),
            Some(vec![42, 125, 53])
        );
        assert_eq!(
            chord_keycodes(&chord(&["Super"], "pagedown"), us),
            Some(vec![125, 109])
        );
        assert_eq!(chord_keycodes(&chord(&[], "f1"), us), Some(vec![59]));
        assert_eq!(chord_keycodes(&chord(&[], "f10"), us), Some(vec![68]));
        assert_eq!(
            chord_keycodes(&chord(&["Ctrl"], "f12"), us),
            Some(vec![29, 88])
        );
        assert_eq!(
            chord_keycodes(&chord(&["Super"], "q"), us),
            None,
            "not on the layout"
        );
        assert_eq!(chord_keycodes(&chord(&["Hyper"], "k"), us), None);
    }
}
