//! The install's status log: what the installer reports, newest last.
//!
//! The log is a card holding the reported lines as text, in a scroll region
//! a few lines tall that keeps to its end as lines arrive, so the newest
//! progress is always in view and the person can scroll back through the
//! rest. The newest line is a live region: assistive technology hears each
//! step as it is reported, at once for an error or a request to touch the
//! security key.

use ui::a11y::Politeness;
use ui::theme::Theme;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{Card, Column, Element, Scrollable, Text};

/// How many lines of the log are in view.
const VISIBLE_LINES: u16 = 8;

/// The politeness the newest `line` is announced with.
fn politeness(line: &str) -> Politeness {
    let lower = line.to_ascii_lowercase();
    if lower.contains("error")
        || lower.contains("failed")
        || lower.contains("touch your security key")
    {
        Politeness::Assertive
    } else {
        Politeness::Polite
    }
}

/// The log of `lines`, laid out for `theme`.
pub fn status_log(lines: &[String], theme: &Theme) -> Element {
    let newest = lines.iter().rposition(|line| !line.trim().is_empty());
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related);
    for (index, line) in lines.iter().enumerate() {
        let mut text = Text::new(line).with_role(TextRole::Caption);
        if Some(index) == newest {
            text = text
                .with_id("installer.install-status")
                .with_live(politeness(line));
        }
        column = column.push(Box::new(text));
    }
    let line_height = theme.text(TextRole::Caption).line_height_px();
    Box::new(Card::new(Box::new(
        Scrollable::new_vertical()
            .following_end()
            .with_semantic_key("install_status_log")
            .with_max_height(line_height * f32::from(VISIBLE_LINES))
            .with_child(Box::new(column)),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announced(widget: &dyn ui::widget::Widget) -> Option<ui::a11y::AccessNode> {
        if widget.test_id() == Some("installer.install-status") {
            return widget.accessibility();
        }
        widget.children().into_iter().find_map(announced)
    }

    #[test]
    fn the_newest_line_is_announced_by_its_severity() {
        let theme = Theme::dark();
        {
            let log = status_log(
                &[
                    "Preparing encrypted storage…".into(),
                    "Creating your account…".into(),
                    String::new(),
                ],
                &theme,
            );
            let node = announced(log.as_ref()).expect("status semantics");
            assert_eq!(node.name.as_deref(), Some("Creating your account…"));
            assert_eq!(node.live, Some(Politeness::Polite));
        }
        for line in [
            "error: target identity changed",
            "Touch your security key to continue…",
        ] {
            let log = status_log(&[line.into()], &theme);
            assert_eq!(
                announced(log.as_ref()).and_then(|node| node.live),
                Some(Politeness::Assertive),
                "did not announce {line:?} immediately"
            );
        }
    }
}
