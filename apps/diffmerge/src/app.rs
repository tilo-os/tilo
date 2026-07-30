//! Root widget — orchestrates the diff and merge modes, the top toolbar
//! (mode switch, change navigation, open), and the host-portal file open.
//!
//! Data flow:
//! * The app holds the raw input texts in memory and the two derived views
//!   ([`DiffView`], [`MergeView`]).
//! * The toolbar's buttons are drawn directly and hit-tested in `handle_event`.
//! * Opening files goes through the **host** file chooser (the XDG Desktop
//!   Portal, [`crate::portal`]) — the app never draws its own picker (HIG §5).
//!   The chosen paths are read and re-seed the active mode (two files → diff;
//!   the merge demo stays sample-driven since a three-way pick needs three
//!   roots).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::color_compat::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, SegmentedFace, ToggleGroup, Widget};

use crate::diff_view::DiffView;
use crate::merge_view::MergeView;
use crate::samples;

const TOOLBAR_H: f64 = 48.0;
const STATUS_H: f64 = 26.0;

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

#[derive(Clone, Copy, PartialEq, Eq)]
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
    portal_rx: Option<crate::portal::PortalChannel>,
    /// Files chosen but not yet paired (first of a two-file diff selection).
    pending_left: Option<(String, String)>,
    /// A human-readable reason the last host-portal open failed, surfaced in
    /// the status bar. `None` in the normal (success / cancel) case.
    load_error: Option<String>,
    dirty: bool,
    /// One-shot flag: set when the active mode changed so the host rebuilds the
    /// taffy tree (the flex content child swaps between the two views).
    structure_dirty: bool,
    /// Doc heads captured just before an on-panel affordance dispatched its
    /// [`DiffmergeAction`], handed to the host so it brackets **one** undo
    /// boundary around the action ([`Widget::take_committed_action`]). Rising-
    /// edge: cleared on read.
    action_commit_before: Option<Vec<ChangeHash>>,
    /// The last action encoded by [`Self::perform_action`], kept so a test can
    /// prove the affordance dispatched the expected envelope.
    last_dispatched: Option<qos_actions::EncodedAction>,
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
            dirty: true,
            structure_dirty: false,
            action_commit_before: None,
            last_dispatched: None,
        }
    }

    fn content_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0 + TOOLBAR_H,
            self.rect.x1,
            self.rect.y1 - STATUS_H,
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
                    &base.label, &base.text, &ours.label, &ours.text, &theirs.label,
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
    fn perform_action(&mut self, doc: &mut AutoCommit, action: crate::actions::DiffmergeAction) {
        if self.action_commit_before.is_none() {
            // `get_heads` flushes any pending app writes, so the bracket covers
            // only the action's own change.
            self.action_commit_before = Some(doc.get_heads());
        }
        let encoded = crate::actions::encode(&action);
        self.last_dispatched = Some(encoded.clone());
        let mut tx = DiscardTransport;
        if let Err(e) = qos_actions::dispatch(doc, &action, encoded, &mut tx) {
            tracing::warn!(error = %e, "diffmerge action dispatch failed");
        }
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

    /// Ask the host desktop's file chooser to open a file to compare. Per HIG
    /// §5 the app never draws its own picker: the portal round-trip runs on a
    /// worker thread ([`crate::portal::request_open`]) and the chosen path
    /// comes back over a channel drained each frame in [`Self::drain_portal`].
    /// A diff needs two files, so the drain re-fires this once for the right
    /// file after the left is picked. A second request while one is in flight
    /// is ignored, so a double-click can't stack two host dialogs.
    fn request_open_via_portal(&mut self) {
        if self.portal_rx.is_some() {
            return;
        }
        self.load_error = None;
        self.portal_rx = Some(crate::portal::request_open());
        self.dirty = true;
    }

    /// Drain a completed portal request, if any. Called once per frame from
    /// [`Widget::sync`]. Loads the chosen file (stashing the first of the pair
    /// and re-opening the chooser for the second), records a failure for the
    /// status bar, and clears the in-flight channel. Never blocks — a still
    /// pending picker leaves the channel installed for the next frame.
    fn drain_portal(&mut self) {
        let Some(rx) = self.portal_rx.as_ref() else { return };
        match rx.try_recv() {
            Ok(outcome) => {
                self.portal_rx = None;
                match outcome {
                    crate::portal::PortalOutcome::Opened(path) => {
                        // A `true` return means the left file just landed and
                        // the right one is still needed — fire a fresh request.
                        if self.consume_pick(&path) {
                            self.request_open_via_portal();
                        }
                        self.dirty = true;
                    }
                    // A cancel abandons an in-progress two-file selection.
                    crate::portal::PortalOutcome::Cancelled => {
                        self.pending_left = None;
                        self.dirty = true;
                    }
                    crate::portal::PortalOutcome::Failed(reason) => {
                        self.pending_left = None;
                        self.load_error = Some(format!("Couldn't open the file chooser: {reason}"));
                        self.dirty = true;
                    }
                }
            }
            // Still waiting on the host dialog, or the worker vanished.
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
            }
        }
    }

    /// Consume one portal-chosen path. The first file of a pair is stashed and
    /// `true` is returned so the caller re-opens the chooser for the second;
    /// the second file completes the diff and switches to Diff mode, returning
    /// `false`. Pure w.r.t. the portal (reads the file, mutates model state) so
    /// the two-file flow is unit-testable without a host dialog.
    fn consume_pick(&mut self, path: &Path) -> bool {
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let content = fs::read_to_string(path)
            .unwrap_or_else(|e| format!("<could not read {}: {e}>", path.display()));
        match self.pending_left.take() {
            None => {
                // First file picked — stash and ask for the second.
                self.pending_left = Some((label, content));
                true
            }
            Some((llabel, lcontent)) => {
                self.diff.set_inputs(&llabel, &lcontent, &label, &content);
                self.mode = Mode::Diff;
                self.structure_dirty = true;
                false
            }
        }
    }

    fn draw_toolbar(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.tool_hits.clear();
        let c = &theme.tokens.colors;
        let r = self.rect;
        let bar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOOLBAR_H);
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(Point::new(r.x0, r.y0 + TOOLBAR_H), Point::new(r.x1, r.y0 + TOOLBAR_H)),
        );

        // App title.
        tr.draw_text(
            scene,
            "Diff",
            17.0,
            c.text_primary,
            transform * Affine::translate((r.x0 + 16.0, r.y0 + 14.0)),
            None,
        );

        // Mode segmented control (shared pill-style segmented control).
        let seg_y = r.y0 + 10.0;
        let seg_h = 28.0;
        let mode_labels: [&str; 2] = ["Diff", "3-way merge"];
        let mode_actions = [Action::ModeDiff, Action::ModeMerge];
        let selected = if self.mode == Mode::Merge { 1 } else { 0 };
        let mut mode_rects: Vec<Rect> = Vec::with_capacity(mode_labels.len());
        let mut x = r.x0 + 70.0;
        for label in mode_labels {
            let w = label.len() as f64 * 7.4 + 22.0;
            mode_rects.push(Rect::new(x, seg_y, x + w, seg_y + seg_h));
            x += w + 6.0;
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
            self.tool_hits.push(ToolHit { rect: *rect, action });
        }

        // Right-aligned action buttons.
        let mut rx = r.x1 - 16.0;
        for (label, action) in [
            ("Open files", Action::Open),
            ("Load sample", Action::LoadSample),
            ("Next ›", Action::NextChange),
            ("‹ Prev", Action::PrevChange),
        ] {
            let w = label.len() as f64 * 7.2 + 20.0;
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
                    .hovered(btn.contains(self.cursor)),
            );
            self.tool_hits.push(ToolHit { rect: btn, action });
            rx -= w + 8.0;
        }
    }

    fn draw_status(&self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let r = self.rect;
        let bar = Rect::new(r.x0, r.y1 - STATUS_H, r.x1, r.y1);
        scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &bar);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            c.border_standard,
            None,
            &Line::new(Point::new(r.x0, r.y1 - STATUS_H), Point::new(r.x1, r.y1 - STATUS_H)),
        );

        // A host-portal failure takes over the status line until the next
        // successful open clears it.
        let (text, tcol) = if let Some(err) = &self.load_error {
            (err.clone(), c.semantic.danger)
        } else {
            let text = match self.mode {
                Mode::Diff => {
                    let m = self.diff.model();
                    let active = self
                        .diff
                        .active_block()
                        .map(|b| format!("  ·  change {}/{}", b + 1, m.blocks.len().max(1)))
                        .unwrap_or_default();
                    format!(
                        "+{} additions   −{} deletions   ~{} modifications   {} change block(s){}",
                        m.added, m.removed, m.modified, m.blocks.len(), active
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
            };
            let tcol = match self.mode {
                Mode::Merge if self.merge.model().conflict_count() > 0 => c.semantic.danger,
                Mode::Merge => c.semantic.success,
                _ => c.text_dim,
            };
            (text, tcol)
        };
        tr.draw_text(
            scene,
            &text,
            12.0,
            tcol,
            transform * Affine::translate((r.x0 + 16.0, r.y1 - STATUS_H + 6.0)),
            None,
        );
    }
}

impl Widget for DiffMergeApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
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
        node_map.insert(root, self as *mut dyn Widget);
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
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
        self.draw_status(scene, transform, tr, theme);
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

        // Keyboard shortcuts: F8/F7 or n/p for next/prev change, 1/2 for mode.
        if let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
            ..
        } = event
        {
            match logical_key {
                Key::Character(ch) => match ch.as_str() {
                    "n" | "N" => {
                        self.act(Action::NextChange);
                        *handled = true;
                        return;
                    }
                    "p" | "P" => {
                        self.act(Action::PrevChange);
                        *handled = true;
                        return;
                    }
                    "1" => {
                        self.act(Action::ModeDiff);
                        *handled = true;
                        return;
                    }
                    "2" => {
                        self.act(Action::ModeMerge);
                        *handled = true;
                        return;
                    }
                    _ => {}
                },
                Key::Named(NamedKey::F8) => {
                    self.act(Action::NextChange);
                    *handled = true;
                    return;
                }
                Key::Named(NamedKey::F7) => {
                    self.act(Action::PrevChange);
                    *handled = true;
                    return;
                }
                _ => {}
            }
        }

        // Toolbar clicks.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            let hit = self
                .tool_hits
                .iter()
                .find(|t| t.rect.contains(self.cursor))
                .map(|t| t.action);
            if let Some(action) = hit {
                self.act(action);
                *handled = true;
                return;
            }
        }

        // Route to the active view.
        match self.mode {
            Mode::Diff => {
                if let Some(n) = self.diff_node {
                    self.diff.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy);
                }
            }
            Mode::Merge => {
                if let Some(n) = self.merge_node {
                    self.merge.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy);
                }
                // A chip click on the merge panel queues its resolution; dispatch
                // it through the shared action path so the chip and Ctrl+K produce
                // the same replayable action under one undo boundary.
                if let Some(action) = self.merge.take_pending_action() {
                    self.perform_action(state, action);
                    *handled = true;
                }
            }
        }
    }

    fn name(&self) -> &str {
        "DiffMergeApp"
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
        self.action_commit_before.take()
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
        self.portal_rx.is_some()
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

fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

/// The action transport Diff installs for its own on-panel chips. Peers receive
/// the merge changes through ordinary CRDT sync (the executor pins each action's
/// actor so the change is content-addressed identical to a peer's replay and
/// dedupes on merge), so the local chip path has nothing extra to replicate —
/// this sink drops the envelope. The Ctrl+K palette dispatches through the
/// host's transport, which a collaborative session swaps for a peer-aware one.
struct DiscardTransport;
impl qos_actions::ActionTransport for DiscardTransport {
    fn replicate(&mut self, _env: &qos_actions::ActionEnvelope) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::{inject_for_test, PortalOutcome};

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
        let needs_right = app.consume_pick(&left);
        assert!(needs_right, "after the left file, the right one is still needed");
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

        assert_eq!(app.mode, Mode::Diff, "a completed pair switches to Diff mode");
        assert!(app.pending_left.is_none(), "the pending-left slot clears");
        assert!(app.portal_rx.is_none(), "the portal channel clears after delivering");
        let m = app.diff.model();
        assert_eq!(m.left_lines, 3, "the stashed left file is compared");
        assert_eq!(m.right_lines, 4, "the portal-chosen right file is compared");
        assert!(m.added + m.modified >= 1, "the differing files yield change blocks");
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
        assert!(app.pending_left.is_none(), "cancel abandons the half-finished pair");
        assert!(app.portal_rx.is_none(), "the portal channel clears after a cancel");
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
        app.perform_action(&mut state.doc, action);

        assert!(app.last_dispatched.is_some(), "the affordance dispatched an envelope");
        let after_merged = crate::doc::MergeDoc::merged_text(&state.doc);
        assert_ne!(before_merged, after_merged, "the action mutated the merged output");
        assert_eq!(
            crate::doc::MergeDoc::resolutions(&state.doc),
            vec![(hunk, "theirs".to_string())],
            "the take-a-side choice is recorded in the CRDT"
        );

        // Exactly one undo boundary, bracketed by the host at the pre-action heads.
        let stashed = Widget::take_committed_action(&mut app).expect("heads stashed for the host");
        assert_eq!(stashed, before_heads, "bracket starts at the pre-action heads");
        state.record_committed_action(stashed, None, None);
        assert!(state.peek_last_undo().is_some(), "one undo boundary recorded");

        // Ctrl+Z reverts exactly this action.
        state.undo(None, None).expect("an undo entry to pop");
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
            .find(|r| r.id.as_str() == format!("diffmerge.conflict:{hunk}"))
            .expect("the conflict resource");
        let spec = conflict.actions.iter().find(|a| a.id == "take-theirs").unwrap();
        let from_spec = match &spec.input {
            qos_actions::ActionInput::Immediate(e) => e.clone(),
            _ => panic!("take-theirs is immediate"),
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
}
