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

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use crate::widget::identity_flow::{RecoveryChallenge, ScreenOutcome};

/// Screen 2b: type-3-words verification.
pub struct RecoveryVerifyScreen {
    rect: Rect,
    mnemonic: String,
    challenge: Option<RecoveryChallenge>,
    /// User's typed answers, one per challenge slot.
    answers: [String; 3],
    /// Index of the currently-focused slot (0..3) or `None`.
    focused_slot: Option<usize>,
    /// Whether the last submit produced a mismatch — drives the
    /// inline error message.
    last_attempt_failed: bool,
    slot_rects: [Rect; 3],
    continue_rect: Rect,
    reseed_rect: Rect,
    last_cursor: Point,
}

impl RecoveryVerifyScreen {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            mnemonic: String::new(),
            challenge: None,
            answers: [String::new(), String::new(), String::new()],
            focused_slot: None,
            last_attempt_failed: false,
            slot_rects: [Rect::ZERO; 3],
            continue_rect: Rect::ZERO,
            reseed_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
        }
    }

    /// Inject the mnemonic the verify step quizzes against. Resets
    /// the challenge.
    pub fn seed_mnemonic(&mut self, mnemonic: String) {
        self.mnemonic = mnemonic;
        self.challenge = RecoveryChallenge::from_mnemonic(&self.mnemonic).ok();
        self.answers = [String::new(), String::new(), String::new()];
        self.last_attempt_failed = false;
    }

    pub fn challenge(&self) -> Option<&RecoveryChallenge> {
        self.challenge.as_ref()
    }

    /// Pick a fresh 3-word challenge.
    pub fn reseed(&mut self) {
        self.challenge = RecoveryChallenge::from_mnemonic(&self.mnemonic).ok();
        self.answers = [String::new(), String::new(), String::new()];
        self.last_attempt_failed = false;
    }

    /// Inject a deterministic challenge — used by tests so the order
    /// of `positions` is reproducible.
    pub fn seed_challenge(&mut self, positions: [usize; 3]) {
        self.challenge = RecoveryChallenge::at_positions(&self.mnemonic, positions).ok();
        self.answers = [String::new(), String::new(), String::new()];
        self.last_attempt_failed = false;
    }

    pub fn set_answer(&mut self, slot: usize, value: impl Into<String>) {
        if slot < 3 {
            self.answers[slot] = value.into();
            self.last_attempt_failed = false;
        }
    }

    pub fn answer(&self, slot: usize) -> Option<&str> {
        self.answers.get(slot).map(|s| s.as_str())
    }

    pub fn last_attempt_failed(&self) -> bool {
        self.last_attempt_failed
    }

    /// True when all three slots match the challenge.
    pub fn is_correct(&self) -> bool {
        match &self.challenge {
            None => false,
            Some(c) => {
                let r0 = self.answers[0].as_str();
                let r1 = self.answers[1].as_str();
                let r2 = self.answers[2].as_str();
                c.check(&[r0, r1, r2])
            }
        }
    }

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
                    } else {
                        self.last_attempt_failed = true;
                    }
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
                let answer = &mut self.answers[slot];
                match logical_key {
                    Key::Named(NamedKey::Backspace) => {
                        if !answer.is_empty() {
                            // Pop the last char.
                            let new_len = answer
                                .char_indices()
                                .last()
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            answer.truncate(new_len);
                            self.last_attempt_failed = false;
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::Tab) => {
                        // Move focus to next slot.
                        self.focused_slot = Some((slot + 1) % 3);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        *handled = true;
                        if self.is_correct() {
                            return ScreenOutcome::Advance;
                        } else {
                            self.last_attempt_failed = true;
                        }
                    }
                    _ => {
                        if let Some(t) = text {
                            for c in t.chars() {
                                if c.is_control() {
                                    continue;
                                }
                                answer.push(c);
                                self.last_attempt_failed = false;
                                *handled = true;
                            }
                        }
                    }
                }
                ScreenOutcome::Stay
            }
            _ => ScreenOutcome::Stay,
        }
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
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(800.0),
                    height: Dimension::length(560.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x as f64 + layout.location.x as f64,
            origin.y as f64 + layout.location.y as f64,
            origin.x as f64 + layout.location.x as f64 + layout.size.width as f64,
            origin.y as f64 + layout.location.y as f64 + layout.size.height as f64,
        );
        let card_w = 560.0;
        let card_h = 440.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let card = Rect::new(card_x, card_y, card_x + card_w, card_y + card_h);
        let pad = 32.0;
        let slot_w = (card_w - 2.0 * pad - 32.0) / 3.0;
        for i in 0..3 {
            let x = card.x0 + pad + i as f64 * (slot_w + 16.0);
            let y = card.y0 + 170.0;
            self.slot_rects[i] = Rect::new(x, y, x + slot_w, y + 40.0);
        }
        self.reseed_rect = Rect::new(
            card.x0 + pad,
            card.y1 - pad - 44.0,
            card.x0 + pad + 140.0,
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
        let card_w = 560.0;
        let card_h = 440.0;
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
            "Type the words at the positions below to prove you have them.",
            13.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((card.x0 + pad, card.y0 + pad + 32.0)),
            None,
        );

        // Position labels above each slot.
        if let Some(c) = &self.challenge {
            for (i, r) in self.slot_rects.iter().enumerate() {
                text_renderer.draw_text(
                    scene,
                    &format!("Word #{:>2}", c.positions[i]),
                    13.0,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((r.x0, r.y0 - 20.0)),
                    None,
                );
                let focused = self.focused_slot == Some(i);
                let border = if focused {
                    theme.border_focus
                } else {
                    theme.tokens.colors.border_standard
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.base,
                    None,
                    &RoundedRect::from_rect(*r, 6.0),
                );
                scene.stroke(
                    &Stroke::new(if focused { 2.0 } else { 1.0 }),
                    transform,
                    border,
                    None,
                    &RoundedRect::from_rect(*r, 6.0),
                );
                text_renderer.draw_text(
                    scene,
                    self.answers[i].as_str(),
                    14.0,
                    theme.tokens.colors.text_primary,
                    transform * Affine::translate((r.x0 + 10.0, r.y0 + 12.0)),
                    None,
                );
            }
        }

        // Reseed button.
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
        text_renderer.draw_text(
            scene,
            "Pick new words",
            13.0,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((
                    self.reseed_rect.x0 + 16.0,
                    self.reseed_rect.y0 + (self.reseed_rect.height() - 13.0) / 2.0,
                )),
            None,
        );

        // Continue button.
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
                    self.continue_rect.x0 + (self.continue_rect.width() - lw as f64) / 2.0,
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
                transform
                    * Affine::translate((card.x0 + pad, self.continue_rect.y0 - 24.0)),
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

    fn name(&self) -> &str {
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
