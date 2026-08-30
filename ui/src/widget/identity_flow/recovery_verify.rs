//! Screen 2b — recovery-share verification.
//!
//! After screen 2 shows the 24 words, screen 2b challenges the user
//! to type three of them back. Three positions are picked at random
//! when the screen is shown for the first time; the user types each
//! word into the matching slot; the "Continue" button activates
//! when all three slots match (case-insensitive, whitespace
//! trimmed).
//!
//! The challenge re-seeds when the user clicks "Pick new words" —
//! lets them retry without losing earlier progress.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::{LineEditState, RecoveryChallenge, ScreenOutcome};

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
        if slot < 3 {
            self.editors[slot] = LineEditState::with_text(value);
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
        _modifiers: &winit::event::Modifiers,
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
                                _modifiers.state().shift_key(),
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
                        let editor = &mut self.editors[slot];
                        if editor.apply_key(logical_key, _modifiers, text.as_deref()) {
                            self.last_attempt_failed = false;
                            *handled = true;
                        }
                    }
                }
                ScreenOutcome::Stay
            }
            _ => ScreenOutcome::Stay,
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
    let current_pos = positions[slot];
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
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(800.0),
                height: Dimension::length(560.0),
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
        // Matches recovery_share's card dimensions so the two screens feel
        // like the same surface — only the blanks differ.
        let card_w = 680.0;
        let card_h = 540.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let card = Rect::new(card_x, card_y, card_x + card_w, card_y + card_h);
        let pad = 32.0;

        // 6×4 grid, mirroring recovery_share's `apply_layout`.
        let grid_x = card.x0 + pad;
        let grid_y = card.y0 + 110.0;
        let col_w = (card_w - 2.0 * pad) / 4.0;
        let row_h = 50.0;

        if let Some(c) = &self.challenge {
            for (slot_i, pos1) in c.positions.iter().enumerate() {
                let zero = pos1.saturating_sub(1);
                let row = zero / 4;
                let col = zero % 4;
                let cell_x = grid_x + col as f64 * col_w + 28.0;
                let cell_y = grid_y + row as f64 * row_h;
                // Editable slot is the "value area" of the grid cell.
                let slot_x0 = cell_x;
                let slot_y0 = cell_y - 4.0;
                let slot_x1 = grid_x + (col as f64 + 1.0) * col_w - 8.0;
                let slot_y1 = slot_y0 + 30.0;
                self.slot_rects[slot_i] = Rect::new(slot_x0, slot_y0, slot_x1, slot_y1);
            }
        } else {
            self.slot_rects = [Rect::ZERO; 3];
        }

        self.reseed_rect = Rect::new(
            card.x0 + pad,
            card.y1 - pad - 44.0,
            card.x0 + pad + 180.0,
            card.y1 - pad,
        );
        self.continue_rect = Rect::new(
            card.x1 - pad - 160.0,
            card.y1 - pad - 44.0,
            card.x1 - pad,
            card.y1 - pad,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let backdrop = Color::from_rgba8(0, 0, 0, 180);
        scene.fill(Fill::NonZero, transform, backdrop, None, &self.rect);
        let card_w = 680.0;
        let card_h = 540.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let card = Rect::new(card_x, card_y, card_x + card_w, card_y + card_h);
        let pad = 32.0;
        let radius = 12.0;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &RoundedRect::from_rect(card, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(card, radius),
        );

        text_renderer.draw_text(
            scene,
            "Confirm your recovery words",
            22.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((card.x0 + pad, card.y0 + pad)),
            None,
        );
        text_renderer.draw_text(
            scene,
            "Compare with your written list. Fill in the highlighted blanks to confirm.",
            13.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((card.x0 + pad, card.y0 + pad + 32.0)),
            None,
        );

        // 24-word grid mirroring `recovery_share`. Three positions are
        // blanked for the user to fill; the other 21 stay read-only so the
        // user can scan the full list against their written-down copy.
        let challenge_positions: [usize; 3] =
            self.challenge.as_ref().map_or([0, 0, 0], |c| c.positions);
        let words: Vec<&str> = self.mnemonic.split_whitespace().collect();
        let grid_x = card.x0 + pad;
        let grid_y = card.y0 + 110.0;
        let col_w = (card_w - 2.0 * pad) / 4.0;
        let row_h = 50.0;
        for i in 0..24usize {
            let row = i / 4;
            let col = i % 4;
            let pos1 = i + 1;
            let x = grid_x + col as f64 * col_w;
            let y = grid_y + row as f64 * row_h;

            let num = format!("{pos1:>2}.");
            text_renderer.draw_text(
                scene,
                &num,
                12.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x, y + 8.0)),
                None,
            );

            let slot = challenge_positions.iter().position(|&p| p == pos1);
            if let Some(slot_i) = slot {
                let r = self.slot_rects[slot_i];
                let focused = self.focused_slot == Some(slot_i);
                let answer_text = self.editors[slot_i].text.as_str();
                let filled_correct = self.challenge.as_ref().is_some_and(|c| {
                    answer_text
                        .trim()
                        .eq_ignore_ascii_case(&c.expected_word(slot_i))
                });
                let border = if focused {
                    theme.border_focus
                } else if filled_correct {
                    theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX]
                } else if self.last_attempt_failed && !answer_text.is_empty() {
                    Color::from_rgb8(220, 60, 60)
                } else {
                    theme.tokens.colors.border_standard
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.base,
                    None,
                    &RoundedRect::from_rect(r, 6.0),
                );
                scene.stroke(
                    &Stroke::new(if focused { 2.0 } else { 1.0 }),
                    transform,
                    border,
                    None,
                    &RoundedRect::from_rect(r, 6.0),
                );
                if let Some((lo, hi)) = self.editors[slot_i].selection_range() {
                    let (pre_w, _) = text_renderer.measure(&answer_text[..lo], 14.0, None);
                    let (mid_w, _) = text_renderer.measure(&answer_text[lo..hi], 14.0, None);
                    let sel_color = theme.accent_primary.with_alpha_factor(0.30);
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        sel_color,
                        None,
                        &Rect::new(
                            r.x0 + 8.0 + f64::from(pre_w),
                            r.y0 + 5.0,
                            r.x0 + 8.0 + f64::from(pre_w) + f64::from(mid_w),
                            r.y0 + r.height() - 5.0,
                        ),
                    );
                }
                text_renderer.draw_text(
                    scene,
                    answer_text,
                    14.0,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((r.x0 + 8.0, r.y0 + 7.0)),
                    None,
                );
                // Caret if focused. Painted over the selection.
                if focused {
                    let cursor_byte = self.editors[slot_i].cursor.min(answer_text.len());
                    let prefix = &answer_text[..cursor_byte];
                    let (prefix_w, _) = text_renderer.measure(prefix, 14.0, None);
                    let caret_x = r.x0 + 8.0 + f64::from(prefix_w);
                    let caret_top = r.y0 + 5.0;
                    let caret_bottom = r.y0 + r.height() - 5.0;
                    let caret_rect = Rect::new(caret_x, caret_top, caret_x + 1.5, caret_bottom);
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX],
                        None,
                        &caret_rect,
                    );
                }
            } else {
                let word = words.get(i).copied().unwrap_or("");
                text_renderer.draw_text(
                    scene,
                    word,
                    14.0,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((x + 28.0, y + 6.0)),
                    None,
                );
            }
        }

        // Reseed button: picks DIFFERENT blanks against the same mnemonic;
        // does not regenerate the recovery share itself.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &RoundedRect::from_rect(self.reseed_rect, 8.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(self.reseed_rect, 8.0),
        );
        let (rw, _) = text_renderer.measure("Pick different blanks", 13.0, None);
        text_renderer.draw_text(
            scene,
            "Pick different blanks",
            13.0,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((
                    self.reseed_rect.x0 + (self.reseed_rect.width() - f64::from(rw)) / 2.0,
                    self.reseed_rect.y0 + (self.reseed_rect.height() - 13.0) / 2.0,
                )),
            None,
        );

        let ready = self.is_correct();
        let bg = if ready {
            theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX]
        } else {
            theme.tokens.colors.surfaces.base
        };
        let text_color = if ready {
            Color::from_rgb8(255, 255, 255)
        } else {
            theme.tokens.colors.text_dim
        };
        scene.fill(
            Fill::NonZero,
            transform,
            bg,
            None,
            &RoundedRect::from_rect(self.continue_rect, 8.0),
        );
        let (lw, _) = text_renderer.measure("Finish setup", 14.0, None);
        text_renderer.draw_text(
            scene,
            "Finish setup",
            14.0,
            text_color,
            transform
                * Affine::translate((
                    self.continue_rect.x0 + (self.continue_rect.width() - f64::from(lw)) / 2.0,
                    self.continue_rect.y0 + (self.continue_rect.height() - 14.0) / 2.0,
                )),
            None,
        );

        if self.last_attempt_failed {
            text_renderer.draw_text(
                scene,
                "One or more words didn't match. Try again or pick new words.",
                12.0,
                Color::from_rgb8(220, 80, 80),
                transform * Affine::translate((card.x0 + pad, self.continue_rect.y0 - 24.0)),
                None,
            );
        }
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
            .map(|s| s.to_string())
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
}
