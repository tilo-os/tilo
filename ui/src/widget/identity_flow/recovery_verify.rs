//! Screen 2b — recovery-share verification.
//!
//! After screen 2 shows the 24 words, screen 2b challenges the user
//! to type three of them back. Three positions are picked at random
//! when the screen is shown for the first time; the user types each
//! word into the matching slot; the "Finish setup" button activates
//! when all three slots match (case-insensitive, whitespace
//! trimmed).
//!
//! The challenge re-seeds when the user clicks "Pick different blanks" —
//! lets them retry without losing earlier progress.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::sheet::{
    Align, BodyScroll, Field, GRID_SHEET_WIDTH, Resolved, Role, Rows, SheetSpec, WordGrid,
    paint_field, paint_text, paragraph_height, resolve, with_layout_renderer,
};
use crate::widget::identity_flow::{LineEditState, RecoveryChallenge, ScreenOutcome};
use crate::widget::modal_frame::ModalAction;

const TITLE: &str = "Confirm your recovery words";
const EXPLANATION: &str =
    "Compare with your written list. Fill in the highlighted blanks to confirm.";
const MISMATCH: &str = "One or more words didn't match. Try again or pick new words.";
const RESEED_LABEL: &str = "Pick different blanks";
const FINISH_LABEL: &str = "Finish setup";

/// Action-row indices, in authoring order.
const ACTION_RESEED: usize = 0;
const ACTION_FINISH: usize = 1;

/// The mnemonic is 24 words; the grid never shows more.
const WORD_COUNT: usize = 24;

/// Every rect the screen paints and hit-tests, from one pass over the rows.
#[derive(Clone, Debug, Default)]
struct Plan {
    explanation: Rect,
    grid: Rect,
    /// One editable cell per challenge slot, in storage order.
    slots: [Rect; 3],
    error: Option<Rect>,
    bottom: f64,
}

/// Screen 2b: type-3-words verification.
pub struct RecoveryVerifyScreen {
    rect: Rect,
    mnemonic: String,
    challenge: Option<RecoveryChallenge>,
    /// One `LineEditState` per slot so the user gets the full
    /// keyboard-shortcut kit (Ctrl+arrows, Shift selection, Ctrl+A/C/X/V).
    editors: [LineEditState; 3],
    focused_slot: Option<usize>,
    /// Whether the last submit produced a mismatch — drives the inline
    /// error message.
    last_attempt_failed: bool,
    slot_rects: [Rect; 3],
    continue_rect: Rect,
    reseed_rect: Rect,
    last_cursor: Point,
    /// The theme `build_layout` saw, so `apply_layout` can resolve the rows
    /// without waiting for a paint.
    layout_theme: Option<Theme>,
    scroll: BodyScroll,
    body_height: f64,
}

impl Default for RecoveryVerifyScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl RecoveryVerifyScreen {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            mnemonic: String::new(),
            challenge: None,
            editors: [
                LineEditState::new(),
                LineEditState::new(),
                LineEditState::new(),
            ],
            focused_slot: None,
            last_attempt_failed: false,
            slot_rects: [Rect::ZERO; 3],
            continue_rect: Rect::ZERO,
            reseed_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            layout_theme: None,
            scroll: BodyScroll::default(),
            body_height: 0.0,
        }
    }

    /// Inject the mnemonic the verify step quizzes against; resets the challenge.
    pub fn seed_mnemonic(&mut self, mnemonic: String) {
        self.mnemonic = mnemonic;
        self.challenge = RecoveryChallenge::from_mnemonic(&self.mnemonic).ok();
        self.editors = [
            LineEditState::new(),
            LineEditState::new(),
            LineEditState::new(),
        ];
        self.last_attempt_failed = false;
    }

    pub fn challenge(&self) -> Option<&RecoveryChallenge> {
        self.challenge.as_ref()
    }

    /// Pick a fresh 3-word challenge.
    pub fn reseed(&mut self) {
        self.challenge = RecoveryChallenge::from_mnemonic(&self.mnemonic).ok();
        self.editors = [
            LineEditState::new(),
            LineEditState::new(),
            LineEditState::new(),
        ];
        self.last_attempt_failed = false;
    }

    /// Inject a deterministic challenge, for reproducible test ordering.
    pub fn seed_challenge(&mut self, positions: [usize; 3]) {
        self.challenge = RecoveryChallenge::at_positions(&self.mnemonic, positions).ok();
        self.editors = [
            LineEditState::new(),
            LineEditState::new(),
            LineEditState::new(),
        ];
        self.last_attempt_failed = false;
    }

    pub fn set_answer(&mut self, slot: usize, value: impl Into<String>) {
        if let Some(editor) = self.editors.get_mut(slot) {
            *editor = LineEditState::with_text(value);
            self.last_attempt_failed = false;
        }
    }

    pub fn answer(&self, slot: usize) -> Option<&str> {
        self.editors.get(slot).map(|e| e.text.as_str())
    }

    pub fn last_attempt_failed(&self) -> bool {
        self.last_attempt_failed
    }

    /// True when all three slots match the challenge.
    pub fn is_correct(&self) -> bool {
        match &self.challenge {
            None => false,
            Some(c) => {
                let r0 = self.editors[0].text.as_str();
                let r1 = self.editors[1].text.as_str();
                let r2 = self.editors[2].text.as_str();
                c.check(&[r0, r1, r2])
            }
        }
    }

    // Mirrors `Widget::handle_event`'s parameter list on purpose --
    // this is invoked from the trait impl, forwarding the same event,
    // scale, modifiers, CRDT handle, and out-params the screen host
    // received; bundling it would mean bundling the trait method too.
    #[allow(clippy::too_many_arguments)]
    pub fn handle_event_screen(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) -> ScreenOutcome {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                for (i, r) in self.slot_rects.iter().enumerate() {
                    if r.contains(self.last_cursor) {
                        self.focused_slot = Some(i);
                        *handled = true;
                        return ScreenOutcome::Stay;
                    }
                }
                if self.reseed_rect.contains(self.last_cursor) {
                    self.reseed();
                    *handled = true;
                    return ScreenOutcome::Stay;
                }
                if self.continue_rect.contains(self.last_cursor) {
                    *handled = true;
                    if self.is_correct() {
                        return ScreenOutcome::Advance;
                    }
                    self.last_attempt_failed = true;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                if self.scroll.key(logical_key, self.body_height) {
                    *handled = true;
                    return ScreenOutcome::Stay;
                }
                let Some(slot) = self.focused_slot else {
                    return ScreenOutcome::Stay;
                };
                // Tab + Enter are screen-level shortcuts; everything else
                // delegates to the slot's LineEditState for Ctrl+arrows,
                // Shift selection, Ctrl+A/C/X/V, Home/End.
                match logical_key {
                    Key::Named(NamedKey::Tab) => {
                        // Visual (ascending position), not storage, order.
                        if let Some(c) = &self.challenge {
                            self.focused_slot = Some(next_slot_in_visual_order(
                                slot,
                                &c.positions,
                                modifiers.state().shift_key(),
                            ));
                        }
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        *handled = true;
                        if self.is_correct() {
                            return ScreenOutcome::Advance;
                        }
                        self.last_attempt_failed = true;
                    }
                    _ => {
                        if let Some(editor) = self.editors.get_mut(slot)
                            && editor.apply_key(logical_key, modifiers, text.as_deref())
                        {
                            self.last_attempt_failed = false;
                            *handled = true;
                        }
                    }
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.scroll.wheel(delta) {
                    *handled = true;
                }
                ScreenOutcome::Stay
            }
            _ => ScreenOutcome::Stay,
        }
    }

    /// One geometry pass for both `apply_layout` and `draw`; the blanks'
    /// targets are the parts of them inside the visible body.
    fn resolve(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) -> Resolved<Plan> {
        let actions = self.actions();
        let spec = SheetSpec {
            title: TITLE,
            max_width: GRID_SHEET_WIDTH,
            actions: &actions,
        };
        let mut scroll = self.scroll;
        let resolved = resolve(
            self.rect,
            &spec,
            &mut scroll,
            text_renderer,
            theme,
            |rows, control_h, tr, theme| {
                let plan = self.plan(rows, control_h, tr, theme);
                let bottom = plan.bottom;
                (plan, bottom)
            },
        );
        self.scroll = scroll;
        let body = resolved.sheet.body();
        self.body_height = body.height();
        for (slot, rect) in self.slot_rects.iter_mut().zip(resolved.plan.slots.iter()) {
            *slot = BodyScroll::visible(body, *rect);
        }
        self.reseed_rect = resolved.sheet.action(ACTION_RESEED);
        self.continue_rect = resolved.sheet.action(ACTION_FINISH);
        resolved
    }

    fn actions(&self) -> [ModalAction<'static>; 2] {
        [
            ModalAction::secondary(RESEED_LABEL)
                .hovered(self.reseed_rect.contains(self.last_cursor)),
            ModalAction::primary(FINISH_LABEL)
                .enabled(self.is_correct())
                .hovered(self.continue_rect.contains(self.last_cursor)),
        ]
    }

    /// Walk the body rows once: explanation, the same six control-tall
    /// grid rows the share screen lays out (so the two read as one surface
    /// with three blanks punched in), then the mismatch line if any.
    fn plan(
        &self,
        mut rows: Rows,
        control_h: f64,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Plan {
        let sp = &theme.tokens.spacing;
        let width = rows.width();
        let explanation = rows.row(paragraph_height(
            text_renderer,
            theme,
            Role::Body,
            EXPLANATION,
            width,
        ));
        rows.gap(sp.s6);
        let grid = rows.row(WordGrid::height(WORD_COUNT, control_h));
        let cells = WordGrid::new(grid, control_h, text_renderer, theme);
        let mut slots = [Rect::ZERO; 3];
        if let Some(c) = &self.challenge {
            for (slot, pos1) in slots.iter_mut().zip(c.positions.iter()) {
                // Positions are one-based; the editable blank is the word
                // column of its cell.
                let (_, word) = cells.cell(pos1.saturating_sub(1), theme);
                *slot = word;
            }
        }
        let error = self.last_attempt_failed.then(|| {
            rows.gap(sp.s4);
            rows.row(paragraph_height(
                text_renderer,
                theme,
                Role::Error,
                MISMATCH,
                width,
            ))
        });
        Plan {
            explanation,
            grid,
            slots,
            error,
            bottom: rows.bottom(),
        }
    }

    /// The grid: every cell numbered, twenty-one words read-only and the
    /// three challenged ones as editable fields in the same cells.
    fn draw_word_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        plan: &Plan,
        control_h: f64,
    ) {
        let grid = WordGrid::new(plan.grid, control_h, text_renderer, theme);
        let positions = self.challenge.as_ref().map_or([0, 0, 0], |c| c.positions);
        let words: Vec<&str> = self.mnemonic.split_whitespace().collect();
        for i in 0..WORD_COUNT {
            let (ordinal, cell) = grid.cell(i, theme);
            let pos1 = i.saturating_add(1);
            paint_text(
                scene,
                transform,
                text_renderer,
                theme,
                ordinal,
                &format!("{pos1}."),
                Role::Label,
                Align::Start,
                false,
            );
            match positions.iter().position(|&p| p == pos1) {
                Some(slot) => {
                    let rect = plan.slots.get(slot).copied().unwrap_or(cell);
                    self.draw_blank_slot(scene, transform, text_renderer, theme, slot, rect);
                }
                None => paint_text(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    cell,
                    words.get(i).copied().unwrap_or(""),
                    Role::Value,
                    Align::Start,
                    false,
                ),
            }
        }
    }

    /// The editable blank at `slot`, painted into its full grid cell `rect`
    /// (the hit target is the visible part of it): a field with the shared
    /// look, plus a verdict edge — accent once it matches, danger after a
    /// failed attempt while it still holds a wrong word.
    fn draw_blank_slot(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        slot: usize,
        rect: Rect,
    ) {
        let Some(editor) = self.editors.get(slot) else {
            return;
        };
        let focused = self.focused_slot == Some(slot);
        paint_field(
            scene,
            transform,
            text_renderer,
            theme,
            &Field {
                rect,
                text: &editor.text,
                placeholder: "",
                focused,
                editor: Some(editor),
                multiline: false,
            },
        );
        let answer = editor.text.as_str();
        let matches = self
            .challenge
            .as_ref()
            .is_some_and(|c| answer.trim().eq_ignore_ascii_case(&c.expected_word(slot)));
        let verdict = if focused {
            None
        } else if matches {
            Some(theme.accent_primary)
        } else if self.last_attempt_failed && !answer.is_empty() {
            Some(theme.tokens.colors.semantic.danger)
        } else {
            None
        };
        if let Some(edge) = verdict {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                edge,
                None,
                &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.md)),
            );
        }
    }
}

/// Given `slot` and the challenge's `positions` array (storage order, NOT
/// visual order), return the slot index Tab should land on — the slot
/// whose position number is next-larger (Tab) or next-smaller
/// (Shift+Tab), wrapping at the ends.
///
/// Example: positions = [13, 12, 23], slot = 0 (position 13). Visual
/// order is [12, 13, 23] → Tab goes to 23 (slot 2), Shift+Tab to 12 (slot 1).
pub fn next_slot_in_visual_order(slot: usize, positions: &[usize; 3], backward: bool) -> usize {
    let Some(&current_pos) = positions.get(slot) else {
        return slot;
    };
    if backward {
        // Largest position smaller than current; wrap to largest.
        positions
            .iter()
            .enumerate()
            .filter(|(_, p)| **p < current_pos)
            .max_by_key(|(_, p)| **p)
            .map(|(i, _)| i)
            .or_else(|| {
                positions
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, p)| **p)
                    .map(|(i, _)| i)
            })
            .unwrap_or(slot)
    } else {
        // Smallest position larger than current; wrap to smallest.
        positions
            .iter()
            .enumerate()
            .filter(|(_, p)| **p > current_pos)
            .min_by_key(|(_, p)| **p)
            .map(|(i, _)| i)
            .or_else(|| {
                positions
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, p)| **p)
                    .map(|(i, _)| i)
            })
            .unwrap_or(slot)
    }
}

impl Widget for RecoveryVerifyScreen {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = Some(theme.clone());
        // The screen owns the viewport, so the sheet centres on the window
        // rather than inside a fixed box at the origin.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if let Some(theme) = self.layout_theme.clone() {
            with_layout_renderer(&theme, |tr| {
                self.resolve(&theme, tr);
            });
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
        let resolved = self.resolve(theme, text_renderer);
        let actions = self.actions();
        let spec = SheetSpec {
            title: TITLE,
            max_width: GRID_SHEET_WIDTH,
            actions: &actions,
        };
        let sheet = &resolved.sheet;
        let plan = &resolved.plan;
        let control_h = sheet.control_height(theme);

        sheet.paint_frame(scene, transform, text_renderer, theme, &spec);
        sheet.clip_body(scene, transform);
        paint_text(
            scene,
            transform,
            text_renderer,
            theme,
            plan.explanation,
            EXPLANATION,
            Role::Body,
            Align::Start,
            true,
        );
        self.draw_word_grid(scene, transform, text_renderer, theme, plan, control_h);
        if let Some(rect) = plan.error {
            paint_text(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                MISMATCH,
                Role::Error,
                Align::Start,
                true,
            );
        }
        scene.pop_layer();
        sheet.paint_actions(scene, transform, text_renderer, theme, &spec);
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &'static str {
        "RecoveryVerifyScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_recovery_verify")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_mnemonic() -> String {
        "ability abandon able about above absent absorb abstract absurd \
         abuse access accident account accuse achieve acid acoustic acquire \
         across act action actor actress actual"
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn tab_walks_slots_in_ascending_position_order_not_storage_order() {
        // Positions stored in whatever order Fisher-Yates picked
        // ([13, 12, 23], not ascending) — Tab must still walk visually.
        let positions = [13usize, 12, 23];

        assert_eq!(
            next_slot_in_visual_order(0, &positions, false),
            2,
            "Tab from pos 13 should land on pos 23",
        );
        assert_eq!(
            next_slot_in_visual_order(0, &positions, true),
            1,
            "Shift+Tab from pos 13 should land on pos 12",
        );
        assert_eq!(
            next_slot_in_visual_order(2, &positions, false),
            1,
            "Tab from largest pos should wrap to smallest",
        );
        assert_eq!(
            next_slot_in_visual_order(1, &positions, true),
            2,
            "Shift+Tab from smallest pos should wrap to largest",
        );
    }

    #[test]
    fn challenge_is_some_after_seed() {
        let mut s = RecoveryVerifyScreen::new();
        s.seed_mnemonic(fixture_mnemonic());
        assert!(s.challenge().is_some());
    }

    #[test]
    fn is_correct_true_for_matching_answers() {
        let mut s = RecoveryVerifyScreen::new();
        s.seed_mnemonic(fixture_mnemonic());
        s.seed_challenge([1, 12, 24]);
        let words: Vec<String> = fixture_mnemonic()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        s.set_answer(0, &words[0]);
        s.set_answer(1, &words[11]);
        s.set_answer(2, &words[23]);
        assert!(s.is_correct());
    }

    #[test]
    fn is_correct_false_for_wrong_word() {
        let mut s = RecoveryVerifyScreen::new();
        s.seed_mnemonic(fixture_mnemonic());
        s.seed_challenge([1, 12, 24]);
        s.set_answer(0, "nope");
        s.set_answer(1, "abstract");
        s.set_answer(2, "actual");
        assert!(!s.is_correct());
    }

    #[test]
    fn reseed_clears_answers_and_picks_new_challenge() {
        let mut s = RecoveryVerifyScreen::new();
        s.seed_mnemonic(fixture_mnemonic());
        s.set_answer(0, "stale");
        s.set_answer(1, "stale");
        s.set_answer(2, "stale");
        s.reseed();
        for i in 0..3 {
            assert!(s.answer(i).unwrap().is_empty());
        }
        assert!(s.challenge().is_some());
    }

    #[test]
    fn failed_attempt_flag_sticky_until_user_types() {
        let mut s = RecoveryVerifyScreen::new();
        s.seed_mnemonic(fixture_mnemonic());
        s.seed_challenge([1, 12, 24]);
        s.set_answer(0, "wrong");
        s.set_answer(1, "wrong");
        s.set_answer(2, "wrong");
        assert!(!s.is_correct());
        // Manually flip the flag the way the click handler would.
        s.last_attempt_failed = true;
        assert!(s.last_attempt_failed());
        // Editing any slot clears the flag.
        s.set_answer(0, "");
        assert!(!s.last_attempt_failed());
    }

    /// The blanks sit in the cells the share screen shows the same words
    /// in: one control tall, in the word column of their grid cell.
    #[test]
    fn plan_places_each_blank_in_its_words_grid_cell() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let body = Rect::new(100.0, 200.0, 740.0, 200.0);
        let mut s = RecoveryVerifyScreen::new();
        s.seed_mnemonic(fixture_mnemonic());
        s.seed_challenge([1, 12, 24]);
        let plan = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
        assert!((plan.grid.height() - 6.0 * 34.0).abs() < 1e-9);
        let grid = WordGrid::new(plan.grid, 34.0, &mut tr, &theme);
        for (slot, pos1) in [1usize, 12, 24].iter().enumerate() {
            let (_, word) = grid.cell(pos1 - 1, &theme);
            assert_eq!(plan.slots[slot], word, "slot {slot} sits in cell {pos1}");
            assert_eq!(plan.slots[slot].height(), 34.0);
        }
        assert!(plan.slots[0].y0 < plan.slots[1].y0 && plan.slots[1].y0 < plan.slots[2].y0);
        assert!(plan.error.is_none());
        s.last_attempt_failed = true;
        let failed = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
        assert!(failed.error.is_some(), "a failed attempt gets its own row");
    }
}
