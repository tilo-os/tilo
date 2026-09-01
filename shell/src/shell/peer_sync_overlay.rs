//! Peer invite/connect overlay.
//!
//! This component owns the complete local interaction: prompt state, editing,
//! clipboard actions, painted geometry and input routing. The shell host only
//! performs the network request emitted by [`PeerSyncOverlay::handle_event`]
//! and returns its result through [`PeerSyncOverlay::finish_request`].

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// A transport request emitted by the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSyncRequest {
    IssueInvite,
    IssueInviteFor {
        name: String,
        role: crate::network::invite::InviteRole,
    },
    AcceptInvite(String),
    ListPendingInvitations,
    ResendInvitation([u8; 32]),
    CancelInvitation([u8; 32]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInvitationSummary {
    pub id: [u8; 32],
    pub name: String,
    pub role: crate::network::invite::InviteRole,
    pub expiry_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSyncResponse {
    Message(String),
    PendingInvitations(Vec<PendingInvitationSummary>),
}

impl From<String> for PeerSyncResponse {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

impl From<&str> for PeerSyncResponse {
    fn from(value: &str) -> Self {
        Self::Message(value.to_string())
    }
}

/// State exposed to the compositor's structured test dump.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeerSyncSnapshot {
    pub invite_token: Option<String>,
    pub connect_input: Option<String>,
    pub invite_name: Option<String>,
    pub invite_for: Option<String>,
    pub invite_role: crate::network::invite::InviteRole,
    pub pending_invitations: Vec<PendingInvitationSummary>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Button {
    CreateInvite,
    CycleRole,
    Join,
    Cancel,
    CopyToken,
    PasteToken,
    Resend([u8; 32]),
    CancelPending([u8; 32]),
}

/// Result of routing one input event through the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventOutcome {
    pub consumed: bool,
    pub request: Option<PeerSyncRequest>,
    /// A named invite asks to copy only after minting succeeds.
    pub copy_result: bool,
}

impl EventOutcome {
    fn consumed() -> Self {
        Self {
            consumed: true,
            request: None,
            copy_result: false,
        }
    }

    fn ignored() -> Self {
        Self {
            consumed: false,
            request: None,
            copy_result: false,
        }
    }

    fn request(request: PeerSyncRequest, copy_result: bool) -> Self {
        Self {
            consumed: true,
            request: Some(request),
            copy_result,
        }
    }
}

#[derive(Default)]
pub struct PeerSyncOverlay {
    state: PeerSyncSnapshot,
    rect: Rect,
    button_rects: Vec<(Button, Rect)>,
    pending_offset: usize,
}

impl PeerSyncOverlay {
    pub fn snapshot(&self) -> PeerSyncSnapshot {
        self.state.clone()
    }

    pub fn visible(&self) -> bool {
        self.state.connect_input.is_some()
            || self.state.invite_token.is_some()
            || self.state.invite_name.is_some()
    }

    pub fn prompt_invite(&mut self) {
        self.state.invite_name = Some(String::new());
        self.state.connect_input = None;
        self.state.invite_token = None;
        self.state.invite_for = None;
        self.state.status = None;
        self.pending_offset = 0;
    }

    pub fn prompt_connect(&mut self) {
        self.state.invite_name = None;
        self.state.connect_input = Some(String::new());
        self.state.status = Some("Paste or type the invite token".to_string());
    }

    pub fn issue_invite_request(&self) -> PeerSyncRequest {
        PeerSyncRequest::IssueInvite
    }

    /// Apply the host's transport result. Failed named invites retain the
    /// typed name so they can be retried; successful connects close the input.
    pub fn finish_request(
        &mut self,
        request: &PeerSyncRequest,
        outcome: Result<String, String>,
        copy_result: bool,
    ) {
        let outcome = outcome.map(PeerSyncResponse::Message);
        self.finish_typed_request(request, outcome, copy_result);
    }

    pub fn finish_typed_request(
        &mut self,
        request: &PeerSyncRequest,
        outcome: Result<PeerSyncResponse, String>,
        copy_result: bool,
    ) {
        match (request, outcome) {
            (PeerSyncRequest::IssueInvite, Ok(PeerSyncResponse::Message(token))) => {
                self.finish_invite(token, None, copy_result);
            }
            (
                PeerSyncRequest::IssueInviteFor { name, .. },
                Ok(PeerSyncResponse::Message(token)),
            ) => {
                self.finish_invite(token, Some(name.clone()), copy_result);
            }
            (PeerSyncRequest::IssueInvite | PeerSyncRequest::IssueInviteFor { .. }, Err(error)) => {
                self.state.invite_token = None;
                self.state.invite_for = None;
                self.state.status = Some(format!("Invite failed: {error}"));
            }
            (PeerSyncRequest::AcceptInvite(_), Ok(PeerSyncResponse::Message(room))) => {
                self.state.connect_input = None;
                self.state.status = Some(format!("Joined room {room}"));
            }
            (PeerSyncRequest::AcceptInvite(_), Err(error)) => {
                self.state.status = Some(format!("Refused: {error}"));
            }
            (
                PeerSyncRequest::ListPendingInvitations,
                Ok(PeerSyncResponse::PendingInvitations(rows)),
            ) => self.state.pending_invitations = rows,
            (PeerSyncRequest::ListPendingInvitations, Err(error)) => {
                self.state.status = Some(format!("Pending invites unavailable: {error}"));
            }
            (PeerSyncRequest::ResendInvitation(id), Ok(PeerSyncResponse::Message(token))) => {
                let name = self
                    .state
                    .pending_invitations
                    .iter()
                    .find(|invite| &invite.id == id)
                    .map(|invite| invite.name.clone());
                self.finish_invite(token, name, true);
            }
            (PeerSyncRequest::ResendInvitation(_), Err(error)) => {
                self.state.status = Some(format!("Could not resend: {error}"));
            }
            (PeerSyncRequest::CancelInvitation(_), Ok(PeerSyncResponse::Message(_))) => {
                self.state.status = Some("Invite canceled".to_string());
            }
            (PeerSyncRequest::CancelInvitation(_), Err(error)) => {
                self.state.status = Some(format!("Could not cancel: {error}"));
            }
            (_, Ok(_)) => {
                self.state.status = Some("Unexpected collaboration response".to_string());
            }
        }
    }

    fn finish_invite(&mut self, token: String, invite_for: Option<String>, copy_result: bool) {
        self.state.status = None;
        self.state.invite_token = Some(token);
        self.state.invite_for = invite_for;
        self.state.invite_name = None;
        if copy_result {
            self.copy_invite_token();
        }
    }

    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        cursor: Option<Point>,
    ) -> EventOutcome {
        let connect_open = self.state.connect_input.is_some();
        let composer_open = self.state.invite_name.is_some();
        let mods = modifiers.state();
        let chord = mods.control_key() || mods.super_key();
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => self.handle_key(logical_key, chord, connect_open, composer_open),
            WindowEvent::KeyboardInput { .. } => EventOutcome {
                consumed: connect_open || composer_open,
                request: None,
                copy_result: false,
            },
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                let Some(point) = cursor else {
                    return EventOutcome::ignored();
                };
                if let Some(button) = self.button_rects.iter().find_map(|(button, rect)| {
                    (rect.area() > 0.0 && rect.contains(point)).then_some(*button)
                }) {
                    return self.activate(button);
                }
                EventOutcome {
                    consumed: self.visible() && self.rect.contains(point),
                    request: None,
                    copy_result: false,
                }
            }
            WindowEvent::MouseWheel { delta, .. } if composer_open => {
                let down = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y < 0.0,
                    winit::event::MouseScrollDelta::PixelDelta(point) => point.y < 0.0,
                };
                let max = self.state.pending_invitations.len().saturating_sub(3);
                self.pending_offset = if down {
                    self.pending_offset.saturating_add(1).min(max)
                } else {
                    self.pending_offset.saturating_sub(1)
                };
                EventOutcome::consumed()
            }
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::MouseInput { .. } => EventOutcome {
                consumed: self.visible() && cursor.is_some_and(|p| self.rect.contains(p)),
                request: None,
                copy_result: false,
            },
            _ => EventOutcome::ignored(),
        }
    }

    fn handle_key(
        &mut self,
        key: &Key,
        chord: bool,
        connect_open: bool,
        composer_open: bool,
    ) -> EventOutcome {
        match key {
            Key::Character(s) if chord && s.eq_ignore_ascii_case("c") => {
                self.copy_invite_token();
                EventOutcome::consumed()
            }
            Key::Character(s) if chord && s.eq_ignore_ascii_case("v") => {
                self.paste_invite_token();
                EventOutcome::consumed()
            }
            Key::Named(NamedKey::Escape) if self.visible() => {
                self.dismiss();
                EventOutcome::consumed()
            }
            Key::Named(NamedKey::Enter) if composer_open => self.submit_invite(),
            Key::Named(NamedKey::Tab) if composer_open => {
                self.state.invite_role = self.state.invite_role.next();
                EventOutcome::consumed()
            }
            Key::Named(NamedKey::Enter) if connect_open => self.submit_connect(),
            Key::Named(NamedKey::Backspace) if connect_open || composer_open => {
                self.active_input_mut(connect_open).map(String::pop);
                EventOutcome::consumed()
            }
            Key::Character(s) if (connect_open || composer_open) && !chord => {
                if let Some(input) = self.active_input_mut(connect_open) {
                    input.push_str(s.as_str());
                }
                EventOutcome::consumed()
            }
            Key::Named(NamedKey::Space) if composer_open && !chord => {
                if let Some(input) = self.state.invite_name.as_mut() {
                    input.push(' ');
                }
                EventOutcome::consumed()
            }
            _ if connect_open || composer_open => EventOutcome::consumed(),
            _ => EventOutcome::ignored(),
        }
    }

    fn active_input_mut(&mut self, connect_open: bool) -> Option<&mut String> {
        if connect_open {
            self.state.connect_input.as_mut()
        } else {
            self.state.invite_name.as_mut()
        }
    }

    fn activate(&mut self, button: Button) -> EventOutcome {
        match button {
            Button::CreateInvite => self.submit_invite(),
            Button::CycleRole => {
                self.state.invite_role = self.state.invite_role.next();
                EventOutcome::consumed()
            }
            Button::Join => self.submit_connect(),
            Button::Cancel => {
                self.state.connect_input = None;
                self.state.invite_name = None;
                self.state.status = None;
                EventOutcome::consumed()
            }
            Button::CopyToken => {
                self.copy_invite_token();
                EventOutcome::consumed()
            }
            Button::PasteToken => {
                self.paste_invite_token();
                EventOutcome::consumed()
            }
            Button::Resend(id) => {
                EventOutcome::request(PeerSyncRequest::ResendInvitation(id), false)
            }
            Button::CancelPending(id) => {
                EventOutcome::request(PeerSyncRequest::CancelInvitation(id), false)
            }
        }
    }

    fn submit_invite(&mut self) -> EventOutcome {
        let Some(name) = self.state.invite_name.as_deref() else {
            return EventOutcome::ignored();
        };
        let name = name.trim().to_string();
        if name.is_empty() {
            self.state.status = Some("Say who this invite is for".to_string());
            return EventOutcome::consumed();
        }
        EventOutcome::request(
            PeerSyncRequest::IssueInviteFor {
                name,
                role: self.state.invite_role,
            },
            true,
        )
    }

    fn submit_connect(&self) -> EventOutcome {
        let Some(token) = self.state.connect_input.as_deref() else {
            return EventOutcome::ignored();
        };
        EventOutcome::request(
            PeerSyncRequest::AcceptInvite(token.trim().to_string()),
            false,
        )
    }

    fn copy_invite_token(&mut self) {
        let Some(token) = self.state.invite_token.as_deref() else {
            return;
        };
        ui::text_selection::clipboard_copy(token);
        self.state.status = Some("Copied".to_string());
    }

    fn paste_invite_token(&mut self) {
        if self.state.connect_input.is_none() {
            return;
        }
        let pasted = ui::text_selection::clipboard_paste()
            .map(|s| s.split_whitespace().collect::<String>())
            .filter(|s| !s.is_empty());
        match pasted {
            Some(token) => {
                self.state.connect_input = Some(token);
                self.state.status = Some("Pasted — press Enter to join".to_string());
            }
            None => self.state.status = Some("Nothing on the clipboard to paste".to_string()),
        }
    }

    fn dismiss(&mut self) {
        self.state = PeerSyncSnapshot::default();
    }

    pub fn draw(
        &mut self,
        host_rect: Rect,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.button_rects.clear();
        if !self.visible() {
            self.rect = Rect::ZERO;
            return;
        }
        self.rect = card_rect(host_rect, &self.state);
        let rect = self.rect;
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let card = RoundedRect::from_rect(rect, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &card,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &card,
        );

        let pad = 20.0;
        let mut y = rect.y0 + pad;
        let title_size = tokens.typography.scale.base.max(14.0);
        let body_size = tokens.typography.scale.xs.max(10.0);
        let name_size = tokens.typography.scale.sm.max(12.0);
        let avail = rect.width() - pad * 2.0;
        let title = if self.state.invite_name.is_some() {
            "Invite someone"
        } else if self.state.connect_input.is_some() {
            "Join a workspace"
        } else {
            "Workspace invite"
        };
        text_renderer.draw_text(
            scene,
            title,
            title_size,
            colors.text_primary,
            transform.pre_translate((rect.x0 + pad, y).into()),
            None,
        );
        y += f64::from(title_size) + 14.0;
        y = self.draw_body(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            pad,
            y,
            body_size,
            name_size,
            avail,
        );
        if self.state.invite_name.is_some() && !self.state.pending_invitations.is_empty() {
            y = self.draw_pending_invitations(
                scene,
                transform,
                text_renderer,
                theme,
                rect,
                pad,
                y,
                body_size,
            );
        }
        if let Some(status) = self.state.status.as_deref() {
            let status = text_renderer.elide_to_width(status, body_size, f64_to_f32(avail));
            text_renderer.draw_text(
                scene,
                &status,
                body_size,
                colors.text_primary,
                transform.pre_translate((rect.x0 + pad, y).into()),
                None,
            );
        }
        self.draw_buttons(scene, transform, text_renderer, theme, rect, pad, body_size);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "immediate-mode invite rows need the same compact paint context as the card"
    )]
    fn draw_pending_invitations(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        pad: f64,
        mut y: f64,
        body: f32,
    ) -> f64 {
        let colors = &theme.tokens.colors;
        let total = self.state.pending_invitations.len();
        let end = self.pending_offset.saturating_add(3).min(total);
        let range = if total > 3 {
            format!(
                " · {}–{} of {total} · scroll",
                self.pending_offset.saturating_add(1),
                end
            )
        } else {
            String::new()
        };
        text.draw_text(
            scene,
            &format!("PENDING INVITES{range}"),
            body,
            colors.text_dim,
            transform.pre_translate((rect.x0 + pad, y).into()),
            None,
        );
        y += f64::from(body) + 8.0;
        for invite in self
            .state
            .pending_invitations
            .iter()
            .skip(self.pending_offset)
            .take(3)
        {
            let row = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 42.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surface_white_5,
                None,
                &RoundedRect::from_rect(row, f64::from(theme.tokens.radii.sm)),
            );
            let detail = format!(
                "{} · expires in {}",
                invite.role.label(),
                time_until(invite.expiry_unix_seconds)
            );
            let shown_name = text.elide_to_width(&invite.name, body, 150.0);
            let shown_detail = text.elide_to_width(&detail, body, 190.0);
            text.draw_text(
                scene,
                &shown_name,
                body,
                colors.text_primary,
                transform.pre_translate((row.x0 + 10.0, row.y0 + 6.0).into()),
                None,
            );
            text.draw_text(
                scene,
                &shown_detail,
                body,
                colors.text_dim,
                transform.pre_translate((row.x0 + 10.0, row.y0 + 23.0).into()),
                None,
            );
            let cancel = Rect::new(row.x1 - 62.0, row.y0 + 8.0, row.x1 - 8.0, row.y1 - 8.0);
            let resend = Rect::new(cancel.x0 - 68.0, cancel.y0, cancel.x0 - 6.0, cancel.y1);
            for (label, button, button_rect) in [
                ("Copy", Button::Resend(invite.id), resend),
                ("Cancel", Button::CancelPending(invite.id), cancel),
            ] {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.surface_white_5,
                    None,
                    &RoundedRect::from_rect(button_rect, button_rect.height() / 2.0),
                );
                let (width, height) = text.measure(label, body, None);
                text.draw_text(
                    scene,
                    label,
                    body,
                    colors.text_primary,
                    transform.pre_translate(
                        (
                            button_rect.center().x - f64::from(width) / 2.0,
                            button_rect.center().y - f64::from(height) / 2.0,
                        )
                            .into(),
                    ),
                    None,
                );
                self.button_rects.push((button, button_rect));
            }
            y = row.y1 + 6.0;
        }
        y
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_body(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        pad: f64,
        y: f64,
        body: f32,
        name: f32,
        avail: f64,
    ) -> f64 {
        if let Some(typed) = self.state.invite_name.as_deref() {
            draw_naming(
                scene,
                transform,
                text,
                theme,
                rect,
                pad,
                y,
                body,
                name,
                avail,
                typed,
                self.state.invite_role,
            )
        } else if let Some(token) = self.state.invite_token.as_deref() {
            draw_invite(
                scene,
                transform,
                text,
                theme,
                rect,
                pad,
                y,
                avail,
                token,
                self.state.invite_for.as_deref(),
            )
        } else {
            draw_receiving(
                scene,
                transform,
                text,
                theme,
                rect,
                pad,
                y,
                body,
                name,
                avail,
                self.state.connect_input.as_deref().unwrap_or(""),
            )
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, matching draw_body's own \
                  allow just above in this impl; a context struct would only move the \
                  count rather than shrink it for a single call site"
    )]
    fn draw_buttons(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        pad: f64,
        body: f32,
    ) {
        let buttons: Vec<(String, Button)> = if self.state.invite_name.is_some() {
            vec![
                ("Create invite".into(), Button::CreateInvite),
                (
                    format!("Role: {}", self.state.invite_role.label()),
                    Button::CycleRole,
                ),
                ("Cancel".into(), Button::Cancel),
            ]
        } else if self.state.connect_input.is_some() {
            vec![
                ("Join".into(), Button::Join),
                ("Cancel".into(), Button::Cancel),
                ("Paste".into(), Button::PasteToken),
            ]
        } else {
            vec![("Copy".into(), Button::CopyToken)]
        };
        let colors = &theme.tokens.colors;
        let bh = 30.0;
        let by = rect.y1 - pad - bh;
        let mut bx = rect.x1 - pad;
        for (label, button) in buttons {
            let (tw, th) = text.measure(&label, body, None);
            let bw = f64::from(tw) + 26.0;
            let brect = Rect::new(bx - bw, by, bx, by + bh);
            let primary = matches!(
                button,
                Button::Join | Button::CopyToken | Button::CreateInvite
            );
            scene.fill(
                Fill::NonZero,
                transform,
                if primary {
                    colors.accent[5]
                } else {
                    colors.surface_white_5
                },
                None,
                &RoundedRect::from_rect(brect, bh / 2.0),
            );
            text.draw_text(
                scene,
                &label,
                body,
                colors.text_primary,
                transform.pre_translate(
                    (
                        f64::midpoint(brect.x0, brect.x1) - f64::from(tw) / 2.0,
                        f64::midpoint(brect.y0, brect.y1) - f64::from(th) / 2.0,
                    )
                        .into(),
                ),
                None,
            );
            self.button_rects.push((button, brect));
            bx = brect.x0 - 8.0;
        }
    }

    pub fn button_rects_for_tests(&self) -> Vec<(&'static str, Rect)> {
        self.button_rects
            .iter()
            .map(|(button, rect)| {
                (
                    match button {
                        Button::CreateInvite => "create",
                        Button::CycleRole => "role",
                        Button::Join => "join",
                        Button::Cancel => "cancel",
                        Button::CopyToken => "copy",
                        Button::PasteToken => "paste",
                        Button::Resend(_) => "resend",
                        Button::CancelPending(_) => "cancel-pending",
                    },
                    *rect,
                )
            })
            .collect()
    }

    #[cfg(test)]
    pub fn set_snapshot_for_tests(&mut self, snapshot: PeerSyncSnapshot) {
        self.state = snapshot;
    }
}

fn card_rect(host: Rect, state: &PeerSyncSnapshot) -> Rect {
    let w = (host.width() * 0.42).clamp(400.0, 520.0);
    let h = if state.invite_name.is_some() {
        if state.pending_invitations.is_empty() {
            210.0
        } else {
            390.0
        }
    } else if state.connect_input.is_some() {
        220.0
    } else {
        200.0
    };
    let cx = f64::midpoint(host.x0, host.x1);
    let cy = f64::midpoint(host.y0, host.y1);
    Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
}

fn parse_invite(token: &str) -> Option<crate::network::invite::PeerInviteToken> {
    crate::network::invite::validate_invite_token(token.trim()).ok()
}

fn peer_fingerprint(peer_id: &str) -> String {
    let chars: Vec<char> = peer_id.chars().collect();
    let tail = chars.get(chars.len().saturating_sub(8)..).unwrap_or(&[]);
    if tail.len() <= 4 {
        return tail.iter().collect();
    }
    let (a, b) = tail.split_at(4);
    format!(
        "{} {}",
        a.iter().collect::<String>(),
        b.iter().collect::<String>()
    )
}

fn time_until(expiry: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match expiry.saturating_sub(now) {
        0 => "moments".into(),
        s if s < 120 => format!("{s} seconds"),
        s if s < 5400 => format!("{} minutes", s / 60),
        s if s < 172_800 => format!("{} hours", s / 3600),
        s => format!("{} days", s / 86_400),
    }
}

fn elide_token(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 20 {
        return token.to_string();
    }
    format!(
        "{}…{}",
        chars.get(..10).unwrap_or(&[]).iter().collect::<String>(),
        chars
            .get(chars.len().saturating_sub(6)..)
            .unwrap_or(&[])
            .iter()
            .collect::<String>()
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_naming(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    pad: f64,
    mut y: f64,
    body: f32,
    name: f32,
    avail: f64,
    typed: &str,
    role: crate::network::invite::InviteRole,
) -> f64 {
    let colors = &theme.tokens.colors;
    text.draw_text(
        scene,
        "Who are you inviting?",
        body,
        colors.text_dim,
        transform.pre_translate((rect.x0 + pad, y).into()),
        None,
    );
    y += f64::from(body) + 8.0;
    let field = Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 38.0);
    let rr = RoundedRect::from_rect(field, f64::from(theme.tokens.radii.sm));
    scene.fill(Fill::NonZero, transform, colors.surface_white_5, None, &rr);
    scene.stroke(
        &vello::kurbo::Stroke::new(1.0),
        transform,
        colors.accent[5],
        None,
        &rr,
    );
    let shown = text.elide_to_width(typed, name, f64_to_f32(avail - 40.0));
    let (tw, th) = text.measure(&shown, name, None);
    let ty = f64::midpoint(field.y0, field.y1) - f64::from(th) / 2.0;
    text.draw_text(
        scene,
        if typed.is_empty() {
            "Their name"
        } else {
            &shown
        },
        name,
        if typed.is_empty() {
            colors.text_dim
        } else {
            colors.text_primary
        },
        transform.pre_translate((field.x0 + 12.0, ty).into()),
        None,
    );
    let caret_x = field.x0
        + 12.0
        + if typed.is_empty() {
            0.0
        } else {
            f64::from(tw) + 2.0
        };
    scene.fill(
        Fill::NonZero,
        transform,
        colors.accent[5],
        None,
        &Rect::new(caret_x, field.y0 + 9.0, caret_x + 1.5, field.y1 - 9.0),
    );
    y = field.y1 + 12.0;
    let note = text.elide_to_width(
        &format!(
            "They join as {} · use the Role button or Tab to change.",
            role.label()
        ),
        body,
        f64_to_f32(avail),
    );
    text.draw_text(
        scene,
        &note,
        body,
        colors.text_dim,
        transform.pre_translate((rect.x0 + pad, y).into()),
        None,
    );
    y + f64::from(body) + 10.0
}

#[allow(clippy::too_many_arguments)]
fn draw_invite(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    pad: f64,
    mut y: f64,
    avail: f64,
    token: &str,
    invite_for: Option<&str>,
) -> f64 {
    let parsed = parse_invite(token);
    let who = invite_for.unwrap_or("Anyone with this invite");
    let sub = parsed.as_ref().map_or_else(String::new, |t| {
        format!(
            "{} · expires in {}",
            t.role.label(),
            time_until(t.expiry_unix_seconds)
        )
    });
    y = draw_person(
        scene,
        transform,
        text,
        theme,
        Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 44.0),
        who,
        &sub,
        invite_for.is_some(),
    ) + 12.0;
    if let Some(t) = parsed {
        y = draw_code(
            scene,
            transform,
            text,
            theme,
            Point::new(rect.x0 + pad, y),
            &peer_fingerprint(&t.owner_peer_id),
            avail,
        );
    }
    y
}

#[allow(clippy::too_many_arguments)]
fn draw_receiving(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    pad: f64,
    mut y: f64,
    body: f32,
    name: f32,
    avail: f64,
    typed: &str,
) -> f64 {
    let colors = &theme.tokens.colors;
    if let Some(t) = parse_invite(typed) {
        y = draw_person(
            scene,
            transform,
            text,
            theme,
            Rect::new(rect.x0 + pad, y, rect.x1 - pad, y + 44.0),
            "Invite received",
            &format!(
                "{} access · expires in {}",
                t.role.label(),
                time_until(t.expiry_unix_seconds)
            ),
            false,
        ) + 12.0;
        return draw_code(
            scene,
            transform,
            text,
            theme,
            Point::new(rect.x0 + pad, y),
            &peer_fingerprint(&t.owner_peer_id),
            avail,
        );
    }
    let line = if typed.is_empty() {
        "Paste the invite they sent you.".to_string()
    } else {
        format!(
            "{} · {} characters",
            elide_token(typed),
            typed.chars().count()
        )
    };
    let line = text.elide_to_width(&line, name, f64_to_f32(avail));
    text.draw_text(
        scene,
        &line,
        name,
        if typed.is_empty() {
            colors.text_dim
        } else {
            colors.text_primary
        },
        transform.pre_translate((rect.x0 + pad, y).into()),
        None,
    );
    y += f64::from(name) + 8.0;
    if !typed.is_empty() {
        text.draw_text(
            scene,
            "Not a whole invite yet",
            body,
            colors.text_dim,
            transform.pre_translate((rect.x0 + pad, y).into()),
            None,
        );
        y += f64::from(body) + 8.0;
    }
    y
}

#[allow(clippy::too_many_arguments)]
fn draw_person(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    who: &str,
    sub: &str,
    named: bool,
) -> f64 {
    let colors = &theme.tokens.colors;
    let center = Point::new(rect.x0 + 16.0, rect.y0 + 18.0);
    if named {
        let initial = who
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        ui::widget::Avatar::draw_at(
            scene,
            transform,
            Rect::new(
                center.x - 16.0,
                center.y - 16.0,
                center.x + 16.0,
                center.y + 16.0,
            ),
            &initial,
            colors.accent[5],
            theme,
            text,
        );
    } else {
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0).with_dashes(0.0, [3.0, 3.0]),
            transform,
            colors.border_white_10,
            None,
            &vello::kurbo::Circle::new(center, 16.0),
        );
    }
    let x = center.x + 30.0;
    let ns = theme.tokens.typography.scale.sm.max(12.0);
    let ss = theme.tokens.typography.scale.xs.max(10.0);
    let who = text.elide_to_width(who, ns, f64_to_f32(rect.x1 - x));
    text.draw_text(
        scene,
        &who,
        ns,
        colors.text_primary,
        transform.pre_translate((x, rect.y0 + 2.0).into()),
        None,
    );
    let sub = text.elide_to_width(sub, ss, f64_to_f32(rect.x1 - x));
    text.draw_text(
        scene,
        &sub,
        ss,
        colors.text_dim,
        transform.pre_translate((x, rect.y0 + 2.0 + f64::from(ns) + 6.0).into()),
        None,
    );
    rect.y0 + 2.0 + f64::from(ns) + 6.0 + f64::from(ss)
}

fn draw_code(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    at: Point,
    code: &str,
    avail: f64,
) -> f64 {
    let colors = &theme.tokens.colors;
    let size = theme.tokens.typography.scale.lg.max(15.0);
    let (_, fh) = text.measure(code, size, None);
    let field = Rect::new(at.x, at.y, at.x + avail, at.y + f64::from(fh) + 20.0);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surface_white_5,
        None,
        &RoundedRect::from_rect(field, f64::from(theme.tokens.radii.md)),
    );
    text.draw_text(
        scene,
        code,
        size,
        colors.text_primary,
        transform.pre_translate((field.x0 + 14.0, field.y0 + 10.0).into()),
        None,
    );
    field.y1
}

#[allow(clippy::cast_possible_truncation)]
fn f64_to_f32(v: f64) -> f32 {
    v as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key) -> WindowEvent {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.logical_key = key;
        event.text = None;
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event,
            is_synthetic: false,
        }
    }

    #[test]
    fn named_invite_refuses_blank_and_preserves_name_after_transport_failure() {
        let mut overlay = PeerSyncOverlay::default();
        overlay.prompt_invite();
        let blank = overlay.handle_event(
            &key(Key::Named(NamedKey::Enter)),
            Modifiers::default(),
            None,
        );
        assert!(blank.request.is_none());
        assert_eq!(
            overlay.state.status.as_deref(),
            Some("Say who this invite is for")
        );
        overlay.handle_event(
            &key(Key::Character("Ada".into())),
            Modifiers::default(),
            None,
        );
        let submit = overlay.handle_event(
            &key(Key::Named(NamedKey::Enter)),
            Modifiers::default(),
            None,
        );
        assert_eq!(
            submit.request,
            Some(PeerSyncRequest::IssueInviteFor {
                name: "Ada".into(),
                role: crate::network::invite::InviteRole::Editor,
            })
        );
        overlay.finish_request(
            submit.request.as_ref().unwrap(),
            Err("offline".into()),
            submit.copy_result,
        );
        assert_eq!(overlay.state.invite_name.as_deref(), Some("Ada"));
    }

    #[test]
    fn invite_role_is_keyboard_selectable_and_reaches_the_request() {
        let mut overlay = PeerSyncOverlay::default();
        overlay.prompt_invite();
        overlay.handle_event(
            &key(Key::Character("Lin".into())),
            Modifiers::default(),
            None,
        );
        overlay.handle_event(&key(Key::Named(NamedKey::Tab)), Modifiers::default(), None);
        let submit = overlay.handle_event(
            &key(Key::Named(NamedKey::Enter)),
            Modifiers::default(),
            None,
        );
        assert_eq!(
            submit.request,
            Some(PeerSyncRequest::IssueInviteFor {
                name: "Lin".into(),
                role: crate::network::invite::InviteRole::Viewer,
            })
        );
    }

    #[test]
    fn pending_invites_are_named_and_emit_exact_resend_and_cancel_requests() {
        let mut overlay = PeerSyncOverlay::default();
        overlay.prompt_invite();
        let row = PendingInvitationSummary {
            id: [7; 32],
            name: "Ada".into(),
            role: crate::network::invite::InviteRole::Commenter,
            expiry_unix_seconds: u64::MAX,
        };
        overlay.finish_typed_request(
            &PeerSyncRequest::ListPendingInvitations,
            Ok(PeerSyncResponse::PendingInvitations(vec![row.clone()])),
            false,
        );
        assert_eq!(overlay.snapshot().pending_invitations, vec![row]);
        assert_eq!(
            overlay.activate(Button::Resend([7; 32])).request,
            Some(PeerSyncRequest::ResendInvitation([7; 32]))
        );
        assert_eq!(
            overlay.activate(Button::CancelPending([7; 32])).request,
            Some(PeerSyncRequest::CancelInvitation([7; 32]))
        );
    }

    #[test]
    fn connect_is_trimmed_and_only_closes_after_success() {
        let mut overlay = PeerSyncOverlay::default();
        overlay.prompt_connect();
        overlay.handle_event(
            &key(Key::Character(" token ".into())),
            Modifiers::default(),
            None,
        );
        let submit = overlay.handle_event(
            &key(Key::Named(NamedKey::Enter)),
            Modifiers::default(),
            None,
        );
        assert_eq!(
            submit.request,
            Some(PeerSyncRequest::AcceptInvite("token".into()))
        );
        overlay.finish_request(
            submit.request.as_ref().unwrap(),
            Err("bad token".into()),
            false,
        );
        assert!(overlay.state.connect_input.is_some());
        overlay.finish_request(submit.request.as_ref().unwrap(), Ok("room-7".into()), false);
        assert!(overlay.state.connect_input.is_none());
    }

    #[test]
    fn fingerprint_is_stable_grouped_and_safe_for_short_ids() {
        let a = "12D3KooWQr4Zc8gN3xkLpWv7bTqA9sYd2EfGh1JkLmNpQrStUvWx";
        assert_eq!(peer_fingerprint(a), "QrSt UvWx");
        assert_ne!(
            peer_fingerprint(a),
            peer_fingerprint("12D3KooWQr4Zc8gN3xkLpWv7bTqA9sYd2EfGh1JkLmNpQrStUvAb")
        );
        assert_eq!(peer_fingerprint("abc"), "abc");
        assert_eq!(peer_fingerprint(""), "");
    }

    #[test]
    fn long_tokens_are_elided_without_changing_short_ones() {
        let shown = elide_token(&"e".repeat(900));
        assert!(shown.chars().count() < 20);
        assert!(shown.contains('…'));
        assert_eq!(elide_token("short-one"), "short-one");
    }
}
