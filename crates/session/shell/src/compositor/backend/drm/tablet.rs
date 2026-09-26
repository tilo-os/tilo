//! libinput tablet-tool events → [`TabletInput`], mapped onto the output
//! the pen is tied to (Settings' "Pen & tablet" `pen_output`), or across the
//! whole desktop when it is tied to none or that output is unplugged.

use smithay::backend::input::{Event, TabletToolCapabilities, TabletToolEvent, TabletToolType};
use smithay::backend::libinput::LibinputInputBackend;

use crate::compositor::backend::{
    TabletDevice, TabletInput, TabletPhase, TabletTool, TabletToolKind, map_tablet_position,
};

/// Resolution used to normalise libinput's absolute position.
const NORMALISE: i32 = 1 << 16;

/// The global rect (`x, y, width, height`) the pen maps onto: the named
/// output when it is live, otherwise the bounding box of every output.
/// `outputs` are `(connector name, settings key, position, logical size)`.
pub(super) fn pen_target<'a>(
    outputs: impl Iterator<Item = (&'a str, &'a str, (i32, i32), (u32, u32))> + Clone,
    pen_output: Option<&str>,
) -> Option<(f64, f64, f64, f64)> {
    let rect = |(x, y): (i32, i32), (w, h): (u32, u32)| {
        (f64::from(x), f64::from(y), f64::from(w), f64::from(h))
    };
    if let Some(wanted) = pen_output
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

pub(super) fn tablet_input<E>(
    event: &E,
    phase: TabletPhase,
    target: (f64, f64, f64, f64),
) -> TabletInput
where
    E: TabletToolEvent<LibinputInputBackend> + Event<LibinputInputBackend>,
{
    let device = event.device();
    let descriptor = event.tool();
    let normalised = (
        event.x_transformed(NORMALISE) / f64::from(NORMALISE),
        event.y_transformed(NORMALISE) / f64::from(NORMALISE),
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
        pressure: event.pressure(),
        distance: event.distance(),
        tilt: (event.tilt_x(), event.tilt_y()),
        rotation: event.rotation(),
        slider: event.slider_position(),
        wheel: (event.wheel_delta(), event.wheel_delta_discrete()),
        time_msec: event.time_msec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
