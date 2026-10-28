//! Where someone's windows are on their screen, as their live presence says
//! (`tilo_relay_protocol::live::LiveLayout`): what a mini view of their
//! screen is drawn from. Each window is named by its app and a short digest
//! of its record only, which a viewer matches against the records it holds
//! itself; nothing of the window's content travels.

use vello::kurbo::Rect;

/// A window's edges are in this many parts of the screen's width or height.
const UNITS: f64 = 10_000.0;
/// Most windows a screen shows, as the wire carries at most.
const MAX_WINDOWS: usize = 8;

/// A member's layout as the page passes it: `{size, windows: [{app, record,
/// rect, focused}]}`, `record` 16 hex digits, edges in 1/10000 of `size`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct MemberLayout {
    #[serde(default)]
    pub size: [u16; 2],
    #[serde(default)]
    pub windows: Vec<MemberWindow>,
}

/// One window of a [`MemberLayout`].
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct MemberWindow {
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub record: String,
    #[serde(default)]
    pub rect: [u16; 4],
    #[serde(default)]
    pub focused: bool,
}

/// Someone's screen: its shape and their windows on it, back to front.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    /// The screen's content area, logical pixels.
    pub size: (f64, f64),
    pub windows: Vec<ScreenWindow>,
}

/// One window on someone's [`Screen`].
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenWindow {
    /// The app it shows.
    pub app: String,
    /// The short digest of the record it shows
    /// (`tilo_relay_protocol::live::LiveRecord::short_digest`).
    pub record: [u8; 8],
    /// Where it is, in the unit square of the screen.
    pub rect: Rect,
    /// The window they are in.
    pub focused: bool,
}

impl Screen {
    /// The screen `layout` describes; malformed windows are left out, and
    /// a layout of no size is none.
    #[must_use]
    pub fn from_layout(layout: &MemberLayout) -> Option<Self> {
        let [width, height] = layout.size;
        if width == 0 || height == 0 {
            return None;
        }
        let windows = layout
            .windows
            .iter()
            .filter_map(ScreenWindow::from_member)
            .take(MAX_WINDOWS)
            .collect();
        Some(Self {
            size: (f64::from(width), f64::from(height)),
            windows,
        })
    }
}

impl ScreenWindow {
    fn from_member(window: &MemberWindow) -> Option<Self> {
        let record = <[u8; 8]>::try_from(hex::decode(&window.record).ok()?).ok()?;
        let [x0, y0, x1, y1] = window.rect.map(f64::from);
        if window.app.is_empty() || x0 > x1 || y0 > y1 || x1 > UNITS || y1 > UNITS {
            return None;
        }
        Some(Self {
            app: window.app.clone(),
            record,
            rect: Rect::new(x0 / UNITS, y0 / UNITS, x1 / UNITS, y1 / UNITS),
            focused: window.focused,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(app: &str, record: &str, rect: [u16; 4]) -> MemberWindow {
        MemberWindow {
            app: app.into(),
            record: record.into(),
            rect,
            focused: false,
        }
    }

    #[test]
    fn a_layout_becomes_windows_in_the_unit_square() {
        let screen = Screen::from_layout(&MemberLayout {
            size: [1440, 852],
            windows: vec![
                MemberWindow {
                    focused: true,
                    ..window("so.tilo.notes", "0123456789abcdef", [0, 0, 5000, 10_000])
                },
                window(
                    "so.tilo.chat",
                    "fedcba9876543210",
                    [5000, 2500, 10_000, 7500],
                ),
            ],
        })
        .unwrap();
        assert_eq!(screen.size, (1440.0, 852.0));
        assert_eq!(
            screen.windows,
            vec![
                ScreenWindow {
                    app: "so.tilo.notes".into(),
                    record: [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef],
                    rect: Rect::new(0.0, 0.0, 0.5, 1.0),
                    focused: true,
                },
                ScreenWindow {
                    app: "so.tilo.chat".into(),
                    record: [0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10],
                    rect: Rect::new(0.5, 0.25, 1.0, 0.75),
                    focused: false,
                },
            ]
        );
    }

    #[test]
    fn malformed_windows_are_left_out_and_a_sizeless_screen_is_none() {
        let screen = Screen::from_layout(&MemberLayout {
            size: [100, 100],
            windows: vec![
                window("", "0123456789abcdef", [0, 0, 1, 1]),
                window("a", "not hex", [0, 0, 1, 1]),
                window("a", "0123", [0, 0, 1, 1]),
                window("a", "0123456789abcdef", [5, 0, 1, 1]),
                window("a", "0123456789abcdef", [0, 0, 10_001, 1]),
                window("ok", "0123456789abcdef", [0, 0, 1, 1]),
            ],
        })
        .unwrap();
        assert_eq!(screen.windows.len(), 1);
        assert_eq!(screen.windows[0].app, "ok");
        assert_eq!(
            Screen::from_layout(&MemberLayout {
                size: [0, 10],
                windows: Vec::new()
            }),
            None
        );
    }
}
