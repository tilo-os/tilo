//! Where a pointer is, said relative to what it is over, so every viewer
//! places it in their own layout.
//!
//! People in one room rarely share a screen size: the same window is wider
//! on one display than another, and each viewer scrolls a strip on their own.
//! A point in one page's surface pixels lands somewhere else on every other
//! page. A pointer therefore travels relative to a window, named by the
//! record it shows, which every member names alike: a point within the
//! window it is over, or, over the empty desktop, a point beside the nearest
//! window. Strips are no common reference: where a room's layout is personal,
//! every member's strips are their own, with ids of their own. Only on a
//! desk showing no window at all does a pointer fall back to the strip's own
//! coordinates, which a member showing the same strip places. Each viewer
//! maps a pointer back through [`PointerLayout::place`], and does not draw
//! what it does not show.

use serde::{Deserialize, Serialize};

use super::{LiveRecord, LiveWireError, Result, finite};

/// What a pointer is over, and where within it, in logical pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum LivePointer {
    /// Over the window showing `record`, `at` from the top-left corner of
    /// its content.
    Window {
        record: LiveRecord,
        /// From the window content's top-left corner.
        at: [f32; 2],
    },
    /// Over the empty desktop beside the window showing `record`: from
    /// the point of the window nearest to it, which is `edge` across the
    /// window's content (fractions of its width and height, each 0 to 1),
    /// by `at`. Right of a window, `edge` is `[1, y]` and `at` `[dx, 0]`:
    /// a narrower window elsewhere keeps the pointer as far right of it.
    Beside {
        record: LiveRecord,
        /// The window's point nearest to the pointer, as fractions of its
        /// content's size.
        edge: [f32; 2],
        /// From that point.
        at: [f32; 2],
    },
    /// Over the empty desktop of strip `strip` showing no window, `at` in
    /// the strip's own coordinates: from the left edge of its content
    /// (wherever it is scrolled to) and the top of the area it is shown in.
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
            Self::Window { at, .. } | Self::Beside { at, .. } | Self::Strip { at, .. } => *at,
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
            Self::Beside { record, edge, .. } => Self::Beside {
                record: record.clone(),
                edge: *edge,
                at,
            },
            Self::Strip { strip, .. } => Self::Strip { strip: *strip, at },
        }
    }

    /// Whether `other` is over the same thing, so a point may move smoothly
    /// from one to the other.
    pub fn same_anchor(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Window { record: a, .. }, Self::Window { record: b, .. })
            | (Self::Beside { record: a, .. }, Self::Beside { record: b, .. }) => a == b,
            (Self::Strip { strip: a, .. }, Self::Strip { strip: b, .. }) => a == b,
            _ => false,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        match self {
            Self::Window { record, .. } => record.validate()?,
            Self::Beside { record, edge, .. } => {
                record.validate()?;
                if !edge.iter().all(|fraction| (0.0..=1.0).contains(fraction)) {
                    return Err(LiveWireError::Invalid);
                }
            }
            Self::Strip { .. } => {}
        }
        finite(&self.at()).map_err(|_| LiveWireError::Invalid)
    }
}

/// The strip a viewer shows: its id, the area it is drawn in and how far
/// its content is scrolled, in the viewer's logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShownStrip {
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
    pub strip: Option<ShownStrip>,
}

fn contains(rect: [f32; 4], [x, y]: [f32; 2]) -> bool {
    (rect[0]..rect[2]).contains(&x) && (rect[1]..rect[3]).contains(&y)
}

impl PointerLayout {
    /// The pointer at `point` on this surface, said relative to what it is
    /// over: the topmost window there, else beside the nearest window, else
    /// (no window shown) the shown strip. `None` over nothing these name
    /// (outside the strip of a desktop showing no window).
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
        let nearest = self
            .windows
            .iter()
            .filter(|(_, rect)| rect[2] > rect[0] && rect[3] > rect[1])
            .map(|(record, rect)| {
                let near = [
                    point[0].clamp(rect[0], rect[2]),
                    point[1].clamp(rect[1], rect[3]),
                ];
                let away = [point[0] - near[0], point[1] - near[1]];
                (record, rect, near, away)
            })
            .min_by(|a, b| {
                let distance = |away: [f32; 2]| away[0].hypot(away[1]);
                distance(a.3).total_cmp(&distance(b.3))
            });
        if let Some((record, rect, near, away)) = nearest {
            return Some(LivePointer::Beside {
                record: record.clone(),
                edge: [
                    (near[0] - rect[0]) / (rect[2] - rect[0]),
                    (near[1] - rect[1]) / (rect[3] - rect[1]),
                ],
                at: away,
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
    /// not show what it is relative to (a window of a record it has not
    /// open, a strip it is not on, a part of the strip scrolled out of
    /// view). A point beside a window may land off this surface; the
    /// viewer draws only what lands on it. A
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
            LivePointer::Beside { record, edge, at } => {
                let (_, rect) = self.windows.iter().find(|(shown, _)| shown == record)?;
                Some([
                    rect[0] + edge[0] * (rect[2] - rect[0]) + at[0],
                    rect[1] + edge[1] * (rect[3] - rect[1]) + at[1],
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
#[expect(
    clippy::float_cmp,
    reason = "the points are sums and products of small binary fractions, exact in f32"
)]
mod tests {
    use super::*;

    fn notes() -> LiveRecord {
        LiveRecord {
            app: "so.tilo.notes".into(),
            record: "welcome".into(),
        }
    }

    fn chat() -> LiveRecord {
        LiveRecord {
            app: "so.tilo.chat".into(),
            record: String::new(),
        }
    }

    /// One room on a wide screen and a narrow one whose layout is personal:
    /// the same records' windows at other places and sizes, on strips of
    /// each member's own, with ids of their own.
    fn wide() -> PointerLayout {
        PointerLayout {
            windows: vec![
                (notes(), [100.0, 80.0, 700.0, 600.0]),
                (chat(), [708.0, 80.0, 1308.0, 600.0]),
            ],
            strip: Some(ShownStrip {
                id: 7,
                area: [0.0, 40.0, 1920.0, 1080.0],
                scroll_x: 0.0,
            }),
        }
    }

    fn narrow() -> PointerLayout {
        PointerLayout {
            windows: vec![
                (notes(), [20.0, 60.0, 420.0, 580.0]),
                (chat(), [428.0, 60.0, 828.0, 580.0]),
            ],
            strip: Some(ShownStrip {
                id: 9,
                area: [0.0, 32.0, 1024.0, 720.0],
                scroll_x: 0.0,
            }),
        }
    }

    /// A pointer over a window lands on the same spot of that window on a
    /// screen of another size, and one over the empty desktop as far beside
    /// the nearest window, though the two members' strips have nothing in
    /// common.
    #[test]
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

        // 44 px right of the chat window, halfway down it.
        let over_desktop = wide().anchor([1352.0, 340.0]).expect("beside a window");
        assert_eq!(
            over_desktop,
            LivePointer::Beside {
                record: chat(),
                edge: [1.0, 0.5],
                at: [44.0, 0.0]
            }
        );
        let placed = narrow()
            .place(&over_desktop)
            .expect("the narrow screen shows the chat");
        assert_eq!(
            placed,
            [872.0, 320.0],
            "44 px right of its chat, halfway down"
        );
        assert_eq!(narrow().anchor(placed), Some(over_desktop), "and back");

        // Above and left of the first window: from its corner.
        let corner = wide().anchor([60.0, 50.0]).expect("beside a window");
        assert_eq!(
            corner,
            LivePointer::Beside {
                record: notes(),
                edge: [0.0, 0.0],
                at: [-40.0, -30.0]
            }
        );
        assert_eq!(
            narrow().place(&corner),
            Some([-20.0, 30.0]),
            "off the narrow screen"
        );
    }

    #[test]
    fn what_a_viewer_does_not_show_is_not_placed() {
        let sheets = LiveRecord {
            app: "so.tilo.sheets".into(),
            record: String::new(),
        };
        let elsewhere = LivePointer::Window {
            record: sheets.clone(),
            at: [10.0, 10.0],
        };
        assert_eq!(wide().place(&elsewhere), None, "a window not open here");
        let beside_elsewhere = LivePointer::Beside {
            record: sheets,
            edge: [1.0, 0.0],
            at: [10.0, 0.0],
        };
        assert_eq!(wide().place(&beside_elsewhere), None);
        assert_eq!(PointerLayout::default().anchor([1.0, 1.0]), None);
    }

    /// A desk showing no window has only its strip to say where a pointer
    /// is: a member showing that strip places it, however scrolled; one
    /// whose strips are their own does not.
    #[test]
    fn an_empty_desk_points_in_its_strip() {
        let empty = |id, scroll_x| PointerLayout {
            windows: Vec::new(),
            strip: Some(ShownStrip {
                id,
                area: [0.0, 40.0, 1000.0, 800.0],
                scroll_x,
            }),
        };
        let pointer = empty(7, 0.0).anchor([600.0, 300.0]).expect("on the strip");
        assert_eq!(
            pointer,
            LivePointer::Strip {
                strip: 7,
                at: [600.0, 260.0]
            }
        );
        assert_eq!(empty(7, 200.0).place(&pointer), Some([400.0, 300.0]));
        assert_eq!(
            empty(7, 700.0).place(&pointer),
            None,
            "scrolled out of view"
        );
        assert_eq!(
            empty(9, 0.0).place(&pointer),
            None,
            "another member's own strip"
        );
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
    fn anchors_are_compared_by_what_they_are_over() {
        let a = LivePointer::Beside {
            record: notes(),
            edge: [1.0, 0.25],
            at: [1.0, 2.0],
        };
        assert!(a.same_anchor(&a.with_at([5.0, 6.0])));
        assert_eq!(a.with_at([5.0, 6.0]).at(), [5.0, 6.0]);
        assert!(!a.same_anchor(&LivePointer::Window {
            record: notes(),
            at: [1.0, 2.0]
        }));
        let bad_edge = LivePointer::Beside {
            record: notes(),
            edge: [1.5, 0.0],
            at: [1.0, 2.0],
        };
        assert!(bad_edge.validate().is_err());
    }
}
