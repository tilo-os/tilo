//! Where a pointer is, said relative to what it is over, so every viewer
//! places it in their own layout.
//!
//! People in one room rarely share a screen size: the same window is wider
//! on one display than another, and each viewer scrolls a strip on their own.
//! A point in one page's surface pixels lands somewhere else on every other
//! page. A pointer therefore travels as a point within the window it is over
//! (named by the record the window shows, which every member names alike),
//! or, over a strip's empty desktop, as a point in that strip's own
//! coordinates, whatever it is scrolled to. Each viewer maps it back through
//! [`PointerLayout::place`], and does not draw what it does not show.

use serde::{Deserialize, Serialize};

use super::{LiveRecord, LiveWireError, Result, finite};

/// What a pointer is over, and where within it, in logical pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum LivePointer {
    /// Over the window showing `record`, `at` from the top-left corner of
    /// its content.
    Window {
        /// The record the window shows.
        record: LiveRecord,
        /// From the window content's top-left corner.
        at: [f32; 2],
    },
    /// Over the empty desktop of strip `strip`, `at` in the strip's own
    /// coordinates: from the left edge of its content (wherever it is
    /// scrolled to) and the top of the area it is shown in.
    Strip {
        /// The strip's id.
        strip: u64,
        /// In the strip's coordinates.
        at: [f32; 2],
    },
}

impl LivePointer {
    /// The point, in whatever it is relative to.
    pub fn at(&self) -> [f32; 2] {
        match self {
            Self::Window { at, .. } | Self::Strip { at, .. } => *at,
        }
    }

    /// The same anchor with the point moved to `at`.
    #[must_use]
    pub fn with_at(&self, at: [f32; 2]) -> Self {
        match self {
            Self::Window { record, .. } => Self::Window {
                record: record.clone(),
                at,
            },
            Self::Strip { strip, .. } => Self::Strip { strip: *strip, at },
        }
    }

    /// Whether `other` is over the same thing, so a point may move smoothly
    /// from one to the other.
    pub fn same_anchor(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Window { record: a, .. }, Self::Window { record: b, .. }) => a == b,
            (Self::Strip { strip: a, .. }, Self::Strip { strip: b, .. }) => a == b,
            _ => false,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        if let Self::Window { record, .. } = self {
            record.validate()?;
        }
        finite(&self.at()).map_err(|_| LiveWireError::Invalid)
    }
}

/// The strip a viewer shows: its id, the area it is drawn in and how far
/// its content is scrolled, in the viewer's logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShownStrip {
    /// The strip's id.
    pub id: u64,
    /// Where the strip is drawn: `[x0, y0, x1, y1]`.
    pub area: [f32; 4],
    /// How far its content is scrolled from its left edge.
    pub scroll_x: f32,
}

/// What a viewer shows that a pointer can be over, in its surface's logical
/// pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PointerLayout {
    /// Each shown window's content area, `[x0, y0, x1, y1]`, by the record
    /// it shows, topmost first.
    pub windows: Vec<(LiveRecord, [f32; 4])>,
    /// The strip shown, if any.
    pub strip: Option<ShownStrip>,
}

fn contains(rect: [f32; 4], [x, y]: [f32; 2]) -> bool {
    (rect[0]..rect[2]).contains(&x) && (rect[1]..rect[3]).contains(&y)
}

impl PointerLayout {
    /// The pointer at `point` on this surface, said relative to what it is
    /// over: the topmost window there, else the shown strip. `None` over
    /// nothing either names (outside the strip, a desktop showing none).
    pub fn anchor(&self, point: [f32; 2]) -> Option<LivePointer> {
        if !point.iter().all(|value| value.is_finite()) {
            return None;
        }
        if let Some((record, rect)) = self.windows.iter().find(|(_, rect)| contains(*rect, point)) {
            return Some(LivePointer::Window {
                record: record.clone(),
                at: [point[0] - rect[0], point[1] - rect[1]],
            });
        }
        let strip = self.strip.filter(|strip| contains(strip.area, point))?;
        Some(LivePointer::Strip {
            strip: strip.id,
            at: [
                point[0] - strip.area[0] + strip.scroll_x,
                point[1] - strip.area[1],
            ],
        })
    }

    /// Where `pointer` is on this surface, or `None` when this viewer does
    /// not show what it is over (a window of a record it has not open, a
    /// strip it is not on, a part of the strip scrolled out of view). A
    /// point past the edge of a window narrower here than where it was
    /// published stays on the window's edge: it is still over that window.
    pub fn place(&self, pointer: &LivePointer) -> Option<[f32; 2]> {
        match pointer {
            LivePointer::Window { record, at } => {
                let (_, rect) = self.windows.iter().find(|(shown, _)| shown == record)?;
                let clamp = |value: f32, low: f32, high: f32| value.min(high).max(low);
                Some([
                    clamp(rect[0] + at[0], rect[0], (rect[2] - 1.0).max(rect[0])),
                    clamp(rect[1] + at[1], rect[1], (rect[3] - 1.0).max(rect[1])),
                ])
            }
            LivePointer::Strip { strip, at } => {
                let shown = self.strip.filter(|shown| shown.id == *strip)?;
                let point = [
                    shown.area[0] + at[0] - shown.scroll_x,
                    shown.area[1] + at[1],
                ];
                contains(shown.area, point).then_some(point)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes() -> LiveRecord {
        LiveRecord {
            app: "so.tilo.notes".into(),
            record: "welcome".into(),
        }
    }

    /// The same room on a wide screen and a narrow, scrolled one: windows
    /// at other places and sizes, the strip scrolled differently.
    fn wide() -> PointerLayout {
        PointerLayout {
            windows: vec![(notes(), [100.0, 80.0, 700.0, 600.0])],
            strip: Some(ShownStrip {
                id: 7,
                area: [0.0, 40.0, 1920.0, 1080.0],
                scroll_x: 0.0,
            }),
        }
    }

    fn narrow() -> PointerLayout {
        PointerLayout {
            windows: vec![(notes(), [20.0, 60.0, 420.0, 500.0])],
            strip: Some(ShownStrip {
                id: 7,
                area: [0.0, 32.0, 800.0, 720.0],
                scroll_x: 300.0,
            }),
        }
    }

    /// A pointer over a window lands on the same spot of that window on a
    /// screen of another size, and one over empty desktop on the same spot
    /// of the strip, however each viewer scrolled it.
    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the points are sums of small whole numbers, exact in f32"
    )]
    fn a_pointer_lands_on_the_same_spot_in_layouts_of_other_sizes() {
        let over_window = wide().anchor([350.0, 200.0]).expect("over the window");
        assert_eq!(
            over_window,
            LivePointer::Window {
                record: notes(),
                at: [250.0, 120.0]
            }
        );
        assert_eq!(narrow().place(&over_window), Some([270.0, 180.0]));
        assert_eq!(
            narrow().anchor([270.0, 180.0]),
            Some(over_window),
            "and back"
        );

        let over_desktop = wide().anchor([900.0, 300.0]).expect("over the strip");
        assert_eq!(
            over_desktop,
            LivePointer::Strip {
                strip: 7,
                at: [900.0, 260.0]
            }
        );
        let placed = narrow()
            .place(&over_desktop)
            .expect("in view on the narrow screen");
        assert_eq!(placed, [600.0, 292.0], "scrolled 300 px further along");
        assert_eq!(narrow().anchor(placed), Some(over_desktop));
    }

    #[test]
    fn what_a_viewer_does_not_show_is_not_placed() {
        let elsewhere = LivePointer::Window {
            record: LiveRecord {
                app: "so.tilo.chat".into(),
                record: String::new(),
            },
            at: [10.0, 10.0],
        };
        assert_eq!(wide().place(&elsewhere), None, "a window not open here");
        let other_strip = LivePointer::Strip {
            strip: 8,
            at: [10.0, 10.0],
        };
        assert_eq!(wide().place(&other_strip), None, "a strip not shown here");
        let scrolled_away = LivePointer::Strip {
            strip: 7,
            at: [100.0, 100.0],
        };
        assert_eq!(
            narrow().place(&scrolled_away),
            None,
            "a part of the strip scrolled out of view"
        );
        assert_eq!(PointerLayout::default().anchor([1.0, 1.0]), None);
    }

    #[test]
    fn a_point_past_a_narrower_window_stays_on_its_edge() {
        let far_right = LivePointer::Window {
            record: notes(),
            at: [950.0, 10.0],
        };
        assert_eq!(narrow().place(&far_right), Some([419.0, 70.0]));
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the points are sums of small whole numbers, exact in f32"
    )]
    fn anchors_are_compared_by_what_they_are_over() {
        let a = LivePointer::Strip {
            strip: 7,
            at: [1.0, 2.0],
        };
        assert!(a.same_anchor(&a.with_at([5.0, 6.0])));
        assert_eq!(a.with_at([5.0, 6.0]).at(), [5.0, 6.0]);
        assert!(!a.same_anchor(&LivePointer::Window {
            record: notes(),
            at: [1.0, 2.0]
        }));
    }
}
