//! The row's resolved metrics: every length and style a row lays its columns out by, read off
//! one theme, so the retained row and the immediate face measure alike.

use crate::chrome::row_bleed;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;

/// The resolved geometry of every row in `theme`: what the face paints and
/// the retained row measures, from one place.
#[derive(Clone, Debug)]
pub(super) struct RowMetrics {
    pub(super) bleed: f64,
    pub(super) pad_y: f64,
    pub(super) min_height: f64,
    pub(super) column_gap: f64,
    pub(super) title: TextStyle,
    pub(super) title_strong: TextStyle,
    pub(super) subtitle: TextStyle,
    pub(super) secondary: TextStyle,
    pub(super) meta: TextStyle,
    pub(super) badge: TextStyle,
    pub(super) value_body: TextStyle,
    pub(super) value_title: TextStyle,
    pub(super) icon: f64,
    pub(super) mark: f64,
    pub(super) action: f64,
    pub(super) action_gap: f64,
    pub(super) dot: f64,
    pub(super) radius: f64,
    pub(super) tag_gap: f64,
    /// One step of nesting: the gutter a disclosure chevron hangs in, left of its content.
    pub(super) nest: f64,
}

impl RowMetrics {
    pub(super) fn new(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        Self {
            bleed: row_bleed(theme),
            // The row's height comes from its kind (36 / 52 / 64, compact
            // 28 / 44 / 52); the text keeps a tight inset inside it.
            pad_y: f64::from(spacing.rhythm.tight),
            min_height: f64::from(metrics.row_single),
            column_gap: f64::from(spacing.s3),
            title: theme.text(TextRole::Body),
            title_strong: theme.text(TextRole::Body).strong(),
            subtitle: theme.text(TextRole::Body),
            secondary: theme.text(TextRole::Caption),
            meta: theme.text(TextRole::Caption).numeric(),
            badge: theme.text(TextRole::Caption).strong(),
            value_body: theme.text(TextRole::Body).numeric(),
            value_title: theme.text(TextRole::Title).numeric(),
            icon: f64::from(metrics.icon_control),
            mark: f64::from(metrics.icon_sm),
            action: f64::from(metrics.bar_control_height),
            action_gap: f64::from(spacing.s1),
            dot: f64::from(spacing.s2),
            radius: f64::from(theme.tokens.radii.item),
            tag_gap: f64::from(spacing.s1),
            nest: crate::chrome::disclosure_gutter(theme),
        }
    }

    /// The metrics of a row shown as a card: the row is its own surface, so nothing of it
    /// bleeds past its box, and the padding inside that surface is its own — compact enough
    /// that a card of one title line and its facts stands under a control's height.
    pub(super) fn card(theme: &Theme) -> Self {
        let mut m = Self::new(theme);
        m.bleed = f64::from(theme.tokens.spacing.rhythm.sibling);
        m.pad_y = f64::from(theme.tokens.spacing.rhythm.tight);
        m
    }

    /// The metrics of a dense row: one line at the caption's size and the small glyph, for a
    /// reference that stands among content (what a message was made from) or a list that shows
    /// a lot at once. Its height comes from its own line and inset, not the list's row height;
    /// a row a finger presses still grows to its target.
    pub(super) fn dense(theme: &Theme) -> Self {
        let mut m = Self::new(theme);
        m.min_height = 0.0;
        m.column_gap = f64::from(theme.tokens.spacing.s2);
        m.title = theme.text(TextRole::Caption);
        m.title_strong = theme.text(TextRole::Caption).strong();
        m.subtitle = theme.text(TextRole::Caption);
        m.icon = f64::from(theme.tokens.metrics.icon_sm);
        m
    }

    pub(super) fn line(text: &mut TextRenderer, style: &TextStyle) -> f64 {
        f64::from(text.measure_style("Ag", None, style).1)
    }

    /// The avatar's side: the inline size beside a one-line row, the list
    /// size beside a row with more lines. Fixed per kind of row, so a list's
    /// titles share one edge whatever each row's secondary text does.
    pub(super) fn avatar(theme: &Theme, extra_lines: bool) -> f64 {
        let metrics = &theme.tokens.metrics;
        if extra_lines {
            f64::from(metrics.avatar_row)
        } else {
            f64::from(metrics.avatar_inline)
        }
    }

    /// The least a row is tall: one line 36, a title and a secondary line
    /// 52, a row led by a 40 px avatar 64 (28 / 44 / 52 compact).
    pub(super) fn min_height_for(theme: &Theme, extra_lines: bool, big_leading: bool) -> f64 {
        let metrics = &theme.tokens.metrics;
        f64::from(if extra_lines && big_leading {
            metrics.row_avatar
        } else if extra_lines {
            metrics.row_double
        } else {
            metrics.row_single
        })
    }
}
