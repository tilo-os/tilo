//! Breadcrumb widget — a horizontal path indicator with clickable ancestor
//! segments and a chevron icon between them.
//!
//! The trail takes the width it is given. When its segments do not fit, it
//! truncates the middle: keeps the first segment, puts the
//! [`Icon::MoreHorizontal`] glyph in place of the hidden ones, keeps the
//! last two. A trail of 4 or fewer segments never truncates (the truncated
//! form wouldn't be shorter).
//!
//! An optional root icon ([`Breadcrumb::root_icon`]) leads the first
//! segment (a drive, Home). A trail that doubles as an editable path
//! reports a click on its empty space through
//! [`Breadcrumb::on_edit_requested`], so the host can swap in a field.
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::icons::Icon;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::text::centered_text_top;

/// The separator between two segments: a chevron from the one icon set,
/// at the small glyph size, never a typed character.
fn separator_width(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.icon_sm)
}

/// The stand-in for the hidden middle segments in the visible list; it
/// paints as [`Icon::MoreHorizontal`], never as text.
const ELLIPSIS: &str = "...";

/// `segment_rects` and `visible_origin` mirror the truncated
/// `cached_visible` list (not the original `segments`) so the click
/// hit-test matches what's on screen.
pub struct Breadcrumb {
    /// All segments, in path order. The rightmost is the active page.
    segments: Vec<String>,
    on_navigate: Option<Box<dyn FnMut(usize)>>,
    /// Per-visible-segment hit rects, populated in `apply_layout`.
    segment_rects: Vec<Rect>,
    /// Original-index mapping for each entry in `segment_rects`: `None` for
    /// the (non-clickable) ellipsis placeholder.
    visible_origin: Vec<Option<usize>>,
    /// Cached visible-segment list as of the last `apply_layout`, so the
    /// draw pass and the click handler agree on labels without recomputing.
    cached_visible: Vec<String>,
    /// Index (into `cached_visible`) the cursor is hovering. Ellipsis
    /// entries don't latch hover.
    hover_idx: Option<usize>,
    /// A glyph leading the first segment.
    root_icon: Option<Icon>,
    last_cursor: Option<Point>,
    /// A click on the trail's empty space asks to edit the path.
    on_edit_requested: Option<Box<dyn FnMut()>>,
    /// A click on the `…` hands the hidden segments and the `…`'s rect to
    /// the host, to offer them in a menu.
    on_overflow: Option<Box<dyn FnMut(Vec<usize>, Rect)>>,
    /// The labels' text role: a caption in a header, body text where the
    /// trail is a window's location.
    role: TextRole,
    rect: Rect,
}

impl Breadcrumb {
    /// The last segment is treated as the active (current) page and
    /// rendered non-interactively; everything before it is clickable.
    pub fn new(segments: Vec<String>) -> Self {
        Self {
            segments,
            on_navigate: None,
            segment_rects: Vec::new(),
            visible_origin: Vec::new(),
            cached_visible: Vec::new(),
            hover_idx: None,
            root_icon: None,
            last_cursor: None,
            on_edit_requested: None,
            on_overflow: None,
            role: TextRole::Caption,
            rect: Rect::ZERO,
        }
    }

    /// Set the labels in `role` (a window's location reads as body text).
    #[must_use]
    pub(crate) fn with_role(mut self, role: TextRole) -> Self {
        self.role = role;
        self
    }

    /// Called with the indices of the segments the `…` stands for, and its
    /// rect, when the person clicks it: the host offers them in a menu, so
    /// no ancestor of a deep path is out of reach.
    #[must_use]
    pub fn on_overflow<F: FnMut(Vec<usize>, Rect) + 'static>(mut self, f: F) -> Self {
        self.on_overflow = Some(Box::new(f));
        self
    }

    /// Lead the first segment with `icon` (a drive, Home). It belongs to
    /// the first segment: a click on it navigates there.
    #[must_use]
    pub fn root_icon(mut self, icon: Icon) -> Self {
        self.root_icon = Some(icon);
        self
    }

    /// Called when the person clicks the trail's empty space, past its last
    /// segment: the host swaps the trail for an editable path field.
    #[must_use]
    pub fn on_edit_requested<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_edit_requested = Some(Box::new(f));
        self
    }

    /// Builder: install a click callback. The callback receives the index
    /// into the original `segments` vector — never the visible-list index
    /// — so middle-truncation never confuses the consumer.
    #[must_use]
    pub fn on_navigate<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_navigate = Some(Box::new(f));
        self
    }

    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// `segments.len() - 1` for two or more segments, zero for a single
    /// segment. Exposed explicitly so a regression that paints a stray
    /// chevron for one-segment trails flips the suite red.
    pub fn separator_count(&self) -> usize {
        self.segments.len().saturating_sub(1)
    }

    /// Compute the list of visible labels for the given available width.
    /// Falls back to `self.segments` when everything fits; otherwise
    /// applies the keep-first / `...` / keep-last-two rule. Pure function
    /// — does not mutate `self`. Exposed for tests that want to pin the
    /// truncation behaviour without running a full layout pass.
    pub fn visible_segments(
        &self,
        available_width: f64,
        theme: &Theme,
        tr: &mut TextRenderer,
    ) -> Vec<String> {
        if self.segments.is_empty() {
            return Vec::new();
        }
        let full_width =
            measure_total_width(&self.segments, self.role, theme, tr) + self.root_width(theme);
        if full_width <= available_width {
            return self.segments.clone();
        }

        // The truncated form is no shorter than the full form here, so
        // truncating would only make the render worse.
        if self.segments.len() <= 4 {
            return self.segments.clone();
        }

        // At least 5 segments here (the `<= 4` guard above), so this slice
        // pattern always matches — reading first/second-last/last through
        // it keeps the lookup indexing-free rather than computing `n - 1`
        // / `n - 2` and indexing with them.
        if let [first, .., second_last, last] = self.segments.as_slice() {
            vec![
                first.clone(),
                ELLIPSIS.to_string(),
                second_last.clone(),
                last.clone(),
            ]
        } else {
            self.segments.clone()
        }
    }

    /// Recompute `cached_visible`, `segment_rects`, and `visible_origin`
    /// from the current `self.rect` and segment list. Called from
    /// `apply_layout` and the test helpers so the click hit-test and
    /// draw pass operate on the same geometry.
    fn rebuild_rects(&mut self, theme: &Theme, tr: &mut TextRenderer) {
        let available = self.rect.width().max(0.0);
        let labels = self.visible_segments(available, theme, tr);
        // Unambiguous: when truncation fires we know exactly which
        // originals survive (first + last two); otherwise each visible
        // index equals the original index.
        let n = self.segments.len();
        let truncated = labels.len() == 4 && n > 4 && labels.get(1).is_some_and(|l| l == ELLIPSIS);
        let mut origin = Vec::with_capacity(labels.len());
        if truncated {
            origin.push(Some(0));
            origin.push(None); // ellipsis is non-clickable
            origin.push(Some(n.saturating_sub(2)));
            origin.push(Some(n.saturating_sub(1)));
        } else {
            for i in 0..labels.len() {
                origin.push(Some(i));
            }
        }

        // Lay the labels out left-to-right; the chevron between segments is
        // painted by `draw` but never needs its own rect (never clickable).
        // The root icon leads the first segment's rect.
        let style = theme.text(self.role);
        let sep_pad = f64::from(theme.tokens.spacing.s2);
        let sep_w = separator_width(theme);
        let mut rects = Vec::with_capacity(labels.len());
        let mut cursor_x = self.rect.x0;
        let pill_y0 = self.rect.y0;
        let pill_y1 = self.rect.y1;
        for (i, label) in labels.iter().enumerate() {
            let w = if label == ELLIPSIS && origin.get(i).copied().flatten().is_none() {
                f64::from(theme.tokens.metrics.icon_sm)
            } else {
                f64::from(tr.measure_style(label, None, &style).0)
            } + if i == 0 { self.root_width(theme) } else { 0.0 };
            let r = Rect::new(cursor_x, pill_y0, cursor_x + w, pill_y1);
            rects.push(r);
            cursor_x += w;
            if i.saturating_add(1) < labels.len() {
                cursor_x += sep_pad + sep_w + sep_pad;
            }
        }

        self.cached_visible = labels;
        self.segment_rects = rects;
        self.visible_origin = origin;
    }

    /// Each painted segment's rect with the index of the segment it shows,
    /// as laid out at the last paint; the inert `…` is left out. For a host
    /// that makes the trail a drop target.
    pub fn segment_rects(&self) -> Vec<(usize, Rect)> {
        self.visible_origin
            .iter()
            .zip(self.segment_rects.iter())
            .filter_map(|(origin, rect)| origin.map(|index| (index, *rect)))
            .collect()
    }

    /// The room the root icon takes before the first label, with its gap.
    fn root_width(&self, theme: &Theme) -> f64 {
        self.root_icon.map_or(0.0, |_| {
            f64::from(theme.tokens.metrics.icon_sm) + f64::from(theme.tokens.metrics.icon_label_gap)
        })
    }

    /// The trail's empty space: past the last segment, inside the rect.
    fn in_empty_space(&self, point: Point) -> bool {
        let end = self
            .segment_rects
            .last()
            .map_or(self.rect.x0, |rect| rect.x1);
        self.rect.contains(point) && point.x > end
    }

    /// Place the trail in `rect`, for a host that draws it without a layout
    /// pass (a window's bar). Its segments are laid out at the next draw.
    pub(crate) fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// The segment a click at `point` goes to: an ancestor, as laid out at
    /// the last draw; not the current segment, nor the `…`.
    pub(crate) fn ancestor_at(&self, point: Point) -> Option<usize> {
        let active = self.segments.len().saturating_sub(1);
        self.segment_rects
            .iter()
            .zip(&self.visible_origin)
            .find(|(rect, _)| rect.contains(point))
            .and_then(|(_, origin)| *origin)
            .filter(|index| *index != active)
    }

    /// Light the segment under `point` (`None`: the pointer is off the
    /// trail), as a hover does. Returns whether that changed.
    pub(crate) fn hover_at(&mut self, point: Option<Point>) -> bool {
        let active = self.segments.len().saturating_sub(1);
        let hover = point.and_then(|point| {
            self.segment_rects
                .iter()
                .zip(&self.visible_origin)
                .position(|(rect, origin)| {
                    rect.contains(point) && origin.is_some_and(|index| index != active)
                })
        });
        let changed = hover != self.hover_idx;
        self.hover_idx = hover;
        changed
    }

    /// Test-only helper: rebuild rects against a synthetic `self.rect`
    /// without running a full taffy layout pass. Keeps the click test
    /// independent of GPU + taffy availability.
    #[cfg(test)]
    fn apply_layout_helper(&mut self, theme: &Theme, tr: &mut TextRenderer) {
        self.rebuild_rects(theme, tr);
    }
}

/// Sum the widths of all `labels` + interleaved separators (with `s2`
/// padding on each side of each separator). Used by
/// [`Breadcrumb::visible_segments`] to decide whether the full path
/// fits. A free function (not a method) since it touches no `self` field.
fn measure_total_width(
    labels: &[String],
    role: TextRole,
    theme: &Theme,
    tr: &mut TextRenderer,
) -> f64 {
    let style = theme.text(role);
    let sep_pad = f64::from(theme.tokens.spacing.s2);
    let sep_block = separator_width(theme) + sep_pad * 2.0;
    let mut total = 0.0_f64;
    for (i, label) in labels.iter().enumerate() {
        let (w, _) = tr.measure_style(label, None, &style);
        total += f64::from(w);
        if i.saturating_add(1) < labels.len() {
            total += sep_block;
        }
    }
    total
}

impl Widget for Breadcrumb {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The trail takes the width its parent gives it (and truncates to
        // it); the height is a line of `Caption` with room for a click.
        // In a row it shrinks to what its neighbours leave and stretches to
        // the row's height; in a column it spans the column.
        let line_h = theme.tokens.typography.size(self.role) * 1.5;
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            min_size: Size::from_lengths(0.0, line_h),
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        // Per-segment rects need text measurements, which need a
        // `TextRenderer` we don't have here — deferred to `draw`. Stale
        // rects until the first paint are fine: no hover happens before it.
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        // Rebuild rects every frame so a window-resize that shrinks the
        // available width retriggers truncation on the next paint.
        self.rebuild_rects(theme, text_renderer);

        let style = theme.text(self.role);
        let sep_pad = f64::from(theme.tokens.spacing.s2);
        // One measured line height serves every segment and chevron: all are
        // single lines at one style, so cosmic-text gives each the same line
        // box. Centre that, not the font style — the latter sat the whole
        // trail 1.5 px below the row's centre.
        let (_, line_h) = text_renderer.measure_style("Home", None, &style);
        let text_y = centered_text_top(self.rect.y0, self.rect.height(), f64::from(line_h));

        let active_origin = self.segments.len().saturating_sub(1);
        let text_primary = theme.tokens.colors.text_primary;
        let text_dim = theme.tokens.colors.text_dim;

        let labels = self.cached_visible.clone();
        let n = labels.len();
        if let (Some(icon), Some(first)) = (self.root_icon, self.segment_rects.first()) {
            let edge = f64::from(theme.tokens.metrics.icon_sm);
            let hovered = self.hover_idx == Some(0) && active_origin != 0;
            icon.draw(
                scene,
                transform,
                Rect::from_center_size(
                    Point::new(first.x0 + edge / 2.0, self.rect.center().y),
                    (edge, edge),
                ),
                if hovered || active_origin == 0 {
                    text_primary
                } else {
                    text_dim
                },
            );
        }
        // Zipped rather than indexed by a shared `i`: `rebuild_rects` builds
        // `cached_visible`, `segment_rects` and `visible_origin` together
        // from the same label list, so they're always the same length —
        // zipping makes that invariant visible instead of trusting three
        // separate index lookups to stay in bounds.
        for (i, ((label, &r), &orig)) in labels
            .iter()
            .zip(self.segment_rects.iter())
            .zip(self.visible_origin.iter())
            .enumerate()
        {
            // The ellipsis lights up under the pointer only when it opens a
            // menu (it latches hover only then).
            let color = match orig {
                Some(idx) if idx == active_origin => text_primary,
                _ if Some(i) == self.hover_idx => text_primary,
                _ => text_dim,
            };
            if orig.is_none() {
                let edge = f64::from(theme.tokens.metrics.icon_sm);
                Icon::MoreHorizontal.draw(
                    scene,
                    transform,
                    Rect::from_center_size(
                        Point::new(r.center().x, self.rect.center().y),
                        (edge, edge),
                    ),
                    color,
                );
            } else {
                let x = if i == 0 {
                    r.x0 + self.root_width(theme)
                } else {
                    r.x0
                };
                text_renderer.draw_style(
                    scene,
                    label,
                    color,
                    transform.pre_translate((x, text_y).into()),
                    None,
                    &style,
                );
            }
            // Always dim — the chevron is chrome, not content.
            if i.saturating_add(1) < n {
                let edge = separator_width(theme);
                let centre = Point::new(r.x1 + sep_pad + edge / 2.0, self.rect.center().y);
                crate::icons::draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(centre, (edge, edge)),
                    crate::icons::Icon::ChevronRight,
                    text_dim,
                    crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = ctx.logical(position);
                self.last_cursor = Some(logical);
                let mut new_hover: Option<usize> = None;
                let active_origin = self.segments.len().saturating_sub(1);
                for (i, r) in self.segment_rects.iter().enumerate() {
                    if !r.contains(logical) {
                        continue;
                    }
                    // Only clickable segments latch hover — the active
                    // segment and the ellipsis are inert.
                    match self.visible_origin.get(i).copied() {
                        Some(Some(idx)) if idx != active_origin => {
                            new_hover = Some(i);
                            break;
                        }
                        // The `…` is a control when the host offers a menu.
                        Some(None) if self.on_overflow.is_some() => {
                            new_hover = Some(i);
                            break;
                        }
                        _ => {}
                    }
                }
                if new_hover != self.hover_idx {
                    self.hover_idx = new_hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(hover_idx) = self.hover_idx
                    && let Some(Some(orig)) = self.visible_origin.get(hover_idx).copied()
                {
                    if let Some(cb) = self.on_navigate.as_mut() {
                        cb(orig);
                    }
                    *ctx.handled = true;
                } else if let Some(hover_idx) = self.hover_idx
                    && self.visible_origin.get(hover_idx).copied() == Some(None)
                    && let Some(rect) = self.segment_rects.get(hover_idx).copied()
                    && let Some(cb) = self.on_overflow.as_mut()
                {
                    let shown: Vec<usize> = self.visible_origin.iter().flatten().copied().collect();
                    let hidden = (0..self.segments.len())
                        .filter(|index| !shown.contains(index))
                        .collect();
                    cb(hidden, rect);
                    *ctx.handled = true;
                } else if let Some(point) = self.last_cursor
                    && self.in_empty_space(point)
                    && let Some(cb) = self.on_edit_requested.as_mut()
                {
                    cb();
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Breadcrumb"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Segments are drawn, not child widgets, so the container carries
        // the list semantics; the rightmost segment is surfaced as the value.
        let value = self.segments.last().cloned();
        Some(
            AccessNode::new(Role::List)
                .with_name(crate::i18n::widget_format("widget-breadcrumb", &[]))
                .with_optional_value(value)
                .with_state(AccessState::default()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::dispatch;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn cursor_at(p: Point) -> WindowEvent {
        crate::synthetic_input::cursor_moved(p.x, p.y)
    }

    use crate::synthetic_input::mouse_press;

    #[test]
    fn breadcrumb_paints_separators_centres_labels_and_truncates() {
        fn breadcrumb_with_one_segment_paints_no_separator() {
            let bc = Breadcrumb::new(vec!["home".to_string()]);
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let mut bc = bc;
            bc.rect = Rect::new(0.0, 0.0, 500.0, 24.0);
            bc.apply_layout_helper(&theme, &mut tr);
            assert_eq!(
                bc.separator_count(),
                0,
                "one-segment breadcrumb must report zero separators"
            );
            assert_eq!(bc.cached_visible, vec!["home".to_string()]);
        }

        /// Labels and chevrons are centred on the line the renderer measured,
        /// not on the font size — the latter sat every segment 1.5 px below the
        /// row's centre (12 px type lays out as a 15 px line).
        fn breadcrumb_labels_and_chevrons_centre_their_measured_line() {
            let mut bc = Breadcrumb::new(vec!["home".to_string(), "docs".to_string()]);
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            // An 18 px band, as `build_layout` gives a small-type trail.
            let rect = Rect::new(10.0, 40.0, 500.0, 58.0);
            bc.rect = rect;
            bc.apply_layout_helper(&theme, &mut tr);
            let mut scene = Scene::new();
            bc.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &crate::presence::PresenceCtx::default(),
            );
            assert_eq!(
                scene.encoding().resources.glyph_runs.len(),
                2,
                "home and docs; the chevron is an icon, not a glyph"
            );
            let style = theme.text(TextRole::Caption);
            for (index, text) in ["home", "docs"].into_iter().enumerate() {
                crate::widget::text::assert_glyph_run_centred_in_band(
                    &scene,
                    index,
                    text,
                    &style,
                    rect.y0,
                    rect.height(),
                    &mut tr,
                    theme.text_primary,
                );
            }
        }

        fn breadcrumb_with_long_path_truncates_middle_segments() {
            let segments: Vec<String> = (0..10).map(|i| format!("segment_{i:02}")).collect();
            let bc = Breadcrumb::new(segments);
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();

            // 200px at `sm` (12px text) fits the four-label fallback but
            // rejects the ten-label full path.
            let visible = bc.visible_segments(200.0, &theme, &mut tr);
            assert_eq!(
                visible,
                vec![
                    "segment_00".to_string(),
                    ELLIPSIS.to_string(),
                    "segment_08".to_string(),
                    "segment_09".to_string(),
                ],
                "long path must collapse to first + ellipsis + last two segments"
            );
        }

        breadcrumb_with_one_segment_paints_no_separator();
        breadcrumb_labels_and_chevrons_centre_their_measured_line();
        breadcrumb_with_long_path_truncates_middle_segments();
    }

    #[test]
    fn breadcrumb_navigation_and_semantics_name_the_current_segment() {
        fn breadcrumb_clicking_middle_segment_calls_on_navigate_with_that_index() {
            let received = Rc::new(RefCell::new(None::<usize>));
            let received_clone = received.clone();
            let mut bc = Breadcrumb::new(vec![
                "home".to_string(),
                "docs".to_string(),
                "project".to_string(),
            ])
            .on_navigate(move |idx| {
                *received_clone.borrow_mut() = Some(idx);
            });

            bc.rect = Rect::new(0.0, 0.0, 800.0, 24.0);
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            bc.apply_layout_helper(&theme, &mut tr);

            assert_eq!(
                bc.segment_rects.len(),
                3,
                "expected one rect per visible segment"
            );
            let middle_center = bc.segment_rects[1].center();
            dispatch(&mut bc, &cursor_at(middle_center));
            dispatch(&mut bc, &mouse_press());

            assert_eq!(
                *received.borrow(),
                Some(1),
                "clicking the middle segment must call on_navigate with the original index 1"
            );
        }

        fn accessibility_reports_list_role_name_and_current_segment() {
            use crate::a11y::Role;
            let bc = Breadcrumb::new(vec![
                "home".to_string(),
                "docs".to_string(),
                "readme.md".to_string(),
            ]);
            let node = Widget::accessibility(&bc).expect("breadcrumb is not presentational");
            assert_eq!(node.role, Role::List);
            assert_eq!(node.name.as_deref(), Some("Path"));
            assert_eq!(
                node.value.as_deref(),
                Some("readme.md"),
                "the rightmost segment is the current page"
            );
        }

        fn accessibility_empty_trail_has_no_value() {
            let bc = Breadcrumb::new(Vec::new());
            let node = Widget::accessibility(&bc).unwrap();
            assert_eq!(node.value, None, "empty trail → no current segment");
        }

        breadcrumb_clicking_middle_segment_calls_on_navigate_with_that_index();
        accessibility_reports_list_role_name_and_current_segment();
        accessibility_empty_trail_has_no_value();
    }
}
