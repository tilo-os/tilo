//! Screen 4 — pair this device with an existing identity.
//!
//! Counterpart of screen 3. Shows a 6-box code input and a manual
//! pairing-payload field. The existing device provides both values; this
//! device accepts a copied payload without pretending a camera scanner exists.
//!
//! On submit: verify the code matches the payload's derived short code
//! (defence against typing the wrong code into the right payload), decode
//! the payload and build this device's DKG round-1/round-2 contributions,
//! publish the round-2 reply with an HMAC, then wait for the host's own
//! round-2 reply and adopt the DKG outcome.
//!
//! Shell integration tests exercise the complete state machine through an
//! independently listening pinned-TLS broker; narrow widget tests can still
//! inject the in-memory backend.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use rand_core::OsRng;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use threshold_identity::{DkgSession, EnrolmentRound1, EnrolmentRound2, ThresholdIdentity};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey, SmolStr};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::sheet::{
    Align, BodyScroll, FORM_SHEET_WIDTH, Field, Resolved, Role, Rows, SheetSpec, line_height,
    paint_cell, paint_field, paint_text, paragraph_height, resolve, with_layout_renderer,
};
use crate::widget::identity_flow::{
    IdentityFlowError, PairingHandshake, PairingPayload, ScreenOutcome, derive_pairing_code,
    format_pairing_code,
};
use crate::widget::modal_frame::ModalAction;

/// The pairing code is exactly six digits, one per input box.
const CODE_LEN: usize = 6;
const MAX_PAIRING_PAYLOAD_TEXT_BYTES: usize = 256 * 1024;
const PAYLOAD_PLACEHOLDER: &str = "Paste the pairing payload from your other device";

const TITLE: &str = "Pair with existing identity";
const CODE_LABEL: &str = "Pairing code";
const PAYLOAD_LABEL: &str = "Pairing payload";
const PAIR_LABEL: &str = "Pair";
const CONTINUE_LABEL: &str = "Continue";
/// Lines of payload the field shows before the text scrolls out of view.
const PAYLOAD_LINES: f64 = 4.0;

/// Widens a code-box index to `f64` for pixel-space arithmetic. The row
/// holds exactly [`CODE_LEN`] boxes, so this is lossless; going through
/// `u32` states that to the compiler instead of an `as` cast.
fn box_index_to_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Every rect the screen paints and hit-tests, from one pass over the rows.
#[derive(Clone, Debug, Default)]
struct Plan {
    status: Rect,
    code_label: Rect,
    boxes: [Rect; CODE_LEN],
    payload_label: Rect,
    payload: Rect,
    error: Option<Rect>,
    bottom: f64,
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
    /// The theme `build_layout` saw, so `apply_layout` can resolve the rows
    /// without waiting for a paint.
    layout_theme: Option<Theme>,
    scroll: BodyScroll,
    body_height: f64,
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
            layout_theme: None,
            scroll: BodyScroll::default(),
            body_height: 0.0,
        }
    }

    /// Inject the 6-digit code (used by tests and the manual pairing flow).
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
        let payload = payload.into();
        if payload.len() > MAX_PAIRING_PAYLOAD_TEXT_BYTES {
            self.error = Some("Pairing payload is larger than 256 KiB.".to_string());
            return;
        }
        self.payload_text = payload;
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
        let host_round1 = payload.initiator_round1()?;
        // Build the pairer's own DKG session (share index 2 in a 2-of-2 group).
        let mut rng = OsRng;
        let mut my_identity = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
        let (mut my_session, my_round1) = my_identity.begin_enrolment(2, 2, 2, &mut rng)?;
        // Process the host's round-1 → produce our round-2 reply.
        let outgoing = my_session.receive_round1(std::slice::from_ref(&host_round1))?;
        let reply = outgoing
            .into_iter()
            .find(|(idx, _)| *idx == 1)
            .map(|(_, r)| r)
            .ok_or_else(|| {
                IdentityFlowError::DecodeFailed("DKG produced no round-2 reply to share 1".into())
            })?;
        let mac = super::pairing_transcript_mac(payload, "pairer-to-host", &my_round1, &reply)?;
        // Publication is one acknowledged, idempotent exchange. A transient
        // TLS timeout must not strand round 1 or round 2 while both screens
        // continue to say they are waiting for the other device.
        let publish_deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(15))
            .ok_or_else(|| IdentityFlowError::DecodeFailed("pairing deadline overflow".into()))?;
        loop {
            if hs.publish_pairer_exchange(&nonce, my_round1.clone(), reply.clone(), mac) {
                break;
            }
            if std::time::Instant::now() >= publish_deadline {
                return Err(IdentityFlowError::DecodeFailed(
                    "pairing broker did not acknowledge the DKG exchange".into(),
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // The pairer also needs the host's round-1 contribution, sent as the
        // host's own round-2 message; bounded-poll the broker mailbox for it.
        let mut host_reply: Option<EnrolmentRound2> = None;
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(15))
            .ok_or_else(|| IdentityFlowError::DecodeFailed("pairing deadline overflow".into()))?;
        loop {
            if let Some((r, host_mac)) = hs.take_host_round2(&nonce) {
                let expected =
                    super::pairing_transcript_mac(payload, "host-to-pairer", &my_round1, &r)?;
                if expected != host_mac {
                    return Err(IdentityFlowError::PairingMacInvalid);
                }
                host_reply = Some(r);
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
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

    /// Connect to the exact pinned broker carried by `payload` and run the
    /// pairer's complete DKG half.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint/pin is invalid or any authenticated
    /// DKG exchange fails or times out.
    pub fn perform_pairing_from_payload(
        &mut self,
        payload: &PairingPayload,
    ) -> Result<ThresholdIdentity, IdentityFlowError> {
        let broker = payload.remote_broker()?;
        self.perform_pairing(payload, &broker)
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
            } => {
                if self.scroll.key(logical_key, self.body_height) {
                    *handled = true;
                    return ScreenOutcome::Stay;
                }
                self.handle_key_press(logical_key, text.as_ref(), handled)
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

    /// One geometry pass for both `apply_layout` and `draw`; hit rects are
    /// clipped to the visible body.
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
            |rows, control_h, tr, theme| {
                let plan = self.plan(rows, control_h, tr, theme);
                let bottom = plan.bottom;
                (plan, bottom)
            },
        );
        self.scroll = scroll;
        let body = resolved.sheet.body();
        self.body_height = body.height();
        for (slot, rect) in self.code_box_rects.iter_mut().zip(resolved.plan.boxes.iter()) {
            *slot = BodyScroll::visible(body, *rect);
        }
        self.payload_rect = BodyScroll::visible(body, resolved.plan.payload);
        self.submit_rect = resolved.sheet.action(0);
        resolved
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
                self.error = Some("Enter both the code and pairing payload.".to_string());
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
                            if self.payload_text.len().saturating_add(c.len_utf8())
                                <= MAX_PAIRING_PAYLOAD_TEXT_BYTES
                            {
                                self.payload_text.push(c);
                                *handled = true;
                            } else {
                                self.error =
                                    Some("Pairing payload is larger than 256 KiB.".to_string());
                            }
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

    fn actions(&self) -> [ModalAction<'static>; 1] {
        let label = if self.completed {
            CONTINUE_LABEL
        } else {
            PAIR_LABEL
        };
        [ModalAction::primary(label).hovered(self.submit_hovered)]
    }

    /// Walk the body rows once: status, the six digit boxes under their
    /// label, the payload field under its label, then any error.
    fn plan(
        &self,
        mut rows: Rows,
        _control_h: f64,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Plan {
        let sp = &theme.tokens.spacing;
        let width = rows.width();
        let status = rows.row(paragraph_height(
            text_renderer,
            theme,
            Role::Body,
            &self.status,
            width,
        ));
        rows.gap(sp.s6);
        let code_label = rows.row(line_height(text_renderer, theme, Role::Label));
        rows.gap(sp.s1);
        // Six `s12` squares, `s2` apart, starting on the label's edge so
        // the code reads from the same margin as everything above it.
        let side = f64::from(sp.s12);
        let box_row = rows.row(side);
        let mut boxes = [Rect::ZERO; CODE_LEN];
        for (i, slot) in boxes.iter_mut().enumerate() {
            let x = box_row.x0 + box_index_to_f64(i) * (side + f64::from(sp.s2));
            *slot = Rect::new(x, box_row.y0, x + side, box_row.y1);
        }
        rows.gap(sp.s6);
        let payload_label = rows.row(line_height(text_renderer, theme, Role::Label));
        rows.gap(sp.s1);
        let payload_h =
            PAYLOAD_LINES * line_height(text_renderer, theme, Role::Value) + 2.0 * f64::from(sp.s2);
        let payload = rows.row(payload_h);
        let error = self.error.as_deref().map(|err| {
            rows.gap(sp.s3);
            rows.row(paragraph_height(
                text_renderer,
                theme,
                Role::Error,
                err,
                width,
            ))
        });
        Plan {
            status,
            code_label,
            boxes,
            payload_label,
            payload,
            error,
            bottom: rows.bottom(),
        }
    }

    /// The six digit boxes: field cells with their digit centred in the
    /// `Code` role, the focused one wearing the focus edge.
    fn draw_code_boxes(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        plan: &Plan,
    ) {
        for (i, (r, ch)) in plan.boxes.iter().zip(self.code_digits.iter()).enumerate() {
            let focused = self.code_focus == i && !self.payload_focus;
            paint_cell(scene, transform, theme, *r, focused);
            if *ch != '_' {
                paint_text(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    *r,
                    &ch.to_string(),
                    Role::Code,
                    Align::Center,
                    false,
                );
            }
        }
    }
}

impl Widget for PairDeviceScreen {
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
            max_width: FORM_SHEET_WIDTH,
            actions: &actions,
        };
        let sheet = &resolved.sheet;
        let plan = &resolved.plan;

        sheet.paint_frame(scene, transform, text_renderer, theme, &spec);
        sheet.clip_body(scene, transform);
        paint_text(
            scene,
            transform,
            text_renderer,
            theme,
            plan.status,
            &self.status,
            Role::Body,
            Align::Start,
            true,
        );
        paint_text(
            scene,
            transform,
            text_renderer,
            theme,
            plan.code_label,
            CODE_LABEL,
            Role::Label,
            Align::Start,
            false,
        );
        self.draw_code_boxes(scene, transform, text_renderer, theme, plan);
        paint_text(
            scene,
            transform,
            text_renderer,
            theme,
            plan.payload_label,
            PAYLOAD_LABEL,
            Role::Label,
            Align::Start,
            false,
        );
        paint_field(
            scene,
            transform,
            text_renderer,
            theme,
            &Field {
                rect: plan.payload,
                text: &self.payload_text,
                placeholder: PAYLOAD_PLACEHOLDER,
                focused: self.payload_focus,
                editor: None,
                multiline: true,
            },
        );
        if let (Some(rect), Some(err)) = (plan.error, self.error.as_deref()) {
            paint_text(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                err,
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
    fn manual_payload_copy_is_the_only_advertised_input() {
        assert!(PAYLOAD_PLACEHOLDER.contains("Paste the pairing payload"));
        assert!(!PAYLOAD_PLACEHOLDER.contains("scanner"));
        assert!(!PAYLOAD_PLACEHOLDER.contains("TODO"));
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

    /// Six square boxes on the spacing scale, starting on the body's edge
    /// under their label; the payload field spans the body and is several
    /// lines tall.
    #[test]
    fn plan_lays_six_square_code_boxes_under_their_label() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let body = Rect::new(100.0, 200.0, 620.0, 200.0);
        let s = PairDeviceScreen::new();
        let plan = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
        let sp = &theme.tokens.spacing;
        let side = f64::from(sp.s12);
        assert_eq!(plan.boxes[0].x0, plan.code_label.x0);
        for b in &plan.boxes {
            assert_eq!(b.width(), side);
            assert_eq!(b.height(), side);
        }
        for pair in plan.boxes.windows(2) {
            assert!((pair[1].x0 - pair[0].x1 - f64::from(sp.s2)).abs() < 1e-9);
        }
        assert!(plan.boxes[5].x1 <= body.x1, "the row fits the body");
        assert_eq!((plan.payload.x0, plan.payload.x1), (body.x0, body.x1));
        assert!(plan.payload.height() > 3.0 * line_height(&mut tr, &theme, Role::Value));
        assert!((plan.bottom - plan.payload.y1).abs() < 1e-9);
    }
}
