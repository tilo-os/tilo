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
//! Draw with the theme of the ground the rows stand on
//! (`theme.in_region(Region::Sidebar)` in a sidebar, `theme.on_ground(..)`
//! elsewhere): the painter reads that ground from `surfaces.content`.

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;

use super::shell::{SelectionState, paint_row_selection};
use crate::icons::Icon;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;

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
    /// Icon over a caption, for a compact rail.
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
    /// `Some(expanded)` when the row has children.
    pub disclosure: Option<bool>,
    /// Selected (`active`), hovered, pressed.
    pub state: SelectionState,
    /// Keyboard focus is on this row.
    pub focused: bool,
    /// Trailing actions shown on hover or focus, in order. While they show,
    /// they take the count's place.
    pub actions: &'a [Icon],
    /// The action under the pointer.
    pub action_hover: Option<usize>,
    pub style: NavRowStyle,
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
            disclosure: None,
            state: SelectionState::default(),
            focused: false,
            actions: &[],
            action_hover: None,
            style: NavRowStyle::List,
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
    pub icon: Option<Rect>,
    /// The horizontal run the label is elided into.
    pub label_x0: f64,
    pub label_x1: f64,
    /// The trailing disclosure chevron of a row with children.
    pub disclosure: Option<Rect>,
    /// The count, or `None` while actions take its place.
    pub count: Option<Rect>,
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
    let r = face.rect;
    let selection = Rect::new(r.x0 + s2, r.y0 + hairline, r.x1 - s2, r.y1 - hairline);
    let mid = selection.center().y;

    let mut x = selection.x0 + s2 + depth_f64(face.depth) * s4;
    let icon = face.icon.map(|_| {
        let side = f64::from(tokens.metrics.icon_md);
        let rect = Rect::new(x, mid - side / 2.0, x + side, mid + side / 2.0);
        x = rect.x1 + f64::from(tokens.metrics.icon_label_gap);
        rect
    });
    let label_x0 = x;

    let mut trailing = selection.x1 - s2;
    let action_side = (selection.height() - 2.0 * s1).max(1.0);
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
                let rect = Rect::new(
                    right - action_side,
                    mid - action_side / 2.0,
                    right,
                    mid + action_side / 2.0,
                );
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
    let count = if actions.is_empty() {
        face.count.map(|count| {
            let style = count_style(theme, count);
            let (w, h) = text_renderer.measure_style(&count.text(), None, &style);
            let (w, h) = (f64::from(w), f64::from(h));
            let rect = match count {
                NavCount::Neutral(_) => {
                    Rect::new(trailing - w, mid - h / 2.0, trailing, mid + h / 2.0)
                }
                NavCount::Live(_) => {
                    let height = h + s1;
                    let width = (w + s2).max(height);
                    Rect::new(
                        trailing - width,
                        mid - height / 2.0,
                        trailing,
                        mid + height / 2.0,
                    )
                }
            };
            trailing = rect.x0 - s2;
            rect
        })
    } else {
        None
    };
    let disclosure = face.disclosure.map(|_| {
        let side = f64::from(tokens.metrics.icon_sm);
        let rect = Rect::new(
            trailing - side,
            mid - side / 2.0,
            trailing,
            mid + side / 2.0,
        );
        trailing = rect.x0 - s2;
        rect
    });
    NavRowLayout {
        selection,
        icon,
        label_x0,
        label_x1: trailing.max(label_x0),
        disclosure,
        count,
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
        icon: Some(icon),
        label_x0: selection.x0 + s1,
        label_x1: selection.x1 - s1,
        disclosure: None,
        count,
        actions: Vec::new(),
    }
}

/// Paint one navigation row. `theme` is the theme of the ground the row
/// stands on; its `surfaces.content` is that ground.
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
    if let (Some(icon), Some(rect)) = (face.icon, layout.icon) {
        icon.draw(scene, transform, rect, ink);
    }

    let style = label_style(theme, face);
    let measure = label_measure_style(theme, face.style);
    let max_w = (layout.label_x1 - layout.label_x0).max(0.0);
    let label = text_renderer.elide_to_width_style(face.label, to_f32(max_w), &measure);
    let (label_w, line_h) = text_renderer.measure_style(&label, None, &style);
    let (x, y) = match face.style {
        NavRowStyle::List => (
            layout.label_x0,
            crate::text_metrics::centered_text_top(
                layout.selection.y0,
                layout.selection.height(),
                f64::from(line_h),
            ),
        ),
        NavRowStyle::Rail => (
            layout.selection.center().x - f64::from(label_w) / 2.0,
            layout.icon.map_or(layout.selection.y0, |icon| icon.y1) + f64::from(tokens.spacing.s1),
        ),
    };
    text_renderer.draw_style(
        scene,
        &label,
        ink,
        transform.pre_translate(Vec2::new(x, y)),
        None,
        &style,
    );

    if let (Some(expanded), Some(rect)) = (face.disclosure, layout.disclosure) {
        let chevron = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        chevron.draw(scene, transform, rect, colors.text_tertiary);
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
    for (index, (icon, rect)) in face.actions.iter().zip(&layout.actions).enumerate() {
        let hovered = face.action_hover == Some(index);
        if hovered {
            paint_row_selection(
                scene,
                transform,
                *rect,
                SelectionState::new(false, true, false),
                // The action lifts off the row it sits on.
                if face.lifted() {
                    colors.lift_on(ground, 1.0)
                } else if face.state.hover {
                    colors.lift_on(ground, super::shell::HOVER_LIFT_T)
                } else {
                    ground
                },
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
