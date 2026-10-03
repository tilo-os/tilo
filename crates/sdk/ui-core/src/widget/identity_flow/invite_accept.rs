//! Screen 6 — channel invite acceptance.
//!
//! Triggered by a `tilo://invite/...` URL (typed, chat handoff, or system "open").
//! On accept, builds a `ChannelEvent::AttestationIssued` signed by the embedded
//! delegation key the inviting owner pre-signed, and emits it via the supplied
//! callback.
//!
//! The link field is the shared [`TextInput`], embedded: the sheet places
//! it, the screen forwards it every event it does not claim (Enter
//! re-parses, PageUp/PageDown scroll), and it hangs off the screen's
//! layout node as a child of its own so focus traversal, the accessibility
//! tree and the headless driver reach it by name. Every edit that lands in
//! the field re-parses the link, whether typed, pasted, committed by an
//! IME or set through an accessibility action.

use crate::time::SystemTime;
use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use collab_types::{ChannelEvent, Ed25519PublicKey};
use taffy::{Dimension, Display, NodeId, Size, Style, TaffyTree};
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
    Align, BodyScroll, FORM_SHEET_WIDTH, Resolved, Role, Rows, SheetSpec, edited, line_height,
    paint_text, paragraph_height, place_field, resolve, with_layout_renderer,
};
use crate::widget::identity_flow::{
    IdentityFlowError, InviteEnvelope, ScreenOutcome, build_attestation_from_invite,
};
use crate::widget::modal_dialog::ModalAction;
use crate::widget::text_input::TextInput;

const TITLE: &str = "Accept channel invite";
const LINK_LABEL: &str = "Invite link";
const LINK_PLACEHOLDER: &str = "Paste a tilo://invite/... URL";
const EMPTY_HINT: &str = "Paste an invite link to see who sent it and what it grants.";
const DECLINE_LABEL: &str = "Decline";
const ACCEPT_LABEL: &str = "Accept invite";
/// `test_id` of the invite-link field.
pub const URL_INPUT_ID: &str = "identity_invite_url";

/// Action-row indices, in authoring order.
const ACTION_DECLINE: usize = 0;
const ACTION_ACCEPT: usize = 1;

/// Every rect the screen paints and hit-tests, from one pass over the rows.
#[derive(Clone, Debug, Default)]
struct Plan {
    link_label: Rect,
    url: Rect,
    /// The parsed invite's banner and expiry, when a valid link is present.
    banner: Option<Rect>,
    expires: Option<Rect>,
    /// The parse error, or the hint while the field is empty.
    status: Option<Rect>,
    error: Option<Rect>,
    bottom: f64,
}

pub struct InviteAcceptScreen {
    rect: Rect,
    /// The invite-link field: the shared text input, embedded. Read it
    /// with [`Self::url`], seed it with [`Self::set_url`].
    url: TextInput,
    /// The field's own layout node, minted by `build_layout`.
    url_node: Option<NodeId>,
    /// The link as last parsed, so an edit is a change of text and a caret
    /// move is not (see [`edited`]).
    last_url: String,
    parsed: Option<InviteEnvelope>,
    parse_error: Option<String>,
    url_rect: Rect,
    accept_rect: Rect,
    decline_rect: Rect,
    accept_hovered: bool,
    decline_hovered: bool,
    /// The `AttestationIssued` event for the shell to broadcast.
    pub last_accepted_event: Option<ChannelEvent>,
    accept_error: Option<String>,
    last_cursor: Point,
    /// The theme `build_layout` saw, so `apply_layout` can resolve the rows
    /// without waiting for a paint.
    layout_theme: Option<Theme>,
    scroll: BodyScroll,
    body_height: f64,
}

impl Default for InviteAcceptScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl InviteAcceptScreen {
    pub fn new() -> Self {
        // The screen exists to receive a link, so its one field starts
        // focused: the first paste lands without a click.
        let mut url = TextInput::local()
            .with_id(URL_INPUT_ID)
            .with_placeholder(LINK_PLACEHOLDER)
            .with_accessible_name(LINK_LABEL);
        url.set_focus(true);
        Self {
            rect: Rect::ZERO,
            url,
            url_node: None,
            last_url: String::new(),
            parsed: None,
            parse_error: None,
            url_rect: Rect::ZERO,
            accept_rect: Rect::ZERO,
            decline_rect: Rect::ZERO,
            accept_hovered: false,
            decline_hovered: false,
            last_accepted_event: None,
            accept_error: None,
            last_cursor: Point::ZERO,
            layout_theme: None,
            scroll: BodyScroll::default(),
            body_height: 0.0,
        }
    }

    pub fn set_url(&mut self, url: impl Into<String>) {
        let url = url.into();
        self.url.set_text(&url);
        self.url.set_cursor_pos(url.len());
        self.parse();
    }

    pub fn url(&self) -> &str {
        self.url.value()
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
        self.last_url.clear();
        self.last_url.push_str(self.url.value());
        self.last_accepted_event = None;
        self.accept_error = None;
        if self.url.value().is_empty() {
            self.parsed = None;
            self.parse_error = None;
            return;
        }
        match InviteEnvelope::from_url(self.url.value().trim()) {
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
    ///
    /// # Errors
    ///
    /// Returns [`IdentityFlowError::DecodeFailed`] if nothing has been
    /// successfully parsed yet (see [`Self::parse`]), or any error
    /// [`build_attestation_from_invite`] returns for the parsed envelope.
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
        Ok(self.last_accepted_event.insert(event))
    }

    pub fn decline(&mut self) {
        self.parsed = None;
        self.url.set_text("");
        self.last_url.clear();
        self.parse_error = None;
        self.last_accepted_event = None;
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
        let handled = &mut *ctx.handled;
        let field_node = self.url_node.unwrap_or(ctx.node_id);
        let mut forward = |url: &mut TextInput, handled: &mut bool| {
            url.handle_event(
                event,
                &mut crate::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: ctx.modifiers,
                    state: ctx.state,
                    path: ctx.path,
                    handled,
                    focus_target: ctx.focus_target,
                    node_id: field_node,
                    taffy: ctx.taffy,
                },
            );
        };
        // Set after the match, once `forward` has released `focus_target`:
        // a click in the field tells the host the field's node holds focus
        // even when the field already believed it did (it starts focused).
        let mut claim_focus = false;
        let outcome = match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = p;
                let a = self.accept_rect.contains(p) && self.parsed.is_some();
                let d = self.decline_rect.contains(p);
                if a != self.accept_hovered || d != self.decline_hovered {
                    self.accept_hovered = a;
                    self.decline_hovered = d;
                    *handled = true;
                }
                forward(&mut self.url, handled);
                ScreenOutcome::Stay
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.url_rect.contains(self.last_cursor) {
                    self.url.set_focus(true);
                    forward(&mut self.url, handled);
                    claim_focus = true;
                    *handled = true;
                } else if self.decline_rect.contains(self.last_cursor) {
                    self.decline();
                    *handled = true;
                    return ScreenOutcome::Back;
                }
                // Accept needs the invitee pubkey, which only the shell has — it
                // calls `accept()` directly, and the container catches Advance and
                // consults `last_accepted_event` for the result.
                else if self.accept_rect.contains(self.last_cursor) && self.parsed.is_some() {
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
                        ..
                    },
                ..
            } => {
                if self.scroll.key(logical_key, self.body_height) {
                    *handled = true;
                    return ScreenOutcome::Stay;
                }
                if matches!(logical_key, Key::Named(NamedKey::Enter)) {
                    *handled = true;
                    self.parse();
                    return ScreenOutcome::Stay;
                }
                forward(&mut self.url, handled);
                ScreenOutcome::Stay
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.scroll.wheel(delta) {
                    *handled = true;
                }
                ScreenOutcome::Stay
            }
            // Key releases, IME preedit/commit, modifiers: the field's.
            _ => {
                forward(&mut self.url, handled);
                ScreenOutcome::Stay
            }
        };
        if claim_focus {
            *ctx.focus_target = Some(field_node);
        }
        self.reparse_if_edited();
        outcome
    }

    /// Re-parse after any edit that landed in the field — typed, pasted,
    /// IME-committed or set by an accessibility action. A caret or
    /// selection move alone is not an edit and leaves the parse alone.
    fn reparse_if_edited(&mut self) {
        if edited(&mut self.url, &mut self.last_url) {
            self.parse();
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
        // An accessibility `SetValue` lands outside the event path; the
        // layout pass that follows it is where the link gets re-read.
        self.reparse_if_edited();
        place_field(&mut self.url, resolved.plan.url);
        self.url_rect = BodyScroll::visible(body, resolved.plan.url);
        self.decline_rect = resolved.sheet.action(ACTION_DECLINE);
        self.accept_rect = resolved.sheet.action(ACTION_ACCEPT);
        resolved
    }

    fn actions(&self) -> [ModalAction<'static>; 2] {
        [
            ModalAction::secondary(DECLINE_LABEL).hovered(self.decline_hovered),
            ModalAction::primary(ACCEPT_LABEL)
                .enabled(self.parsed.is_some())
                .hovered(self.accept_hovered),
        ]
    }

    /// What the status row says: who invited, or why the link failed, or
    /// what to do while the field is empty.
    fn status_text(&self) -> Option<(String, Role)> {
        if self.parsed.is_some() {
            return None;
        }
        match &self.parse_error {
            Some(err) => Some((err.clone(), Role::Error)),
            None => Some((EMPTY_HINT.to_string(), Role::Caption)),
        }
    }

    fn banner_text(&self) -> Option<(String, String)> {
        let env = self.parsed.as_ref()?;
        let banner = format!(
            "{} invited you to #{} as {:?}",
            env.inviter_label, env.channel_name, env.role
        );
        let when = chrono::DateTime::<chrono::Utc>::from_timestamp(
            i64::try_from(env.expires_unix).unwrap_or(i64::MAX),
            0,
        )
        .unwrap_or_default()
        .to_rfc3339();
        Some((banner, format!("Expires {when}")))
    }

    /// Walk the body rows once: the link field, then what the link means.
    fn plan(
        &self,
        mut rows: Rows,
        control_h: f64,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> Plan {
        let sp = &theme.tokens.spacing;
        let width = rows.width();
        let link_label = rows.row(line_height(text_renderer, theme, Role::Label));
        rows.gap(sp.s1);
        let url = rows.row(control_h);
        rows.gap(sp.s6);
        let (mut banner, mut expires, mut status) = (None, None, None);
        if let Some((banner_text, _)) = self.banner_text() {
            banner = Some(rows.row(paragraph_height(
                text_renderer,
                theme,
                Role::Value,
                &banner_text,
                width,
            )));
            rows.gap(sp.s1);
            expires = Some(rows.row(line_height(text_renderer, theme, Role::Caption)));
        } else if let Some((text, role)) = self.status_text() {
            status = Some(rows.row(paragraph_height(text_renderer, theme, role, &text, width)));
        }
        let error = self.accept_error.as_deref().map(|err| {
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
            link_label,
            url,
            banner,
            expires,
            status,
            error,
            bottom: rows.bottom(),
        }
    }
}

impl Widget for InviteAcceptScreen {
    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = Some(theme.clone());
        // The field is a child node of its own — placed by the sheet, not
        // by taffy (`Display::None`), but present in the tree so the host's
        // focus order and the accessibility tree line up with `children()`.
        let field = taffy.leaf_node(Style {
            display: Display::None,
            ..Default::default()
        });
        node_map.insert(field, std::ptr::from_mut::<dyn Widget>(&mut self.url));
        self.url_node = Some(field);
        // The screen owns the viewport, so the sheet centres on the window
        // rather than inside a fixed box at the origin.
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[field],
        );
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
        presence: &PresenceCtx,
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
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            plan.link_label,
            LINK_LABEL,
            Role::Label,
            Align::Start,
            false,
        );
        self.url
            .draw(scene, transform, text_renderer, theme, presence);
        if let Some((banner, expires)) = self.banner_text() {
            if let Some(rect) = plan.banner {
                paint_text(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    rect,
                    &banner,
                    Role::Value,
                    Align::Start,
                    true,
                );
            }
            if let Some(rect) = plan.expires {
                paint_text(
                    &mut PaintCtx::new(scene, transform, text_renderer, theme),
                    rect,
                    &expires,
                    Role::Caption,
                    Align::Start,
                    false,
                );
            }
        } else if let (Some(rect), Some((text, role))) = (plan.status, self.status_text()) {
            paint_text(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                &text,
                role,
                Align::Start,
                true,
            );
        }
        if let (Some(rect), Some(err)) = (plan.error, self.accept_error.as_deref()) {
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
    }

    fn name(&self) -> &'static str {
        "InviteAcceptScreen"
    }

    fn test_id(&self) -> Option<&str> {
        Some("identity_flow_invite_accept")
    }

    /// The field is the screen's one child, in the order `build_layout`
    /// minted its node.
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.url]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.url]
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.url)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Widget::ime_purpose(&self.url)
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
    use automerge::AutoCommit;
    use collab_types::{ChannelId, Role as ChannelRole, generate_keypair};

    fn fixture_url(now: SystemTime) -> String {
        use crate::widget::identity_flow::mint_invite;
        let (sk, pk) = generate_keypair();
        let env = mint_invite(
            ChannelId::new(),
            "engineering",
            pk,
            &sk,
            "Alice",
            ChannelRole::Editor,
            now.checked_add(Duration::from_secs(3600)).unwrap_or(now),
        )
        .unwrap();
        env.to_url().unwrap()
    }

    #[test]
    fn invite_accept_parses_verifies_and_surfaces_link_errors() {
        fn pasting_valid_url_populates_parsed() {
            let url = fixture_url(crate::time::wall());
            let mut s = InviteAcceptScreen::new();
            s.set_url(&url);
            assert!(s.parsed().is_some(), "parse error: {:?}", s.parse_error());
            assert_eq!(s.parsed().unwrap().role, ChannelRole::Editor);
        }

        fn pasting_garbage_url_surfaces_error() {
            let mut s = InviteAcceptScreen::new();
            s.set_url("https://example.com/not-an-invite");
            assert!(s.parsed().is_none());
            assert!(s.parse_error().is_some());
        }

        fn tampered_url_fails_signature() {
            let url = fixture_url(crate::time::wall());
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

        /// An edit that lands in the shared field through any door — here the
        /// accessibility `SetValue` an agent or screen reader uses — re-parses
        /// the link on the next layout pass, exactly as typing did.
        fn an_edit_through_the_shared_field_reparses_the_link() {
            let url = fixture_url(crate::time::wall());
            let mut s = InviteAcceptScreen::new();
            assert!(s.parsed().is_none());
            let mut doc = AutoCommit::new();
            assert!(Widget::perform_action(
                &mut s.url,
                &crate::a11y::Action::SetValue(url.clone()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(s.url(), url, "the value landed in the shared field");
            assert!(
                s.parsed().is_none(),
                "not yet re-read: no layout pass has run"
            );
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map = HashMap::new();
            let node = s.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(1280.0),
                        height: taffy::AvailableSpace::Definite(800.0),
                    },
                )
                .expect("layout");
            s.apply_layout(&taffy, node, Point::ORIGIN);
            assert!(
                s.parsed().is_some(),
                "the layout pass re-parsed the edited link"
            );
            let ids: Vec<&str> = s.children().iter().filter_map(|c| c.test_id()).collect();
            assert_eq!(ids, vec![URL_INPUT_ID]);
            assert!(
                node_map.contains_key(&s.url_node.expect("field node")),
                "the field is registered for the host's focus and semantics"
            );
        }

        /// Empty, invalid and valid links each get a designed row under the
        /// field, and the field is one control tall on the body's full width.
        fn plan_has_a_status_row_for_every_link_state() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let body = Rect::new(100.0, 200.0, 620.0, 200.0);
            let mut s = InviteAcceptScreen::new();
            let empty = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
            assert_eq!(empty.url.height(), 34.0);
            assert_eq!((empty.url.x0, empty.url.x1), (body.x0, body.x1));
            assert!(empty.status.is_some() && empty.banner.is_none());

            s.set_url("https://example.com/not-an-invite");
            let bad = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
            assert!(bad.status.is_some(), "a parse error has a row");

            s.set_url(fixture_url(crate::time::wall()));
            let good = s.plan(Rows::new(body), 34.0, &mut tr, &theme);
            let banner = good.banner.expect("a valid link shows its banner");
            let expires = good.expires.expect("and its expiry");
            assert!(banner.y0 > good.url.y1);
            assert!((expires.y0 - banner.y1 - f64::from(theme.tokens.spacing.s1)).abs() < 1e-9);
            assert!(good.status.is_none());
        }

        pasting_valid_url_populates_parsed();
        pasting_garbage_url_surfaces_error();
        tampered_url_fails_signature();
        an_edit_through_the_shared_field_reparses_the_link();
        plan_has_a_status_row_for_every_link_state();
    }

    #[test]
    fn invite_accept_builds_attestation_or_declines() {
        fn accept_builds_attestation_event() {
            let url = fixture_url(crate::time::wall());
            let mut s = InviteAcceptScreen::new();
            s.set_url(&url);
            let (_invitee_sk, invitee_pk) = generate_keypair();
            let event = s
                .accept(invitee_pk, crate::time::wall())
                .expect("accept must succeed")
                .clone();
            match event {
                ChannelEvent::AttestationIssued(ra) => {
                    assert_eq!(ra.attestation.subject.as_bytes(), invitee_pk.as_bytes());
                }
                _ => panic!("expected AttestationIssued"),
            }
        }

        fn decline_clears_state() {
            let url = fixture_url(crate::time::wall());
            let mut s = InviteAcceptScreen::new();
            s.set_url(&url);
            s.decline();
            assert!(s.parsed().is_none());
            assert!(s.url().is_empty());
        }

        accept_builds_attestation_event();
        decline_clears_state();
    }
}
