//! `channel_browser` example — eyeball the Phase 5 channel-browser
//! modal sheet with seeded mock entries.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p ui --example channel_browser
//! ```
//!
//! Notes:
//! - No broker / wheel-disc / shell integration — the widget is pure
//!   UI and consumes `Vec<ChannelEntry>` directly. The wheel-disc
//!   spoke overflow that opens this in production lives in `shell`
//!   and is blocked on the Phase-5 follow-up.
//! - Click a space-filter pill to narrow; arrow keys cycle through
//!   pills + rows; Enter prints the selected channel; Esc dismisses.

use std::time::{Duration, SystemTime};

use ui::widget::channel_browser::{ChannelBrowser, ChannelEntry};
use ui::{App, State};
use qos_collab_types::ChannelId;

fn ch(
    name: &str,
    space: &str,
    hue: f32,
    minutes_ago: u64,
    unread: u32,
    pinned: bool,
    role: Option<&str>,
) -> ChannelEntry {
    ChannelEntry {
        channel_id: ChannelId::new(),
        space_name: space.into(),
        space_color_hue: hue,
        name: name.into(),
        last_activity: SystemTime::now() - Duration::from_secs(minutes_ago * 60),
        unread_count: unread,
        is_pinned: pinned,
        role_label: role.map(str::to_string),
    }
}

fn main() {
    // Mocked entries — two spaces, a mix of roles, pins, and unread
    // counts. Matches the shape the shell will produce in Phase 5
    // once the wheel-disc auto-promote algorithm runs.
    let entries = vec![
        ch("engineering", "Company", 220.0, 5, 12, true, Some("Editor")),
        ch("design", "Company", 220.0, 25, 0, false, Some("Editor")),
        ch("ops", "Company", 220.0, 90, 3, false, Some("Owner")),
        ch("marketing", "Company", 220.0, 240, 0, false, Some("Viewer")),
        ch("standup", "Company", 220.0, 8, 1, false, Some("Editor")),
        ch("journal", "Personal", 30.0, 60, 0, true, None),
        ch("recipes", "Personal", 30.0, 720, 0, false, None),
        ch("reading-list", "Personal", 30.0, 1440, 5, false, None),
        ch("side-project", "Side hustles", 130.0, 180, 2, false, Some("Owner")),
    ];

    let state = State::new();

    let browser = ChannelBrowser::with_entries(entries)
        .on_select(|e| {
            eprintln!(
                "selected: {} (space={}, unread={})",
                e.name, e.space_name, e.unread_count
            );
        })
        .on_dismiss(|| {
            eprintln!("dismissed");
        });

    let app = App::new(state, Box::new(browser));
    app.run();
}
