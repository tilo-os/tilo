//! Screen 6 — channel invite acceptance.
//!
//! Triggered by a `qos://invite/...` URL (typed, chat handoff, or system "open").
//! On accept, builds a `ChannelEvent::AttestationIssued` signed by the embedded
//! delegation key the inviting owner pre-signed, and emits it via the supplied
//! callback.

use std::collections::HashMap;
use std::time::SystemTime;

use automerge::{AutoCommit, ObjId};
use collab_types::{ChannelEvent, Ed25519PublicKey};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::{
    IdentityFlowError, InviteEnvelope, ScreenOutcome, build_attestation_from_invite,
};

pub struct InviteAcceptScreen {
    rect: Rect,
    url: String,
    parsed: Option<InviteEnvelope>,
    parse_error: Option<String>,
    url_focus: bool,
    url_rect: Rect,
    accept_rect: Rect,
    decline_rect: Rect,
    accept_hovered: bool,
    decline_hovered: bool,
    /// The `AttestationIssued` event for the shell to broadcast.
    pub last_accepted_event: Option<ChannelEvent>,
    accept_error: Option<String>,
    last_cursor: Point,
}

impl Default for InviteAcceptScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl InviteAcceptScreen {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            url: String::new(),
            parsed: None,
            parse_error: None,
            url_focus: true,
            url_rect: Rect::ZERO,
            accept_rect: Rect::ZERO,
            decline_rect: Rect::ZERO,
            accept_hovered: false,
            decline_hovered: false,
            last_accepted_event: None,
            accept_error: None,
            last_cursor: Point::ZERO,
        }
    }

    pub fn set_url(&mut self, url: impl Into<String>) {
        self.url = url.into();
        self.parse();
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn parsed(&self) -> Option<&InviteEnvelope> {
        self.parsed.as_ref()
    }

    pub fn parse_error(&self) -> Option<&str> {
        self.parse_error.as_deref()
    }

    pub fn accept_error(&self) -> Option<&str> {
        self.accept_error.as_deref()
    }

    pub fn last_accepted_event(&self) -> Option<&ChannelEvent> {
        self.last_accepted_event.as_ref()
    }

    pub fn parse(&mut self) {
        self.last_accepted_event = None;
        self.accept_error = None;
        if self.url.is_empty() {
            self.parsed = None;
            self.parse_error = None;
            return;
        }
        match InviteEnvelope::from_url(self.url.trim()) {
            Ok(env) => match env.verify_signature() {
                Ok(()) => {
                    self.parsed = Some(env);
                    self.parse_error = None;
                }
                Err(e) => {
                    self.parsed = None;
                    self.parse_error = Some(format!("Signature invalid: {e}"));
                }
            },
            Err(e) => {
                self.parsed = None;
                self.parse_error = Some(format!("{e}"));
            }
        }
    }

    /// On success, stores the attestation in `last_accepted_event`; the container
    /// should treat that as a `ScreenOutcome::Advance`.
    pub fn accept(
        &mut self,
        invitee_pubkey: Ed25519PublicKey,
        now: SystemTime,
    ) -> Result<&ChannelEvent, IdentityFlowError> {
        let env = self
            .parsed
            .as_ref()
            .ok_or_else(|| IdentityFlowError::DecodeFailed("no parsed invite to accept".into()))?;
        let event = build_attestation_from_invite(env, invitee_pubkey, now)?;
        self.last_accepted_event = Some(event);
        Ok(self.last_accepted_event.as_ref().unwrap())
    }

    pub fn decline(&mut self) {
        self.parsed = None;
        self.url.clear();
        self.parse_error = None;
        self.last_accepted_event = None;
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
                let a = self.accept_rect.contains(p) && self.parsed.is_some();
                let d = self.decline_rect.contains(p);
                if a != self.accept_hovered || d != self.decline_hovered {
                    self.accept_hovered = a;
                    self.decline_hovered = d;
                    *handled = true;
                }
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.url_rect.contains(self.last_cursor) {
                    self.url_focus = true;
                    *handled = true;
                    return ScreenOutcome::Stay;
                }
                if self.decline_rect.contains(self.last_cursor) {
                    self.decline();
                    *handled = true;
                    return ScreenOutcome::Back;
                }
                // Accept needs the invitee pubkey, which only the shell has — it
                // calls `accept()` directly, and the container catches Advance and
                // consults `last_accepted_event` for the result.
                if self.accept_rect.contains(self.last_cursor) && self.parsed.is_some() {
                    *handled = true;
                    return ScreenOutcome::Advance;
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
                if !self.url_focus {
                    return ScreenOutcome::Stay;
                }
                match logical_key {
                    Key::Named(NamedKey::Backspace) => {
                        if !self.url.is_empty() {
                            self.url.pop();
                            *handled = true;
                            self.parse();
                        }
                    }
                    Key::Named(NamedKey::Enter) => {
                        *handled = true;
                        self.parse();
                    }
                    _ => {
                        if let Some(t) = text {
                            for c in t.chars() {
                                if c.is_control() {
                                    continue;
                                }
                                self.url.push(c);
                                *handled = true;
                            }
                            self.parse();
                        }
                    }
                }
                ScreenOutcome::Stay
            }
            _ => ScreenOutcome::Stay,
        }
    }
}

impl Widget for InviteAcceptScreen {
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
                height: Dimension::length(540.0),
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
        let card_h = 440.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let pad = 24.0;
        self.url_rect = Rect::new(
            card_x + pad,
            card_y + 80.0,
            card_x + card_w - pad,
            card_y + 80.0 + 56.0,
        );
        self.accept_rect = Rect::new(
            card_x + card_w - pad - 160.0,
            card_y + card_h - pad - 44.0,
            card_x + card_w - pad,
            card_y + card_h - pad,
        );
        self.decline_rect = Rect::new(
            card_x + pad,
            card_y + card_h - pad - 44.0,
            card_x + pad + 140.0,
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
        let card_h = 440.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let card = Rect::new(card_x, card_y, card_x + card_w, card_y + card_h);
        let radius = 12.0;
        let pad = 24.0;
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
            "Accept channel invite",
            22.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((card.x0 + pad, card.y0 + pad)),
            None,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &RoundedRect::from_rect(self.url_rect, 8.0),
        );
        scene.stroke(
            &Stroke::new(if self.url_focus { 2.0 } else { 1.0 }),
            transform,
            if self.url_focus {
                theme.border_focus
            } else {
                theme.tokens.colors.border_standard
            },
            None,
            &RoundedRect::from_rect(self.url_rect, 8.0),
        );
        let placeholder = if self.url.is_empty() {
            "Paste a qos://invite/... URL".to_string()
        } else {
            self.url.clone()
        };
        let url_color = if self.url.is_empty() {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.text_primary
        };
        text_renderer.draw_text(
            scene,
            &placeholder,
            13.0,
            url_color,
            transform * Affine::translate((self.url_rect.x0 + 12.0, self.url_rect.y0 + 12.0)),
            None,
        );
        if let Some(env) = &self.parsed {
            let banner = format!(
                "{} invited you to #{} as {:?}",
                env.inviter_label, env.channel_name, env.role
            );
            text_renderer.draw_text(
                scene,
                &banner,
                15.0,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((card.x0 + pad, card.y0 + 170.0)),
                None,
            );
            let when = chrono::DateTime::<chrono::Utc>::from(
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(env.expires_unix),
            )
            .to_rfc3339();
            text_renderer.draw_text(
                scene,
                &format!("Expires {when}"),
                12.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((card.x0 + pad, card.y0 + 200.0)),
                None,
            );
        } else if let Some(err) = &self.parse_error {
            text_renderer.draw_text(
                scene,
                err,
                13.0,
                Color::from_rgb8(220, 80, 80),
                transform * Affine::translate((card.x0 + pad, card.y0 + 170.0)),
                None,
            );
        }
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &RoundedRect::from_rect(self.decline_rect, 8.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(self.decline_rect, 8.0),
        );
        text_renderer.draw_text(
            scene,
            "Decline",
            13.0,
            theme.tokens.colors.text_primary,
            transform
                * Affine::translate((
                    self.decline_rect.x0 + 16.0,
                    self.decline_rect.y0 + (self.decline_rect.height() - 13.0) / 2.0,
                )),
            None,
        );
        let enabled = self.parsed.is_some();
        let bg = if !enabled {
            theme.tokens.colors.surfaces.base
        } else if self.accept_hovered {
            theme.bg_active
        } else {
            theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX]
        };
        let label_color = if enabled {
            Color::from_rgb8(255, 255, 255)
        } else {
            theme.tokens.colors.text_dim
        };
        scene.fill(
            Fill::NonZero,
            transform,
            bg,
            None,
            &RoundedRect::from_rect(self.accept_rect, 8.0),
        );
        let (aw, _) = text_renderer.measure("Accept invite", 14.0, None);
        text_renderer.draw_text(
            scene,
            "Accept invite",
            14.0,
            label_color,
            transform
                * Affine::translate((
                    self.accept_rect.x0 + (self.accept_rect.width() - f64::from(aw)) / 2.0,
                    self.accept_rect.y0 + (self.accept_rect.height() - 14.0) / 2.0,
                )),
            None,
        );
        if let Some(e) = &self.accept_error {
            text_renderer.draw_text(
                scene,
                e,
                12.0,
                Color::from_rgb8(220, 80, 80),
                transform * Affine::translate((card.x0 + pad, self.accept_rect.y0 - 24.0)),
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
        "InviteAcceptScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_invite_accept")
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
    use collab_types::{ChannelId, Role, generate_keypair};
    use std::time::Duration;

    fn fixture_url(now: SystemTime) -> String {
        use crate::widget::identity_flow::mint_invite;
        let (sk, pk) = generate_keypair();
        let env = mint_invite(
            ChannelId::new(),
            "engineering",
            pk,
            &sk,
            "Alice",
            Role::Editor,
            now + Duration::from_secs(3600),
        )
        .unwrap();
        env.to_url().unwrap()
    }

    #[test]
    fn pasting_valid_url_populates_parsed() {
        let url = fixture_url(SystemTime::now());
        let mut s = InviteAcceptScreen::new();
        s.set_url(&url);
        assert!(s.parsed().is_some(), "parse error: {:?}", s.parse_error());
        assert_eq!(s.parsed().unwrap().role, Role::Editor);
    }

    #[test]
    fn pasting_garbage_url_surfaces_error() {
        let mut s = InviteAcceptScreen::new();
        s.set_url("https://example.com/not-an-invite");
        assert!(s.parsed().is_none());
        assert!(s.parse_error().is_some());
    }

    #[test]
    fn accept_builds_attestation_event() {
        let url = fixture_url(SystemTime::now());
        let mut s = InviteAcceptScreen::new();
        s.set_url(&url);
        let (_invitee_sk, invitee_pk) = generate_keypair();
        let event = s
            .accept(invitee_pk, SystemTime::now())
            .expect("accept must succeed")
            .clone();
        match event {
            ChannelEvent::AttestationIssued(ra) => {
                assert_eq!(ra.attestation.subject.as_bytes(), invitee_pk.as_bytes());
            }
            _ => panic!("expected AttestationIssued"),
        }
    }

    #[test]
    fn decline_clears_state() {
        let url = fixture_url(SystemTime::now());
        let mut s = InviteAcceptScreen::new();
        s.set_url(&url);
        s.decline();
        assert!(s.parsed().is_none());
        assert!(s.url().is_empty());
    }

    #[test]
    fn tampered_url_fails_signature() {
        let url = fixture_url(SystemTime::now());
        // Truncation exercises the parse_error path (doesn't decode cleanly).
        let mut chars: Vec<char> = url.chars().collect();
        for _ in 0..4 {
            chars.pop();
        }
        let bad: String = chars.into_iter().collect();
        let mut s = InviteAcceptScreen::new();
        s.set_url(&bad);
        assert!(s.parsed().is_none());
        assert!(s.parse_error().is_some());
    }
}
