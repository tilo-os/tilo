//! The room a widget may rest in on a display, as the device publishes it (`device.room`): what
//! the shell's own bar and dock leave of the display when the style laid them out.

use serde::{Deserialize, Serialize};
use tilo_resource::NoEffect;

/// The room left on one display for what the shell keeps over it, in logical pixels, from the
/// display's own top-leading corner: a widget rests inside it and never under the bar or the dock.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Room {
    /// The room's leading edge.
    pub x: f64,
    /// The room's top edge.
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One body of the shell's chrome as it was laid out, a bar or a dock: its rectangle from the
/// display's top-leading corner, in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Chrome {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Room {
    /// The room `chrome` — each bar and dock the shell's style laid out — leaves of `display`,
    /// both as `(x, y, width, height)` from the display's top-leading corner.
    ///
    /// A body takes the edge its own middle is nearest, and the room comes in to its far side on
    /// that edge: a bar across the top, a floating one inset from it, a dock at the foot, and a
    /// dock up either side each take their own edge, at whatever height and width the person's
    /// settings and text scale gave them.
    #[must_use]
    pub fn of(display: (f64, f64, f64, f64), chrome: &[Chrome]) -> Self {
        let (dx, dy, width, height) = display;
        let (mut x0, mut y0) = (dx, dy);
        let (mut x1, mut y1) = (dx + width, dy + height);
        for body in chrome {
            if body.width <= 0.0 || body.height <= 0.0 {
                continue;
            }
            let (cx, cy) = (body.x + body.width / 2.0, body.y + body.height / 2.0);
            let (top, bottom) = ((cy - dy).abs(), (dy + height - cy).abs());
            let (leading, trailing) = ((cx - dx).abs(), (dx + width - cx).abs());
            // The edge its own middle is nearest; the first of two alike, so a body centred on
            // the display takes the top.
            let edge = [top, bottom, leading, trailing]
                .into_iter()
                .enumerate()
                .min_by(|(_, one), (_, two)| one.total_cmp(two))
                .map_or(0, |(at, _)| at);
            match edge {
                0 => y0 = y0.max(body.y + body.height),
                1 => y1 = y1.min(body.y),
                2 => x0 = x0.max(body.x + body.width),
                _ => x1 = x1.min(body.x),
            }
        }
        Self {
            x: x0,
            y: y0,
            width: (x1 - x0).max(0.0),
            height: (y1 - y0).max(0.0),
        }
    }
}

/// The room as a view of another language reads it (`device.room`).
#[must_use]
pub(crate) fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Room, NoEffect>("room", "Room")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: (f64, f64, f64, f64) = (0.0, 0.0, 1920.0, 1080.0);

    fn chrome(x: f64, y: f64, width: f64, height: f64) -> Chrome {
        Chrome {
            x,
            y,
            width,
            height,
        }
    }

    /// Every edge the chrome rides takes the room in to itself, at the size it was laid out:
    /// a bar at the top or the foot, a dock at the foot and up either side, floating or edge to
    /// edge, and both at once.
    #[test]
    fn the_chrome_takes_its_own_edge_at_the_size_it_was_laid_out() {
        assert_eq!(
            Room::of(DISPLAY, &[]),
            Room {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
            "no chrome leaves the whole display"
        );
        assert_eq!(
            Room::of(DISPLAY, &[chrome(0.0, 0.0, 1920.0, 48.0)]),
            Room {
                x: 0.0,
                y: 48.0,
                width: 1920.0,
                height: 1032.0,
            },
            "a bar across the top"
        );
        assert_eq!(
            Room::of(DISPLAY, &[chrome(792.0, 8.0, 336.0, 40.0)]),
            Room {
                x: 0.0,
                y: 48.0,
                width: 1920.0,
                height: 1032.0,
            },
            "a floating bar inset from the top edge"
        );
        assert_eq!(
            Room::of(DISPLAY, &[chrome(0.0, 1000.0, 1920.0, 80.0)]),
            Room {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1000.0,
            },
            "a dock at the foot"
        );
        assert_eq!(
            Room::of(DISPLAY, &[chrome(720.0, 992.0, 480.0, 80.0)]),
            Room {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 992.0,
            },
            "a floating dock at the foot"
        );
        assert_eq!(
            Room::of(DISPLAY, &[chrome(0.0, 0.0, 96.0, 1080.0)]),
            Room {
                x: 96.0,
                y: 0.0,
                width: 1824.0,
                height: 1080.0,
            },
            "a dock up the leading edge"
        );
        assert_eq!(
            Room::of(DISPLAY, &[chrome(1840.0, 240.0, 72.0, 600.0)]),
            Room {
                x: 0.0,
                y: 0.0,
                width: 1840.0,
                height: 1080.0,
            },
            "a dock up the trailing edge"
        );
        assert_eq!(
            Room::of(
                DISPLAY,
                &[
                    chrome(0.0, 0.0, 1920.0, 44.0),
                    chrome(0.0, 1008.0, 96.0, 72.0)
                ]
            ),
            Room {
                x: 0.0,
                y: 44.0,
                width: 1920.0,
                height: 964.0,
            },
            "a bar across the top and a dock at the foot: each takes its own band, so the room \
             stands clear of both"
        );
    }

    /// A body larger than the display, or a display with no size yet, never leaves a room
    /// outside the display or one that runs backwards.
    #[test]
    fn a_room_never_runs_outside_its_display() {
        assert_eq!(
            Room::of(
                (0.0, 0.0, 1920.0, 1080.0),
                &[chrome(0.0, 0.0, 1920.0, 2000.0)]
            ),
            Room {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 0.0,
            },
            "a bar taller than the display leaves nothing beside it"
        );
        assert_eq!(Room::of((0.0, 0.0, 0.0, 0.0), &[]), Room::default());
        assert_eq!(
            Room::of((0.0, 0.0, 1920.0, 1080.0), &[chrome(0.0, 0.0, 0.0, 0.0)]),
            Room {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
            "a body nothing laid out takes no edge"
        );
    }
}
