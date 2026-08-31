//! Screen 4 — pair this device with an existing identity.
//!
//! Counterpart of screen 3. Shows a 6-box monospace code input, a textarea
//! fallback for pasting the QR payload JSON (no camera in the test env, so
//! a scanner isn't wired up), and status + error banners.
//!
//! On submit: verify the code matches the payload's derived short code
//! (defence against typing the wrong code into the right payload), decode
//! the payload and build this device's DKG round-1/round-2 contributions,
//! publish the round-2 reply with an HMAC, then wait for the host's own
//! round-2 reply and adopt the DKG outcome.
//!
//! The end-to-end test exercises the full state machine via the in-memory
//! `PairingHandshake`.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use rand_core::OsRng;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use threshold_identity::{DkgSession, EnrolmentRound1, EnrolmentRound2, ThresholdIdentity};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey, SmolStr};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::{
    IdentityFlowError, PairingHandshake, PairingPayload, ScreenOutcome, compute_pairing_mac,
    derive_pairing_code, derive_pairing_mac_key, format_pairing_code,
};

/// The pairing code is exactly six digits, one per input box.
const CODE_LEN: usize = 6;

/// Composes `transform` with a translation to `(x, y)` for one draw call.
/// Goes through `Affine::pre_translate` so the composition stays inside
/// `kurbo`'s floating-point matrix math rather than reading as integer
/// arithmetic at every call site.
fn at(transform: Affine, x: f64, y: f64) -> Affine {
    transform.pre_translate(Vec2::new(x, y))
}

/// Widens a code-box index to `f64` for pixel-space arithmetic. The row
/// holds exactly [`CODE_LEN`] boxes, so this is lossless; going through
/// `u32` states that to the compiler instead of an `as` cast.
fn box_index_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

pub struct PairDeviceScreen {
    rect: Rect,
    /// 6-digit code, one box per digit.
    code_digits: [char; CODE_LEN],
    code_focus: usize,
    code_box_rects: [Rect; CODE_LEN],
    /// Pasted JSON payload from the QR (textarea fallback).
    payload_text: String,
    payload_focus: bool,
    payload_rect: Rect,
    submit_hovered: bool,
    submit_rect: Rect,
    last_cursor: Point,
    status: String,
    error: Option<String>,
    /// Once `true`, the container advances on the next event tick.
    completed: bool,
}

impl Default for PairDeviceScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl PairDeviceScreen {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            code_digits: ['_'; CODE_LEN],
            code_focus: 0,
            code_box_rects: [Rect::ZERO; CODE_LEN],
            payload_text: String::new(),
            payload_focus: false,
            payload_rect: Rect::ZERO,
            submit_hovered: false,
            submit_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            status: "Enter the 6-digit code shown on the other device.".to_string(),
            error: None,
            completed: false,
        }
    }

    /// Inject the 6-digit code (used by tests + the QR-scanner shim).
    pub fn set_code(&mut self, code: &str) {
        let digits = code.chars().filter(char::is_ascii_digit);
        for (slot, entered) in self
            .code_digits
            .iter_mut()
            .zip(digits.map(Some).chain(std::iter::repeat(None)))
        {
            *slot = entered.unwrap_or('_');
        }
    }

    /// Current code as a 6-character string; `"------"` when unset.
    pub fn code_string(&self) -> String {
        self.code_digits
            .iter()
            .map(|c| if *c == '_' { '-' } else { *c })
            .collect()
    }

    /// Parse the entered code to a u32; `None` if any slot is still empty.
    pub fn parse_code(&self) -> Option<u32> {
        let mut acc: u32 = 0;
        for c in &self.code_digits {
            let digit = c.to_digit(10)?;
            acc = acc.saturating_mul(10).saturating_add(digit);
        }
        Some(acc)
    }

    pub fn set_payload_text(&mut self, payload: impl Into<String>) {
        self.payload_text = payload.into();
        self.error = None;
    }

    pub fn payload_text(&self) -> &str {
        &self.payload_text
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_completed(&self) -> bool {
        self.completed
    }

    /// Sanity check that the typed code matches the payload's derived
    /// code, before launching the DKG.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError`] when the payload's nonce cannot be
    /// decoded, or when `code` does not match the code that nonce derives.
    pub fn validate_code_against_payload(
        code: u32,
        payload: &PairingPayload,
    ) -> Result<(), IdentityFlowError> {
        let nonce = payload.nonce()?;
        let derived = derive_pairing_code(&nonce);
        if derived != code {
            return Err(IdentityFlowError::DecodeFailed(format!(
                "code mismatch: got {}, payload derives {}",
                format_pairing_code(code),
                format_pairing_code(derived)
            )));
        }
        Ok(())
    }

    /// Drive the pairer's half of the DKG: builds a fresh share-2 session
    /// on this device, exchanges round-1/round-2 packages with the host
    /// via `PairingHandshake`, and adopts the resulting threshold identity.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError`] when the payload cannot be decoded, when
    /// any DKG round fails, when the round-2 reply cannot be encoded, or when
    /// the host never publishes its own round-2 within the poll window.
    pub fn perform_pairing(
        &mut self,
        payload: &PairingPayload,
        hs: &PairingHandshake,
    ) -> Result<ThresholdIdentity, IdentityFlowError> {
        let nonce = payload.nonce()?;
        let mac_key = derive_pairing_mac_key(&nonce);
        let host_round1 = payload.initiator_round1()?;
        // Build the pairer's own DKG session (share index 2 in a 2-of-2 group).
        let mut rng = OsRng;
        let mut my_identity = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
        let (mut my_session, _my_round1) = my_identity.begin_enrolment(2, 2, 2, &mut rng)?;
        // Process the host's round-1 → produce our round-2 reply.
        let outgoing = my_session.receive_round1(std::slice::from_ref(&host_round1))?;
        let reply = outgoing
            .into_iter()
            .find(|(idx, _)| *idx == 1)
            .map(|(_, r)| r)
            .ok_or_else(|| {
                IdentityFlowError::DecodeFailed("DKG produced no round-2 reply to share 1".into())
            })?;
        let reply_bytes = postcard::to_allocvec(&reply)
            .map_err(|e| IdentityFlowError::EncodeFailed(e.to_string()))?;
        let mac = compute_pairing_mac(&mac_key, &reply_bytes);
        hs.publish_pairer_reply(&nonce, reply, mac);
        // The pairer also needs the host's round-1 contribution, sent as the
        // host's own round-2 message; block-poll the in-memory shim for it.
        let mut host_reply: Option<EnrolmentRound2> = None;
        for _ in 0..16 {
            if let Some((r, _mac)) = hs.take_host_round2(&nonce) {
                host_reply = Some(r);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let host_reply = host_reply.ok_or_else(|| {
            IdentityFlowError::DecodeFailed("host never published round-2 to pairer".into())
        })?;
        my_session.receive_round2(&[host_reply])?;
        my_identity.adopt_dkg_outcome(my_session)?;
        self.completed = true;
        self.status = "Paired.".to_string();
        Ok(my_identity)
    }

    /// Companion to `perform_pairing` that *exposes* the round-1 the
    /// pairer would send, for tests that inspect the wire format.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError`] when bootstrapping the device identity or
    /// beginning its enrolment round fails.
    pub fn produce_pairer_round1() -> Result<(DkgSession, EnrolmentRound1), IdentityFlowError> {
        let mut rng = OsRng;
        let mut id = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
        Ok(id.begin_enrolment(2, 2, 2, &mut rng)?)
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
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = p;
                let h = self.submit_rect.contains(p);
                if h != self.submit_hovered {
                    self.submit_hovered = h;
                    *handled = true;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_mouse_press(handled),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => self.handle_key_press(logical_key, text.as_ref(), handled),
            _ => ScreenOutcome::Stay,
        }
    }

    /// `MouseInput` arm of [`Self::handle_event_screen`]: focuses a code box,
    /// the payload field, or triggers submit, whichever rect the last cursor
    /// position landed in.
    fn handle_mouse_press(&mut self, handled: &mut bool) -> ScreenOutcome {
        for (i, r) in self.code_box_rects.iter().enumerate() {
            if r.contains(self.last_cursor) {
                self.code_focus = i;
                self.payload_focus = false;
                *handled = true;
                return ScreenOutcome::Stay;
            }
        }
        if self.payload_rect.contains(self.last_cursor) {
            self.payload_focus = true;
            *handled = true;
            return ScreenOutcome::Stay;
        }
        if self.submit_rect.contains(self.last_cursor) {
            *handled = true;
            if self.completed {
                return ScreenOutcome::Advance;
            }
            self.error = None;
            self.status = "Pairing…".to_string();
            // Driving the handshake needs the `PairingHandshake` the shell
            // owns; the screen exposes `perform_pairing` for the shell to
            // call. A click here just checks both fields are filled.
            if self.parse_code().is_none() || self.payload_text.is_empty() {
                self.error = Some("Enter both the code and the QR payload.".to_string());
            }
            return ScreenOutcome::Stay;
        }
        ScreenOutcome::Stay
    }

    /// `KeyboardInput` arm of [`Self::handle_event_screen`]: routes typing
    /// and backspace to either the payload textarea or the focused code box,
    /// depending on which currently has focus.
    fn handle_key_press(
        &mut self,
        logical_key: &Key,
        text: Option<&SmolStr>,
        handled: &mut bool,
    ) -> ScreenOutcome {
        if self.payload_focus {
            match logical_key {
                Key::Named(NamedKey::Backspace) => {
                    self.payload_text.pop();
                    *handled = true;
                }
                _ => {
                    if let Some(t) = text {
                        for c in t.chars() {
                            if c == '\u{8}' || c == '\u{7f}' {
                                continue;
                            }
                            self.payload_text.push(c);
                            *handled = true;
                        }
                    }
                }
            }
        } else {
            match logical_key {
                Key::Named(NamedKey::Backspace) => {
                    if let Some(prev) = self.code_focus.checked_sub(1)
                        && let Some(slot) = self.code_digits.get_mut(prev)
                    {
                        *slot = '_';
                        self.code_focus = prev;
                        *handled = true;
                    }
                }
                _ => {
                    if let Some(t) = text {
                        for c in t.chars() {
                            if !c.is_ascii_digit() {
                                continue;
                            }
                            if let Some(slot) = self.code_digits.get_mut(self.code_focus) {
                                *slot = c;
                                self.code_focus = self.code_focus.saturating_add(1);
                                *handled = true;
                            }
                        }
                    }
                }
            }
        }
        ScreenOutcome::Stay
    }

    /// The card panel behind everything, plus its heading and status line.
    fn draw_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        pad: f64,
    ) {
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
            "Pair with existing identity",
            22.0,
            theme.tokens.colors.text_primary,
            at(transform, card.x0 + pad, card.y0 + pad),
            None,
        );
        text_renderer.draw_text(
            scene,
            &self.status,
            13.0,
            theme.tokens.colors.text_dim,
            at(transform, card.x0 + pad, card.y0 + pad + 28.0),
            None,
        );
    }

    /// The six monospace digit boxes, each with the digit typed into it.
    fn draw_code_boxes(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        for (i, (r, ch)) in self
            .code_box_rects
            .iter()
            .zip(self.code_digits.iter())
            .enumerate()
        {
            let focused = self.code_focus == i && !self.payload_focus;
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
            let display: String = if *ch == '_' {
                String::new()
            } else {
                ch.to_string()
            };
            let (w, _) = text_renderer.measure(&display, 26.0, None);
            text_renderer.draw_text(
                scene,
                &display,
                26.0,
                theme.tokens.colors.text_primary,
                at(
                    transform,
                    r.x0 + (r.width() - f64::from(w)) / 2.0,
                    r.y0 + (r.height() - 26.0) / 2.0,
                ),
                None,
            );
        }
    }

    /// The QR-payload textarea fallback, showing its placeholder when empty.
    fn draw_payload_field(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &RoundedRect::from_rect(self.payload_rect, 8.0),
        );
        scene.stroke(
            &Stroke::new(if self.payload_focus { 2.0 } else { 1.0 }),
            transform,
            if self.payload_focus {
                theme.border_focus
            } else {
                theme.tokens.colors.border_standard
            },
            None,
            &RoundedRect::from_rect(self.payload_rect, 8.0),
        );
        let placeholder = if self.payload_text.is_empty() {
            "Paste the QR payload here (TODO: real scanner)"
        } else {
            self.payload_text.as_str()
        };
        let color = if self.payload_text.is_empty() {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.text_primary
        };
        text_renderer.draw_text(
            scene,
            placeholder,
            13.0,
            color,
            at(
                transform,
                self.payload_rect.x0 + 12.0,
                self.payload_rect.y0 + 12.0,
            ),
            None,
        );
    }

    /// The primary button and, when one is set, the error line above it.
    fn draw_submit_and_error(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        pad: f64,
    ) {
        let bg = if self.submit_hovered {
            theme.bg_active
        } else {
            theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX]
        };
        scene.fill(
            Fill::NonZero,
            transform,
            bg,
            None,
            &RoundedRect::from_rect(self.submit_rect, 8.0),
        );
        let label = if self.completed { "Continue" } else { "Pair" };
        let (lw, _) = text_renderer.measure(label, 14.0, None);
        text_renderer.draw_text(
            scene,
            label,
            14.0,
            Color::from_rgb8(255, 255, 255),
            at(
                transform,
                self.submit_rect.x0 + (self.submit_rect.width() - f64::from(lw)) / 2.0,
                self.submit_rect.y0 + (self.submit_rect.height() - 14.0) / 2.0,
            ),
            None,
        );
        if let Some(err) = &self.error {
            text_renderer.draw_text(
                scene,
                err,
                12.0,
                Color::from_rgb8(220, 80, 80),
                at(transform, card.x0 + pad, self.submit_rect.y0 - 24.0),
                None,
            );
        }
    }
}

impl Widget for PairDeviceScreen {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(720.0),
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
        let card_w = 560.0;
        let card_h = 460.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let pad = 32.0;
        let box_w = 56.0;
        let box_h = 64.0;
        let gap = 8.0;
        let total = 6.0 * box_w + 5.0 * gap;
        let start_x = card_x + (card_w - total) / 2.0;
        let row_y = card_y + 100.0;
        for (i, slot) in self.code_box_rects.iter_mut().enumerate() {
            let x = start_x + box_index_to_f64(i) * (box_w + gap);
            *slot = Rect::new(x, row_y, x + box_w, row_y + box_h);
        }
        self.payload_rect = Rect::new(
            card_x + pad,
            card_y + 220.0,
            card_x + card_w - pad,
            card_y + 220.0 + 120.0,
        );
        self.submit_rect = Rect::new(
            card_x + card_w - pad - 160.0,
            card_y + card_h - pad - 44.0,
            card_x + card_w - pad,
            card_y + card_h - pad,
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
        let card_h = 460.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let card = Rect::new(card_x, card_y, card_x + card_w, card_y + card_h);
        let pad = 32.0;

        self.draw_card(scene, transform, text_renderer, theme, card, pad);
        self.draw_code_boxes(scene, transform, text_renderer, theme);
        self.draw_payload_field(scene, transform, text_renderer, theme);
        self.draw_submit_and_error(scene, transform, text_renderer, theme, card, pad);
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
        "PairDeviceScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_pair_device")
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

    #[test]
    fn set_code_handles_partial_input() {
        let mut s = PairDeviceScreen::new();
        s.set_code("123");
        assert_eq!(s.code_string(), "123---");
        s.set_code("987654");
        assert_eq!(s.code_string(), "987654");
    }

    #[test]
    fn parse_code_requires_six_digits() {
        let mut s = PairDeviceScreen::new();
        assert!(s.parse_code().is_none());
        s.set_code("12345");
        assert!(s.parse_code().is_none());
        s.set_code("000042");
        assert_eq!(s.parse_code(), Some(42));
    }

    #[test]
    fn validate_code_against_payload_round_trip() {
        let mut rng = rand_core::OsRng;
        let mut id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (_sess, r1) = id.begin_enrolment(1, 2, 2, &mut rng).unwrap();
        let nonce = [0x77u8; 16];
        let payload = PairingPayload::new(nonce, id.group_public_key(), &r1).unwrap();
        let derived = derive_pairing_code(&nonce);
        // Correct code accepted.
        PairDeviceScreen::validate_code_against_payload(derived, &payload).unwrap();
        // Wrong code rejected.
        let err =
            PairDeviceScreen::validate_code_against_payload(derived.wrapping_add(1), &payload)
                .unwrap_err();
        assert!(matches!(err, IdentityFlowError::DecodeFailed(_)));
    }
}
