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
use std::path::Path;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::permissions::PermissionContext;
use ui::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, PickRequest, PortalChannel, PortalOutcome,
};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, SegmentedFace, ToggleGroup, Widget};

use crate::actions::DiffmergeOperation;
use crate::diff_view::DiffView;
use crate::merge_view::MergeView;
use crate::samples;

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
    /// A chooser delivering contents, where files have no path (the web).
    portal_contents_rx: Option<ContentsChannel>,
    /// Files chosen but not yet paired (first of a two-file diff selection).
    pending_left: Option<(String, String)>,
    /// A human-readable reason the last host-portal open failed, surfaced in
    /// the status bar. `None` in the normal (success / cancel) case.
    load_error: Option<String>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    pending_resource_paths: Option<(
        tilo_actions::InvocationId,
        std::path::PathBuf,
        std::path::PathBuf,
    )>,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    dirty: bool,
    /// One-shot flag: set when the active mode changed so the host rebuilds the
    /// taffy tree (the flex content child swaps between the two views).
    structure_dirty: bool,
    /// Framework-owned action dispatch: every on-panel affordance routes its
    /// [`DiffmergeAction`] through here so it brackets **one** undo boundary,
    /// encodes under [`DIFFMERGE_ACTION_TYPE`](crate::actions::DIFFMERGE_ACTION_TYPE),
    /// and replays byte-identically on peers — the same path Ctrl+K takes.
    actions: crate::resource::ActionSink,
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
    /// Fingerprint of the last shared three-way projection written to the doc.
    shared_merge_fingerprint: Option<String>,
}

impl DiffMergeApp {
    /// The live app: starts EMPTY (blank panes + the "Open files" / "Load
    /// sample" toolbar actions), never a fabricated diff.
    pub fn new() -> Self {
        let mut app = Self::build(false);
        // Where files have no path, the pair last compared.
        if !crate::web_file::native_paths()
            && let Some(file) = crate::web_file::recall()
            && file.name == "pair"
            && let Ok([(left_label, left), (right_label, right)]) =
                serde_json::from_slice::<[(String, String); 2]>(&file.bytes)
        {
            app.diff
                .set_inputs(&left_label, &left, &right_label, &right);
            app.mode = Mode::Diff;
            app.structure_dirty = true;
        }
        app
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
            portal_contents_rx: None,
            pending_left: None,
            load_error: None,
            resource_invocation: None,
            pending_resource_paths: None,
            resource_result: None,
            dirty: true,
            structure_dirty: false,
            actions: crate::resource::ActionSink::default(),
            status_bubble: ui::StatusBubble::new(),
            status_bubble_rect: Rect::ZERO,
            last_status: String::new(),
            shots_status: false,
            toolbar_focus: None,
            toolbar_h: TOOLBAR_H,
            shared_merge_fingerprint: None,
        }
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
    /// applied through the Tilo action executor (a pinned actor + committed
    /// change, so it replays byte-identically on peers), exactly as the palette
    /// does. The document heads captured just before the first action of the
    /// event are stashed so the host records **one** undo boundary bracketing it
    /// ([`CommittedActions::take_committed_action`](ui::undo::CommittedActions::take_committed_action)). The in-memory merge model was already
    /// updated by the chip handler for immediate feedback and is reconciled from
    /// the document on the next `sync`.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &crate::actions::DiffmergeAction) {
        self.merge.reconcile_content(doc);
        if !self.merge.context_matches(doc) {
            return;
        }
        self.actions.dispatch_with_context(
            doc,
            crate::actions::DIFFMERGE_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.diffmerge", "Diff", action),
        );
        self.merge.acknowledge_content(doc);
    }

    /// Show the merged buffer the framework resource already holds.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::doc::MergeState) {
        self.merge.adopt_merged(&state.merged);
        if !state.model.hunks.is_empty() {
            self.merge.adopt_shared_model(&state.title, &state.model);
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Diffmerge>,
    ) {
        self.actions.attach(dispatcher);
        self.merge.set_framework_hosted(true);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    #[cfg(test)]
    pub(crate) fn set_mode_for_test(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            self.structure_dirty = true;
        }
    }

    /// Label for an in-flight open-files job, when one is running.
    pub(crate) fn open_job_label(&self) -> Option<String> {
        if self.portal_rx.is_some()
            || self.pending_resource_paths.is_some()
            || self.resource_invocation.is_some()
        {
            Some("Opening files…".into())
        } else {
            None
        }
    }

    /// Write the shared three-way projection when merge inputs move.
    fn publish_shared_merge_if_needed(&mut self, doc: &mut AutoCommit) {
        let Some(shared) = self.merge.shared_merge().cloned() else {
            return;
        };
        let fingerprint = shared.fingerprint();
        if self.shared_merge_fingerprint.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        if self.actions.is_hosted() {
            let json = serde_json::to_string(&shared).unwrap_or_else(|_| "{}".into());
            let action = crate::actions::DiffmergeAction::WriteSharedMerge { json };
            self.actions.dispatch_with_context(
                doc,
                crate::actions::DIFFMERGE_ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::for_action("so.tilo.diffmerge", "Diff", &action),
            );
        } else {
            // Standalone seeds write the projection beside the direct merged
            // buffer so ActionHost's undo bracket stays free for user actions.
            crate::doc::SharedMerge::write(doc, &shared);
        }
        self.shared_merge_fingerprint = Some(fingerprint);
        self.merge.acknowledge_content(doc);
    }

    /// The last action dispatched, for the `affordance == ActionSpec` covenant
    /// test.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The presentation context Ctrl+K needs to build the merge resource tree.
    /// `None` unless a three-way merge with at least one change hunk is the
    /// active mode — the diff mode is read-only and mutates nothing.
    pub(crate) fn merge_resource_ctx(
        &self,
        doc: &AutoCommit,
    ) -> Option<crate::actions::MergeResourceCtx> {
        if self.mode != Mode::Merge {
            return None;
        }
        if self.merge.context_matches(doc) {
            return self.merge.resource_ctx();
        }
        crate::doc::MergeDoc::has_merged(doc).then(|| crate::actions::MergeResourceCtx {
            title: "Saved merge output · original inputs unavailable".into(),
            model: crate::model::MergeModel::default(),
        })
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
        if self.portal_rx.is_some() || self.portal_contents_rx.is_some() {
            return;
        }
        self.load_error = None;
        if crate::web_file::native_paths() {
            self.portal_rx = Some(diff_open_request().send());
        } else {
            self.portal_contents_rx = Some(diff_open_request().send_contents());
        }
        self.dirty = true;
    }

    /// Drain a completed portal request, if any. Called once per frame from
    /// [`Widget::sync`]. Loads the chosen file (stashing the first of the pair
    /// and re-opening the chooser for the second), records a failure for the
    /// status bar, and clears the in-flight channel. Never blocks — a still
    /// pending picker leaves the channel installed for the next frame.
    fn drain_portal(&mut self) {
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
            // Still waiting on the host dialog, or the worker vanished.
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
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

    /// Apply one chooser answer to the two-file selection.
    fn apply_pick(&mut self, outcome: Pick) {
        {
            {
                match outcome {
                    Pick::Opened(picked) => {
                        // A `true` return means the left file just landed and
                        // the right one is still needed — fire a fresh request.
                        match picked.map(|(label, text)| self.consume_text(label, text)) {
                            Ok(true) => self.request_open_via_portal(),
                            Ok(false) => {
                                if let Some(invocation) = self.resource_invocation.take() {
                                    self.resource_result =
                                        Some(tilo_actions::InvocationUpdate::Succeeded {
                                            invocation,
                                            output: tilo_actions::ActionOutput::default(),
                                        });
                                }
                            }
                            Err(reason) => {
                                self.load_error = Some(reason.clone());
                                if let Some(invocation) = self.resource_invocation.take() {
                                    self.resource_result =
                                        Some(tilo_actions::InvocationUpdate::Failed {
                                            invocation,
                                            error: tilo_actions::ActionFailure {
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
                    Pick::Cancelled => {
                        self.pending_left = None;
                        if let Some(invocation) = self.resource_invocation.take() {
                            self.resource_result =
                                Some(tilo_actions::InvocationUpdate::Cancelled {
                                    invocation,
                                    message: Some("file selection cancelled".into()),
                                });
                        }
                        self.dirty = true;
                    }
                    Pick::Failed(reason) => {
                        self.pending_left = None;
                        self.load_error = Some(format!("Couldn't open the file chooser: {reason}"));
                        if let Some(invocation) = self.resource_invocation.take() {
                            self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                                invocation,
                                error: tilo_actions::ActionFailure {
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
        }
    }

    /// Consume one portal-chosen path. The first file of a pair is stashed and
    /// `true` is returned so the caller re-opens the chooser for the second;
    /// the second file completes the diff and switches to Diff mode, returning
    /// `false`. Pure w.r.t. the portal (reads the file, mutates model state) so
    /// the two-file flow is unit-testable without a host dialog.
    #[cfg(test)]
    fn consume_pick(&mut self, path: &Path) -> Result<bool, String> {
        let (label, content) = label_and_text(path)?;
        Ok(self.consume_text(label, content))
    }

    /// As [`Self::consume_pick`], for a file already read.
    fn consume_text(&mut self, label: String, content: String) -> bool {
        match self.pending_left.take() {
            None => {
                // First file picked — stash and ask for the second.
                self.pending_left = Some((label, content));
                true
            }
            Some((left_label, left_content)) => {
                self.diff
                    .set_inputs(&left_label, &left_content, &label, &content);
                if !crate::web_file::native_paths()
                    && let Ok(pair) =
                        serde_json::to_vec(&[(&left_label, &left_content), (&label, &content)])
                {
                    crate::web_file::remember("pair", &pair);
                }
                self.mode = Mode::Diff;
                self.structure_dirty = true;
                false
            }
        }
    }

    fn load_paths(&mut self, left: &Path, right: &Path) -> Result<(), String> {
        let left_content = read_text(left)?;
        let right_content = read_text(right)?;
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
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::DIFFMERGE_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if cancellation_requested
            && self
                .pending_resource_paths
                .as_ref()
                .is_some_and(|(id, _, _)| *id == invocation)
        {
            self.pending_resource_paths = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("cancelled before files were read".into()),
            });
        }
        if cancellation_requested && self.resource_invocation == Some(invocation) {
            self.resource_invocation = None;
            self.portal_rx = None;
            self.pending_left = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
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

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        if let Some((invocation, left, right)) = self.pending_resource_paths.take() {
            let result = self.load_paths(&left, &right);
            self.resource_result = Some(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
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
        let mut seeded = false;
        {
            let (doc, _) = ui::doc::document(state, path);
            self.publish_shared_merge_if_needed(doc);
            if self.actions.is_hosted()
                && let Some(merged) = self.merge.take_seed_request()
            {
                let action = crate::actions::DiffmergeAction::SeedMerge { merged };
                self.actions.dispatch_with_context(
                    doc,
                    crate::actions::DIFFMERGE_ACTION_TYPE,
                    &action,
                    tilo_actions::ActionContext::for_action("so.tilo.diffmerge", "Diff", &action),
                );
                self.merge.acknowledge_content(doc);
                seeded = true;
            }
        }
        if seeded {
            self.merge.sync(state, path);
        }
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
        self.merge.reconcile_content(ctx.state);
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
                // A chip click on the merge panel queues its resolution; dispatch
                // it through the shared action path so the chip and Ctrl+K produce
                // the same replayable action under one undo boundary.
                if let Some(action) = self.merge.take_pending_action() {
                    self.perform_action(ctx.state, &action);
                    *ctx.handled = true;
                }
            }
        }
    }

    fn name(&self) -> &'static str {
        "DiffMergeApp"
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
        let (state, _) = ui::doc::document(state, path);
        self.merge.reconcile_content(state);
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
        self.portal_rx.is_some() || self.status_bubble.needs_frame(ui::time::now())
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

impl ui::resources::PublishesResources for DiffMergeApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the workspace door — keep only local two-way comparisons and in-flight
    /// open jobs.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self)
                .into_iter()
                .find(|resource| resource.id.as_str() == id);
        }
        self.publish_resource(doc, id)
    }
}

impl ui::history_mode::capability::DocumentHistory for DiffMergeApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
    fn document_action_applied(
        &mut self,
        action: &tilo_actions::EncodedAction,
        before: &[ChangeHash],
        doc: &AutoCommit,
        _path: &ObjId,
    ) {
        if action.type_id == crate::actions::DIFFMERGE_ACTION_TYPE
            && doc
                .clone()
                .fork_at(before)
                .ok()
                .is_some_and(|prior| self.merge.context_matches(&prior))
        {
            self.merge.acknowledge_content(doc);
        } else {
            self.merge.reconcile_content(doc);
        }
    }
}

impl ui::undo::CommittedActions for DiffMergeApp {
    /// Hand the host the heads captured before an on-panel chip dispatched its
    /// resolution action, so it records one undo boundary for it (see
    /// `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }
}

impl DiffMergeApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
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
            DiffmergeOperation::OpenFiles { .. } => Action::Open,
            DiffmergeOperation::LoadSample => Action::LoadSample,
        });
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
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
                .answer_resource_operation(kind, type_id, &encoded, doc, path)
                .map(|handled| {
                    handled.then(|| {
                        ui::widget::ResourceOperationStart::Completed(
                            ui::tilo_actions::ActionOutput::default(),
                        )
                    })
                });
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
    use ui::headless::Framed;

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

    /// Press the "Open files" toolbar action under a recording host that
    /// answers the file chooser with `outcome`.
    fn open_with_outcome(app: &mut DiffMergeApp, outcome: PortalOutcome) {
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(outcome);
        ui::host::scoped(&services, || app.act(Action::Open));
        assert_eq!(
            recorder.pick_requests().len(),
            1,
            "one chooser was requested"
        );
    }

    /// Opening files: a direct pair via the resource operation, the toolbar Open
    /// action reaching the host portal (with in-flight guard), the stash-left step,
    /// and portal success / cancel / failure. Each phase uses a fresh app, temp
    /// files and recording host.
    #[test]
    fn open_files_flow_through_direct_operation_and_host_portal() {
        direct_pair_is_accepted_then_succeeds();
        open_action_routes_to_the_host_portal();
        first_pick_stashes_left_and_asks_for_the_right();
        portal_pick_completes_two_file_diff_via_frame_drain();
        portal_cancel_clears_pending_selection();
        portal_failure_surfaces_on_the_status_line();
    }

    fn direct_pair_is_accepted_then_succeeds() {
        let (mut app, mut state) = app_with_state();
        let left = write_temp("Grün links.txt", "Grün\nshared\n");
        let right = write_temp("Blau rechts.txt", "Blau\nshared\nadded\n");
        let invocation = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&crate::actions::DiffmergeOperation::OpenFiles {
            left: Some(left.display().to_string()),
            right: Some(right.display().to_string()),
        })
        .unwrap();
        let started = Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                invocation,
                tilo_actions::OperationKind::Effect,
                crate::actions::DIFFMERGE_OPERATION_TYPE,
                &payload,
            ),
            &mut state.doc,
            &state.root,
        )
        .unwrap()
        .unwrap();
        assert_eq!(started, ui::widget::ResourceOperationStart::Accepted);
        app.sync(&mut state.doc, &state.root);
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &state.doc, &state.root),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        let model = app.diff.model();
        assert_eq!(model.left_lines, 2);
        assert_eq!(model.right_lines, 3);
        assert!(model.modified + model.added > 0);
        std::fs::remove_dir_all(left.parent().unwrap()).ok();
        std::fs::remove_dir_all(right.parent().unwrap()).ok();
    }

    /// The "Open files" toolbar action must reach the host portal, never an
    /// in-app dialog. A recording host stands in for the desktop, and the
    /// in-flight guard must keep a second press from stacking another dialog.
    fn open_action_routes_to_the_host_portal() {
        let (mut app, _st) = app_with_state();
        let (services, recorder) = ui::host::HostServices::recording();
        ui::host::scoped(&services, || {
            app.act(Action::Open);
            app.act(Action::Open);
        });
        assert!(
            app.portal_rx.is_some(),
            "Open routes through the host-portal request path (not an in-app picker)"
        );
        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1, "the in-flight guard drops the second press");
        assert_eq!(picks[0].title(), "Open a file to compare");
    }

    /// The first file of a diff pair is stashed and a follow-up pick is
    /// requested (`consume_pick` returns true) — no host dialog is spawned by
    /// the consume step itself.
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
    fn portal_pick_completes_two_file_diff_via_frame_drain() {
        let (mut app, mut st) = app_with_state();
        app.pending_left = Some(("left.txt".into(), "alpha\nbeta\ngamma\n".into()));
        let right = write_temp("right.txt", "alpha\nBETA\ngamma\ndelta\n");

        open_with_outcome(&mut app, PortalOutcome::Opened(right.clone()));
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
    fn portal_cancel_clears_pending_selection() {
        let (mut app, mut st) = app_with_state();
        app.pending_left = Some(("left.txt".into(), "x\n".into()));
        open_with_outcome(&mut app, PortalOutcome::Cancelled);
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
    fn portal_failure_surfaces_on_the_status_line() {
        let (mut app, mut st) = app_with_state();
        open_with_outcome(&mut app, PortalOutcome::Failed("no portal".into()));
        app.sync(&mut st.doc, &st.root);
        assert!(app.pending_left.is_none());
        assert!(app.portal_rx.is_none());
        let err = app.load_error.as_deref().expect("a failure is recorded");
        assert!(err.contains("no portal"), "the reason is surfaced: {err}");
    }

    /// One resolution action behind every entry point: the chip dispatch records one
    /// undo boundary and reverts, chip and palette encode identically, a pointer click
    /// on the drawn chip resolves that hunk, and the root accessibility tree resolves
    /// through the CRDT path. Fresh demo app and state per phase.
    #[test]
    fn hunk_resolution_is_one_action_from_chip_palette_pointer_and_a11y() {
        chip_dispatch_records_one_undo_boundary_and_reverts();
        chip_and_palette_encode_identically();
        clicking_a_resolution_chip_resolves_that_hunk_through_the_pointer_path();
        root_accessibility_includes_active_hunks_and_uses_the_crdt_resolution_path();
    }

    /// The full covenant on one on-panel affordance: driving the chip handler
    /// exactly as a click does queues a resolution; dispatching it through the
    /// app's shared action path mutates the merged buffer, records **one** undo
    /// boundary bracketed at the pre-action heads, and Ctrl+Z reverts precisely
    /// that action.
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
        let resources = ui::resources::resources(&app, &state.doc);
        let conflict = resources[0]
            .children
            .iter()
            .find(|r| r.actions.iter().find(|a| a.id == "take-theirs").is_some_and(|spec| {
                let tilo_actions::ActionInput::Immediate(encoded) = &spec.input else { return false };
                matches!(serde_json::from_slice::<crate::actions::DiffmergeAction>(&encoded.payload), Ok(crate::actions::DiffmergeAction::ResolveHunk { hunk: target, .. }) if target == hunk as u64)
            }))
            .expect("the conflict resource");
        let spec = conflict
            .actions
            .iter()
            .find(|a| a.id == "take-theirs")
            .unwrap();
        let from_spec = match &spec.input {
            tilo_actions::ActionInput::Immediate(e) => e.clone(),
            tilo_actions::ActionInput::Prompted { .. }
            | tilo_actions::ActionInput::Deferred { .. } => panic!("take-theirs is immediate"),
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
        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
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
        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
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
            let mut app = DiffMergeApp::new_demo();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
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
            let mut app = DiffMergeApp::new_demo();
            let mut state = ui::State::new();
            app.sync(&mut state.doc, &state.root);
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
        let mut one = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        one.sync(&mut state.doc, &state.root);
        let one = frame_scaled(one, state, (1200.0, 800.0), 1.0);
        let (row_1, header_1, chip_1) = one.merge.metrics_for_test();
        let content_1 = one.merge.content_height_for_test();

        let mut two = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        two.sync(&mut state.doc, &state.root);
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
        let mut one = DiffMergeApp::new_demo();
        one.act(Action::ModeDiff);
        let mut state = ui::State::new();
        one.sync(&mut state.doc, &state.root);
        let one = frame_scaled(one, state, (1200.0, 800.0), 1.0);
        let (row_1, header_1, gutter_1) = one.diff.metrics_for_test();

        let mut two = DiffMergeApp::new_demo();
        two.act(Action::ModeDiff);
        let mut state = ui::State::new();
        two.sync(&mut state.doc, &state.root);
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

        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
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
        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
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

        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
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
        let mut app = DiffMergeApp::new_demo();
        app.mode = Mode::Diff;
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
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
        let mut app = DiffMergeApp::for_shot(Some("error"));
        app.arm_status_bubble_for_shots();
        let app = merge_frame(app, ui::State::new(), (390.0, 760.0));
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

    fn root_accessibility_includes_active_hunks_and_uses_the_crdt_resolution_path() {
        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let mut app = merge_frame(app, state, (980.0, 720.0));
        let (app, state) = Framed::split(&mut app);

        let root = Widget::accessibility(&*app).expect("root semantics");
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
            &mut *app,
            &target,
            &ui::a11y::Action::Activate,
            &mut state.doc,
            &state.root,
        ));
        assert_ne!(crate::doc::MergeDoc::merged_text(&state.doc), before);
    }

    fn root_accessibility_explains_empty_and_error_content() {
        let empty = DiffMergeApp::for_shot(Some("empty"));
        let empty = merge_frame(empty, ui::State::new(), (980.0, 720.0));
        let empty_semantics = Widget::accessibility(&*empty).expect("empty semantics");
        let names = empty_semantics
            .children
            .iter()
            .filter_map(|child| child.name.as_deref())
            .collect::<Vec<_>>();
        assert!(names.contains(&"No files loaded"), "{names:?}");

        let error = DiffMergeApp::for_shot(Some("error"));
        let error = merge_frame(error, ui::State::new(), (980.0, 720.0));
        let error_semantics = Widget::accessibility(&*error).expect("error semantics");
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
    fn clicking_a_resolution_chip_resolves_that_hunk_through_the_pointer_path() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{DeviceId, Modifiers};

        let mut app = DiffMergeApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let mut app = merge_frame(app, state, (1200.0, 760.0));
        let (app, state) = Framed::split(&mut app);

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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &m,
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
                modifiers: &m,
                state: &mut state.doc,
                path: &state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
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

#[cfg(test)]
mod history_input_tests {
    use super::*;
    use crate::doc::MergeDoc;
    #[test]
    fn restored_output_invalidates_queued_hunks_even_with_same_label_and_choices() {
        let mut app = DiffMergeApp::new_demo();
        app.mode = Mode::Merge;
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let hunk = app.merge.model().change_indices()[0];
        app.merge.resolve(hunk, crate::actions::HunkChoice::Theirs);
        let pending = app.merge.take_pending_action().unwrap();
        MergeDoc::set_merged(
            &mut doc,
            "Restored output without matching original inputs\n",
        );
        let heads = doc.get_heads();
        assert!(
            ui::resources::resources(&app, &doc)
                .iter()
                .flat_map(|r| &r.children)
                .all(|r| r.kind != "diffmerge.conflict")
        );
        app.perform_action(&mut doc, &pending);
        assert_eq!(doc.get_heads(), heads);
        assert_eq!(
            MergeDoc::merged_text(&doc),
            "Restored output without matching original inputs\n"
        );
        assert!(!app.perform_semantic_action(
            &format!("diffmerge.merge.resolve:{hunk}:ours"),
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT
        ));
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            MergeDoc::merged_text(&doc),
            "Restored output without matching original inputs\n"
        );
    }
}
