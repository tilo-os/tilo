//! Shell-owned presentation state for the workspace-settings lens.
//!
//! Policy mutations deliberately remain in the overlay host: this component
//! reports typed hits, while the host applies them through the existing
//! authenticated `Workspace` mutation methods.

use std::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use ui::ColorCompat;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::shell::chrome_state::{Chrome, WsTab};
use crate::shell::overlay_host::modal_transition_curve;
use crate::shell::state::{CapRole, UserId, WorkspaceTree};
use crate::shell::workspace_settings::{
    DefaultAppsCtx, agents_add_rect, agents_pane_rows, default_app_rows, draw_agents_pane,
    draw_default_apps_pane, draw_members_pane, draw_permissions_pane, members_pane_rows,
    ownership_action_rect, ownership_transfer_resource, permission_profiles_rect,
    permissions_pane_layout,
};

/// The "At a glance" stat tiles: the base surface inside the raised summary
/// card, with a hairline derived against the card. Where a palette makes
/// `base` and `raised` the same colour (high contrast), the lift alone
/// cannot separate the tile from its card (HIG §9.3's caveat) and the edge
/// does the job; `tests::stat_tiles_keep_an_edge_on_every_palette` holds it.
fn stat_tile_face(theme: &Theme) -> (vello::peniko::Color, vello::peniko::Color) {
    let colors = &theme.tokens.colors;
    (
        colors.surfaces.base,
        ui::tokens::colors::Colors::hairline_on(
            colors.surfaces.raised,
            ui::chrome::shell::HAIRLINE_ALPHA,
        ),
    )
}

/// The Permissions tab's grant-composer controls, as painted.
#[derive(Clone, Copy, Debug)]
pub struct GrantComposerRects {
    pub subject: Rect,
    pub capability: Rect,
    pub expiry: [Rect; 3],
    pub submit: Rect,
}

/// A semantic control selected inside the workspace-settings lens.
#[derive(Clone, Debug, PartialEq)]
pub enum EventOutcome {
    Close,
    WorkspaceDefault(desktop_policy::ResourceKind),
    UserDefault(desktop_policy::ResourceKind),
    ComposerSubject,
    ComposerCapability,
    ComposerExpiry(usize),
    ComposerSubmit,
    /// Opens the canonical reusable-role actions in global search.
    ManagePermissionProfiles,
    MemberRole(UserId),
    RemoveMember(UserId),
    /// Opens the canonical, resource-addressed ownership actions.
    ManageOwnership,
    /// Starts a named, explicit agent setup in the command palette.
    AddAgent,
    /// Opens Privacy & Security with this agent preselected as the subject.
    ReviewAgent(String),
    RemoveAgent(String),
    Revoke(String),
    SetMemberRole(UserId, CapRole),
    ConfirmRemoveMember(UserId),
    ConfirmRevoke(String),
    ReviewPermissionActivity,
    DismissMemberSheet,
    SelectTab(WsTab),
    CreateInvite,
    EditName,
    CommitName,
}

/// Keyboard operations understood by the focused member-permission sheet.
/// The host maps platform keys into this small vocabulary so presentation
/// state stays independent from winit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberSheetKey {
    Previous,
    Next,
    Activate,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MemberSheet {
    Role(UserId),
    Remove(UserId),
    Revoke(String),
}

impl MemberSheet {
    const fn action_count() -> usize {
        // Every sheet offers three explicit choices. Destructive sheets use
        // cancel, inspect history, then complete the policy-checked action;
        // the role sheet offers its three concrete roles.
        3
    }
}

/// Values the host needs to paint the lens body between the component's
/// opening and closing draw calls.
#[derive(Clone, Copy, Debug)]
pub struct DrawPlan {
    pub transform: Affine,
    layered: bool,
}

/// Transition, geometry, painted hit targets, and container chrome for the
/// workspace-settings lens.
pub struct WorkspaceSettingsOverlay {
    viewport: Rect,
    general_scroll_y: f64,
    general_text_scale: f64,
    transition: ui::Transition,
    tab_pill_rects: [Rect; 5],
    default_app_rects: Vec<(desktop_policy::ResourceKind, Rect, Rect)>,
    revoke_rects: Vec<(String, Rect)>,
    grant_composer_rects: Option<GrantComposerRects>,
    permission_profiles_rect: Rect,
    member_control_rects: Vec<(UserId, Rect, Rect)>,
    ownership_action_rect: Rect,
    agent_add_rect: Rect,
    agent_control_rects: Vec<(String, Rect, Rect)>,
    member_sheet: Option<MemberSheet>,
    member_sheet_focus: usize,
    member_sheet_option_rects: Vec<Rect>,
}

impl Default for WorkspaceSettingsOverlay {
    fn default() -> Self {
        Self {
            viewport: Rect::ZERO,
            general_scroll_y: 0.0,
            general_text_scale: 1.0,
            transition: ui::Transition::closed(),
            tab_pill_rects: [Rect::ZERO; 5],
            default_app_rects: Vec::new(),
            revoke_rects: Vec::new(),
            grant_composer_rects: None,
            permission_profiles_rect: Rect::ZERO,
            member_control_rects: Vec::new(),
            ownership_action_rect: Rect::ZERO,
            agent_add_rect: Rect::ZERO,
            agent_control_rects: Vec::new(),
            member_sheet: None,
            member_sheet_focus: 0,
            member_sheet_option_rects: Vec::new(),
        }
    }
}

impl WorkspaceSettingsOverlay {
    pub fn layout(&mut self, viewport: Rect) {
        self.viewport = viewport;
    }

    pub fn rect(&self) -> Rect {
        Self::rect_for(self.viewport)
    }

    pub fn rect_for(viewport: Rect) -> Rect {
        // This lens is a floating application surface, not edge chrome.  A
        // 12 px inset made its outline and footer read as if they were glued
        // to the monitor bezel, especially at 1x laptop density.  Keep a
        // conventional 20 px breathing edge on every output while retaining
        // the responsive width calculation below.
        let margin = 20.0;
        let top = viewport.y0 + f64::from(crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX) + margin;
        let bottom = (viewport.y1 - margin).max(top + 320.0);
        if viewport.width() < 820.0 {
            return Rect::new(viewport.x0 + margin, top, viewport.x1 - margin, bottom);
        }
        // Keep the workspace itself legible beside this contextual lens. A
        // 620px floor consumed half of a 1280px laptop even when the pane's
        // responsive tab layout no longer needed a sidebar.
        let lens_w = (viewport.width() * 0.42).clamp(540.0, 720.0);
        let right = viewport.x1 - margin;
        Rect::new(right - lens_w, top, right, bottom)
    }

    /// The lens header keeps one rhythm (HIG §7b): `s4` from the panel's
    /// top to the identity card, `s4` from that card to the tab strip, and
    /// `s4` from the strip to the first content card. The body rect starts
    /// `s2` under the strip; every tab's first card or pane pad supplies the
    /// other `s2` (`PANE_PAD_TOP`, General's `8 × text_scale` lead-in).
    const HEADER_PAD: f64 = ui::tokens::Spacing::BASE.s4 as f64;
    const BODY_LEAD: f64 = ui::tokens::Spacing::BASE.s2 as f64;
    /// The identity mark is a `min_pointer_target` glyph tile grown by one
    /// `s4`; the card is that tile with `s4` above and below, and its text
    /// column starts `s4 + mark + s3` in.
    const IDENTITY_MARK: f64 =
        (ui::tokens::Metrics::BASE.min_pointer_target + ui::tokens::Spacing::BASE.s4) as f64;
    const IDENTITY_H: f64 = Self::IDENTITY_MARK + 2.0 * Self::HEADER_PAD;
    /// Tab pills are one pointer target tall inside a track that insets
    /// them by `s1` on every side.
    const PILL_INSET: f64 = ui::tokens::Spacing::BASE.s1 as f64;
    const NAV_H: f64 = ui::tokens::Metrics::BASE.min_pointer_target as f64 + 2.0 * Self::PILL_INSET;

    /// The identity card at the head of the lens.
    pub fn identity_rect_for(card: Rect) -> Rect {
        Rect::new(
            card.x0 + Self::HEADER_PAD,
            card.y0 + Self::HEADER_PAD,
            card.x1 - Self::HEADER_PAD,
            card.y0 + Self::HEADER_PAD + Self::IDENTITY_H,
        )
    }

    /// The tab strip's track, one `s4` under the identity card.
    pub fn nav_rect_for(card: Rect) -> Rect {
        let identity = Self::identity_rect_for(card);
        Rect::new(
            card.x0 + Self::HEADER_PAD,
            identity.y1 + Self::HEADER_PAD,
            card.x1 - Self::HEADER_PAD,
            identity.y1 + Self::HEADER_PAD + Self::NAV_H,
        )
    }

    pub fn body_rect_for(card: Rect) -> Rect {
        let nav = Self::nav_rect_for(card);
        Rect::new(
            card.x0 + Self::HEADER_PAD,
            nav.y1 + Self::BODY_LEAD,
            card.x1 - Self::HEADER_PAD,
            // Reserve a real footer band for the global close hint.  Letting
            // tab content extend to y1-16 painted empty states, policy copy,
            // and the hint into the same last line on compact laptop panels.
            card.y1 - 44.0,
        )
    }

    fn body_rect(card: Rect) -> Rect {
        Self::body_rect_for(card)
    }

    fn draw_ws_settings_close_hint(
        scene: &mut Scene,
        transform: Affine,
        card: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let hint = "Press Esc to close";
        let size = theme.tokens.typography.scale.xs;
        let (width, _) = text_renderer.measure(hint, size, None);
        text_renderer.draw_text(
            scene,
            hint,
            size,
            theme.tokens.colors.text_dim,
            transform.pre_translate((card.x1 - f64::from(width) - 24.0, card.y1 - 24.0).into()),
            None,
        );
    }

    pub fn general_body_rect(&self) -> Rect {
        Self::body_rect_for(self.rect())
    }

    pub fn general_summary_rect(&self) -> Rect {
        Self::summary_card_rect_for(
            self.general_body_rect(),
            self.general_text_scale,
            self.general_scroll_y,
        )
    }

    /// The "At a glance" card of the General tab: its authored place under
    /// the identity and sharing cards, scaled with the body's text and
    /// shifted by the body's scroll. One definition for hit-testing and
    /// painting.
    pub fn summary_card_rect_for(body: Rect, text_scale: f64, scroll_y: f64) -> Rect {
        Rect::new(
            body.x0,
            body.y0 + 268.0 * text_scale - scroll_y,
            body.x1,
            body.y0 + 374.0 * text_scale - scroll_y,
        )
    }

    /// Scroll only General's body; headers, tabs and footer remain anchored.
    pub fn scroll_general(&mut self, delta: f64) -> bool {
        if !delta.is_finite() || self.member_sheet.is_some() {
            return false;
        }
        let max = (382.0 * self.general_text_scale - self.general_body_rect().height()).max(0.0);
        let old = self.general_scroll_y;
        self.general_scroll_y = (old + delta).clamp(0.0, max);
        (old - self.general_scroll_y).abs() > f64::EPSILON
    }

    pub fn name_rect(&self) -> Rect {
        let body = self.general_body_rect();
        Rect::new(
            body.x0 + 16.0,
            body.y0 + 62.0 * self.general_text_scale - self.general_scroll_y,
            body.x1 - 16.0,
            body.y0 + 96.0 * self.general_text_scale - self.general_scroll_y,
        )
    }

    pub fn name_rect_for(card: Rect) -> Rect {
        let body = Self::body_rect_for(card);
        Rect::new(
            body.x0 + 16.0,
            body.y0 + 62.0,
            body.x1 - 16.0,
            body.y0 + 96.0,
        )
    }

    pub fn close_rect(&self) -> Rect {
        Self::close_rect_for(self.rect())
    }

    pub fn close_rect_for(rect: Rect) -> Rect {
        // One pointer target (HIG §9.6: 32 × 32 minimum), `s3` inside the
        // identity card's trailing edge and centred on its centre line — it
        // was pinned 24 px from the panel top, 14 px above that line.
        let identity = Self::identity_rect_for(rect);
        let target = ui::tokens::Metrics::BASE.min_pointer_target as f64;
        let inset = ui::tokens::Spacing::BASE.s3 as f64;
        let cy = identity.center().y;
        Rect::new(
            identity.x1 - inset - target,
            cy - target / 2.0,
            identity.x1 - inset,
            cy + target / 2.0,
        )
    }

    pub fn invite_rect(&self) -> Rect {
        let body = self.general_body_rect();
        let y = body.y0 + 202.0 * self.general_text_scale - self.general_scroll_y;
        Rect::new(
            body.x1 - 16.0 - 72.0 * self.general_text_scale,
            y,
            body.x1 - 16.0,
            y + 34.0 * self.general_text_scale,
        )
    }

    pub fn invite_rect_for(card: Rect) -> Rect {
        let body = Self::body_rect_for(card);
        let link_y = body.y0 + 202.0;
        Rect::new(body.x1 - 88.0, link_y, body.x1 - 16.0, link_y + 34.0)
    }

    pub fn force_settled(&mut self) {
        self.transition = ui::Transition::open();
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.transition.is_animating(now)
    }

    pub fn tab_pill_rect(&self, tab: WsTab) -> Rect {
        self.tab_pill_rects
            .get(match tab {
                WsTab::General => 0,
                WsTab::Members => 1,
                WsTab::Agents => 2,
                WsTab::Permissions => 3,
                WsTab::Apps => 4,
            })
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    pub fn revoke_rects(&self) -> &[(String, Rect)] {
        &self.revoke_rects
    }

    pub fn grant_composer_rects(&self) -> Option<&GrantComposerRects> {
        self.grant_composer_rects.as_ref()
    }

    pub fn member_control_rects(&self) -> &[(UserId, Rect, Rect)] {
        &self.member_control_rects
    }

    /// Bounds of the resource-addressed ownership entry point painted in the
    /// Members pane. The shell accessibility bridge uses the exact same rect
    /// for assistive-technology activation as pointer input.
    pub fn ownership_action_rect(&self) -> Rect {
        self.ownership_action_rect
    }

    /// Button bounds from the currently focused role / confirmation sheet.
    /// Kept primarily for end-to-end pointer coverage; the bounds are still
    /// produced by the same draw pass as the visible controls.
    pub fn member_sheet_option_rects(&self) -> &[Rect] {
        &self.member_sheet_option_rects
    }

    pub fn member_sheet_is_open(&self) -> bool {
        self.member_sheet.is_some()
    }

    pub fn open_member_role_sheet(&mut self, user: UserId, current: CapRole) {
        self.member_sheet = Some(MemberSheet::Role(user));
        self.member_sheet_focus = [CapRole::Viewer, CapRole::Commenter, CapRole::Editor]
            .iter()
            .position(|role| *role == current)
            .unwrap_or(0);
    }

    pub fn open_remove_member_sheet(&mut self, user: UserId) {
        self.member_sheet = Some(MemberSheet::Remove(user));
        self.member_sheet_focus = 0;
    }

    pub fn open_revoke_sheet(&mut self, cap_id: String) {
        self.member_sheet = Some(MemberSheet::Revoke(cap_id));
        self.member_sheet_focus = 0;
    }

    pub fn dismiss_member_sheet(&mut self) {
        self.member_sheet = None;
        self.member_sheet_focus = 0;
        self.member_sheet_option_rects.clear();
    }

    /// Resolve an operation in the sheet's intentionally tiny keyboard
    /// vocabulary. Navigation wraps so the choice remains reachable without
    /// a pointer, and every other key is absorbed by the host while open.
    pub fn handle_member_sheet_key(&mut self, key: MemberSheetKey) -> Option<EventOutcome> {
        let sheet = self.member_sheet.clone()?;
        let count = MemberSheet::action_count();
        match key {
            MemberSheetKey::Previous => {
                self.member_sheet_focus = self
                    .member_sheet_focus
                    .checked_sub(1)
                    .unwrap_or_else(|| count.saturating_sub(1));
                None
            }
            MemberSheetKey::Next => {
                self.member_sheet_focus = self
                    .member_sheet_focus
                    .checked_add(1)
                    .filter(|next| *next < count)
                    .unwrap_or(0);
                None
            }
            MemberSheetKey::Cancel => Some(EventOutcome::DismissMemberSheet),
            MemberSheetKey::Activate => Self::member_sheet_outcome(&sheet, self.member_sheet_focus),
        }
    }

    fn member_sheet_outcome(sheet: &MemberSheet, index: usize) -> Option<EventOutcome> {
        match sheet {
            MemberSheet::Role(user) => [CapRole::Viewer, CapRole::Commenter, CapRole::Editor]
                .get(index)
                .copied()
                .map(|role| EventOutcome::SetMemberRole(user.clone(), role)),
            MemberSheet::Remove(user) => match index {
                0 => Some(EventOutcome::DismissMemberSheet),
                1 => Some(EventOutcome::ReviewPermissionActivity),
                2 => Some(EventOutcome::ConfirmRemoveMember(user.clone())),
                _ => None,
            },
            MemberSheet::Revoke(cap_id) => match index {
                0 => Some(EventOutcome::DismissMemberSheet),
                1 => Some(EventOutcome::ReviewPermissionActivity),
                2 => Some(EventOutcome::ConfirmRevoke(cap_id.clone())),
                _ => None,
            },
        }
    }

    fn clear_hits(&mut self) {
        self.tab_pill_rects = [Rect::ZERO; 5];
        self.default_app_rects.clear();
        self.revoke_rects.clear();
        self.grant_composer_rects = None;
        self.permission_profiles_rect = Rect::ZERO;
        self.member_control_rects.clear();
        self.ownership_action_rect = Rect::ZERO;
        self.agent_add_rect = Rect::ZERO;
        self.agent_control_rects.clear();
        self.member_sheet_option_rects.clear();
    }

    /// Paint the transition-owned marker, shadow and card, returning the
    /// transform under which the host paints policy-backed pane contents.
    pub fn begin_draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        open: bool,
        anchor: Option<Rect>,
    ) -> Option<DrawPlan> {
        if !open {
            self.general_scroll_y = 0.0;
            self.clear_hits();
            self.dismiss_member_sheet();
        }
        self.transition.set_open(
            open,
            if theme.reduce_motion {
                Duration::ZERO
            } else {
                // A Panel opens over `durations.base` (HIG §9.7), read from
                // the motion tokens rather than a per-surface constant.
                Duration::from_millis(u64::from(theme.tokens.motion.durations.base_ms))
            },
            ui::Curve::EaseOut,
        );
        let now = Instant::now();
        if !self.transition.is_visible(now) {
            return None;
        }
        let alpha = self.transition.value(now);
        let in_flight = alpha < 1.0;
        let rect = self.rect();
        if !theme.reduce_motion
            && in_flight
            && let Some(anchor) = anchor.filter(|anchor| anchor.area() > 0.0)
        {
            let start = anchor.center();
            let end = Point::new(rect.x0 + 28.0, rect.y0 + 24.0);
            let t = f64::from(alpha);
            let point = Point::new(
                start.x + (end.x - start.x) * t,
                start.y + (end.y - start.y) * t,
            );
            let pulse_alpha = (std::f32::consts::PI * alpha).sin().max(0.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.accent[5].with_alpha_factor(pulse_alpha),
                None,
                &RoundedRect::from_rect(
                    Rect::new(point.x - 14.0, point.y - 2.0, point.x + 14.0, point.y + 2.0),
                    2.0,
                ),
            );
        }
        if in_flight {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                transform,
                &self.viewport,
            );
        }
        // The one Panel entrance (HIG §9.7): opacity, a short rise and a
        // scale-up about the sheet's centre, shared with the other modal
        // surfaces through `modal_transition_curve` — not a bespoke 48 px
        // slide in from the right. Reduced motion keeps the end state.
        let (_, dy, scale) = if theme.reduce_motion {
            (alpha, 0.0, 1.0)
        } else {
            modal_transition_curve(&self.transition, now, 0.95)
        };
        let centre = rect.center();
        let panel_transform = transform
            .pre_translate((centre.x, centre.y + dy).into())
            .pre_scale(scale)
            .pre_translate((-centre.x, -centre.y).into());
        // Panel role (HIG §9.1–§9.3): the Panel radius, an elevation-token
        // shadow that follows the sheet's own corners and fades with it,
        // `panel_ground` and the derived Panel border.
        let radius = ui::chrome::ChromeRole::Panel.radius(theme, rect.height());
        ui::chrome::shell::panel_shadow(scene, panel_transform, rect, radius, theme, alpha);
        let card = RoundedRect::from_rect(rect, radius);
        scene.fill(
            Fill::NonZero,
            panel_transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &card,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            panel_transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &card,
        );
        Some(DrawPlan {
            transform: panel_transform,
            layered: in_flight,
        })
    }

    pub fn finish_draw(scene: &mut Scene, plan: DrawPlan) {
        if plan.layered {
            scene.pop_layer();
        }
    }

    /// Resolve a click to one semantic outcome. The host remains the sole
    /// owner of authorization and mutation side effects.
    pub fn hit_test(&self, point: Point, active_tab: WsTab) -> EventOutcome {
        if let Some(sheet) = &self.member_sheet {
            if let Some(index) = self
                .member_sheet_option_rects
                .iter()
                .position(|rect| rect.contains(point))
            {
                return Self::member_sheet_outcome(sheet, index)
                    .unwrap_or(EventOutcome::DismissMemberSheet);
            }
            // This is a modal decision, so a stray click cannot alter a
            // member behind it. Outside-click follows the standard sheet
            // convention and abandons the pending choice.
            return EventOutcome::DismissMemberSheet;
        }
        if self.close_rect().contains(point) {
            return EventOutcome::Close;
        }
        for (resource, workspace, user) in &self.default_app_rects {
            if workspace.area() > 0.0 && workspace.contains(point) {
                return EventOutcome::WorkspaceDefault(resource.clone());
            }
            if user.area() > 0.0 && user.contains(point) {
                return EventOutcome::UserDefault(resource.clone());
            }
        }
        if let Some(composer) = self.grant_composer_rects {
            if composer.subject.area() > 0.0 && composer.subject.contains(point) {
                return EventOutcome::ComposerSubject;
            }
            if composer.capability.contains(point) {
                return EventOutcome::ComposerCapability;
            }
            if composer.submit.area() > 0.0 && composer.submit.contains(point) {
                return EventOutcome::ComposerSubmit;
            }
            for (index, rect) in composer.expiry.iter().enumerate() {
                if rect.contains(point) {
                    return EventOutcome::ComposerExpiry(index);
                }
            }
        }
        if matches!(active_tab, WsTab::Permissions)
            && self.permission_profiles_rect.area() > 0.0
            && self.permission_profiles_rect.contains(point)
        {
            return EventOutcome::ManagePermissionProfiles;
        }
        for (user, role, remove) in &self.member_control_rects {
            if role.area() > 0.0 && role.contains(point) {
                return EventOutcome::MemberRole(user.clone());
            }
            if remove.area() > 0.0 && remove.contains(point) {
                return EventOutcome::RemoveMember(user.clone());
            }
        }
        if self.ownership_action_rect.area() > 0.0 && self.ownership_action_rect.contains(point) {
            return EventOutcome::ManageOwnership;
        }
        if matches!(active_tab, WsTab::Agents)
            && self.agent_add_rect.area() > 0.0
            && self.agent_add_rect.contains(point)
        {
            return EventOutcome::AddAgent;
        }
        for (agent, review, remove) in &self.agent_control_rects {
            if review.area() > 0.0 && review.contains(point) {
                return EventOutcome::ReviewAgent(agent.clone());
            }
            if remove.area() > 0.0 && remove.contains(point) {
                return EventOutcome::RemoveAgent(agent.clone());
            }
        }
        for (id, rect) in &self.revoke_rects {
            if rect.area() > 0.0 && rect.contains(point) {
                return EventOutcome::Revoke(id.clone());
            }
        }
        for (index, rect) in self.tab_pill_rects.iter().enumerate() {
            if rect.area() > 0.0
                && rect.contains(point)
                && let Some(tab) = WsTab::all().get(index)
            {
                return EventOutcome::SelectTab(*tab);
            }
        }
        if matches!(active_tab, WsTab::General) && !self.general_body_rect().contains(point) {
            return EventOutcome::CommitName;
        }
        if matches!(active_tab, WsTab::General) && self.invite_rect().contains(point) {
            return EventOutcome::CreateInvite;
        }
        if self.name_rect().contains(point) {
            EventOutcome::EditName
        } else {
            EventOutcome::CommitName
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "tree/state feed the per-tab pane bodies and the General tab's tail; \
                  cloning Workspace to narrow the guard further isn't worth the paint-path allocation"
    )]
    pub fn draw_contents(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        chrome: &Chrome,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;

        // `ws_settings_rect()` is the single source of truth for the
        // centered-modal geometry; hit-tests reach for the same helper
        // so a width bump can't desync the card vs. the clickable bounds.
        let rect = self.rect();
        // Borrowed only once the header's geometry (which needs no chrome
        // state at all) is already painted, and read again below for the
        // per-tab pane bodies and — on the General tab — the invite URL
        // and collaborator tile near the end of the function; those need
        // a live `&Workspace`, so cloning it to shrink the guard further
        // would add an allocation to a hot paint path for no real gain.
        let state = chrome.borrow();
        let tree = state.tree();

        let active_name = self.draw_ws_settings_header_text(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            &state,
            &tree,
        );

        // Reads the typed `ws_settings_tab` field on chrome state, kept
        // orthogonal to the name-edit buffer.
        let active_tab = state.ws_settings_tab;
        self.draw_ws_settings_tab_strip(scene, transform, text_renderer, theme, rect, active_tab);

        // Members, Permissions and Apps each own their whole body and
        // paint it here; General is the fall-through into the rest of
        // this function.
        let fully_painted = self.draw_ws_settings_dispatch_tab(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            active_tab,
            &state,
            &tree,
        );

        if fully_painted {
            self.draw_member_sheet(scene, transform, text_renderer, theme, rect, &tree);
            return;
        }

        // Unreachable today (every non-General arm returns `true` above),
        // but guards against a new `WsTab` variant falling through to
        // paint the General body over its pane.
        if !matches!(active_tab, WsTab::General) {
            return;
        }

        self.general_text_scale = f64::from(theme.text_scale);
        self.scroll_general(0.0);
        let body_clip = self.general_body_rect();
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &body_clip,
        );
        let content_rect = Rect::new(
            rect.x0,
            rect.y0 - self.general_scroll_y,
            rect.x1,
            rect.y1 - self.general_scroll_y,
        );
        let body_y = rect.y0 + 168.0;
        let editing = self.draw_ws_settings_name_input(
            scene,
            transform,
            text_renderer,
            theme,
            content_rect,
            &state,
            active_name.as_str(),
            body_y,
        );
        let can_invite = tree.active_workspace().is_some_and(|workspace| {
            tree.governs_room(workspace.room) && workspace.may_administer(&state.local_user_id)
        });
        let invite_y = self.draw_ws_settings_invite_link(
            scene,
            transform,
            text_renderer,
            theme,
            content_rect,
            can_invite,
            body_y,
        );
        let collab_y = invite_y + 72.0;
        Self::draw_ws_settings_collaborators(
            scene,
            transform,
            text_renderer,
            theme,
            content_rect,
            &tree,
            collab_y,
        );

        scene.pop_layer();

        let hint = if editing {
            "Enter to save · Esc to cancel"
        } else {
            "Press Esc to close"
        };
        let hsize = tokens.typography.scale.xs;
        let (hw, _hh) = text_renderer.measure(hint, hsize, None);
        text_renderer.draw_text(
            scene,
            hint,
            hsize,
            colors.text_dim,
            transform.pre_translate((rect.x1 - f64::from(hw) - 24.0, rect.y1 - 24.0).into()),
            None,
        );
        self.draw_member_sheet(scene, transform, text_renderer, theme, rect, &tree);
    }

    /// Paint the focused decision surface over the settings lens. The outer
    /// modal remains visible for context, while this compact sheet makes the
    /// exact permission being changed and its consequences impossible to miss.
    #[allow(
        clippy::too_many_lines,
        reason = "the focused permission sheet is one compact renderer whose geometry, hit targets, focus treatment, and exact security consequences must stay in lockstep"
    )]
    fn draw_member_sheet(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        settings_rect: Rect,
        tree: &WorkspaceTree,
    ) {
        let Some(sheet) = self.member_sheet.clone() else {
            self.member_sheet_option_rects.clear();
            return;
        };
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let role_sheet = matches!(sheet, MemberSheet::Role(_));
        let height = if role_sheet { 326.0 } else { 300.0 };
        let width = (settings_rect.width() - 48.0).clamp(330.0, 440.0);
        let card = Rect::new(
            settings_rect.center().x - width / 2.0,
            settings_rect.center().y - height / 2.0,
            settings_rect.center().x + width / 2.0,
            settings_rect.center().y + height / 2.0,
        );
        // The scrim behind a modal Panel is the scrim token (HIG §9.2), on
        // the lens's own corner so it does not leak past the sheet it dims.
        let backdrop = RoundedRect::from_rect(
            settings_rect,
            ui::chrome::ChromeRole::Panel.radius(theme, settings_rect.height()),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.scrim,
            None,
            &backdrop,
        );
        // Panel role (HIG §9.1–§9.3): Panel radius, elevation-token shadow,
        // `panel_ground` and the derived border — not a sprite shadow off a
        // ramp slot over a declared border.
        let sheet_radius = ui::chrome::ChromeRole::Panel.radius(theme, card.height());
        let surface = RoundedRect::from_rect(card, sheet_radius);
        ui::chrome::shell::panel_shadow(scene, transform, card, sheet_radius, theme, 1.0);
        scene.fill(
            Fill::NonZero,
            transform,
            ui::chrome::shell::panel_ground(theme),
            None,
            &surface,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &surface,
        );

        let (title, description, subject): (&str, String, Option<String>) = match &sheet {
            MemberSheet::Role(user) => {
                let current = tree
                    .active_workspace()
                    .and_then(|workspace| workspace.role_of(user))
                    .map_or("Viewer", crate::shell::workspace_settings::role_label);
                (
                    "Choose workspace access",
                    format!(
                        "{} is currently {current}. Choose exactly what they can do.",
                        crate::shell::workspace_settings::short_id(user)
                    ),
                    None,
                )
            }
            MemberSheet::Remove(user) => (
                "Remove this person?",
                "They will stop receiving future sync and edits. Downloaded copies remain on their devices."
                    .to_string(),
                Some(crate::shell::workspace_settings::short_id(user)),
            ),
            MemberSheet::Revoke(cap_id) => (
                "Revoke this permission?",
                "The selected capability will stop applying immediately. Review its activity first if you are unsure."
                    .to_string(),
                Some(crate::shell::workspace_settings::short_id(cap_id)),
            ),
        };
        text_renderer.draw_text(
            scene,
            title,
            tokens.typography.scale.lg,
            colors.text_primary,
            transform.pre_translate((card.x0 + 20.0, card.y0 + 20.0).into()),
            None,
        );
        if let Some(subject) = subject {
            text_renderer.draw_text(
                scene,
                &subject,
                tokens.typography.scale.sm,
                colors.accent[4],
                transform.pre_translate((card.x0 + 20.0, card.y0 + 48.0).into()),
                None,
            );
        }
        let desc_y = if role_sheet {
            card.y0 + 50.0
        } else {
            card.y0 + 72.0
        };
        let description = text_renderer.elide_to_width(
            &description,
            tokens.typography.scale.sm,
            f64_to_f32_lossy(card.width() - 40.0),
        );
        text_renderer.draw_text(
            scene,
            &description,
            tokens.typography.scale.sm,
            colors.text_dim,
            transform.pre_translate((card.x0 + 20.0, desc_y).into()),
            None,
        );

        self.member_sheet_option_rects.clear();
        let option_x0 = card.x0 + 20.0;
        let option_x1 = card.x1 - 20.0;
        let (labels, previews): (Vec<String>, Vec<String>) = match &sheet {
            MemberSheet::Role(_) => {
                let roles = [CapRole::Viewer, CapRole::Commenter, CapRole::Editor];
                (
                    roles
                        .iter()
                        .map(|role| crate::shell::workspace_settings::role_label(*role).to_string())
                        .collect(),
                    roles
                        .iter()
                        .map(|role| {
                            crate::shell::workspace_settings::role_capability_preview(*role)
                                .to_string()
                        })
                        .collect(),
                )
            }
            MemberSheet::Remove(_) => (
                vec![
                    "Keep access".into(),
                    "Review activity first".into(),
                    "Remove person".into(),
                ],
                vec![
                    "No changes are made.".into(),
                    "Open workspace history before deciding.".into(),
                    "This removes their workspace capabilities.".into(),
                ],
            ),
            MemberSheet::Revoke(_) => (
                vec![
                    "Keep permission".into(),
                    "Review activity first".into(),
                    "Revoke permission".into(),
                ],
                vec![
                    "No changes are made.".into(),
                    "Open workspace history before deciding.".into(),
                    "This removes the selected capability immediately.".into(),
                ],
            ),
        };
        let row_h = if role_sheet { 58.0 } else { 42.0 };
        let gap = 8.0;
        let start_y = if role_sheet {
            card.y0 + 88.0
        } else {
            card.y0 + 112.0
        };
        for (index, (label, preview)) in labels.iter().zip(&previews).enumerate() {
            let y0 = start_y + usize_to_f64_lossy(index) * (row_h + gap);
            let option = Rect::new(option_x0, y0, option_x1, y0 + row_h);
            self.member_sheet_option_rects.push(option);
            let focused = index == self.member_sheet_focus;
            let destructive = index == 2 && !role_sheet;
            let tint = if destructive {
                colors.semantic.danger
            } else {
                colors.accent[5]
            };
            let rr = RoundedRect::from_rect(option, f64::from(tokens.radii.md));
            scene.fill(
                Fill::NonZero,
                transform,
                tint.with_alpha_factor(if focused { 0.25 } else { 0.10 }),
                None,
                &rr,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(if focused { 2.0 } else { 1.0 }),
                transform,
                tint.with_alpha_factor(if focused { 0.95 } else { 0.45 }),
                None,
                &rr,
            );
            text_renderer.draw_text(
                scene,
                label,
                tokens.typography.scale.sm,
                colors.text_primary,
                transform.pre_translate((option.x0 + 12.0, option.y0 + 9.0).into()),
                None,
            );
            if role_sheet {
                text_renderer.draw_text(
                    scene,
                    preview,
                    tokens.typography.scale.xs,
                    colors.text_dim,
                    transform.pre_translate((option.x0 + 12.0, option.y0 + 29.0).into()),
                    None,
                );
            }
        }
        let hint = "↑ ↓ to choose · Enter to confirm · Esc to cancel";
        text_renderer.draw_text(
            scene,
            hint,
            tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((card.x0 + 20.0, card.y1 - 22.0).into()),
            None,
        );
    }

    /// Title plus the "Active workspace: …" line; returns the active
    /// workspace's name so the General tab's name input can fall back
    /// to it while nothing is being edited.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state/tree the \
                  name and role are read from; matching every other draw_ws_settings_* \
                  helper in this impl"
    )]
    fn draw_ws_settings_header_text(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) -> String {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let (identity, close) =
            self.draw_ws_settings_identity_card_chrome(scene, transform, theme, rect);
        if theme.text_scale > 1.0 {
            let name = tree
                .active_workspace()
                .map_or("Workspace", |workspace| workspace.name.as_str());
            let role = tree
                .active_workspace()
                .and_then(|workspace| workspace.role_of(&state.local_user_id))
                .map_or("Guest", crate::shell::workspace_settings::role_label);
            let width = f64_to_f32_lossy(close.x0 - identity.x0 - 88.0);
            let caption = text_renderer.elide_to_width(
                &format!("Workspace · {role}"),
                tokens.typography.scale.xs,
                width,
            );
            let name = text_renderer.elide_to_width(name, tokens.typography.scale.base, width);
            text_renderer.draw_text(
                scene,
                &caption,
                tokens.typography.scale.xs,
                colors.accent[4],
                transform.pre_translate((identity.x0 + 76.0, identity.y0 + 8.0).into()),
                None,
            );
            text_renderer.draw_text(
                scene,
                &name,
                tokens.typography.scale.base,
                colors.text_primary,
                transform.pre_translate((identity.x0 + 76.0, identity.y0 + 38.0).into()),
                None,
            );
            return tree
                .active_workspace()
                .map_or_else(|| "Workspace".into(), |workspace| workspace.name.clone());
        }
        text_renderer.draw_text(
            scene,
            "CURRENT WORKSPACE",
            tokens.typography.scale.xs,
            colors.accent[4],
            transform.pre_translate((identity.x0 + 76.0, identity.y0 + 12.0).into()),
            None,
        );
        let active_name = tree
            .active_workspace()
            .map_or_else(|| "Workspace".into(), |w| w.name.clone());
        let tsize = tokens.typography.scale.xl;
        let shown_name = text_renderer.elide_to_width(
            &active_name,
            tsize,
            f64_to_f32_lossy(close.x0 - (identity.x0 + 76.0) - 12.0),
        );
        text_renderer.draw_text(
            scene,
            &shown_name,
            tsize,
            colors.text_primary,
            transform.pre_translate((identity.x0 + 76.0, identity.y0 + 34.0).into()),
            None,
        );
        let active_role = tree
            .active_workspace()
            .and_then(|workspace| workspace.role_of(&state.local_user_id))
            .map_or("Guest", crate::shell::workspace_settings::role_label);
        let label = text_renderer.elide_to_width(
            &format!("{active_role} · Settings and policy stay with this workspace"),
            tokens.typography.scale.sm,
            f64_to_f32_lossy(identity.x1 - identity.x0 - 92.0),
        );
        text_renderer.draw_text(
            scene,
            &label,
            tokens.typography.scale.sm,
            colors.text_dim,
            transform.pre_translate((identity.x0 + 76.0, identity.y0 + 58.0).into()),
            None,
        );
        active_name
    }

    /// The identity card's background, room-icon mark and close button —
    /// the purely-geometric chrome of [`Self::draw_ws_settings_header_text`],
    /// split out to keep that under `too_many_lines`. Returns the
    /// identity card rect and the close-button rect, since the caller's
    /// text placement is relative to both.
    fn draw_ws_settings_identity_card_chrome(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        rect: Rect,
    ) -> (Rect, Rect) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let identity = Self::identity_rect_for(rect);
        // Card with its derived rest edge (HIG §9.5): the hairline is
        // measured against the card's own fill, so it registers on the
        // raised tier where a declared `border_standard` vanished.
        ui::surface::card(
            scene,
            transform,
            identity,
            f64::from(tokens.radii.lg),
            colors.surfaces.raised,
        );
        let mark = Rect::new(
            identity.x0 + Self::HEADER_PAD,
            identity.y0 + Self::HEADER_PAD,
            identity.x0 + Self::HEADER_PAD + Self::IDENTITY_MARK,
            identity.y1 - Self::HEADER_PAD,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5].with_alpha_factor(0.18),
            None,
            &RoundedRect::from_rect(mark, f64::from(tokens.radii.lg)),
        );
        ui::icons::draw_icon(
            scene,
            transform,
            mark.inset(14.0),
            ui::icons::Icon::LayoutGrid,
            colors.accent[4],
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let close = self.close_rect();
        // Control role (HIG §9.1): a pressable chip at the Control radius,
        // centred on the identity card's centre line, with its edge derived
        // from its own fill.
        ui::surface::card(
            scene,
            transform,
            close,
            ui::chrome::ChromeRole::Control.radius(theme, close.height()),
            colors.surfaces.base,
        );
        ui::icons::draw_icon(
            scene,
            transform,
            close.inset(8.0),
            ui::icons::Icon::X,
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        (identity, close)
    }

    /// Tab strip. Every pill is a real destination: a host who has
    /// granted nothing yet still needs to reach the tab that grants.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered paint pass keeps responsive label bounds aligned with tab hit targets"
    )]
    fn draw_ws_settings_tab_strip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        active_tab: WsTab,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let lsize = tokens.typography.scale.base;
        let tabs = WsTab::all();
        let nav = Self::nav_rect_for(rect);
        // The strip's track: a quiet lift under the Selector pills and a
        // hairline derived against the panel ground (HIG §9.2/§9.5), not a
        // ramp slot inside a faded declared border.
        let nav_rr = RoundedRect::from_rect(nav, f64::from(tokens.radii.lg));
        scene.fill(Fill::NonZero, transform, colors.lift(0.35), None, &nav_rr);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::tokens::colors::Colors::hairline_on(
                ui::chrome::shell::panel_ground(theme),
                ui::chrome::shell::HAIRLINE_ALPHA,
            ),
            None,
            &nav_rr,
        );
        let segment_w = nav.width() / usize_to_f64_lossy(tabs.len());
        // Every pill gets the same treatment: the plan is measured once for
        // the whole strip, so the strip never shows one elided label next to
        // four intact ones or one icon next to four bare labels.
        let plan = tab_strip_plan(text_renderer, lsize, segment_w - 2.0 * Self::PILL_INSET);
        // Segment widths follow the measured labels once equal fifths cannot
        // hold the widest one (the `s1` pill margins each side are part of
        // the need), so "General" at 200 % is not elided beside pills with
        // slack.
        let reserved = TAB_LABEL_INSET
            + 2.0 * Self::PILL_INSET
            + if plan.icons {
                TAB_ICON_PX + TAB_ICON_GAP
            } else {
                0.0
            };
        let needs: Vec<f64> = tabs
            .iter()
            .map(|tab| {
                let label = if plan.compact {
                    tab.compact_label()
                } else {
                    tab.label()
                };
                f64::from(text_renderer.measure(label, lsize, None).0) + reserved
            })
            .collect();
        let widths = tab_pill_widths(&needs, nav.width());
        // Written into the component's cache after the loop
        // so one source of geometry feeds draw and click-routing.
        let mut pill_rects = [Rect::ZERO; 5];
        for (i, tab) in tabs.iter().enumerate() {
            let active = *tab == active_tab;
            let label = if plan.compact {
                tab.compact_label()
            } else {
                tab.label()
            };

            let seg_x0 = nav.x0 + widths.iter().take(i).sum::<f64>();
            let seg_w = widths.get(i).copied().unwrap_or(segment_w);
            let pill_rect = Rect::new(
                seg_x0 + Self::PILL_INSET,
                nav.y0 + Self::PILL_INSET,
                seg_x0 + seg_w - Self::PILL_INSET,
                nav.y1 - Self::PILL_INSET,
            );
            if let Some(slot) = pill_rects.get_mut(i) {
                *slot = pill_rect;
            }
            // Selector role (HIG §9.1/§9.3): a capsule, and the chosen tab is
            // a lift with a derived hairline — never a declared border on a
            // raised fill, which is invisible on the dark preset.
            let pill_radius = ui::chrome::ChromeRole::Selector.radius(theme, pill_rect.height());
            let pill = RoundedRect::from_rect(pill_rect, pill_radius);
            if active {
                let tier =
                    ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(true));
                scene.fill(Fill::NonZero, transform, tier.fill, None, &pill);
                if let Some(border) = tier.border {
                    // Crisp: the hairline covers the pill's outermost pixel
                    // row, the same row every `ui::surface::card` edge uses.
                    scene.stroke(
                        &vello::kurbo::Stroke::new(1.0),
                        transform,
                        border,
                        None,
                        &ui::surface::edge_outline(pill_rect, pill_radius),
                    );
                }
            }
            let color = if active {
                colors.text_primary
            } else {
                colors.text_dim
            };
            let icon = match tab {
                WsTab::General => ui::icons::Icon::Settings2,
                WsTab::Members => ui::icons::Icon::Home,
                WsTab::Agents => ui::icons::Icon::Sparkles,
                WsTab::Permissions => ui::icons::Icon::Shield,
                WsTab::Apps => ui::icons::Icon::LayoutGrid,
            };
            let show_icon = plan.icons;
            let icon_size = if show_icon { TAB_ICON_PX } else { 0.0 };
            let icon_gap = if show_icon { TAB_ICON_GAP } else { 0.0 };
            let label = text_renderer.elide_to_width(
                label,
                lsize,
                f64_to_f32_lossy(pill_rect.width() - icon_size - icon_gap - 8.0),
            );
            let (w, h) = text_renderer.measure(&label, lsize, None);
            let content_w = icon_size + icon_gap + f64::from(w);
            let content_x = pill_rect.center().x - content_w / 2.0;
            if show_icon {
                ui::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(
                        content_x,
                        pill_rect.center().y - icon_size / 2.0,
                        content_x + icon_size,
                        pill_rect.center().y + icon_size / 2.0,
                    ),
                    icon,
                    if active { colors.accent[4] } else { color },
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            text_renderer.draw_text(
                scene,
                &label,
                lsize,
                color,
                transform.pre_translate(
                    (
                        content_x + icon_size + icon_gap,
                        pill_rect.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
            );
        }
        self.tab_pill_rects = pill_rects;
    }

    /// Dispatches to the Members/Permissions/Apps tab body when one of
    /// those is active and fully paints it; returns `true` when it did,
    /// so the caller knows to stop. Returns `false` on `General`, whose
    /// clearing happens here too, but whose body the caller continues
    /// painting below.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state/tree the tab \
                  bodies read from; matching every other draw_ws_settings_* helper in this impl"
    )]
    fn draw_ws_settings_dispatch_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        active_tab: WsTab,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) -> bool {
        match active_tab {
            WsTab::Members => {
                self.draw_ws_settings_members_tab(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    rect,
                    state,
                    tree,
                );
                true
            }
            WsTab::Agents => {
                self.draw_ws_settings_agents_tab(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    rect,
                    state,
                    tree,
                );
                true
            }
            WsTab::Permissions => {
                self.draw_ws_settings_permissions_tab(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    rect,
                    state,
                    tree,
                );
                true
            }
            WsTab::Apps => {
                self.draw_ws_settings_apps_tab(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    rect,
                    state,
                    tree,
                );
                true
            }
            WsTab::General => {
                // Fall through to the General body below.
                self.revoke_rects.clear();
                self.grant_composer_rects = None;
                self.member_control_rects.clear();
                self.ownership_action_rect = Rect::ZERO;
                self.agent_add_rect = Rect::ZERO;
                self.agent_control_rects.clear();
                self.default_app_rects.clear();
                false
            }
        }
    }

    /// The Members tab body: roster rows, then the shared close hint.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state/tree the tab \
                  body reads from; matching every other draw_ws_settings_* helper in this impl"
    )]
    fn draw_ws_settings_members_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) {
        self.revoke_rects.clear();
        self.grant_composer_rects = None;
        self.permission_profiles_rect = Rect::ZERO;
        self.member_control_rects.clear();
        self.ownership_action_rect = Rect::ZERO;
        self.agent_add_rect = Rect::ZERO;
        self.agent_control_rects.clear();
        let body_rect = Self::body_rect(rect);
        let is_owner = is_active_workspace_owner(state);
        if let Some(ws) = tree.active_workspace() {
            let actor = state.active_local_user();
            if !ownership_transfer_resource(ws, &actor).actions.is_empty() {
                self.ownership_action_rect = ownership_action_rect(body_rect);
            }
            for row in members_pane_rows(ws, body_rect) {
                self.member_control_rects
                    .push((row.user.clone(), row.role_rect, row.remove_rect));
            }
            draw_members_pane(
                scene,
                transform,
                body_rect,
                ws,
                &state.local_user_id,
                is_owner,
                text_renderer,
                theme,
            );
        }
        Self::draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
    }

    /// The Agents tab body: an explicitly non-human roster backed by the
    /// same signed membership and revocation policy as the broker.
    #[allow(
        clippy::too_many_arguments,
        reason = "matches the other workspace-settings tab paint helpers"
    )]
    fn draw_ws_settings_agents_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) {
        self.revoke_rects.clear();
        self.grant_composer_rects = None;
        self.member_control_rects.clear();
        self.ownership_action_rect = Rect::ZERO;
        self.agent_control_rects.clear();
        self.default_app_rects.clear();
        let body_rect = Self::body_rect(rect);
        self.agent_add_rect = agents_add_rect(body_rect);
        let is_owner = is_active_workspace_owner(state);
        if let Some(ws) = tree.active_workspace() {
            for row in agents_pane_rows(ws, body_rect) {
                self.agent_control_rects.push((
                    row.agent.clone(),
                    row.review_rect,
                    row.remove_rect,
                ));
            }
            draw_agents_pane(
                scene,
                transform,
                body_rect,
                ws,
                is_owner,
                text_renderer,
                theme,
            );
        }
        Self::draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
    }

    /// The Permissions tab body: the grant/revoke pane, then the shared
    /// close hint.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state/tree the tab \
                  body reads from; matching every other draw_ws_settings_* helper in this impl"
    )]
    fn draw_ws_settings_permissions_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) {
        let body_rect = Self::body_rect(rect);
        let is_owner = is_active_workspace_owner(state);
        let draft = state.ws_grant_draft;
        self.revoke_rects.clear();
        self.grant_composer_rects = None;
        self.permission_profiles_rect = Rect::ZERO;
        self.member_control_rects.clear();
        self.ownership_action_rect = Rect::ZERO;
        self.agent_add_rect = Rect::ZERO;
        if let Some(ws) = tree.active_workspace() {
            if is_owner {
                self.permission_profiles_rect = permission_profiles_rect(body_rect);
            }
            let layout = permissions_pane_layout(ws, body_rect);
            for row in &layout.rows {
                self.revoke_rects
                    .push((row.cap_id.clone(), row.revoke_rect));
            }
            self.grant_composer_rects = Some(GrantComposerRects {
                subject: layout.subject_rect,
                capability: layout.choice_rect,
                expiry: layout.expiry_rects,
                submit: layout.submit_rect,
            });
            draw_permissions_pane(
                scene,
                transform,
                body_rect,
                ws,
                &draft,
                is_owner,
                text_renderer,
                theme,
            );
        }
        Self::draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
    }

    /// The Apps tab body: default-app rows, then the shared close hint.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state/tree the tab \
                  body reads from; matching every other draw_ws_settings_* helper in this impl"
    )]
    fn draw_ws_settings_apps_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        tree: &WorkspaceTree,
    ) {
        self.revoke_rects.clear();
        self.grant_composer_rects = None;
        self.member_control_rects.clear();
        self.ownership_action_rect = Rect::ZERO;
        self.agent_add_rect = Rect::ZERO;
        self.default_app_rects.clear();
        let body_rect = Self::body_rect(rect);
        let candidates = crate::shell::launcher::default_app_candidates();
        if let Some(ws) = tree.active_workspace() {
            self.default_app_rects.extend(
                default_app_rows(body_rect, &candidates)
                    .into_iter()
                    .map(|row| (row.resource, row.workspace_rect, row.user_rect)),
            );
            draw_default_apps_pane(
                scene,
                transform,
                body_rect,
                &DefaultAppsCtx {
                    ws,
                    local_user: &state.local_user_id,
                    candidates: &candidates,
                },
                text_renderer,
                theme,
            );
        }
        Self::draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
    }

    /// Name input — edits fall through to `editing_workspace_name`.
    /// Returns whether the name is currently being edited, so the
    /// caller's footer hint can match.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the state it's read \
                  from; matching every other draw_ws_settings_* helper in this impl"
    )]
    fn name_selection_geometry(
        text_renderer: &mut TextRenderer,
        shown: &str,
        input_size: f32,
        input_rect: Rect,
        selection: ui::text_selection::Selection,
    ) -> (Rect, Rect) {
        let x = input_rect.x0 + 12.0;
        let (_, height) = text_renderer.measure("Ag", input_size, None);
        let y = input_rect.y0 + (input_rect.height() - f64::from(height)) / 2.0;
        let advance = |renderer: &mut TextRenderer, offset| {
            f64::from(
                renderer
                    .measure(shown.get(..offset).unwrap_or(shown), input_size, None)
                    .0,
            )
        };
        let anchor = x + advance(text_renderer, selection.anchor);
        let focus = x + advance(text_renderer, selection.cursor);
        (
            Rect::new(
                anchor.min(focus),
                y,
                anchor.max(focus),
                y + f64::from(height),
            ),
            Rect::new(focus, y, focus + 2.0, y + f64::from(height)),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Workspace identity painting combines render context and card bounds with chrome state, active name, and the existing body-position parameter."
    )]

    fn draw_ws_settings_name_input(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        state: &crate::shell::chrome_state::ChromeState,
        active_name: &str,
        _body_y: f64,
    ) -> bool {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body = Self::body_rect(rect);
        let identity_card = Rect::new(
            body.x0,
            body.y0 + 8.0 * f64::from(theme.text_scale),
            body.x1,
            body.y0 + 112.0 * f64::from(theme.text_scale),
        );
        ui::surface::card(
            scene,
            transform,
            identity_card,
            f64::from(tokens.radii.lg),
            colors.surfaces.raised,
        );
        text_renderer.draw_text(
            scene,
            "Workspace identity",
            tokens.typography.scale.base,
            colors.text_primary,
            transform.pre_translate(
                (
                    identity_card.x0 + 16.0,
                    identity_card.y0 + 13.0 * f64::from(theme.text_scale),
                )
                    .into(),
            ),
            None,
        );
        let description = text_renderer.elide_to_width(
            "This name appears in the top bar, invites, and shared activity.",
            tokens.typography.scale.xs,
            f64_to_f32_lossy(identity_card.width() - 32.0),
        );
        text_renderer.draw_text(
            scene,
            &description,
            tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate(
                (
                    identity_card.x0 + 16.0,
                    identity_card.y0 + 36.0 * f64::from(theme.text_scale),
                )
                    .into(),
            ),
            None,
        );
        let input_rect = self.name_rect();
        let input = RoundedRect::from_rect(input_rect, f64::from(tokens.radii.sm));
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &input);
        let can_rename = is_active_workspace_owner(state);
        let editing = can_rename && state.editing_workspace_name.is_some();
        let border_color = if editing {
            colors.accent[5]
        } else {
            ui::surface::edge_on(colors.surfaces.base)
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(if editing { 2.0 } else { 1.0 }),
            transform,
            border_color,
            None,
            &input,
        );
        let shown = if editing {
            state
                .editing_workspace_name
                .as_deref()
                .unwrap_or(active_name)
        } else {
            active_name
        };
        let input_size = tokens.typography.scale.base / text_renderer.text_scale();
        let (selected, caret) = Self::name_selection_geometry(
            text_renderer,
            shown,
            input_size,
            input_rect,
            state.editing_workspace_name_selection,
        );
        if editing && selected.width() > 0.0 {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent[5].with_alpha_factor(0.35),
                None,
                &selected,
            );
        }
        text_renderer.draw_text(
            scene,
            shown,
            input_size,
            colors.text_primary,
            transform.pre_translate((input_rect.x0 + 12.0, caret.y0).into()),
            None,
        );
        if editing {
            scene.fill(Fill::NonZero, transform, colors.accent[5], None, &caret);
        } else if !can_rename {
            text_renderer.draw_text(
                scene,
                "Only a workspace administrator can rename this workspace.",
                tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate((identity_card.x0 + 16.0, identity_card.y1 - 12.0).into()),
                None,
            );
        }
        editing
    }

    /// Sharing card that opens the signed invite composer.
    #[allow(
        clippy::too_many_arguments,
        reason = "one positional argument per painted value, plus the invite permission \
                  state; matching every other draw_ws_settings_* helper \
                  in this impl"
    )]
    fn draw_ws_settings_invite_link(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        can_invite: bool,
        _body_y: f64,
    ) -> f64 {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body = Self::body_rect(rect);
        let sharing_card =
            Self::draw_ws_settings_invite_link_card(scene, transform, theme, text_renderer, body);
        let invite_y = body.y0 + 202.0 * f64::from(theme.text_scale);
        let url_pill = Rect::new(
            sharing_card.x0 + 16.0,
            invite_y,
            sharing_card.x1 - 24.0 - 72.0 * f64::from(theme.text_scale),
            invite_y + 34.0 * f64::from(theme.text_scale),
        );
        ui::surface::card(
            scene,
            transform,
            url_pill,
            f64::from(tokens.radii.sm),
            colors.surfaces.base,
        );
        let invite_url = if can_invite {
            "Choose a person and role; Tilo signs a one-hour invite."
        } else {
            "Only a governing administrator can create workspace invites."
        };
        let invite_url = text_renderer.elide_to_width(
            invite_url,
            tokens.typography.scale.sm,
            f64_to_f32_lossy(url_pill.width() - 24.0),
        );
        text_renderer.draw_text(
            scene,
            &invite_url,
            tokens.typography.scale.sm,
            colors.text_primary,
            transform.pre_translate((url_pill.x0 + 12.0, url_pill.y0 + 9.0).into()),
            None,
        );
        let invite_btn = self.invite_rect();
        scene.fill(
            Fill::NonZero,
            transform,
            if can_invite {
                colors.accent[5]
            } else {
                colors.surfaces.base
            },
            None,
            &RoundedRect::from_rect(
                invite_btn,
                ui::chrome::ChromeRole::Control.radius(theme, invite_btn.height()),
            ),
        );
        let invite_label = "Create";
        let (cw, _ch) = text_renderer.measure(invite_label, tokens.typography.scale.sm, None);
        text_renderer.draw_text(
            scene,
            invite_label,
            tokens.typography.scale.sm,
            if can_invite {
                colors.text_primary
            } else {
                colors.text_dim
            },
            transform.pre_translate(
                (
                    invite_btn.x0 + (invite_btn.width() - f64::from(cw)) / 2.0,
                    invite_btn.y0 + 9.0,
                )
                    .into(),
            ),
            None,
        );
        invite_y
    }

    /// The sharing card's background, icon and title/subtitle — split out
    /// of [`Self::draw_ws_settings_invite_link`] to keep that under
    /// `too_many_lines`. Returns the card's rect, since the caller's URL
    /// pill and copy button are placed relative to it.
    fn draw_ws_settings_invite_link_card(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        body: Rect,
    ) -> Rect {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let sharing_card = Rect::new(
            body.x0,
            body.y0 + 128.0 * f64::from(theme.text_scale),
            body.x1,
            body.y0 + 252.0 * f64::from(theme.text_scale),
        );
        ui::surface::card(
            scene,
            transform,
            sharing_card,
            f64::from(tokens.radii.lg),
            colors.surfaces.raised,
        );
        let share_icon = Rect::new(
            sharing_card.x0 + 16.0,
            sharing_card.y0 + 14.0 * f64::from(theme.text_scale),
            sharing_card.x0 + 48.0,
            sharing_card.y0 + 46.0 * f64::from(theme.text_scale),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.accent[5].with_alpha_factor(0.16),
            None,
            &RoundedRect::from_rect(share_icon, f64::from(tokens.radii.md)),
        );
        ui::icons::draw_icon(
            scene,
            transform,
            share_icon.inset(8.0),
            ui::icons::Icon::Copy,
            colors.accent[4],
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_text(
            scene,
            "Invite people",
            tokens.typography.scale.base,
            colors.text_primary,
            transform.pre_translate(
                (
                    sharing_card.x0 + 60.0,
                    sharing_card.y0 + 13.0 * f64::from(theme.text_scale),
                )
                    .into(),
            ),
            None,
        );
        let description = text_renderer.elide_to_width(
            "Invites bind the workspace, recipient role, expiry and this device's identity.",
            tokens.typography.scale.xs,
            f64_to_f32_lossy(sharing_card.width() - 76.0),
        );
        text_renderer.draw_text(
            scene,
            &description,
            tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate(
                (
                    sharing_card.x0 + 60.0,
                    sharing_card.y0 + 36.0 * f64::from(theme.text_scale),
                )
                    .into(),
            ),
            None,
        );
        sharing_card
    }

    /// Live workspace summary. Detailed member management belongs to the
    /// Members tab; General stays an honest overview rather than inventing
    /// a placeholder collaborator.
    fn draw_ws_settings_collaborators(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        tree: &WorkspaceTree,
        _collab_y: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body = Self::body_rect(rect);
        let summary_card = Self::summary_card_rect_for(body, f64::from(theme.text_scale), 0.0);
        ui::surface::card(
            scene,
            transform,
            summary_card,
            f64::from(tokens.radii.lg),
            colors.surfaces.raised,
        );
        text_renderer.draw_text(
            scene,
            "At a glance",
            tokens.typography.scale.base,
            colors.text_primary,
            transform.pre_translate(
                (
                    summary_card.x0 + 16.0,
                    summary_card.y0 + 13.0 * f64::from(theme.text_scale),
                )
                    .into(),
            ),
            None,
        );
        let (members, grants, app_rules) = tree.active_workspace().map_or((0, 0, 0), |ws| {
            let members = ws.roster().len();
            let grants = ws
                .capabilities
                .iter()
                .filter(|cap| {
                    cap.effect == crate::shell::state::Effect::Grant && !cap.is_room_membership()
                })
                .count();
            (members, grants, ws.default_apps.policy.len())
        });
        let stats = [
            (members.to_string(), "members"),
            (grants.to_string(), "specific grants"),
            (app_rules.to_string(), "app rules"),
        ];
        let stat_gap = 8.0;
        let stat_y0 = summary_card.y0 + 44.0 * f64::from(theme.text_scale);
        let stat_w = (summary_card.width() - 32.0 - stat_gap * 2.0) / 3.0;
        for (i, (value, label)) in stats.iter().enumerate() {
            let x0 = summary_card.x0 + 16.0 + usize_to_f64_lossy(i) * (stat_w + stat_gap);
            let stat = Rect::new(
                x0,
                stat_y0,
                x0 + stat_w,
                summary_card.y1 - 14.0 * f64::from(theme.text_scale),
            );
            let stat_rr = RoundedRect::from_rect(stat, f64::from(tokens.radii.md));
            let (stat_fill, stat_edge) = stat_tile_face(theme);
            scene.fill(Fill::NonZero, transform, stat_fill, None, &stat_rr);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                stat_edge,
                None,
                &stat_rr,
            );
            text_renderer.draw_text(
                scene,
                value,
                tokens.typography.scale.lg,
                colors.text_primary,
                transform.pre_translate(
                    (stat.x0 + 12.0, stat.y0 + 8.0 * f64::from(theme.text_scale)).into(),
                ),
                None,
            );
            text_renderer.draw_text(
                scene,
                label,
                tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate(
                    (stat.x0 + 12.0, stat.y0 + 33.0 * f64::from(theme.text_scale)).into(),
                ),
                None,
            );
        }
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "workspace-settings has at most a handful of rows"
)]
/// Glyph size and gap for a tab pill's leading icon.
const TAB_ICON_PX: f64 = 15.0;
const TAB_ICON_GAP: f64 = 7.0;
/// Text inset inside a tab pill, both sides together.
const TAB_LABEL_INSET: f64 = 8.0;

/// One decision for the whole tab strip: which label set to show, and
/// whether the pills have room for their icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabStripPlan {
    /// Use every tab's `compact_label` instead of its full label.
    pub compact: bool,
    /// Paint the leading icon in every pill.
    pub icons: bool,
}

/// Measure the strip's labels at the size they are drawn and choose the
/// densest treatment where **every** tab still fits its pill intact.
///
/// The previous rule switched to compact labels on the lens width alone
/// and decided icons per pill, so at 200 % text the active "General"
/// pill elided while its neighbours did not and one pill out of five kept
/// an icon. Elision is the last resort, applied uniformly, never the first.
pub(crate) fn tab_strip_plan(
    text_renderer: &mut TextRenderer,
    size: f32,
    pill_w: f64,
) -> TabStripPlan {
    let mut widest = |labels: &[&str]| -> f64 {
        labels
            .iter()
            .map(|label| f64::from(text_renderer.measure(label, size, None).0))
            .fold(0.0, f64::max)
    };
    let full: Vec<&str> = WsTab::all().iter().map(|tab| tab.label()).collect();
    let compact: Vec<&str> = WsTab::all().iter().map(|tab| tab.compact_label()).collect();
    let icon_w = TAB_ICON_PX + TAB_ICON_GAP;
    let fits = |label_w: f64, with_icon: bool| {
        label_w + TAB_LABEL_INSET + if with_icon { icon_w } else { 0.0 } <= pill_w
    };
    let full_w = widest(&full);
    if fits(full_w, true) {
        return TabStripPlan {
            compact: false,
            icons: true,
        };
    }
    let compact_w = widest(&compact);
    if fits(compact_w, true) {
        return TabStripPlan {
            compact: true,
            icons: true,
        };
    }
    if fits(full_w, false) {
        return TabStripPlan {
            compact: false,
            icons: false,
        };
    }
    TabStripPlan {
        compact: true,
        icons: false,
    }
}

/// Segment widths for the tab strip. Equal shares while every label fits
/// its share; otherwise each segment takes its measured share of the strip,
/// so one long label is not elided while its neighbours keep slack. Always
/// sums to `nav_width`.
pub(crate) fn tab_pill_widths(needs: &[f64], nav_width: f64) -> Vec<f64> {
    let count = needs.len();
    if count == 0 {
        return Vec::new();
    }
    let equal = nav_width / usize_to_f64_lossy(count);
    let total: f64 = needs.iter().sum();
    if needs.iter().all(|need| *need <= equal) || total <= 0.0 {
        return vec![equal; count];
    }
    needs.iter().map(|need| need / total * nav_width).collect()
}

fn usize_to_f64_lossy(value: usize) -> f64 {
    value as f64
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space geometry stays within f32 precision"
)]
fn f64_to_f32_lossy(value: f64) -> f32 {
    value as f32
}

fn is_active_workspace_owner(state: &crate::shell::chrome_state::ChromeState) -> bool {
    if let Some(forced) = state.test_only_owner_override {
        return forced;
    }
    state.can_rename_active_workspace()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_selection_paint_tracks_selected_range_and_caret() {
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let rect = Rect::new(20.0, 30.0, 400.0, 100.0);
        let (selected, caret) = WorkspaceSettingsOverlay::name_selection_geometry(
            &mut tr,
            "Home",
            16.0,
            rect,
            ui::text_selection::Selection {
                anchor: 4,
                cursor: 0,
            },
        );
        assert!(
            selected.width() > 20.0,
            "select-all needs a painted highlight"
        );
        assert_eq!(caret.x0, rect.x0 + 12.0);
        assert!(caret.height() >= 32.0);
    }

    /// HIG §9.3's caveat: where a palette makes `base` and `raised` the same
    /// colour, a tile filled with `base` inside a `raised` card has no lift
    /// left to separate it. The derived hairline must therefore move
    /// against the card on every shipped palette — measured as the
    /// composite of the edge over the card, not asserted from its alpha.
    #[test]
    fn stat_tiles_keep_an_edge_on_every_palette() {
        use ui::a11y::relative_luminance;
        let over = |top: vello::peniko::Color, bottom: vello::peniko::Color| {
            let a = top.components[3];
            let t = top.components;
            let b = bottom.components;
            vello::peniko::Color::new([
                t[0] * a + b[0] * (1.0 - a),
                t[1] * a + b[1] * (1.0 - a),
                t[2] * a + b[2] * (1.0 - a),
                1.0,
            ])
        };
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let card = theme.tokens.colors.surfaces.raised;
            let (_, edge) = stat_tile_face(&theme);
            let delta = (relative_luminance(over(edge, card)) - relative_luminance(card)).abs();
            assert!(
                delta >= 0.008,
                "{}: the stat tile's edge must separate it from the card (Δ luminance {delta:.4})",
                theme.name
            );
        }
    }

    /// Every card class the lens paints — raised cards (identity, sharing,
    /// summary), base-filled fields (name, invite link) and the base-filled
    /// close chip — gets its edge from `ui::surface::edge_on(fill)`; that
    /// edge must move against its fill on every shipped palette.
    #[test]
    fn lens_cards_and_fields_keep_an_edge_on_every_palette() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let colors = &theme.tokens.colors;
            for (what, fill) in [
                ("raised card", colors.surfaces.raised),
                ("base field", colors.surfaces.base),
            ] {
                let delta = ui::surface::edge_delta(ui::surface::edge_on(fill), fill);
                assert!(
                    delta >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{}: the {what} edge must separate it from its fill (Δ luminance {delta:.4})",
                    theme.name
                );
            }
        }
    }

    /// One rhythm across the header (HIG §7b): `s4` above the identity
    /// card, `s4` between card and tab strip, `s4` between strip and the
    /// first content card; the close chip is centred on the card's line.
    #[test]
    fn lens_header_keeps_one_rhythm_and_centres_the_close_chip() {
        let s4 = f64::from(ui::tokens::Spacing::BASE.s4);
        let s3 = f64::from(ui::tokens::Spacing::BASE.s3);
        for card in [
            Rect::new(880.0, 68.0, 1580.0, 880.0),
            Rect::new(0.0, 0.0, 540.0, 700.0),
        ] {
            let identity = WorkspaceSettingsOverlay::identity_rect_for(card);
            let nav = WorkspaceSettingsOverlay::nav_rect_for(card);
            let body = WorkspaceSettingsOverlay::body_rect_for(card);
            assert_eq!(
                identity.y0 - card.y0,
                s4,
                "panel pad above the identity card"
            );
            assert_eq!(nav.y0 - identity.y1, s4, "identity card → tab strip");
            // General's first card sits `8 × text_scale` under the body top.
            assert_eq!(body.y0 + 8.0 - nav.y1, s4, "tab strip → first content card");
            assert_eq!(
                nav.height() - 2.0 * WorkspaceSettingsOverlay::PILL_INSET,
                f64::from(ui::tokens::Metrics::BASE.min_pointer_target),
                "a tab pill is one pointer target tall"
            );
            let close = WorkspaceSettingsOverlay::close_rect_for(card);
            assert!(
                (close.center().y - identity.center().y).abs() < f64::EPSILON,
                "close chip is centred on the identity card"
            );
            assert!(close.x1 <= identity.x1 - s3 + f64::EPSILON);
            assert!(close.y0 > identity.y0 && close.y1 < identity.y1);
        }
    }

    /// General at 1×: the name field sits on the card pad (`s4`) inside the
    /// body and the "At a glance" card starts a token step or more under it,
    /// sharing the body's left edge. ux-bench cannot measure this pair at
    /// 200 % text, where the two are never co-visible on an 800 px body, so
    /// the production geometry helpers carry the assertion.
    #[test]
    fn general_name_field_sits_on_the_card_pad_and_clears_the_summary_card() {
        let s1 = f64::from(ui::tokens::Spacing::BASE.s1);
        let s4 = f64::from(ui::tokens::Spacing::BASE.s4);
        for card in [
            Rect::new(720.0, 68.0, 1260.0, 780.0),
            Rect::new(0.0, 0.0, 540.0, 700.0),
        ] {
            let body = WorkspaceSettingsOverlay::body_rect_for(card);
            let name = WorkspaceSettingsOverlay::name_rect_for(card);
            let summary = WorkspaceSettingsOverlay::summary_card_rect_for(body, 1.0, 0.0);
            assert_eq!(name.x0 - body.x0, s4, "name field sits on the card pad");
            assert_eq!(
                body.x1 - name.x1,
                s4,
                "name field keeps the card pad on the right"
            );
            assert!(
                summary.y0 - name.y1 >= s1,
                "summary card clears the name field (gap {})",
                summary.y0 - name.y1
            );
            assert_eq!(
                summary.x0, body.x0,
                "summary card shares the body's left edge"
            );
            assert_eq!(summary.x1, body.x1);
        }
    }

    #[test]
    fn close_transition_remains_visible_until_exit_settles() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        overlay.force_settled();
        // The lens opens and closes over the motion token's `base` (HIG §9.7).
        let open_ms = u64::from(Theme::dark().tokens.motion.durations.base_ms);
        overlay
            .transition
            .set_open(false, Duration::from_millis(open_ms), ui::Curve::EaseOut);
        let now = Instant::now();
        assert!(overlay.transition.is_visible(now));
        assert!(
            !overlay
                .transition
                .is_visible(now + Duration::from_millis(open_ms + 500))
        );
    }

    #[test]
    fn tab_strip_plan_compacts_every_label_before_it_elides_any() {
        let mut text = TextRenderer::new();
        // The 540 px laptop lens at 100 %: five ~95 px pills.
        let base = Theme::dark();
        let plan = tab_strip_plan(&mut text, base.tokens.typography.scale.base, 95.6);
        assert!(plan.icons, "at 100% every pill has room for its icon");
        // The same lens at 200 % text: no pill can hold a full label, so the
        // whole strip drops to compact labels (and icons only if they all
        // fit) instead of eliding the active tab alone.
        let large = Theme::dark().with_text_scale(2.0);
        let lsize = large.tokens.typography.scale.base;
        // In the 540 px lens even compact labels overflow a 95.6 px pill at
        // 28 px type, so the plan drops to compact labels without icons and
        // every pill elides by the same rule — never the active tab alone.
        let plan = tab_strip_plan(&mut text, lsize, 95.6);
        assert_eq!(
            plan,
            TabStripPlan {
                compact: true,
                icons: false
            }
        );
        // In the 800 px lens (145.6 px pills) compact labels fit with icons.
        let plan = tab_strip_plan(&mut text, lsize, 145.6);
        assert_eq!(
            plan,
            TabStripPlan {
                compact: true,
                icons: true
            }
        );
        let widest = WsTab::all()
            .iter()
            .map(|tab| f64::from(text.measure(tab.compact_label(), lsize, None).0))
            .fold(0.0, f64::max);
        assert!(
            widest + TAB_LABEL_INSET + TAB_ICON_PX + TAB_ICON_GAP <= 145.6,
            "the chosen plan fits its widest label ({widest}) into the pill"
        );
    }

    #[test]
    fn tab_pills_take_measured_widths_only_when_equal_shares_cannot_hold_the_widest_label() {
        let equal = tab_pill_widths(&[80.0, 60.0, 70.0, 75.0, 50.0], 500.0);
        assert_eq!(equal.len(), 5);
        assert!(equal.iter().all(|w| (*w - 100.0).abs() < f64::EPSILON));

        let measured = tab_pill_widths(&[120.0, 80.0, 80.0, 90.0, 70.0], 500.0);
        assert!(
            (measured.iter().sum::<f64>() - 500.0).abs() < 1e-9,
            "fills the strip exactly"
        );
        for (need, width) in [120.0, 80.0, 80.0, 90.0, 70.0].iter().zip(&measured) {
            assert!(width >= need, "every label fits its pill: {measured:?}");
        }
        assert!(measured[0] > measured[4]);
        assert!(tab_pill_widths(&[], 500.0).is_empty());
    }

    #[test]
    fn close_button_is_one_pointer_target() {
        let close = WorkspaceSettingsOverlay::close_rect_for(Rect::new(0.0, 0.0, 540.0, 700.0));
        assert!(close.width() >= 32.0 && close.height() >= 32.0);
    }

    #[test]
    fn laptop_lens_preserves_most_of_the_workspace() {
        let viewport = Rect::new(0.0, 0.0, 1280.0, 720.0);
        let lens = WorkspaceSettingsOverlay::rect_for(viewport);
        assert_eq!(lens.x1, viewport.x1 - 20.0);
        assert!(lens.width() <= 540.0);
        assert!(lens.x0 >= 720.0, "the workspace keeps a usable majority");
    }

    #[test]
    fn lens_keeps_a_visible_bezel_gap_on_wide_and_compact_outputs() {
        for viewport in [
            Rect::new(0.0, 0.0, 1600.0, 900.0),
            Rect::new(0.0, 0.0, 960.0, 540.0),
            Rect::new(0.0, 0.0, 600.0, 700.0),
        ] {
            let lens = WorkspaceSettingsOverlay::rect_for(viewport);
            assert_eq!(viewport.x1 - lens.x1, 20.0);
            assert_eq!(viewport.y1 - lens.y1, 20.0);
            assert!(lens.y0 >= viewport.y0 + 20.0);
        }
    }

    #[test]
    fn tab_body_reserves_a_non_overlapping_close_hint_footer() {
        for card in [
            Rect::new(880.0, 68.0, 1580.0, 880.0),
            Rect::new(408.0, 68.0, 940.0, 520.0),
        ] {
            let body = WorkspaceSettingsOverlay::body_rect_for(card);
            assert_eq!(card.y1 - body.y1, 44.0);
            let close_hint_baseline = card.y1 - 24.0;
            assert!(body.y1 <= close_hint_baseline - 20.0);
        }
    }

    #[test]
    fn typed_hits_prefer_policy_controls_over_tabs_and_name() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        let hit_rect = Rect::new(700.0, 200.0, 760.0, 240.0);
        overlay.tab_pill_rects = [hit_rect, Rect::ZERO, Rect::ZERO, Rect::ZERO, Rect::ZERO];
        let Ok(resource) = desktop_policy::ResourceKind::extension("pdf") else {
            panic!("pdf is a valid extension resource");
        };
        overlay
            .default_app_rects
            .push((resource.clone(), hit_rect, Rect::ZERO));
        assert_eq!(
            overlay.hit_test(hit_rect.center(), WsTab::General),
            EventOutcome::WorkspaceDefault(resource)
        );
    }

    #[test]
    fn agent_revoke_hit_keeps_the_typed_agent_identity() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        let revoke = Rect::new(900.0, 300.0, 980.0, 334.0);
        overlay
            .agent_control_rects
            .push(("scribe".into(), Rect::ZERO, revoke));
        assert_eq!(
            overlay.hit_test(revoke.center(), WsTab::Agents),
            EventOutcome::RemoveAgent("scribe".into()),
        );
    }

    #[test]
    fn ownership_entry_point_is_a_typed_actionable_control() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        let action = Rect::new(900.0, 220.0, 1_020.0, 258.0);
        overlay.ownership_action_rect = action;
        assert_eq!(
            overlay.hit_test(action.center(), WsTab::Members),
            EventOutcome::ManageOwnership
        );
    }

    #[test]
    fn permission_roles_entry_point_opens_the_canonical_role_actions() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        let action = Rect::new(900.0, 220.0, 1_020.0, 244.0);
        overlay.permission_profiles_rect = action;
        assert_eq!(
            overlay.hit_test(action.center(), WsTab::Permissions),
            EventOutcome::ManagePermissionProfiles
        );
        assert_eq!(
            overlay.hit_test(action.center(), WsTab::Members),
            EventOutcome::CommitName,
            "a retained Permissions hit cannot leak into another tab"
        );
    }

    #[test]
    fn role_sheet_keyboard_selects_an_explicit_role_without_cycling() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        overlay.open_member_role_sheet("ada".into(), CapRole::Viewer);
        assert!(overlay.member_sheet_is_open());
        assert_eq!(
            overlay.handle_member_sheet_key(MemberSheetKey::Next),
            None,
            "arrow navigation only moves the focused choice"
        );
        assert_eq!(
            overlay.handle_member_sheet_key(MemberSheetKey::Activate),
            Some(EventOutcome::SetMemberRole(
                "ada".into(),
                CapRole::Commenter
            ))
        );
    }

    #[test]
    fn agent_setup_and_review_keep_the_agent_identity_typed() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        let add = Rect::new(680.0, 220.0, 800.0, 254.0);
        let review = Rect::new(810.0, 300.0, 920.0, 334.0);
        overlay.agent_add_rect = add;
        overlay
            .agent_control_rects
            .push(("scribe".into(), review, Rect::ZERO));
        assert_eq!(
            overlay.hit_test(add.center(), WsTab::Agents),
            EventOutcome::AddAgent,
        );
        assert_eq!(
            overlay.hit_test(review.center(), WsTab::Agents),
            EventOutcome::ReviewAgent("scribe".into()),
        );
    }

    #[test]
    fn destructive_sheet_needs_the_final_confirmation_option() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        overlay.open_revoke_sheet("cap-camera".into());
        assert_eq!(
            overlay.handle_member_sheet_key(MemberSheetKey::Activate),
            Some(EventOutcome::DismissMemberSheet),
            "the initial focus is the safe keep-permission action"
        );
        overlay.open_revoke_sheet("cap-camera".into());
        let _ = overlay.handle_member_sheet_key(MemberSheetKey::Next);
        let _ = overlay.handle_member_sheet_key(MemberSheetKey::Next);
        assert_eq!(
            overlay.handle_member_sheet_key(MemberSheetKey::Activate),
            Some(EventOutcome::ConfirmRevoke("cap-camera".into()))
        );
    }
}

#[cfg(test)]
mod general_scroll_tests {
    use super::*;
    #[test]
    fn compact_general_body_scroll_reaches_summary_and_keeps_hit_tests_in_clip() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 960.0, 540.0));
        let name = overlay.name_rect();
        assert!(overlay.general_body_rect().contains(name.center()));
        assert!(overlay.scroll_general(1000.0));
        let summary = overlay.general_summary_rect();
        assert!(summary.y1 <= overlay.general_body_rect().y1);
        assert!(summary.y0 >= overlay.general_body_rect().y0);
        assert_eq!(
            overlay.hit_test(name.center(), WsTab::General),
            EventOutcome::CommitName
        );
        assert!(overlay.scroll_general(-1000.0));
        assert_eq!(overlay.name_rect(), name);
    }
}
