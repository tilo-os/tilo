//! Root widget — orchestrates the diff and merge modes, the top toolbar
//! (mode switch, change navigation, open), and the file picker overlay.
//!
//! Data flow:
//! * The app holds the raw input texts in memory and the two derived views
//!   ([`DiffView`], [`MergeView`]).
//! * The toolbar's buttons are drawn directly and hit-tested in `handle_event`.
//! * The file picker, when confirmed, reads file contents and re-seeds the
//!   active mode (two files → diff; the merge demo stays sample-driven since a
//!   three-way pick needs three roots).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::color_compat::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    FilePicker, FilePickerOutcome, Widget,
};

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
    picker: Option<FilePicker>,
    /// Files chosen but not yet paired (first of a two-file diff selection).
    pending_left: Option<(String, String)>,
    dirty: bool,
    /// One-shot flag: set when the active mode changed so the host rebuilds the
    /// taffy tree (the flex content child swaps between the two views).
    structure_dirty: bool,
}

impl DiffMergeApp {
    pub fn new() -> Self {
        let mut diff = DiffView::new();
        let (l, r) = samples::diff_pair();
        diff.set_inputs(&l.label, &l.text, &r.label, &r.text);

        let mut merge = MergeView::new();
        let (base, ours, theirs) = samples::merge_triple();
        merge.set_inputs(
            &base.label,
            &base.text,
            &ours.label,
            &ours.text,
            &theirs.label,
            &theirs.text,
        );

        // The app opens on the three-way merge so a fresh launch immediately
        // shows a conflict to resolve. `DIFFMERGE_MODE=diff` flips the initial
        // view to the side-by-side diff (handy for demos/screenshots).
        let mode = match std::env::var("DIFFMERGE_MODE").ok().as_deref() {
            Some("diff") => Mode::Diff,
            _ => Mode::Merge,
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
            picker: None,
            pending_left: None,
            dirty: true,
            structure_dirty: false,
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
                self.open_picker();
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

    fn open_picker(&mut self) {
        // Root the picker at the user's home (falling back to cwd) so two real
        // files can be diffed. Accessible roots gate navigation per the
        // sandbox model.
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
        let title = if self.pending_left.is_none() {
            "Open left file"
        } else {
            "Open right file"
        };
        let picker = FilePicker::new(title, &home, vec![home.clone()]);
        self.picker = Some(picker);
        self.dirty = true;
    }

    fn drain_picker(&mut self) {
        let outcome = match &mut self.picker {
            Some(picker) => picker.take_outcome(),
            None => return,
        };
        let Some(outcome) = outcome else {
            return;
        };
        match outcome {
            FilePickerOutcome::Cancelled => {
                self.picker = None;
                self.pending_left = None;
            }
            FilePickerOutcome::Selected(paths) => {
                self.picker = None;
                if let Some(path) = paths.first() {
                    let label = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    let content = fs::read_to_string(path).unwrap_or_else(|e| {
                        format!("<could not read {}: {e}>", path.display())
                    });
                    match self.pending_left.take() {
                        None => {
                            // First file picked — stash and ask for the second.
                            self.pending_left = Some((label, content));
                            self.open_picker();
                        }
                        Some((llabel, lcontent)) => {
                            self.diff.set_inputs(&llabel, &lcontent, &label, &content);
                            self.mode = Mode::Diff;
                        }
                    }
                }
                self.dirty = true;
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

        // Mode segmented control.
        let mut x = r.x0 + 70.0;
        let seg_y = r.y0 + 10.0;
        let seg_h = 28.0;
        for (label, action, selected) in [
            ("Diff", Action::ModeDiff, self.mode == Mode::Diff),
            ("3-way merge", Action::ModeMerge, self.mode == Mode::Merge),
        ] {
            let w = label.len() as f64 * 7.4 + 22.0;
            let seg = Rect::new(x, seg_y, x + w, seg_y + seg_h);
            let bg = if selected { c.accent[5] } else { with_alpha(c.neutral[3], 120) };
            let rr = RoundedRect::from_rect(seg, 6.0);
            scene.fill(Fill::NonZero, transform, bg, None, &rr);
            let tcol = if selected { Color::WHITE } else { c.text_dim };
            tr.draw_text(
                scene,
                label,
                13.0,
                tcol,
                transform * Affine::translate((x + 11.0, seg_y + 6.0)),
                None,
            );
            self.tool_hits.push(ToolHit { rect: seg, action });
            x += w + 6.0;
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
            let rr = RoundedRect::from_rect(btn, 6.0);
            scene.fill(Fill::NonZero, transform, with_alpha(c.neutral[3], 150), None, &rr);
            scene.stroke(&Stroke::new(1.0), transform, c.border_standard, None, &rr);
            tr.draw_text(
                scene,
                label,
                12.5,
                c.text_primary,
                transform * Affine::translate((rx - w + 10.0, seg_y + 6.0)),
                None,
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

        // Centre the picker if open.
        if let Some(picker) = &mut self.picker {
            let pw = 560.0_f64.min(self.rect.width() - 80.0);
            let ph = 460.0_f64.min(self.rect.height() - 80.0);
            let px = self.rect.x0 + (self.rect.width() - pw) / 2.0;
            let py = self.rect.y0 + (self.rect.height() - ph) / 2.0;
            picker.set_rect(Rect::new(px, py, px + pw, py + ph));
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

        // File picker overlay on top.
        if let Some(picker) = &mut self.picker {
            // Dim scrim.
            scene.fill(
                Fill::NonZero,
                transform,
                c.surfaces.overlay,
                None,
                &self.rect,
            );
            picker.draw(scene, transform, tr, theme, presence);
        }
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
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // Picker is modal when open.
        if let Some(picker) = &mut self.picker {
            picker.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
            self.drain_picker();
            *handled = true;
            self.dirty = true;
            return;
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
            }
        }
    }

    fn name(&self) -> &str {
        "DiffMergeApp"
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
        self.dirty
            || self.diff.needs_redraw()
            || self.merge.needs_redraw()
            || self.picker.as_ref().map(|p| p.needs_redraw()).unwrap_or(false)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.diff.mark_drawn();
        self.merge.mark_drawn();
        if let Some(p) = &mut self.picker {
            p.mark_drawn();
        }
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
