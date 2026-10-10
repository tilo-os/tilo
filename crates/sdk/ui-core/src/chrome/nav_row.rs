//! One navigation row and one navigation section header, painted the same
//! everywhere.
//!
//! `NavList` paints every row through [`paint_nav_row`] and every section
//! header through [`paint_nav_section`]; a painted app that cannot host a
//! `NavList` calls the same two functions with the same faces, so a sidebar
//! drawn by hand is pixel-for-pixel the sidebar the widget draws. The
//! geometry ([`nav_row_layout`], [`nav_section_layout`]) is public so a
//! painted host hit-tests exactly what was painted.
//!
//! A row is the list's `s2` inset from both edges; its selection is the
//! neutral lift of the ground it stands on ([`super::paint_row_selection`]),
//! with primary ink. Counts are `Caption` tabular figures in tertiary ink; a
//! [`NavCount::Live`] count (unread, needs attention) is the one filled
//! badge, in the person's own colour. Section headers are sentence-case
//! `Subheading` in secondary ink.
//!
//! Every part a row can show, leading to trailing:
//!
//! | Part | Face | Look |
//! |---|---|---|
//! | check | [`NavRowFace::checked`] | a [`crate::widget::CheckboxFace`]; the whole row toggles it |
//! | icon, swatch or avatar | [`NavRowFace::icon`], [`NavRowFace::swatch`], [`NavRowFace::avatar`] | a `ui::icons` glyph, the content colour the row stands for, or the person it stands for |
//! | badge | [`NavRowFace::badge`] | a short classifying code on a quiet pill, in a column shared by its section |
//! | label | [`NavRowFace::new`] | `Body`; `Body.strong()` while selected |
//! | secondary | [`NavRowFace::secondary`] | one `Caption` line under the label, in secondary ink |
//! | level | [`NavRowFace::level`] | a usage bar under the text, on the progress bar's track and fill |
//! | presence | [`NavRowFace::presence`] | the people on it, as marks in their colour |
//! | status label | [`NavRowFace::status_label`] | a quiet tinted badge ("Active") |
//! | meta | [`NavRowFace::meta`] | a tertiary `Caption` figure (a size) |
//! | count | [`NavRowFace::count`] | quiet tabular figures, or the live badge |
//! | status | [`NavRowFace::status`] | a dot at the trailing edge |
//!
//! Hover and focus swap the trailing parts for the row's actions.
//!
//! Draw with the theme of the ground the rows stand on
//! (`theme.in_region(Region::Sidebar)` in a sidebar, `theme.on_ground(..)`
//! elsewhere): the painter reads that ground from `surfaces.content`.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};

use super::shell::{SelectionState, paint_row_selection};
use crate::icons::Icon;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{AvatarFace, BadgeFace, ChipStyle, RowStatus, Tone};

/// How many items a navigation row stands for, beside its label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavCount {
    /// A plain count (messages in a folder, tracks in a playlist): quiet
    /// tabular figures in tertiary ink.
    Neutral(u32),
    /// Something that wants attention (unread, mentions): the one filled
    /// badge in a navigation list, in the person's own colour.
    Live(u32),
}

impl NavCount {
    /// The number shown.
    #[must_use]
    pub fn value(self) -> u32 {
        match self {
            Self::Neutral(n) | Self::Live(n) => n,
        }
    }

    fn text(self) -> String {
        self.value().to_string()
    }
}

/// A row laid out as a list row or as a tile in an icon-over-label rail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NavRowStyle {
    /// Icon, label and count on one line.
    #[default]
    List,
    /// Icon over a caption of up to two lines, for a compact rail.
    Rail,
}

/// Everything a navigation row shows. Build with [`NavRowFace::new`] and
/// paint with [`paint_nav_row`].
#[derive(Clone, Debug)]
pub struct NavRowFace<'a> {
    /// The row's full band, edge to edge of the list. The painter applies
    /// the list inset itself.
    pub rect: Rect,
    pub label: &'a str,
    pub icon: Option<Icon>,
    pub count: Option<NavCount>,
    /// Nesting level below the top level of its section.
    pub depth: usize,
    /// Quiet leading inset for destinations beneath a named section.
    pub under_section: bool,
    /// `Some(expanded)` when the row has children.
    pub disclosure: Option<bool>,
    /// Selected (`active`), hovered, pressed.
    pub state: SelectionState,
    /// Keyboard focus is on this row.
    pub focused: bool,
    /// Trailing actions shown on hover or focus, in order. While they show,
    /// they take the place of the trailing parts.
    pub actions: &'a [Icon],
    /// The action under the pointer.
    pub action_hover: Option<usize>,
    pub style: NavRowStyle,
    /// One `Caption` line under the label (a host and port, a schedule).
    pub secondary: Option<&'a str>,
    /// A tertiary figure at the trailing edge (a size).
    pub meta: Option<&'a str>,
    /// A status dot at the trailing edge.
    pub status: Option<RowStatus<'a>>,
    /// A quiet tinted badge naming a state ("Active").
    pub status_label: Option<(&'a str, Tone)>,
    /// A short leading code that classifies the row (`GET`).
    pub badge: Option<&'a str>,
    /// The width of the badge column the row's section shares; `None`
    /// sizes the column to this row's own badge.
    pub badge_column: Option<f64>,
    /// The content colour the row stands for (a calendar, a project), in
    /// the icon's place.
    pub swatch: Option<Color>,
    /// The person the row stands for (a participant in a call), as a small
    /// avatar in the icon's place.
    pub avatar: Option<AvatarFace<'a>>,
    /// `Some(checked)`: a checkbox in the leading slot. The whole row is
    /// its target.
    pub checked: Option<bool>,
    /// A usage bar under the text, `0.0..=1.0`.
    pub level: Option<f32>,
    /// The people on this row's place, by identity, each a mark in their
    /// presence colour.
    pub presence: &'a [String],
}

impl<'a> NavRowFace<'a> {
    #[must_use]
    pub fn new(rect: Rect, label: &'a str) -> Self {
        Self {
            rect,
            label,
            icon: None,
            count: None,
            depth: 0,
            under_section: false,
            disclosure: None,
            state: SelectionState::default(),
            focused: false,
            actions: &[],
            action_hover: None,
            style: NavRowStyle::List,
            secondary: None,
            meta: None,
            status: None,
            status_label: None,
            badge: None,
            badge_column: None,
            swatch: None,
            avatar: None,
            checked: None,
            level: None,
            presence: &[],
        }
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn count(mut self, count: NavCount) -> Self {
        self.count = Some(count);
        self
    }

    /// Give rows beneath a labelled section a quiet leading inset without
    /// changing tree depth, selection bounds or pointer targets.
    #[must_use]
    pub fn under_section(mut self) -> Self {
        self.under_section = true;
        self
    }

    #[must_use]
    pub fn depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    #[must_use]
    pub fn disclosure(mut self, expanded: bool) -> Self {
        self.disclosure = Some(expanded);
        self
    }

    #[must_use]
    pub fn state(mut self, state: SelectionState) -> Self {
        self.state = state;
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.state.active = selected;
        self
    }

    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.state.hover = hovered;
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn actions(mut self, actions: &'a [Icon], hover: Option<usize>) -> Self {
        self.actions = actions;
        self.action_hover = hover;
        self
    }

    #[must_use]
    pub fn rail(mut self) -> Self {
        self.style = NavRowStyle::Rail;
        self
    }

    /// One `Caption` line under the label; it elides, never wraps.
    #[must_use]
    pub fn secondary(mut self, text: &'a str) -> Self {
        self.secondary = Some(text);
        self
    }

    /// A tertiary figure at the trailing edge (a size, a duration).
    #[must_use]
    pub fn meta(mut self, text: &'a str) -> Self {
        self.meta = Some(text);
        self
    }

    /// A status dot at the trailing edge (a drive's health, a connection).
    #[must_use]
    pub fn status(mut self, status: RowStatus<'a>) -> Self {
        self.status = Some(status);
        self
    }

    /// A quiet tinted badge naming the row's state ("Active").
    #[must_use]
    pub fn status_label(mut self, label: &'a str, tone: Tone) -> Self {
        self.status_label = Some((label, tone));
        self
    }

    /// A short leading code that classifies the row (an HTTP method). Pass
    /// the section's [`nav_badge_column`] to [`Self::badge_column`] so the
    /// labels of its rows line up.
    #[must_use]
    pub fn badge(mut self, code: &'a str) -> Self {
        self.badge = Some(code);
        self
    }

    /// The width of the badge column this row's section shares.
    #[must_use]
    pub fn badge_column(mut self, width: f64) -> Self {
        self.badge_column = Some(width);
        self
    }

    /// The content colour the row stands for, in the icon's place.
    #[must_use]
    pub fn swatch(mut self, color: Color) -> Self {
        self.swatch = Some(color);
        self
    }

    /// The person the row stands for, as a small avatar in the icon's place.
    #[must_use]
    pub fn avatar(mut self, avatar: AvatarFace<'a>) -> Self {
        self.avatar = Some(avatar);
        self
    }

    /// A checkbox in the leading slot, toggled by the whole row.
    #[must_use]
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// A usage bar under the text; `fraction` is clamped to `0..=1`.
    #[must_use]
    pub fn level(mut self, fraction: f32) -> Self {
        self.level = Some(fraction.clamp(0.0, 1.0));
        self
    }

    /// The people on this row's place, by identity.
    #[must_use]
    pub fn presence(mut self, identities: &'a [String]) -> Self {
        self.presence = identities;
        self
    }

    /// The height this row asks for in a list: [`nav_item_height`].
    #[must_use]
    pub fn height(&self, theme: &Theme) -> f64 {
        nav_item_height(theme, self.secondary.is_some(), self.level.is_some())
    }

    fn lifted(&self) -> bool {
        self.state.active || self.state.pressed
    }
}

/// Where each part of a navigation row lands. Painting and hit-testing
/// both read it.
#[derive(Clone, Debug, PartialEq)]
pub struct NavRowLayout {
    /// The selection lift (and the focus ring) of the row.
    pub selection: Rect,
    /// The leading checkbox.
    pub check: Option<Rect>,
    /// The icon, or the swatch or avatar in its place.
    pub icon: Option<Rect>,
    /// The leading badge column.
    pub badge: Option<Rect>,
    /// The horizontal run the label is elided into.
    pub label_x0: f64,
    pub label_x1: f64,
    /// The top of the label's line box.
    pub label_top: f64,
    /// The top of the secondary line's box.
    pub secondary_top: Option<f64>,
    /// The usage bar's track.
    pub level: Option<Rect>,
    /// The trailing disclosure chevron of a row with children.
    pub disclosure: Option<Rect>,
    /// The people marks, leading to trailing.
    pub presence: Vec<Rect>,
    /// The tinted status badge.
    pub status_label: Option<Rect>,
    /// The meta figure.
    pub meta: Option<Rect>,
    /// The count, or `None` while actions take its place.
    pub count: Option<Rect>,
    /// The status dot.
    pub status: Option<Rect>,
    /// Trailing action squares, in action order.
    pub actions: Vec<Rect>,
}

/// Height of one list row: [`crate::tokens::Metrics::nav_row_height`],
/// grown with the body line at large text.
#[must_use]
pub fn nav_row_height(theme: &Theme) -> f64 {
    crate::widget::text_control_height(
        f64::from(crate::tokens::Metrics::BASE.nav_row_height),
        theme,
    )
}

/// Height of a list row that carries a secondary line and/or a usage bar
/// under its label: the stacked text inside the row's `s2` padding, never
/// shorter than [`nav_row_height`].
#[must_use]
pub fn nav_item_height(theme: &Theme, secondary: bool, level: bool) -> f64 {
    let base = nav_row_height(theme);
    if !secondary && !level {
        return base;
    }
    let block = text_block_height(theme, secondary, level);
    (block + 2.0 * f64::from(theme.tokens.spacing.s2))
        .ceil()
        .max(base)
}

/// The label, the secondary line and the usage bar stacked.
fn text_block_height(theme: &Theme, secondary: bool, level: bool) -> f64 {
    let mut block = f64::from(theme.text(TextRole::Body).line_height_px());
    if secondary {
        block += f64::from(theme.text(TextRole::Caption).line_height_px());
    }
    if level {
        block += level_gap(theme) + level_height(theme);
    }
    block
}

/// The usage bar's thickness: one `s1` step.
fn level_height(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s1)
}

/// The space between the text and the usage bar under it.
fn level_gap(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s1)
}

/// Height of one rail tile: the icon over a caption line inside the row
/// padding.
#[must_use]
pub fn nav_rail_row_height(theme: &Theme) -> f64 {
    let tokens = &theme.tokens;
    let caption = f64::from(theme.text(TextRole::Caption).line_height_px());
    f64::from(tokens.metrics.icon_md)
        + f64::from(tokens.spacing.s1)
        + caption
        + 2.0 * f64::from(tokens.spacing.s2)
}

/// Height of the rail tile for `label` in a rail `width` wide: one caption
/// line more than [`nav_rail_row_height`] when the caption wraps.
#[must_use]
pub fn nav_rail_item_height(
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    label: &str,
    width: f64,
) -> f64 {
    let lines = rail_caption_lines(
        theme,
        text_renderer,
        label,
        rail_caption_width(theme, width),
    );
    let caption = f64::from(theme.text(TextRole::Caption).line_height_px());
    nav_rail_row_height(theme) + caption * count_f64(lines.len().saturating_sub(1))
}

/// The run a rail caption is set in, inside a tile `width` wide.
fn rail_caption_width(theme: &Theme, width: f64) -> f64 {
    (width - 2.0 * f64::from(theme.tokens.spacing.s1)).max(0.0)
}

/// A rail caption broken into at most two lines of `max_w`: at a space
/// when there is one, inside the word (with a hyphen) when one word is too
/// long for a line; the second line elides.
#[must_use]
pub fn rail_caption_lines(
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    label: &str,
    max_w: f64,
) -> Vec<String> {
    let style = label_measure_style(theme, NavRowStyle::Rail);
    let fits = |text: &mut TextRenderer, s: &str| {
        f64::from(text.measure_style(s, None, &style).0) <= max_w
    };
    if label.is_empty() || fits(text_renderer, label) {
        return vec![label.to_owned()];
    }
    let words: Vec<&str> = label.split_whitespace().collect();
    // The most whole words that fit on the first line.
    let mut first = String::new();
    let mut taken = 0;
    for word in &words {
        let candidate = if first.is_empty() {
            (*word).to_owned()
        } else {
            format!("{first} {word}")
        };
        if !fits(text_renderer, &candidate) {
            break;
        }
        first = candidate;
        taken += 1;
    }
    let (first, rest) = if taken > 0 {
        (first, words.get(taken..).unwrap_or_default().join(" "))
    } else {
        // One word longer than the line: break inside it, at the last
        // syllable boundary of the reader's language whose head still fits
        // ("Connec-tions", not "Connecti-ons"). A word with no such boundary
        // that fits falls back to the last character that does.
        let word = words.first().copied().unwrap_or(label);
        let locale = crate::i18n::I18nManager::current_locale();
        let syllable_cut = crate::text_break::discretionary_hyphen_breaks(word, &locale)
            .into_iter()
            .rev()
            .find(|&byte| {
                word.get(..byte)
                    .is_some_and(|head| fits(text_renderer, &format!("{head}-")))
            });
        let cut = syllable_cut.unwrap_or_else(|| {
            let mut cut = 0;
            for (byte, _) in word.char_indices().skip(1) {
                if word
                    .get(..byte)
                    .is_some_and(|head| fits(text_renderer, &format!("{head}-")))
                {
                    cut = byte;
                } else {
                    break;
                }
            }
            cut
        });
        if cut == 0 {
            let line = text_renderer.elide_to_width_style(label, to_f32(max_w), &style);
            return vec![line];
        }
        let (head, tail) = word.split_at(cut);
        let rest = std::iter::once(tail)
            .chain(words.iter().skip(1).copied())
            .collect::<Vec<_>>()
            .join(" ");
        (format!("{head}-"), rest)
    };
    if rest.is_empty() {
        return vec![first];
    }
    let second = text_renderer.elide_to_width_style(&rest, to_f32(max_w), &style);
    vec![first, second]
}

/// Height of a section header. Every section after the first is separated
/// from the rows above it by `s3` of space, not by a line.
#[must_use]
pub fn nav_section_height(theme: &Theme, first: bool) -> f64 {
    let lead = if first {
        0.0
    } else {
        f64::from(theme.tokens.spacing.s3)
    };
    nav_row_height(theme) + lead
}

/// The width of the badge column a section's rows share: the widest of
/// `codes` on its pill.
#[must_use]
pub fn nav_badge_column(theme: &Theme, text_renderer: &mut TextRenderer, codes: &[&str]) -> f64 {
    crate::widget::badge_column_width(text_renderer, theme, codes)
}

/// The style a row's label is set in: `Body`, or `Body.strong()` while the
/// row is selected. Measure elision with [`label_measure_style`] so the
/// label does not jump when the weight changes.
#[must_use]
pub fn label_style(theme: &Theme, face: &NavRowFace<'_>) -> TextStyle {
    let role = match face.style {
        NavRowStyle::List => TextRole::Body,
        NavRowStyle::Rail => TextRole::Caption,
    };
    let style = theme.text(role);
    if face.lifted() { style.strong() } else { style }
}

/// The heavier of a row's two weights: elision is measured with it, so a
/// label that fits selected also fits unselected.
#[must_use]
pub fn label_measure_style(theme: &Theme, style: NavRowStyle) -> TextStyle {
    let role = match style {
        NavRowStyle::List => TextRole::Body,
        NavRowStyle::Rail => TextRole::Caption,
    };
    theme.text(role).strong()
}

fn count_style(theme: &Theme, count: NavCount) -> TextStyle {
    let style = theme.text(TextRole::Caption).numeric();
    match count {
        NavCount::Neutral(_) => style,
        NavCount::Live(_) => style.strong(),
    }
}

fn meta_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption).numeric()
}

fn secondary_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption)
}

/// Lay out `face` inside its row (see [`NavRowLayout`]).
#[must_use]
pub fn nav_row_layout(
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    face: &NavRowFace<'_>,
) -> NavRowLayout {
    match face.style {
        NavRowStyle::List => list_layout(theme, text_renderer, face),
        NavRowStyle::Rail => rail_layout(theme, face),
    }
}

/// A square of `side` centred on `mid` with its leading edge at `x`.
fn slot(x: f64, mid: f64, side: f64) -> Rect {
    Rect::new(x, mid - side / 2.0, x + side, mid + side / 2.0)
}

/// A rect `w` × `h` ending at `trailing`, centred on `mid`.
fn trailing_rect(trailing: f64, mid: f64, w: f64, h: f64) -> Rect {
    Rect::new(trailing - w, mid - h / 2.0, trailing, mid + h / 2.0)
}

#[allow(
    clippy::too_many_lines,
    reason = "every part of a row is placed in one ordered pass, leading to trailing"
)]
fn list_layout(
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    face: &NavRowFace<'_>,
) -> NavRowLayout {
    let tokens = &theme.tokens;
    let s1 = f64::from(tokens.spacing.s1);
    let s2 = f64::from(tokens.spacing.s2);
    let s4 = f64::from(tokens.spacing.s4);
    let hairline = f64::from(tokens.metrics.chrome_rule_px);
    let icon_side = f64::from(tokens.metrics.icon_md);
    let gap = f64::from(tokens.metrics.icon_label_gap);
    let r = face.rect;
    let selection = Rect::new(r.x0 + s2, r.y0 + hairline, r.x1 - s2, r.y1 - hairline);
    let mid = selection.center().y;

    let mut x =
        selection.x0 + s2 + depth_f64(face.depth) * s4 + if face.under_section { s1 } else { 0.0 };
    let check = face.checked.map(|_| {
        let rect = slot(x, mid, icon_side);
        x = rect.x1 + gap;
        rect
    });
    let icon = (face.icon.is_some() || face.swatch.is_some() || face.avatar.is_some()).then(|| {
        let rect = slot(x, mid, icon_side);
        x = rect.x1 + gap;
        rect
    });
    let badge = face.badge.map(|code| {
        let width = face
            .badge_column
            .unwrap_or_else(|| nav_badge_column(theme, text_renderer, &[code]));
        let rect = Rect::new(x, mid - icon_side / 2.0, x + width, mid + icon_side / 2.0);
        x = rect.x1 + gap;
        rect
    });
    let label_x0 = x;

    // The stacked text block: the label, the secondary line, the bar.
    let label_line = f64::from(theme.text(TextRole::Body).line_height_px());
    let stacked = face.secondary.is_some() || face.level.is_some();
    let (label_top, secondary_top, level_top) = if stacked {
        let block = text_block_height(theme, face.secondary.is_some(), face.level.is_some());
        let top = mid - block / 2.0;
        let secondary_top = face.secondary.map(|_| top + label_line);
        let text_bottom = top
            + label_line
            + face.secondary.map_or(0.0, |_| {
                f64::from(theme.text(TextRole::Caption).line_height_px())
            });
        (
            top,
            secondary_top,
            face.level.map(|_| text_bottom + level_gap(theme)),
        )
    } else {
        // One line, centred on the measured line box as before.
        let (_, line_h) = text_renderer.measure_style("Ag", None, &label_style(theme, face));
        (
            crate::text_metrics::centered_text_top(
                selection.y0,
                selection.height(),
                f64::from(line_h),
            ),
            None,
            None,
        )
    };

    let mut trailing = selection.x1 - s2;
    let action_side = (selection
        .height()
        .min(nav_row_height(theme) - 2.0 * hairline)
        - 2.0 * s1)
        .max(1.0);
    let actions: Vec<Rect> = if face.actions.is_empty() {
        Vec::new()
    } else {
        // Flush with the selection's trailing edge, the last action last.
        let mut right = selection.x1 - s1;
        let mut rects: Vec<Rect> = face
            .actions
            .iter()
            .rev()
            .map(|_| {
                let rect = trailing_rect(right, mid, action_side, action_side);
                right = rect.x0 - s1 / 2.0;
                rect
            })
            .collect();
        rects.reverse();
        if let Some(first) = rects.first() {
            trailing = first.x0 - s1;
        }
        rects
    };
    let quiet = actions.is_empty();
    let status = face.status.filter(|_| quiet).map(|_| {
        let rect = trailing_rect(trailing, mid, s2, s2);
        trailing = rect.x0 - s2;
        rect
    });
    let count = face.count.filter(|_| quiet).map(|count| {
        let style = count_style(theme, count);
        let (w, h) = text_renderer.measure_style(&count.text(), None, &style);
        let (w, h) = (f64::from(w), f64::from(h));
        let rect = match count {
            NavCount::Neutral(_) => trailing_rect(trailing, mid, w, h),
            NavCount::Live(_) => {
                let height = h + s1;
                trailing_rect(trailing, mid, (w + s2).max(height), height)
            }
        };
        trailing = rect.x0 - s2;
        rect
    });
    let meta = face.meta.filter(|_| quiet).map(|meta| {
        let (w, h) = text_renderer.measure_style(meta, None, &meta_style(theme));
        let rect = trailing_rect(trailing, mid, f64::from(w), f64::from(h));
        trailing = rect.x0 - s2;
        rect
    });
    let status_label = face.status_label.filter(|_| quiet).map(|(label, _)| {
        let w = BadgeFace::intrinsic_width(label, text_renderer, theme);
        let rect = trailing_rect(trailing, mid, w, BadgeFace::height(theme));
        trailing = rect.x0 - s2;
        rect
    });
    let presence = if quiet && !face.presence.is_empty() {
        // Marks overlap by an `s1` step, the trailing one on top.
        let side = f64::from(tokens.spacing.s3);
        let step = side - s1;
        let total = side + step * count_f64(face.presence.len().saturating_sub(1));
        let x0 = trailing - total;
        trailing = x0 - s2;
        (0..face.presence.len())
            .map(|index| {
                let x = x0 + step * count_f64(index);
                Rect::new(x, mid - side / 2.0, x + side, mid + side / 2.0)
            })
            .collect()
    } else {
        Vec::new()
    };
    let disclosure = face.disclosure.map(|_| {
        let side = f64::from(tokens.metrics.icon_sm);
        let rect = trailing_rect(trailing, mid, side, side);
        trailing = rect.x0 - s2;
        rect
    });
    let label_x1 = trailing.max(label_x0);
    let level = level_top.map(|top| Rect::new(label_x0, top, label_x1, top + level_height(theme)));
    NavRowLayout {
        selection,
        check,
        icon,
        badge,
        label_x0,
        label_x1,
        label_top,
        secondary_top,
        level,
        disclosure,
        presence,
        status_label,
        meta,
        count,
        status,
        actions,
    }
}

fn rail_layout(theme: &Theme, face: &NavRowFace<'_>) -> NavRowLayout {
    let tokens = &theme.tokens;
    let s1 = f64::from(tokens.spacing.s1);
    let s2 = f64::from(tokens.spacing.s2);
    let r = face.rect;
    let selection = Rect::new(r.x0 + s1, r.y0 + s1 / 2.0, r.x1 - s1, r.y1 - s1 / 2.0);
    let side = f64::from(tokens.metrics.icon_md);
    let top = selection.y0 + s2;
    let cx = selection.center().x;
    let icon = Rect::new(cx - side / 2.0, top, cx + side / 2.0, top + side);
    // A live count sits on the icon's trailing shoulder.
    let count = face.count.and_then(|count| match count {
        NavCount::Live(_) => {
            let dot = s2;
            Some(Rect::new(
                icon.x1 - dot / 2.0,
                icon.y0 - dot / 2.0,
                icon.x1 + dot / 2.0,
                icon.y0 + dot / 2.0,
            ))
        }
        NavCount::Neutral(_) => None,
    });
    NavRowLayout {
        selection,
        check: None,
        icon: Some(icon),
        badge: None,
        // The selection already contributes the rail's `s1` inset on each
        // edge. A second caption inset made short destinations unreadable at
        // enlarged text (`Channels` became `Cha- / nn…`) even though the
        // selection still had room for the complete two-line label.
        label_x0: selection.x0,
        label_x1: selection.x1,
        label_top: icon.y1 + s1,
        secondary_top: None,
        level: None,
        disclosure: None,
        presence: Vec::new(),
        status_label: None,
        meta: None,
        count,
        status: None,
        actions: Vec::new(),
    }
}

/// Paint one navigation row. `theme` is the theme of the ground the row
/// stands on; its `surfaces.content` is that ground.
#[allow(
    clippy::too_many_lines,
    reason = "every part of a row paints in one ordered pass, leading to trailing"
)]
pub fn paint_nav_row(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    face: &NavRowFace<'_>,
) {
    let layout = nav_row_layout(theme, text_renderer, face);
    let tokens = &theme.tokens;
    let colors = &tokens.colors;
    let ground = colors.surfaces.content;
    paint_row_selection(
        scene,
        transform,
        layout.selection,
        face.state,
        ground,
        theme,
    );
    if face.focused {
        paint_focus(scene, transform, theme, layout.selection);
    }
    // Primary ink once the row is chosen or under the pointer; secondary at
    // rest, so the selected row reads by ink as well as by its lift.
    let ink = if face.lifted() || face.state.hover {
        colors.text_primary
    } else {
        colors.text_dim
    };
    if let (Some(checked), Some(rect)) = (face.checked, layout.check) {
        crate::widget::Checkbox::paint(
            scene,
            transform,
            theme,
            &crate::widget::CheckboxFace::new(rect, checked).hovered(face.state.hover),
        );
    }
    if let Some(rect) = layout.icon {
        if let Some(avatar) = face.avatar {
            // The icon slot is the small avatar's side, so a person's row
            // lines its label up with the icon rows around it.
            avatar.paint(
                &mut crate::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
            );
        } else if let Some(color) = face.swatch {
            paint_swatch(scene, transform, theme, rect, color);
        } else if let Some(icon) = face.icon {
            icon.draw(scene, transform, rect, ink);
        }
    }
    if let (Some(code), Some(rect)) = (face.badge, layout.badge) {
        crate::widget::list_row::paint_code_badge(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            code,
        );
    }

    let style = label_style(theme, face);
    let measure = label_measure_style(theme, face.style);
    let max_w = (layout.label_x1 - layout.label_x0).max(0.0);
    match face.style {
        NavRowStyle::List => {
            let label = text_renderer.elide_to_width_style(face.label, to_f32(max_w), &measure);
            text_renderer.draw_style(
                scene,
                &label,
                ink,
                transform.pre_translate(Vec2::new(layout.label_x0, layout.label_top)),
                None,
                &style,
            );
        }
        NavRowStyle::Rail => {
            let caption = f64::from(theme.text(TextRole::Caption).line_height_px());
            let lines = rail_caption_lines(theme, text_renderer, face.label, max_w);
            for (index, line) in lines.iter().enumerate() {
                let (line_w, _) = text_renderer.measure_style(line, None, &style);
                let x = layout.selection.center().x - f64::from(line_w) / 2.0;
                let y = layout.label_top + caption * count_f64(index);
                text_renderer.draw_style(
                    scene,
                    line,
                    ink,
                    transform.pre_translate(Vec2::new(x, y)),
                    None,
                    &style,
                );
            }
        }
    }
    if let (Some(secondary), Some(top)) = (face.secondary, layout.secondary_top) {
        let style = secondary_style(theme);
        let line = text_renderer.elide_to_width_style(secondary, to_f32(max_w), &style);
        text_renderer.draw_style(
            scene,
            &line,
            colors.text_dim,
            transform.pre_translate(Vec2::new(layout.label_x0, top)),
            None,
            &style,
        );
    }
    if let (Some(fraction), Some(track)) = (face.level, layout.level) {
        crate::widget::ProgressBarFace::new(fraction).paint(
            &mut crate::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            track,
        );
    }

    if let (Some(expanded), Some(rect)) = (face.disclosure, layout.disclosure) {
        let chevron = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        chevron.draw(scene, transform, rect, colors.text_tertiary);
    }
    let lift = if face.lifted() {
        colors.lift_on(ground, 1.0)
    } else if face.state.hover {
        colors.lift_on(ground, super::shell::HOVER_LIFT_T)
    } else {
        ground
    };
    for (identity, rect) in face.presence.iter().zip(&layout.presence) {
        let hue = crate::tokens::colors::PresenceHue::of_identity(identity);
        let circle = Circle::new(rect.center(), rect.width() / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.presence.hue(hue),
            None,
            &circle,
        );
        // Each mark is cut out of the one beside it by the row's own ground.
        scene.stroke(
            &Stroke::new(f64::from(tokens.metrics.chrome_rule_px) * 1.5),
            transform,
            lift,
            None,
            &circle,
        );
    }
    if let (Some((label, tone)), Some(rect)) = (face.status_label, layout.status_label) {
        BadgeFace::new(rect, label)
            .style(ChipStyle::Tinted(tone))
            .paint(scene, transform, text_renderer, theme);
    }
    if let (Some(meta), Some(rect)) = (face.meta, layout.meta) {
        text_renderer.draw_style(
            scene,
            meta,
            colors.text_tertiary,
            transform.pre_translate(Vec2::new(rect.x0, rect.y0)),
            None,
            &meta_style(theme),
        );
    }
    if let (Some(count), Some(rect)) = (face.count, layout.count) {
        paint_count(
            scene,
            transform,
            text_renderer,
            theme,
            face.style,
            count,
            rect,
        );
    }
    if let (Some(status), Some(rect)) = (face.status, layout.status) {
        scene.fill(
            Fill::NonZero,
            transform,
            status.colour(theme),
            None,
            &Circle::new(rect.center(), rect.width() / 2.0),
        );
    }
    for (index, (icon, rect)) in face.actions.iter().zip(&layout.actions).enumerate() {
        let hovered = face.action_hover == Some(index);
        if hovered {
            paint_row_selection(
                scene,
                transform,
                *rect,
                SelectionState::new(false, true, false),
                // The action lifts off the row it sits on.
                lift,
                theme,
            );
        }
        let inset = (rect.width() - f64::from(tokens.metrics.icon_sm)).max(0.0) / 2.0;
        icon.draw(
            scene,
            transform,
            rect.inset(-inset),
            if hovered {
                colors.text_primary
            } else {
                colors.text_dim
            },
        );
    }
}

/// A content colour in the icon's place: a rounded square inside the icon
/// slot, edged so a white swatch on paper keeps its outline.
fn paint_swatch(scene: &mut Scene, transform: Affine, theme: &Theme, slot: Rect, color: Color) {
    let side = f64::from(theme.tokens.metrics.icon_sm);
    let rect = Rect::from_center_size(slot.center(), (side, side));
    let radius = (f64::from(theme.tokens.radii.item) / 2.0).max(1.0);
    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, transform, color, None, &shape);
    scene.stroke(
        &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px)),
        transform,
        crate::surface::edge_on(color),
        None,
        &crate::surface::edge_outline(rect, radius),
    );
}

/// The spoken form of a row's extra parts, for its accessible description:
/// the secondary line, the meta, the state.
#[must_use]
pub fn nav_row_description(face: &NavRowFace<'_>) -> Option<String> {
    let parts: Vec<String> = [
        face.secondary.map(str::to_owned),
        face.status_label.map(|(label, _)| label.to_owned()),
        face.status.and_then(RowStatus::spoken).map(str::to_owned),
        face.meta.map(str::to_owned),
        face.level
            .map(|fraction| format!("{}% used", (fraction * 100.0).round())),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}
fn paint_count(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    style: NavRowStyle,
    count: NavCount,
    rect: Rect,
) {
    let colors = &theme.tokens.colors;
    if style == NavRowStyle::Rail {
        // The rail has room for a mark only.
        scene.fill(
            Fill::NonZero,
            transform,
            colors.intent.you,
            None,
            &RoundedRect::from_rect(rect, rect.height() / 2.0),
        );
        return;
    }
    let text = count.text();
    let text_style = count_style(theme, count);
    let (w, h) = text_renderer.measure_style(&text, None, &text_style);
    let ink = match count {
        NavCount::Neutral(_) => colors.text_tertiary,
        NavCount::Live(_) => {
            let fill = colors.intent.you;
            scene.fill(
                Fill::NonZero,
                transform,
                fill,
                None,
                &RoundedRect::from_rect(rect, rect.height() / 2.0),
            );
            colors.on_surface(fill).ink
        }
    };
    let x = rect.center().x - f64::from(w) / 2.0;
    let y = rect.center().y - f64::from(h) / 2.0;
    text_renderer.draw_style(
        scene,
        &text,
        ink,
        transform.pre_translate(Vec2::new(x, y)),
        None,
        &text_style,
    );
}

/// The keyboard focus ring around a row: the person's own colour.
fn paint_focus(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
    let tokens = &theme.tokens;
    let width = f64::from(tokens.metrics.chrome_rule_px) * 2.0;
    let radius = f64::from(tokens.radii.item).min(rect.height() / 2.0);
    scene.stroke(
        &Stroke::new(width),
        transform,
        tokens.colors.border_focus,
        None,
        &RoundedRect::from_rect(rect.inset(-width / 2.0), (radius - width / 2.0).max(0.0)),
    );
}

/// A section header in a navigation list.
#[derive(Clone, Debug)]
pub struct NavSectionFace<'a> {
    /// The header's full band, edge to edge of the list, including the
    /// space above it ([`nav_section_height`]).
    pub rect: Rect,
    /// Sentence case; painted as written.
    pub title: &'a str,
    /// `Some(expanded)` when the section collapses.
    pub collapsible: Option<bool>,
    /// A trailing action (usually [`Icon::Plus`] for "new …").
    pub action: Option<Icon>,
    /// The pointer is over the header; a collapsible section shows its
    /// chevron then.
    pub hover: bool,
    pub action_hover: bool,
    pub focused: bool,
}

impl<'a> NavSectionFace<'a> {
    #[must_use]
    pub fn new(rect: Rect, title: &'a str) -> Self {
        Self {
            rect,
            title,
            collapsible: None,
            action: None,
            hover: false,
            action_hover: false,
            focused: false,
        }
    }

    #[must_use]
    pub fn collapsible(mut self, expanded: bool) -> Self {
        self.collapsible = Some(expanded);
        self
    }

    #[must_use]
    pub fn action(mut self, icon: Icon) -> Self {
        self.action = Some(icon);
        self
    }

    #[must_use]
    pub fn hovered(mut self, hover: bool) -> Self {
        self.hover = hover;
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
}

/// Where each part of a section header lands.
#[derive(Clone, Debug, PartialEq)]
pub struct NavSectionLayout {
    /// The header's line, below its leading space.
    pub band: Rect,
    pub title_x0: f64,
    pub title_x1: f64,
    pub disclosure: Option<Rect>,
    pub action: Option<Rect>,
}

/// Lay out a section header (see [`NavSectionLayout`]).
#[must_use]
pub fn nav_section_layout(theme: &Theme, face: &NavSectionFace<'_>) -> NavSectionLayout {
    let tokens = &theme.tokens;
    let s1 = f64::from(tokens.spacing.s1);
    let s2 = f64::from(tokens.spacing.s2);
    let r = face.rect;
    let row_h = nav_row_height(theme).min(r.height());
    let band = Rect::new(r.x0 + s2, r.y1 - row_h, r.x1 - s2, r.y1);
    let mid = band.center().y;
    let mut trailing = band.x1 - s1;
    let action = face.action.map(|_| {
        let side = (band.height() - 2.0 * s1).max(1.0);
        let rect = Rect::new(
            trailing - side,
            mid - side / 2.0,
            trailing,
            mid + side / 2.0,
        );
        trailing = rect.x0 - s1;
        rect
    });
    let disclosure = face.collapsible.map(|_| {
        let side = f64::from(tokens.metrics.icon_sm);
        let right = trailing - s1;
        let rect = Rect::new(right - side, mid - side / 2.0, right, mid + side / 2.0);
        trailing = rect.x0 - s1;
        rect
    });
    let title_x0 = band.x0 + s2;
    NavSectionLayout {
        band,
        title_x0,
        title_x1: trailing.max(title_x0),
        disclosure,
        action,
    }
}

/// Paint a section header: the title in `Subheading`, secondary ink; a
/// collapsible section's chevron while the pointer or focus is on it (or
/// while it is collapsed, so a hidden section is never mistaken for an
/// empty one); and its trailing action.
pub fn paint_nav_section(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    face: &NavSectionFace<'_>,
) {
    let layout = nav_section_layout(theme, face);
    let colors = &theme.tokens.colors;
    if face.focused {
        paint_focus(scene, transform, theme, layout.band);
    }
    let style = theme.text(TextRole::Subheading);
    let max_w = (layout.title_x1 - layout.title_x0).max(0.0);
    let title = text_renderer.elide_to_width_style(face.title, to_f32(max_w), &style);
    let (_, line_h) = text_renderer.measure_style(&title, None, &style);
    let y = crate::text_metrics::centered_text_top(
        layout.band.y0,
        layout.band.height(),
        f64::from(line_h),
    );
    text_renderer.draw_style(
        scene,
        &title,
        colors.text_dim,
        transform.pre_translate(Vec2::new(layout.title_x0, y)),
        None,
        &style,
    );
    if let (Some(expanded), Some(rect)) = (face.collapsible, layout.disclosure)
        && (face.hover || face.focused || !expanded)
    {
        let chevron = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        chevron.draw(scene, transform, rect, colors.text_tertiary);
    }
    if let (Some(icon), Some(rect)) = (face.action, layout.action) {
        if face.action_hover {
            paint_row_selection(
                scene,
                transform,
                rect,
                SelectionState::new(false, true, false),
                colors.surfaces.content,
                theme,
            );
        }
        let inset = (rect.width() - f64::from(theme.tokens.metrics.icon_sm)).max(0.0) / 2.0;
        icon.draw(
            scene,
            transform,
            rect.inset(-inset),
            if face.action_hover {
                colors.text_primary
            } else {
                colors.text_dim
            },
        );
    }
}

fn count_f64(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

fn depth_f64(depth: usize) -> f64 {
    f64::from(u32::try_from(depth).unwrap_or(u32::MAX))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "an on-screen width in logical pixels; f32 holds it exactly at every UI extent"
)]
fn to_f32(v: f64) -> f32 {
    v as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::shell::{SELECTION_MIN_STEP, lightness_step};

    fn row(theme: &Theme) -> Rect {
        Rect::new(0.0, 0.0, 280.0, nav_row_height(theme))
    }

    #[test]
    fn rows_inset_the_list_and_give_the_label_the_rest() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let face = NavRowFace::new(row(&theme), "Inbox")
            .icon(Icon::Inbox)
            .count(NavCount::Neutral(12));
        let layout = nav_row_layout(&theme, &mut text, &face);
        let s2 = f64::from(theme.tokens.spacing.s2);
        assert!((layout.selection.x0 - s2).abs() < 1e-9);
        assert!((layout.selection.x1 - (280.0 - s2)).abs() < 1e-9);
        let icon = layout.icon.expect("icon");
        assert!(icon.x0 > layout.selection.x0);
        assert!(layout.label_x0 > icon.x1);
        let count = layout.count.expect("count");
        assert!(
            layout.label_x1 < count.x0,
            "the label elides before the count"
        );
        // Nesting indents by s4.
        let nested = nav_row_layout(&theme, &mut text, &face.clone().depth(1));
        let s4 = f64::from(theme.tokens.spacing.s4);
        assert!((nested.label_x0 - layout.label_x0 - s4).abs() < 1e-9);
        // Actions take the count's place.
        let actions = [Icon::MoreHorizontal];
        let hovered = nav_row_layout(&theme, &mut text, &face.clone().actions(&actions, None));
        assert!(hovered.count.is_none());
        assert_eq!(hovered.actions.len(), 1);
        assert!(hovered.label_x1 < hovered.actions[0].x0);
    }

    #[test]
    fn section_inset_is_quiet_and_preserves_real_tree_depth_and_targets() {
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut text = TextRenderer::new();
            let face = NavRowFace::new(row(&theme), "Request").icon(Icon::File);
            let plain = nav_row_layout(&theme, &mut text, &face);
            let grouped = nav_row_layout(&theme, &mut text, &face.clone().under_section());
            assert_eq!(plain.selection, grouped.selection);
            assert_eq!(plain.label_x1, grouped.label_x1);
            assert_eq!(
                grouped.label_x0 - plain.label_x0,
                f64::from(theme.tokens.spacing.s1)
            );
            let nested = nav_row_layout(&theme, &mut text, &face.under_section().depth(1));
            assert_eq!(
                nested.label_x0 - grouped.label_x0,
                f64::from(theme.tokens.spacing.s4)
            );
        }
    }

    #[test]
    fn rows_grow_with_the_text() {
        let theme = Theme::dark();
        let big = Theme::dark().with_text_scale(2.0);
        assert!((nav_row_height(&theme) - 32.0).abs() < 1e-9);
        assert!(nav_row_height(&big) > nav_row_height(&theme));
        assert!(nav_section_height(&theme, false) > nav_section_height(&theme, true));
    }

    #[test]
    fn a_selected_row_is_a_visible_neutral_lift() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            for region in [
                crate::surface::Region::Content,
                crate::surface::Region::Sidebar,
                crate::surface::Region::Inspector,
            ] {
                let local = theme.in_region(region);
                let ground = local.tokens.colors.surfaces.content;
                let lift =
                    crate::chrome::selection_tier_on(&local, SelectionState::active(true), ground)
                        .fill;
                assert!(
                    lightness_step(lift, ground) >= SELECTION_MIN_STEP,
                    "{} {region:?}",
                    theme.name
                );
                let lift = crate::color::Oklch::from_color(lift);
                assert!(
                    lift.c <= 0.011,
                    "{} {region:?}: the lift carries no hue (C {})",
                    theme.name,
                    lift.c
                );
            }
        }
    }
}
