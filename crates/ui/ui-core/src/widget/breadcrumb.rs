//! Breadcrumb widget — a horizontal path indicator with clickable ancestor segments and a chevron icon
//! between them.
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::widget::text::centered_text_top;
use tilo_ui_base::icons::Icon;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;

#[path = "breadcrumb_component.rs"]
mod component;
pub use component::{BreadcrumbProps, BreadcrumbSegmentProps};

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
    key: String,
    /// All segments, in path order. The rightmost is the active page.
    segments: Vec<String>,
    on_navigate: Option<Box<dyn FnMut(usize)>>,
    segment_rects: Vec<Rect>,
    /// Original-index mapping for each entry in `segment_rects`: `None` for
    /// the (non-clickable) ellipsis placeholder.
    visible_origin: Vec<Option<usize>>,
    /// Cached visible-segment list as of the last `apply_layout`, so the
    /// draw pass and the click handler agree on labels without recomputing.
    cached_visible: Vec<String>,
    hover_idx: Option<usize>,
    root_icon: Option<Icon>,
    last_cursor: Option<Point>,
    on_edit_requested: Option<Box<dyn FnMut()>>,
    on_overflow: Option<Box<dyn FnMut(Vec<usize>, Rect)>>,
    role: TextRole,
    rect: Rect,

    dirty: bool,
}

impl Breadcrumb {
    pub fn new(segments: Vec<String>) -> Self {
        Self {
            dirty: true,
            key: String::new(),
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

    #[must_use]
    pub fn root_icon(mut self, icon: Icon) -> Self {
        self.root_icon = Some(icon);
        self
    }

    #[must_use]
    pub fn on_edit_requested<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_edit_requested = Some(Box::new(f));
        self
    }

    /// Called with the index of the segment gone to, into the original
    /// `segments` (never the visible list's, so middle truncation never
    /// confuses the app), whether a click, a key or a screen reader chose it.
    /// `id` names the trail. Where the window is shown is its own state,
    /// never an edit.
    #[must_use]
    pub fn on_choose<F: FnMut(usize) + 'static>(mut self, id: impl Into<String>, f: F) -> Self {
        self.key = id.into();
        self.on_navigate = Some(Box::new(f));
        self
    }

    #[must_use]
    pub(crate) fn on_navigate<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_navigate = Some(Box::new(f));
        self
    }

    fn navigate(&mut self, index: usize) {
        if let Some(navigate) = self.on_navigate.as_mut() {
            navigate(index);
        }
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
    /// The whole trail when it fits; otherwise the first segment, `...`
    /// and the last two; then the first, `...` and the last; then `...`
    /// and the last (for a trail of two as well), the last cut short where
    /// it does not stand whole; and at the narrowest, where the `...`
    /// leaves the last no width for a letter, the last alone, cut short.
    /// The current folder always shows.
    /// Pure function — does not mutate `self`. Exposed for tests that want
    /// to pin the truncation behaviour without running a full layout pass.
    pub fn visible_segments(
        &self,
        available_width: f64,
        theme: &Theme,
        tr: &mut TextRenderer,
    ) -> Vec<String> {
        self.labels_of(&self.visible_origins(available_width, theme, tr))
    }

    fn visible_origins(
        &self,
        available_width: f64,
        theme: &Theme,
        tr: &mut TextRenderer,
    ) -> Vec<Option<usize>> {
        let n = self.segments.len();
        let full: Vec<Option<usize>> = (0..n).map(Some).collect();
        if n < 2 {
            return full;
        }
        let last = n - 1;
        let mut candidates = vec![full];
        if n > 3 {
            candidates.push(vec![Some(0), None, Some(last - 1), Some(last)]);
        }
        if n > 2 {
            candidates.push(vec![Some(0), None, Some(last)]);
        }
        candidates.push(vec![None, Some(last)]);
        // The current place's name stands whole before a trail that cuts it: it is what the
        // trail is for.
        candidates.push(vec![Some(last)]);
        if let Some(fits) = candidates
            .iter()
            .find(|origins| self.width_of(origins, theme, tr) <= available_width)
        {
            return fits.clone();
        }
        vec![Some(last)]
    }

    fn labels_of(&self, origins: &[Option<usize>]) -> Vec<String> {
        origins
            .iter()
            .map(|origin| match origin {
                Some(index) => self.segments.get(*index).cloned().unwrap_or_default(),
                None => ELLIPSIS.to_string(),
            })
            .collect()
    }

    fn segment_width(&self, origin: Option<usize>, theme: &Theme, tr: &mut TextRenderer) -> f64 {
        let style = theme.text(self.role);
        match origin {
            None => f64::from(theme.tokens.metrics.icon_sm),
            Some(index) => {
                let label = self.segments.get(index).map_or("", String::as_str);
                f64::from(tr.measure_style(label, None, &style).0)
                    + if index == 0 {
                        self.root_width(theme)
                    } else {
                        0.0
                    }
            }
        }
    }

    fn separator_block(&self, theme: &Theme) -> f64 {
        separator_width(theme) + f64::from(theme.tokens.spacing.s2) * 2.0
    }

    fn width_of(&self, origins: &[Option<usize>], theme: &Theme, tr: &mut TextRenderer) -> f64 {
        let sep_block = self.separator_block(theme);
        let labels: f64 = origins
            .iter()
            .map(|origin| self.segment_width(*origin, theme, tr))
            .sum();
        labels + sep_block * origins.len().saturating_sub(1) as f64
    }

    /// Recompute `cached_visible`, `segment_rects`, and `visible_origin`
    /// from the current `self.rect` and segment list. Called from
    /// `apply_layout` and the test helpers so the click hit-test and
    /// draw pass operate on the same geometry.
    fn rebuild_rects(&mut self, theme: &Theme, tr: &mut TextRenderer) {
        let available = self.rect.width().max(0.0);
        let origin = self.visible_origins(available, theme, tr);
        // Lay the labels out left-to-right; the chevron between segments is
        // painted by `draw` but never needs its own rect (never clickable).
        // The root icon leads the first segment's rect.
        let sep_pad = f64::from(theme.tokens.spacing.s2);
        let sep_w = separator_width(theme);
        let mut rects = Vec::with_capacity(origin.len());
        let mut cursor_x = self.rect.x0;
        for (i, shown) in origin.iter().enumerate() {
            let w = self.segment_width(*shown, theme, tr);
            rects.push(Rect::new(
                cursor_x,
                self.rect.y0,
                cursor_x + w,
                self.rect.y1,
            ));
            cursor_x += w;
            if i.saturating_add(1) < origin.len() {
                cursor_x += sep_pad + sep_w + sep_pad;
            }
        }
        let mut labels = self.labels_of(&origin);
        if let (Some(rect), Some(label)) = (rects.last_mut(), labels.last_mut())
            && rect.x1 > self.rect.x1
        {
            rect.x1 = self.rect.x1.max(rect.x0);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a segment's width is a bounded on-screen size, far inside f32"
            )]
            let spare = rect.width() as f32;
            let style = theme.text(self.role);
            *label = tr.elide_to_width_style(label, spare, &style);
            // Too little width for even the ellipsis: nothing is painted
            // rather than something past the trail's end.
            if tr.measure_style(label, None, &style).0 > spare {
                label.clear();
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

    fn root_width(&self, theme: &Theme) -> f64 {
        self.root_icon.map_or(0.0, |_| {
            f64::from(theme.tokens.metrics.icon_sm) + f64::from(theme.tokens.metrics.icon_label_gap)
        })
    }

    fn in_empty_space(&self, point: Point) -> bool {
        let end = self
            .segment_rects
            .last()
            .map_or(self.rect.x0, |rect| rect.x1);
        self.rect.contains(point) && point.x > end
    }

    pub(crate) fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub(crate) fn ancestor_at(&self, point: Point) -> Option<usize> {
        let active = self.segments.len().saturating_sub(1);
        self.segment_rects
            .iter()
            .zip(&self.visible_origin)
            .find(|(rect, _)| rect.contains(point))
            .and_then(|(_, origin)| *origin)
            .filter(|index| *index != active)
    }

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

impl Widget for Breadcrumb {
    tilo_ui_base::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        // Per-segment rects need text measurements, which need a
        // `TextRenderer` we don't have here — deferred to `draw`. Stale
        // rects until the first paint are fine: no hover happens before it.
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
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
        if let (Some(icon), Some(first), Some(Some(0))) = (
            self.root_icon,
            self.segment_rects.first(),
            self.visible_origin.first(),
        ) {
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
            if i.saturating_add(1) < n {
                let edge = separator_width(theme);
                let centre = Point::new(r.x1 + sep_pad + edge / 2.0, self.rect.center().y);
                tilo_ui_base::icons::draw_icon(
                    scene,
                    transform,
                    Rect::from_center_size(centre, (edge, edge)),
                    tilo_ui_base::icons::Icon::ChevronRight,
                    text_dim,
                    tilo_ui_base::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        self.dirty = true;
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
                    self.navigate(orig);
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

    fn needs_redraw(&self) -> bool {
        tilo_ui_base::widget::redraw_or_children(self.dirty, self)
    }

    /// Its box is the width it is given and one line tall: what it shows truncates to it as it
    /// is drawn, so once placed it asks for no layout.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        tilo_ui_base::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "Breadcrumb"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        // Segments are drawn, not child widgets, so the container carries
        // the list semantics; the rightmost segment is surfaced as the value.
        let value = self.segments.last().cloned();
        Some(
            AccessNode::new(Role::List)
                .with_name(tilo_ui_base::i18n::widget_format("widget-breadcrumb", &[]))
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
    use vello::Scene;
    use vello::kurbo::Affine;

    fn cursor_at(p: Point) -> WindowEvent {
        tilo_ui_base::synthetic_input::cursor_moved(p.x, p.y)
    }

    use tilo_ui_base::synthetic_input::mouse_press;

    tilo_test_env::scenario! {
        /// The breadcrumb.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_breadcrumb_paints_and_navigates_its_segments;

        phase breadcrumb_paints_separators_centres_labels_and_truncates;
        {
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
                bc.draw(&mut tilo_ui_base::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme, tilo_ui_base::presence::PresenceCtx::default().draw_env()));
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
                let labels =
                    |shown: &[&str]| shown.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
                let first_two_last = [Some(0), None, Some(8), Some(9)];
                let first_last = [Some(0), None, Some(9)];
                let wide = bc.width_of(&first_two_last, &theme, &mut tr);
                assert_eq!(
                    bc.visible_segments(wide, &theme, &mut tr),
                    labels(&["segment_00", ELLIPSIS, "segment_08", "segment_09"]),
                    "a long path collapses to the first, `...` and the last two"
                );
                let middling = bc.width_of(&first_last, &theme, &mut tr);
                assert!(middling < wide);
                assert_eq!(
                    bc.visible_segments(middling, &theme, &mut tr),
                    labels(&["segment_00", ELLIPSIS, "segment_09"]),
                    "narrower, the second-last goes too"
                );
                assert_eq!(
                    bc.visible_segments(middling - 1.0, &theme, &mut tr),
                    labels(&[ELLIPSIS, "segment_09"]),
                    "at the narrowest the current folder still shows"
                );
                assert!(
                    bc.width_of(&[None, Some(9)], &theme, &mut tr) <= middling - 1.0,
                    "and fits"
                );
            }

            breadcrumb_with_one_segment_paints_no_separator();
            breadcrumb_labels_and_chevrons_centre_their_measured_line();
            breadcrumb_with_long_path_truncates_middle_segments();
        }

        phase breadcrumb_navigation_and_semantics_name_the_current_segment;
        {
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
                use tilo_ui_base::a11y::Role;
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
}
