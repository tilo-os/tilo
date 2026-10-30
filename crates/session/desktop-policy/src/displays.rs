//! The displays a desktop spreads over, as the compositor's outputs make
//! them.
//!
//! Outputs that extend the desktop tile edge to edge and never overlap; a
//! mirror is laid over the rectangle of the output it copies. So an output
//! overlapping one already taken shows that one's picture and is not a
//! display of its own: the first output of a mirror set stands for it.

use tilo_desktop_model::Display;
use ui::kurbo::Rect;

/// One output as the host knows it: who it is (the monitor's identity,
/// stable across restarts and port swaps when the host can name it) and
/// where it is in the surface's coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub monitor: String,
    pub rect: Rect,
}

/// The displays among `outputs` (primary first, as the host lists them):
/// every output with an area, a mirror set as one. Each display's id is
/// its output's place in `outputs`.
#[must_use]
pub fn distinct(outputs: &[Output]) -> Vec<Display> {
    let mut displays: Vec<Display> = Vec::with_capacity(outputs.len());
    for (index, output) in outputs.iter().enumerate() {
        let rect = output.rect;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        let mirrors = displays.iter().any(|display| {
            let kept = Rect::new(
                display.x,
                display.y,
                display.x + display.width,
                display.y + display.height,
            );
            let shared = kept.intersect(rect);
            shared.width() > 0.0 && shared.height() > 0.0
        });
        let Ok(id) = u32::try_from(index) else {
            break;
        };
        if !mirrors {
            displays.push(Display {
                id,
                monitor: output.monitor.clone(),
                x: rect.x0,
                y: rect.y0,
                width: rect.width(),
                height: rect.height(),
            });
        }
    }
    displays
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(monitor: &str, x: f64, w: f64) -> Output {
        Output {
            monitor: monitor.into(),
            rect: Rect::new(x, 0.0, x + w, 600.0),
        }
    }

    #[test]
    fn side_by_side_outputs_are_displays_of_their_own() {
        let displays = distinct(&[output("a", 0.0, 800.0), output("b", 800.0, 1024.0)]);
        let ids: Vec<(u32, &str)> = displays
            .iter()
            .map(|d| (d.id, d.monitor.as_str()))
            .collect();
        assert_eq!(ids, [(0, "a"), (1, "b")]);
        let place = (displays[1].x, displays[1].width);
        assert!(place == (800.0, 1024.0), "{place:?}");
    }

    #[test]
    fn a_mirror_is_one_display_with_the_output_it_copies() {
        let displays = distinct(&[
            output("panel", 0.0, 800.0),
            output("projector", 0.0, 800.0),
            output("monitor", 800.0, 800.0),
        ]);
        let monitors: Vec<&str> = displays.iter().map(|d| d.monitor.as_str()).collect();
        assert_eq!(monitors, ["panel", "monitor"]);
        assert_eq!(displays[1].id, 2, "ids stay the outputs' places");
    }

    #[test]
    fn an_output_without_an_area_is_no_display() {
        let displays = distinct(&[output("off", 0.0, 0.0), output("on", 0.0, 800.0)]);
        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0].monitor, "on");
    }
}
