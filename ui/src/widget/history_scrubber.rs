//! `HistoryScrubber` — Phase 1 history-replay slider widget.
//!
//! Spec: `docs/superpowers/specs/2026-06-29-spaces-channels-and-cryptographic-roles.md`
//! Phase 1 ("Audit timeline + soft-delete + restore-from-history").
//!
//! Lets the user drag a handle through an artifact's
//! `Vec<HistoryEntry>` to scrub through historical states. Each tick
//! on the slider is one entry; the rightmost tick is always the
//! "current" (live) state. Two action buttons sit under the track:
//! `[Preview]` (fires `on_preview` with the tick's entry id, expecting
//! the host to mount a preview surface) and `[Restore to this version]`
//! (fires `on_restore`, expecting the host to emit a `Restore` op).
//!
//! The widget owns **scrubbing state only** — `current_tick: usize`.
//! It does not mutate the underlying CRDT; replaying is the host's
//! job. That keeps the widget portable across artifact kinds (a
//! kanban + a note share the same scrubber, but their restore wiring
//! differs).
//!
//! ## Geometry
//!
//! - Track runs across `[x0 + 16, x1 - 16]` to leave breathing room
//!   for the handle when it sits at the endpoints.
//! - Each tick is laid out at `x = track.x0 + (i / (n-1)) * track_w`.
//!   With `n == 1` (a single entry), the handle pins at the right
//!   edge.
//! - The popover above the handle shows the entry's summary +
//!   author + relative time. It sits just above the handle and
//!   clamps to stay inside `self.rect`.
//! - Action buttons sit under the track at fixed widths
//!   (`PREVIEW_W`, `RESTORE_W`).

use std::collections::HashMap;
use std::time::SystemTime;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::history::{HistoryEntry, HistoryEntryId, format_relative_time};

/// Track inset from each side of the rect. Has to be larger than the
/// handle radius so the handle visually clears the rect edge.
const TRACK_INSET: f64 = 16.0;
/// Vertical position of the track inside the rect, measured from
/// the top. Leaves room above for the popover and below for the
/// action buttons.
const TRACK_Y_FROM_TOP: f64 = 56.0;
/// Track thickness.
const TRACK_HEIGHT: f64 = 6.0;
/// Tick mark height (rendered as a 1px-wide notch).
const TICK_MARK_H: f64 = 12.0;
/// Handle radius. The slider widget uses 10 px; we match for visual
/// consistency.
const HANDLE_RADIUS: f64 = 9.0;
/// Hit-radius for the handle press — slightly larger than the visual
/// radius so a near-miss still latches the drag.
const HANDLE_HIT_RADIUS: f64 = 14.0;
const PREVIEW_W: f64 = 96.0;
const RESTORE_W: f64 = 160.0;
const BUTTON_H: f64 = 28.0;
/// Vertical offset of the action button row from the track.
const BUTTON_Y_OFFSET: f64 = 24.0;
/// Popover dimensions.
const POPOVER_H: f64 = 36.0;

/// Box-erased callback type. We use `FnMut` so the host can mutate
/// captured state (e.g. flip a preview-mounted flag) without
/// needing interior mutability.
pub type ScrubCallback = Box<dyn FnMut(&HistoryEntryId)>;

/// Horizontal-slider history scrubber.
pub struct HistoryScrubber {
    entries: Vec<HistoryEntry>,
    rect: Rect,
    /// 0-based index of the currently-selected tick. Always in
    /// `0..entries.len()` (or 0 when entries is empty).
    current_tick: usize,
    /// Whether the handle is currently being dragged. Drives the
    /// visual press state + lets the widget keep capturing pointer
    /// events when the cursor leaves `rect` mid-drag.
    is_dragging: bool,
    /// Hover state on the handle — drives the slight knob-radius
    /// scale on hover (mirrors `Slider`'s contract).
    is_hovered: bool,
    /// Hover state on the action buttons. Stored separately so we
    /// can paint the press affordance even if a future polish adds
    /// keyboard activation.
    hover_preview: bool,
    hover_restore: bool,
    last_cursor: Point,
    on_preview: Option<ScrubCallback>,
    on_restore: Option<ScrubCallback>,
    /// Pinned "now" for relative-time rendering in the popover. Tests
    /// pin this so the popover label stays stable.
    now_override: Option<SystemTime>,
    dirty: bool,
}

impl Default for HistoryScrubber {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl HistoryScrubber {
    /// Construct a scrubber over `entries`. The handle starts pinned
    /// at the rightmost tick — i.e. "current state" — so the user
    /// has to actively scrub left to view history.
    pub fn new(entries: Vec<HistoryEntry>) -> Self {
        let current = entries.len().saturating_sub(1);
        Self {
            entries,
            rect: Rect::ZERO,
            current_tick: current,
            is_dragging: false,
            is_hovered: false,
            hover_preview: false,
            hover_restore: false,
            last_cursor: Point::ZERO,
            on_preview: None,
            on_restore: None,
            now_override: None,
            dirty: true,
        }
    }

    /// Replace the entry list. Resets the handle to the rightmost
    /// tick (the new "current" state).
    pub fn set_entries(&mut self, entries: Vec<HistoryEntry>) {
        self.entries = entries;
        self.current_tick = self.entries.len().saturating_sub(1);
        self.dirty = true;
    }

    /// Mutable handle to the entries list for streaming producers.
    pub fn entries_mut(&mut self) -> &mut Vec<HistoryEntry> {
        &mut self.entries
    }

    /// Builder: install a preview callback.
    pub fn on_preview<F: FnMut(&HistoryEntryId) + 'static>(mut self, f: F) -> Self {
        self.on_preview = Some(Box::new(f));
        self
    }

    /// Builder: install a restore callback.
    pub fn on_restore<F: FnMut(&HistoryEntryId) + 'static>(mut self, f: F) -> Self {
        self.on_restore = Some(Box::new(f));
        self
    }

    /// Pin "now" for the popover's relative-time label. Tests use
    /// this so the label stays stable across runs.
    pub fn set_now_for_test(&mut self, now: SystemTime) {
        self.now_override = Some(now);
    }

    /// Current tick index (0-based, into `entries`).
    pub fn current_tick(&self) -> usize {
        self.current_tick
    }

    /// Borrow the entry at the current tick.
    pub fn current_entry(&self) -> Option<&HistoryEntry> {
        self.entries.get(self.current_tick)
    }

    /// Move the handle to `tick`, clamped to `[0, entries.len()-1]`.
    pub fn set_current_tick(&mut self, tick: usize) {
        let new_tick = if self.entries.is_empty() {
            0
        } else {
            tick.min(self.entries.len() - 1)
        };
        if new_tick != self.current_tick {
            self.current_tick = new_tick;
            self.dirty = true;
        }
    }

    /// Number of ticks (= number of entries).
    pub fn tick_count(&self) -> usize {
        self.entries.len()
    }

    /// Rect of the horizontal track.
    pub fn track_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + TRACK_INSET,
            self.rect.y0 + TRACK_Y_FROM_TOP - TRACK_HEIGHT / 2.0,
            self.rect.x1 - TRACK_INSET,
            self.rect.y0 + TRACK_Y_FROM_TOP + TRACK_HEIGHT / 2.0,
        )
    }

    /// X coordinate of tick `i`. With a single entry, pins to the
    /// rightmost track position.
    pub fn tick_x(&self, i: usize) -> f64 {
        let track = self.track_rect();
        let n = self.entries.len();
        if n <= 1 {
            return track.x1;
        }
        let t = (i as f64) / ((n - 1) as f64);
        track.x0 + t * track.width()
    }

    /// Nearest tick index to a given x coordinate.
    pub fn tick_at_x(&self, x: f64) -> usize {
        let track = self.track_rect();
        let n = self.entries.len();
        if n <= 1 {
            return 0;
        }
        let t = ((x - track.x0) / track.width()).clamp(0.0, 1.0);
        (t * ((n - 1) as f64)).round() as usize
    }

    /// Centre point of the handle at the current tick.
    pub fn handle_center(&self) -> Point {
        Point::new(self.tick_x(self.current_tick), self.track_rect().center().y)
    }

    /// Rect of the popover above the handle.
    pub fn popover_rect(&self) -> Rect {
        let centre = self.handle_center();
        // Width is generous enough for "summary by Author · 3 min ago"
        // without a runtime measurement; the renderer truncates
        // if it overflows.
        let w = 220.0_f64.min(self.rect.width() - 16.0).max(80.0);
        let x0 = (centre.x - w / 2.0)
            .clamp(self.rect.x0 + 8.0, self.rect.x1 - 8.0 - w);
        let y0 = self.rect.y0 + 8.0;
        Rect::new(x0, y0, x0 + w, y0 + POPOVER_H)
    }

    /// Rect of the `[Preview]` button under the track.
    pub fn preview_button_rect(&self) -> Rect {
        let track = self.track_rect();
        let y0 = track.y1 + BUTTON_Y_OFFSET;
        Rect::new(
            self.rect.x0 + TRACK_INSET,
            y0,
            self.rect.x0 + TRACK_INSET + PREVIEW_W,
            y0 + BUTTON_H,
        )
    }

    /// Rect of the `[Restore to this version]` button under the
    /// track.
    pub fn restore_button_rect(&self) -> Rect {
        let track = self.track_rect();
        let y0 = track.y1 + BUTTON_Y_OFFSET;
        Rect::new(
            self.rect.x1 - TRACK_INSET - RESTORE_W,
            y0,
            self.rect.x1 - TRACK_INSET,
            y0 + BUTTON_H,
        )
    }

    /// Current "now" for relative-time rendering.
    fn now(&self) -> SystemTime {
        self.now_override.unwrap_or_else(SystemTime::now)
    }

    /// Fire `on_preview` with the current tick's entry id.
    fn invoke_preview(&mut self) {
        let Some(entry) = self.entries.get(self.current_tick) else {
            return;
        };
        let id = entry.id;
        if let Some(cb) = self.on_preview.as_mut() {
            cb(&id);
        }
    }

    /// Fire `on_restore` with the current tick's entry id.
    fn invoke_restore(&mut self) {
        let Some(entry) = self.entries.get(self.current_tick) else {
            return;
        };
        let id = entry.id;
        if let Some(cb) = self.on_restore.as_mut() {
            cb(&id);
        }
    }

    /// Draw the per-tick notches above the track.
    fn draw_ticks(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let track = self.track_rect();
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let tick_color = theme.tokens.colors.text_dim;
        for i in 0..n {
            let x = self.tick_x(i);
            let mark = Rect::new(
                x - 0.5,
                track.center().y - TICK_MARK_H / 2.0,
                x + 0.5,
                track.center().y + TICK_MARK_H / 2.0,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tick_color,
                None,
                &mark,
            );
        }
    }

    /// Paint a single button body + label. Used for both Preview
    /// and Restore so styling stays uniform.
    #[allow(clippy::too_many_arguments)]
    fn draw_button(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        hovered: bool,
        primary: bool,
    ) {
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.sm as f64);
        let (fill, border, text_color) = if primary {
            // Restore is the primary action — accent fill.
            let fill = if hovered {
                theme.tokens.colors.accent[4]
            } else {
                theme.tokens.colors.accent[5]
            };
            (fill, theme.tokens.colors.border_focus, theme.tokens.colors.text_primary)
        } else if hovered {
            (
                theme.tokens.colors.surface_white_20,
                theme.tokens.colors.border_white_20,
                theme.tokens.colors.text_primary,
            )
        } else {
            (
                theme.tokens.colors.surface_white_10,
                theme.tokens.colors.border_white_20,
                theme.tokens.colors.text_primary,
            )
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            fill,
            None,
            &shape,
        );
        scene.stroke(&Stroke::new(1.0), transform, border, None, &shape);
        let size = theme.tokens.typography.scale.sm;
        let (w, h) = text_renderer.measure(label, size, None);
        text_renderer.draw_text(
            scene,
            label,
            size,
            text_color,
            transform
                * Affine::translate((
                    rect.x0 + (rect.width() - w as f64) / 2.0,
                    rect.y0 + (rect.height() - h as f64) / 2.0,
                )),
            None,
        );
    }
}

impl Widget for HistoryScrubber {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

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
                    height: Dimension::length(140.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let prev = self.rect;
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        if prev != self.rect {
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Surface card.
        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.md as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &shape,
        );

        if self.entries.is_empty() {
            // Empty state — centred dim label.
            let label = "No history";
            let size = theme.tokens.typography.scale.sm;
            let (w, h) = text_renderer.measure(label, size, None);
            text_renderer.draw_text(
                scene,
                label,
                size,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        (self.rect.x0 + self.rect.x1) / 2.0 - w as f64 / 2.0,
                        (self.rect.y0 + self.rect.y1) / 2.0 - h as f64 / 2.0,
                    )),
                None,
            );
            self.dirty = false;
            return;
        }

        // Track.
        let track = self.track_rect();
        let track_shape = RoundedRect::from_rect(track, 3.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.neutral[3],
            None,
            &track_shape,
        );

        // Active band — from leftmost tick up to the current handle.
        let active = Rect::new(
            track.x0,
            track.y0,
            self.handle_center().x,
            track.y1,
        );
        if active.width() > 0.0 {
            let active_shape = RoundedRect::from_rect(active, 3.0);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.accent[5],
                None,
                &active_shape,
            );
        }

        // Per-tick notches.
        self.draw_ticks(scene, transform, theme);

        // "Current" indicator — a small caret/diamond at the rightmost
        // tick so users can spot where the live state lives even when
        // the handle has scrubbed back.
        let current_x = self.tick_x(self.entries.len() - 1);
        let live_dot = Circle::new(
            Point::new(current_x, track.center().y - 18.0),
            3.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.accent[5],
            None,
            &live_dot,
        );

        // Handle.
        let handle = self.handle_center();
        let radius = if self.is_dragging {
            HANDLE_RADIUS * 1.15
        } else if self.is_hovered {
            HANDLE_RADIUS * 1.08
        } else {
            HANDLE_RADIUS
        };
        let handle_shape = Circle::new(handle, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.text_primary,
            None,
            &handle_shape,
        );
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            theme.tokens.colors.border_white_20,
            None,
            &handle_shape,
        );

        // Popover above the handle.
        let pop = self.popover_rect();
        let pop_shape = RoundedRect::from_rect(pop, theme.tokens.radii.sm as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &pop_shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_white_20,
            None,
            &pop_shape,
        );
        if let Some(entry) = self.current_entry() {
            let now = self.now();
            let line = format!(
                "{} by {} \u{00b7} {}",
                entry.summary,
                entry.author.name,
                format_relative_time(entry.timestamp, now)
            );
            let size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);
            // Truncate to fit. Cheap binary-chop over char count.
            let budget = pop.width() - 12.0;
            let (full_w, _) = text_renderer.measure(&line, size, None);
            let drawn = if full_w as f64 > budget {
                let mut lo = 0;
                let mut hi = line.chars().count();
                while lo < hi {
                    let mid = (lo + hi).div_ceil(2);
                    let s: String = line.chars().take(mid).collect();
                    let (w, _) = text_renderer.measure(&s, size, None);
                    if w as f64 <= budget {
                        lo = mid;
                    } else {
                        hi = mid - 1;
                    }
                }
                line.chars().take(lo).collect::<String>()
            } else {
                line
            };
            let (lw, lh) = text_renderer.measure(&drawn, size, None);
            text_renderer.draw_text(
                scene,
                &drawn,
                size,
                theme.tokens.colors.text_primary,
                transform
                    * Affine::translate((
                        pop.x0 + (pop.width() - lw as f64) / 2.0,
                        pop.y0 + (pop.height() - lh as f64) / 2.0,
                    )),
                None,
            );
        }

        // Action buttons.
        self.draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            self.preview_button_rect(),
            "Preview",
            self.hover_preview,
            false,
        );
        self.draw_button(
            scene,
            transform,
            text_renderer,
            theme,
            self.restore_button_rect(),
            "Restore to this version",
            self.hover_restore,
            true,
        );

        self.dirty = false;
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Empty scrubbers are inert.
        if self.entries.is_empty() {
            return;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;

                // Handle hover (visual).
                let handle = self.handle_center();
                let dx = p.x - handle.x;
                let dy = p.y - handle.y;
                let hovered = (dx * dx + dy * dy).sqrt() <= HANDLE_HIT_RADIUS;
                if hovered != self.is_hovered {
                    self.is_hovered = hovered;
                    self.dirty = true;
                    *handled = true;
                }

                // Button hover.
                let pv = self.preview_button_rect().contains(p);
                let rs = self.restore_button_rect().contains(p);
                if pv != self.hover_preview {
                    self.hover_preview = pv;
                    self.dirty = true;
                    *handled = true;
                }
                if rs != self.hover_restore {
                    self.hover_restore = rs;
                    self.dirty = true;
                    *handled = true;
                }

                // Drag updates the current tick.
                if self.is_dragging {
                    let tick = self.tick_at_x(p.x);
                    if tick != self.current_tick {
                        self.current_tick = tick;
                        self.dirty = true;
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if *element_state == ElementState::Pressed {
                    // Press on a button → fire its callback.
                    if self.preview_button_rect().contains(self.last_cursor) {
                        self.invoke_preview();
                        *handled = true;
                        return;
                    }
                    if self.restore_button_rect().contains(self.last_cursor) {
                        self.invoke_restore();
                        *handled = true;
                        return;
                    }

                    // Press on the handle → start dragging.
                    let handle = self.handle_center();
                    let dx = self.last_cursor.x - handle.x;
                    let dy = self.last_cursor.y - handle.y;
                    if (dx * dx + dy * dy).sqrt() <= HANDLE_HIT_RADIUS {
                        self.is_dragging = true;
                        *focus_target = Some(node_id);
                        *handled = true;
                        return;
                    }

                    // Press on the track → jump the handle to the
                    // nearest tick and start dragging from there.
                    let track = self.track_rect();
                    let on_track_y =
                        (self.last_cursor.y - track.center().y).abs() <= TICK_MARK_H;
                    let on_track_x =
                        self.last_cursor.x >= track.x0 && self.last_cursor.x <= track.x1;
                    if on_track_x && on_track_y {
                        let tick = self.tick_at_x(self.last_cursor.x);
                        self.current_tick = tick;
                        self.is_dragging = true;
                        *focus_target = Some(node_id);
                        self.dirty = true;
                        *handled = true;
                    }
                } else if *element_state == ElementState::Released && self.is_dragging {
                    self.is_dragging = false;
                    self.dirty = true;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    /// Drag must continue tracking the cursor even when it leaves
    /// `rect` — mirrors the slider's contract.
    fn wants_pointer_capture(&self) -> bool {
        self.is_dragging
    }

    fn name(&self) -> &str {
        "HistoryScrubber"
    }

    fn test_id(&self) -> Option<&str> {
        Some("history_scrubber")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;
    use qos_collab_types::{ArtifactKind, OpClass};
    use winit::event::Modifiers;
    use crate::widget::history::HistoryAuthor;

    fn entry(id: u8) -> HistoryEntry {
        let mut bytes = [0u8; 16];
        bytes[0] = id;
        HistoryEntry {
            id: HistoryEntryId(bytes),
            author: HistoryAuthor::new("Alice", 120.0, "deadbeef"),
            op_class: OpClass::Splice,
            artifact_kind: ArtifactKind::Note,
            timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(id as u64 * 60),
            summary: format!("op {}", id),
            diff_payload: None,
        }
    }

    fn scrubber_at(rect: Rect, n: usize) -> HistoryScrubber {
        let entries: Vec<HistoryEntry> = (0..n as u8).map(entry).collect();
        let mut s = HistoryScrubber::new(entries);
        s.rect = rect;
        s.set_now_for_test(SystemTime::UNIX_EPOCH + Duration::from_secs(3600));
        s
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn mouse_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    fn dispatch(s: &mut HistoryScrubber, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(
            event,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &tree,
        );
    }

    #[test]
    fn handle_drag_advances_tick() {
        // Seed an 11-tick scrubber, press at tick 0, drag right to
        // (about) tick 5, release; current_tick must be 5.
        let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 11);
        s.set_current_tick(0);
        let track = s.track_rect();
        let start_x = s.tick_x(0);

        // CursorMoved to the handle, press, then drag to tick 5's x.
        let handle_y = track.center().y;
        dispatch(&mut s, &cursor_moved(start_x, handle_y));
        dispatch(&mut s, &mouse_press());
        assert!(s.is_dragging, "press on handle must start drag");

        let target_x = s.tick_x(5);
        dispatch(&mut s, &cursor_moved(target_x, handle_y));
        assert_eq!(s.current_tick(), 5, "drag must advance tick (got {})", s.current_tick());

        dispatch(&mut s, &mouse_release());
        assert!(!s.is_dragging, "release must end drag");
    }

    #[test]
    fn click_on_track_jumps_to_tick() {
        // Press anywhere on the track (away from the handle) should
        // jump the handle to the nearest tick.
        let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 11);
        s.set_current_tick(0);
        let track = s.track_rect();
        let target = s.tick_x(7);

        dispatch(&mut s, &cursor_moved(target, track.center().y));
        dispatch(&mut s, &mouse_press());
        assert_eq!(s.current_tick(), 7);
        dispatch(&mut s, &mouse_release());
    }

    #[test]
    fn restore_callback_fires_on_button_click() {
        // End-to-end: hover the Restore button, press, the callback
        // fires with the current tick's entry id.
        let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 5);
        s.set_current_tick(2);
        let captured: Rc<RefCell<Option<HistoryEntryId>>> = Rc::new(RefCell::new(None));
        let captured_clone = captured.clone();
        s.on_restore = Some(Box::new(move |id| {
            *captured_clone.borrow_mut() = Some(*id);
        }));

        let btn = s.restore_button_rect();
        dispatch(&mut s, &cursor_moved(btn.center().x, btn.center().y));
        dispatch(&mut s, &mouse_press());

        let mut id_bytes = [0u8; 16];
        id_bytes[0] = 2;
        assert_eq!(*captured.borrow(), Some(HistoryEntryId(id_bytes)));
    }

    #[test]
    fn preview_callback_fires_on_button_click() {
        // Mirror of the restore test for the Preview button.
        let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 5);
        s.set_current_tick(3);
        let captured: Rc<RefCell<Option<HistoryEntryId>>> = Rc::new(RefCell::new(None));
        let captured_clone = captured.clone();
        s.on_preview = Some(Box::new(move |id| {
            *captured_clone.borrow_mut() = Some(*id);
        }));

        let btn = s.preview_button_rect();
        dispatch(&mut s, &cursor_moved(btn.center().x, btn.center().y));
        dispatch(&mut s, &mouse_press());

        let mut id_bytes = [0u8; 16];
        id_bytes[0] = 3;
        assert_eq!(*captured.borrow(), Some(HistoryEntryId(id_bytes)));
    }

    #[test]
    fn scrubber_with_no_entries_is_a_no_op() {
        // Empty scrubber must not panic on events + must keep
        // current_tick at 0. The render path takes the empty-state
        // branch.
        let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 0);
        assert_eq!(s.tick_count(), 0);
        assert_eq!(s.current_tick(), 0);
        dispatch(&mut s, &cursor_moved(100.0, 60.0));
        dispatch(&mut s, &mouse_press());
        dispatch(&mut s, &mouse_release());
        assert_eq!(s.current_tick(), 0);
        // Set/get round-trips even with empty entries.
        s.set_current_tick(999);
        assert_eq!(s.current_tick(), 0);
    }

    #[test]
    fn drag_saturates_at_endpoints() {
        // Dragging far past the right edge of the track must
        // saturate at the last tick — important so a fast cursor
        // swipe doesn't bypass the rightmost entry.
        let mut s = scrubber_at(Rect::new(0.0, 0.0, 400.0, 140.0), 11);
        s.set_current_tick(5);
        let track = s.track_rect();
        let start_x = s.tick_x(5);
        dispatch(&mut s, &cursor_moved(start_x, track.center().y));
        dispatch(&mut s, &mouse_press());
        // Move 10000 px past the rect's right edge.
        dispatch(&mut s, &cursor_moved(10_000.0, track.center().y));
        assert_eq!(s.current_tick(), 10, "drag past right must saturate at last tick");
        // And past the left.
        dispatch(&mut s, &cursor_moved(-10_000.0, track.center().y));
        assert_eq!(s.current_tick(), 0, "drag past left must saturate at tick 0");
    }
}
