//! Screen 5 — mint a channel invite.
//!
//! Channel owner picks a title + expiry, clicks "Generate invite", and gets
//! back a `qos://invite/<base32>` URL (with Copy/Revoke buttons) and a
//! QR-code rendering of it.
//!
//! Cryptographic flow: generate a one-time delegation keypair, wrap the
//! invitee role + expiry + delegation pubkey in an `InviteEnvelope` signed
//! over the canonical view with the owner's key, then encode it as postcard +
//! base32 into the URL.
//!
//! `mint_now` is exposed for tests / shell so the owner's signing key (an
//! Ed25519 secret) can be passed in without threading it through the widget
//! event surface.

use std::collections::HashMap;
use std::time::SystemTime;

use automerge::{AutoCommit, ObjId};
use collab_types::{ChannelId, Ed25519PublicKey, Role};
use ed25519_dalek::SigningKey as DalekSigningKey;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::text_selection::clipboard_copy;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::{InviteEnvelope, InviteExpiry, ScreenOutcome, mint_invite};
use crate::widget::qr::QrWidget;

/// Screen 5: invite-mint form.
pub struct InviteMintScreen {
    rect: Rect,
    /// Channel id we're minting an invite into. The container seeds
    /// this; tests pass it explicitly via `set_channel`.
    channel: Option<ChannelId>,
    channel_name: String,
    expiry: InviteExpiry,
    expiry_rects: Vec<Rect>,
    generate_rect: Rect,
    revoke_rect: Rect,
    copy_rect: Rect,
    last_cursor: Point,
    /// Most recent minted envelope, if any.
    pub last_envelope: Option<InviteEnvelope>,
    pub last_url: Option<String>,
    pub qr: Option<QrWidget>,
    clipboard_msg: Option<String>,
    error: Option<String>,
}

impl Default for InviteMintScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl InviteMintScreen {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            channel: None,
            channel_name: String::new(),
            expiry: InviteExpiry::OneDay,
            expiry_rects: Vec::new(),
            generate_rect: Rect::ZERO,
            revoke_rect: Rect::ZERO,
            copy_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            last_envelope: None,
            last_url: None,
            qr: None,
            clipboard_msg: None,
            error: None,
        }
    }

    pub fn set_channel(&mut self, channel: ChannelId, name: impl Into<String>) {
        self.channel = Some(channel);
        self.channel_name = name.into();
    }

    pub fn set_expiry(&mut self, e: InviteExpiry) {
        self.expiry = e;
    }

    pub fn expiry(&self) -> InviteExpiry {
        self.expiry
    }

    pub fn last_url(&self) -> Option<&str> {
        self.last_url.as_deref()
    }

    pub fn last_envelope(&self) -> Option<&InviteEnvelope> {
        self.last_envelope.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn clipboard_msg(&self) -> Option<&str> {
        self.clipboard_msg.as_deref()
    }

    /// Mint a new invite using the supplied owner identity.
    ///
    /// `now` lets tests pin a deterministic timestamp. The
    /// inviter label is taken from the surrounding identity record
    /// — the container reads it from the loaded `IdentityRecord`.
    ///
    /// # Errors
    ///
    /// Returns an error if no channel has been set via [`Self::set_channel`],
    /// or if minting/encoding the invite fails.
    ///
    /// # Panics
    ///
    /// Never panics: the envelope stored just above is always read back
    /// successfully.
    pub fn mint_now(
        &mut self,
        owner_pubkey: Ed25519PublicKey,
        owner_signing_key: &DalekSigningKey,
        inviter_label: impl Into<String>,
        role: Role,
        now: SystemTime,
    ) -> Result<&InviteEnvelope, crate::widget::identity_flow::IdentityFlowError> {
        let channel = self.channel.ok_or_else(|| {
            crate::widget::identity_flow::IdentityFlowError::DecodeFailed("no channel set".into())
        })?;
        let expires_at = self.expiry.resolve(now);
        let env = mint_invite(
            channel,
            self.channel_name.clone(),
            owner_pubkey,
            owner_signing_key,
            inviter_label,
            role,
            expires_at,
        )?;
        let url = env.to_url()?;
        self.qr = Some(QrWidget::new(url.as_bytes().to_vec()));
        self.last_envelope = Some(env);
        self.last_url = Some(url);
        self.error = None;
        self.clipboard_msg = None;
        self.last_envelope.as_ref().ok_or_else(|| {
            crate::widget::identity_flow::IdentityFlowError::DecodeFailed(
                "invite envelope was not stored after minting".into(),
            )
        })
    }

    pub fn copy_url_to_clipboard(&mut self) {
        let Some(url) = &self.last_url else {
            self.clipboard_msg = Some("Generate an invite first.".to_string());
            return;
        };
        clipboard_copy(url);
        self.clipboard_msg = Some("Copied to clipboard.".to_string());
    }

    pub fn revoke(&mut self) {
        self.last_envelope = None;
        self.last_url = None;
        self.qr = None;
        self.clipboard_msg = None;
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
                for (i, r) in self.expiry_rects.iter().enumerate() {
                    if r.contains(self.last_cursor) {
                        self.expiry = match i {
                            0 => InviteExpiry::OneHour,
                            1 => InviteExpiry::OneDay,
                            2 => InviteExpiry::OneWeek,
                            _ => InviteExpiry::Never,
                        };
                        *handled = true;
                        return ScreenOutcome::Stay;
                    }
                }
                if self.revoke_rect.contains(self.last_cursor) {
                    self.revoke();
                    *handled = true;
                    return ScreenOutcome::Stay;
                }
                if self.copy_rect.contains(self.last_cursor) {
                    self.copy_url_to_clipboard();
                    *handled = true;
                    return ScreenOutcome::Stay;
                }
                // Needs the owner's key, which the shell-side host provides
                // through `mint_now`; the button is wired up at that layer.
                if self.generate_rect.contains(self.last_cursor) {
                    *handled = true;
                    self.error = Some("Wire owner signing key via mint_now".to_string());
                    return ScreenOutcome::Stay;
                }
                ScreenOutcome::Stay
            }
            _ => ScreenOutcome::Stay,
        }
    }

    /// The centered dialog card rect, derived from `self.rect`. Recomputed
    /// rather than cached so `draw`'s split-out helpers below don't need
    /// it threaded through as a parameter.
    fn card_rect(&self) -> Rect {
        let card_w = 600.0;
        let card_h = 480.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        Rect::new(card_x, card_y, card_x + card_w, card_y + card_h)
    }

    /// Backdrop, card background/border, and the title + "Expires after"
    /// labels.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the `transform * Affine::translate(..)` calls below compose 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
    )]
    fn draw_frame(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let card = self.card_rect();
        let backdrop = Color::from_rgba8(0, 0, 0, 180);
        scene.fill(Fill::NonZero, transform, backdrop, None, &self.rect);
        let pad = 24.0;
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
        let title = if self.channel_name.is_empty() {
            "Generate invite".to_string()
        } else {
            format!("Invite to #{}", self.channel_name)
        };
        text_renderer.draw_text(
            scene,
            &title,
            22.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((card.x0 + pad, card.y0 + pad)),
            None,
        );
        text_renderer.draw_text(
            scene,
            "Expires after",
            13.0,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((card.x0 + pad, card.y0 + 100.0)),
            None,
        );
    }

    /// The four expiry-preset segments (`self.expiry_rects`), highlighting
    /// whichever matches `self.expiry`.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the `transform * Affine::translate(..)` call below composes 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
    )]
    fn draw_expiry_options(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        for (i, r) in self.expiry_rects.iter().enumerate() {
            let label = match i {
                0 => InviteExpiry::OneHour.label(),
                1 => InviteExpiry::OneDay.label(),
                2 => InviteExpiry::OneWeek.label(),
                _ => InviteExpiry::Never.label(),
            };
            let selected = matches!(
                (self.expiry, i),
                (InviteExpiry::OneHour, 0)
                    | (InviteExpiry::OneDay, 1)
                    | (InviteExpiry::OneWeek, 2)
                    | (InviteExpiry::Never, 3)
            );
            let bg = if selected {
                theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX]
            } else {
                theme.tokens.colors.surfaces.base
            };
            let text_color = if selected {
                Color::from_rgb8(255, 255, 255)
            } else {
                theme.tokens.colors.text_primary
            };
            scene.fill(
                Fill::NonZero,
                transform,
                bg,
                None,
                &RoundedRect::from_rect(*r, 6.0),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_standard,
                None,
                &RoundedRect::from_rect(*r, 6.0),
            );
            let (lw, _) = text_renderer.measure(label, 13.0, None);
            text_renderer.draw_text(
                scene,
                label,
                13.0,
                text_color,
                transform
                    * Affine::translate((
                        r.x0 + (r.width() - f64::from(lw)) / 2.0,
                        r.y0 + (r.height() - 13.0) / 2.0,
                    )),
                None,
            );
        }
    }

    /// The "Generate invite" call-to-action button.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the `transform * Affine::translate(..)` call below composes 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
    )]
    fn draw_generate_button(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.accent[crate::tokens::colors::ACCENT_PRIMARY_IDX],
            None,
            &RoundedRect::from_rect(self.generate_rect, 8.0),
        );
        let (gw, _) = text_renderer.measure("Generate invite", 13.0, None);
        text_renderer.draw_text(
            scene,
            "Generate invite",
            13.0,
            Color::from_rgb8(255, 255, 255),
            transform
                * Affine::translate((
                    self.generate_rect.x0 + (self.generate_rect.width() - f64::from(gw)) / 2.0,
                    self.generate_rect.y0 + (self.generate_rect.height() - 13.0) / 2.0,
                )),
            None,
        );
    }

    /// The minted invite URL, shown once `self.last_url` is populated.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the `transform * Affine::translate(..)` call below composes 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
    )]
    fn draw_url_line(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(url) = &self.last_url else {
            return;
        };
        let card = self.card_rect();
        let pad = 24.0;
        let text_y = card.y0 + 240.0;
        text_renderer.draw_text(
            scene,
            url,
            12.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((card.x0 + pad, text_y)),
            None,
        );
    }

    /// The "Copy URL" and "Revoke" footer buttons.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the `transform * Affine::translate(..)` calls below compose 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
    )]
    fn draw_copy_and_revoke_buttons(
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
            &RoundedRect::from_rect(self.copy_rect, 8.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &RoundedRect::from_rect(self.copy_rect, 8.0),
        );
        text_renderer.draw_text(
            scene,
            "Copy URL",
            13.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.copy_rect.x0 + 16.0, self.copy_rect.y0 + 12.0)),
            None,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &RoundedRect::from_rect(self.revoke_rect, 8.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            Color::from_rgb8(220, 80, 80),
            None,
            &RoundedRect::from_rect(self.revoke_rect, 8.0),
        );
        text_renderer.draw_text(
            scene,
            "Revoke",
            13.0,
            Color::from_rgb8(220, 80, 80),
            transform * Affine::translate((self.revoke_rect.x0 + 16.0, self.revoke_rect.y0 + 12.0)),
            None,
        );
    }

    /// The clipboard-copy confirmation and mint-error messages, stacked
    /// above the "Copy URL" button.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the `transform * Affine::translate(..)` calls below compose 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
    )]
    fn draw_status_messages(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let card = self.card_rect();
        let pad = 24.0;
        if let Some(msg) = &self.clipboard_msg {
            text_renderer.draw_text(
                scene,
                msg,
                12.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((card.x0 + pad, self.copy_rect.y0 - 20.0)),
                None,
            );
        }
        if let Some(err) = &self.error {
            text_renderer.draw_text(
                scene,
                err,
                12.0,
                Color::from_rgb8(220, 80, 80),
                transform * Affine::translate((card.x0 + pad, self.copy_rect.y0 - 36.0)),
                None,
            );
        }
    }
}

impl Widget for InviteMintScreen {
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
                height: Dimension::length(580.0),
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
        let card_w = 600.0;
        let card_h = 480.0;
        let card_x = self.rect.x0 + (self.rect.width() - card_w) / 2.0;
        let card_y = self.rect.y0 + (self.rect.height() - card_h) / 2.0;
        let pad = 24.0;
        let seg_w = (card_w - 2.0 * pad) / 4.0;
        let seg_h = 36.0;
        let seg_y = card_y + 130.0;
        self.expiry_rects.clear();
        for i in 0..4 {
            let x = card_x + pad + f64::from(i) * seg_w;
            self.expiry_rects
                .push(Rect::new(x, seg_y, x + seg_w, seg_y + seg_h));
        }
        self.generate_rect = Rect::new(
            card_x + card_w - pad - 160.0,
            card_y + 180.0,
            card_x + card_w - pad,
            card_y + 180.0 + 40.0,
        );
        self.copy_rect = Rect::new(
            card_x + pad,
            card_y + card_h - pad - 44.0,
            card_x + pad + 140.0,
            card_y + card_h - pad,
        );
        self.revoke_rect = Rect::new(
            card_x + card_w - pad - 140.0,
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
        self.draw_frame(scene, transform, text_renderer, theme);
        self.draw_expiry_options(scene, transform, text_renderer, theme);
        self.draw_generate_button(scene, transform, text_renderer, theme);
        self.draw_url_line(scene, transform, text_renderer, theme);
        self.draw_copy_and_revoke_buttons(scene, transform, text_renderer, theme);
        self.draw_status_messages(scene, transform, text_renderer, theme);
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
        "InviteMintScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_invite_mint")
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
    use collab_types::generate_keypair;
    use std::time::Duration;

    #[test]
    fn mint_now_produces_url_and_envelope() {
        let mut s = InviteMintScreen::new();
        s.set_channel(ChannelId::new(), "engineering");
        s.set_expiry(InviteExpiry::OneHour);
        let (sk, pk) = generate_keypair();
        let env = s
            .mint_now(pk, &sk, "Alice", Role::Editor, SystemTime::now())
            .unwrap()
            .clone();
        assert!(s.last_url().unwrap().starts_with("qos://invite/"));
        assert_eq!(env.role, Role::Editor);
    }

    #[test]
    fn mint_now_errors_without_channel() {
        let mut s = InviteMintScreen::new();
        let (sk, pk) = generate_keypair();
        let err = s
            .mint_now(pk, &sk, "Alice", Role::Editor, SystemTime::now())
            .unwrap_err();
        assert!(matches!(
            err,
            crate::widget::identity_flow::IdentityFlowError::DecodeFailed(_)
        ));
    }

    #[test]
    fn revoke_clears_state() {
        let mut s = InviteMintScreen::new();
        s.set_channel(ChannelId::new(), "engineering");
        let (sk, pk) = generate_keypair();
        s.mint_now(pk, &sk, "Alice", Role::Editor, SystemTime::now())
            .unwrap();
        assert!(s.last_envelope().is_some());
        s.revoke();
        assert!(s.last_envelope().is_none());
        assert!(s.last_url().is_none());
        assert!(s.qr.is_none());
    }

    #[test]
    fn expiry_one_hour_resolves_one_hour_ahead() {
        let now = SystemTime::UNIX_EPOCH;
        let expires = InviteExpiry::OneHour.resolve(now);
        assert_eq!(
            expires.duration_since(now).unwrap(),
            Duration::from_secs(3600)
        );
    }
}
