//! The install's status log: what the installer reports, newest last.

use ui::a11y::Politeness;
use ui::theme::Theme;
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{Card, Column, Element, Scrollable, Text};

const VISIBLE_LINES: u16 = 8;

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
