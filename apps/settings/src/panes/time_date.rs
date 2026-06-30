//! Time & Date pane — clock, time zone, and date/time display format.
//!
//! The clock-format controls here are a *local* view onto the same locale
//! concepts the Region & Formats pane owns globally; this pane focuses on
//! the clock surface (automatic time, 24-hour clock, time zone) while
//! Region owns the system-wide currency/date/measurement contract.
//!
//! Automatic-time and time-zone reads are sampled from the system where
//! cheap (`/etc/timezone` if present); otherwise a realistic sample is
//! shown so the panel reads as designed under `--shots`.

use std::fs;
use std::path::Path;

use ui::widget::{Toggle, Widget};

use crate::widgets::{card, caption, rows, setting_row, value_row};

pub const AUTO_TIME_KEY: &str = "time.auto";
pub const H24_KEY: &str = "time.h24";

/// Read the configured time zone from `/etc/timezone`, falling back to a
/// sample so the pane is never blank.
fn timezone() -> String {
    for p in ["/etc/timezone"] {
        if let Ok(s) = fs::read_to_string(Path::new(p)) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    // `/etc/localtime` is usually a symlink into zoneinfo; derive the zone
    // from its target when `/etc/timezone` is absent (common on Fedora).
    if let Ok(target) = fs::read_link("/etc/localtime") {
        if let Some(idx) = target.to_string_lossy().find("zoneinfo/") {
            return target.to_string_lossy()[idx + "zoneinfo/".len()..].to_string();
        }
    }
    "Europe/Amsterdam".to_string()
}

pub fn build() -> Box<dyn Widget> {
    let tz = timezone();

    let clock = card(
        "Clock",
        rows(vec![
            setting_row(
                "Set time automatically",
                Box::new(Toggle::new("settings.time.auto", AUTO_TIME_KEY).with_id("time.auto")),
            ),
            setting_row(
                "24-hour clock",
                Box::new(Toggle::new("settings.time.h24", H24_KEY).with_id("time.h24")),
            ),
            value_row("Time zone", &tz),
            value_row("Current time", "14:05"),
            value_row("Current date", "Tuesday, 30 June 2026"),
        ]),
    );

    let formats = card(
        "Display",
        rows(vec![
            value_row("Date format", "ISO (2023-03-15)"),
            value_row("First day of week", "Monday"),
            caption(
                "Date and number formats are set system-wide in \
                 Region & Formats; this pane shows the clock surface.",
            ),
        ]),
    );

    Box::new(
        ui::widget::Column::new()
            .with_intrinsic_height()
            .push(clock)
            .push(formats),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    #[test]
    fn builds_layout() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    #[test]
    fn timezone_is_never_empty() {
        assert!(!timezone().is_empty());
    }
}
