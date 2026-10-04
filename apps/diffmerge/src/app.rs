//! Root widget — orchestrates the diff and merge modes, the top toolbar
//! (mode switch, change navigation, open), and the host-portal file open.
//!
//! Data flow:
//! * The document holds what is open — the comparison, the merge and its
//!   merged result — and the app is [presented](DiffMergeApp::present_state)
//!   it; the two views ([`DiffView`], [`MergeView`]) draw from that.
//! * The toolbar's buttons are drawn directly and hit-tested in `handle_event`.
//! * Opening files is the [`DiffmergeEffect::OpenFiles`] effect, whatever
//!   asks for it: it goes through the **host** file chooser (the XDG Desktop
//!   Portal, [`ui::portal`]) — the app never draws its own picker (HIG §5) —
//!   or reads the two paths it was given, and opens what it read as a
//!   comparison through an action.

use std::collections::HashMap;
use std::path::Path;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::framework::{Dispatcher, EffectAnswer, EffectCx, EffectReply, Effected};
use ui::permissions::PermissionContext;
use ui::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, PickRequest, PortalChannel, PortalOutcome,
};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, SegmentedFace, ToggleGroup, Widget};

use crate::actions::{DiffmergeAction, DiffmergeEffect, DiffmergeOperation};
use crate::diff_view::DiffView;
use crate::doc::{Comparison, MergeInputs, MergeState};
use crate::merge_view::MergeView;
use crate::resource::Diffmerge;

/// Minimum toolbar band; the measured control height plus its vertical
/// breathing room wins when the text scale makes the labels taller.
const TOOLBAR_H: f64 = 48.0;
/// Minimum toolbar control height.
const TOOLBAR_CONTROL_H: f64 = 28.0;
/// Text sizes the toolbar controls paint with (`SegmentedFace` default and the
/// explicit `ButtonFace::text_size`); the measurements below use the same.
const TOOLBAR_SEGMENT_FONT: f32 = 13.0;
const TOOLBAR_BUTTON_FONT: f32 = 12.5;
/// Horizontal label padding inside a segment / button, wide and compact.
const SEGMENT_PAD_X: f64 = 11.0;
const SEGMENT_PAD_X_COMPACT: f64 = 9.0;
const BUTTON_PAD_X: f64 = 10.0;
const BUTTON_PAD_X_COMPACT: f64 = 8.0;

/// The error the `error` screenshot scenario shows.
const SHOT_LOAD_ERROR: &str = "Couldn’t read the selected files. Check permissions and try again.";

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

/// Whether chosen files have native paths here; without them (a browser
/// page) the chooser hands over their contents.
fn native_paths() -> bool {
    matches!(
        tilo_files::availability(tilo_files::Feature::NativePaths),
        tilo_files::Availability::Available
    )
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
    /// An in-flight XDG-portal file-open. The portal round-trip runs on a
    /// worker thread and hands the chosen path back over this channel,
    /// drained once per frame in [`Widget::sync`]. `None` when no picker is
    /// in flight — the app never draws its own picker (HIG §5).
    portal_rx: Option<PortalChannel>,
    /// A chooser delivering contents, where files have no path (the web).
    portal_contents_rx: Option<ContentsChannel>,
    /// Files chosen but not yet paired (first of a two-file diff selection).
    pending_left: Option<(String, String)>,
    /// The caller of the open-files effect a chooser is answering.
    open_reply: Option<EffectReply>,
    /// A human-readable reason the last open failed, surfaced in the status
    /// bubble. `None` in the normal (success / cancel) case.
    load_error: Option<String>,
    dirty: bool,
    /// One-shot flag: set when the active mode changed so the host rebuilds the
    /// taffy tree (the flex content child swaps between the two views).
    structure_dirty: bool,
    /// Where edits and effects go; `None` until the framework attaches the
    /// surface, when every edit is dropped.
    dispatcher: Option<Dispatcher<Diffmerge>>,
    /// What was open when the app was last presented the document, so a
    /// newly opened comparison or merge is shown in its mode.
    presented: Option<(Option<Comparison>, Option<MergeInputs>)>,
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
    /// Measured toolbar band height from the last draw (see
    /// [`Self::measure_toolbar`]); the content rect starts below it.
    toolbar_h: f64,
}

impl DiffMergeApp {
    /// The app before it is presented the document: empty panes and the
    /// "Open files" / "Load sample" toolbar actions.
    pub fn new() -> Self {
        Self {
            mode: Mode::Diff,
            diff: DiffView::new(),
            merge: MergeView::new(),
            rect: Rect::ZERO,
            diff_node: None,
            merge_node: None,
            tool_hits: Vec::new(),
            cursor: Point::ZERO,
            portal_rx: None,
            portal_contents_rx: None,
            pending_left: None,
            open_reply: None,
            load_error: None,
            dirty: true,
            structure_dirty: false,
            dispatcher: None,
            presented: None,
            status_bubble: ui::StatusBubble::new(),
            status_bubble_rect: Rect::ZERO,
            last_status: String::new(),
            shots_status: false,
            toolbar_focus: None,
            toolbar_h: TOOLBAR_H,
        }
    }

    /// Show the document's state. A comparison or merge opened since the
    /// last state is shown in its mode; the first state opens on the
    /// comparison unless only a merge is open.
    pub(crate) fn present_state(&mut self, state: &MergeState) {
        let comparison_changed = self
            .presented
            .as_ref()
            .is_none_or(|(comparison, _)| *comparison != state.comparison);
        let merge_changed = self
            .presented
            .as_ref()
            .is_none_or(|(_, merge)| *merge != state.merge);
        if comparison_changed {
            match &state.comparison {
                Some(c) => self
                    .diff
                    .set_inputs(&c.left_label, &c.left, &c.right_label, &c.right),
                None => self.diff.set_inputs("", "", "", ""),
            }
        }
        self.merge.present(state);
        let mode = if self.presented.is_none() {
            Some(if state.comparison.is_none() && state.merge.is_some() {
                Mode::Merge
            } else {
                Mode::Diff
            })
        } else {
            match (comparison_changed, merge_changed) {
                (true, false) if state.comparison.is_some() => Some(Mode::Diff),
                (false, true) if state.merge.is_some() => Some(Mode::Merge),
                _ => None,
            }
        };
        if let Some(mode) = mode {
            self.show(mode);
        }
        self.presented = Some((state.comparison.clone(), state.merge.clone()));
        self.dirty = true;
    }

    /// Submit edits and effects through `dispatcher`.
    pub(crate) fn attach_dispatcher(&mut self, dispatcher: Dispatcher<Diffmerge>) {
        self.merge
            .attach(crate::resource::merged_dispatcher(&dispatcher));
        self.merge.set_read_only(dispatcher.read_only());
        self.dispatcher = Some(dispatcher);
    }

    pub(crate) fn set_read_only(&mut self, read_only: bool) {
        self.merge.set_read_only(read_only);
    }

    /// Stage a screenshot scenario's view state.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("diff" | "empty") => self.show(Mode::Diff),
            Some("error") => {
                self.show(Mode::Diff);
                self.load_error = Some(SHOT_LOAD_ERROR.into());
                self.arm_status_bubble_for_shots();
            }
            Some("status") => {
                self.show(Mode::Merge);
                self.arm_status_bubble_for_shots();
            }
            _ => self.show(Mode::Merge),
        }
    }

    fn show(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            self.structure_dirty = true;
        }
        self.dirty = true;
    }

    /// Answer one of the app's effects, from a toolbar button, the palette,
    /// an agent or a script.
    pub(crate) fn answer_effect(
        &mut self,
        effect: DiffmergeEffect,
        cx: &mut EffectCx<Diffmerge, Self>,
    ) -> EffectAnswer {
        match effect {
            DiffmergeEffect::LoadSample => {
                let dispatcher = cx.dispatcher();
                dispatcher.group(|| {
                    for action in crate::actions::sample_actions() {
                        dispatcher.apply(&action);
                    }
                });
                self.pending_left = None;
                self.load_error = None;
                self.dirty = true;
                Ok(Effected::done())
            }
            DiffmergeEffect::OpenFiles {
                left: Some(left),
                right: Some(right),
            } => Ok(cx.thread(
                move |_| -> Result<DiffmergeAction, String> {
                    let (left_label, left) = label_and_text(Path::new(&left))?;
                    let (right_label, right) = label_and_text(Path::new(&right))?;
                    Ok(DiffmergeAction::OpenComparison {
                        left_label,
                        left,
                        right_label,
                        right,
                    })
                },
                |app: &mut Self, opened, cx| match opened {
                    Ok(action) => {
                        app.load_error = None;
                        cx.apply(&action);
                        app.show(Mode::Diff);
                        Ok(Effected::done())
                    }
                    Err(reason) => {
                        app.load_error = Some(reason.clone());
                        app.dirty = true;
                        Err(reason)
                    }
                },
            )),
            DiffmergeEffect::OpenFiles {
                left: None,
                right: None,
            } => {
                if self.choosing() {
                    return Err("another file operation is already running".into());
                }
                self.request_open_via_portal();
                self.open_reply = Some(cx.reply());
                Ok(Effected::Pending)
            }
            DiffmergeEffect::OpenFiles { .. } => {
                Err("left and right paths must be supplied together".into())
            }
        }
    }

    /// Whether a file chooser is open.
    fn choosing(&self) -> bool {
        self.portal_rx.is_some() || self.portal_contents_rx.is_some() || self.open_reply.is_some()
    }

    /// `--shots`: force the floating status bubble fully visible so a single
    /// headless frame captures it (no live pointer hovers the reveal zone).
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(ui::time::now());
    }

    fn content_rect(&self) -> Rect {
        // Content spans to the window bottom; the summary floats as a
        // transient bubble (see `draw_status_bubble`), not a bottom strip.
        Rect::new(
            self.rect.x0,
            self.rect.y0 + self.toolbar_h,
            self.rect.x1,
            self.rect.y1,
        )
    }

    /// Give both views the content rect. Called from `apply_layout` and again
    /// from `draw` when the measured toolbar height changed, so the views are
    /// never painted a frame behind the band above them.
    fn place_views(&mut self) {
        let content = self.content_rect();
        self.diff.place(content);
        self.merge.place(content);
    }

    /// `(control_h, toolbar_h)` measured from the label sizes the controls
    /// paint with, floored at the design minimums.
    fn measure_toolbar(tr: &mut TextRenderer) -> (f64, f64) {
        let seg_h = f64::from(
            tr.measure_weighted("Ag", TOOLBAR_SEGMENT_FONT, None, TextWeight::Medium)
                .1,
        );
        let btn_h = f64::from(
            tr.measure_weighted("Ag", TOOLBAR_BUTTON_FONT, None, TextWeight::Regular)
                .1,
        );
        let control_h = (seg_h.max(btn_h) + 10.0).max(TOOLBAR_CONTROL_H);
        (control_h, (control_h + 20.0).max(TOOLBAR_H))
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::ModeDiff => self.show(Mode::Diff),
            Action::ModeMerge => self.show(Mode::Merge),
            Action::PrevChange | Action::NextChange => {
                let forward = action == Action::NextChange;
                match self.mode {
                    Mode::Diff => {
                        self.diff.navigate(forward);
                    }
                    Mode::Merge => {
                        self.merge.navigate(forward);
                    }
                }
                self.dirty = true;
            }
            Action::Open => self.effect(&DiffmergeEffect::OpenFiles {
                left: None,
                right: None,
            }),
            Action::LoadSample => self.effect(&DiffmergeEffect::LoadSample),
        }
    }

    /// Run one of the app's effects through the same door the palette uses.
    fn effect(&mut self, effect: &DiffmergeEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
        self.dirty = true;
    }

    /// Submit the resolution a chip, a key or an accessibility action just
    /// queued on the merge panel — the same action the palette's row builds.
    fn submit_resolution(&mut self) {
        if let Some(action) = self.merge.take_pending_action()
            && let Some(dispatcher) = &self.dispatcher
        {
            dispatcher.apply(&action);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_mode_for_test(&mut self, mode: Mode) {
        self.show(mode);
    }

    /// Ask the host desktop's file chooser to open a file to compare. Per HIG
    /// §5 the app never draws its own picker: the portal round-trip runs on a
    /// worker thread ([`ui::portal::PickRequest`]) and the chosen path
    /// comes back over a channel drained each frame in [`Self::drain_portal`].
    /// A diff needs two files, so the drain re-fires this once for the right
    /// file after the left is picked.
    fn request_open_via_portal(&mut self) {
        self.load_error = None;
        if native_paths() {
            self.portal_rx = Some(diff_open_request().send());
        } else {
            self.portal_contents_rx = Some(diff_open_request().send_contents());
        }
        self.dirty = true;
    }

    /// Drain a completed portal request, if any. Called once per frame from
    /// [`Widget::sync`]. Loads the chosen file (stashing the first of the pair
    /// and re-opening the chooser for the second), records a failure for the
    /// status bubble, and clears the in-flight channel. Never blocks — a
    /// still pending picker leaves the channel installed for the next frame.
    fn drain_portal(&mut self) {
        if self
            .open_reply
            .as_ref()
            .is_some_and(EffectReply::is_cancelled)
        {
            self.portal_rx = None;
            self.portal_contents_rx = None;
            self.pending_left = None;
            self.open_reply = None;
            self.dirty = true;
            return;
        }
        if let Some(rx) = self.portal_contents_rx.as_ref() {
            match rx.try_recv() {
                Ok(outcome) => {
                    self.portal_contents_rx = None;
                    let outcome = match outcome {
                        ContentsOutcome::Picked(files) => match files.into_iter().next() {
                            Some(file) => Pick::Opened(
                                String::from_utf8(file.bytes)
                                    .map(|text| (file.name.clone(), text))
                                    .map_err(|_| format!("{}: not UTF-8 text", file.name)),
                            ),
                            None => Pick::Cancelled,
                        },
                        ContentsOutcome::Cancelled => Pick::Cancelled,
                        ContentsOutcome::Failed(reason) => Pick::Failed(reason),
                    };
                    self.apply_pick(outcome);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.portal_contents_rx = None;
                    self.apply_pick(Pick::Failed("file picker disconnected".into()));
                }
            }
        }
        let Some(rx) = self.portal_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                let outcome = match outcome {
                    PortalOutcome::Opened(path) => Pick::Opened(label_and_text(&path)),
                    PortalOutcome::Cancelled => Pick::Cancelled,
                    PortalOutcome::Failed(reason) => Pick::Failed(reason),
                };
                self.apply_pick(outcome);
            }
            // Still waiting on the host dialog.
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
                self.apply_pick(Pick::Failed("file picker disconnected".into()));
            }
        }
    }

    /// Answer the open-files effect a chooser was answering.
    fn finish_open(&mut self, result: Result<(), String>) {
        if let Some(reply) = self.open_reply.take() {
            reply.finish(result.map(|()| tilo_actions::ActionOutput::default()));
        }
    }

    /// Apply one chooser answer to the two-file selection.
    fn apply_pick(&mut self, outcome: Pick) {
        match outcome {
            Pick::Opened(Ok((label, text))) => {
                // A `true` return means the left file just landed and the
                // right one is still needed — ask for it.
                if self.consume_text(label, text) {
                    self.request_open_via_portal();
                } else {
                    self.finish_open(Ok(()));
                }
            }
            Pick::Opened(Err(reason)) => {
                self.pending_left = None;
                self.load_error = Some(reason.clone());
                self.finish_open(Err(reason));
            }
            // A cancel abandons an in-progress two-file selection.
            Pick::Cancelled => {
                self.pending_left = None;
                self.finish_open(Err("file selection cancelled".into()));
            }
            Pick::Failed(reason) => {
                self.pending_left = None;
                self.load_error = Some(format!("Couldn't open the file chooser: {reason}"));
                self.finish_open(Err(reason));
            }
        }
        self.dirty = true;
    }

    /// Consume one chosen file. The first file of a pair is stashed and
    /// `true` is returned so the caller re-opens the chooser for the second;
    /// the second file opens the comparison and switches to Diff mode,
    /// returning `false`.
    fn consume_text(&mut self, label: String, content: String) -> bool {
        match self.pending_left.take() {
            None => {
                self.pending_left = Some((label, content));
                true
            }
            Some((left_label, left)) => {
                if let Some(dispatcher) = &self.dispatcher {
                    dispatcher.apply(&DiffmergeAction::OpenComparison {
                        left_label,
                        left,
                        right_label: label,
                        right: content,
                    });
                }
                self.show(Mode::Diff);
                false
            }
        }
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
        // Every control is sized from its measured label with the exact style
        // it is painted in (`len * 7.x` estimates clipped longer locales and
        // fell apart at 200% text), and the band grows with the control height.
        let (seg_h, toolbar_h) = Self::measure_toolbar(tr);
        self.toolbar_h = toolbar_h;
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + toolbar_h);
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &bar);
        // The bar's bottom rule is derived against the bar (HIG §9.5).
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &Line::new(Point::new(r.x0, bar.y1), Point::new(r.x1, bar.y1)),
        );

        // Mode segmented control (shared pill-style segmented control). The OS
        // titlebar carries the app name, so no self-drawn wordmark here — the
        // mode control claims the reclaimed left slot.
        let seg_y = r.y0 + (toolbar_h - seg_h) / 2.0;
        let mode_labels: [&str; 2] = if compact {
            ["Diff", "Merge"]
        } else {
            ["Diff", "3-way merge"]
        };
        let mode_actions = [Action::ModeDiff, Action::ModeMerge];
        let selected = usize::from(self.mode == Mode::Merge);
        let mut mode_rects: Vec<Rect> = Vec::with_capacity(mode_labels.len());
        let seg_pad = if compact {
            SEGMENT_PAD_X_COMPACT
        } else {
            SEGMENT_PAD_X
        };
        let mut x = r.x0 + if compact { 8.0 } else { 16.0 };
        for label in mode_labels {
            let (lw, _) =
                tr.measure_weighted(label, TOOLBAR_SEGMENT_FONT, None, TextWeight::Medium);
            let w = f64::from(lw) + 2.0 * seg_pad;
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
        let btn_pad = if compact {
            BUTTON_PAD_X_COMPACT
        } else {
            BUTTON_PAD_X
        };
        for (label, action) in actions {
            let (lw, _) =
                tr.measure_weighted(label, TOOLBAR_BUTTON_FONT, None, TextWeight::Regular);
            let w = f64::from(lw) + 2.0 * btn_pad;
            let btn = Rect::new(rx - w, seg_y, rx, seg_y + seg_h);
            Button::paint(
                scene,
                transform,
                tr,
                theme,
                &ButtonFace::new(btn, label)
                    .radius(6.0)
                    .text_size(TOOLBAR_BUTTON_FONT)
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
        let now = ui::time::now();
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

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        self.answer_view_operation(request.kind, request.type_id, request.payload)
            .map(|answered| {
                answered.then(|| {
                    ui::widget::ResourceOperationStart::Completed(
                        ui::tilo_actions::ActionOutput::default(),
                    )
                })
            })
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // Pull in any file the host portal picker returned since the last
        // frame (off-thread; never blocks the UI).
        self.drain_portal();
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
        // Drive both views to the content rect directly so mode switches
        // don't require a taffy rebuild to position correctly, and so the
        // views end at the window bottom rather than a toolbar's height below
        // it (the taffy content node is the full root height).
        self.place_views();
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

        // Measure the toolbar band first so the views are placed below the
        // height this frame will actually paint.
        let (_, toolbar_h) = Self::measure_toolbar(tr);
        #[expect(
            clippy::float_cmp,
            reason = "change detection against the cached measurement: any bit difference means the views must be re-placed"
        )]
        let toolbar_changed = toolbar_h != self.toolbar_h;
        if toolbar_changed {
            self.toolbar_h = toolbar_h;
            self.place_views();
        }

        match self.mode {
            Mode::Diff => self.diff.draw(scene, transform, tr, theme, presence),
            Mode::Merge => self.merge.draw(scene, transform, tr, theme, presence),
        }

        self.draw_toolbar(scene, transform, tr, theme);
        self.draw_status_bubble(scene, transform, tr, theme);
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }

        let merge_editor_focused = self.mode == Mode::Merge && self.merge.merged_editor_focused();
        if !merge_editor_focused && self.handle_keyboard_shortcut(event, *ctx.modifiers) {
            *ctx.handled = true;
            return;
        }

        if self.handle_toolbar_click(event) {
            *ctx.handled = true;
            return;
        }

        // Route to the active view.
        match self.mode {
            Mode::Diff => {
                if let Some(n) = self.diff_node {
                    self.diff
                        .handle_event(event, &mut ctx.reborrow(ctx.path, n));
                }
            }
            Mode::Merge => {
                if let Some(n) = self.merge_node {
                    self.merge
                        .handle_event(event, &mut ctx.reborrow(ctx.path, n));
                }
                // A chip click on the merge panel queues its resolution;
                // submit it as the same action the palette's row builds.
                self.submit_resolution();
            }
        }
    }

    fn name(&self) -> &'static str {
        "DiffMergeApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("diffmerge.app")
    }

    // The toolbar is immediate-mode chrome, while the active comparison view
    // owns its own semantic controls. Exposing that live child is what lets the
    // compact source switcher reach the actual Tilo accessibility tree rather
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
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let _ = (state, path);
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
            if self.mode != Mode::Merge || !self.merge.can_resolve(hunk) {
                return false;
            }
            if matches!(action, ui::a11y::Action::Activate) {
                self.merge.resolve(hunk, choice);
                self.submit_resolution();
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
        self.choosing() || self.status_bubble.needs_frame(ui::time::now())
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

impl DiffMergeApp {
    /// Answer a view operation: move around what is open.
    fn answer_view_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
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
        });
        Ok(true)
    }
}

/// What the chooser answered.
enum Pick {
    /// A file's label and text, or why it could not be read.
    Opened(Result<(String, String), String>),
    Cancelled,
    Failed(String),
}

fn label_and_text(path: &Path) -> Result<(String, String), String> {
    let label = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().to_string(),
    );
    Ok((label, read_text(path)?))
}

fn read_text(path: &Path) -> Result<String, String> {
    let failed = |error: &dyn std::fmt::Display| format!("{}: {error}", path.display());
    let (dir, name) = tilo_files::file_at(path).map_err(|e| failed(&e))?;
    let bytes = tilo_files::blocking::read(&dir, &name).map_err(|e| failed(&e))?;
    String::from_utf8(bytes).map_err(|e| failed(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::MergeDoc;
    use crate::resource::DiffmergeShell;
    use tilo_actions::Action as _;
    use ui::framework::testing::TestApp;
    use ui::headless::Framed;
    use ui::tilo_resource::Resource as _;

    /// A document as a new workspace starts.
    fn new_document() -> ui::State {
        let mut state = ui::State::new();
        Diffmerge::initialize(&mut state.doc).unwrap();
        state
    }

    /// The bundled samples, opened in a document through their actions.
    fn sample_document() -> ui::State {
        let mut state = new_document();
        for action in crate::actions::sample_actions() {
            action.apply(&mut state.doc).unwrap();
        }
        state
    }

    /// The app presented `state` and staged for `scenario`, as a capture
    /// shows it.
    fn presented(state: ui::State, scenario: Option<&str>) -> (DiffMergeApp, ui::State) {
        let mut app = DiffMergeApp::new();
        app.present_state(&MergeState::from_doc(&state.doc));
        app.stage_scenario(scenario);
        (app, state)
    }

    /// The samples open on the three-way merge.
    fn demo() -> (DiffMergeApp, ui::State) {
        presented(sample_document(), None)
    }

    /// A screenshot scenario's document and view state.
    fn staged(scenario: Option<&str>) -> (DiffMergeApp, ui::State) {
        let state = if scenario == Some("empty") {
            new_document()
        } else {
            sample_document()
        };
        presented(state, scenario)
    }

    fn write_temp(name: &str, body: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("diffmerge-open-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn sample_app() -> TestApp<DiffmergeShell> {
        let mut app = TestApp::<DiffmergeShell>::new().unwrap();
        app.effect(&DiffmergeEffect::LoadSample).unwrap();
        app.settle();
        app
    }

    fn first_conflict(state: &MergeState) -> usize {
        state
            .model
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .expect("the sample merge has a conflict")
    }

    /// The bounds of the drawn semantic node whose action target is `target`.
    fn target_bounds(app: &mut TestApp<DiffmergeShell>, target: &str) -> Option<Rect> {
        fn in_access(node: &ui::a11y::AccessNode, target: &str) -> Option<Rect> {
            if node.action_target.as_deref() == Some(target) {
                return node.bounds.map(|b| Rect::new(b.x0, b.y0, b.x1, b.y1));
            }
            node.children
                .iter()
                .find_map(|child| in_access(child, target))
        }
        fn in_node(node: &ui::headless::InspectionNode, target: &str) -> Option<Rect> {
            node.semantics
                .as_ref()
                .and_then(|semantics| in_access(semantics, target))
                .or_else(|| {
                    node.children
                        .iter()
                        .find_map(|child| in_node(child, target))
                })
        }
        in_node(&app.harness().inspection().root, target)
    }

    /// Opening files is one effect, whoever asks: two paths are read and
    /// opened as a comparison; without paths the host's chooser is asked
    /// twice, and a cancelled or failed chooser opens nothing and says why.
    #[test]
    fn open_files_reads_paths_or_asks_the_host_chooser() {
        let mut app = TestApp::<DiffmergeShell>::new().unwrap();
        let left = write_temp("Grün links.txt", "Grün\nshared\n");
        let right = write_temp("Blau rechts.txt", "Blau\nshared\nadded\n");
        app.effect(&DiffmergeEffect::OpenFiles {
            left: Some(left.display().to_string()),
            right: Some(right.display().to_string()),
        })
        .unwrap();
        app.settle();
        let comparison = app.state().comparison.expect("the pair is open");
        assert_eq!(comparison.left_label, "Grün links.txt");
        assert_eq!(comparison.right, "Blau\nshared\nadded\n");
        let model = comparison.model();
        assert_eq!((model.left_lines, model.right_lines), (2, 3));

        let half = app.effect(&DiffmergeEffect::OpenFiles {
            left: Some(left.display().to_string()),
            right: None,
        });
        assert!(half.is_err(), "both paths or neither");

        let (services, recorder) = ui::host::HostServices::recording();
        let chosen_left = write_temp("left.txt", "alpha\nbeta\ngamma\n");
        let chosen_right = write_temp("right.txt", "alpha\nBETA\ngamma\ndelta\n");
        recorder.respond_to_pick(PortalOutcome::Opened(chosen_left.clone()));
        recorder.respond_to_pick(PortalOutcome::Opened(chosen_right.clone()));
        ui::host::scoped(&services, || {
            app.effect(&DiffmergeEffect::OpenFiles {
                left: None,
                right: None,
            })
        })
        .expect("both files were chosen");
        app.settle();
        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 2, "one chooser per file");
        assert_eq!(picks[0].title(), "Open a file to compare");
        let comparison = app.state().comparison.unwrap();
        assert_eq!(
            (
                comparison.left_label.as_str(),
                comparison.right_label.as_str()
            ),
            ("left.txt", "right.txt")
        );

        // The recording host cancels once its queued answers run out.
        let cancelled = ui::host::scoped(&services, || {
            app.effect(&DiffmergeEffect::OpenFiles {
                left: None,
                right: None,
            })
        });
        assert!(cancelled.is_err(), "a cancelled chooser opens nothing");
        assert_eq!(app.state().comparison.unwrap().left_label, "left.txt");

        recorder.respond_to_pick(PortalOutcome::Failed("no portal".into()));
        let failed = ui::host::scoped(&services, || {
            app.effect(&DiffmergeEffect::OpenFiles {
                left: None,
                right: None,
            })
        });
        assert!(format!("{failed:?}").contains("no portal"), "{failed:?}");
        app.settle();
        let tree = app.harness().inspection_json().unwrap();
        assert!(tree.contains("File loading failed"), "the failure is shown");

        for path in [left, right, chosen_left, chosen_right] {
            std::fs::remove_dir_all(path.parent().unwrap()).ok();
        }
    }

    /// Taking a side is one action whichever way it is asked for: the
    /// palette's conflict row, a click on the drawn chip, or the chip's
    /// accessibility action. Each is one undo step, and typing in the merged
    /// result afterwards is an edit of the document's text that stops sides
    /// from being taken.
    #[test]
    fn hunk_resolution_is_one_action_from_palette_pointer_and_a11y() {
        let mut app = sample_app();
        let state = app.state();
        assert!(state.composed && state.merge.is_some());
        let hunk = first_conflict(&state);
        let before = state.merged;

        // The palette: the conflict's row.
        let surface = app.agent_surface();
        let conflict = surface
            .as_array()
            .unwrap()
            .iter()
            .find(|row| {
                row["resource_kind"] == "diffmerge.conflict" && row["action"] == "take-theirs"
            })
            .and_then(|row| row["resource"].as_str())
            .expect("the conflict is published")
            .to_owned();
        app.call_on(&conflict, "take-theirs", serde_json::json!({}))
            .unwrap();
        app.settle();
        assert_ne!(app.state().merged, before);
        let resolved = app.read_document(MergeDoc::resolutions);
        assert!(
            matches!(resolved.as_slice(), [(_, choice)] if choice == "theirs"),
            "the row resolved its conflict: {resolved:?}"
        );
        assert!(app.undo(), "one undo step");
        app.settle();
        assert_eq!(app.state().merged, before, "undo took exactly it back");

        // A click on the drawn chip.
        let theirs = target_bounds(&mut app, &format!("diffmerge.merge.resolve:{hunk}:theirs"))
            .expect("the take-theirs chip is drawn");
        app.harness().click_at(theirs.center().x, theirs.center().y);
        app.settle();
        let state = app.state();
        assert_eq!(
            app.read_document(MergeDoc::resolutions),
            vec![(hunk, "theirs".to_string())],
            "the click resolved that hunk"
        );
        assert_ne!(state.merged, before);
        assert!(app.undo());
        app.settle();

        // The accessibility action on the same chip.
        let target = format!("diffmerge.merge.resolve:{hunk}:both");
        assert!(target_bounds(&mut app, &target).is_some());
        app.harness()
            .perform_semantic_named("diffmerge.app", Some(&target), &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
        assert_eq!(
            app.read_document(MergeDoc::resolutions),
            vec![(hunk, "both".to_string())]
        );

        // Typing in the merged result.
        // The merged result fills the bottom of the window.
        let [width, height] = app.harness().inspection().viewport;
        app.harness()
            .click_at(f64::from(width) / 2.0, f64::from(height) - 40.0);
        app.harness().type_text("Z");
        app.settle();
        let state = app.state();
        assert!(state.merged.contains('Z'), "{:?}", state.merged);
        assert!(!state.composed, "a hand edit stops sides from being taken");
        assert!(
            target_bounds(&mut app, &format!("diffmerge.merge.resolve:{hunk}:ours")).is_none(),
            "no chip is offered over a hand edit"
        );
    }

    /// Each screenshot scenario stages through the framework: the samples
    /// through their actions, the mode, error and status through the view.
    #[test]
    fn scenarios_stage_through_actions_and_view_state() {
        let empty = TestApp::<DiffmergeShell>::capture(Some("empty")).unwrap();
        assert!(empty.state().comparison.is_none() && empty.state().merge.is_none());
        for scenario in ["diff", "merge", "error", "status"] {
            let mut app = TestApp::<DiffmergeShell>::capture(Some(scenario)).unwrap();
            let state = app.state();
            assert!(
                state.comparison.is_some() && state.merge.is_some(),
                "{scenario}"
            );
            let tree = app.harness().inspection_json().unwrap();
            match scenario {
                "diff" => assert!(tree.contains("Diff between"), "{scenario}"),
                "merge" | "status" => assert!(tree.contains("Three-way merge"), "{scenario}"),
                _ => assert!(tree.contains("File loading failed"), "{scenario}"),
            }
        }
    }

    /// Mount `app` over the case's `state` in the production frame host and run
    /// one frame, which lays out and draws the merge panel (populating the
    /// resolution-chip hit rects).
    fn merge_frame(
        app: DiffMergeApp,
        state: ui::State,
        viewport: (f32, f32),
    ) -> Framed<DiffMergeApp> {
        frame_scaled(app, state, viewport, 1.0)
    }

    /// [`merge_frame`] at `text_scale`, the way the launcher's `--text-scale`
    /// does: the theme's type tokens and the renderer's scale move together.
    fn frame_scaled(
        app: DiffMergeApp,
        state: ui::State,
        viewport: (f32, f32),
        text_scale: f32,
    ) -> Framed<DiffMergeApp> {
        Framed::new(
            state,
            app,
            ui::headless::viewport(viewport),
            Theme::dark().with_text_scale(text_scale),
        )
    }

    /// Rebuild and run the next frame of a mounted app at a new size.
    fn rerender_merge_frame(app: &mut Framed<DiffMergeApp>, viewport: (f32, f32)) {
        Framed::harness_mut(app)
            .relayout_themed(ui::headless::viewport(viewport), Theme::dark())
            .expect("diffmerge frame lays out");
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

    /// Phone-width layout: the three-way merge and the two-way diff present a
    /// one-source switcher, the toolbar stays on screen without overlap, and a long
    /// error bubble stays inside the viewport. Fresh app per phase.
    #[test]
    fn phone_width_keeps_controls_onscreen_with_one_source_switchers() {
        phone_merge_uses_a_full_width_source_switcher();
        phone_toolbar_keeps_every_control_visible_without_overlap();
        phone_diff_uses_an_accessible_one_source_switcher();
        narrow_status_bubble_stays_within_the_viewport_for_long_errors();
    }

    /// At phone width, a three-way merge intentionally presents one full-width
    /// source pane plus an explicit Base/Ours/Theirs switcher. This guards
    /// against a future layout change quietly restoring three unreadable
    /// columns at 390px.
    fn phone_merge_uses_a_full_width_source_switcher() {
        let (app, state) = demo();
        let mut app = merge_frame(app, state, (390.0, 760.0));

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
        rerender_merge_frame(&mut app, (800.0, 760.0));
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
    fn phone_toolbar_keeps_every_control_visible_without_overlap() {
        let (app, state) = demo();
        let app = merge_frame(app, state, (390.0, 760.0));

        assert_eq!(app.tool_hits.len(), 6, "two modes and four actions");
        let mut rects: Vec<_> = app.tool_hits.iter().map(|hit| hit.rect).collect();
        rects.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        assert!(rects.iter().all(|rect| rect.x0 >= 0.0 && rect.x1 <= 390.0));
        assert!(rects.windows(2).all(|pair| pair[0].x1 <= pair[1].x0));
    }

    /// Measured-text layout at 1x and 200%: toolbar controls, merge chips, merge
    /// rows and the diff gutter all follow the renderer. Fresh apps per phase.
    #[test]
    fn layout_metrics_follow_the_measured_text_at_every_scale() {
        toolbar_controls_fit_their_measured_labels_at_every_text_scale();
        merge_chips_stay_inside_their_pane();
        merge_rows_follow_the_scaled_line_height();
        diff_rows_and_gutter_follow_the_scaled_line_height();
    }

    /// Every toolbar control is sized from its measured label and centred on
    /// the measured bar, at 1× and at 200% text; the content starts below the
    /// bar that was actually painted.
    #[expect(
        clippy::float_cmp,
        reason = "at 1x the scale literal and the authored toolbar height are reproduced exactly"
    )]
    fn toolbar_controls_fit_their_measured_labels_at_every_text_scale() {
        let mut widths_at_1x: Vec<f64> = Vec::new();
        for scale in [1.0f32, 2.0] {
            let (app, state) = demo();
            let app = frame_scaled(app, state, (800.0, 700.0), scale);

            assert_eq!(
                app.tool_hits.len(),
                6,
                "{scale}x: two modes and four actions"
            );
            let mut rects: Vec<_> = app.tool_hits.iter().map(|hit| hit.rect).collect();
            rects.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            for rect in &rects {
                assert!(
                    rect.x0 >= 0.0 && rect.x1 <= 800.0,
                    "{scale}x: control {rect:?} leaves the window"
                );
                assert!(
                    (rect.center().y - app.toolbar_h / 2.0).abs() < 0.01,
                    "{scale}x: control {rect:?} is off the bar's centre line"
                );
                assert!(
                    rect.y1 <= app.toolbar_h + 0.01,
                    "{scale}x: control escapes the bar"
                );
            }
            assert!(
                rects.windows(2).all(|pair| pair[0].x1 <= pair[1].x0),
                "{scale}x: toolbar controls overlap: {rects:?}"
            );
            let active = match app.mode {
                Mode::Diff => app.diff.get_rect(),
                Mode::Merge => app.merge.get_rect(),
            };
            assert!(
                (active.y0 - app.toolbar_h).abs() < 0.01,
                "{scale}x: content {active:?} does not start below the painted bar {}",
                app.toolbar_h
            );
            assert!(
                (active.y1 - 700.0).abs() < 0.01,
                "{scale}x: content ends at the window"
            );

            let widths: Vec<f64> = app.tool_hits.iter().map(|hit| hit.rect.width()).collect();
            if scale == 1.0 {
                assert_eq!(app.toolbar_h, TOOLBAR_H, "1x keeps the design minimum");
                widths_at_1x = widths;
            } else {
                assert!(app.toolbar_h > TOOLBAR_H, "200% text needs a taller bar");
                for (wide, narrow) in widths.iter().zip(&widths_at_1x) {
                    assert!(wide > narrow, "a scaled label needs a wider control");
                }
            }
        }
    }

    /// Every resolution chip paints inside the pane that owns it, at 800 and
    /// 1200 wide and at 200% text: chip widths are measured, and the long
    /// "Take …" labels fall back to the short form when they cannot fit.
    fn merge_chips_stay_inside_their_pane() {
        for (w, h, scale) in [
            (800.0f32, 700.0f32, 1.0f32),
            (1200.0, 800.0, 1.0),
            (800.0, 900.0, 2.0),
        ] {
            let (app, state) = demo();
            let app = frame_scaled(app, state, (w, h), scale);
            let pane = app.merge.get_rect();
            let hits = app.merge.chip_hits();
            assert!(
                !hits.is_empty(),
                "{w}x{h}@{scale}: the demo merge has chips"
            );
            let (_, _, chip_h) = app.merge.metrics_for_test();
            let compact = pane.width() < 640.0;
            let pane_x0 = if compact {
                pane.x0
            } else {
                pane.x0 + pane.width() * 2.0 / 3.0
            };
            for (rect, hunk, _) in &hits {
                assert!(
                    rect.x0 >= pane_x0 - 0.01,
                    "{w}x{h}@{scale}: chip for hunk {hunk} {rect:?} starts before its pane"
                );
                assert!(
                    rect.x1 <= pane.x1 - 8.0 + 0.01,
                    "{w}x{h}@{scale}: chip for hunk {hunk} {rect:?} runs past the pane edge"
                );
                assert!((rect.height() - chip_h).abs() < 0.01);
            }
        }
    }

    /// Row pitch, header band and chip band follow the renderer's measured
    /// line height, so 200% text does not overprint 19px rows.
    fn merge_rows_follow_the_scaled_line_height() {
        let (one, state) = demo();
        let one = frame_scaled(one, state, (1200.0, 800.0), 1.0);
        let (row_1, header_1, chip_1) = one.merge.metrics_for_test();
        let content_1 = one.merge.content_height_for_test();

        let (two, state) = demo();
        let two = frame_scaled(two, state, (1200.0, 800.0), 2.0);
        let (row_2, header_2, chip_2) = two.merge.metrics_for_test();
        let content_2 = two.merge.content_height_for_test();

        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let line_2 = f64::from(tr.measure("Ag", 12.5, None).1);
        assert!(
            row_2 >= line_2,
            "row pitch {row_2} shorter than the 2x line {line_2}"
        );
        assert!(row_2 > row_1 * 1.5 && header_2 > header_1 && chip_2 > chip_1);
        assert!(content_2 > content_1 * 1.5, "spans grow with the rows");
    }

    /// The two-way diff shares the same contract: rows, header and the
    /// line-number gutter grow with the measured text.
    fn diff_rows_and_gutter_follow_the_scaled_line_height() {
        let (mut one, state) = demo();
        one.act(Action::ModeDiff);
        let one = frame_scaled(one, state, (1200.0, 800.0), 1.0);
        let (row_1, header_1, gutter_1) = one.diff.metrics_for_test();

        let (mut two, state) = demo();
        two.act(Action::ModeDiff);
        let two = frame_scaled(two, state, (1200.0, 800.0), 2.0);
        let (row_2, header_2, gutter_2) = two.diff.metrics_for_test();

        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let line_2 = f64::from(tr.measure("Ag", 13.0, None).1);
        let digits_2 = f64::from(tr.measure("0000", 12.0, None).0);
        assert!(row_2 >= line_2 && row_2 > row_1 * 1.5);
        assert!(header_2 > header_1);
        assert!(
            gutter_2 >= digits_2 + 16.0 && gutter_2 > gutter_1,
            "gutter {gutter_2} cannot hold four 2x digits ({digits_2})"
        );
    }

    /// Input parity for the chrome: the compact source switcher by pointer, keyboard
    /// and a11y; toolbar semantics, focus and activation; Tab/Enter order; and the
    /// empty/error content explained to assistive tech. Fresh app per phase.
    #[test]
    fn controls_have_pointer_keyboard_and_semantic_parity() {
        compact_source_switcher_has_pointer_keyboard_and_a11y_parity();
        toolbar_controls_have_semantic_names_focus_geometry_and_activation();
        toolbar_keyboard_tab_and_enter_follow_the_painted_control_order();
        root_accessibility_explains_empty_and_error_content();
    }

    /// Pointer, keyboard, and assistive activation all select the same compact
    /// source state. The test drives the real event/semantic routes rather
    /// than calling the selection helper directly.
    fn compact_source_switcher_has_pointer_keyboard_and_a11y_parity() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, Modifiers};

        let (app, state) = demo();
        let mut app = merge_frame(app, state, (390.0, 760.0));
        let (app, state) = Framed::split(&mut app);
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        app.handle_event(
            &WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(handled);
        assert_eq!(selected_compact_source(app), "Show Theirs source");

        handled = false;
        app.handle_event(
            &keyboard_event(Key::Named(NamedKey::ArrowLeft)),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(handled);
        assert_eq!(selected_compact_source(app), "Show Ours source");

        let theirs_key = ui::a11y::hash_key(&"diffmerge.source.theirs");
        assert!(Widget::perform_action_at(
            &mut app.merge,
            &ui::a11y::Action::Activate,
            &[theirs_key],
            &mut state.doc,
            &state.root,
        ));
        assert_eq!(selected_compact_source(app), "Show Theirs source");
    }

    fn toolbar_controls_have_semantic_names_focus_geometry_and_activation() {
        let (app, state) = demo();
        let mut app = merge_frame(app, state, (980.0, 720.0));
        let (app, state) = Framed::split(&mut app);

        let semantics = Widget::accessibility(&*app).expect("toolbar semantics");
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
            &mut *app,
            &[0, sample_index]
        ));
        assert_eq!(app.toolbar_focus, Some(Action::LoadSample));
        assert!(Widget::perform_semantic_action(
            &mut *app,
            "diffmerge.toolbar.show-diff",
            &ui::a11y::Action::Activate,
            &mut state.doc,
            &state.root,
        ));
        assert_eq!(app.mode, Mode::Diff);
    }

    fn toolbar_keyboard_tab_and_enter_follow_the_painted_control_order() {
        use winit::event::Modifiers;

        let (mut app, mut state) = demo();
        let mut handled = false;
        let mut focus = None;
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let modifiers = Modifiers::default();
        app.handle_event(
            &keyboard_event(Key::Named(NamedKey::Tab)),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert!(handled);
        assert_eq!(app.toolbar_focus, Some(Action::ModeDiff));
        handled = false;
        app.handle_event(
            &keyboard_event(Key::Named(NamedKey::Tab)),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert_eq!(app.toolbar_focus, Some(Action::ModeMerge));
        app.handle_event(
            &keyboard_event(Key::Named(NamedKey::Enter)),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        assert_eq!(app.mode, Mode::Merge);
    }

    fn phone_diff_uses_an_accessible_one_source_switcher() {
        let (mut app, state) = demo();
        app.set_mode_for_test(Mode::Diff);
        let mut app = merge_frame(app, state, (390.0, 760.0));
        let (app, state) = Framed::split(&mut app);
        let root = Widget::accessibility(&*app).expect("diff semantics");
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
            &mut *app,
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
        let mut state = ui::State::new();
        let payload = serde_json::to_vec(&DiffmergeOperation::ShowDiffBlock {
            block: 0,
            fingerprint: stale_fingerprint,
        })
        .expect("operation serializes");

        let error = Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                ui::tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                crate::actions::DIFFMERGE_OPERATION_TYPE,
                &payload,
            ),
            &mut state.doc,
            &state.root,
        )
        .expect_err("stale local operation must explain why its target is unsafe");
        assert!(error.contains("comparison changed"));
    }

    fn narrow_status_bubble_stays_within_the_viewport_for_long_errors() {
        let (app, state) = staged(Some("error"));
        let app = merge_frame(app, state, (390.0, 760.0));
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

    fn root_accessibility_explains_empty_and_error_content() {
        let (empty, state) = staged(Some("empty"));
        let empty = merge_frame(empty, state, (980.0, 720.0));
        let empty_semantics = Widget::accessibility(&*empty).expect("empty semantics");
        let names = empty_semantics
            .children
            .iter()
            .filter_map(|child| child.name.as_deref())
            .collect::<Vec<_>>();
        assert!(names.contains(&"No files loaded"), "{names:?}");

        let (error, state) = staged(Some("error"));
        let error = merge_frame(error, state, (980.0, 720.0));
        let error_semantics = Widget::accessibility(&*error).expect("error semantics");
        let names = error_semantics
            .children
            .iter()
            .filter_map(|child| child.name.as_deref())
            .collect::<Vec<_>>();
        assert!(names.contains(&"File loading failed"), "{names:?}");
    }
}
