//! Screen 3 — host the pairing handshake for a new device.
//!
//! Shows:
//! - A live QR encoding the pairing-channel JSON payload.
//! - The same payload's 6-digit short code (derived from the nonce).
//! - A "Waiting for the other device…" spinner / status label.
//! - A cancel button.
//!
//! When the new device sends its DKG round-2 reply the host's
//! background polling task picks it up, completes the DKG, and the
//! screen advances. The screen itself is *passive* — it just renders
//! state and reports back when the protocol completes. Driving the
//! actual round-2 receive happens in the shell-side
//! `identity_provisioning` module via the bounded, pinned-TLS broker. Focused
//! unit tests may inject an in-memory `PairingHandshake`; the shell integration
//! test drives the full protocol over an independent socket.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use collab_types::Ed25519PublicKey;
use rand_core::{OsRng, RngCore};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use threshold_identity::{DkgSession, EnrolmentRound1, EnrolmentRound2};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::{
    IdentityFlowError, PairingHandshake, PairingPayload, ScreenOutcome, derive_pairing_code,
    format_pairing_code,
};
#[cfg(test)]
use crate::widget::identity_flow::{compute_pairing_mac, derive_pairing_mac_key};
use crate::widget::qr::QrWidget;

/// Returned by `host_complete_dkg` — the new key material the host
/// adopts after the pairer finishes. Pre-built so the test can
/// inspect every field without poking the engine.
#[derive(Debug)]
pub struct HostDkgOutcome {
    pub completed: bool,
    pub round2_to_pairer: Option<EnrolmentRound2>,
    /// Host's newly rotated threshold identity. The caller must persist this
    /// through the device secret provider before reporting success.
    pub identity: Option<threshold_identity::ThresholdIdentity>,
}

/// State + rendering for hosting a device-pairing handshake — the QR + short
/// code + status the new device pairs against.
pub struct AddDeviceHostScreen {
    rect: Rect,
    qr_rect: Rect,
    cancel_rect: Rect,
    cancel_hovered: bool,
    last_cursor: Point,
    /// QR widget cached so re-rendering doesn't re-encode every frame.
    pub qr: Option<QrWidget>,
    /// Active handshake state. `None` until `begin` is called.
    session: Option<HostHandshakeState>,
    /// Final-line status, surfaced beneath the QR.
    status: String,
    completed_identity: Option<threshold_identity::ThresholdIdentity>,
}

/// Per-handshake state, owned by the host screen.
pub struct HostHandshakeState {
    pub nonce: [u8; 16],
    pub code: u32,
    pub payload: PairingPayload,
    /// Initiator's `DkgSession` — the engine handle the host drives
    /// once the pairer replies. Kept inside `Option<…>` on the screen
    /// because Rust's borrow checker prefers explicit "drained" via
    /// `take` to "leaked" via `&mut`.
    pub session: Option<DkgSession>,
    /// Cached round-1 packet — the same payload encoded inside the
    /// QR, kept here so `host_complete_dkg` can echo it during
    /// round-2 receive.
    pub round1: EnrolmentRound1,
    pub initiator_pk: Ed25519PublicKey,
    /// True once the pairer's round-2 reply has been adopted.
    pub completed: bool,
}

impl Default for AddDeviceHostScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl AddDeviceHostScreen {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            qr_rect: Rect::ZERO,
            cancel_rect: Rect::ZERO,
            cancel_hovered: false,
            last_cursor: Point::ZERO,
            qr: None,
            session: None,
            status: "Begin pairing to see a QR code.".to_string(),
            completed_identity: None,
        }
    }

    /// Begin a new pairing handshake. The host picks the nonce,
    /// generates a fresh `DkgSession`, and primes the QR widget +
    /// status label.
    ///
    /// The `initiator_pk` is the host's existing single-device
    /// identity public key — embedded in the payload so the pairer
    /// can address its reply.
    ///
    /// # Errors
    /// Returns an error if the pairing payload cannot be constructed or
    /// JSON-encoded.
    pub fn begin(
        &mut self,
        host_identity_round1: EnrolmentRound1,
        host_session: DkgSession,
        initiator_pk: Ed25519PublicKey,
    ) -> Result<(), IdentityFlowError> {
        let mut nonce = [0u8; 16];
        OsRng.fill_bytes(&mut nonce);
        let payload = PairingPayload::new(nonce, &initiator_pk, &host_identity_round1)?;
        let code = derive_pairing_code(&nonce);
        let json = payload.to_json_bytes()?;
        self.qr = Some(QrWidget::new(json));
        self.session = Some(HostHandshakeState {
            nonce,
            code,
            payload,
            session: Some(host_session),
            round1: host_identity_round1,
            initiator_pk,
            completed: false,
        });
        self.completed_identity = None;
        self.status = format!(
            "Waiting for the new device. Show them code {}.",
            format_pairing_code(code)
        );
        Ok(())
    }

    /// Current 6-digit pairing code as a zero-padded string. Returns
    /// `None` before `begin` has run.
    pub fn pairing_code_display(&self) -> Option<String> {
        self.session.as_ref().map(|s| format_pairing_code(s.code))
    }

    /// Reproduce the 6-digit code from the JSON payload alone — used
    /// by the headline test to confirm a QR scanner that pasted the
    /// payload arrives at the same code.
    ///
    /// # Errors
    /// Returns an error if the payload's nonce cannot be decoded.
    pub fn derive_code_from_payload(payload: &PairingPayload) -> Result<u32, IdentityFlowError> {
        let nonce = payload.nonce()?;
        Ok(derive_pairing_code(&nonce))
    }

    /// JSON-encoded payload (what the QR contains). `None` before
    /// begin.
    pub fn payload_json(&self) -> Option<Vec<u8>> {
        self.session
            .as_ref()
            .and_then(|s| s.payload.to_json_bytes().ok())
    }

    /// Bind the independently running TLS broker to the already-created
    /// ceremony and regenerate the QR. This must happen before the QR is
    /// shown; the endpoint and exact certificate become part of every frame's
    /// authenticated transcript.
    pub fn attach_broker(
        &mut self,
        endpoint: impl Into<String>,
        certificate_der: &[u8],
    ) -> Result<(), IdentityFlowError> {
        let session = self.session.as_mut().ok_or_else(|| {
            IdentityFlowError::DecodeFailed("begin pairing before attaching broker".into())
        })?;
        session.payload.set_broker(endpoint, certificate_der)?;
        self.qr = Some(QrWidget::new(session.payload.to_json_bytes()?));
        Ok(())
    }

    pub fn handshake(&self) -> Option<&HostHandshakeState> {
        self.session.as_ref()
    }

    /// Process the pairer's round-2 reply.
    ///
    /// Verifies the MAC, drains the `DkgSession`, runs
    /// `receive_round2` on it, and produces the host's own round-2
    /// contribution which the pairer needs to finalise. The caller
    /// (the shell broker) is responsible for publishing that reply
    /// back onto the pairing channel.
    ///
    /// # Errors
    /// Returns an error if there is no active handshake, the reply's MAC
    /// is invalid, the session was already drained, or the underlying DKG
    /// round fails.
    pub fn host_complete_dkg(
        &mut self,
        pairer_round1: EnrolmentRound1,
        reply: EnrolmentRound2,
        mac: [u8; 64],
    ) -> Result<HostDkgOutcome, IdentityFlowError> {
        let s = self
            .session
            .as_mut()
            .ok_or_else(|| IdentityFlowError::DecodeFailed("no active handshake".into()))?;
        let expected = crate::widget::identity_flow::pairing_transcript_mac(
            &s.payload,
            "pairer-to-host",
            &pairer_round1,
            &reply,
        )?;
        if expected != mac {
            return Err(IdentityFlowError::PairingMacInvalid);
        }
        let mut session = s
            .session
            .take()
            .ok_or_else(|| IdentityFlowError::DecodeFailed("session already drained".into()))?;
        // The reply is addressed to share index 1 (the host); the pairer is
        // share index 2, so a single round-2 receive completes 2-of-2 DKG.
        let outgoing = session.receive_round1(&[pairer_round1])?;
        let round2_to_pairer = outgoing.get(&2).cloned().ok_or_else(|| {
            IdentityFlowError::DecodeFailed("DKG produced no round-2 packet for share 2".into())
        })?;
        session.receive_round2(&[reply])?;
        let mut rng = OsRng;
        let mut identity =
            threshold_identity::ThresholdIdentity::bootstrap_single_device(&mut rng)?;
        identity.adopt_dkg_outcome(session)?;
        s.completed = true;
        Ok(HostDkgOutcome {
            completed: true,
            round2_to_pairer: Some(round2_to_pairer),
            identity: Some(identity),
        })
    }

    /// Pump one tick of the host-side handshake by polling the
    /// pairing handshake for the pairer's reply. Returns `true` iff
    /// the handshake completed this tick.
    ///
    /// # Errors
    /// Returns an error if there is no active handshake, or if completing
    /// the DKG round for a pending reply fails.
    pub fn poll_handshake(&mut self, hs: &PairingHandshake) -> Result<bool, IdentityFlowError> {
        let s = self
            .session
            .as_ref()
            .ok_or_else(|| IdentityFlowError::DecodeFailed("no active handshake".into()))?;
        let nonce = s.nonce;
        if let Some((round1, reply, mac)) = hs.take_pairer_exchange(&nonce) {
            let pairer_round1 = round1.clone();
            let outcome = self.host_complete_dkg(round1, reply, mac)?;
            if let Some(round2) = outcome.round2_to_pairer.clone() {
                let payload = self
                    .session
                    .as_ref()
                    .ok_or_else(|| IdentityFlowError::DecodeFailed("session disappeared".into()))?
                    .payload
                    .clone();
                let tag = crate::widget::identity_flow::pairing_transcript_mac(
                    &payload,
                    "host-to-pairer",
                    &pairer_round1,
                    &round2,
                )?;
                hs.publish_host_round2(&nonce, round2, tag);
            }
            self.completed_identity = outcome.identity;
            self.status = if outcome.completed {
                "Device paired.".to_string()
            } else {
                "Waiting for the new device…".to_string()
            };
            Ok(outcome.completed)
        } else {
            Ok(false)
        }
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    /// Drain the host's completed threshold identity for immediate secure
    /// persistence. Pairing is not durable until the caller has done so.
    pub fn take_completed_identity(&mut self) -> Option<threshold_identity::ThresholdIdentity> {
        self.completed_identity.take()
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
                let h = self.cancel_rect.contains(p);
                if h != self.cancel_hovered {
                    self.cancel_hovered = h;
                    *handled = true;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.cancel_rect.contains(self.last_cursor) {
                    *handled = true;
                    return ScreenOutcome::Back;
                }
                ScreenOutcome::Stay
            }
            _ => ScreenOutcome::Stay,
        }
    }

    /// Draws the QR code once a handshake is active, or a placeholder
    /// panel + label before `begin` has been called.
    fn draw_qr(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(qr) = self.qr.as_mut() else {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &self.qr_rect,
            );
            text_renderer.draw_text(
                scene,
                "(QR will appear after begin)",
                12.0,
                theme.tokens.colors.text_dim,
                at(
                    transform,
                    self.qr_rect.x0 + 12.0,
                    self.qr_rect.center().y - 6.0,
                ),
                None,
            );
            return;
        };
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgb8(255, 255, 255),
            None,
            &self.qr_rect,
        );
        let total = modules_to_f64(qr.total_modules());
        let module = self.qr_rect.width() / total;
        let quiet_modules = qr.quiet_modules();
        let quiet = modules_to_f64(quiet_modules) * module;
        // The quiet zone runs around all four sides, so it is subtracted twice.
        let visible = qr
            .total_modules()
            .saturating_sub(quiet_modules.saturating_mul(2));
        for y in 0..visible {
            for x in 0..visible {
                if !qr.module(x, y) {
                    continue;
                }
                let px = self.qr_rect.x0 + quiet + modules_to_f64(x) * module;
                let py = self.qr_rect.y0 + quiet + modules_to_f64(y) * module;
                let cell = Rect::new(px, py, px + module, py + module);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    Color::from_rgb8(0, 0, 0),
                    None,
                    &cell,
                );
            }
        }
    }

    /// Draws the cancel button, hover-tinted per `self.cancel_hovered`.
    fn draw_cancel_button(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let cancel_bg = if self.cancel_hovered {
            theme.bg_hover
        } else {
            theme.tokens.colors.surfaces.base
        };
        scene.fill(
            Fill::NonZero,
            transform,
            cancel_bg,
            None,
            &RoundedRect::from_rect(self.cancel_rect, 8.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(self.cancel_rect, 8.0),
        );
        text_renderer.draw_text(
            scene,
            "Cancel",
            13.0,
            theme.tokens.colors.text_primary,
            at(
                transform,
                self.cancel_rect.x0 + 16.0,
                self.cancel_rect.y0 + (self.cancel_rect.height() - 13.0) / 2.0,
            ),
            None,
        );
    }
}

/// Composes `transform` with a translation to `(x, y)` for one draw call.
/// `Affine::translate` composition is floating-point matrix math, not
/// integer arithmetic — it cannot overflow or panic.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo Affine composition is floating-point matrix math; it cannot overflow or panic"
)]
fn at(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

/// Widens a QR module index/count to `f64` for pixel-space arithmetic.
/// Clamped to `u32::MAX` — a QR code never has anywhere near four billion
/// modules — so unlike a raw `as` cast this can never silently lose
/// precision.
fn modules_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

impl Widget for AddDeviceHostScreen {
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
                height: Dimension::length(640.0),
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
        let card_w = 520.0;
        let card_h = 540.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let qr_size = 280.0;
        let qr_x = card_x + (card_w - qr_size) / 2.0;
        let qr_y = card_y + 100.0;
        self.qr_rect = Rect::new(qr_x, qr_y, qr_x + qr_size, qr_y + qr_size);
        self.cancel_rect = Rect::new(
            card_x + 32.0,
            card_y + card_h - 32.0 - 44.0,
            card_x + 32.0 + 140.0,
            card_y + card_h - 32.0,
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
        let card_w = 520.0;
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
            "Pair another device",
            22.0,
            theme.tokens.colors.text_primary,
            at(transform, card.x0 + pad, card.y0 + pad),
            None,
        );
        self.draw_qr(scene, transform, text_renderer, theme);
        let code_label = self
            .pairing_code_display()
            .unwrap_or_else(|| "------".to_string());
        text_renderer.draw_text(
            scene,
            &format!("Code: {code_label}"),
            18.0,
            theme.tokens.colors.text_primary,
            at(transform, card.x0 + pad, self.qr_rect.y1 + 16.0),
            None,
        );
        text_renderer.draw_text(
            scene,
            &self.status,
            13.0,
            theme.tokens.colors.text_dim,
            at(transform, card.x0 + pad, self.qr_rect.y1 + 48.0),
            None,
        );
        self.draw_cancel_button(scene, transform, text_renderer, theme);
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
        "AddDeviceHostScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_add_device_host")
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
    use rand_core::OsRng;
    use threshold_identity::ThresholdIdentity;

    fn fixture_host() -> (AddDeviceHostScreen, [u8; 16]) {
        let mut rng = OsRng;
        let mut id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (sess, r1) = id.begin_enrolment(1, 2, 2, &mut rng).unwrap();
        let mut s = AddDeviceHostScreen::new();
        s.begin(r1, sess, *id.group_public_key()).unwrap();
        let nonce = s.handshake().unwrap().nonce;
        (s, nonce)
    }

    #[test]
    fn begin_populates_qr_payload_code_and_nonce() {
        let (s, nonce) = fixture_host();
        assert!(s.qr.is_some());
        let code = s.pairing_code_display().unwrap();
        assert_eq!(code.len(), 6);
        assert_eq!(derive_pairing_code(&nonce), s.handshake().unwrap().code);
    }

    #[test]
    fn payload_round_trips_to_same_code() {
        let (s, _) = fixture_host();
        let json = s.payload_json().unwrap();
        let payload = PairingPayload::from_json_bytes(&json).unwrap();
        let derived = AddDeviceHostScreen::derive_code_from_payload(&payload).unwrap();
        assert_eq!(
            format_pairing_code(derived),
            s.pairing_code_display().unwrap()
        );
    }

    #[test]
    fn pairer_reply_with_wrong_mac_is_rejected() {
        let (mut s, nonce) = fixture_host();
        let mut rng = OsRng;
        let mut pairer = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (_session, round1) = pairer.begin_enrolment(2, 2, 2, &mut rng).unwrap();
        let reply = EnrolmentRound2 {
            from_share_index: 2,
            to_share_index: 1,
            package_bytes: vec![1, 2, 3, 4],
        };
        let wrong_key = derive_pairing_mac_key(&[0xFFu8; 16]);
        let payload = postcard::to_allocvec(&reply).unwrap();
        let wrong_mac = compute_pairing_mac(&wrong_key, &payload);
        let err = s.host_complete_dkg(round1, reply, wrong_mac).unwrap_err();
        assert!(matches!(err, IdentityFlowError::PairingMacInvalid));
        let _ = nonce; // explicit unused
    }

    #[test]
    fn poll_handshake_returns_false_when_no_reply() {
        let (mut s, _) = fixture_host();
        let hs = PairingHandshake::new();
        assert!(!s.poll_handshake(&hs).unwrap());
    }

    #[test]
    fn broker_handshake_exchanges_both_real_dkg_rounds() {
        let (mut host, _) = fixture_host();
        let payload = host.handshake().unwrap().payload.clone();
        let channel = PairingHandshake::new();
        let pairer_channel = channel.clone();
        let pairer = std::thread::spawn(move || {
            let mut screen = crate::widget::identity_flow::pair_device::PairDeviceScreen::new();
            screen.perform_pairing(&payload, &pairer_channel).unwrap()
        });
        let mut completed = false;
        for _ in 0..500 {
            if host.poll_handshake(&channel).unwrap() {
                completed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(completed);
        let host_identity = host.take_completed_identity().unwrap();
        let pairer_identity = pairer.join().unwrap();
        assert_eq!(
            host_identity.group_public_key(),
            pairer_identity.group_public_key()
        );
        assert!(!host_identity.is_single_device());
        assert!(!pairer_identity.is_single_device());
    }

    #[test]
    fn begin_without_handshake_returns_no_code() {
        let s = AddDeviceHostScreen::new();
        assert!(s.pairing_code_display().is_none());
        assert!(s.payload_json().is_none());
    }
}
