//! Screen 5 — mint a channel invite.
//!
//! Channel owner picks a title + expiry, clicks "Generate invite", and gets
//! back a `tilo://invite/<base32>` URL (with Copy/Revoke buttons) and a
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

use crate::time::SystemTime;
use std::collections::HashMap;

use collab_types::{ChannelId, Ed25519PublicKey, Role as ChannelRole};
use ed25519_dalek::SigningKey as DalekSigningKey;
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::text_selection::clipboard_copy;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::identity_flow::sheet::{
    Align, BodyScroll, FORM_SHEET_WIDTH, Resolved, Role, Rows, SheetSpec, line_height, paint_qr,
    paint_segments, paint_text, paragraph_height, resolve, screen_style, segment_rects,
    with_layout_renderer, wrap_chars,
};
use crate::widget::identity_flow::{InviteEnvelope, InviteExpiry, ScreenOutcome, mint_invite};
use crate::widget::modal_dialog::ModalAction;
use crate::widget::qr::QrWidget;

const EXPIRY_LABEL: &str = "Expires after";
const LINK_LABEL: &str = "Invite link";
const EMPTY_HINT: &str = "Generate an invite to get a link and a QR code to share.";
const GENERATE_LABEL: &str = "Generate invite";
const COPY_LABEL: &str = "Copy URL";
const REVOKE_LABEL: &str = "Revoke";

/// The four expiry presets in segment order.
const EXPIRIES: [InviteExpiry; 4] = [
    InviteExpiry::OneHour,
    InviteExpiry::OneDay,
    InviteExpiry::OneWeek,
    InviteExpiry::Never,
];

/// Action-row indices, in authoring order.
const ACTION_COPY: usize = 0;
const ACTION_REVOKE: usize = 1;
const ACTION_GENERATE: usize = 2;

/// Every rect the screen paints and hit-tests, from one pass over the rows.
#[derive(Clone, Debug, Default)]
struct Plan {
    expiry_label: Rect,
    segments: Vec<Rect>,
    /// The QR and the link under it, once an invite exists. The link is
    /// one unbreakable token, so it is wrapped by character into as many
    /// rows as it needs — never elided, so it can be read back.
    qr: Option<Rect>,
    link_label: Option<Rect>,
    link_lines: Vec<(String, Rect)>,
    /// The empty-state hint while nothing has been minted.
    hint: Option<Rect>,
    clipboard: Option<Rect>,
    error: Option<Rect>,
    bottom: f64,
}

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
    /// The theme `build_layout` saw, so `apply_layout` can resolve the rows
    /// without waiting for a paint.
    layout_theme: Option<Theme>,
    scroll: BodyScroll,
    body_height: f64,
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
            layout_theme: None,
            scroll: BodyScroll::default(),
            body_height: 0.0,
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
        role: ChannelRole,
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
    pub fn handle_event_screen(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> ScreenOutcome {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                for (i, r) in self.expiry_rects.iter().enumerate() {
                    if r.contains(self.last_cursor) {
                        self.expiry = EXPIRIES.get(i).copied().unwrap_or(InviteExpiry::Never);
                        *ctx.handled = true;
                        return ScreenOutcome::Stay;
                    }
                }
                if self.last_url.is_some() && self.revoke_rect.contains(self.last_cursor) {
                    self.revoke();
                    *ctx.handled = true;
                    return ScreenOutcome::Stay;
                }
                if self.last_url.is_some() && self.copy_rect.contains(self.last_cursor) {
                    self.copy_url_to_clipboard();
                    *ctx.handled = true;
                    return ScreenOutcome::Stay;
                }
                // Needs the owner's key, which the shell-side host provides
                // through `mint_now`; the button is wired up at that layer.
                if self.generate_rect.contains(self.last_cursor) {
                    *ctx.handled = true;
                    self.error = Some("Wire owner signing key via mint_now".to_string());
                    return ScreenOutcome::Stay;
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

    /// One geometry pass for both `apply_layout` and `draw`; hit rects are
    /// clipped to the visible body.
    fn resolve(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) -> Resolved<Plan> {
        let title = self.title();
        let actions = self.actions();
        let spec = SheetSpec {
            title: &title,
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
        self.expiry_rects = resolved
            .plan
            .segments
            .iter()
            .map(|r| BodyScroll::visible(body, *r))
            .collect();
        self.copy_rect = resolved.sheet.action(ACTION_COPY);
        self.revoke_rect = resolved.sheet.action(ACTION_REVOKE);
        self.generate_rect = resolved.sheet.action(ACTION_GENERATE);
        resolved
    }

    fn title(&self) -> String {
        if self.channel_name.is_empty() {
            GENERATE_LABEL.to_string()
        } else {
            format!("Invite to #{}", self.channel_name)
        }
    }

    fn selected_index(&self) -> usize {
        EXPIRIES.iter().position(|e| *e == self.expiry).unwrap_or(1)
    }

    fn actions(&self) -> [ModalAction<'static>; 3] {
        let minted = self.last_url.is_some();
        [
            ModalAction::secondary(COPY_LABEL)
                .enabled(minted)
                .hovered(self.copy_rect.contains(self.last_cursor)),
            ModalAction::danger(REVOKE_LABEL)
                .enabled(minted)
                .hovered(self.revoke_rect.contains(self.last_cursor)),
            ModalAction::primary(GENERATE_LABEL)
                .hovered(self.generate_rect.contains(self.last_cursor)),
        ]
    }

    /// Walk the body rows once: the expiry control, then either the minted
    /// QR + link or the empty-state hint, then any status lines.
    fn plan(
        &self,
        mut rows: Rows,
        control_h: f64,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Plan {
        let sp = &theme.tokens.spacing;
        let width = rows.width();
        let expiry_label = rows.row(line_height(text_renderer, theme, Role::Label));
        rows.gap(sp.s1);
        let segments = segment_rects(rows.row(control_h), EXPIRIES.len());
        rows.gap(sp.s6);
        let (mut qr, mut link_label, mut link_lines, mut hint) = (None, None, Vec::new(), None);
        if let Some(url) = self.last_url.as_deref() {
            // A QR wants to be as large as the body allows, up to four
            // `s16` — big enough to scan from a phone across a desk.
            let side = width.min(f64::from(sp.s16) * 4.0);
            let qr_row = rows.row(side);
            let x0 = qr_row.x0 + (qr_row.width() - side) / 2.0;
            qr = Some(Rect::new(x0, qr_row.y0, x0 + side, qr_row.y1));
            rows.gap(sp.s3);
            link_label = Some(rows.row(line_height(text_renderer, theme, Role::Label)));
            rows.gap(sp.s1);
            let line_h = line_height(text_renderer, theme, Role::Value);
            link_lines = wrap_chars(text_renderer, theme, Role::Value, url, width)
                .into_iter()
                .map(|line| (line, rows.row(line_h)))
                .collect();
        } else {
            hint = Some(rows.row(paragraph_height(
                text_renderer,
                theme,
                Role::Caption,
                EMPTY_HINT,
                width,
            )));
        }
        let clipboard = self.clipboard_msg.as_deref().map(|msg| {
            rows.gap(sp.s3);
            rows.row(paragraph_height(
                text_renderer,
                theme,
                Role::Caption,
                msg,
                width,
            ))
        });
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
            expiry_label,
            segments,
            qr,
            link_label,
            link_lines,
            hint,
            clipboard,
            error,
            bottom: rows.bottom(),
        }
    }

    /// The minted state: QR, its label, and the whole link wrapped under it.
    fn draw_minted(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        plan: &Plan,
    ) {
        if self.last_url.is_none() {
            return;
        }
        if let (Some(qr), Some(rect)) = (&self.qr, plan.qr) {
            paint_qr(scene, transform, text_renderer, theme, qr, rect);
        }
        if let Some(rect) = plan.link_label {
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                LINK_LABEL,
                Role::Label,
                Align::Center,
                false,
            );
        }
        for (line, rect) in &plan.link_lines {
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                *rect,
                line,
                Role::Value,
                Align::Center,
                false,
            );
        }
    }
}

impl Widget for InviteMintScreen {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = Some(theme.clone());
        taffy.widget_leaf(screen_style(), node_map, self)
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
        _presence: &PresenceCtx,
    ) {
        // Identity ceremonies, invites and joins are security surfaces:
        // every look-alike letter is set apart.
        text_renderer.security_surface(|text_renderer| {
            let resolved = self.resolve(theme, text_renderer);
            let title = self.title();
            let actions = self.actions();
            let spec = SheetSpec {
                title: &title,
                max_width: FORM_SHEET_WIDTH,
                actions: &actions,
            };
            let sheet = &resolved.sheet;
            let plan = &resolved.plan;

            sheet.paint_frame(scene, transform, text_renderer, theme, &spec);
            sheet.clip_body(scene, transform);
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                plan.expiry_label,
                EXPIRY_LABEL,
                Role::Label,
                Align::Start,
                false,
            );
            let labels: Vec<&str> = EXPIRIES.iter().map(super::InviteExpiry::label).collect();
            let hovered = plan
                .segments
                .iter()
                .position(|r| r.contains(self.last_cursor));
            paint_segments(
                scene,
                transform,
                text_renderer,
                theme,
                &plan.segments,
                &labels,
                self.selected_index(),
                hovered,
            );
            self.draw_minted(scene, transform, text_renderer, theme, plan);
            if let Some(rect) = plan.hint {
                paint_text(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    rect,
                    EMPTY_HINT,
                    Role::Caption,
                    Align::Start,
                    true,
                );
            }
            if let (Some(rect), Some(msg)) = (plan.clipboard, self.clipboard_msg.as_deref()) {
                paint_text(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    rect,
                    msg,
                    Role::Caption,
                    Align::Start,
                    true,
                );
            }
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
    use crate::time::Duration;
    use collab_types::generate_keypair;

    #[test]
    fn invite_mint_produces_revokes_and_lays_out_links() {
        fn mint_now_produces_url_and_envelope() {
            let mut s = InviteMintScreen::new();
            s.set_channel(ChannelId::new(), "engineering");
            s.set_expiry(InviteExpiry::OneHour);
            let (sk, pk) = generate_keypair();
            let env = s
                .mint_now(pk, &sk, "Alice", ChannelRole::Editor, crate::time::wall())
                .unwrap()
                .clone();
            assert!(s.last_url().unwrap().starts_with("tilo://invite/"));
            assert_eq!(env.role, ChannelRole::Editor);
        }

        fn mint_now_errors_without_channel() {
            let mut s = InviteMintScreen::new();
            let (sk, pk) = generate_keypair();
            let err = s
                .mint_now(pk, &sk, "Alice", ChannelRole::Editor, crate::time::wall())
                .unwrap_err();
            assert!(matches!(
                err,
                crate::widget::identity_flow::IdentityFlowError::DecodeFailed(_)
            ));
        }

        fn revoke_clears_state() {
            let mut s = InviteMintScreen::new();
            s.set_channel(ChannelId::new(), "engineering");
            let (sk, pk) = generate_keypair();
            s.mint_now(pk, &sk, "Alice", ChannelRole::Editor, crate::time::wall())
                .unwrap();
            assert!(s.last_envelope().is_some());
            s.revoke();
            assert!(s.last_envelope().is_none());
            assert!(s.last_url().is_none());
            assert!(s.qr.is_none());
        }

        fn expiry_one_hour_resolves_one_hour_ahead() {
            let now = SystemTime::UNIX_EPOCH;
            let expires = InviteExpiry::OneHour.resolve(now);
            assert_eq!(
                expires.duration_since(now).unwrap(),
                Duration::from_secs(3600)
            );
        }

        /// Before an invite exists the body is the control plus a hint; after
        /// one exists the QR is square, centred, and the link sits under it —
        /// nothing at an absolute offset, nothing running off the card.
        fn plan_shows_a_centred_qr_and_link_only_once_minted() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let body = Rect::new(100.0, 200.0, 620.0, 200.0);
            let mut s = InviteMintScreen::new();
            let empty = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
            assert_eq!(empty.segments.len(), 4);
            assert_eq!(empty.segments[0].height(), 34.0);
            assert!(empty.qr.is_none() && empty.link_lines.is_empty());
            assert!(
                empty.hint.is_some(),
                "an empty state is designed, not blank"
            );

            s.set_channel(ChannelId::new(), "engineering");
            let (sk, pk) = generate_keypair();
            s.mint_now(pk, &sk, "Alice", ChannelRole::Editor, crate::time::wall())
                .unwrap();
            let minted = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
            let qr = minted.qr.expect("a minted invite shows its QR");
            assert!((qr.width() - qr.height()).abs() < 1e-9, "the QR is square");
            assert!((qr.center().x - body.center().x).abs() < 1e-9, "centred");
            assert!(qr.width() <= body.width());
            assert!(
                minted.link_lines.len() > 1,
                "a 200-character link needs more than one row at this width"
            );
            let whole: String = minted.link_lines.iter().map(|(l, _)| l.as_str()).collect();
            assert_eq!(whole, s.last_url().unwrap(), "the link is readable in full");
            let (_, first) = &minted.link_lines[0];
            let (_, last) = minted.link_lines.last().unwrap();
            assert!(first.y0 > qr.y1);
            assert!(first.x1 <= body.x1, "the link rows stay inside the body");
            assert!(minted.hint.is_none());
            assert!((minted.bottom - last.y1).abs() < 1e-9);
        }

        mint_now_produces_url_and_envelope();
        mint_now_errors_without_channel();
        revoke_clears_state();
        expiry_one_hour_resolves_one_hour_ahead();
        plan_shows_a_centred_qr_and_link_only_once_minted();
    }
}
