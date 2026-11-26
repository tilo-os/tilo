//! A failure's Details, open under its state: the lines a service or a launch answered, each on
//! its own line and left-aligned, in the order they were said. A line indented four spaces (or a
//! tab) is something to type or paste somewhere, a command or a line for a file: it is set apart in
//! the code style, in a well of its own, with a Copy action that copies exactly that line.

use vello::kurbo::{Point, Rect};

use tilo_ui_base::{text_render::TextRenderer, text_render::TextStyle, text_render::TextWrapMode};
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;

/// What one line of Details is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// Words to read.
    Words,
    /// Something to type or paste, exactly as written.
    Verbatim,
}

/// One line of Details where the layout put it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Line {
    pub kind: Kind,
    pub text: String,
    /// The line's box: its words, or the well of a verbatim line.
    pub rect: Rect,
    /// Where its text starts.
    pub origin: Point,
    /// The width its text wraps at.
    pub wrap: f64,
    /// The Copy action of a verbatim line.
    pub copy: Option<Rect>,
}

/// The lines of `details`, blank ones left out.
fn split(details: &str) -> impl Iterator<Item = (Kind, &str)> {
    details.lines().filter_map(|raw| {
        if raw.trim().is_empty() {
            None
        } else if let Some(verbatim) = raw.strip_prefix("    ").or_else(|| raw.strip_prefix('\t')) {
            Some((Kind::Verbatim, verbatim.trim()))
        } else {
            Some((Kind::Words, raw.trim()))
        }
    })
}

/// How Details are set, in a theme.
#[derive(Clone, Debug)]
pub(super) struct Style {
    pub words: TextStyle,
    pub verbatim: TextStyle,
    line_gap: f64,
    pad_x: f64,
    pad_y: f64,
    pub copy: f64,
    pub icon: f64,
    pub radius: f64,
}

impl Style {
    pub fn new(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        Self {
            words: theme
                .text(TextRole::Caption)
                .with_wrap_mode(TextWrapMode::WordOrGlyph),
            verbatim: theme
                .text(TextRole::Code)
                .with_wrap_mode(TextWrapMode::WordOrGlyph),
            line_gap: f64::from(spacing.s2),
            pad_x: f64::from(spacing.s2),
            pad_y: f64::from(spacing.s1),
            copy: f64::from(metrics.bar_control_height),
            icon: f64::from(metrics.icon_sm),
            radius: f64::from(theme.tokens.radii.item),
        }
    }
}

/// `details` laid out from `top` in the column `x0..x0 + width`, and the height they take.
pub(super) fn lay_out(
    text: &mut TextRenderer,
    style: &Style,
    details: &str,
    x0: f64,
    top: f64,
    width: f64,
) -> (Vec<Line>, f64) {
    let mut lines = Vec::new();
    let mut y = top;
    for (kind, said) in split(details) {
        if !lines.is_empty() {
            y += style.line_gap;
        }
        let line = match kind {
            Kind::Words => {
                let wrap = width.max(1.0);
                let (_, h) =
                    text.measure_style(said, Some(tilo_ui_base::num::f32_of_f64(wrap)), &style.words);
                let rect = Rect::new(x0, y, x0 + width, y + f64::from(h));
                Line {
                    kind,
                    text: said.to_owned(),
                    rect,
                    origin: rect.origin(),
                    wrap,
                    copy: None,
                }
            }
            Kind::Verbatim => {
                let wrap = (width - style.pad_x * 2.0 - style.copy).max(1.0);
                let (_, h) =
                    text.measure_style(said, Some(tilo_ui_base::num::f32_of_f64(wrap)), &style.verbatim);
                let height = (f64::from(h) + style.pad_y * 2.0).max(style.copy);
                let rect = Rect::new(x0, y, x0 + width, y + height);
                let text_y = rect.center().y - f64::from(h) / 2.0;
                Line {
                    kind,
                    text: said.to_owned(),
                    rect,
                    origin: Point::new(x0 + style.pad_x, text_y),
                    wrap,
                    copy: Some(Rect::new(
                        rect.x1 - style.copy,
                        rect.y0,
                        rect.x1,
                        rect.y0 + style.copy,
                    )),
                }
            }
        };
        y = line.rect.y1;
        lines.push(line);
    }
    (lines, y - top)
}

#[cfg(test)]
mod tests {
    use super::{Kind, split};

    /// A launch's answer reads in the order it was said, each line its own, and the indented
    /// lines are the ones to copy, without their indent.
    #[test]
    fn indented_lines_are_the_ones_to_copy() {
        let said: Vec<_> = split(
            "kernel.x turns them off.\nTurn them on:\n    sudo sysctl kernel.x=1\n\n\tkernel.x = 1\nso.tilo.notes ended: exit status: 1",
        )
        .collect();
        assert_eq!(
            said,
            [
                (Kind::Words, "kernel.x turns them off."),
                (Kind::Words, "Turn them on:"),
                (Kind::Verbatim, "sudo sysctl kernel.x=1"),
                (Kind::Verbatim, "kernel.x = 1"),
                (Kind::Words, "so.tilo.notes ended: exit status: 1"),
            ]
        );
    }
}
