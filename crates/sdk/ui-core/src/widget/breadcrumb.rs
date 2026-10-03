//! Breadcrumb widget — a horizontal path indicator with clickable ancestor
//! segments and a chevron (`>`) separator between them.
//!
//! When the total width exceeds the available rect, the widget truncates
//! the middle with `...`: keeps the first segment, inserts `...`, keeps the
//! last two. A trail of 4 or fewer segments never truncates (the truncated
//! form wouldn't be shorter).
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::text_metrics::renderer_type_size;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::text::centered_text_top;

/// ASCII `>` so it always falls back to the system font even when no
/// fancy chevron face is loaded.
const SEPARATOR_GLYPH: &str = ">";

/// Three ASCII dots (rather than the `…` codepoint) keep the truncation
/// visible in plain-text dumps.
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
            rect: Rect::ZERO,
        }
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
        let full_width = measure_total_width(&self.segments, theme, tr);
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
        let size = renderer_type_size(tr, theme.tokens.typography.scale.sm);
        let sep_pad = f64::from(theme.tokens.spacing.s2);
        let (sep_w, _sep_h) = tr.measure(SEPARATOR_GLYPH, size, None);
        let sep_w = f64::from(sep_w);
        let mut rects = Vec::with_capacity(labels.len());
        let mut cursor_x = self.rect.x0;
        let pill_y0 = self.rect.y0;
        let pill_y1 = self.rect.y1;
        for (i, label) in labels.iter().enumerate() {
            let (w, _h) = tr.measure(label, size, None);
            let w = f64::from(w);
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
fn measure_total_width(labels: &[String], theme: &Theme, tr: &mut TextRenderer) -> f64 {
    let size = renderer_type_size(tr, theme.tokens.typography.scale.sm);
    let sep_pad = f64::from(theme.tokens.spacing.s2);
    let (sep_w, _) = tr.measure(SEPARATOR_GLYPH, size, None);
    let sep_block = f64::from(sep_w) + sep_pad * 2.0;
    let mut total = 0.0_f64;
    for (i, label) in labels.iter().enumerate() {
        let (w, _) = tr.measure(label, size, None);
        total += f64::from(w);
        if i.saturating_add(1) < labels.len() {
            total += sep_block;
        }
    }
    total
}

/// `kurbo::Affine` composition is plain f64 matrix math with no panic
/// path; `arithmetic_side_effects` flags the `*` operator overload anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine composition is f64 matrix math; it never panics \
              or overflows, unlike integer +/-/*"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

impl Widget for Breadcrumb {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Width isn't known until taffy runs, so leave it auto; height is a
        // single line of `sm` text plus 1.5x line-height for click comfort.
        let line_h = theme.tokens.typography.scale.sm * 1.5;
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::auto(),
                height: Dimension::length(line_h),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
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

        let size = renderer_type_size(text_renderer, theme.tokens.typography.scale.sm);
        let sep_pad = f64::from(theme.tokens.spacing.s2);
        // One measured line height serves every segment and chevron: all are
        // single lines at one size, so cosmic-text gives each the same line
        // box. Centre that, not the font size — the latter sat the whole
        // trail 1.5 px below the row's centre.
        let (_, line_h) = text_renderer.measure(SEPARATOR_GLYPH, size, None);
        let text_y = centered_text_top(self.rect.y0, self.rect.height(), f64::from(line_h));

        let active_origin = self.segments.len().saturating_sub(1);
        let text_primary = theme.tokens.colors.text_primary;
        let text_dim = theme.tokens.colors.text_dim;
        let accent = theme.tokens.colors.intent.inverted;

        let labels = self.cached_visible.clone();
        let n = labels.len();
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
            // Ellipsis (orig == None) is treated as dim/idle.
            let color = match orig {
                Some(idx) if idx == active_origin => text_primary,
                Some(_) if Some(i) == self.hover_idx => accent,
                _ => text_dim,
            };
            text_renderer.draw_text(
                scene,
                label,
                size,
                color,
                translated(transform, r.x0, text_y),
                None,
            );
            // Always dim — the chevron is chrome, not content.
            if i.saturating_add(1) < n {
                let sep_x = r.x1 + sep_pad;
                text_renderer.draw_text(
                    scene,
                    SEPARATOR_GLYPH,
                    size,
                    text_dim,
                    translated(transform, sep_x, text_y),
                    None,
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let mut new_hover: Option<usize> = None;
                let active_origin = self.segments.len().saturating_sub(1);
                for (i, r) in self.segment_rects.iter().enumerate() {
                    if !r.contains(logical) {
                        continue;
                    }
                    // Only clickable segments latch hover — the active
                    // segment and the ellipsis are inert.
                    match self.visible_origin.get(i).copied().flatten() {
                        Some(idx) if idx != active_origin => {
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
                .with_name("Breadcrumb")
                .with_optional_value(value)
                .with_state(AccessState::default()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use std::cell::RefCell;
    use std::rc::Rc;
    use winit::event::{DeviceId, ElementState, Modifiers, MouseButton};

    fn synth_device() -> DeviceId {
        // SAFETY: `DeviceId` is opaque; tests construct one via
        // `mem::zeroed`, mirroring `permission_prompt.rs`'s helper.
        unsafe { std::mem::zeroed() }
    }

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    /// Drive an event through the breadcrumb with default scaffolding.
    /// Mirrors `permission_prompt.rs::tests::dispatch`.
    fn dispatch(bc: &mut Breadcrumb, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        bc.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
    }

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
                3,
                "home, chevron, docs"
            );
            let size = theme.tokens.typography.scale.sm;
            for (index, text) in ["home", SEPARATOR_GLYPH, "docs"].into_iter().enumerate() {
                crate::widget::text::assert_glyph_run_centred_in_band(
                    &scene,
                    index,
                    text,
                    size,
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
            dispatch(&mut bc, &left_press());

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
            assert_eq!(node.name.as_deref(), Some("Breadcrumb"));
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
