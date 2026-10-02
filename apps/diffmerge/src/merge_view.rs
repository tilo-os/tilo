//! Three-way merge view.
//!
//! Top band: three columns — base / ours / theirs — rendered hunk-by-hunk so a
//! conflict shows the same vertical span across all three. Conflicts get
//! per-hunk action chips (take ours / take theirs / take both). Bottom band:
//! the merged result, mirrored from the Automerge `Text` so it is the
//! collaboratively-editable authoritative output. Clicking an action chip
//! re-composes the merged buffer and writes it into the CRDT.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::AutoCommit;
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{CodeEditor, Language, Widget};

use crate::actions::{self, DiffmergeAction, HunkChoice, MergeResourceCtx};
use crate::doc::{MergeDoc, SharedMerge};
use crate::highlight;
use crate::model::{MergeHunk, MergeModel, Resolution};
use ui::paint::{PaintCtx, with_alpha};
use ui::widget::code_editor::Span;

/// Minimum code-line pitch; the measured line height of `FONT_SIZE` wins
/// when the text scale makes it taller (see [`Metrics`]).
const ROW_H: f64 = 19.0;
/// Minimum column / merged header band.
const HEADER_H: f64 = 30.0;
/// Minimum band a change hunk reserves under its rows for the chips
/// (`chip_h + 6`).
const ACTION_H: f64 = 26.0;
pub(crate) const FONT_SIZE: f32 = 12.5;
const PAD_X: f64 = 8.0;
const GAP: f64 = 6.0;
/// Text sizes the panel paints with; [`Metrics::measure`] measures the same
/// values so bands and glyphs move together.
const HEADER_FONT_SIZE: f32 = 12.5;
const CHIP_FONT_SIZE: f32 = 11.5;
const COMPACT_SOURCE_FONT_SIZE: f32 = 12.0;
const MERGED_HEADER_FONT_SIZE: f32 = 13.0;
const GUTTER_FONT_SIZE: f32 = 11.0;
/// Chip label padding for the wide and compact layouts. The inter-chip gap
/// is a spacing step read from the theme at the use site (`s2` wide, `s1`
/// compact — HIG §7b siblings in a dense row).
const CHIP_PAD_X: f64 = 8.0;
const CHIP_PAD_X_COMPACT: f64 = 7.0;
/// Minimum x where compact chips start (after the status label).
const COMPACT_CHIPS_X: f64 = 64.0;
/// Minimum merged-band line-number gutter.
const MERGED_GUTTER_W: f64 = 44.0;

/// Row metrics measured from the renderer the panel paints with.
///
/// Refreshed at the top of every draw; hunk spans, scrolling, chip hit rects,
/// the accessibility export and the paint all read this one snapshot, so a
/// text-scale change moves rows, chips and the merged band together instead
/// of letting 25px glyphs overprint 19px rows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    /// Code line pitch.
    row_h: f64,
    /// Column header / merged header band.
    header_h: f64,
    /// Resolution chip height.
    chip_h: f64,
    /// Band a change hunk reserves under its rows (`chip_h` plus breathing room).
    action_h: f64,
    /// Top offset that centres a `FONT_SIZE` line inside `row_h`.
    text_dy: f64,
    /// Merged band line-number gutter width.
    gutter_w: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            row_h: ROW_H,
            header_h: HEADER_H,
            chip_h: ACTION_H - 6.0,
            action_h: ACTION_H,
            text_dy: 2.0,
            gutter_w: MERGED_GUTTER_W,
        }
    }
}

impl Metrics {
    fn measure(tr: &mut TextRenderer) -> Self {
        let line_h = f64::from(tr.measure("Ag", FONT_SIZE, None).1);
        let row_h = (line_h + 3.0).max(ROW_H);
        let header_label_h = f64::from(tr.measure("Ag", HEADER_FONT_SIZE, None).1);
        let header_h = (header_label_h + 14.0).max(HEADER_H);
        let chip_label_h = f64::from(tr.measure("Ag", CHIP_FONT_SIZE, None).1);
        let chip_h = (chip_label_h + 5.0).max(ACTION_H - 6.0);
        let gutter_w =
            (f64::from(tr.measure("000", GUTTER_FONT_SIZE, None).0) + 20.0).max(MERGED_GUTTER_W);
        Self {
            row_h,
            header_h,
            chip_h,
            action_h: chip_h + 6.0,
            text_dy: (row_h - line_h) / 2.0,
            gutter_w,
        }
    }
}
/// Below this width, three code panes cease to be readable.  Keep one full
/// source pane and make switching sources explicit instead of merely squeezing
/// desktop geometry into a phone.
const COMPACT_BREAKPOINT: f64 = 640.0;

/// One of the source revisions shown by the compact merge view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourcePane {
    Base,
    Ours,
    Theirs,
}

impl SourcePane {
    const ALL: [Self; 3] = [Self::Base, Self::Ours, Self::Theirs];

    fn label(self) -> &'static str {
        match self {
            Self::Base => "Base",
            Self::Ours => "Ours",
            Self::Theirs => "Theirs",
        }
    }

    fn a11y_id(self) -> &'static str {
        match self {
            Self::Base => "diffmerge.source.base",
            Self::Ours => "diffmerge.source.ours",
            Self::Theirs => "diffmerge.source.theirs",
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Base => Self::Theirs,
            Self::Ours => Self::Base,
            Self::Theirs => Self::Ours,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Base => Self::Ours,
            Self::Ours => Self::Theirs,
            Self::Theirs => Self::Base,
        }
    }
}

/// A compact source-switcher hit rect. The same `SourcePane` is used by
/// pointer input, keyboard shortcuts, and accessibility actions.
#[derive(Clone, Copy)]
struct SourceHit {
    rect: Rect,
    source: SourcePane,
}

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
    /// Last three-way inputs loaded locally (projected into the document).
    shared: Option<SharedMerge>,
    base_label: String,
    ours_label: String,
    theirs_label: String,
    /// Syntax spans for each hunk's source lines; every side is tokenized as
    /// one file across its hunks.
    source_spans: Vec<HunkSpans>,
    rect: Rect,
    scroll_y: f64,
    /// Cached y-extent (in content space) of each hunk row in the source band.
    hunk_spans: Vec<(f64, f64)>,
    action_hits: Vec<ActionHit>,
    source_hits: Vec<SourceHit>,
    /// The visible source revision on a narrow display. Wider displays retain
    /// the simultaneous three-pane comparison.
    selected_source: SourcePane,
    active_hunk: Option<usize>,
    /// Cursor position cache for click hit-testing.
    cursor: Point,
    dirty: bool,
    /// Set when a fresh set of inputs was loaded: the next `sync` seeds the
    /// merged buffer (a projection of the external inputs, not a replayable
    /// action) and clears any stale per-hunk resolutions.
    pending_seed: bool,
    /// When the framework hosts this view, seed writes are queued here instead
    /// of being written into the document directly.
    hosted: bool,
    seed_request: Option<String>,
    expected_content: Option<crate::doc::AuthoredSnapshot>,
    merged_output: String,
    /// A conflict resolution the user just triggered via a chip, waiting to be
    /// drained by the parent app and dispatched as a [`DiffmergeAction`] through
    /// the shared action path (so a chip and Ctrl+K produce one undo boundary).
    pending_action: Option<DiffmergeAction>,
    /// Last measured row metrics (see [`Metrics`]); defaults until the first draw.
    metrics: Metrics,
    /// The authoritative merged CRDT text, edited through the same shared
    /// caret/selection/IME/clipboard implementation as every code surface.
    merged_editor: CodeEditor,
}

impl Default for MergeView {
    fn default() -> Self {
        Self::new()
    }
}

impl MergeView {
    pub fn new() -> Self {
        Self {
            model: MergeModel::default(),
            shared: None,
            base_label: String::new(),
            ours_label: String::new(),
            theirs_label: String::new(),
            source_spans: Vec::new(),
            rect: Rect::ZERO,
            scroll_y: 0.0,
            hunk_spans: Vec::new(),
            action_hits: Vec::new(),
            source_hits: Vec::new(),
            selected_source: SourcePane::Ours,
            active_hunk: None,
            cursor: Point::ZERO,
            dirty: true,
            pending_seed: false,
            hosted: false,
            seed_request: None,
            expected_content: None,
            merged_output: String::new(),
            pending_action: None,
            metrics: Metrics::default(),
            merged_editor: CodeEditor::new(Vec::new())
                .with_language(Language::PlainText)
                .with_id("diffmerge_merged_editor"),
        }
    }

    pub(crate) fn context_matches(&self, doc: &AutoCommit) -> bool {
        self.expected_content
            .as_ref()
            .is_some_and(|expected| *expected == crate::doc::authored_snapshot(doc))
    }
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_framework_hosted(&mut self, hosted: bool) {
        self.hosted = hosted;
    }

    /// The provisional buffer a hosted view wants written as [`DiffmergeAction::SeedMerge`].
    pub(crate) fn take_seed_request(&mut self) -> Option<String> {
        self.seed_request.take()
    }

    /// Show the merged buffer the framework resource already holds.
    pub(crate) fn adopt_merged(&mut self, merged: &str) {
        if self.merged_output != merged {
            self.merged_output = merged.to_string();
        }
    }

    pub(crate) fn acknowledge_content(&mut self, doc: &AutoCommit) {
        self.expected_content = Some(crate::doc::authored_snapshot(doc));
        self.merged_output = MergeDoc::merged_text(doc);
    }
    /// Historical/remote content cannot identify the unsaved original input hunks.
    pub(crate) fn reconcile_content(&mut self, doc: &AutoCommit) {
        if !self.pending_seed && !self.context_matches(doc) {
            if let Some(shared) = SharedMerge::read(doc) {
                self.adopt_shared(shared, doc);
            } else {
                self.model = MergeModel::default();
                self.shared = None;
                self.base_label.clear();
                self.ours_label.clear();
                self.theirs_label.clear();
                self.source_spans.clear();
                self.pending_action = None;
                self.action_hits.clear();
                self.hunk_spans.clear();
                self.active_hunk = None;
                self.expected_content = None;
                self.dirty = true;
            }
        }
        self.merged_output = MergeDoc::merged_text(doc);
    }

    /// Adopt a shared three-way projection from the document (or present).
    pub(crate) fn adopt_shared_model(&mut self, title: &str, model: &MergeModel) {
        let _ = title;
        if self.model.hunks.is_empty() && !model.hunks.is_empty() {
            self.model = model.clone();
            self.source_spans = HunkSpans::for_model(
                &self.model,
                [&self.base_label, &self.ours_label, &self.theirs_label],
            );
            self.recompute_spans();
            self.active_hunk = self
                .first_conflict()
                .or_else(|| self.model.change_indices().first().copied());
            self.dirty = true;
        }
    }

    fn adopt_shared(&mut self, shared: SharedMerge, doc: &AutoCommit) {
        self.base_label = shared.base_label.clone();
        self.ours_label = shared.ours_label.clone();
        self.theirs_label = shared.theirs_label.clone();
        self.shared = Some(shared.clone());
        self.model = shared.into_model();
        crate::doc::apply_resolutions(&mut self.model, doc);
        self.source_spans = HunkSpans::for_model(
            &self.model,
            [&self.base_label, &self.ours_label, &self.theirs_label],
        );
        self.recompute_spans();
        self.active_hunk = self
            .first_conflict()
            .or_else(|| self.model.change_indices().first().copied());
        self.expected_content = Some(crate::doc::authored_snapshot(doc));
        self.pending_action = None;
        self.action_hits.clear();
        self.dirty = true;
    }

    pub(crate) fn shared_merge(&self) -> Option<&SharedMerge> {
        self.shared.as_ref()
    }

    /// Place the panel at `rect` directly (the parent app owns the toolbar
    /// band above it and re-places the view whenever that band's measured
    /// height changes, so paint and layout agree within one frame).
    pub(crate) fn place(&mut self, rect: Rect) {
        if rect != self.rect {
            self.rect = rect;
            self.recompute_spans();
            self.scroll_y = self.scroll_y.min(self.max_scroll());
        }
    }

    /// `(row_h, header_h, chip_h)` from the last draw.
    #[cfg(test)]
    pub(crate) fn metrics_for_test(&self) -> (f64, f64, f64) {
        (
            self.metrics.row_h,
            self.metrics.header_h,
            self.metrics.chip_h,
        )
    }

    /// Total content height of the source band in content space.
    #[cfg(test)]
    pub(crate) fn content_height_for_test(&self) -> f64 {
        self.content_height()
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
        self.shared = Some(SharedMerge {
            base: base.to_string(),
            ours: ours.to_string(),
            theirs: theirs.to_string(),
            base_label: base_label.to_string(),
            ours_label: ours_label.to_string(),
            theirs_label: theirs_label.to_string(),
        });
        self.model = MergeModel::compute(base, ours, theirs);
        self.base_label = base_label.to_string();
        self.ours_label = ours_label.to_string();
        self.theirs_label = theirs_label.to_string();
        self.source_spans =
            HunkSpans::for_model(&self.model, [base_label, ours_label, theirs_label]);
        // Spans depend on the model as much as on the metrics; a new input
        // set at an unchanged rect must not paint against stale spans.
        self.recompute_spans();
        self.scroll_y = 0.0;
        self.active_hunk = self
            .first_conflict()
            .or_else(|| self.model.change_indices().first().copied());
        self.dirty = true;
        self.pending_seed = true;
    }

    pub fn model(&self) -> &MergeModel {
        &self.model
    }

    pub(crate) fn merged_editor_focused(&self) -> bool {
        self.merged_editor.is_focused()
    }

    fn is_compact(&self) -> bool {
        self.rect.width() < COMPACT_BREAKPOINT
    }

    fn select_source(&mut self, source: SourcePane) {
        if self.selected_source != source {
            self.selected_source = source;
            self.dirty = true;
        }
    }

    /// Compact source selector geometry, shared by painting, hit testing and
    /// the accessibility export. Empty on tablet/desktop, where all three
    /// source panes remain visible at once.
    fn source_selector_rects(&self) -> Vec<SourceHit> {
        if !self.is_compact() {
            return Vec::new();
        }
        let inset = PAD_X;
        let gap = 4.0;
        let available = (self.rect.width() - 2.0 * inset - 2.0 * gap).max(0.0);
        let width = available / SourcePane::ALL.len() as f64;
        SourcePane::ALL
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let x0 = self.rect.x0 + inset + index as f64 * (width + gap);
                SourceHit {
                    rect: Rect::new(
                        x0,
                        self.rect.y0 + 4.0,
                        x0 + width,
                        self.rect.y0 + self.metrics.header_h - 4.0,
                    ),
                    source: *source,
                }
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn compact_source_hits(&self) -> Vec<(Rect, &'static str)> {
        self.source_selector_rects()
            .into_iter()
            .map(|hit| (hit.rect, hit.source.label()))
            .collect()
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
        // `next_pos` is derived from `pos` (a valid index into `changes`) via
        // `min`/`saturating_sub`, so it always stays in bounds.
        let Some(&hunk) = changes.get(next_pos) else {
            return false;
        };
        self.active_hunk = Some(hunk);
        self.scroll_hunk_into_view(hunk);
        self.dirty = true;
        true
    }

    fn scroll_hunk_into_view(&mut self, hunk: usize) {
        if let Some((top, bot)) = self.hunk_spans.get(hunk).copied() {
            let vh = self.source_band_height();
            let mid = f64::midpoint(top, bot);
            self.scroll_y = (mid - vh / 2.0).max(0.0).min(self.max_scroll());
        }
    }

    fn source_band_height(&self) -> f64 {
        // The source band occupies the top ~58% of the panel.
        ((self.rect.height() - self.metrics.header_h) * 0.58).max(0.0)
    }

    fn merged_band_rect(&self) -> Rect {
        let r = self.rect;
        let band_top = r.y0 + self.metrics.header_h + self.source_band_height();
        Rect::new(r.x0, band_top, r.x1, r.y1)
    }

    fn content_height(&self) -> f64 {
        self.hunk_spans.last().map_or(0.0, |(_, b)| *b)
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.source_band_height()).max(0.0)
    }

    fn scroll_by(&mut self, dy: f64) {
        let new = (self.scroll_y + dy).max(0.0).min(self.max_scroll());
        if (new - self.scroll_y).abs() > f64::EPSILON {
            self.scroll_y = new;
            self.dirty = true;
        }
    }

    /// Recompute the per-hunk vertical spans (content space).
    fn recompute_spans(&mut self) {
        self.hunk_spans.clear();
        let m = self.metrics;
        let mut y = 0.0;
        for h in &self.model.hunks {
            let rows = h.base.len().max(h.ours.len()).max(h.theirs.len()).max(1);
            let mut height = rows as f64 * m.row_h;
            if h.is_change {
                height += m.action_h + GAP;
            }
            self.hunk_spans.push((y, y + height));
            y += height;
        }
    }

    /// Column headers (base / ours / theirs) above the source band.
    fn draw_column_headers(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        if self.is_compact() {
            self.source_hits = self.source_selector_rects();
            for hit in &self.source_hits {
                let selected = hit.source == self.selected_source;
                let accent = match hit.source {
                    SourcePane::Base => c.text_dim,
                    SourcePane::Ours => c.semantic.success,
                    SourcePane::Theirs => c.semantic.info,
                };
                let chip = RoundedRect::from_rect(hit.rect, 5.0);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if selected {
                        with_alpha(accent, 210)
                    } else {
                        with_alpha(accent, 42)
                    },
                    None,
                    &chip,
                );
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    with_alpha(accent, if selected { 230 } else { 130 }),
                    None,
                    &chip,
                );
                let label = hit.source.label();
                let (_, lh) = tr.measure(label, COMPACT_SOURCE_FONT_SIZE, None);
                tr.draw_text(
                    scene,
                    label,
                    COMPACT_SOURCE_FONT_SIZE,
                    if selected { c.surfaces.base } else { accent },
                    transform
                        * Affine::translate((
                            hit.rect.x0 + 8.0,
                            hit.rect.y0 + (hit.rect.height() - f64::from(lh)) / 2.0,
                        )),
                    Some((hit.rect.width() - 16.0) as f32),
                );
            }
            return;
        }

        let header_h = self.metrics.header_h;
        let col_w = r.width() / 3.0;
        let base_col = Rect::new(r.x0, r.y0, r.x0 + col_w, r.y0 + header_h);
        let ours_col = Rect::new(r.x0 + col_w, r.y0, r.x0 + 2.0 * col_w, r.y0 + header_h);
        let theirs_col = Rect::new(r.x0 + 2.0 * col_w, r.y0, r.x1, r.y0 + header_h);

        draw_header(
            scene,
            transform,
            tr,
            theme,
            base_col,
            &self.base_label,
            c.text_dim,
        );
        draw_header(
            scene,
            transform,
            tr,
            theme,
            ours_col,
            &self.ours_label,
            c.semantic.success,
        );
        draw_header(
            scene,
            transform,
            tr,
            theme,
            theirs_col,
            &self.theirs_label,
            c.semantic.info,
        );
    }

    /// Vertical separators between the three source columns.
    fn draw_band_separators(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.is_compact() {
            return;
        }
        let c = &theme.tokens.colors;
        let r = self.rect;
        let col_w = r.width() / 3.0;
        let band_top = r.y0 + self.metrics.header_h;
        let band_bot = band_top + self.source_band_height();
        for k in 1..3 {
            let x = r.x0 + f64::from(k) * col_w;
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                ui::surface::edge_on(c.surfaces.raised),
                None,
                &Line::new(Point::new(x, band_top), Point::new(x, band_bot)),
            );
        }
    }

    /// Draw every hunk row in the source band: side tints, the active-hunk
    /// ring, the three code columns and (for change hunks) the resolution
    /// chips. Populates `self.action_hits` for the next click hit-test.
    /// Hunk tint per side plus the active-hunk highlight ring (part of
    /// [`Self::draw_hunks`]).
    fn draw_hunk_tint(
        ctx: &mut PaintCtx<'_>,
        block: Rect,
        compact_source: Option<SourcePane>,
        h: &MergeHunk,
        is_active: bool,
    ) {
        let col_w = block.width() / 3.0;
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
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

        if let Some(source) = compact_source {
            let tint = match source {
                SourcePane::Base => None,
                SourcePane::Ours => ours_tint,
                SourcePane::Theirs => theirs_tint,
            };
            if let Some(t) = tint {
                ctx.fill_rect(block, t);
            }
        } else {
            if let Some(t) = ours_tint {
                let rect = Rect::new(block.x0 + col_w, block.y0, block.x0 + 2.0 * col_w, block.y1);
                ctx.fill_rect(rect, t);
            }
            if let Some(t) = theirs_tint {
                let rect = Rect::new(block.x0 + 2.0 * col_w, block.y0, block.x1, block.y1);
                ctx.fill_rect(rect, t);
            }
        }

        // Active hunk highlight ring.
        if is_active && h.is_change {
            let ring = Rect::new(
                block.x0 + 1.0,
                block.y0 - 1.0,
                block.x1 - 1.0,
                block.y1 + 1.0,
            );
            ctx.stroke_round(ring, 3.0, c.accent[5], 1.5);
        }
    }

    /// The resolution status label + take-ours/theirs/both chips painted
    /// under hunk `hi`'s `block` (part of [`Self::draw_hunks`]). Returns the
    /// chips' hit targets.
    /// Hunk `h`'s resolution status label, under `block` on the left. On a
    /// phone, keep this semantically distinct from the source switcher and the
    /// mutation chips: it says whether the hunk still needs a resolution, while
    /// every chip says what it will do. Returns the label's right edge.
    fn draw_hunk_status(
        ctx: &mut PaintCtx<'_>,
        block: Rect,
        h: &MergeHunk,
        compact: bool,
        m: Metrics,
    ) -> f64 {
        let ay = block.y1 + 2.0;
        let status = resolution_label(h.resolution);
        let status_label = if compact {
            compact_status_label(h.resolution)
        } else {
            status.0
        };
        let status_x = if compact {
            block.x0 + PAD_X
        } else {
            block.x0 + block.width() / 3.0 + PAD_X
        };
        let (status_w, status_h) = ctx.text.measure(status_label, CHIP_FONT_SIZE, None);
        ctx.text.draw_text(
            ctx.scene,
            status_label,
            CHIP_FONT_SIZE,
            status.1.color(&ctx.theme.tokens.colors),
            ctx.transform
                * Affine::translate((status_x, ay + (m.chip_h - f64::from(status_h)) / 2.0)),
            None,
        );
        status_x + f64::from(status_w)
    }

    fn draw_hunk_action_chips(
        ctx: &mut PaintCtx<'_>,
        block: Rect,
        (hi, h): (usize, &MergeHunk),
        compact: bool,
        m: Metrics,
    ) -> Vec<ActionHit> {
        let PaintCtx {
            scene,
            transform,
            text: tr,
            theme,
        } = ctx.reborrow();
        let (r, col_w) = (block, block.width() / 3.0);
        let mut action_hits = Vec::with_capacity(3);
        let c = &theme.tokens.colors;
        let ay = block.y1 + 2.0;
        // Chip hues track the theme so a palette / high-contrast swap
        // keeps the resolution controls coherent: ours = success (green),
        // theirs = info (blue), both = the neutral dim tone — mirroring
        // the status label's own `ColorClass` mapping.
        let choices = [
            (HunkChoice::Ours, c.semantic.success),
            (HunkChoice::Theirs, c.semantic.info),
            (HunkChoice::Both, c.text_dim),
        ];
        let sp = &theme.tokens.spacing;
        let (pad_x, gap) = if compact {
            (CHIP_PAD_X_COMPACT, f64::from(sp.s1))
        } else {
            (CHIP_PAD_X, f64::from(sp.s2))
        };

        let status_x1 = Self::draw_hunk_status(
            &mut PaintCtx::new(scene, transform, tr, theme),
            block,
            h,
            compact,
            m,
        );

        // Chips live in the theirs column (wide) or after the status label
        // (compact). Their labels are measured with the style they are painted
        // in; when the explicit verb form cannot fit the pane the short form is
        // used, so no chip ever paints past the pane edge.
        let chips_x0 = if compact {
            (r.x0 + COMPACT_CHIPS_X).max(status_x1 + PAD_X)
        } else {
            r.x0 + 2.0 * col_w + PAD_X
        };
        let available = r.x1 - PAD_X - chips_x0;
        let long: [&str; 3] = if compact {
            ["Use ours", "Use theirs", "Use both"]
        } else {
            ["Take ours", "Take theirs", "Take both"]
        };
        let short: [&str; 3] = ["Ours", "Theirs", "Both"];
        let prefer_long = compact || h.resolution.is_conflict();
        let widths_for = |tr: &mut TextRenderer, labels: &[&str; 3]| -> [f64; 3] {
            let mut widths = [0.0; 3];
            for (w, label) in widths.iter_mut().zip(labels) {
                *w = f64::from(tr.measure(label, CHIP_FONT_SIZE, None).0) + 2.0 * pad_x;
            }
            widths
        };
        let mut labels = if prefer_long { long } else { short };
        let mut widths = widths_for(tr, &labels);
        if prefer_long && widths.iter().sum::<f64>() + 2.0 * gap > available {
            labels = short;
            widths = widths_for(tr, &short);
        }

        let mut cx = chips_x0;
        for ((label, w), (choice, col)) in labels.iter().zip(widths).zip(choices) {
            let chip = Rect::new(cx, ay, cx + w, ay + m.chip_h);
            let selected = chip_is_selected(h.resolution, choice);
            let bg = if selected {
                with_alpha(col, 220)
            } else {
                with_alpha(col, 50)
            };
            let rr = RoundedRect::from_rect(chip, 5.0);
            scene.fill(Fill::NonZero, transform, bg, None, &rr);
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                with_alpha(col, 160),
                None,
                &rr,
            );
            // A selected chip is filled in its hue; the label takes the ink
            // that reads on that hue rather than a fixed near-black.
            let text_col = if selected { c.on_surface(col).ink } else { col };
            let (lw, lh) = tr.measure(label, CHIP_FONT_SIZE, None);
            tr.draw_text(
                scene,
                label,
                CHIP_FONT_SIZE,
                text_col,
                transform
                    * Affine::translate((
                        cx + (w - f64::from(lw)) / 2.0,
                        ay + (m.chip_h - f64::from(lh)) / 2.0,
                    )),
                None,
            );
            action_hits.push(ActionHit {
                rect: chip,
                hunk: hi,
                choice,
            });
            cx += w + gap;
        }
        action_hits
    }

    fn draw_hunks(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let r = self.rect;
        let m = self.metrics;
        let col_w = r.width() / 3.0;
        let compact_source = self.is_compact().then_some(self.selected_source);
        let band_top = r.y0 + m.header_h;
        let band_bot = band_top + self.source_band_height();

        let mut ctx = PaintCtx::new(scene, transform, tr, theme);
        for (hi, h) in self.model.hunks.iter().enumerate() {
            // `hunk_spans` is rebuilt one-for-one with `model.hunks` in
            // `recompute_spans`; skip drawing rather than panic if that ever
            // drifts out of sync.
            let Some(&(top, _bot)) = self.hunk_spans.get(hi) else {
                continue;
            };
            let y = band_top + top - self.scroll_y;
            let rows = h.base.len().max(h.ours.len()).max(h.theirs.len()).max(1);
            let block_h = rows as f64 * m.row_h;

            // Cull off-screen hunks cheaply.
            if y + block_h + m.action_h < band_top || y > band_bot {
                continue;
            }

            let block = Rect::new(r.x0, y, r.x1, y + block_h);
            Self::draw_hunk_tint(
                &mut ctx,
                block,
                compact_source,
                h,
                Some(hi) == self.active_hunk,
            );

            // Phones show one source at a time; tablets and desktops keep the
            // aligned three-column comparison.
            let spans = self.source_spans.get(hi);
            let side = |pick: fn(&HunkSpans) -> &[Vec<Span>]| spans.map_or(&[][..], pick);
            let (base, ours, theirs) = (
                (&h.base, side(|s| &s.base)),
                (&h.ours, side(|s| &s.ours)),
                (&h.theirs, side(|s| &s.theirs)),
            );
            match compact_source {
                Some(SourcePane::Base) => {
                    draw_lines(&mut ctx, Point::new(r.x0 + PAD_X, y), base, m);
                }
                Some(SourcePane::Ours) => {
                    draw_lines(&mut ctx, Point::new(r.x0 + PAD_X, y), ours, m);
                }
                Some(SourcePane::Theirs) => {
                    draw_lines(&mut ctx, Point::new(r.x0 + PAD_X, y), theirs, m);
                }
                None => {
                    draw_lines(&mut ctx, Point::new(r.x0 + PAD_X, y), base, m);
                    draw_lines(&mut ctx, Point::new(r.x0 + col_w + PAD_X, y), ours, m);
                    draw_lines(
                        &mut ctx,
                        Point::new(r.x0 + 2.0 * col_w + PAD_X, y),
                        theirs,
                        m,
                    );
                }
            }

            // Action chips for change hunks.
            if h.is_change {
                let hits = Self::draw_hunk_action_chips(
                    &mut ctx,
                    block,
                    (hi, h),
                    compact_source.is_some(),
                    m,
                );
                self.action_hits.extend(hits);
            }
        }
    }

    /// The merged-result band: header (with conflict count) and the scrolled
    /// merged buffer body.
    fn draw_merged_band(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let m = self.metrics;
        let merged = self.merged_band_rect();
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &merged);
        // Merged header.
        let mh = Rect::new(merged.x0, merged.y0, merged.x1, merged.y0 + m.header_h);
        scene.fill(
            Fill::NonZero,
            transform,
            with_alpha(c.accent[5], 40),
            None,
            &mh,
        );
        let conflicts = self.model.conflict_count();
        let header_label = if self.expected_content.is_none() {
            "Saved merged output · originals unavailable".to_string()
        } else if conflicts == 0 {
            if self.is_compact() {
                "Merged result · resolved".to_string()
            } else {
                "Merged result · all conflicts resolved".to_string()
            }
        } else if self.is_compact() {
            format!("Merged result · {conflicts} conflict(s)")
        } else {
            format!("Merged result · {conflicts} conflict(s) remaining")
        };
        let hc = if conflicts == 0 {
            c.semantic.success
        } else {
            c.semantic.danger
        };
        let (_, hh) = tr.measure(&header_label, MERGED_HEADER_FONT_SIZE, None);
        tr.draw_text(
            scene,
            &header_label,
            MERGED_HEADER_FONT_SIZE,
            hc,
            transform
                * Affine::translate((
                    merged.x0 + PAD_X + 4.0,
                    merged.y0 + (m.header_h - f64::from(hh)) / 2.0,
                )),
            Some((merged.width() - PAD_X * 2.0 - 8.0) as f32),
        );

        // Merged buffer body. The shared editor owns text, caret, selection,
        // scrolling, clipboard, IME, accessibility, and CRDT splices.
        let merged_body = Rect::new(merged.x0, merged.y0 + m.header_h, merged.x1, merged.y1);
        self.merged_editor.set_rect(merged_body);
        self.merged_editor
            .draw_embedded(scene, transform, tr, theme);
    }
}

impl Widget for MergeView {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        if self.hosted {
            if self.pending_seed {
                self.seed_request = Some(self.model.merged_text());
                self.pending_seed = false;
            } else {
                self.reconcile_content(state);
                if !MergeDoc::has_merged(state) {
                    self.seed_request = Some(self.model.merged_text());
                }
            }
        } else if self.pending_seed {
            // Fresh inputs: seed the provisional merged output and drop any
            // stale per-hunk resolutions whose indices no longer correspond.
            // The hosted path queues this as `SeedMerge` instead.
            MergeDoc::clear_resolutions(state);
            MergeDoc::set_merged(state, &self.model.merged_text());
            self.pending_seed = false;
            self.acknowledge_content(state);
        } else {
            self.reconcile_content(state);
        }
        if !self.hosted && !MergeDoc::has_merged(state) {
            MergeDoc::set_merged(state, &self.model.merged_text());
        }
        let merged = MergeDoc::merged_text_obj(state);
        self.merged_editor.bind_text(merged);
        self.merged_editor.pull(state);
        // Reconcile chip state from resolutions recorded by (possibly remote)
        // actions, so a reload or a peer shows the same take-a-side selections.
        // The merged `Text` itself stays authoritative — the actions already
        // wrote it — so this only tints the in-memory hunks.
        for (hunk, choice) in MergeDoc::resolutions(state) {
            if let (Some(h), Some(c)) = (
                self.model.hunks.get_mut(hunk),
                HunkChoice::from_tag(&choice),
            ) && h.is_change
            {
                let res = c.to_resolution();
                if h.resolution != res {
                    h.resolution = res;
                    self.dirty = true;
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
        // `new_leaf` on a freshly created tree never fails.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh tree cannot fail")]
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
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.place(Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        ));
        self.recompute_spans();
        let merged = self.merged_band_rect();
        self.merged_editor.set_rect(Rect::new(
            merged.x0,
            merged.y0 + self.metrics.header_h,
            merged.x1,
            merged.y1,
        ));
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
        self.source_hits.clear();
        // One metric snapshot per frame: spans, scroll clamp, chips and paint
        // all follow the renderer's actual line height.
        let measured = Metrics::measure(tr);
        if measured != self.metrics {
            self.metrics = measured;
            self.recompute_spans();
            self.scroll_y = self.scroll_y.min(self.max_scroll());
        }
        let c = &theme.tokens.colors;
        let r = self.rect;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &r);

        self.draw_column_headers(scene, transform, tr, theme);
        self.draw_band_separators(scene, transform, theme);

        let band_top = r.y0 + self.metrics.header_h;
        let band_bot = band_top + self.source_band_height();
        let band_clip = Rect::new(r.x0, band_top, r.x1, band_bot);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &band_clip,
        );
        self.draw_hunks(scene, transform, tr, theme);
        if self.expected_content.is_none() && !self.merged_output.is_empty() {
            tr.draw_text(scene, "Original inputs unavailable for this saved output. Load new inputs to resolve hunks.", 14., c.text_dim,
                transform * Affine::translate((r.x0 + 16., band_top + 16.)), Some((r.width()-32.) as f32));
        }
        scene.pop_layer();

        // Divider between source band and merged band.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_focus,
            None,
            &Line::new(Point::new(r.x0, band_bot), Point::new(r.x1, band_bot)),
        );

        self.draw_merged_band(scene, transform, tr, theme);
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.merged_editor.is_focused() || self.merged_editor.rect().contains(self.cursor) {
            self.merged_editor.handle_event(event, ctx);
            if *ctx.handled {
                return;
            }
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Only scroll the source band when the cursor is above the
                // merged divider; otherwise let the merged band stay put.
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(-*y) * 60.0,
                    MouseScrollDelta::PixelDelta(pp) => -pp.y,
                };
                self.scroll_by(dy);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(source) = self
                    .source_hits
                    .iter()
                    .find(|hit| hit.rect.contains(self.cursor))
                    .map(|hit| hit.source)
                {
                    self.select_source(source);
                    *ctx.handled = true;
                    return;
                }
                let hit = self
                    .action_hits
                    .iter()
                    .find(|a| a.rect.contains(self.cursor))
                    .copied();
                if let Some(a) = hit {
                    self.resolve(a.hunk, a.choice);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_compact() => {
                let source = match logical_key {
                    Key::Named(NamedKey::ArrowLeft) => Some(self.selected_source.previous()),
                    Key::Named(NamedKey::ArrowRight) => Some(self.selected_source.next()),
                    Key::Character(ch) if ch.eq_ignore_ascii_case("b") => Some(SourcePane::Base),
                    Key::Character(ch) if ch.eq_ignore_ascii_case("o") => Some(SourcePane::Ours),
                    Key::Character(ch) if ch.eq_ignore_ascii_case("t") => Some(SourcePane::Theirs),
                    _ => None,
                };
                if let Some(source) = source {
                    self.select_source(source);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "MergeView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.merged_editor.set_focused(focused);
    }

    fn is_focused(&self) -> bool {
        self.merged_editor.is_focused()
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        self.source_selector_rects()
            .into_iter()
            .map(|hit| {
                (
                    format!("merge_source:{}", hit.source.label().to_lowercase()),
                    hit.rect,
                )
            })
            .collect()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut node = AccessNode::new(Role::Group).with_name("Three-way merge");
        let mut children = Vec::new();
        if self.is_compact() {
            children.extend(SourcePane::ALL.into_iter().map(|source| {
                AccessNode::new(Role::ToggleButton)
                    .with_name(format!("Show {} source", source.label()))
                    .with_description("Switch the visible merge source")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(source == self.selected_source),
                    )
                    .with_hashed_key(source.a11y_id())
            }));
            node = node.with_description(
                "One source is visible at a time. Use Left and Right Arrow to switch source.",
            );
        } else {
            node = node.with_description("Base, ours, and theirs are shown side by side.");
        }
        children.extend(self.action_hits.iter().map(|hit| {
            let choice = match hit.choice {
                HunkChoice::Ours => "ours",
                HunkChoice::Theirs => "theirs",
                HunkChoice::Both => "both",
            };
            let label = match hit.choice {
                HunkChoice::Ours => "Take ours",
                HunkChoice::Theirs => "Take theirs",
                HunkChoice::Both => "Take both",
            };
            AccessNode::new(Role::Button)
                .with_name(format!("{label} for change {}", hit.hunk.saturating_add(1)))
                .with_description("Resolve this shared merge hunk")
                .with_action_target(format!("diffmerge.merge.resolve:{}:{choice}", hit.hunk))
                .with_bounds(hit.rect)
                .with_state(
                    AccessState::new().focusable(true).selected(
                        self.model
                            .hunks
                            .get(hit.hunk)
                            .is_some_and(|hunk| chip_is_selected(hunk.resolution, hit.choice)),
                    ),
                )
        }));
        if !children.is_empty() {
            node = node.with_children(children);
        }
        Some(node)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let index = *path.first()?;
        self.source_selector_rects().get(index).map(|hit| hit.rect)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let Some(&index) = path.first() else {
            return false;
        };
        let Some(source) = SourcePane::ALL.get(index).copied() else {
            return false;
        };
        self.select_source(source);
        true
    }

    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        use ui::a11y::hash_key;
        if !self.is_compact()
            || !matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus)
        {
            return false;
        }
        let Some(&key) = key_path.first() else {
            return false;
        };
        let Some(source) = SourcePane::ALL
            .into_iter()
            .find(|source| hash_key(&source.a11y_id()) == key)
        else {
            return false;
        };
        self.select_source(source);
        true
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

fn chip_is_selected(current: Resolution, chip: HunkChoice) -> bool {
    matches!(
        (current, chip),
        (Resolution::TookOurs | Resolution::Ours, HunkChoice::Ours)
            | (
                Resolution::TookTheirs | Resolution::Theirs,
                HunkChoice::Theirs
            )
            | (Resolution::TookBoth, HunkChoice::Both)
    )
}

/// The phone-width status label: only whether the hunk still needs a
/// resolution, so it stays distinct from the chips that say what they do.
const fn compact_status_label(res: Resolution) -> &'static str {
    match res {
        Resolution::Conflict => "Conflict",
        Resolution::Ours
        | Resolution::Theirs
        | Resolution::TookOurs
        | Resolution::TookTheirs
        | Resolution::TookBoth => "Resolved",
        Resolution::Unchanged => "",
    }
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
    // The header band's top edge, derived against the band itself, so the
    // raised header reads as a surface above the raised source columns.
    scene.fill(
        Fill::NonZero,
        transform,
        ui::surface::edge_on(c.surfaces.raised),
        None,
        &Rect::new(col.x0, col.y0, col.x1, col.y0 + 1.0),
    );
    // Accent underline keyed to the column role.
    let underline = Rect::new(col.x0, col.y1 - 2.0, col.x1, col.y1);
    scene.fill(Fill::NonZero, transform, accent, None, &underline);
    if !label.is_empty() {
        let (_, lh) = tr.measure(label, HEADER_FONT_SIZE, None);
        tr.draw_text(
            scene,
            label,
            HEADER_FONT_SIZE,
            c.text_primary,
            transform
                * Affine::translate((
                    col.x0 + PAD_X + 2.0,
                    col.y0 + (col.height() - f64::from(lh)) / 2.0,
                )),
            Some((col.width() - 2.0 * PAD_X) as f32),
        );
    }
}

/// One hunk's source lines, as syntax spans per side.
struct HunkSpans {
    base: Vec<Vec<Span>>,
    ours: Vec<Vec<Span>>,
    theirs: Vec<Vec<Span>>,
}

impl HunkSpans {
    /// Tokenize each side as one continuous file (hunks tile it in order),
    /// with the grammar its label names, then split the spans back per hunk.
    fn for_model(model: &MergeModel, [base, ours, theirs]: [&str; 3]) -> Vec<Self> {
        fn side(
            model: &MergeModel,
            label: &str,
            lines: fn(&MergeHunk) -> &Vec<String>,
        ) -> Vec<Vec<Vec<Span>>> {
            let grammar = highlight::grammar_for_label(label);
            let mut spans = highlight::document(
                &grammar,
                model
                    .hunks
                    .iter()
                    .flat_map(|hunk| lines(hunk).iter().map(String::as_str)),
            )
            .into_iter();
            model
                .hunks
                .iter()
                .map(|hunk| spans.by_ref().take(lines(hunk).len()).collect())
                .collect()
        }
        let base = side(model, base, |hunk| &hunk.base);
        let ours = side(model, ours, |hunk| &hunk.ours);
        let theirs = side(model, theirs, |hunk| &hunk.theirs);
        base.into_iter()
            .zip(ours)
            .zip(theirs)
            .map(|((base, ours), theirs)| Self { base, ours, theirs })
            .collect()
    }
}

/// Draw a list of code lines starting at `origin`, highlighted, one per
/// measured row pitch. `source` pairs the lines with their syntax spans.
fn draw_lines(
    ctx: &mut PaintCtx<'_>,
    origin: Point,
    (lines, spans): (&Vec<String>, &[Vec<Span>]),
    m: Metrics,
) {
    for (i, line) in lines.iter().enumerate() {
        let y = origin.y + i as f64 * m.row_h + m.text_dy;
        let line_spans = spans.get(i).map_or(&[][..], Vec::as_slice);
        highlight::draw_line(ctx, Point::new(origin.x, y), line, line_spans, FONT_SIZE);
    }
}
