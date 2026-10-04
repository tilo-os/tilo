//! Three-way merge view.
//!
//! Top band: three columns — base / ours / theirs — rendered hunk-by-hunk so a
//! conflict shows the same vertical span across all three. Every change takes
//! a status badge and a segmented resolution (take ours / theirs / both).
//! Bottom band: the merged result, mirrored from the Automerge `Text` so it is
//! the collaboratively-editable authoritative output. Choosing a resolution
//! re-composes the merged buffer and writes it into the CRDT.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::AutoCommit;
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::code_editor::Span;
use ui::widget::toggle_group::SegmentedStyle;
use ui::widget::{
    BadgeFace, ChipStyle, CodeEditor, ControlSize, EmptyState, EmptyStateContent, Language,
    SegmentedFace, ToggleGroup, Tone, Widget,
};

use crate::actions::{self, DiffmergeAction, HunkChoice, MergeResourceCtx};
use crate::doc::{MergeDoc, SharedMerge};
use crate::highlight;
use crate::i18n::{count, t, t_args};
use crate::model::{MergeHunk, MergeModel, Resolution};
use crate::pane::{self, CodeMetrics};
use crate::scroll::{HScroll, wheel_pixels};
use ui::icons::Icon;

/// Row metrics measured from the renderer the panel paints with.
///
/// Refreshed at the top of every draw; hunk spans, scrolling, resolution hit
/// rects, the accessibility export and the paint all read this one snapshot,
/// so a text-scale change moves rows, controls and the merged band together
/// instead of letting large glyphs overprint small rows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    code: CodeMetrics,
    /// Height of the segmented resolution control.
    strip_h: f64,
    /// Height of a status badge.
    badge_h: f64,
    /// Band the merged result's title sits in.
    merged_header_h: f64,
    /// Below this width three code panes cease to be readable. Keep one full
    /// source pane and make switching sources explicit instead of merely
    /// squeezing desktop geometry into a phone.
    breakpoint: f64,
    /// Space between a hunk's rows and its resolution controls, and after.
    gap: f64,
}

impl Metrics {
    fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let code = CodeMetrics::measure(tr, theme);
        let sp = &theme.tokens.spacing;
        let subheading_h = f64::from(
            tr.measure_style("Ag", None, &theme.text(TextRole::Subheading))
                .1,
        );
        Self {
            strip_h: SegmentedFace::height(ControlSize::Small, theme),
            badge_h: BadgeFace::height(theme),
            merged_header_h: (subheading_h + 2.0 * f64::from(sp.s2)).ceil(),
            breakpoint: f64::from(theme.tokens.metrics.compact_breakpoint),
            gap: f64::from(sp.s2),
            code,
        }
    }

    /// What is known before the first frame measures anything.
    fn unmeasured(theme: &Theme) -> Self {
        let code = CodeMetrics::unmeasured(theme);
        let sp = &theme.tokens.spacing;
        Self {
            strip_h: SegmentedFace::height(ControlSize::Small, theme),
            badge_h: BadgeFace::height(theme),
            merged_header_h: code.header_h,
            breakpoint: f64::from(theme.tokens.metrics.compact_breakpoint),
            gap: f64::from(sp.s2),
            code,
        }
    }

    /// The band a change hunk reserves under its rows for its status and
    /// resolution: side by side in the wide layout, stacked in the narrow.
    fn action_h(&self, compact: bool) -> f64 {
        let controls = if compact {
            self.badge_h + self.gap + self.strip_h
        } else {
            self.strip_h.max(self.badge_h)
        };
        self.gap + controls + self.gap
    }
}

/// The prefix of the accessibility target that switches the narrow layout's
/// source.
pub(crate) const SOURCE_TARGET: &str = "diffmerge.merge-source:";

/// The fewest lines of the merged result the source band leaves room for.
const MERGED_MIN_LINES: usize = 5;

/// One of the source revisions shown by the compact merge view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourcePane {
    Base,
    Ours,
    Theirs,
}

impl SourcePane {
    const ALL: [Self; 3] = [Self::Base, Self::Ours, Self::Theirs];

    fn label(self) -> String {
        match self {
            Self::Base => t("source-base"),
            Self::Ours => t("source-ours"),
            Self::Theirs => t("source-theirs"),
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Ours => "ours",
            Self::Theirs => "theirs",
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

    /// The colour that marks this source: the rule under its column header.
    fn provenance(self, theme: &Theme) -> Color {
        let s = &theme.tokens.colors.semantic;
        match self {
            Self::Base => theme.text_dim,
            Self::Ours => s.success,
            Self::Theirs => s.info,
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

/// A resolution segment's rect tagged with the hunk + side it takes.
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
    modifiers: ModifiersState,
    /// Horizontal scroll shared by the three source columns.
    across: HScroll,
    /// The widest source line in characters, which sets how far code scrolls.
    widest: usize,
    /// The hunk the next draw scrolls into view, once the pane has a size:
    /// the change under review when fresh inputs were loaded.
    reveal: Option<usize>,
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
    /// A conflict resolution the user just triggered, waiting to be drained by
    /// the parent app and dispatched as a [`DiffmergeAction`] through the
    /// shared action path (so a click and Ctrl+K produce one undo boundary).
    pending_action: Option<DiffmergeAction>,
    /// Last measured row metrics (see [`Metrics`]); an estimate until the
    /// first draw.
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
            modifiers: ModifiersState::empty(),
            across: HScroll::default(),
            widest: 0,
            reveal: None,
            dirty: true,
            pending_seed: false,
            hosted: false,
            seed_request: None,
            expected_content: None,
            merged_output: String::new(),
            pending_action: None,
            metrics: Metrics::unmeasured(&Theme::dark()),
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
            let adoptable = SharedMerge::read(doc).filter(|shared| {
                let mut model = shared.into_model();
                crate::doc::apply_resolutions(&mut model, doc);
                model.merged_text() == MergeDoc::merged_text(doc)
            });
            if let Some(shared) = adoptable {
                self.adopt_shared(&shared, doc);
            } else {
                // Missing shared projection, or a restored merged buffer that no
                // longer matches the three-way compose — drop actionable hunks
                // so a queued ResolveHunk cannot overwrite the restored output.
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

    fn adopt_shared(&mut self, shared: &SharedMerge, doc: &AutoCommit) {
        self.base_label.clone_from(&shared.base_label);
        self.ours_label.clone_from(&shared.ours_label);
        self.theirs_label.clone_from(&shared.theirs_label);
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

    /// Place the panel at `rect` directly (the parent app reserves the status
    /// bar below it and re-places the view whenever that band's measured
    /// height changes, so paint and layout agree within one frame).
    pub(crate) fn place(&mut self, rect: Rect) {
        if rect != self.rect {
            self.rect = rect;
            self.recompute_spans();
            self.scroll_y = self.scroll_y.min(self.max_scroll());
        }
    }

    /// `(row_h, header_h, strip_h)` from the last draw.
    #[cfg(test)]
    pub(crate) fn metrics_for_test(&self) -> (f64, f64, f64) {
        (
            self.metrics.code.row_h,
            self.metrics.code.header_h,
            self.metrics.strip_h,
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
        self.across.reset();
        self.active_hunk = self
            .first_conflict()
            .or_else(|| self.model.change_indices().first().copied());
        self.reveal = self.active_hunk;
        self.dirty = true;
        self.pending_seed = true;
    }

    pub fn model(&self) -> &MergeModel {
        &self.model
    }

    /// The change the reviewer is on, for the status bar.
    pub fn active_change(&self) -> Option<(usize, usize)> {
        let changes = self.model.change_indices();
        let active = self.active_hunk?;
        let position = changes.iter().position(|&c| c == active)?;
        Some((position + 1, changes.len()))
    }

    pub(crate) fn merged_editor_focused(&self) -> bool {
        self.merged_editor.is_focused()
    }

    fn is_compact(&self) -> bool {
        self.rect.width() < self.metrics.breakpoint
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
        let m = self.metrics;
        let inset = m.code.pad;
        let available = (self.rect.width() - 2.0 * inset).max(0.0);
        let width = available / SourcePane::ALL.len() as f64;
        let y0 = self.rect.y0 + (m.code.header_h - m.strip_h) / 2.0;
        SourcePane::ALL
            .iter()
            .enumerate()
            .map(|(index, source)| {
                let x0 = self.rect.x0 + inset + index as f64 * width;
                SourceHit {
                    rect: Rect::new(x0, y0, x0 + width, y0 + m.strip_h),
                    source: *source,
                }
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn compact_source_hits(&self) -> Vec<(Rect, String)> {
        self.source_selector_rects()
            .into_iter()
            .map(|hit| (hit.rect, hit.source.label()))
            .collect()
    }

    /// The resolution-segment hit rects captured by the last `draw`, each with
    /// the hunk and choice it dispatches. Lets a headless test click a segment
    /// through the real pointer path instead of calling `resolve` directly.
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
        let title = crate::doc::merge_title(&self.ours_label, &self.theirs_label);
        Some(MergeResourceCtx {
            title,
            model: self.model.clone(),
        })
    }

    /// Take the pending resolution, if any, so the parent app can dispatch it
    /// through the shared action path. Rising-edge: cleared on read.
    pub(crate) fn take_pending_action(&mut self) -> Option<DiffmergeAction> {
        self.pending_action.take()
    }

    /// The controls the last draw laid out, as accessibility nodes: the source
    /// switch of the narrow layout and every change's resolution. The root
    /// publishes them, so they are exposed once and act through the same
    /// action path as a click.
    pub(crate) fn access_controls(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut controls = Vec::new();
        if self.is_compact() {
            controls.extend(SourcePane::ALL.into_iter().map(|source| {
                AccessNode::new(Role::ToggleButton)
                    .with_name(format!("Show {} source", source.label()))
                    .with_description("Switch the visible merge source")
                    .with_action_target(format!("{SOURCE_TARGET}{}", source.tag()))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(source == self.selected_source),
                    )
            }));
        }
        controls.extend(self.action_hits.iter().map(|hit| {
            let choice = match hit.choice {
                HunkChoice::Ours => "ours",
                HunkChoice::Theirs => "theirs",
                HunkChoice::Both => "both",
            };
            let label = match hit.choice {
                HunkChoice::Ours => t("take-ours"),
                HunkChoice::Theirs => t("take-theirs"),
                HunkChoice::Both => t("take-both"),
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
        controls
    }

    /// Show the source an accessibility action names (`base`, `ours`,
    /// `theirs`). Returns whether it named one.
    pub(crate) fn select_source_tagged(&mut self, tag: &str) -> bool {
        let Some(source) = SourcePane::ALL.into_iter().find(|s| s.tag() == tag) else {
            return false;
        };
        self.select_source(source);
        true
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
    /// the shared action path — so a click and the Ctrl+K row produce the
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

    /// Scroll just far enough that the whole of `hunk`, with its resolution,
    /// shows in the band.
    fn reveal_hunk(&mut self, hunk: usize) {
        if let Some((top, bottom)) = self.hunk_spans.get(hunk).copied() {
            let band = self.source_band_height();
            let target = if top < self.scroll_y {
                top
            } else if bottom > self.scroll_y + band {
                bottom - band
            } else {
                self.scroll_y
            };
            self.scroll_y = target.max(0.0).min(self.max_scroll());
        }
    }

    fn scroll_hunk_into_view(&mut self, hunk: usize) {
        if let Some((top, bot)) = self.hunk_spans.get(hunk).copied() {
            let vh = self.source_band_height();
            let mid = f64::midpoint(top, bot);
            self.scroll_y = (mid - vh / 2.0).max(0.0).min(self.max_scroll());
        }
    }

    /// The height of the source band. It takes what the hunks need, so every
    /// change and its resolution show at once, and leaves the merged result
    /// at least [`MERGED_MIN_LINES`] lines. When the hunks need more than
    /// that, the band ends where the last hunk that fits ends, so what rests
    /// at the foot of the band is a whole hunk and not half a control.
    fn source_band_height(&self) -> f64 {
        let m = &self.metrics;
        let total = (self.rect.height() - m.code.header_h).max(0.0);
        let merged_min = m.merged_header_h + MERGED_MIN_LINES as f64 * m.code.row_h;
        let room = (total - merged_min).max(0.0);
        let content = self.content_height();
        if self.hunk_spans.is_empty() {
            return room.max(total / 2.0);
        }
        if content <= room {
            return content;
        }
        self.hunk_spans
            .iter()
            .map(|&(_, bottom)| bottom)
            .take_while(|&bottom| bottom <= room)
            .last()
            .unwrap_or(room)
    }

    /// The source columns as `(x0, x1)`: three side by side, or the one the
    /// narrow layout shows.
    fn source_columns(&self) -> Vec<(f64, f64)> {
        let r = self.rect;
        if self.is_compact() {
            return vec![(r.x0, r.x1)];
        }
        let col_w = r.width() / 3.0;
        (0..3)
            .map(|i| {
                let x0 = r.x0 + f64::from(i) * col_w;
                (x0, if i == 2 { r.x1 } else { x0 + col_w })
            })
            .collect()
    }

    /// Tell the horizontal scroll how wide the code is and how much of it
    /// shows.
    fn fit_across(&mut self) {
        let m = self.metrics;
        let col_w = self.rect.width() / self.source_columns().len().max(1) as f64;
        let viewport = (col_w - 2.0 * m.code.pad).max(0.0);
        let content = self.widest as f64 * m.code.cell_w + m.code.pad;
        self.across.set_extent(content, viewport);
    }

    fn merged_band_rect(&self) -> Rect {
        let r = self.rect;
        let band_top = r.y0 + self.metrics.code.header_h + self.source_band_height();
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
        self.widest = self
            .model
            .hunks
            .iter()
            .flat_map(|h| h.base.iter().chain(&h.ours).chain(&h.theirs))
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        let m = self.metrics;
        let compact = self.is_compact();
        let mut y = 0.0;
        for h in &self.model.hunks {
            let rows = h.base.len().max(h.ours.len()).max(h.theirs.len()).max(1);
            let mut height = rows as f64 * m.code.row_h;
            if h.is_change {
                height += m.action_h(compact);
            }
            self.hunk_spans.push((y, y + height));
            y += height;
        }
    }

    /// Column headers (base / ours / theirs) above the source band.
    fn draw_column_headers(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let r = self.rect;
        let m = self.metrics;
        if self.is_compact() {
            self.source_hits = self.source_selector_rects();
            let rects: Vec<Rect> = self.source_hits.iter().map(|hit| hit.rect).collect();
            let labels: Vec<String> = SourcePane::ALL.iter().map(|s| s.label()).collect();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            let selected = SourcePane::ALL
                .iter()
                .position(|s| *s == self.selected_source)
                .unwrap_or(0);
            ToggleGroup::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
                &SegmentedFace::new(&rects, &labels, selected)
                    .style(SegmentedStyle::Segmented)
                    .control_size(ControlSize::Small)
                    .hovered(rects.iter().position(|rect| rect.contains(self.cursor))),
            );
            return;
        }

        let col_w = r.width() / 3.0;
        for (index, (source, label)) in [
            (SourcePane::Base, &self.base_label),
            (SourcePane::Ours, &self.ours_label),
            (SourcePane::Theirs, &self.theirs_label),
        ]
        .into_iter()
        .enumerate()
        {
            let x0 = r.x0 + index as f64 * col_w;
            let band = Rect::new(x0, r.y0, x0 + col_w, r.y0 + m.code.header_h);
            pane::paint_header(ctx, band, label, Some(source.provenance(theme)));
        }
    }

    /// Vertical separators between the three source columns.
    fn draw_band_separators(&self, ctx: &mut PaintCtx<'_>) {
        if self.is_compact() {
            return;
        }
        let edge = ui::surface::edge_on(ctx.theme.tokens.colors.surfaces.content);
        let r = self.rect;
        let col_w = r.width() / 3.0;
        let band_top = r.y0 + self.metrics.code.header_h;
        let band_bot = band_top + self.source_band_height();
        for k in 1..3 {
            let x = r.x0 + f64::from(k) * col_w;
            ctx.fill_rect(Rect::new(x - 0.5, band_top, x + 0.5, band_bot), edge);
        }
    }

    /// The wash each side of a hunk takes for its resolution; `active` is the
    /// hunk under review, which takes the stronger wash of its hue.
    fn hunk_washes(
        resolution: Resolution,
        active: bool,
        theme: &Theme,
    ) -> (Option<Color>, Option<Color>) {
        let c = &theme.tokens.colors;
        let washed = |tone| pane::washed(c.surfaces.content, tone, active);
        match resolution {
            Resolution::Unchanged => (None, None),
            Resolution::Conflict => (
                Some(washed(c.semantic.danger)),
                Some(washed(c.semantic.danger)),
            ),
            Resolution::Ours | Resolution::TookOurs => (Some(washed(c.semantic.success)), None),
            Resolution::Theirs | Resolution::TookTheirs => (None, Some(washed(c.semantic.info))),
            Resolution::TookBoth => (
                Some(washed(c.semantic.success)),
                Some(washed(c.semantic.info)),
            ),
        }
    }

    /// Hunk tint per side. The hunk under review takes a stronger wash of its
    /// own hue; a side with no hue stays on the ground, since a neutral lift
    /// beside a coloured wash reads as an unrelated grey.
    fn draw_hunk_tint(
        ctx: &mut PaintCtx<'_>,
        block: Rect,
        compact_source: Option<SourcePane>,
        h: &MergeHunk,
        is_active: bool,
    ) {
        let active = is_active && h.is_change;
        let (ours_tint, theirs_tint) = Self::hunk_washes(h.resolution, active, ctx.theme);
        if let Some(source) = compact_source {
            let tint = match source {
                SourcePane::Base => None,
                SourcePane::Ours => ours_tint,
                SourcePane::Theirs => theirs_tint,
            };
            if let Some(tint) = tint {
                ctx.fill_rect(block, tint);
            }
            return;
        }
        let col_w = block.width() / 3.0;
        for (index, tint) in [None, ours_tint, theirs_tint].into_iter().enumerate() {
            if let Some(tint) = tint {
                let x0 = block.x0 + index as f64 * col_w;
                ctx.fill_rect(Rect::new(x0, block.y0, x0 + col_w, block.y1), tint);
            }
        }
    }

    /// Hunk `h`'s status badge, with its left edge at `x`, centred in the
    /// control band starting at `y`. Returns nothing for an unchanged hunk.
    fn draw_hunk_status(ctx: &mut PaintCtx<'_>, h: &MergeHunk, x: f64, y: f64, m: Metrics) {
        let Some((label, style)) = resolution_badge(h.resolution) else {
            return;
        };
        let width = BadgeFace::intrinsic_width(&label, ctx.text, ctx.theme);
        let rect = Rect::new(x, y, x + width, y + m.badge_h);
        BadgeFace::new(rect, &label).style(style).paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
        );
    }

    /// The take-ours / take-theirs / take-both segments, starting at `x0` and
    /// ending by `x_max`, in the band whose top is `y`. The explicit "Take …"
    /// labels are used wherever they fit; where they cannot the short form is
    /// used, so no segment paints past the pane edge. Returns the segments'
    /// hit targets.
    fn draw_hunk_resolution(
        ctx: &mut PaintCtx<'_>,
        (hi, h): (usize, &MergeHunk),
        (x0, x_max, y): (f64, f64, f64),
        m: Metrics,
        hover: Point,
    ) -> Vec<ActionHit> {
        let choices = [HunkChoice::Ours, HunkChoice::Theirs, HunkChoice::Both];
        let long = [t("take-ours"), t("take-theirs"), t("take-both")];
        let short = [
            t("take-ours-short"),
            t("take-theirs-short"),
            t("take-both-short"),
        ];
        let available = (x_max - x0).max(0.0);
        let mut labels = as_refs(&long);
        let mut width =
            SegmentedFace::intrinsic_width(&labels, ControlSize::Small, ctx.text, ctx.theme);
        if width > available {
            labels = as_refs(&short);
            width =
                SegmentedFace::intrinsic_width(&labels, ControlSize::Small, ctx.text, ctx.theme);
        }
        let width = width.min(available);
        let segment_w = width / 3.0;
        let rects: Vec<Rect> = (0..3u8)
            .map(|i| {
                let sx = x0 + f64::from(i) * segment_w;
                Rect::new(sx, y, sx + segment_w, y + m.strip_h)
            })
            .collect();
        let selected = choices
            .iter()
            .position(|choice| chip_is_selected(h.resolution, *choice))
            .unwrap_or(usize::MAX);
        ToggleGroup::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &SegmentedFace::new(&rects, &labels, selected)
                .style(SegmentedStyle::Segmented)
                .control_size(ControlSize::Small)
                .hovered(rects.iter().position(|rect| rect.contains(hover))),
        );
        rects
            .into_iter()
            .zip(choices)
            .map(|(rect, choice)| ActionHit {
                rect,
                hunk: hi,
                choice,
            })
            .collect()
    }

    /// Draw every hunk row in the source band: side tints, the three code
    /// columns and (for change hunks) the status badge and resolution.
    /// Populates `self.action_hits` for the next click hit-test.
    fn draw_hunks(&mut self, ctx: &mut PaintCtx<'_>) {
        let r = self.rect;
        let m = self.metrics;
        let col_w = r.width() / 3.0;
        let compact = self.is_compact();
        let compact_source = compact.then_some(self.selected_source);
        let band_top = r.y0 + m.code.header_h;
        let band_bot = band_top + self.source_band_height();
        let action_h = m.action_h(compact);

        for (hi, h) in self.model.hunks.iter().enumerate() {
            // `hunk_spans` is rebuilt one-for-one with `model.hunks` in
            // `recompute_spans`; skip drawing rather than panic if that ever
            // drifts out of sync.
            let Some(&(top, _bot)) = self.hunk_spans.get(hi) else {
                continue;
            };
            let y = band_top + top - self.scroll_y;
            let rows = h.base.len().max(h.ours.len()).max(h.theirs.len()).max(1);
            let block_h = rows as f64 * m.code.row_h;

            // Cull off-screen hunks cheaply.
            if y + block_h + action_h < band_top || y > band_bot {
                continue;
            }

            let block = Rect::new(r.x0, y, r.x1, y + block_h);
            Self::draw_hunk_tint(ctx, block, compact_source, h, Some(hi) == self.active_hunk);

            // Phones show one source at a time; tablets and desktops keep the
            // aligned three-column comparison.
            let spans = self.source_spans.get(hi);
            let side = |pick: fn(&HunkSpans) -> &[Vec<Span>]| spans.map_or(&[][..], pick);
            let (base, ours, theirs) = (
                (&h.base, side(|s| &s.base)),
                (&h.ours, side(|s| &s.ours)),
                (&h.theirs, side(|s| &s.theirs)),
            );
            let pad = m.code.pad;
            let across = self.across.offset();
            let sources = match compact_source {
                Some(source) => vec![(source, r.x0, r.x1)],
                None => vec![
                    (SourcePane::Base, r.x0, r.x0 + col_w),
                    (SourcePane::Ours, r.x0 + col_w, r.x0 + 2.0 * col_w),
                    (SourcePane::Theirs, r.x0 + 2.0 * col_w, r.x1),
                ],
            };
            for (source, x0, x1) in sources {
                let lines = match source {
                    SourcePane::Base => base,
                    SourcePane::Ours => ours,
                    SourcePane::Theirs => theirs,
                };
                let view = Rect::new(x0 + pad, y, x1 - pad, y + block_h);
                ctx.scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    ctx.transform,
                    &view,
                );
                draw_lines(ctx, Point::new(view.x0 - across, y), lines, m);
                ctx.scene.pop_layer();
            }

            // Status and resolution for change hunks.
            if h.is_change {
                let band_y = block.y1 + m.gap;
                let (status_x, status_y, strip_x, strip_y) = if compact {
                    (r.x0 + pad, band_y, r.x0 + pad, band_y + m.badge_h + m.gap)
                } else {
                    let centre = |height: f64| band_y + (m.strip_h.max(m.badge_h) - height) / 2.0;
                    (
                        r.x0 + col_w + pad,
                        centre(m.badge_h),
                        r.x0 + 2.0 * col_w + pad,
                        centre(m.strip_h),
                    )
                };
                Self::draw_hunk_status(ctx, h, status_x, status_y, m);
                let hits = Self::draw_hunk_resolution(
                    ctx,
                    (hi, h),
                    (strip_x, r.x1 - pad, strip_y),
                    m,
                    self.cursor,
                );
                self.action_hits.extend(hits);
            }
        }
    }

    /// The merged-result band: its title with the conflict count, and the
    /// scrolled merged buffer body.
    fn draw_merged_band(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let m = self.metrics;
        let merged = self.merged_band_rect();
        // The merged header stands on the content ground, closed by a hairline.
        let header = Rect::new(
            merged.x0,
            merged.y0,
            merged.x1,
            merged.y0 + m.merged_header_h,
        );
        let pad = m.code.pad;
        let title = t("merged-title");
        let title_style = theme.text(TextRole::Subheading);
        let (title_w, _) = ctx.text.measure_style(&title, None, &title_style);
        ctx.text_role_in_band(
            &title,
            TextRole::Subheading,
            c.text_primary,
            header.x0 + pad,
            header,
            None,
        );
        if self.expected_content.is_some() {
            let conflicts = self.model.conflict_count();
            let (label, tone) = if conflicts == 0 {
                (t("merged-resolved"), Tone::Success)
            } else {
                (
                    t_args("merged-conflicts", &[("count", count(conflicts))]),
                    Tone::Danger,
                )
            };
            let width = BadgeFace::intrinsic_width(&label, ctx.text, theme);
            let x = header.x0 + pad + f64::from(title_w) + m.gap * 2.0;
            let rect = Rect::from_center_size(
                Point::new(x + width / 2.0, header.center().y),
                (width, m.badge_h),
            );
            BadgeFace::new(rect, &label)
                .style(ChipStyle::Tinted(tone))
                .paint(ctx.scene, ctx.transform, ctx.text, theme);
        }
        ui::surface::separator(
            ctx.scene,
            ctx.transform,
            header.x0,
            header.x1,
            header.y1,
            0.0,
            c.surfaces.content,
        );

        // Merged buffer body. The shared editor owns text, caret, selection,
        // scrolling, clipboard, IME, accessibility, and CRDT splices.
        let merged_body = Rect::new(merged.x0, header.y1, merged.x1, merged.y1);
        self.merged_editor.set_rect(merged_body);
        self.merged_editor
            .draw_embedded(ctx.scene, ctx.transform, ctx.text, theme);
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
            merged.y0 + self.metrics.merged_header_h,
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
        // One metric snapshot per frame: spans, scroll clamp, controls and paint
        // all follow the renderer's actual line height.
        let measured = Metrics::measure(tr, theme);
        if measured != self.metrics {
            self.metrics = measured;
            self.recompute_spans();
            self.scroll_y = self.scroll_y.min(self.max_scroll());
        }
        self.fit_across();
        if self.rect.height() > 0.0
            && let Some(hunk) = self.reveal.take()
        {
            self.reveal_hunk(hunk);
        }
        let r = self.rect;
        let m = self.metrics;
        let mut ctx = PaintCtx::new(scene, transform, tr, theme);

        self.draw_column_headers(&mut ctx);
        self.draw_band_separators(&mut ctx);

        let band_top = r.y0 + self.metrics.code.header_h;
        let band_bot = band_top + self.source_band_height();
        let band_clip = Rect::new(r.x0, band_top, r.x1, band_bot);
        ui::surface::separator(
            ctx.scene,
            ctx.transform,
            r.x0,
            r.x1,
            band_top,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &band_clip,
        );
        if self.expected_content.is_none() && !self.merged_output.is_empty() {
            let title = t("merged-unavailable");
            let content = EmptyStateContent {
                icon: Some(Icon::TriangleAlert),
                ..EmptyStateContent::new(&title)
            };
            EmptyState::paint(&mut ctx, band_clip, &content, Point::new(-1.0, -1.0));
        } else {
            self.draw_hunks(&mut ctx);
            let tracks: Vec<Rect> = self
                .source_columns()
                .into_iter()
                .map(|(x0, x1)| {
                    let view = Rect::new(x0 + m.code.pad, band_top, x1 - m.code.pad, band_bot);
                    HScroll::track_in(view, theme)
                })
                .collect();
            self.across.draw(&mut ctx, &tracks);
        }
        ctx.scene.pop_layer();

        // Divider between source band and merged band.
        ui::surface::separator(
            ctx.scene,
            ctx.transform,
            r.x0,
            r.x1,
            band_bot,
            0.0,
            theme.tokens.colors.surfaces.content,
        );

        self.draw_merged_band(&mut ctx);
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
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let mut moved = false;
                if self.across.pointer(event, self.cursor, &mut moved) {
                    self.dirty |= moved;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let mut moved = false;
                if self.across.pointer(event, self.cursor, &mut moved) {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) =
                    wheel_pixels(*delta, self.metrics.code.row_h, self.modifiers.shift_key());
                self.scroll_by(dy);
                if self.across.scroll_by(dx) {
                    self.dirty = true;
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let mut moved = false;
                if self.across.pointer(event, self.cursor, &mut moved) {
                    self.dirty |= moved;
                    *ctx.handled = true;
                    return;
                }
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
        use ui::a11y::{AccessNode, Role};
        let description = if self.is_compact() {
            "One source is visible at a time. Use Left and Right Arrow to switch source."
        } else {
            "Base, ours, and theirs are shown side by side."
        };
        Some(
            AccessNode::new(Role::Group)
                .with_name("Three-way merge")
                .with_description(description),
        )
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

/// The status badge of a hunk's resolution: what happened to it, in the tone
/// that says how it stands. An unchanged hunk has none.
fn resolution_badge(res: Resolution) -> Option<(String, ChipStyle)> {
    let (key, style) = match res {
        Resolution::Conflict => ("hunk-conflict", ChipStyle::Tinted(Tone::Danger)),
        Resolution::Ours => ("hunk-auto-ours", ChipStyle::Neutral),
        Resolution::Theirs => ("hunk-auto-theirs", ChipStyle::Neutral),
        Resolution::TookOurs => ("hunk-took-ours", ChipStyle::Tinted(Tone::Success)),
        Resolution::TookTheirs => ("hunk-took-theirs", ChipStyle::Tinted(Tone::Success)),
        Resolution::TookBoth => ("hunk-took-both", ChipStyle::Tinted(Tone::Success)),
        Resolution::Unchanged => return None,
    };
    Some((t(key), style))
}

/// Three owned labels as the borrowed slice a segmented face takes.
fn as_refs(labels: &[String; 3]) -> [&str; 3] {
    [labels[0].as_str(), labels[1].as_str(), labels[2].as_str()]
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
        let y = origin.y + i as f64 * m.code.row_h + m.code.text_dy;
        let line_spans = spans.get(i).map_or(&[][..], Vec::as_slice);
        highlight::draw_line(ctx, Point::new(origin.x, y), line, line_spans);
    }
}
