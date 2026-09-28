//! Screen 3 — host the pairing handshake for a new device.
//!
//! Shows:
//! - A live QR encoding the pairing-channel JSON payload.
//! - The same payload's 6-digit short code (derived from the nonce).
//! - A "Waiting for the other device…" status label.
//! - A cancel action.
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

use collab_types::Ed25519PublicKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use threshold_identity::{DkgSession, EnrolmentRound1, EnrolmentRound2};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::sheet::{
    Align, BodyScroll, FORM_SHEET_WIDTH, Resolved, Role, Rows, SheetSpec, line_height, paint_cell,
    paint_qr, paint_text, paragraph_height, resolve, with_layout_renderer,
};
use crate::widget::identity_flow::{
    IdentityFlowError, PairingHandshake, PairingPayload, ScreenOutcome, derive_pairing_code,
    format_pairing_code,
};
#[cfg(test)]
use crate::widget::identity_flow::{compute_pairing_mac, derive_pairing_mac_key};
use crate::widget::modal_frame::ModalAction;
use crate::widget::qr::QrWidget;

const TITLE: &str = "Pair another device";
const QR_PLACEHOLDER: &str = "The QR code appears once pairing begins";
const CODE_UNSET: &str = "------";
const CANCEL_LABEL: &str = "Cancel";

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

/// Every rect the screen paints and hit-tests, from one pass over the rows.
#[derive(Clone, Debug, Default)]
struct Plan {
    qr: Rect,
    code: Rect,
    status: Rect,
    bottom: f64,
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
    /// The theme `build_layout` saw, so `apply_layout` can resolve the rows
    /// without waiting for a paint.
    layout_theme: Option<Theme>,
    scroll: BodyScroll,
    body_height: f64,
    /// Keep the DKG outcome until the mailbox acknowledges its final reply.
    pending_completion: Option<(HostDkgOutcome, EnrolmentRound2, [u8; 64])>,
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
            layout_theme: None,
            scroll: BodyScroll::default(),
            body_height: 0.0,
            pending_completion: None,
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
        UnwrapErr(SysRng).fill_bytes(&mut nonce);
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
        self.pending_completion = None;
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
    ///
    /// # Errors
    ///
    /// Returns an error if pairing has not begun, the broker endpoint or
    /// certificate is invalid, or the refreshed QR payload cannot be encoded.
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
        let mut rng = UnwrapErr(SysRng);
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
        if self.pending_completion.is_none()
            && let Some((round1, reply, mac)) = hs.take_pairer_exchange(&nonce)
        {
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
                self.pending_completion = Some((outcome, round2, tag));
            } else {
                self.completed_identity = outcome.identity;
                self.status = if outcome.completed {
                    "Device paired.".to_string()
                } else {
                    "Waiting for the new device…".to_string()
                };
                return Ok(outcome.completed);
            }
        }
        if let Some((_, round2, tag)) = self.pending_completion.as_ref() {
            if !hs.publish_host_round2(&nonce, round2.clone(), *tag) {
                self.status = "Waiting for the pairing broker…".to_string();
                return Ok(false);
            }
            let (outcome, _, _) = self.pending_completion.take().ok_or_else(|| {
                IdentityFlowError::DecodeFailed("pending DKG completion disappeared".into())
            })?;
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
    pub fn handle_event_screen(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> ScreenOutcome {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = p;
                let h = self.cancel_rect.contains(p);
                if h != self.cancel_hovered {
                    self.cancel_hovered = h;
                    *ctx.handled = true;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.cancel_rect.contains(self.last_cursor) {
                    *ctx.handled = true;
                    return ScreenOutcome::Back;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.scroll.wheel(delta) {
                    *ctx.handled = true;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.scroll.key(logical_key, self.body_height) {
                    *ctx.handled = true;
                }
                ScreenOutcome::Stay
            }
            _ => ScreenOutcome::Stay,
        }
    }

    fn actions(&self) -> [ModalAction<'static>; 1] {
        [ModalAction::secondary(CANCEL_LABEL).hovered(self.cancel_hovered)]
    }

    /// One geometry pass for both `apply_layout` and `draw`.
    fn resolve(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) -> Resolved<Plan> {
        let actions = self.actions();
        let spec = SheetSpec {
            title: TITLE,
            max_width: FORM_SHEET_WIDTH,
            actions: &actions,
        };
        let mut scroll = self.scroll;
        let resolved = resolve(
            self.rect,
            &spec,
            &mut scroll,
            text_renderer,
            theme,
            |rows, _control_h, tr, theme| {
                let plan = self.plan(rows, tr, theme);
                let bottom = plan.bottom;
                (plan, bottom)
            },
        );
        self.scroll = scroll;
        let body = resolved.sheet.body();
        self.body_height = body.height();
        self.qr_rect = BodyScroll::visible(body, resolved.plan.qr);
        self.cancel_rect = resolved.sheet.action(0);
        resolved
    }

    fn code_text(&self) -> String {
        self.pairing_code_display()
            .map_or_else(|| CODE_UNSET.to_string(), |code| format!("Code {code}"))
    }

    /// Walk the body rows once: a square QR as wide as the body allows,
    /// the code large under it, the status under that — all centred on
    /// the QR, which is what the other device's owner is looking at.
    fn plan(&self, mut rows: Rows, text_renderer: &mut TextRenderer, theme: &Theme) -> Plan {
        let sp = &theme.tokens.spacing;
        let width = rows.width();
        let side = width.min(f64::from(sp.s16) * 4.0);
        let qr_row = rows.row(side);
        let x0 = qr_row.x0 + (qr_row.width() - side) / 2.0;
        let qr = Rect::new(x0, qr_row.y0, x0 + side, qr_row.y1);
        rows.gap(sp.s4);
        let code = rows.row(line_height(text_renderer, theme, Role::Code));
        rows.gap(sp.s2);
        let status = rows.row(paragraph_height(
            text_renderer,
            theme,
            Role::Caption,
            &self.status,
            width,
        ));
        Plan {
            qr,
            code,
            status,
            bottom: rows.bottom(),
        }
    }
}

impl Widget for AddDeviceHostScreen {
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
            max_width: FORM_SHEET_WIDTH,
            actions: &actions,
        };
        let sheet = &resolved.sheet;
        let plan = &resolved.plan;

        sheet.paint_frame(scene, transform, text_renderer, theme, &spec);
        sheet.clip_body(scene, transform);
        if let Some(qr) = &self.qr {
            paint_qr(scene, transform, text_renderer, theme, qr, plan.qr);
        } else {
            paint_cell(scene, transform, theme, plan.qr, false);
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                plan.qr,
                QR_PLACEHOLDER,
                Role::Caption,
                Align::Center,
                true,
            );
        }
        paint_text(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            plan.code,
            &self.code_text(),
            Role::Code,
            Align::Center,
            false,
        );
        paint_text(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            plan.status,
            &self.status,
            Role::Caption,
            Align::Center,
            true,
        );
        scene.pop_layer();
        sheet.paint_actions(scene, transform, text_renderer, theme, &spec);
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
    use threshold_identity::ThresholdIdentity;

    fn fixture_host() -> (AddDeviceHostScreen, [u8; 16]) {
        let mut rng = UnwrapErr(SysRng);
        let mut id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        let (sess, r1) = id.begin_enrolment(1, 2, 2, &mut rng).unwrap();
        let mut s = AddDeviceHostScreen::new();
        s.begin(r1, sess, *id.group_public_key()).unwrap();
        let nonce = s.handshake().unwrap().nonce;
        (s, nonce)
    }

    #[test]
    fn host_begin_publishes_qr_payload_code_and_layout() {
        fn begin_populates_qr_payload_code_and_nonce() {
            let (s, nonce) = fixture_host();
            assert!(s.qr.is_some());
            let code = s.pairing_code_display().unwrap();
            assert_eq!(code.len(), 6);
            assert_eq!(derive_pairing_code(&nonce), s.handshake().unwrap().code);
        }

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

        fn begin_without_handshake_returns_no_code() {
            let s = AddDeviceHostScreen::new();
            assert!(s.pairing_code_display().is_none());
            assert!(s.payload_json().is_none());
        }

        /// The QR is square and centred; the code and status stack under it on
        /// the spacing scale.
        fn plan_centres_a_square_qr_with_code_and_status_beneath() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let body = Rect::new(100.0, 200.0, 620.0, 200.0);
            let (s, _) = fixture_host();
            let plan = s.plan(Rows::new(body), &mut tr, &theme);
            let sp = &theme.tokens.spacing;
            assert!((plan.qr.width() - plan.qr.height()).abs() < 1e-9);
            assert!((plan.qr.center().x - body.center().x).abs() < 1e-9);
            assert!(plan.qr.width() <= body.width());
            assert!((plan.code.y0 - plan.qr.y1 - f64::from(sp.s4)).abs() < 1e-9);
            assert!((plan.status.y0 - plan.code.y1 - f64::from(sp.s2)).abs() < 1e-9);
            assert!((plan.bottom - plan.status.y1).abs() < 1e-9);
        }

        begin_populates_qr_payload_code_and_nonce();
        payload_round_trips_to_same_code();
        begin_without_handshake_returns_no_code();
        plan_centres_a_square_qr_with_code_and_status_beneath();
    }

    #[test]
    fn host_handshake_verifies_replies_and_runs_both_dkg_rounds() {
        fn pairer_reply_with_wrong_mac_is_rejected() {
            let (mut s, nonce) = fixture_host();
            let mut rng = UnwrapErr(SysRng);
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

        fn poll_handshake_returns_false_when_no_reply() {
            let (mut s, _) = fixture_host();
            let hs = PairingHandshake::new();
            assert!(!s.poll_handshake(&hs).unwrap());
        }

        fn broker_handshake_exchanges_both_real_dkg_rounds() {
            let (mut host, _) = fixture_host();
            let payload = host.handshake().unwrap().payload.clone();
            let channel = PairingHandshake::new();
            let pairer_channel = channel.clone();
            let pairer = std::thread::spawn(move || {
                let mut screen = crate::widget::identity_flow::pair_device::PairDeviceScreen::new();
                screen.perform_pairing(&payload, &pairer_channel).unwrap()
            });
            tilo_test_env::wait_until("the host's pairing handshake", || {
                host.poll_handshake(&channel).unwrap()
            });
            let host_identity = host.take_completed_identity().unwrap();
            let pairer_identity = pairer.join().unwrap();
            assert_eq!(
                host_identity.group_public_key(),
                pairer_identity.group_public_key()
            );
            assert!(!host_identity.is_single_device());
            assert!(!pairer_identity.is_single_device());
        }

        fn host_retries_contended_round2_publication_without_regenerating_identity() {
            let (mut host, nonce) = fixture_host();
            let payload = host.handshake().unwrap().payload.clone();
            let mut rng = UnwrapErr(SysRng);
            let mut pairer = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
            let (mut pairer_session, pairer_round1) =
                pairer.begin_enrolment(2, 2, 2, &mut rng).unwrap();
            let outgoing = pairer_session
                .receive_round1(&[payload.initiator_round1().unwrap()])
                .unwrap();
            let reply = outgoing.get(&1).unwrap().clone();
            let mac = super::super::pairing_transcript_mac(
                &payload,
                "pairer-to-host",
                &pairer_round1,
                &reply,
            )
            .unwrap();
            let outcome = host
                .host_complete_dkg(pairer_round1.clone(), reply, mac)
                .unwrap();
            let expected_key = *outcome.identity.as_ref().unwrap().group_public_key();
            let round2 = outcome.round2_to_pairer.as_ref().unwrap().clone();
            let tag = super::super::pairing_transcript_mac(
                &payload,
                "host-to-pairer",
                &pairer_round1,
                &round2,
            )
            .unwrap();
            // Pause at the production publication boundary, after the one-shot
            // DKG session has been consumed, with a real competing mailbox lock.
            host.pending_completion = Some((outcome, round2, tag));
            let temp = tempfile::tempdir().unwrap();
            let channel = PairingHandshake::durable(
                temp.path(),
                crate::time::wall() + crate::time::Duration::from_secs(60),
            )
            .unwrap();
            let lock_path = temp.path().join(format!("{}.lock", hex::encode(nonce)));
            let lock = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
                .unwrap();
            assert!(!host.poll_handshake(&channel).unwrap());
            assert!(host.take_completed_identity().is_none());
            assert!(host.pending_completion.is_some());
            assert!(host.handshake().unwrap().session.is_none());
            drop(lock);
            std::fs::remove_file(lock_path).unwrap();

            assert!(host.poll_handshake(&channel).unwrap());
            let identity = host.take_completed_identity().unwrap();
            assert_eq!(*identity.group_public_key(), expected_key);
            let (delivered, delivered_tag) = channel.take_host_round2(&nonce).unwrap();
            assert_eq!(delivered_tag, tag);
            pairer_session.receive_round2(&[delivered]).unwrap();
            pairer.adopt_dkg_outcome(pairer_session).unwrap();
            assert_eq!(identity.group_public_key(), pairer.group_public_key());
            assert!(host.pending_completion.is_none());
            assert!(!host.poll_handshake(&channel).unwrap());
            assert!(channel.take_host_round2(&nonce).is_none());
        }

        pairer_reply_with_wrong_mac_is_rejected();
        poll_handshake_returns_false_when_no_reply();
        broker_handshake_exchanges_both_real_dkg_rounds();
        host_retries_contended_round2_publication_without_regenerating_identity();
    }
}
