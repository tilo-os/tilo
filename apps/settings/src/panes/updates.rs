//! Updates pane — the system update manager surface.
//!
//! qOS is an immutable bootc image: updates are atomic image pulls staged
//! for the next boot, not piecemeal package upgrades. This pane shows the
//! current deployment, whether a newer image is staged, the release
//! channel, and a "Check for updates" action.
//!
//! The deployment/version reads come from `/etc/os-release` where present;
//! the "staged update" status is sampled (a real deployment reads
//! `bootc status` / rpm-ostree). The shape matches what that reader
//! produces so wiring it live only changes [`UpdateStatus::from_system`].

use std::fs;
use std::path::Path;

use ui::widget::{Button, Column, Dropdown, Row, Spacer, Text, Toggle, Widget};

use crate::widgets::{card, caption, rows, setting_row, value_row, El};

pub const CHECK_KEY: &str = "updates.check";
pub const AUTO_KEY: &str = "updates.auto";
pub const CHANNEL_KEY: &str = "updates.channel";

/// Snapshot of the update state shown in the pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateStatus {
    /// Current image / OS pretty name.
    pub current_version: String,
    /// A staged-for-next-boot version, if any.
    pub staged_version: Option<String>,
    pub channel: String,
    pub last_checked: String,
}

impl UpdateStatus {
    /// Sample the system: pretty name from `/etc/os-release`; staged
    /// status is the sample (real reader: `bootc status`).
    pub fn from_system() -> Self {
        let current = fs::read_to_string(Path::new("/etc/os-release"))
            .ok()
            .and_then(|s| parse_pretty_name(&s))
            .unwrap_or_else(|| "qOS (Fedora bootc)".to_string());
        Self {
            current_version: current,
            staged_version: Some("qOS 2026.07 (build 1842)".to_string()),
            channel: "Stable".to_string(),
            last_checked: "Today, 13:40".to_string(),
        }
    }
}

fn parse_pretty_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(rest) = line.trim().strip_prefix("PRETTY_NAME=") {
            let v = rest.trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

pub fn build() -> Box<dyn Widget> {
    build_with(&UpdateStatus::from_system())
}

pub fn build_with(status: &UpdateStatus) -> Box<dyn Widget> {
    // Status card — current + staged + a primary action.
    let action: El = match &status.staged_version {
        Some(staged) => Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(
                    Text::new(&format!("Update ready: {staged}")).with_size(13.0),
                ))
                .push(Box::new(
                    Text::new("Restart to apply the staged image.")
                        .with_size(11.0)
                        .with_color(ui::Theme::dark().text_dim),
                ))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(Button::new(
                            "settings.updates.restart",
                            "\u{f0709}  Restart & update",
                            Some("updates.restart"),
                        )))
                        .push(Box::new(Spacer::new())),
                )),
        ),
        None => Box::new(Text::new("Your system is up to date.").with_size(13.0)),
    };

    let status_card = card("Software updates", action);

    let detail = card(
        "Details",
        rows(vec![
            value_row("Current version", &status.current_version),
            value_row("Last checked", &status.last_checked),
            setting_row(
                "Release channel",
                Box::new(
                    Dropdown::new(
                        vec!["Stable".into(), "Testing".into(), "Nightly".into()],
                        CHANNEL_KEY,
                    )
                    .with_id("updates.channel")
                    .with_placeholder(&status.channel),
                ),
            ),
            setting_row(
                "Automatic updates",
                Box::new(Toggle::new("settings.updates.auto", AUTO_KEY).with_id("updates.auto")),
            ),
            Box::new(
                Row::new()
                    .push(Box::new(Button::new(
                        "settings.updates.check",
                        "\u{f0450}  Check for updates",
                        Some(CHECK_KEY),
                    )))
                    .push(Box::new(Spacer::new())),
            ),
        ]),
    );

    let note = card(
        "How updates work",
        rows(vec![caption(
            "qOS updates are atomic: a new image is staged and applied on \
             restart, so an update never half-installs. If something is \
             wrong you can roll back to the previous image at boot.",
        )]),
    );

    crate::widgets::cards(vec![status_card, detail, note])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    fn sample() -> UpdateStatus {
        UpdateStatus {
            current_version: "qOS 2026.06".into(),
            staged_version: Some("qOS 2026.07".into()),
            channel: "Stable".into(),
            last_checked: "Today".into(),
        }
    }

    #[test]
    fn parses_pretty_name() {
        let s = "PRETTY_NAME=\"qOS 42\"\n";
        assert_eq!(parse_pretty_name(s).as_deref(), Some("qOS 42"));
    }

    #[test]
    fn builds_layout_with_and_without_staged_update() {
        let theme = Theme::dark();
        for staged in [Some("v2".to_string()), None] {
            let mut st = sample();
            st.staged_version = staged;
            let mut pane = build_with(&st);
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
        }
    }
}
