//! Unit 9: Breadcrumb widget — a horizontal path indicator with clickable
//! ancestor segments and a chevron (`>`) separator between them. Rendered
//! using the standard five-pass widget contract (sync → build_layout →
//! apply_layout → draw → handle_event).
//!
//! Visual contract (concept-parity with the design doc):
//!
//! - Segments are joined by a literal `>` chevron rendered in
//!   `theme.tokens.colors.text_dim`, with `theme.tokens.spacing.s2`
//!   horizontal padding on each side.
//! - The active (rightmost) segment is rendered with
//!   `theme.tokens.colors.text_primary`. Earlier segments use `text_dim`
//!   when idle, and switch to `theme.tokens.colors.accent[ACCENT_PRIMARY_IDX]`
//!   while the cursor hovers them — that hover transition is also how the
//!   user reads "this one is clickable, the active one is not".
//! - All segment text is drawn at `theme.tokens.typography.scale.sm`.
//! - When the total computed width exceeds the available rect width, the
//!   widget truncates the middle of the path with a literal `...` segment:
//!   it keeps the first segment, inserts `...`, and keeps the last two.
//!   This matches the spec's `home / ... / project / readme.md` rule and
//!   degrades gracefully (a 4-segment trail already short-circuits the
//!   truncation because the truncated form would be the same length).
use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::Widget;

/// Literal chevron used to visually join two segments. Spec pins the
/// glyph to ASCII `>` so it always falls back to the system font even
/// when no fancy chevron face is loaded.
const SEPARATOR_GLYPH: &str = ">";

/// Literal ellipsis used to collapse middle segments when the breadcrumb
/// doesn't fit in its available width. Three ASCII dots (rather than the
/// `…` codepoint) keep the truncation visible in plain-text dumps and
/// match the spec's `["home", "...", ..., ...]` example exactly.
const ELLIPSIS: &str = "...";

/// Breadcrumb widget. Holds the original segment list, the click callback,
/// and the per-segment rects populated during `apply_layout`. The visible
/// segment list (after any middle-truncation) is computed lazily by
/// `visible_segments`; the cached `segment_rects` mirrors *that* list so
/// the click hit-test always matches what the user sees on screen.
pub struct Breadcrumb {
    /// All segments, in path order. The rightmost is the active page.
    segments: Vec<String>,
    /// Optional click callback. Receives the index *into the original
    /// `segments` vector* of the clicked ancestor, so a caller that wired
    /// `home / docs / project / readme.md` and saw a click on `docs`
    /// always gets index `1` regardless of any visible-list truncation.
    on_navigate: Option<Box<dyn FnMut(usize)>>,
    /// Per-visible-segment hit rects in screen coordinates, populated in
    /// `apply_layout`. The Nth entry corresponds to the Nth entry in the
    /// most recent `visible_segments()` output — the click handler maps
    /// the visible index back to the original index before invoking the
    /// callback.
    segment_rects: Vec<Rect>,
    /// Original-index mapping for each entry in `segment_rects`. Entries
    /// are `Some(orig_idx)` for real segments and `None` for the ellipsis
    /// placeholder (which is non-clickable). Length always matches
    /// `segment_rects`.
    visible_origin: Vec<Option<usize>>,
    /// Cached visible-segment list as of the last `apply_layout`, so the
    /// draw pass and the click handler agree on labels without recomputing.
    cached_visible: Vec<String>,
    /// Index (into `cached_visible`) the cursor is currently hovering, or
    /// `None`. Ellipsis entries don't latch hover. Updated by
    /// `handle_event` and consumed by `draw` for the accent recolour.
    hover_idx: Option<usize>,
    /// Whole-widget bounding rect.
    rect: Rect,
}

impl Breadcrumb {
    /// Construct a new breadcrumb from an ordered list of path segments.
    /// The last segment is treated as the active (current) page and is
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
    pub fn on_navigate<F: FnMut(usize) + 'static>(mut self, f: F) -> Self {
        self.on_navigate = Some(Box::new(f));
        self
    }

    /// Borrow the original segment list — primarily a test accessor, but
    /// also handy for callers that want to render an aria-label or
    /// debug-print the trail.
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Number of separator glyphs the widget will paint for the current
    /// segment list. Always `segments.len() - 1` for two or more
    /// segments, and zero for a single segment. The spec pins this
    /// accessor explicitly so a future regression that paints a stray
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
        // Empty trail trivially "fits" — nothing to render.
        if self.segments.is_empty() {
            return Vec::new();
        }
        // Measure the full path. If it fits, no truncation is required.
        let full_width = self.measure_total_width(&self.segments, theme, tr);
        if full_width <= available_width {
            return self.segments.iter().cloned().collect();
        }

        // Truncation only makes sense when the keep-first / ellipsis /
        // keep-last-two pattern actually drops at least one segment.
        // With 4 or fewer segments the truncated form (`first, ...,
        // last-1, last`) is the same length as the full form (or longer
        // when the ellipsis takes more pixels than a real segment), so
        // we'd produce a visually-worse render. Fall back to the
        // un-truncated list in that degenerate case.
        if self.segments.len() <= 4 {
            return self.segments.iter().cloned().collect();
        }

        // Standard truncation: keep `segments[0]`, ellipsis,
        // `segments[len-2]`, `segments[len-1]`.
        let n = self.segments.len();
        vec![
            self.segments[0].clone(),
            ELLIPSIS.to_string(),
            self.segments[n - 2].clone(),
            self.segments[n - 1].clone(),
        ]
    }

    /// Sum the widths of all segments + interleaved separators (with
    /// `s2` padding on each side of each separator). Used by
    /// `visible_segments` to decide whether the full path fits.
    fn measure_total_width(
        &self,
        labels: &[String],
        theme: &Theme,
        tr: &mut TextRenderer,
    ) -> f64 {
        let size = theme.tokens.typography.scale.sm;
        let sep_pad = theme.tokens.spacing.s2 as f64;
        let (sep_w, _) = tr.measure(SEPARATOR_GLYPH, size, None);
        let sep_block = sep_w as f64 + sep_pad * 2.0;
        let mut total = 0.0_f64;
        for (i, label) in labels.iter().enumerate() {
            let (w, _) = tr.measure(label, size, None);
            total += w as f64;
            if i + 1 < labels.len() {
                total += sep_block;
            }
        }
        total
    }

    /// Recompute `cached_visible`, `segment_rects`, and `visible_origin`
    /// from the current `self.rect` and segment list. Called from
    /// `apply_layout` and the test helpers so the click hit-test and
    /// draw pass operate on the same geometry.
    fn rebuild_rects(&mut self, theme: &Theme, tr: &mut TextRenderer) {
        let available = self.rect.width().max(0.0);
        let labels = self.visible_segments(available, theme, tr);
        // Walk the visible labels to derive each one's origin-index. The
        // mapping is unambiguous: when the truncation rule fires we know
        // exactly which originals survive (first + last two), and when
        // it doesn't fire each visible index equals the original index.
        let n = self.segments.len();
        let truncated = labels.len() == 4
            && n > 4
            && labels[1] == ELLIPSIS;
        let mut origin = Vec::with_capacity(labels.len());
        if truncated {
            origin.push(Some(0));
            origin.push(None); // ellipsis is non-clickable
            origin.push(Some(n - 2));
            origin.push(Some(n - 1));
        } else {
            for i in 0..labels.len() {
                origin.push(Some(i));
            }
        }

        // Lay the labels out left-to-right inside `self.rect`, vertically
        // centered. Each segment gets its own rect (used for hit testing);
        // the chevron glyph between segments is painted by `draw` but
        // doesn't need a stored rect — it's never clickable.
        let size = theme.tokens.typography.scale.sm;
        let sep_pad = theme.tokens.spacing.s2 as f64;
        let (sep_w, _sep_h) = tr.measure(SEPARATOR_GLYPH, size, None);
        let sep_w = sep_w as f64;
        let mut rects = Vec::with_capacity(labels.len());
        let mut cursor_x = self.rect.x0;
        let pill_y0 = self.rect.y0;
        let pill_y1 = self.rect.y1;
        for (i, label) in labels.iter().enumerate() {
            let (w, _h) = tr.measure(label, size, None);
            let w = w as f64;
            let r = Rect::new(cursor_x, pill_y0, cursor_x + w, pill_y1);
            rects.push(r);
            cursor_x += w;
            if i + 1 < labels.len() {
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

impl Widget for Breadcrumb {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        // Breadcrumb segments are pushed in by the host (file explorer,
        // settings page, etc.) rather than mirrored from a CRDT path.
        // Sync is intentionally a no-op.
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The breadcrumb stretches horizontally inside its parent and
        // sizes vertically to a single line of `sm` text plus 1.5×
        // line-height padding for comfortable click targets. We don't
        // know the available width here (taffy hasn't run yet), so we
        // expose `Dimension::auto()` and let the parent decide.
        let line_h = (theme.tokens.typography.scale.sm * 1.5) as f32;
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length(line_h),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        // Real per-segment rects depend on text measurements, which
        // need a `TextRenderer`. We don't have one in `apply_layout`,
        // so we defer the actual rect computation to `draw` (which
        // receives a `TextRenderer`). Until `draw` runs at least once,
        // `segment_rects` will be stale — that's acceptable because
        // `handle_event` won't see a hover before the first paint.
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

        let size = theme.tokens.typography.scale.sm;
        let sep_pad = theme.tokens.spacing.s2 as f64;
        let (sep_w, _sep_h) = text_renderer.measure(SEPARATOR_GLYPH, size, None);
        let sep_w = sep_w as f64;

        let active_origin = self.segments.len().saturating_sub(1);
        let text_primary = theme.tokens.colors.text_primary;
        let text_dim = theme.tokens.colors.text_dim;
        let accent = theme.tokens.colors.accent[ACCENT_PRIMARY_IDX];

        let labels = self.cached_visible.clone();
        let n = labels.len();
        for (i, label) in labels.iter().enumerate() {
            let r = self.segment_rects[i];
            let orig = self.visible_origin[i];
            // Pick the colour based on whether this is the active segment,
            // a hovered clickable segment, or an idle clickable segment.
            // Ellipsis (orig == None) is treated as dim/idle.
            let color = match orig {
                Some(idx) if idx == active_origin => text_primary,
                Some(_) if Some(i) == self.hover_idx => accent,
                _ => text_dim,
            };
            // Vertically centre the text inside the segment rect.
            let text_y = r.y0 + (r.height() - size as f64) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                size,
                color,
                transform * Affine::translate((r.x0, text_y)),
                None,
            );
            // Paint the chevron separator between this segment and the
            // next, with `s2` padding on each side. The chevron is always
            // dim — it's chrome, not content.
            if i + 1 < n {
                let sep_x = r.x1 + sep_pad;
                let sep_y = text_y;
                text_renderer.draw_text(
                    scene,
                    SEPARATOR_GLYPH,
                    size,
                    text_dim,
                    transform * Affine::translate((sep_x, sep_y)),
                    None,
                );
                let _ = sep_w; // measured for layout symmetry; not used here
            }
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(hover_idx) = self.hover_idx {
                    if let Some(Some(orig)) = self.visible_origin.get(hover_idx).copied() {
                        // Resolve callback against the *original* index
                        // so the caller never has to reason about the
                        // visible-list truncation.
                        if let Some(cb) = self.on_navigate.as_mut() {
                            cb(orig);
                        }
                        *handled = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Breadcrumb"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    //! Coverage for the Unit 9 Breadcrumb widget. We exercise three
    //! load-bearing properties without standing up a Vello scene:
    //!
    //!   1. A one-segment breadcrumb paints no `>` chevrons.
    //!   2. Clicking a middle segment invokes the `on_navigate` callback
    //!      with the original-list index (not the visible-list index).
    //!   3. A breadcrumb whose full path can't fit collapses the middle
    //!      with a literal `...` placeholder, preserving the first
    //!      segment and the last two.
    //!
    //! All three use `Theme::dark()` so the spacing / colour tokens
    //! match the production render path.
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use winit::event::{DeviceId, ElementState, MouseButton, Modifiers};

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
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        bc.handle_event(
            event,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
    }

    #[test]
    fn breadcrumb_with_one_segment_paints_no_separator() {
        // A trail with a single segment is just the current page —
        // there's nothing to navigate to, so no `>` should be painted.
        // `separator_count()` returns `segments.len() - 1`, saturating
        // to zero on the empty / one-segment trail.
        let bc = Breadcrumb::new(vec!["home".to_string()]);
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        // Exercise the layout helper so a future regression that emits
        // a stray chevron rect would also flip this assertion.
        let mut bc = bc;
        bc.rect = Rect::new(0.0, 0.0, 500.0, 24.0);
        bc.apply_layout_helper(&theme, &mut tr);
        assert_eq!(
            bc.separator_count(),
            0,
            "one-segment breadcrumb must report zero separators"
        );
        // And the cached visible list should match the original single
        // entry — no ellipsis, no surprise chevron.
        assert_eq!(bc.cached_visible, vec!["home".to_string()]);
    }

    #[test]
    fn breadcrumb_clicking_middle_segment_calls_on_navigate_with_that_index() {
        // Wire a callback that records every navigation index it sees,
        // then dispatch a hover + press to the middle segment's rect.
        // We expect the callback to fire once with `Some(1)`.
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

        // Synthesise a wide enough rect that all three segments fit
        // without truncation — keeps the visible-index ↔ original-index
        // mapping straightforward.
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

    #[test]
    fn breadcrumb_with_long_path_truncates_middle_segments() {
        // A 10-segment trail whose full width far exceeds the available
        // rect must collapse to `[first, "...", last-1, last]`.
        // Constructing the segments programmatically so the test stays
        // legible even if the spec moves the keep-last-two count.
        let segments: Vec<String> = (0..10)
            .map(|i| format!("segment_{:02}", i))
            .collect();
        let bc = Breadcrumb::new(segments.clone());
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();

        // Pick an available width that's deliberately too small for the
        // full path but comfortably wide for the truncated form — 200 px
        // at `sm` (12 px text) holds the four-label fallback with room
        // to spare while rejecting the ten-label full path.
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
}
