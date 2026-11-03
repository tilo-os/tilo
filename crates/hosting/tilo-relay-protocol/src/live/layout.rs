//! Where a member's windows are on their screen, for a mini view of it that
//! others draw from their own copies of what is shared.
//!
//! A layout names each window by its app and a short digest of its live
//! record ([`super::LiveRecord::short_digest`]), never by title or record
//! id: a receiver learns only the app of a window on something not shared
//! with it, and matches the digest against records it already holds to
//! show the rest from its own replica. Nothing is streamed.

use serde::{Deserialize, Serialize};

use super::LiveRecord;

/// Most windows a layout carries: the focused one and those beside it.
pub const MAX_LAYOUT_WINDOWS: usize = 8;
/// Largest app id a layout window names.
pub const MAX_LAYOUT_APP_BYTES: usize = 48;
/// A window's edges are in this many parts of the screen's width or height.
pub const LAYOUT_UNITS: u16 = 10_000;

/// The windows on a member's screen, in their own arrangement.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveLayout {
    /// The screen's content area (under the bar), logical pixels: the shape
    /// the windows are placed in.
    pub size: [u16; 2],
    /// At most [`MAX_LAYOUT_WINDOWS`], back to front.
    pub windows: Vec<LiveWindow>,
}

/// One window of a [`LiveLayout`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveWindow {
    /// The app it shows, 1..=[`MAX_LAYOUT_APP_BYTES`] bytes.
    pub app: String,
    /// [`LiveRecord::short_digest`] of the record it shows.
    pub record: [u8; 8],
    /// `x0, y0, x1, y1` in [`LAYOUT_UNITS`] parts of the layout's size.
    pub rect: [u16; 4],
    /// The window the member is in.
    pub focused: bool,
}

/// A mini view needs no finer edges than a hundredth of the screen, and
/// coarser edges change less often while a strip pans: fewer frames.
const LAYOUT_STEPS: u16 = 100;

impl LiveLayout {
    /// The layout of `windows` (each its record, its rect `[x0, y0, x1, y1]`
    /// and whether it is focused) on a screen whose content area is `area`,
    /// in the same coordinates: windows outside the area are left out and
    /// the rest clipped to it, edges in hundredths of it; a window whose app
    /// the wire cannot carry is left out. At most [`MAX_LAYOUT_WINDOWS`],
    /// the focused one always among them. `None` for an area of no size.
    /// Native shells and browser tabs place their windows by this one rule.
    #[must_use]
    pub fn place(area: [f64; 4], windows: &[(LiveRecord, [f64; 4], bool)]) -> Option<Self> {
        let [ax0, ay0, ax1, ay1] = area;
        let (width, height) = (ax1 - ax0, ay1 - ay0);
        if !(width >= 1.0 && height >= 1.0) {
            return None;
        }
        let units = |value: f64, origin: f64, extent: f64| {
            let fraction = ((value - origin) / extent).clamp(0.0, 1.0);
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a fraction of 0..=1 scaled to 0..=100"
            )]
            let steps = (fraction * f64::from(LAYOUT_STEPS)).round() as u16;
            steps.saturating_mul(LAYOUT_UNITS / LAYOUT_STEPS)
        };
        let mut placed: Vec<LiveWindow> = windows
            .iter()
            .filter_map(|(record, [x0, y0, x1, y1], focused)| {
                let (x0, y0) = (x0.max(ax0), y0.max(ay0));
                let (x1, y1) = (x1.min(ax1), y1.min(ay1));
                if !(x1 - x0 >= 1.0 && y1 - y0 >= 1.0)
                    || record.app.is_empty()
                    || record.app.len() > MAX_LAYOUT_APP_BYTES
                {
                    return None;
                }
                Some(LiveWindow {
                    app: record.app.clone(),
                    record: record.short_digest(),
                    rect: [
                        units(x0, ax0, width),
                        units(y0, ay0, height),
                        units(x1, ax0, width),
                        units(y1, ay0, height),
                    ],
                    focused: *focused,
                })
            })
            .collect();
        // Too many to carry: the focused window stays, then the first others.
        if placed.len() > MAX_LAYOUT_WINDOWS {
            let room =
                MAX_LAYOUT_WINDOWS.saturating_sub(usize::from(placed.iter().any(|w| w.focused)));
            let mut others = 0_usize;
            placed.retain(|window| {
                others = others.saturating_add(usize::from(!window.focused));
                window.focused || others <= room
            });
            placed.truncate(MAX_LAYOUT_WINDOWS);
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to the u16 range first"
        )]
        let side = |value: f64| value.round().clamp(0.0, f64::from(u16::MAX)) as u16;
        Some(Self {
            size: [side(width), side(height)],
            windows: placed,
        })
    }
}

impl LiveRecord {
    /// The first 8 bytes of [`Self::digest`]: what a layout names a window's
    /// record by, enough to find it among the records a receiver holds.
    #[must_use]
    pub fn short_digest(&self) -> [u8; 8] {
        let digest = self.digest();
        let mut short = [0; 8];
        short.copy_from_slice(&digest[..8]);
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layout names each window by its app and record digest: what a
    /// receiver can match against what it holds, and nothing more.
    #[test]
    fn a_layout_names_windows_by_app_and_digest() {
        let area = [0.0, 0.0, 1440.0, 852.0];
        let layout = LiveLayout::place(
            area,
            &[
                (record("r1"), [0.0, 0.0, 500.0, 10_000.0], false),
                (record("r2"), [720.0, 0.0, 1440.0, 852.0], true),
            ],
        )
        .expect("a screen has a size");
        assert_eq!(layout.size, [1440, 852]);
        let named: Vec<_> = layout
            .windows
            .iter()
            .map(|window| (window.app.as_str(), window.record, window.focused))
            .collect();
        assert_eq!(
            named,
            [
                ("so.tilo.notes", record("r1").short_digest(), false),
                ("so.tilo.notes", record("r2").short_digest(), true),
            ]
        );
    }

    fn record(name: &str) -> LiveRecord {
        LiveRecord {
            app: "so.tilo.notes".into(),
            record: name.into(),
        }
    }

    #[test]
    fn windows_are_placed_in_the_content_area_in_hundredths() {
        let area = [0.0, 40.0, 1000.0, 540.0];
        let layout = LiveLayout::place(
            area,
            &[
                (record("r1"), [0.0, 40.0, 503.0, 540.0], false),
                // Panned partly out of view: clipped to what shows.
                (record("r2"), [750.0, 40.0, 1250.0, 540.0], true),
                // Wholly out of view, or of an app the wire cannot carry.
                (record("r3"), [1100.0, 40.0, 1500.0, 540.0], false),
                (
                    LiveRecord {
                        app: "a".repeat(MAX_LAYOUT_APP_BYTES + 1),
                        record: "r4".into(),
                    },
                    [0.0, 40.0, 100.0, 100.0],
                    false,
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            layout,
            LiveLayout {
                size: [1000, 500],
                windows: vec![
                    LiveWindow {
                        app: "so.tilo.notes".into(),
                        record: record("r1").short_digest(),
                        rect: [0, 0, 5000, 10_000],
                        focused: false,
                    },
                    LiveWindow {
                        app: "so.tilo.notes".into(),
                        record: record("r2").short_digest(),
                        rect: [7500, 0, 10_000, 10_000],
                        focused: true,
                    },
                ],
            }
        );
        assert_eq!(LiveLayout::place([0.0, 0.0, 0.0, 10.0], &[]), None);
    }

    #[test]
    fn the_focused_window_is_kept_when_there_are_too_many() {
        let windows: Vec<_> = (1..=12)
            .map(|k| (record(&format!("r{k}")), [0.0, 0.0, 10.0, 10.0], k == 12))
            .collect();
        let layout = LiveLayout::place([0.0, 0.0, 1000.0, 500.0], &windows).unwrap();
        assert_eq!(layout.windows.len(), MAX_LAYOUT_WINDOWS);
        assert!(layout.windows.last().unwrap().focused);
        assert_eq!(layout.windows.iter().filter(|w| w.focused).count(), 1);
    }

    #[test]
    fn a_short_digest_is_the_head_of_the_digest() {
        let record = LiveRecord {
            app: "so.tilo.notes".into(),
            record: "r1".into(),
        };
        assert_eq!(record.short_digest()[..], record.digest()[..8]);
    }
}
