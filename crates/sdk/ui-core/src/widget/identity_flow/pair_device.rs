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
//!
//! Both inputs are the shared [`TextInput`], embedded: six one-digit cells
//! (digits only, each in its own `paint_cell` frame) and the multi-line
//! payload field. The screen forwards them the events it does not claim,
//! and every one hangs off the screen's layout node as a child of its own,
//! so focus traversal, the accessibility tree and the headless driver
//! reach them by name. Code entry keeps its shape: a digit moves on to the
//! next cell, a paste spreads across the cells, Backspace in an empty cell
//! steps back and clears the one before it.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use getrandom::{SysRng, rand_core::UnwrapErr};
use taffy::{NodeId, TaffyTree};
use threshold_identity::{DkgSession, EnrolmentRound1, ThresholdIdentity};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::sheet::{
    Align, BodyScroll, FORM_SHEET_WIDTH, Resolved, Role, Rows, SheetSpec, code_cell_side, edited,
    field_leaf, line_height, paint_cell, paint_text, paragraph_height, place_code_cell,
    place_field, resolve, screen_style, with_layout_renderer,
};
use crate::widget::identity_flow::{
    IdentityFlowError, PairingHandshake, PairingPayload, ScreenOutcome, derive_pairing_code,
    format_pairing_code,
};
use crate::widget::modal_dialog::ModalAction;
use crate::widget::text_input::TextInput;

/// The pairing code is exactly six digits, one per input box.
const CODE_LEN: usize = 6;
const MAX_PAIRING_PAYLOAD_TEXT_BYTES: usize = 256 * 1024;
const PAYLOAD_PLACEHOLDER: &str = "Paste the pairing payload from your other device";
const PAYLOAD_TOO_LARGE: &str = "Pairing payload is larger than 256 KiB.";

const TITLE: &str = "Pair with existing identity";
const CODE_LABEL: &str = "Pairing code";
const PAYLOAD_LABEL: &str = "Pairing payload";
const PAIR_LABEL: &str = "Pair";
const CONTINUE_LABEL: &str = "Continue";
/// Lines of payload the field shows before the text scrolls out of view.
const PAYLOAD_LINES: f64 = 4.0;

/// `test_id`s of the six digit cells, in code order.
pub const CODE_CELL_IDS: [&str; CODE_LEN] = [
    "identity_pair_code_0",
    "identity_pair_code_1",
    "identity_pair_code_2",
    "identity_pair_code_3",
    "identity_pair_code_4",
    "identity_pair_code_5",
];
/// `test_id` of the pairing-payload field.
pub const PAYLOAD_INPUT_ID: &str = "identity_pair_payload";

/// One shared input per digit: digits only, addressable by its cell's
/// `test_id`, named by its place in the code for a screen reader.
fn digit_cells() -> [TextInput; CODE_LEN] {
    std::array::from_fn(|i| {
        TextInput::local()
            .with_id(
                CODE_CELL_IDS
                    .get(i)
                    .copied()
                    .unwrap_or("identity_pair_code"),
            )
            .with_accessible_name(format!("Pairing code digit {}", i.saturating_add(1)))
            .with_char_filter(|c: char| c.is_ascii_digit())
    })
}

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
    /// The six digit cells, one shared text input each. Read them with
    /// [`Self::code_string`] / [`Self::parse_code`].
    cells: [TextInput; CODE_LEN],
    /// Each cell's own layout node, minted by `build_layout`.
    cell_nodes: [Option<NodeId>; CODE_LEN],
    /// Each cell's digit as last seen, so an edit is a change of text and a
    /// caret or selection move is not (see [`edited`]).
    last_digits: [String; CODE_LEN],
    code_box_rects: [Rect; CODE_LEN],
    /// Pasted JSON payload from the QR (textarea fallback): the shared
    /// multi-line text input.
    payload: TextInput,
    payload_node: Option<NodeId>,
    /// The last payload text that fit the size cap; an edit that outgrows
    /// the cap rolls back to it.
    payload_ok: String,
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
        // The screen opens on the code: the first cell starts focused so the
        // digits read off the other device land without a click, as the
        // first code box did before the cells were shared inputs.
        let mut cells = digit_cells();
        if let Some(first) = cells.first_mut() {
            first.set_focus(true);
        }
        Self {
            rect: Rect::ZERO,
            cells,
            cell_nodes: [None; CODE_LEN],
            last_digits: [const { String::new() }; CODE_LEN],
            code_box_rects: [Rect::ZERO; CODE_LEN],
            payload: TextInput::local()
                .multiline(true)
                .with_id(PAYLOAD_INPUT_ID)
                .with_placeholder(PAYLOAD_PLACEHOLDER)
                .with_accessible_name(PAYLOAD_LABEL),
            payload_node: None,
            payload_ok: String::new(),
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
        let mut digits = code.chars().filter(char::is_ascii_digit);
        for (cell, last) in self.cells.iter_mut().zip(self.last_digits.iter_mut()) {
            let s = digits.next().map(|d| d.to_string()).unwrap_or_default();
            cell.set_text(&s);
            cell.set_cursor_pos(s.len());
            *last = s;
        }
    }

    /// Current code as a 6-character string; `"------"` when unset.
    pub fn code_string(&self) -> String {
        self.cells
            .iter()
            .map(|cell| cell.value().chars().next().unwrap_or('-'))
            .collect()
    }

    /// Parse the entered code to a u32; `None` if any slot is still empty.
    pub fn parse_code(&self) -> Option<u32> {
        let mut acc: u32 = 0;
        for cell in &self.cells {
            let digit = cell.value().chars().next()?.to_digit(10)?;
            acc = acc.saturating_mul(10).saturating_add(digit);
        }
        Some(acc)
    }

    pub fn set_payload_text(&mut self, payload: impl Into<String>) {
        let payload = payload.into();
        if payload.len() > MAX_PAIRING_PAYLOAD_TEXT_BYTES {
            self.error = Some(PAYLOAD_TOO_LARGE.to_string());
            return;
        }
        self.payload.set_text(&payload);
        self.payload.set_cursor_pos(payload.len());
        self.payload_ok = payload;
        self.error = None;
    }

    pub fn payload_text(&self) -> &str {
        self.payload.value()
    }

    /// The cell whose field holds keyboard focus, if any.
    fn focused_cell(&self) -> Option<usize> {
        self.cells.iter().position(Widget::is_focused)
    }

    /// Move focus to cell `index`: it takes focus, every other field drops
    /// it, and the host learns which node now holds it.
    fn focus_cell(&mut self, index: usize, focus_target: &mut Option<NodeId>) {
        for (i, cell) in self.cells.iter_mut().enumerate() {
            cell.set_focus(i == index);
        }
        self.payload.set_focus(false);
        if let Some(node) = self.cell_nodes.get(index).copied().flatten() {
            *focus_target = Some(node);
        }
    }

    /// Move focus to the payload field.
    fn focus_payload(&mut self, focus_target: &mut Option<NodeId>) {
        for cell in &mut self.cells {
            cell.set_focus(false);
        }
        self.payload.set_focus(true);
        if let Some(node) = self.payload_node {
            *focus_target = Some(node);
        }
    }

    /// Hand `event` to every field; each answers for its own node and only
    /// a focused (or hovered) one acts on it.
    fn forward_all(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        for i in 0..CODE_LEN {
            let node = self
                .cell_nodes
                .get(i)
                .copied()
                .flatten()
                .unwrap_or(ctx.node_id);
            if let Some(cell) = self.cells.get_mut(i) {
                cell.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            }
        }
        let node = self.payload_node.unwrap_or(ctx.node_id);
        self.payload
            .handle_event(event, &mut ctx.reborrow(ctx.path, node));
    }

    /// After any input. Digits that landed in a cell — one typed, or
    /// several pasted or set through an accessibility action — spread
    /// forward from that cell, one per cell. In the event path
    /// (`focus_target` present) focus then moves on to the cell after the
    /// last digit. From the layout pass (`None`) the digits still settle —
    /// that is where an accessibility edit is noticed — but focus is the
    /// host's and is never moved without it. Either way the focused cell
    /// keeps its digit selected, and the payload is held to its size cap.
    fn settle(&mut self, focus_target: Option<&mut Option<NodeId>>) {
        let mut landed: Option<(usize, Vec<char>)> = None;
        for (i, (cell, last)) in self
            .cells
            .iter_mut()
            .zip(self.last_digits.iter_mut())
            .enumerate()
        {
            if edited(cell, last) && landed.is_none() {
                let digits: Vec<char> = cell.value().chars().filter(char::is_ascii_digit).collect();
                if !digits.is_empty() {
                    landed = Some((i, digits));
                }
            }
        }
        if let Some((start, digits)) = landed {
            let mut last = start;
            for (offset, digit) in digits.iter().enumerate() {
                let index = start.saturating_add(offset);
                let (Some(cell), Some(seen)) =
                    (self.cells.get_mut(index), self.last_digits.get_mut(index))
                else {
                    break;
                };
                let s = digit.to_string();
                cell.set_text(&s);
                cell.set_cursor_pos(s.len());
                *seen = s;
                last = index;
            }
            if let Some(target) = focus_target {
                let next = last.saturating_add(1);
                if next < CODE_LEN {
                    self.focus_cell(next, target);
                }
            }
        }
        // The focused cell keeps its digit selected — whichever door focused
        // it (a click, the host's Tab, a spread) and wherever a caret move
        // put the caret — so the next keystroke replaces the digit rather
        // than appending a second one. Selecting is not a focus move, so
        // this holds from the layout pass as well.
        if let Some(cell) = self.focused_cell().and_then(|i| self.cells.get_mut(i))
            && !cell.value().is_empty()
        {
            cell.select_all();
        }
        // The payload: an edit over the cap rolls back to the last text
        // that fit; the length check is cheap, the compare only runs when
        // the lengths agree.
        if self.payload.value().len() > MAX_PAIRING_PAYLOAD_TEXT_BYTES {
            let keep = self.payload_ok.clone();
            self.payload.set_text(&keep);
            self.payload.set_cursor_pos(keep.len());
            self.error = Some(PAYLOAD_TOO_LARGE.to_string());
        } else if self.payload.value() != self.payload_ok {
            self.payload_ok.clear();
            self.payload_ok.push_str(self.payload.value());
        }
        let _ = self.payload.take_dirty();
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
    ///
    /// Blocks for up to the poll window: call it from a worker, never from a
    /// frame.
    pub fn perform_pairing(
        &mut self,
        payload: &PairingPayload,
        hs: &PairingHandshake,
    ) -> Result<ThresholdIdentity, IdentityFlowError> {
        self.perform_pairing_on_platform(payload, hs, cfg!(not(target_arch = "wasm32")))
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "blocking pairing exchange for callers already on a worker; the broker round-trips are blocking I/O"
    )]
    fn perform_pairing_on_platform(
        &mut self,
        payload: &PairingPayload,
        hs: &PairingHandshake,
        native: bool,
    ) -> Result<ThresholdIdentity, IdentityFlowError> {
        if !native || cfg!(target_arch = "wasm32") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Device pairing unavailable without a browser pairing executor",
            )
            .into());
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (payload, hs);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Device pairing unavailable without a browser pairing executor",
            )
            .into())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let nonce = payload.nonce()?;
            let host_round1 = payload.initiator_round1()?;
            // Build the pairer's own DKG session (share index 2 in a 2-of-2 group).
            let mut rng = UnwrapErr(SysRng);
            let mut my_identity = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
            let (mut my_session, my_round1) = my_identity.begin_enrolment(2, 2, 2, &mut rng)?;
            // Process the host's round-1 → produce our round-2 reply.
            let outgoing = my_session.receive_round1(std::slice::from_ref(&host_round1))?;
            let reply = outgoing
                .into_iter()
                .find(|(idx, _)| *idx == 1)
                .map(|(_, r)| r)
                .ok_or_else(|| {
                    IdentityFlowError::DecodeFailed(
                        "DKG produced no round-2 reply to share 1".into(),
                    )
                })?;
            let mac = super::pairing_transcript_mac(payload, "pairer-to-host", &my_round1, &reply)?;
            // Publication is one acknowledged, idempotent exchange. A transient
            // TLS timeout must not strand round 1 or round 2 while both screens
            // continue to say they are waiting for the other device.
            let publish_deadline = crate::time::now()
                .checked_add(crate::time::Duration::from_secs(15))
                .ok_or_else(|| {
                    IdentityFlowError::DecodeFailed("pairing deadline overflow".into())
                })?;
            loop {
                if hs.publish_pairer_exchange(&nonce, my_round1.clone(), reply.clone(), mac) {
                    break;
                }
                if crate::time::now() >= publish_deadline {
                    return Err(IdentityFlowError::DecodeFailed(
                        "pairing broker did not acknowledge the DKG exchange".into(),
                    ));
                }
                std::thread::sleep(crate::time::Duration::from_millis(20));
            }
            // The pairer also needs the host's round-1 contribution, sent as the
            // host's own round-2 message; bounded-poll the broker mailbox for it.
            let mut host_reply: Option<threshold_identity::EnrolmentRound2> = None;
            let deadline = crate::time::now()
                .checked_add(crate::time::Duration::from_secs(15))
                .ok_or_else(|| {
                    IdentityFlowError::DecodeFailed("pairing deadline overflow".into())
                })?;
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
                if crate::time::now() >= deadline {
                    break;
                }
                std::thread::sleep(crate::time::Duration::from_millis(20));
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
        #[cfg(target_arch = "wasm32")]
        {
            let _ = payload;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Device pairing unavailable without a browser pairing executor",
            )
            .into())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let broker = payload.remote_broker()?;
            self.perform_pairing(payload, &broker)
        }
    }

    /// Companion to `perform_pairing` that *exposes* the round-1 the
    /// pairer would send, for tests that inspect the wire format.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError`] when bootstrapping the device identity or
    /// beginning its enrolment round fails.
    pub fn produce_pairer_round1() -> Result<(DkgSession, EnrolmentRound1), IdentityFlowError> {
        let mut rng = UnwrapErr(SysRng);
        let mut id = ThresholdIdentity::bootstrap_single_device(&mut rng)?;
        Ok(id.begin_enrolment(2, 2, 2, &mut rng)?)
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
        let outcome = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = p;
                let h = self.submit_rect.contains(p);
                if h != self.submit_hovered {
                    self.submit_hovered = h;
                    *ctx.handled = true;
                }
                self.forward_all(event, ctx);
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let outcome = self.handle_mouse_press(ctx.handled, ctx.focus_target);
                self.forward_all(event, ctx);
                outcome
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.scroll.key(logical_key, self.body_height)
                    || self.backspace_steps_back(logical_key, ctx.focus_target)
                {
                    *ctx.handled = true;
                } else {
                    self.forward_all(event, ctx);
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.scroll.wheel(delta) {
                    *ctx.handled = true;
                }
                ScreenOutcome::Stay
            }
            // Key releases, IME preedit/commit, modifiers: the fields'.
            _ => {
                self.forward_all(event, ctx);
                ScreenOutcome::Stay
            }
        };
        self.settle(Some(ctx.focus_target));
        outcome
    }

    /// Backspace in an empty, focused cell steps back to the cell before it
    /// and clears that one — the code-entry idiom the digit cells had before
    /// they were shared inputs. `true` when it did so.
    fn backspace_steps_back(&mut self, key: &Key, focus_target: &mut Option<NodeId>) -> bool {
        if !matches!(key, Key::Named(NamedKey::Backspace)) {
            return false;
        }
        let Some(index) = self.focused_cell() else {
            return false;
        };
        if !self.cells.get(index).is_some_and(|c| c.value().is_empty()) {
            return false;
        }
        let Some(prev) = index.checked_sub(1) else {
            return false;
        };
        if let Some(cell) = self.cells.get_mut(prev) {
            cell.set_text("");
        }
        self.focus_cell(prev, focus_target);
        true
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
        // An accessibility `SetValue` lands outside the event path; the
        // layout pass that follows it is where its digits settle into the
        // cells. Focus is the host's here and is not moved.
        self.settle(None);
        for (cell, rect) in self.cells.iter_mut().zip(resolved.plan.boxes.iter()) {
            place_code_cell(cell, *rect, text_renderer, theme);
        }
        place_field(&mut self.payload, resolved.plan.payload);
        for (slot, rect) in self
            .code_box_rects
            .iter_mut()
            .zip(resolved.plan.boxes.iter())
        {
            *slot = BodyScroll::visible(body, *rect);
        }
        self.payload_rect = BodyScroll::visible(body, resolved.plan.payload);
        self.submit_rect = resolved.sheet.action(0);
        resolved
    }

    /// `MouseInput` arm of [`Self::handle_event_screen`]: focuses a code
    /// cell or the payload field, or triggers submit, whichever rect the
    /// last cursor position landed in. The press is then forwarded to the
    /// fields so the one under the pointer places its caret.
    fn handle_mouse_press(
        &mut self,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
    ) -> ScreenOutcome {
        if let Some(i) = self
            .code_box_rects
            .iter()
            .position(|r| r.contains(self.last_cursor))
        {
            self.focus_cell(i, focus_target);
            *handled = true;
            return ScreenOutcome::Stay;
        }
        if self.payload_rect.contains(self.last_cursor) {
            self.focus_payload(focus_target);
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
            if self.parse_code().is_none() || self.payload.value().is_empty() {
                self.error = Some("Enter both the code and pairing payload.".to_string());
            }
            return ScreenOutcome::Stay;
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
        // Six squares (`s12`, grown to fit the `Code` glyph at a large text
        // scale), `s2` apart, starting on the label's edge so the code reads
        // from the same margin as everything above it.
        let side = code_cell_side(text_renderer, theme);
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

    /// The six digit cells: the field frame, focused or not, with the
    /// shared input seated in it by `resolve` (plain chrome, `Code` role,
    /// centred on the cell).
    fn draw_code_boxes(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        plan: &Plan,
    ) {
        for (cell, r) in self.cells.iter_mut().zip(plan.boxes.iter()) {
            paint_cell(scene, transform, theme, *r, cell.is_focused());
            cell.draw(scene, transform, text_renderer, theme, presence);
        }
    }
}

impl Widget for PairDeviceScreen {
    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = Some(theme.clone());
        // One child node per field — the six cells in code order, then the
        // payload — placed by the sheet, not by taffy (`Display::None`), but
        // present so focus and accessibility walks line up with `children()`.
        let mut leaves = Vec::with_capacity(CODE_LEN.saturating_add(1));
        for i in 0..CODE_LEN {
            let leaf = field_leaf(taffy);
            if let Some(cell) = self.cells.get_mut(i) {
                node_map.insert(leaf, std::ptr::from_mut::<dyn Widget>(cell));
            }
            if let Some(node) = self.cell_nodes.get_mut(i) {
                *node = Some(leaf);
            }
            leaves.push(leaf);
        }
        let payload = field_leaf(taffy);
        node_map.insert(payload, std::ptr::from_mut::<dyn Widget>(&mut self.payload));
        self.payload_node = Some(payload);
        leaves.push(payload);
        taffy.widget_node(screen_style(), &leaves, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
        presence: &PresenceCtx,
    ) {
        // Identity ceremonies, invites and joins are security surfaces:
        // every look-alike letter is set apart.
        text_renderer.security_surface(|text_renderer| {
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
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                plan.status,
                &self.status,
                Role::Body,
                Align::Start,
                true,
            );
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                plan.code_label,
                CODE_LABEL,
                Role::Label,
                Align::Start,
                false,
            );
            self.draw_code_boxes(scene, transform, text_renderer, theme, presence, plan);
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                plan.payload_label,
                PAYLOAD_LABEL,
                Role::Label,
                Align::Start,
                false,
            );
            self.payload
                .draw(scene, transform, text_renderer, theme, presence);
            if let (Some(rect), Some(err)) = (plan.error, self.error.as_deref()) {
                paint_text(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    rect,
                    err,
                    Role::Error,
                    Align::Start,
                    true,
                );
            }
            scene.pop_layer();
            sheet.paint_actions(scene, transform, text_renderer, theme, &spec);
        });
    }

    fn name(&self) -> &'static str {
        "PairDeviceScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_pair_device")
    }

    /// The six cells then the payload — the order `build_layout` minted
    /// their nodes in, which every focus and accessibility walk zips against.
    fn children(&self) -> Vec<&dyn Widget> {
        self.cells
            .iter()
            .map(|c| c as &dyn Widget)
            .chain(std::iter::once(&self.payload as &dyn Widget))
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.cells
            .iter_mut()
            .map(|c| c as &mut (dyn Widget + '_))
            .chain(std::iter::once(&mut self.payload as &mut (dyn Widget + '_)))
            .collect()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.payload.is_focused() {
            return Widget::ime_cursor_rect(&self.payload);
        }
        let cell = self.cells.get(self.focused_cell()?)?;
        Widget::ime_cursor_rect(cell)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if self.payload.is_focused() {
            return Widget::ime_purpose(&self.payload);
        }
        let cell = self.cells.get(self.focused_cell()?)?;
        Widget::ime_purpose(cell)
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
    use automerge::AutoCommit;

    #[test]
    fn pair_code_parsing_and_payload_validation() {
        fn set_code_handles_partial_input() {
            let mut s = PairDeviceScreen::new();
            s.set_code("123");
            assert_eq!(s.code_string(), "123---");
            s.set_code("987654");
            assert_eq!(s.code_string(), "987654");
        }

        fn parse_code_requires_six_digits() {
            let mut s = PairDeviceScreen::new();
            assert!(s.parse_code().is_none());
            s.set_code("12345");
            assert!(s.parse_code().is_none());
            s.set_code("000042");
            assert_eq!(s.parse_code(), Some(42));
        }

        fn validate_code_against_payload_round_trip() {
            let mut rng = UnwrapErr(SysRng);
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

        set_code_handles_partial_input();
        parse_code_requires_six_digits();
        validate_code_against_payload_round_trip();
    }

    #[test]
    fn pair_code_entry_input_and_layout() {
        fn manual_payload_copy_is_the_only_advertised_input() {
            assert!(PAYLOAD_PLACEHOLDER.contains("Paste the pairing payload"));
            assert!(!PAYLOAD_PLACEHOLDER.contains("scanner"));
            assert!(!PAYLOAD_PLACEHOLDER.contains("TODO"));
        }

        /// Code entry through the shared inputs keeps its shape: a digit moves
        /// on to the next cell, a multi-digit edit (a paste, an accessibility
        /// `SetValue`) spreads across the cells, Backspace in an empty cell
        /// steps back and clears the one before it, and typing into a focused
        /// filled cell replaces its digit.
        fn typing_advances_a_paste_spreads_and_backspace_steps_back() {
            use crate::widget::identity_flow::test_events as ev;
            fn send(
                s: &mut PairDeviceScreen,
                event: &WindowEvent,
                doc: &mut AutoCommit,
                taffy: &TaffyTree<()>,
                node: NodeId,
            ) {
                let mut handled = false;
                let mut target = None;
                s.handle_event_screen(
                    event,
                    &mut crate::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &winit::event::Modifiers::default(),
                        state: crate::widget::Edits::lend(doc),
                        path: &automerge::ROOT,
                        handled: &mut handled,
                        focus_target: &mut target,
                        node_id: node,
                        taffy,
                    },
                );
            }
            let (taffy, node) = ev::scratch_node();
            let mut doc = AutoCommit::new();
            let mut s = PairDeviceScreen::new();
            assert_eq!(
                s.focused_cell(),
                Some(0),
                "the screen opens with the first cell focused"
            );

            send(&mut s, &ev::char("9"), &mut doc, &taffy, node);
            assert_eq!(s.code_string(), "9-----");
            assert_eq!(
                s.focused_cell(),
                Some(1),
                "a digit moves on to the next cell"
            );

            send(&mut s, &ev::char("x"), &mut doc, &taffy, node);
            assert_eq!(s.code_string(), "9-----", "a cell admits digits only");
            assert_eq!(s.focused_cell(), Some(1));

            // An accessibility SetValue lands as one edit in a cell and is
            // noticed by the layout pass, which spreads the digits forward but
            // leaves focus where the host put it.
            Widget::perform_action(
                &mut s.cells[1],
                &crate::a11y::Action::SetValue("2468".into()),
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            );
            s.settle(None);
            assert_eq!(s.code_string(), "92468-");
            assert_eq!(
                s.focused_cell(),
                Some(1),
                "the layout pass never moves focus"
            );

            // A selection change is not an edit: nothing spreads, nothing moves.
            Widget::perform_action(
                &mut s.cells[2],
                &crate::a11y::Action::SetSelection {
                    anchor: 0,
                    focus: 1,
                },
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            );
            send(
                &mut s,
                &ev::cursor_at(Point::new(5.0, 5.0)),
                &mut doc,
                &taffy,
                node,
            );
            assert_eq!(s.code_string(), "92468-");
            assert_eq!(s.focused_cell(), Some(1), "a SetSelection is not an edit");

            // Moving the caret inside a filled cell and typing still replaces
            // its digit rather than appending a second one.
            send(
                &mut s,
                &ev::named(NamedKey::ArrowRight),
                &mut doc,
                &taffy,
                node,
            );
            send(&mut s, &ev::named(NamedKey::End), &mut doc, &taffy, node);
            send(&mut s, &ev::char("5"), &mut doc, &taffy, node);
            assert_eq!(
                s.code_string(),
                "95468-",
                "typing after a caret move replaces the cell's digit"
            );
            assert_eq!(s.focused_cell(), Some(2));

            // Focus handed to a filled cell by the host (Tab/Shift+Tab move
            // focus without an event reaching the screen): the layout pass
            // selects the digit, so the next keystroke replaces it too.
            for (i, cell) in s.cells.iter_mut().enumerate() {
                cell.set_focus(i == 3);
            }
            s.settle(None);
            send(&mut s, &ev::char("0"), &mut doc, &taffy, node);
            assert_eq!(
                s.code_string(),
                "95408-",
                "a digit typed into a host-focused filled cell replaces it"
            );
            assert_eq!(s.focused_cell(), Some(4));

            s.focus_cell(5, &mut None);
            send(
                &mut s,
                &ev::named(NamedKey::Backspace),
                &mut doc,
                &taffy,
                node,
            );
            assert_eq!(
                s.code_string(),
                "9540--",
                "backspace in an empty cell clears the one before it"
            );
            assert_eq!(s.focused_cell(), Some(4));

            send(&mut s, &ev::char("7"), &mut doc, &taffy, node);
            send(&mut s, &ev::char("1"), &mut doc, &taffy, node);
            assert_eq!(s.code_string(), "954071");
            assert_eq!(s.parse_code(), Some(954_071));
            assert_eq!(s.focused_cell(), Some(5), "the last cell keeps focus");
            send(&mut s, &ev::char("3"), &mut doc, &taffy, node);
            assert_eq!(
                s.code_string(),
                "954073",
                "typing into a focused, filled cell replaces its digit"
            );

            let ids: Vec<&str> = s.children().iter().filter_map(|c| c.test_id()).collect();
            assert_eq!(ids.len(), CODE_LEN + 1);
            assert_eq!(ids[0], CODE_CELL_IDS[0]);
            assert_eq!(ids[CODE_LEN], PAYLOAD_INPUT_ID);
        }

        /// Six square boxes on the spacing scale, starting on the body's edge
        /// under their label; the payload field spans the body and is several
        /// lines tall.
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

        manual_payload_copy_is_the_only_advertised_input();
        typing_advances_a_paste_spreads_and_backspace_steps_back();
        plan_lays_six_square_code_boxes_under_their_label();
    }

    #[test]
    fn portable_review_pairing_is_unavailable_before_decode_or_retry() {
        let payload = PairingPayload {
            ty: String::new(),
            channel: String::new(),
            initiator_pk: String::new(),
            initiator_dkg_round1: String::new(),
            broker_endpoint: String::new(),
            broker_certificate: String::new(),
        };
        let result = PairDeviceScreen::new().perform_pairing_on_platform(
            &payload,
            &PairingHandshake::new(),
            false,
        );
        assert!(
            matches!(result, Err(IdentityFlowError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported)
        );
    }
}
