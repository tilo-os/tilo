//! Three-way merge view.
//!
//! Top band: three columns — base / ours / theirs — rendered hunk-by-hunk so a
//! conflict shows the same vertical span across all three. Conflicts get
//! per-hunk action chips (take ours / take theirs / take both). Bottom band:
//! the merged result, mirrored from the Automerge `Text` so it is the
//! collaboratively-editable authoritative output. Clicking an action chip
//! re-composes the merged buffer and writes it into the CRDT.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{FontChoice, TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Scrollable, Widget};

use crate::actions::{self, DiffmergeAction, HunkChoice, MergeResourceCtx};
use crate::doc::MergeDoc;
use crate::highlight;
use crate::model::{MergeModel, Resolution};

const ROW_H: f64 = 19.0;
const HEADER_H: f64 = 30.0;
const ACTION_H: f64 = 26.0;
const FONT_SIZE: f32 = 12.5;
const PAD_X: f64 = 8.0;
const GAP: f64 = 6.0;

/// A clickable action chip rect tagged with the hunk + side it takes.
#[derive(Clone, Copy)]
struct ActionHit {
    rect: Rect,
    hunk: usize,
    choice: HunkChoice,
}

/// The three-way merge panel.
pub struct MergeView {
    model: MergeModel,
    base_label: String,
    ours_label: String,
    theirs_label: String,
    rect: Rect,
    /// Shared overlay scrollbar for the base/ours/theirs source band (the three
    /// columns scroll together — one offset, hunk-aligned across all three).
    source_scroll: Scrollable,
    /// Shared overlay scrollbar for the merged-result band below.
    merged_scroll: Scrollable,
    /// Cached y-extent (in content space) of each hunk row in the source band.
    hunk_spans: Vec<(f64, f64)>,
    action_hits: Vec<ActionHit>,
    active_hunk: Option<usize>,
    /// Cursor position cache for click hit-testing.
    cursor: Point,
    dirty: bool,
    /// Set when a fresh set of inputs was loaded: the next `sync` seeds the
    /// merged buffer (a projection of the external inputs, not a replayable
    /// action) and clears any stale per-hunk resolutions.
    pending_seed: bool,
    /// A conflict resolution the user just triggered via a chip, waiting to be
    /// drained by the parent app and dispatched as a [`DiffmergeAction`] through
    /// the shared action path (so a chip and Ctrl+K produce one undo boundary).
    pending_action: Option<DiffmergeAction>,
}

impl MergeView {
    pub fn new() -> Self {
        Self {
            model: MergeModel::default(),
            base_label: String::new(),
            ours_label: String::new(),
            theirs_label: String::new(),
            rect: Rect::ZERO,
            // Persistent (semi-transparent) overlay bars — see DiffView::new.
            source_scroll: Scrollable::new_vertical().always_visible(),
            merged_scroll: Scrollable::new_vertical().always_visible(),
            hunk_spans: Vec::new(),
            action_hits: Vec::new(),
            active_hunk: None,
            cursor: Point::ZERO,
            dirty: true,
            pending_seed: true,
            pending_action: None,
        }
    }

    pub fn set_inputs(
        &mut self,
        base_label: &str,
        base: &str,
        ours_label: &str,
        ours: &str,
        theirs_label: &str,
        theirs: &str,
    ) {
        self.model = MergeModel::compute(base, ours, theirs);
        self.base_label = base_label.to_string();
        self.ours_label = ours_label.to_string();
        self.theirs_label = theirs_label.to_string();
        self.source_scroll.set_offset_y(0.0);
        self.merged_scroll.set_offset_y(0.0);
        self.active_hunk = self.first_conflict().or_else(|| self.model.change_indices().first().copied());
        self.dirty = true;
        self.pending_seed = true;
    }

    pub fn model(&self) -> &MergeModel {
        &self.model
    }

    /// The resolution-chip hit rects captured by the last `draw`, each with the
    /// hunk and choice it dispatches. Lets a headless test click a chip through
    /// the real pointer path instead of calling `resolve` directly.
    #[cfg(test)]
    pub(crate) fn chip_hits(&self) -> Vec<(Rect, usize, HunkChoice)> {
        self.action_hits
            .iter()
            .map(|a| (a.rect, a.hunk, a.choice))
            .collect()
    }

    /// Presentation context for the Ctrl+K resource tree: the merge title plus a
    /// snapshot of the model the per-hunk specs compose from. `None` when there
    /// is nothing to resolve.
    pub(crate) fn resource_ctx(&self) -> Option<MergeResourceCtx> {
        if self.model.change_count() == 0 {
            return None;
        }
        let title = if self.ours_label.is_empty() && self.theirs_label.is_empty() {
            "Merge result".to_string()
        } else {
            format!("Merge · {} ↔ {}", self.ours_label, self.theirs_label)
        };
        Some(MergeResourceCtx {
            title,
            model: self.model.clone(),
        })
    }

    /// Take the pending chip-triggered resolution, if any, so the parent app can
    /// dispatch it through the shared action path. Rising-edge: cleared on read.
    pub(crate) fn take_pending_action(&mut self) -> Option<DiffmergeAction> {
        self.pending_action.take()
    }

    fn first_conflict(&self) -> Option<usize> {
        self.model
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
    }

    /// Resolve `hunk` by taking `choice`. Updates the in-memory model for
    /// immediate feedback and queues the matching [`DiffmergeAction`] (with the
    /// recomposed merged output baked in) for the parent app to dispatch through
    /// the shared action path — so a chip click and the Ctrl+K row produce the
    /// same replayable action under one undo boundary. The document write itself
    /// happens via that action, not here.
    pub fn resolve(&mut self, hunk: usize, choice: HunkChoice) {
        if hunk >= self.model.hunks.len() {
            return;
        }
        // Compose the resulting merged output BEFORE mutating so the queued
        // action is composed from the same model state the palette spec is —
        // guaranteeing a byte-identical envelope from either entry point.
        let merged = actions::compose_choice(&self.model, hunk, choice);
        if let Some(h) = self.model.hunks.get_mut(hunk) {
            h.resolution = choice.to_resolution();
        }
        self.active_hunk = Some(hunk);
        self.pending_action = Some(actions::resolve_hunk_action(hunk, choice, merged));
        self.dirty = true;
    }

    /// Navigate the active hunk among the real-change hunks.
    pub fn navigate(&mut self, forward: bool) -> bool {
        let changes = self.model.change_indices();
        if changes.is_empty() {
            return false;
        }
        let pos = self
            .active_hunk
            .and_then(|a| changes.iter().position(|&c| c == a));
        let next_pos = match pos {
            None => 0,
            Some(p) => {
                if forward {
                    (p + 1).min(changes.len() - 1)
                } else {
                    p.saturating_sub(1)
                }
            }
        };
        let hunk = changes[next_pos];
        self.active_hunk = Some(hunk);
        self.scroll_hunk_into_view(hunk);
        self.dirty = true;
        true
    }

    fn scroll_hunk_into_view(&mut self, hunk: usize) {
        if let Some((top, bot)) = self.hunk_spans.get(hunk).copied() {
            let h = (bot - top).max(1.0) as f32;
            self.source_scroll.scroll_into_view(0.0, top as f32, 1.0, h);
            self.dirty = true;
        }
    }

    fn source_band_height(&self) -> f64 {
        // The source band occupies the top ~58% of the panel.
        ((self.rect.height() - HEADER_H) * 0.58).max(0.0)
    }

    fn merged_band_rect(&self) -> Rect {
        let r = self.rect;
        let band_top = r.y0 + HEADER_H + self.source_band_height();
        Rect::new(r.x0, band_top, r.x1, r.y1)
    }

    fn content_height(&self) -> f64 {
        self.hunk_spans.last().map(|(_, b)| *b).unwrap_or(0.0)
    }

    /// Recompute the per-hunk vertical spans (content space).
    fn recompute_spans(&mut self) {
        self.hunk_spans.clear();
        let mut y = 0.0;
        for h in &self.model.hunks {
            let rows = h.base.len().max(h.ours.len()).max(h.theirs.len()).max(1);
            let mut height = rows as f64 * ROW_H;
            if h.is_change {
                height += ACTION_H + GAP;
            }
            self.hunk_spans.push((y, y + height));
            y += height;
        }
    }
}

impl Widget for MergeView {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if self.pending_seed {
            // Fresh inputs: seed the provisional merged output (a projection of
            // the external inputs, not a replayable action) and drop any stale
            // per-hunk resolutions whose indices no longer correspond.
            MergeDoc::clear_resolutions(state);
            MergeDoc::set_merged(state, &self.model.merged_text());
            self.pending_seed = false;
            return;
        }
        // Seed once for a late joiner that has no buffer yet.
        if !MergeDoc::has_merged(state) {
            MergeDoc::set_merged(state, &self.model.merged_text());
        }
        // Reconcile chip state from resolutions recorded by (possibly remote)
        // actions, so a reload or a peer shows the same take-a-side selections.
        // The merged `Text` itself stays authoritative — the actions already
        // wrote it — so this only tints the in-memory hunks.
        for (hunk, choice) in MergeDoc::resolutions(state) {
            if let (Some(h), Some(c)) = (self.model.hunks.get_mut(hunk), HunkChoice::from_str(&choice)) {
                if h.is_change {
                    let res = c.to_resolution();
                    if h.resolution != res {
                        h.resolution = res;
                        self.dirty = true;
                    }
                }
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        self.recompute_spans();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.action_hits.clear();
        let c = &theme.tokens.colors;
        let r = self.rect;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &r);

        let col_w = (r.width()) / 3.0;
        let base_col = Rect::new(r.x0, r.y0, r.x0 + col_w, r.y0 + HEADER_H);
        let ours_col = Rect::new(r.x0 + col_w, r.y0, r.x0 + 2.0 * col_w, r.y0 + HEADER_H);
        let theirs_col = Rect::new(r.x0 + 2.0 * col_w, r.y0, r.x1, r.y0 + HEADER_H);

        // Column headers (base / ours / theirs).
        draw_header(scene, transform, tr, theme, base_col, &self.base_label, c.text_dim);
        draw_header(scene, transform, tr, theme, ours_col, &self.ours_label, c.semantic.success);
        draw_header(scene, transform, tr, theme, theirs_col, &self.theirs_label, c.semantic.info);

        // Vertical separators across the source band.
        let band_top = r.y0 + HEADER_H;
        let band_h = self.source_band_height();
        let band_bot = band_top + band_h;
        for k in 1..3 {
            let x = r.x0 + k as f64 * col_w;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                c.border_standard,
                None,
                &Line::new(Point::new(x, band_top), Point::new(x, band_bot)),
            );
        }

        // --- Source band, clipped + scrolled (shared overlay scrollbar) ---
        let band_clip = Rect::new(r.x0, band_top, r.x1, band_bot);
        self.source_scroll.set_viewport_rect(band_clip);
        self.source_scroll
            .set_content_size(r.width() as f32, self.content_height() as f32);
        let source_off = self.source_scroll.offset_y() as f64;
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, transform, &band_clip);

        for (hi, h) in self.model.hunks.iter().enumerate() {
            let (top, _bot) = self.hunk_spans[hi];
            let y = band_top + top - source_off;
            let rows = h.base.len().max(h.ours.len()).max(h.theirs.len()).max(1);
            let block_h = rows as f64 * ROW_H;

            // Cull off-screen hunks cheaply.
            if y + block_h + ACTION_H < band_top || y > band_bot {
                continue;
            }

            // Hunk tint per side.
            let (ours_tint, theirs_tint) = match h.resolution {
                Resolution::Unchanged => (None, None),
                Resolution::Conflict => (
                    Some(with_alpha(c.semantic.danger, 34)),
                    Some(with_alpha(c.semantic.danger, 34)),
                ),
                Resolution::Ours | Resolution::TookOurs => {
                    (Some(with_alpha(c.semantic.success, 40)), None)
                }
                Resolution::Theirs | Resolution::TookTheirs => {
                    (None, Some(with_alpha(c.semantic.info, 40)))
                }
                Resolution::TookBoth => (
                    Some(with_alpha(c.semantic.success, 36)),
                    Some(with_alpha(c.semantic.info, 36)),
                ),
            };

            if let Some(t) = ours_tint {
                let rect = Rect::new(r.x0 + col_w, y, r.x0 + 2.0 * col_w, y + block_h);
                scene.fill(Fill::NonZero, transform, t, None, &rect);
            }
            if let Some(t) = theirs_tint {
                let rect = Rect::new(r.x0 + 2.0 * col_w, y, r.x1, y + block_h);
                scene.fill(Fill::NonZero, transform, t, None, &rect);
            }

            // Active hunk highlight ring.
            if Some(hi) == self.active_hunk && h.is_change {
                let ring = RoundedRect::new(r.x0 + 1.0, y - 1.0, r.x1 - 1.0, y + block_h + 1.0, 3.0);
                scene.stroke(&Stroke::new(1.5), transform, c.accent[5], None, &ring);
            }

            // Lines for each column.
            draw_lines(scene, transform, tr, theme, r.x0 + PAD_X, y, &h.base, col_w - 2.0 * PAD_X);
            draw_lines(scene, transform, tr, theme, r.x0 + col_w + PAD_X, y, &h.ours, col_w - 2.0 * PAD_X);
            draw_lines(scene, transform, tr, theme, r.x0 + 2.0 * col_w + PAD_X, y, &h.theirs, col_w - 2.0 * PAD_X);

            // Action chips for change hunks.
            if h.is_change {
                let ay = y + block_h + 2.0;
                // Chip hues track the theme so a palette / high-contrast swap
                // keeps the resolution controls coherent: ours = success (green),
                // theirs = info (blue), both = the neutral dim tone — mirroring
                // the status label's own `ColorClass` mapping.
                let chips: &[(&str, HunkChoice, Color)] = if h.resolution.is_conflict() {
                    &[
                        ("Take ours", HunkChoice::Ours, c.semantic.success),
                        ("Take theirs", HunkChoice::Theirs, c.semantic.info),
                        ("Take both", HunkChoice::Both, c.text_dim),
                    ]
                } else {
                    // Auto-resolved — still allow flipping.
                    &[
                        ("Ours", HunkChoice::Ours, c.semantic.success),
                        ("Theirs", HunkChoice::Theirs, c.semantic.info),
                        ("Both", HunkChoice::Both, c.text_dim),
                    ]
                };
                // Status label on the left.
                let status = resolution_label(h.resolution);
                tr.draw_text(
                    scene,
                    status.0,
                    11.5,
                    status.1.color(c),
                    transform * Affine::translate((r.x0 + col_w + PAD_X, ay + 5.0)),
                    None,
                );

                let mut cx = r.x0 + 2.0 * col_w + PAD_X;
                for (label, choice, col) in chips {
                    let w = label.len() as f64 * 7.2 + 16.0;
                    let chip = Rect::new(cx, ay, cx + w, ay + ACTION_H - 6.0);
                    let selected = chip_is_selected(h.resolution, *choice);
                    let bg = if selected {
                        with_alpha(*col, 220)
                    } else {
                        with_alpha(*col, 50)
                    };
                    let rr = RoundedRect::from_rect(chip, 5.0);
                    scene.fill(Fill::NonZero, transform, bg, None, &rr);
                    scene.stroke(&Stroke::new(1.0), transform, with_alpha(*col, 160), None, &rr);
                    let text_col = if selected { Color::from_rgb8(10, 14, 22) } else { *col };
                    tr.draw_text(
                        scene,
                        label,
                        11.5,
                        text_col,
                        transform * Affine::translate((cx + 8.0, ay + 4.0)),
                        None,
                    );
                    self.action_hits.push(ActionHit {
                        rect: chip,
                        hunk: hi,
                        choice: *choice,
                    });
                    cx += w + 6.0;
                }
            }
        }

        scene.pop_layer();
        // The three source columns share one draggable overlay scrollbar.
        self.source_scroll.draw_bar_overlay(scene, transform, theme);

        // Divider between source band and merged band.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_focus,
            None,
            &Line::new(Point::new(r.x0, band_bot), Point::new(r.x1, band_bot)),
        );

        // --- Merged result band ---
        let merged = self.merged_band_rect();
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &merged);
        // Merged header.
        let mh = Rect::new(merged.x0, merged.y0, merged.x1, merged.y0 + HEADER_H);
        scene.fill(Fill::NonZero, transform, with_alpha(c.accent[5], 40), None, &mh);
        let conflicts = self.model.conflict_count();
        let header_label = if conflicts == 0 {
            "Merged result · all conflicts resolved".to_string()
        } else {
            format!("Merged result · {conflicts} conflict(s) remaining")
        };
        let hc = if conflicts == 0 { c.semantic.success } else { c.semantic.danger };
        tr.draw_text(
            scene,
            &header_label,
            13.0,
            hc,
            transform * Affine::translate((merged.x0 + PAD_X + 4.0, merged.y0 + 7.0)),
            None,
        );

        // Merged buffer body (its own draggable overlay scrollbar).
        let merged_body = Rect::new(merged.x0, merged.y0 + HEADER_H, merged.x1, merged.y1);
        let lines: Vec<String> = self.model.merged_lines();
        let merged_content_h = lines.len() as f64 * ROW_H + 8.0;
        self.merged_scroll.set_viewport_rect(merged_body);
        self.merged_scroll
            .set_content_size(merged.width() as f32, merged_content_h as f32);
        let moff = self.merged_scroll.offset_y() as f64;
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, transform, &merged_body);
        for (i, line) in lines.iter().enumerate() {
            let ly = merged_body.y0 + 4.0 + i as f64 * ROW_H - moff;
            if ly + ROW_H < merged_body.y0 || ly > merged_body.y1 {
                continue;
            }
            // Line number gutter (monospace, so the numbers stay column-aligned).
            let no = (i + 1).to_string();
            tr.draw_text_font(
                scene,
                &no,
                11.0,
                c.text_dim,
                transform * Affine::translate((merged_body.x0 + 6.0, ly + 2.0)),
                None,
                TextWeight::Regular,
                false,
                &FontChoice::Monospace,
            );
            draw_code_segments(scene, transform, tr, theme, merged_body.x0 + 44.0, ly, line);
        }
        scene.pop_layer();
        self.merged_scroll.draw_bar_overlay(scene, transform, theme);

        self.dirty = false;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        m: &winit::event::Modifiers,
        s: &mut AutoCommit,
        p: &ObjId,
        handled: &mut bool,
        _focus: &mut Option<NodeId>,
        _node: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        let band_bot = self.rect.y0 + HEADER_H + self.source_band_height();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Continue a thumb drag in whichever band captured the pointer.
                if self.source_scroll.wants_pointer_capture() {
                    feed_scroll(&mut self.source_scroll, event, scale_factor, m, s, p);
                    self.dirty = true;
                    *handled = true;
                } else if self.merged_scroll.wants_pointer_capture() {
                    feed_scroll(&mut self.merged_scroll, event, scale_factor, m, s, p);
                    self.dirty = true;
                    *handled = true;
                } else {
                    // Let the band under the pointer reveal its overlay bar near
                    // the edge (never consumes).
                    let scroll = if self.cursor.y <= band_bot {
                        &mut self.source_scroll
                    } else {
                        &mut self.merged_scroll
                    };
                    feed_scroll(scroll, event, scale_factor, m, s, p);
                }
            }
            WindowEvent::MouseWheel { .. } => {
                // The band under the pointer scrolls; the three source columns
                // share one offset, so they move together.
                let scroll = if self.cursor.y <= band_bot {
                    &mut self.source_scroll
                } else {
                    &mut self.merged_scroll
                };
                if feed_scroll(scroll, event, scale_factor, m, s, p) {
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // A resolution chip wins the press.
                let hit = self
                    .action_hits
                    .iter()
                    .find(|a| a.rect.contains(self.cursor))
                    .copied();
                if let Some(a) = hit {
                    self.resolve(a.hunk, a.choice);
                    *handled = true;
                    return;
                }
                // Otherwise offer it to the band's scrollbar (thumb / track).
                let scroll = if self.cursor.y <= band_bot {
                    &mut self.source_scroll
                } else {
                    &mut self.merged_scroll
                };
                if feed_scroll(scroll, event, scale_factor, m, s, p) {
                    self.dirty = true;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.source_scroll.wants_pointer_capture() {
                    feed_scroll(&mut self.source_scroll, event, scale_factor, m, s, p);
                    *handled = true;
                } else if self.merged_scroll.wants_pointer_capture() {
                    feed_scroll(&mut self.merged_scroll, event, scale_factor, m, s, p);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "MergeView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.source_scroll.needs_redraw() || self.merged_scroll.needs_redraw()
    }

    fn needs_animation_frame(&self) -> bool {
        self.source_scroll.needs_animation_frame() || self.merged_scroll.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.source_scroll.mark_drawn();
        self.merged_scroll.mark_drawn();
    }
}

/// Forward one raw event into a child-less [`Scrollable`] (thumb-drag / track /
/// wheel) using a throwaway taffy node — the bar lays out and draws no child.
fn feed_scroll(
    scroll: &mut Scrollable,
    event: &WindowEvent,
    scale_factor: f64,
    modifiers: &winit::event::Modifiers,
    state: &mut AutoCommit,
    path: &ObjId,
) -> bool {
    let mut handled = false;
    let mut ft = None;
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let node = taffy.new_leaf(Style::default()).unwrap();
    scroll.handle_event(
        event, scale_factor, modifiers, state, path, &mut handled, &mut ft, node, &taffy,
    );
    handled
}

fn chip_is_selected(current: Resolution, chip: HunkChoice) -> bool {
    matches!(
        (current, chip),
        (Resolution::TookOurs | Resolution::Ours, HunkChoice::Ours)
            | (Resolution::TookTheirs | Resolution::Theirs, HunkChoice::Theirs)
            | (Resolution::TookBoth, HunkChoice::Both)
    )
}

/// Status label + colour-class for a hunk's resolution.
fn resolution_label(res: Resolution) -> (&'static str, ColorClass) {
    match res {
        Resolution::Conflict => ("● conflict", ColorClass::Danger),
        Resolution::Ours => ("auto: ours", ColorClass::Success),
        Resolution::Theirs => ("auto: theirs", ColorClass::Info),
        Resolution::TookOurs => ("resolved: ours", ColorClass::Success),
        Resolution::TookTheirs => ("resolved: theirs", ColorClass::Info),
        Resolution::TookBoth => ("resolved: both", ColorClass::Dim),
        Resolution::Unchanged => ("", ColorClass::Dim),
    }
}

enum ColorClass {
    Success,
    Info,
    Danger,
    Dim,
}

impl ColorClass {
    fn color(&self, c: &ui::tokens::colors::Colors) -> Color {
        match self {
            ColorClass::Success => c.semantic.success,
            ColorClass::Info => c.semantic.info,
            ColorClass::Danger => c.semantic.danger,
            ColorClass::Dim => c.text_dim,
        }
    }
}

fn draw_header(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    col: Rect,
    label: &str,
    accent: Color,
) {
    let c = &theme.tokens.colors;
    scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &col);
    // Accent underline keyed to the column role.
    let underline = Rect::new(col.x0, col.y1 - 2.0, col.x1, col.y1);
    scene.fill(Fill::NonZero, transform, accent, None, &underline);
    if !label.is_empty() {
        tr.draw_text(
            scene,
            label,
            12.5,
            c.text_primary,
            transform * Affine::translate((col.x0 + PAD_X + 2.0, col.y0 + 7.0)),
            Some((col.width() - 2.0 * PAD_X) as f32),
        );
    }
}

/// Draw a list of code lines starting at `(x0, y)`, highlighted.
fn draw_lines(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    x0: f64,
    y0: f64,
    lines: &[String],
    _max_w: f64,
) {
    for (i, line) in lines.iter().enumerate() {
        let y = y0 + i as f64 * ROW_H;
        draw_code_segments(scene, transform, tr, theme, x0, y, line);
    }
}

fn draw_code_segments(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    x0: f64,
    y: f64,
    text: &str,
) {
    let mut x = x0;
    for (seg, color) in highlight::segments(text, theme) {
        if seg.is_empty() {
            continue;
        }
        // Render in the embedded monospace so base / ours / theirs and the
        // merged output all share one fixed-width grid and columns line up.
        tr.draw_text_font(
            scene,
            &seg,
            FONT_SIZE,
            color,
            transform * Affine::translate((x, y + 2.0)),
            None,
            TextWeight::Regular,
            false,
            &FontChoice::Monospace,
        );
        let (w, _) = tr.measure_font(
            &seg, FONT_SIZE, None, TextWeight::Regular, false, &FontChoice::Monospace,
        );
        x += w as f64;
    }
}

fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use ui::widget::ScrollAxis;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, Modifiers};

    /// Run one real frame so the source band lays out and its shared overlay
    /// scrollbar is configured from the hunk spans.
    fn run_frame(view: &mut MergeView, w: f32, h: f32) {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut state = ui::State::new();
        view.sync(&mut state.doc, &state.root);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = view.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size { width: Dimension::length(w), height: Dimension::length(h) },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let mm = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(w),
                        height: AvailableSpace::Definite(h),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = mm.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        view.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        view.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    fn moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved { device_id: DeviceId::dummy(), position: PhysicalPosition::new(x, y) }
    }
    fn mouse(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button: MouseButton::Left }
    }
    fn send(view: &mut MergeView, ev: &WindowEvent) -> bool {
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;
        let mut handled = false;
        let mut focus = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        view.handle_event(ev, 1.0, &Modifiers::default(), &mut doc, &root, &mut handled, &mut focus, node, &taffy);
        handled
    }

    /// The three source columns scroll together through the SHARED overlay
    /// scrollbar, and its thumb drags: with a tall source band a thumb exists,
    /// pressing it captures the pointer, and dragging it down advances the
    /// shared source-band offset.
    #[test]
    fn source_band_thumb_drag_scrolls() {
        let mut view = MergeView::new();
        // A base/ours/theirs triple tall enough to overflow the source band.
        let base: String = (0..60).map(|i| format!("row {i}\n")).collect();
        let ours = format!("{base}ours-tail\n");
        let theirs = format!("{base}theirs-tail\n");
        view.set_inputs("base", &base, "ours", &ours, "theirs", &theirs);
        run_frame(&mut view, 1000.0, 320.0);

        let thumb = view
            .source_scroll
            .thumb_rect(ScrollAxis::Vertical)
            .expect("the overflowing source band draws a draggable thumb");
        let center = thumb.center();
        let before = view.source_scroll.offset_y();
        assert_eq!(before, 0.0, "source band starts at the top");

        send(&mut view, &moved(center.x, center.y));
        send(&mut view, &mouse(ElementState::Pressed));
        assert!(
            view.source_scroll.wants_pointer_capture(),
            "pressing the thumb starts a drag"
        );
        send(&mut view, &moved(center.x, center.y + 90.0));
        send(&mut view, &mouse(ElementState::Released));

        let after = view.source_scroll.offset_y();
        assert!(after > before + 1.0, "dragging the thumb scrolled the source band: {before} -> {after}");
    }
}
