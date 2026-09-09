//! Root widget — orchestrates the diff and merge modes, the top toolbar
//! (mode switch, change navigation, open), and the host-portal file open.
//!
//! Data flow:
//! * The app holds the raw input texts in memory and the two derived views
//!   ([`DiffView`], [`MergeView`]).
//! * The toolbar's buttons are drawn directly and hit-tested in `handle_event`.
//! * Opening files goes through the **host** file chooser (the XDG Desktop
//!   Portal, [`ui::portal`]) — the app never draws its own picker (HIG §5).
//!   The chosen paths are read and re-seed the active mode (two files → diff;
//!   the merge demo stays sample-driven since a three-way pick needs three
//!   roots).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, SegmentedFace, ToggleGroup, Widget};

use crate::actions::DiffmergeOperation;
use crate::diff_view::DiffView;
use crate::merge_view::MergeView;
use crate::samples;

const TOOLBAR_H: f64 = 48.0;

fn diff_open_request() -> PickRequest {
    PickRequest::open("Open a file to compare", "Open")
        .thread_name("diffmerge-portal-open")
        .filter(
            FileFilter::new("Text & source")
                .mimetype("text/plain")
                .glob("*.txt")
                .glob("*.md")
                .glob("*.rs")
                .glob("*.toml")
                .glob("*.json")
                .glob("*.yaml")
                .glob("*.yml")
                .glob("*.c")
                .glob("*.h")
                .glob("*.py")
                .glob("*.js")
                .glob("*.ts"),
        )
        .all_files()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Diff,
    Merge,
}

/// A toolbar button's hit rect tagged with the action it triggers.
#[derive(Clone, Copy)]
struct ToolHit {
    rect: Rect,
    action: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    ModeDiff,
    ModeMerge,
    PrevChange,
    NextChange,
    Open,
    LoadSample,
}

pub struct DiffMergeApp {
    mode: Mode,
    diff: DiffView,
    merge: MergeView,
    rect: Rect,
    diff_node: Option<NodeId>,
    merge_node: Option<NodeId>,
    tool_hits: Vec<ToolHit>,
    cursor: Point,
    /// An in-flight XDG-portal file-open. When the user asks to compare files
    /// (the toolbar "Open files" button) the portal round-trip runs on a
    /// worker thread and hands the chosen path back over this channel, drained
    /// once per frame in [`Widget::sync`]. `None` when no picker is in flight —
    /// the app never draws its own picker (HIG §5).
    portal_rx: Option<PortalChannel>,
    /// Files chosen but not yet paired (first of a two-file diff selection).
    pending_left: Option<(String, String)>,
    /// A human-readable reason the last host-portal open failed, surfaced in
    /// the status bar. `None` in the normal (success / cancel) case.
    load_error: Option<String>,
    resource_invocation: Option<qos_actions::InvocationId>,
    pending_resource_paths: Option<(
        qos_actions::InvocationId,
        std::path::PathBuf,
        std::path::PathBuf,
    )>,
    resource_result: Option<qos_actions::InvocationUpdate>,
    dirty: bool,
    /// One-shot flag: set when the active mode changed so the host rebuilds the
    /// taffy tree (the flex content child swaps between the two views).
    structure_dirty: bool,
    /// Framework-owned action dispatch: every on-panel affordance routes its
    /// [`DiffmergeAction`] through here so it brackets **one** undo boundary,
    /// encodes under [`DIFFMERGE_ACTION_TYPE`](crate::actions::DIFFMERGE_ACTION_TYPE),
    /// and replays byte-identically on peers — the same path Ctrl+K takes.
    actions: ui::ActionHost,
    /// The diff/merge summary, surfaced as a transient floating bubble
    /// (see [`Self::draw_status_bubble`]) instead of a permanent bottom strip.
    /// Poked only when the summary text actually changes, or held open while
    /// the pointer hovers the bottom-right reveal zone; fades otherwise.
    status_bubble: ui::StatusBubble,
    /// Last computed bounds of the transient summary. Keeping this explicit
    /// makes the narrow-width geometry testable as well as ensuring hover
    /// reveal uses exactly the visible target.
    status_bubble_rect: Rect,
    /// The last summary string shown in the bubble. The bubble is poked only on
    /// the transition to a *different* string, never every frame.
    last_status: String,
    /// `--shots`: forces the status bubble fully visible for the one headless
    /// frame (there is no live pointer to hover the reveal zone).
    shots_status: bool,
    /// The custom-painted toolbar exposes this focus state to both the shared
    /// focus ring and the accessibility tree. It intentionally stores the
    /// semantic action, not a draw-order index, so responsive layout never
    /// moves focus to a different operation.
    toolbar_focus: Option<Action>,
}

impl DiffMergeApp {
    /// The live app: starts EMPTY (blank panes + the "Open files" / "Load
    /// sample" toolbar actions), never a fabricated diff.
    pub fn new() -> Self {
        Self::build(false)
    }

    /// The hermetic `--shots` / demo build: seeded with a sample diff + a
    /// three-way merge conflict so the headless screenshot is populated. This
    /// is the ONLY place the sample content is used on construction.
    pub fn new_demo() -> Self {
        Self::build(true)
    }

    /// Deterministic visual-review states. These are intentionally only used
    /// by the offscreen launcher: the live app always opens empty until the
    /// user chooses local files or a sample.
    pub fn for_shot(scenario: Option<&str>) -> Self {
        let mut app = match scenario {
            Some("empty") => Self::new(),
            _ => Self::new_demo(),
        };
        match scenario {
            Some("diff") => {
                app.mode = Mode::Diff;
                app.structure_dirty = true;
            }
            Some("error") => {
                app.mode = Mode::Diff;
                app.load_error = Some(
                    "Couldn’t read the selected files. Check permissions and try again.".into(),
                );
                app.structure_dirty = true;
                app.arm_status_bubble_for_shots();
            }
            Some("status") => app.arm_status_bubble_for_shots(),
            _ => {}
        }
        app
    }

    fn build(demo: bool) -> Self {
        let mut diff = DiffView::new();
        let mut merge = MergeView::new();
        if demo {
            let (l, r) = samples::diff_pair();
            diff.set_inputs(&l.label, &l.text, &r.label, &r.text);
            let (base, ours, theirs) = samples::merge_triple();
            merge.set_inputs(
                &base.label,
                &base.text,
                &ours.label,
                &ours.text,
                &theirs.label,
                &theirs.text,
            );
        }

        // The demo opens on the three-way merge to immediately show a conflict;
        // the live app opens on the side-by-side diff (empty "open files"
        // state). `DIFFMERGE_MODE` overrides either way.
        let mode = match std::env::var("DIFFMERGE_MODE").ok().as_deref() {
            Some("diff") => Mode::Diff,
            Some("merge") => Mode::Merge,
            _ if demo => Mode::Merge,
            _ => Mode::Diff,
        };

        Self {
            mode,
            diff,
            merge,
            rect: Rect::ZERO,
            diff_node: None,
            merge_node: None,
            tool_hits: Vec::new(),
            cursor: Point::ZERO,
            portal_rx: None,
            pending_left: None,
            load_error: None,
            resource_invocation: None,
            pending_resource_paths: None,
            resource_result: None,
            dirty: true,
            structure_dirty: false,
            actions: ui::ActionHost::new(),
            status_bubble: ui::StatusBubble::new(),
            status_bubble_rect: Rect::ZERO,
            last_status: String::new(),
            shots_status: false,
            toolbar_focus: None,
        }
    }

    /// `--shots`: force the floating status bubble fully visible so a single
    /// headless frame captures it (no live pointer hovers the reveal zone).
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(std::time::Instant::now());
    }

    fn content_rect(&self) -> Rect {
        // Content spans to the window bottom; the summary floats as a
        // transient bubble (see `draw_status_bubble`), not a bottom strip.
        Rect::new(
            self.rect.x0,
            self.rect.y0 + TOOLBAR_H,
            self.rect.x1,
            self.rect.y1,
        )
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::ModeDiff => {
                if self.mode != Mode::Diff {
                    self.mode = Mode::Diff;
                    self.structure_dirty = true;
                }
                self.dirty = true;
            }
            Action::ModeMerge => {
                if self.mode != Mode::Merge {
                    self.mode = Mode::Merge;
                    self.structure_dirty = true;
                }
                self.dirty = true;
            }
            Action::PrevChange => {
                match self.mode {
                    Mode::Diff => {
                        self.diff.navigate(false);
                    }
                    Mode::Merge => {
                        self.merge.navigate(false);
                    }
                }
                self.dirty = true;
            }
            Action::NextChange => {
                match self.mode {
                    Mode::Diff => {
                        self.diff.navigate(true);
                    }
                    Mode::Merge => {
                        self.merge.navigate(true);
                    }
                }
                self.dirty = true;
            }
            Action::Open => {
                self.request_open_via_portal();
            }
            Action::LoadSample => {
                let (l, r) = samples::diff_pair();
                self.diff.set_inputs(&l.label, &l.text, &r.label, &r.text);
                let (base, ours, theirs) = samples::merge_triple();
                self.merge.set_inputs(
                    &base.label,
                    &base.text,
                    &ours.label,
                    &ours.text,
                    &theirs.label,
                    &theirs.text,
                );
                self.pending_left = None;
                self.dirty = true;
            }
        }
    }

    /// Dispatch one [`DiffmergeAction`](crate::actions::DiffmergeAction) — the
    /// single CRDT-mutation path shared with the Ctrl+K palette. The action is
    /// applied through the qOS action executor (a pinned actor + committed
    /// change, so it replays byte-identically on peers), exactly as the palette
    /// does. The document heads captured just before the first action of the
    /// event are stashed so the host records **one** undo boundary bracketing it
    /// ([`Widget::take_committed_action`]). The in-memory merge model was already
    /// updated by the chip handler for immediate feedback and is reconciled from
    /// the document on the next `sync`.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &crate::actions::DiffmergeAction) {
        self.actions.dispatch_with_context(
            doc,
            crate::actions::DIFFMERGE_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Diff", "Diff", action),
        );
    }

    /// The last action dispatched, for the `affordance == ActionSpec` covenant
    /// test.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The presentation context Ctrl+K needs to build the merge resource tree.
    /// `None` unless a three-way merge with at least one change hunk is the
    /// active mode — the diff mode is read-only and mutates nothing.
    pub(crate) fn merge_resource_ctx(&self) -> Option<crate::actions::MergeResourceCtx> {
        if self.mode != Mode::Merge {
            return None;
        }
        self.merge.resource_ctx()
    }

    pub(crate) fn diff_resource_ctx(&self) -> Option<crate::actions::DiffResourceCtx> {
        (self.mode == Mode::Diff)
            .then(|| self.diff.resource_ctx())
            .flatten()
    }

    /// Ask the host desktop's file chooser to open a file to compare. Per HIG
    /// §5 the app never draws its own picker: the portal round-trip runs on a
    /// worker thread ([`ui::portal::PickRequest`]) and the chosen path
    /// comes back over a channel drained each frame in [`Self::drain_portal`].
    /// A diff needs two files, so the drain re-fires this once for the right
    /// file after the left is picked. A second request while one is in flight
    /// is ignored, so a double-click can't stack two host dialogs.
    fn request_open_via_portal(&mut self) {
        if self.portal_rx.is_some() {
            return;
        }
        self.load_error = None;
        self.portal_rx = Some(diff_open_request().send());
        self.dirty = true;
    }

    /// Drain a completed portal request, if any. Called once per frame from
    /// [`Widget::sync`]. Loads the chosen file (stashing the first of the pair
    /// and re-opening the chooser for the second), records a failure for the
    /// status bar, and clears the in-flight channel. Never blocks — a still
    /// pending picker leaves the channel installed for the next frame.
    fn drain_portal(&mut self) {
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    PortalOutcome::Opened(path) => {
                        // A `true` return means the left file just landed and
                        // the right one is still needed — fire a fresh request.
                        match self.consume_pick(&path) {
                            Ok(true) => self.request_open_via_portal(),
                            Ok(false) => {
                                if let Some(invocation) = self.resource_invocation.take() {
                                    self.resource_result =
                                        Some(qos_actions::InvocationUpdate::Succeeded {
                                            invocation,
                                            output: qos_actions::ActionOutput::default(),
                                        });
                                }
                            }
                            Err(reason) => {
                                self.load_error = Some(reason.clone());
                                if let Some(invocation) = self.resource_invocation.take() {
                                    self.resource_result =
                                        Some(qos_actions::InvocationUpdate::Failed {
                                            invocation,
                                            error: qos_actions::ActionFailure {
                                                code: "diffmerge.open_failed".into(),
                                                message: reason,
                                                details: None,
                                                retryable: false,
                                            },
                                        });
                                }
                            }
                        }
                        self.dirty = true;
                    }
                    // A cancel abandons an in-progress two-file selection.
                    PortalOutcome::Cancelled => {
                        self.pending_left = None;
                        if let Some(invocation) = self.resource_invocation.take() {
                            self.resource_result = Some(qos_actions::InvocationUpdate::Cancelled {
                                invocation,
                                message: Some("file selection cancelled".into()),
                            });
                        }
                        self.dirty = true;
                    }
                    PortalOutcome::Failed(reason) => {
                        self.pending_left = None;
                        self.load_error = Some(format!("Couldn't open the file chooser: {reason}"));
                        if let Some(invocation) = self.resource_invocation.take() {
                            self.resource_result = Some(qos_actions::InvocationUpdate::Failed {
                                invocation,
                                error: qos_actions::ActionFailure {
                                    code: "diffmerge.open_failed".into(),
                                    message: reason,
                                    details: None,
                                    retryable: false,
                                },
                            });
                        }
                        self.dirty = true;
                    }
                }
            }
            // Still waiting on the host dialog, or the worker vanished.
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
                            code: "diffmerge.portal_disconnected".into(),
                            message: "file picker disconnected".into(),
                            details: None,
                            retryable: false,
                        },
                    });
                }
            }
        }
    }

    /// Consume one portal-chosen path. The first file of a pair is stashed and
    /// `true` is returned so the caller re-opens the chooser for the second;
    /// the second file completes the diff and switches to Diff mode, returning
    /// `false`. Pure w.r.t. the portal (reads the file, mutates model state) so
    /// the two-file flow is unit-testable without a host dialog.
    fn consume_pick(&mut self, path: &Path) -> Result<bool, String> {
        let label = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().to_string(),
        );
        let content = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        match self.pending_left.take() {
            None => {
                // First file picked — stash and ask for the second.
                self.pending_left = Some((label, content));
                Ok(true)
            }
            Some((left_label, left_content)) => {
                self.diff
                    .set_inputs(&left_label, &left_content, &label, &content);
                self.mode = Mode::Diff;
                self.structure_dirty = true;
                Ok(false)
            }
        }
    }

    fn load_paths(&mut self, left: &Path, right: &Path) -> Result<(), String> {
        let left_content =
            fs::read_to_string(left).map_err(|e| format!("{}: {e}", left.display()))?;
        let right_content =
            fs::read_to_string(right).map_err(|e| format!("{}: {e}", right.display()))?;
        let label = |path: &Path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        self.diff
            .set_inputs(&label(left), &left_content, &label(right), &right_content);
        self.mode = Mode::Diff;
        self.structure_dirty = true;
        self.dirty = true;
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "toolbar geometry, paint, focus ring, and matching hit targets stay together"
    )]
    fn draw_toolbar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.tool_hits.clear();
        let c = &theme.tokens.colors;
        let r = self.rect;
        // A single toolbar row remains deliberately compact on phones. Keep
        // both modes and every action reachable, but trade desktop prose for
        // familiar short labels so controls never paint over each other.
        let compact = r.width() < 640.0;
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOOLBAR_H);
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(
                Point::new(r.x0, r.y0 + TOOLBAR_H),
                Point::new(r.x1, r.y0 + TOOLBAR_H),
            ),
        );

        // Mode segmented control (shared pill-style segmented control). The OS
        // titlebar carries the app name, so no self-drawn wordmark here — the
        // mode control claims the reclaimed left slot.
        let seg_y = r.y0 + 10.0;
        let seg_h = 28.0;
        let mode_labels: [&str; 2] = if compact {
            ["Diff", "Merge"]
        } else {
            ["Diff", "3-way merge"]
        };
        let mode_actions = [Action::ModeDiff, Action::ModeMerge];
        let selected = usize::from(self.mode == Mode::Merge);
        let mut mode_rects: Vec<Rect> = Vec::with_capacity(mode_labels.len());
        let mut x = r.x0 + if compact { 8.0 } else { 16.0 };
        for label in mode_labels {
            let w = label.len() as f64 * if compact { 7.0 } else { 7.4 }
                + if compact { 18.0 } else { 22.0 };
            mode_rects.push(Rect::new(x, seg_y, x + w, seg_y + seg_h));
            x += w + if compact { 4.0 } else { 6.0 };
        }
        let hovered = mode_rects.iter().position(|rr| rr.contains(self.cursor));
        ToggleGroup::paint(
            scene,
            transform,
            tr,
            theme,
            &SegmentedFace::new(&mode_rects, &mode_labels, selected)
                .pill()
                .hovered(hovered)
                .radius(6.0),
        );
        for (rect, action) in mode_rects.iter().zip(mode_actions) {
            self.tool_hits.push(ToolHit {
                rect: *rect,
                action,
            });
        }
        if self
            .toolbar_focus
            .is_some_and(|action| matches!(action, Action::ModeDiff | Action::ModeMerge))
        {
            let focused = if self.toolbar_focus == Some(Action::ModeDiff) {
                mode_rects.first().copied()
            } else {
                mode_rects.get(1).copied()
            };
            if let Some(focused) = focused {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.border_focus,
                    None,
                    &RoundedRect::from_rect(focused, 6.0),
                );
            }
        }

        // Right-aligned action buttons.
        let mut rx = r.x1 - if compact { 8.0 } else { 16.0 };
        let actions: &[(&str, Action)] = if compact {
            &[
                ("Open", Action::Open),
                ("Sample", Action::LoadSample),
                ("›", Action::NextChange),
                ("‹", Action::PrevChange),
            ]
        } else {
            &[
                ("Open files", Action::Open),
                ("Load sample", Action::LoadSample),
                ("Next ›", Action::NextChange),
                ("‹ Prev", Action::PrevChange),
            ]
        };
        for (label, action) in actions {
            let w = label.len() as f64 * if compact { 6.8 } else { 7.2 }
                + if compact { 16.0 } else { 20.0 };
            let btn = Rect::new(rx - w, seg_y, rx, seg_y + seg_h);
            Button::paint(
                scene,
                transform,
                tr,
                theme,
                &ButtonFace::new(btn, label)
                    .radius(6.0)
                    .text_size(12.5)
                    .weight(TextWeight::Regular)
                    .focused(self.toolbar_focus == Some(*action))
                    .hovered(btn.contains(self.cursor)),
            );
            self.tool_hits.push(ToolHit {
                rect: btn,
                action: *action,
            });
            rx -= w + if compact { 4.0 } else { 8.0 };
        }
    }

    /// The one-line summary the status bubble shows: a host-portal load error
    /// takes over until the next successful open clears it, otherwise the
    /// diff-mode change counts or the merge-mode hunk/conflict counts.
    fn status_text(&self) -> String {
        if let Some(err) = &self.load_error {
            return err.clone();
        }
        match self.mode {
            Mode::Diff => {
                let m = self.diff.model();
                let active = self
                    .diff
                    .active_block()
                    .map(|b| format!("  ·  change {}/{}", b + 1, m.blocks.len().max(1)))
                    .unwrap_or_default();
                format!(
                    "+{} additions   −{} deletions   ~{} modifications   {} change block(s){}",
                    m.added,
                    m.removed,
                    m.modified,
                    m.blocks.len(),
                    active
                )
            }
            Mode::Merge => {
                let m = self.merge.model();
                format!(
                    "{} change hunk(s)   {} resolved   {} conflict(s) remaining",
                    m.change_count(),
                    m.resolved_count(),
                    m.conflict_count(),
                )
            }
        }
    }

    /// A deliberately compact status for a phone-width viewport. The full
    /// detail remains in the accessible status description; this label is only
    /// the painted transient pill and must never escape its viewport.
    fn compact_status_text(&self) -> String {
        if self.load_error.is_some() {
            return "Couldn’t open files".into();
        }
        match self.mode {
            Mode::Diff => {
                let model = self.diff.model();
                format!(
                    "+{}  −{}  ~{}  ·  {} changes",
                    model.added,
                    model.removed,
                    model.modified,
                    model.blocks.len()
                )
            }
            Mode::Merge => {
                let model = self.merge.model();
                format!(
                    "{} hunks · {} conflicts",
                    model.change_count(),
                    model.conflict_count()
                )
            }
        }
    }

    /// Surfaces on a real change to the summary (poked only when the string
    /// differs) or while the pointer hovers the bottom-right reveal band, then
    /// fades.
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = std::time::Instant::now();
        let raw_label = self.status_text();
        // Poke only on a genuine change to the summary, never every frame.
        if raw_label != self.last_status {
            self.status_bubble.poke(now);
            self.last_status.clone_from(&raw_label);
        }

        let text_size = 12.0f32;
        let margin = 14.0;
        let r = self.rect;
        let preferred = if r.width() < 640.0 {
            self.compact_status_text()
        } else {
            raw_label
        };
        // The pill owns 14px horizontal padding on either side. Elide before
        // measuring so its intrinsic text width can never put its left edge
        // beyond the viewport (the original narrow-layout regression).
        let text_budget = (r.width() - 2.0 * margin - 28.0).max(0.0) as f32;
        let label = tr.elide_to_width(&preferred, text_size, text_budget);
        let (w, h) = ui::status_bubble::pill_size(&label, tr, text_size, 0.0);
        let rect = Rect::new(
            r.x1 - margin - w,
            r.y1 - margin - h,
            r.x1 - margin,
            r.y1 - margin,
        );
        self.status_bubble_rect = rect;

        // Reveal zone: the bottom-right corner band (plus the pill), so nudging
        // the pointer there re-surfaces the summary on demand.
        let zone = Rect::new(rect.x0 - 40.0, rect.y0 - 24.0, r.x1, r.y1);
        let over = self.shots_status || zone.contains(self.cursor);
        self.status_bubble.set_hover(over, now);

        let alpha = self.status_bubble.alpha(now);
        if alpha <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            alpha,
            transform,
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, transform, tr, theme, rect, &label, text_size);
        scene.pop_layer();
    }

    #[cfg(test)]
    fn status_bubble_rect(&self) -> Rect {
        self.status_bubble_rect
    }

    /// Keyboard shortcuts: F8/F7 or n/p for next/prev change, 1/2 for mode.
    /// Returns `true` if the event was consumed.
    fn handle_keyboard_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };

        if matches!(logical_key, Key::Named(NamedKey::Tab)) {
            const TOOLBAR_ORDER: [Action; 6] = [
                Action::ModeDiff,
                Action::ModeMerge,
                Action::PrevChange,
                Action::NextChange,
                Action::LoadSample,
                Action::Open,
            ];
            let next = if let Some(current) = self
                .toolbar_focus
                .and_then(|focused| TOOLBAR_ORDER.iter().position(|item| *item == focused))
            {
                if modifiers.state().shift_key() {
                    current.saturating_sub(1)
                } else {
                    (current + 1).min(TOOLBAR_ORDER.len().saturating_sub(1))
                }
            } else if modifiers.state().shift_key() {
                TOOLBAR_ORDER.len().saturating_sub(1)
            } else {
                0
            };
            let Some(action) = TOOLBAR_ORDER.get(next).copied() else {
                return false;
            };
            self.toolbar_focus = Some(action);
            self.dirty = true;
            return true;
        }
        if (matches!(logical_key, Key::Named(NamedKey::Enter))
            || matches!(logical_key, Key::Character(ch) if ch == " "))
            && let Some(toolbar_action) = self.toolbar_focus
        {
            self.act(toolbar_action);
            return true;
        }

        match logical_key {
            Key::Character(ch) => match ch.as_str() {
                "n" | "N" => {
                    self.act(Action::NextChange);
                    true
                }
                "p" | "P" => {
                    self.act(Action::PrevChange);
                    true
                }
                "1" => {
                    self.act(Action::ModeDiff);
                    true
                }
                "2" => {
                    self.act(Action::ModeMerge);
                    true
                }
                _ => false,
            },
            Key::Named(NamedKey::F8) => {
                self.act(Action::NextChange);
                true
            }
            Key::Named(NamedKey::F7) => {
                self.act(Action::PrevChange);
                true
            }
            _ => false,
        }
    }

    /// Toolbar button clicks. Returns `true` if the event was consumed.
    fn handle_toolbar_click(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let hit = self
            .tool_hits
            .iter()
            .find(|t| t.rect.contains(self.cursor))
            .map(|t| t.action);
        let Some(action) = hit else {
            return false;
        };
        self.toolbar_focus = Some(action);
        self.act(action);
        true
    }

    fn toolbar_action_name(action: Action) -> &'static str {
        match action {
            Action::ModeDiff => "Show two-way diff",
            Action::ModeMerge => "Show three-way merge",
            Action::PrevChange => "Previous change",
            Action::NextChange => "Next change",
            Action::Open => "Open two files",
            Action::LoadSample => "Load sample comparison",
        }
    }

    fn toolbar_action_target(action: Action) -> &'static str {
        match action {
            Action::ModeDiff => "diffmerge.toolbar.show-diff",
            Action::ModeMerge => "diffmerge.toolbar.show-merge",
            Action::PrevChange => "diffmerge.toolbar.previous-change",
            Action::NextChange => "diffmerge.toolbar.next-change",
            Action::Open => "diffmerge.toolbar.open-files",
            Action::LoadSample => "diffmerge.toolbar.load-sample",
        }
    }

    fn toolbar_action_from_target(target: &str) -> Option<Action> {
        match target {
            "diffmerge.toolbar.show-diff" => Some(Action::ModeDiff),
            "diffmerge.toolbar.show-merge" => Some(Action::ModeMerge),
            "diffmerge.toolbar.previous-change" => Some(Action::PrevChange),
            "diffmerge.toolbar.next-change" => Some(Action::NextChange),
            "diffmerge.toolbar.open-files" => Some(Action::Open),
            "diffmerge.toolbar.load-sample" => Some(Action::LoadSample),
            _ => None,
        }
    }
}

impl Default for DiffMergeApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for DiffMergeApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::DIFFMERGE_OPERATION_TYPE]
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == qos_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        self.act(match operation {
            DiffmergeOperation::ShowDiff => Action::ModeDiff,
            DiffmergeOperation::ShowMerge => Action::ModeMerge,
            DiffmergeOperation::PreviousChange => Action::PrevChange,
            DiffmergeOperation::NextChange => Action::NextChange,
            DiffmergeOperation::ShowDiffBlock { block, fingerprint } => {
                let Ok(block) = usize::try_from(block) else {
                    return Err("this change target is no longer available".into());
                };
                if !self.diff.show_block(block, &fingerprint) {
                    return Err(
                        "the comparison changed; reopen this change from the current results"
                            .into(),
                    );
                }
                self.dirty = true;
                return Ok(true);
            }
            DiffmergeOperation::OpenFiles { .. } => Action::Open,
            DiffmergeOperation::LoadSample => Action::LoadSample,
        });
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        let crate::actions::DiffmergeOperation::OpenFiles { left, right } = operation else {
            let encoded = serde_json::to_vec(&operation).map_err(|e| e.to_string())?;
            return self
                .invoke_resource_operation_v2(kind, type_id, &encoded, doc, path)
                .map(|v| v.map(ui::widget::ResourceOperationStart::Completed));
        };
        if self.resource_invocation.is_some() || self.pending_resource_paths.is_some() {
            return Err("another file operation is already running".into());
        }
        match (left, right) {
            (Some(left), Some(right)) => {
                self.pending_resource_paths = Some((invocation, left.into(), right.into()));
            }
            (None, None) => {
                self.resource_invocation = Some(invocation);
                self.request_open_via_portal();
            }
            _ => return Err("left and right paths must be supplied together".into()),
        }
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
        if cancellation_requested
            && self
                .pending_resource_paths
                .as_ref()
                .is_some_and(|(id, _, _)| *id == invocation)
        {
            self.pending_resource_paths = None;
            return Some(qos_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("cancelled before files were read".into()),
            });
        }
        if cancellation_requested && self.resource_invocation == Some(invocation) {
            self.resource_invocation = None;
            self.portal_rx = None;
            self.pending_left = None;
            return Some(qos_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("file selection cancelled".into()),
            });
        }
        self.resource_result
            .as_ref()
            .is_some_and(|u| u.invocation() == invocation)
            .then(|| self.resource_result.take())
            .flatten()
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if let Some((invocation, left, right)) = self.pending_resource_paths.take() {
            let result = self.load_paths(&left, &right);
            self.resource_result = Some(match result {
                Ok(()) => qos_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: qos_actions::ActionOutput::default(),
                },
                Err(message) => qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
                        code: "diffmerge.open_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            });
        }
        // Pull in any file the host portal picker returned since the last
        // frame (off-thread; never blocks the UI).
        self.drain_portal();

        // Only the active mode owns the shared merged buffer; sync the merge
        // panel always so its CRDT mirror stays seeded.
        self.merge.sync(state, path);
        self.diff.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let diff_node = self.diff.build_layout(taffy, theme, node_map);
        let merge_node = self.merge.build_layout(taffy, theme, node_map);
        self.diff_node = Some(diff_node);
        self.merge_node = Some(merge_node);

        // Only the active view participates in the flex content area; the other
        // is laid out off-band (we drive its rect manually in apply_layout).
        let content_child = match self.mode {
            Mode::Diff => diff_node,
            Mode::Merge => merge_node,
        };

        // `new_with_children` only fails when a child belongs to a different
        // tree; `content_child` and `content` were just built in this one.
        #[allow(
            clippy::unwrap_used,
            reason = "content_child was just created in this same tree"
        )]
        let content = taffy
            .new_with_children(
                Style {
                    flex_grow: 1.0,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[content_child],
            )
            .unwrap();

        #[allow(
            clippy::unwrap_used,
            reason = "content was just created in this same tree"
        )]
        let root = taffy
            .new_with_children(
                Style {
                    display: Display::Flex,
                    flex_direction: FlexDirection::Column,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[content],
            )
            .unwrap();
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        // Drive the active view to the content rect directly so mode switches
        // don't require a taffy rebuild to position correctly.
        let content = self.content_rect();
        let new_origin = Point::new(content.x0, content.y0);
        match self.mode {
            Mode::Diff => {
                if let Some(n) = self.diff_node {
                    self.diff.apply_layout(taffy, n, new_origin);
                }
            }
            Mode::Merge => {
                if let Some(n) = self.merge_node {
                    self.merge.apply_layout(taffy, n, new_origin);
                }
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);

        match self.mode {
            Mode::Diff => self.diff.draw(scene, transform, tr, theme, presence),
            Mode::Merge => self.merge.draw(scene, transform, tr, theme, presence),
        }

        self.draw_toolbar(scene, transform, tr, theme);
        self.draw_status_bubble(scene, transform, tr, theme);
        self.dirty = false;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        if self.handle_keyboard_shortcut(event, *modifiers) {
            *handled = true;
            return;
        }

        if self.handle_toolbar_click(event) {
            *handled = true;
            return;
        }

        // Route to the active view.
        match self.mode {
            Mode::Diff => {
                if let Some(n) = self.diff_node {
                    self.diff.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        n,
                        taffy,
                    );
                }
            }
            Mode::Merge => {
                if let Some(n) = self.merge_node {
                    self.merge.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        n,
                        taffy,
                    );
                }
                // A chip click on the merge panel queues its resolution; dispatch
                // it through the shared action path so the chip and Ctrl+K produce
                // the same replayable action under one undo boundary.
                if let Some(action) = self.merge.take_pending_action() {
                    self.perform_action(state, &action);
                    *handled = true;
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "DiffMergeApp"
    }

    // The toolbar is immediate-mode chrome, while the active comparison view
    // owns its own semantic controls. Exposing that live child is what lets the
    // compact source switcher reach the actual qOS accessibility tree rather
    // than only being available to pointer users.
    fn children(&self) -> Vec<&dyn Widget> {
        match self.mode {
            Mode::Diff => vec![&self.diff],
            Mode::Merge => vec![&self.merge],
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match self.mode {
            Mode::Diff => vec![&mut self.diff],
            Mode::Merge => vec![&mut self.merge],
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let controls = self
            .tool_hits
            .iter()
            .map(|hit| {
                let action = hit.action;
                let role = match action {
                    Action::ModeDiff | Action::ModeMerge => Role::ToggleButton,
                    _ => Role::Button,
                };
                let selected = matches!(
                    (action, self.mode),
                    (Action::ModeDiff, Mode::Diff) | (Action::ModeMerge, Mode::Merge)
                );
                AccessNode::new(role)
                    .with_name(Self::toolbar_action_name(action))
                    .with_action_target(Self::toolbar_action_target(action))
                    .with_bounds(hit.rect)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.toolbar_focus == Some(action))
                            .checked(selected),
                    )
            })
            .collect();
        let mut children = vec![
            AccessNode::new(Role::Toolbar)
                .with_name("Comparison controls")
                .with_children(controls),
        ];
        if let Some(content) = match self.mode {
            Mode::Diff => self.diff.accessibility(),
            Mode::Merge => self.merge.accessibility(),
        } {
            children.push(content);
        }
        if self.mode == Mode::Diff && self.diff.model().rows.is_empty() {
            children.push(
                AccessNode::new(Role::Paragraph)
                    .with_name("No files loaded")
                    .with_description("Use Open two files to start a local comparison."),
            );
        }
        if let Some(error) = &self.load_error {
            children.push(
                AccessNode::new(Role::Paragraph)
                    .with_name("File loading failed")
                    .with_description(error),
            );
        }
        children.push(
            AccessNode::new(Role::Paragraph)
                .with_name("Comparison status")
                .with_description(self.status_text()),
        );
        Some(
            AccessNode::new(Role::Group)
                .with_name("Diff and Merge")
                .with_description("Compare local files or resolve a shared three-way merge")
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [0, index] = path else {
            return None;
        };
        self.tool_hits.get(*index).map(|hit| hit.rect)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [0, index] = path else {
            return false;
        };
        let Some(hit) = self.tool_hits.get(*index) else {
            return false;
        };
        self.toolbar_focus = Some(hit.action);
        self.dirty = true;
        true
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus) {
            return false;
        }
        if let Some((block, fingerprint)) = target
            .strip_prefix("diffmerge.diff-block:")
            .and_then(|id| id.split_once(':'))
            .and_then(|(index, fingerprint)| {
                index
                    .parse::<usize>()
                    .ok()
                    .map(|index| (index, fingerprint))
            })
        {
            if self.mode != Mode::Diff || !self.diff.show_block(block, fingerprint) {
                return false;
            }
            self.dirty = true;
            return true;
        }
        if let Some(show_right) = match target {
            "diffmerge.diff-source:left" => Some(false),
            "diffmerge.diff-source:right" => Some(true),
            _ => None,
        } {
            if self.mode != Mode::Diff {
                return false;
            }
            self.diff.select_compact_side(show_right);
            self.dirty = true;
            return true;
        }
        if let Some((hunk, choice)) = target
            .strip_prefix("diffmerge.merge.resolve:")
            .and_then(|value| value.split_once(':'))
            .and_then(|(hunk, choice)| {
                let hunk = hunk.parse::<usize>().ok()?;
                let choice = match choice {
                    "ours" => crate::actions::HunkChoice::Ours,
                    "theirs" => crate::actions::HunkChoice::Theirs,
                    "both" => crate::actions::HunkChoice::Both,
                    _ => return None,
                };
                Some((hunk, choice))
            })
        {
            if self.mode != Mode::Merge || self.merge.model().hunks.get(hunk).is_none() {
                return false;
            }
            if matches!(action, ui::a11y::Action::Activate) {
                self.merge.resolve(hunk, choice);
                if let Some(pending) = self.merge.take_pending_action() {
                    self.perform_action(state, &pending);
                }
            }
            self.dirty = true;
            return true;
        }
        let Some(toolbar_action) = Self::toolbar_action_from_target(target) else {
            return false;
        };
        self.toolbar_focus = Some(toolbar_action);
        if matches!(action, ui::a11y::Action::Activate) {
            self.act(toolbar_action);
        } else {
            self.dirty = true;
        }
        true
    }

    fn tooltip(&self) -> Option<&str> {
        self.tool_hits
            .iter()
            .find(|hit| hit.rect.contains(self.cursor))
            .map(|hit| Self::toolbar_action_name(hit.action))
    }

    /// Publish the open merge + its hunks so the host's Ctrl+K action palette
    /// scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-panel chip dispatched its
    /// resolution action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Mode switches swap which child is in the flex content, so signal a
        // rebuild when the mode flips. We detect this by tracking a one-shot.
        std::mem::take(&mut self.structure_dirty)
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.diff.needs_redraw() || self.merge.needs_redraw()
    }

    /// Keep ticking while a host-portal open is in flight so the off-thread
    /// result is drained the moment it arrives. Idle otherwise (the `--shots`
    /// / offscreen path never has a picker pending, so it settles to zero
    /// frames).
    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming while a host-portal open is in flight, and while
        // the floating status bubble is lingering / fading (so it animates out
        // and hides on its own). Idle otherwise.
        self.portal_rx.is_some() || self.status_bubble.needs_frame(std::time::Instant::now())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.diff.mark_drawn();
        self.merge.mark_drawn();
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, tr, theme, presence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::portal::inject_for_test;

    fn app_with_state() -> (DiffMergeApp, ui::State) {
        let mut app = DiffMergeApp::new();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        (app, state)
    }

    fn write_temp(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("diffmerge-portal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn direct_pair_is_accepted_then_succeeds() {
        let (mut app, mut state) = app_with_state();
        let left = write_temp("direct-left.txt", "left\n");
        let right = write_temp("direct-right.txt", "right\n");
        let invocation = qos_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::DiffmergeOperation::OpenFiles {
            left: Some(left.display().to_string()),
            right: Some(right.display().to_string()),
        })
        .unwrap();
        let started = Widget::start_resource_operation(
            &mut app,
            invocation,
            qos_actions::OperationKind::Effect,
            crate::actions::DIFFMERGE_OPERATION_TYPE,
            &payload,
            &state.doc,
            &state.root,
        )
        .unwrap()
        .unwrap();
        assert_eq!(started, ui::widget::ResourceOperationStart::Accepted);
        app.sync(&mut state.doc, &state.root);
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &state.doc, &state.root),
            Some(qos_actions::InvocationUpdate::Succeeded { .. })
        ));
        std::fs::remove_dir_all(left.parent().unwrap()).ok();
        std::fs::remove_dir_all(right.parent().unwrap()).ok();
    }

    /// The "Open files" toolbar action must reach the host portal, never an
    /// in-app dialog. A sentinel channel is pre-armed so applying Open cannot
    /// launch a real host dialog; the in-flight guard must preserve it.
    #[test]
    fn open_action_routes_to_the_host_portal() {
        let (mut app, _st) = app_with_state();
        app.portal_rx = Some(inject_for_test(PortalOutcome::Cancelled));
        app.act(Action::Open);
        assert!(
            app.portal_rx.is_some(),
            "Open routes through the host-portal request path (not an in-app picker)"
        );
    }

    /// The first file of a diff pair is stashed and a follow-up pick is
    /// requested (`consume_pick` returns true) — no host dialog is spawned by
    /// the consume step itself.
    #[test]
    fn first_pick_stashes_left_and_asks_for_the_right() {
        let (mut app, _st) = app_with_state();
        let left = write_temp("left.txt", "alpha\nbeta\ngamma\n");
        let needs_right = app.consume_pick(&left).unwrap();
        assert!(
            needs_right,
            "after the left file, the right one is still needed"
        );
        let (label, content) = app.pending_left.clone().expect("left file is stashed");
        assert_eq!(label, "left.txt");
        assert_eq!(content, "alpha\nbeta\ngamma\n");
        std::fs::remove_dir_all(left.parent().unwrap()).ok();
    }

    /// The terminal step: with a left file already pending, a portal-chosen
    /// right file completes the two-file diff through the per-frame `sync`
    /// drain — no real dialog, exercising the exact channel the worker feeds.
    #[test]
    fn portal_pick_completes_two_file_diff_via_frame_drain() {
        let (mut app, mut st) = app_with_state();
        app.pending_left = Some(("left.txt".into(), "alpha\nbeta\ngamma\n".into()));
        let right = write_temp("right.txt", "alpha\nBETA\ngamma\ndelta\n");

        app.portal_rx = Some(inject_for_test(PortalOutcome::Opened(right.clone())));
        // The per-frame sync drains the portal channel and builds the diff.
        app.sync(&mut st.doc, &st.root);

        assert_eq!(
            app.mode,
            Mode::Diff,
            "a completed pair switches to Diff mode"
        );
        assert!(app.pending_left.is_none(), "the pending-left slot clears");
        assert!(
            app.portal_rx.is_none(),
            "the portal channel clears after delivering"
        );
        let m = app.diff.model();
        assert_eq!(m.left_lines, 3, "the stashed left file is compared");
        assert_eq!(m.right_lines, 4, "the portal-chosen right file is compared");
        assert!(
            m.added + m.modified >= 1,
            "the differing files yield change blocks"
        );
        std::fs::remove_dir_all(right.parent().unwrap()).ok();
    }

    /// A cancelled portal interaction abandons an in-progress selection and
    /// leaves the channel cleared.
    #[test]
    fn portal_cancel_clears_pending_selection() {
        let (mut app, mut st) = app_with_state();
        app.pending_left = Some(("left.txt".into(), "x\n".into()));
        app.portal_rx = Some(inject_for_test(PortalOutcome::Cancelled));
        app.sync(&mut st.doc, &st.root);
        assert!(
            app.pending_left.is_none(),
            "cancel abandons the half-finished pair"
        );
        assert!(
            app.portal_rx.is_none(),
            "the portal channel clears after a cancel"
        );
    }

    /// A portal failure (e.g. no desktop portal running) surfaces on the
    /// status line rather than silently dropping the request.
    #[test]
    fn portal_failure_surfaces_on_the_status_line() {
        let (mut app, mut st) = app_with_state();
        app.portal_rx = Some(inject_for_test(PortalOutcome::Failed("no portal".into())));
        app.sync(&mut st.doc, &st.root);
        assert!(app.pending_left.is_none());
        assert!(app.portal_rx.is_none());
        let err = app.load_error.as_deref().expect("a failure is recorded");
        assert!(err.contains("no portal"), "the reason is surfaced: {err}");
    }

    /// The full covenant on one on-panel affordance: driving the chip handler
    /// exactly as a click does queues a resolution; dispatching it through the
    /// app's shared action path mutates the merged buffer, records **one** undo
    /// boundary bracketed at the pre-action heads, and Ctrl+Z reverts precisely
    /// that action.
    #[test]
    fn chip_dispatch_records_one_undo_boundary_and_reverts() {
        let mut app = DiffMergeApp::new_demo(); // Merge mode with a real conflict
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root); // seeds merged + clears resolutions

        let hunk = app
            .merge
            .model()
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .expect("the demo merge has a conflict");

        let before_merged = crate::doc::MergeDoc::merged_text(&state.doc);
        let before_heads = state.doc.get_heads();

        // Drive the chip handler exactly as a click does, then dispatch through
        // the shared path (as `handle_event`'s drain does).
        app.merge.resolve(hunk, crate::actions::HunkChoice::Theirs);
        let action = app
            .merge
            .take_pending_action()
            .expect("a chip click queues a resolution");
        app.perform_action(&mut state.doc, &action);

        assert!(
            app.last_dispatched().is_some(),
            "the affordance dispatched an envelope"
        );
        let after_merged = crate::doc::MergeDoc::merged_text(&state.doc);
        assert_ne!(
            before_merged, after_merged,
            "the action mutated the merged output"
        );
        assert_eq!(
            crate::doc::MergeDoc::resolutions(&state.doc),
            vec![(hunk, "theirs".to_string())],
            "the take-a-side choice is recorded in the CRDT"
        );

        // Exactly one undo boundary, drained and recorded by the host's own
        // recorder rather than pushed by hand: attribution happens in there,
        // and undo is author-scoped. The node map is empty because the app
        // widget is the root here.
        let local_author = ui::collaboration::local_user_id();
        let root: *mut dyn Widget = &mut app;
        assert!(
            ui::undo::record_dispatched_action(
                &mut state,
                root,
                &std::collections::HashMap::new(),
                &local_author,
            ),
            "the host recorded an undo boundary for the dispatched action"
        );
        let entry = state.peek_last_undo().expect("one undo boundary recorded");
        assert_eq!(
            entry.before, before_heads,
            "bracket starts at the pre-action heads"
        );
        assert_eq!(
            entry.author.as_ref(),
            Some(&local_author),
            "attributed to the local user, so their Ctrl+Z can reach it"
        );

        // Ctrl+Z reverts exactly this action.
        state
            .undo_as(Some(&local_author), None, None)
            .expect("an undo entry to pop");
        assert_eq!(
            crate::doc::MergeDoc::merged_text(&state.doc),
            before_merged,
            "undo reverted the resolution and nothing else"
        );
    }

    /// Single source of truth: the take-theirs chip on the merge panel builds the
    /// byte-identical [`EncodedAction`] the palette's take-theirs `ActionSpec`
    /// does — one action, two entry points.
    #[test]
    fn chip_and_palette_encode_identically() {
        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);

        let hunk = app
            .merge
            .model()
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .unwrap();

        // Palette path: the take-theirs immediate spec on the conflict resource.
        let resources = Widget::resources(&app, &state.doc);
        let conflict = resources[0]
            .children
            .iter()
            .find(|r| r.actions.iter().find(|a| a.id == "take-theirs").is_some_and(|spec| {
                let qos_actions::ActionInput::Immediate(encoded) = &spec.input else { return false };
                matches!(serde_json::from_slice::<crate::actions::DiffmergeAction>(&encoded.payload), Ok(crate::actions::DiffmergeAction::ResolveHunk { hunk: target, .. }) if target == hunk as u64)
            }))
            .expect("the conflict resource");
        let spec = conflict
            .actions
            .iter()
            .find(|a| a.id == "take-theirs")
            .unwrap();
        let from_spec = match &spec.input {
            qos_actions::ActionInput::Immediate(e) => e.clone(),
            qos_actions::ActionInput::Prompted { .. } => panic!("take-theirs is immediate"),
        };

        // Chip path: `resolve` queues the action a click builds.
        app.merge.resolve(hunk, crate::actions::HunkChoice::Theirs);
        let from_chip = crate::actions::encode(&app.merge.take_pending_action().unwrap());

        assert_eq!(from_spec.type_id, from_chip.type_id);
        assert_eq!(
            from_spec.payload, from_chip.payload,
            "chip and palette are one source of truth"
        );
    }

    /// Run the real five-pass frame so the merge panel lays out and draws,
    /// which is what populates the resolution-chip hit rects.
    fn draw_merge_frame(app: &mut DiffMergeApp, viewport: (f32, f32)) {
        use std::collections::HashMap;
        let (w, h) = viewport;
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w),
                        height: Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(w),
                        height: AvailableSpace::Definite(h),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    fn keyboard_event(key: Key) -> WindowEvent {
        use winit::event::{DeviceId, KeyEvent};
        use winit::keyboard::{KeyLocation, PhysicalKey};

        // `KeyEvent` has an opaque platform payload. The public fields below
        // are exactly the ones the widget reads; this mirrors the toolkit's
        // own headless keyboard helpers.
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = key;
        event.physical_key =
            PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = None;
        WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    fn selected_compact_source(app: &DiffMergeApp) -> String {
        let node = Widget::accessibility(&app.merge).expect("merge is exposed to assistive tech");
        node.children
            .iter()
            .find(|child| child.state.checked == Some(true))
            .and_then(|child| child.name.clone())
            .expect("one compact source is selected")
    }

    /// At phone width, a three-way merge intentionally presents one full-width
    /// source pane plus an explicit Base/Ours/Theirs switcher. This guards
    /// against a future layout change quietly restoring three unreadable
    /// columns at 390px.
    #[test]
    fn phone_merge_uses_a_full_width_source_switcher() {
        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        draw_merge_frame(&mut app, (390.0, 760.0));

        let hits = app.merge.compact_source_hits();
        assert_eq!(hits.len(), 3, "phone has Base/Ours/Theirs source controls");
        assert_eq!(
            hits.iter().map(|(_, name)| *name).collect::<Vec<_>>(),
            vec!["Base", "Ours", "Theirs"]
        );
        for (rect, _) in &hits {
            assert!(
                rect.width() >= 110.0,
                "each source target stays finger-sized: {rect:?}"
            );
            assert!(
                rect.x0 >= 0.0 && rect.x1 <= 390.0,
                "source target stays onscreen: {rect:?}"
            );
        }
        assert!(hits.windows(2).all(|pair| pair[0].0.x1 <= pair[1].0.x0));

        let semantic = Widget::accessibility(&app.merge).expect("merge semantics");
        assert_eq!(
            semantic
                .children
                .iter()
                .filter(|child| child
                    .name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("Show ")))
                .count(),
            3,
            "the source switcher remains accessible alongside resolution controls"
        );
        assert_eq!(selected_compact_source(&app), "Show Ours source");

        // At tablet width the selector disappears because all three panes are
        // deliberately visible side by side.
        draw_merge_frame(&mut app, (800.0, 760.0));
        assert!(app.merge.compact_source_hits().is_empty());
        assert!(
            Widget::accessibility(&app.merge)
                .expect("merge semantics")
                .children
                .iter()
                .all(|child| {
                    !child
                        .name
                        .as_deref()
                        .is_some_and(|name| name.starts_with("Show "))
                })
        );
    }

    /// The compact source switcher is only useful when the surrounding chrome
    /// is usable too. Keep the complete one-row phone toolbar on screen and
    /// non-overlapping (both modes, file open/sample, and change navigation).
    #[test]
    fn phone_toolbar_keeps_every_control_visible_without_overlap() {
        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        draw_merge_frame(&mut app, (390.0, 760.0));

        assert_eq!(app.tool_hits.len(), 6, "two modes and four actions");
        let mut rects: Vec<_> = app.tool_hits.iter().map(|hit| hit.rect).collect();
        rects.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        assert!(rects.iter().all(|rect| rect.x0 >= 0.0 && rect.x1 <= 390.0));
        assert!(rects.windows(2).all(|pair| pair[0].x1 <= pair[1].x0));
    }

    /// Pointer, keyboard, and assistive activation all select the same compact
    /// source state. The test drives the real event/semantic routes rather
    /// than calling the selection helper directly.
    #[test]
    fn compact_source_switcher_has_pointer_keyboard_and_a11y_parity() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, Modifiers};

        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        draw_merge_frame(&mut app, (390.0, 760.0));
        let theirs = app
            .merge
            .compact_source_hits()
            .into_iter()
            .find(|(_, name)| *name == "Theirs")
            .expect("Theirs selector")
            .0
            .center();
        let mods = Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        app.handle_event(
            &WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(theirs.x, theirs.y),
            },
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        app.handle_event(
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert!(handled);
        assert_eq!(selected_compact_source(&app), "Show Theirs source");

        handled = false;
        app.handle_event(
            &keyboard_event(Key::Named(NamedKey::ArrowLeft)),
            1.0,
            &mods,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert!(handled);
        assert_eq!(selected_compact_source(&app), "Show Ours source");

        let theirs_key = ui::a11y::hash_key(&"diffmerge.source.theirs");
        assert!(Widget::perform_action_at(
            &mut app.merge,
            &ui::a11y::Action::Activate,
            &[theirs_key],
            &mut state.doc,
            &state.root,
        ));
        assert_eq!(selected_compact_source(&app), "Show Theirs source");
    }

    #[test]
    fn toolbar_controls_have_semantic_names_focus_geometry_and_activation() {
        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        draw_merge_frame(&mut app, (980.0, 720.0));

        let semantics = Widget::accessibility(&app).expect("toolbar semantics");
        let toolbar = semantics.children.first().expect("toolbar group");
        assert_eq!(toolbar.children.len(), 6, "both modes and four actions");
        assert!(toolbar.children.iter().all(|node| {
            node.state.focusable && node.bounds.is_some() && node.action_target.is_some()
        }));
        let sample_index = toolbar
            .children
            .iter()
            .position(|node| node.name.as_deref() == Some("Load sample comparison"))
            .expect("sample control");
        assert!(Widget::focus_synthetic_accessibility(
            &mut app,
            &[0, sample_index]
        ));
        assert_eq!(app.toolbar_focus, Some(Action::LoadSample));
        assert!(Widget::perform_semantic_action(
            &mut app,
            "diffmerge.toolbar.show-diff",
            &ui::a11y::Action::Activate,
            &mut state.doc,
            &state.root,
        ));
        assert_eq!(app.mode, Mode::Diff);
    }

    #[test]
    fn toolbar_keyboard_tab_and_enter_follow_the_painted_control_order() {
        use winit::event::Modifiers;

        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        let mut handled = false;
        let mut focus = None;
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let modifiers = Modifiers::default();
        app.handle_event(
            &keyboard_event(Key::Named(NamedKey::Tab)),
            1.0,
            &modifiers,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert!(handled);
        assert_eq!(app.toolbar_focus, Some(Action::ModeDiff));
        handled = false;
        app.handle_event(
            &keyboard_event(Key::Named(NamedKey::Tab)),
            1.0,
            &modifiers,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert_eq!(app.toolbar_focus, Some(Action::ModeMerge));
        app.handle_event(
            &keyboard_event(Key::Named(NamedKey::Enter)),
            1.0,
            &modifiers,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        assert_eq!(app.mode, Mode::Merge);
    }

    #[test]
    fn phone_diff_uses_an_accessible_one_source_switcher() {
        let mut app = DiffMergeApp::new_demo();
        app.mode = Mode::Diff;
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        draw_merge_frame(&mut app, (390.0, 760.0));
        let root = Widget::accessibility(&app).expect("diff semantics");
        let diff = root
            .children
            .iter()
            .find(|child| {
                child
                    .name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("Diff between"))
            })
            .expect("active diff content");
        assert_eq!(
            diff.children
                .iter()
                .filter(|child| child
                    .name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("Show ")))
                .count(),
            2
        );
        assert!(Widget::perform_semantic_action(
            &mut app,
            "diffmerge.diff-source:right",
            &ui::a11y::Action::Activate,
            &mut state.doc,
            &state.root,
        ));
        assert!(app.diff.compact_showing_right());
    }

    #[test]
    fn stale_diff_block_operation_refuses_a_recomputed_block_at_the_same_index() {
        let mut app = DiffMergeApp::new();
        app.diff.set_inputs(
            "before.txt",
            "same\nold value\n",
            "after.txt",
            "same\nnew value\n",
        );
        let stale_fingerprint = app.diff.block_fingerprint(0).expect("one change block");
        app.diff.set_inputs(
            "before.txt",
            "same\nold value\n",
            "after.txt",
            "same\nreplacement value\n",
        );
        let state = ui::State::new();
        let payload = serde_json::to_vec(&DiffmergeOperation::ShowDiffBlock {
            block: 0,
            fingerprint: stale_fingerprint,
        })
        .expect("operation serializes");

        let error = Widget::invoke_resource_operation(
            &mut app,
            qos_actions::OperationKind::View,
            crate::actions::DIFFMERGE_OPERATION_TYPE,
            &payload,
            &state.doc,
            &state.root,
        )
        .expect_err("stale local operation must explain why its target is unsafe");
        assert!(error.contains("comparison changed"));
    }

    #[test]
    fn narrow_status_bubble_stays_within_the_viewport_for_long_errors() {
        let mut app = DiffMergeApp::for_shot(Some("error"));
        app.arm_status_bubble_for_shots();
        draw_merge_frame(&mut app, (390.0, 760.0));
        let bubble = app.status_bubble_rect();
        assert!(
            bubble.x0 >= app.rect.x0 && bubble.x1 <= app.rect.x1,
            "narrow status bubble must not clip: {bubble:?} in {:?}",
            app.rect
        );
        assert!(
            bubble.y0 >= app.rect.y0 && bubble.y1 <= app.rect.y1,
            "narrow status bubble must remain vertically visible: {bubble:?} in {:?}",
            app.rect
        );
    }

    #[test]
    fn root_accessibility_includes_active_hunks_and_uses_the_crdt_resolution_path() {
        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        draw_merge_frame(&mut app, (980.0, 720.0));

        let root = Widget::accessibility(&app).expect("root semantics");
        let merge = root
            .children
            .iter()
            .find(|child| child.name.as_deref() == Some("Three-way merge"))
            .expect("active merge content is not hidden behind toolbar chrome");
        let target = merge
            .children
            .iter()
            .find_map(|child| child.action_target.clone())
            .expect("one drawn resolution action is accessible");
        let before = crate::doc::MergeDoc::merged_text(&state.doc);
        assert!(Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::Activate,
            &mut state.doc,
            &state.root,
        ));
        assert_ne!(crate::doc::MergeDoc::merged_text(&state.doc), before);
    }

    #[test]
    fn root_accessibility_explains_empty_and_error_content() {
        let mut empty = DiffMergeApp::for_shot(Some("empty"));
        draw_merge_frame(&mut empty, (980.0, 720.0));
        let empty_semantics = Widget::accessibility(&empty).expect("empty semantics");
        let names = empty_semantics
            .children
            .iter()
            .filter_map(|child| child.name.as_deref())
            .collect::<Vec<_>>();
        assert!(names.contains(&"No files loaded"), "{names:?}");

        let mut error = DiffMergeApp::for_shot(Some("error"));
        draw_merge_frame(&mut error, (980.0, 720.0));
        let error_semantics = Widget::accessibility(&error).expect("error semantics");
        let names = error_semantics
            .children
            .iter()
            .filter_map(|child| child.name.as_deref())
            .collect::<Vec<_>>();
        assert!(names.contains(&"File loading failed"), "{names:?}");
    }

    /// Clicking a resolution chip at its drawn pixel position resolves *that*
    /// hunk — proving the pointer → hit-rect → hunk mapping and the app's
    /// pending-action drain, not just `resolve` in isolation.
    #[test]
    fn clicking_a_resolution_chip_resolves_that_hunk_through_the_pointer_path() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, Modifiers};

        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        draw_merge_frame(&mut app, (1200.0, 760.0));

        let hunk = app
            .merge
            .model()
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .expect("the demo merge has a conflict");
        // The take-theirs chip drawn for that hunk, by its real pixel rect.
        let (rect, _, _) = app
            .merge
            .chip_hits()
            .into_iter()
            .find(|(_, h, ch)| *h == hunk && *ch == crate::actions::HunkChoice::Theirs)
            .expect("a take-theirs chip is drawn for the conflict hunk");
        let center = rect.center();

        let before = crate::doc::MergeDoc::merged_text(&state.doc);

        let m = Modifiers::default();
        let mut handled = false;
        let mut focus = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        app.handle_event(
            &WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: PhysicalPosition::new(center.x, center.y),
            },
            1.0,
            &m,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        app.handle_event(
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &m,
            &mut state.doc,
            &state.root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );

        assert!(handled, "the chip click was consumed");
        assert_eq!(
            app.merge.model().hunks[hunk].resolution,
            crate::model::Resolution::TookTheirs,
            "clicking the take-theirs chip resolved that hunk to theirs"
        );
        assert_ne!(
            before,
            crate::doc::MergeDoc::merged_text(&state.doc),
            "the click dispatched through perform_action and mutated the merged output"
        );
        assert_eq!(
            crate::doc::MergeDoc::resolutions(&state.doc),
            vec![(hunk, "theirs".to_string())],
            "the pointer-driven resolution was recorded in the CRDT"
        );
    }
}
