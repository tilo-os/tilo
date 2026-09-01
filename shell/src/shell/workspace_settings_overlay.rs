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
use crate::shell::state::{UserId, WorkspaceTree};
use crate::shell::workspace_settings::{
    DefaultAppsCtx, default_app_rows, draw_default_apps_pane, draw_members_pane,
    draw_permissions_pane, members_pane_rows, permissions_pane_layout,
};

/// Duration shared by the workspace-settings lens and the global-search
/// modal, whose travel distance and visual weight are equivalent.
pub const MODAL_OPEN_ANIM_MS: u64 = 220;

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
    MemberRole(UserId),
    RemoveMember(UserId),
    Revoke(String),
    SelectTab(WsTab),
    CreateInvite,
    EditName,
    CommitName,
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
    transition: ui::Transition,
    tab_pill_rects: [Rect; 4],
    default_app_rects: Vec<(desktop_policy::ResourceKind, Rect, Rect)>,
    revoke_rects: Vec<(String, Rect)>,
    grant_composer_rects: Option<GrantComposerRects>,
    member_control_rects: Vec<(UserId, Rect, Rect)>,
}

impl Default for WorkspaceSettingsOverlay {
    fn default() -> Self {
        Self {
            viewport: Rect::ZERO,
            transition: ui::Transition::closed(),
            tab_pill_rects: [Rect::ZERO; 4],
            default_app_rects: Vec::new(),
            revoke_rects: Vec::new(),
            grant_composer_rects: None,
            member_control_rects: Vec::new(),
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
        let margin = 12.0;
        let top = viewport.y0 + f64::from(crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX) + margin;
        let bottom = (viewport.y1 - margin).max(top + 320.0);
        if viewport.width() < 820.0 {
            return Rect::new(viewport.x0 + margin, top, viewport.x1 - margin, bottom);
        }
        let lens_w = (viewport.width() * 0.46).clamp(620.0, 780.0);
        let right = viewport.x1 - margin;
        Rect::new(right - lens_w, top, right, bottom)
    }

    pub fn body_rect_for(card: Rect) -> Rect {
        Rect::new(
            card.x0 + 16.0,
            card.y0 + 160.0,
            card.x1 - 16.0,
            card.y1 - 16.0,
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

    pub fn name_rect(&self) -> Rect {
        Self::name_rect_for(self.rect())
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
        Rect::new(
            rect.x1 - 58.0,
            rect.y0 + 25.0,
            rect.x1 - 28.0,
            rect.y0 + 55.0,
        )
    }

    pub fn invite_rect(&self) -> Rect {
        Self::invite_rect_for(self.rect())
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
                WsTab::Permissions => 2,
                WsTab::Apps => 3,
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

    fn clear_hits(&mut self) {
        self.tab_pill_rects = [Rect::ZERO; 4];
        self.default_app_rects.clear();
        self.revoke_rects.clear();
        self.grant_composer_rects = None;
        self.member_control_rects.clear();
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
            self.clear_hits();
        }
        self.transition.set_open(
            open,
            if theme.reduce_motion {
                Duration::ZERO
            } else {
                Duration::from_millis(MODAL_OPEN_ANIM_MS)
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
        let travel = if theme.reduce_motion {
            0.0
        } else {
            48.0 * (1.0 - f64::from(alpha))
        };
        let panel_transform = transform.pre_translate((travel, 0.0).into());
        ui::surface::soft_shadow(
            scene,
            panel_transform,
            rect,
            theme.tokens.colors.neutral[0],
            40.0,
            16.0,
            0.62 * alpha,
        );
        let card = RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            panel_transform,
            theme.tokens.colors.surfaces.base,
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
        for (user, role, remove) in &self.member_control_rects {
            if role.area() > 0.0 && role.contains(point) {
                return EventOutcome::MemberRole(user.clone());
            }
            if remove.area() > 0.0 && remove.contains(point) {
                return EventOutcome::RemoveMember(user.clone());
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
            return;
        }

        // Unreachable today (every non-General arm returns `true` above),
        // but guards against a new `WsTab` variant falling through to
        // paint the General body over its pane.
        if !matches!(active_tab, WsTab::General) {
            return;
        }

        let body_y = rect.y0 + 168.0;
        let editing = self.draw_ws_settings_name_input(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
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
            rect,
            can_invite,
            body_y,
        );
        let collab_y = invite_y + 72.0;
        Self::draw_ws_settings_collaborators(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            &tree,
            collab_y,
        );

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
        let label = format!("{active_role} · Settings and policy stay with this workspace");
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
        let identity = Rect::new(
            rect.x0 + 16.0,
            rect.y0 + 14.0,
            rect.x1 - 16.0,
            rect.y0 + 94.0,
        );
        let identity_rr = RoundedRect::from_rect(identity, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &identity_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &identity_rr,
        );
        let mark = Rect::new(
            identity.x0 + 14.0,
            identity.y0 + 14.0,
            identity.x0 + 62.0,
            identity.y1 - 14.0,
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
        let close_rr = RoundedRect::from_rect(close, f64::from(tokens.radii.md));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.base,
            None,
            &close_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &close_rr,
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
        let nav = Rect::new(
            rect.x0 + 16.0,
            rect.y0 + 104.0,
            rect.x1 - 16.0,
            rect.y0 + 146.0,
        );
        let nav_rr = RoundedRect::from_rect(nav, f64::from(tokens.radii.lg));
        scene.fill(Fill::NonZero, transform, colors.neutral[0], None, &nav_rr);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard.with_alpha_factor(0.72),
            None,
            &nav_rr,
        );
        let segment_w = nav.width() / usize_to_f64_lossy(tabs.len());
        // Written into the component's cache after the loop
        // so one source of geometry feeds draw and click-routing.
        let mut pill_rects = [Rect::ZERO; 4];
        for (i, tab) in tabs.iter().enumerate() {
            let active = *tab == active_tab;
            let label = if rect.width() < 680.0 {
                tab.compact_label()
            } else {
                tab.label()
            };
            let (w, h) = text_renderer.measure(label, lsize, None);
            let ix = usize_to_f64_lossy(i);
            let pill_rect = Rect::new(
                nav.x0 + segment_w * ix + 3.0,
                nav.y0 + 3.0,
                nav.x0 + segment_w * (ix + 1.0) - 3.0,
                nav.y1 - 3.0,
            );
            if let Some(slot) = pill_rects.get_mut(i) {
                *slot = pill_rect;
            }
            let pill = RoundedRect::from_rect(pill_rect, f64::from(tokens.radii.md));
            if active {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.surfaces.raised,
                    None,
                    &pill,
                );
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    transform,
                    colors.border_standard,
                    None,
                    &pill,
                );
            }
            let color = if active {
                colors.text_primary
            } else {
                colors.text_dim
            };
            let icon = match tab {
                WsTab::General => ui::icons::Icon::Settings2,
                WsTab::Members => ui::icons::Icon::Home,
                WsTab::Permissions => ui::icons::Icon::Shield,
                WsTab::Apps => ui::icons::Icon::LayoutGrid,
            };
            let icon_size = 15.0;
            let content_w = icon_size + 7.0 + f64::from(w);
            let content_x = pill_rect.center().x - content_w / 2.0;
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
            text_renderer.draw_text(
                scene,
                label,
                lsize,
                color,
                transform.pre_translate(
                    (
                        content_x + icon_size + 7.0,
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
        self.member_control_rects.clear();
        let body_rect = Self::body_rect(rect);
        let is_owner = is_active_workspace_owner(state);
        if let Some(ws) = tree.active_workspace() {
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
        self.member_control_rects.clear();
        if let Some(ws) = tree.active_workspace() {
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
        let identity_card = Rect::new(body.x0, body.y0 + 8.0, body.x1, body.y0 + 112.0);
        let identity_rr = RoundedRect::from_rect(identity_card, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &identity_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &identity_rr,
        );
        text_renderer.draw_text(
            scene,
            "Workspace identity",
            tokens.typography.scale.base,
            colors.text_primary,
            transform.pre_translate((identity_card.x0 + 16.0, identity_card.y0 + 13.0).into()),
            None,
        );
        text_renderer.draw_text(
            scene,
            "This name appears in the top bar, invites, and shared activity.",
            tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((identity_card.x0 + 16.0, identity_card.y0 + 36.0).into()),
            None,
        );
        let input_rect = self.name_rect();
        let input = RoundedRect::from_rect(input_rect, f64::from(tokens.radii.sm));
        scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &input);
        let editing = state.editing_workspace_name.is_some();
        let border_color = if editing {
            colors.accent[5]
        } else {
            colors.border_standard
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(if editing { 2.0 } else { 1.0 }),
            transform,
            border_color,
            None,
            &input,
        );
        let shown = state
            .editing_workspace_name
            .as_deref()
            .unwrap_or(active_name);
        let input_size = tokens.typography.scale.base;
        text_renderer.draw_text(
            scene,
            shown,
            input_size,
            colors.text_primary,
            transform.pre_translate((input_rect.x0 + 12.0, input_rect.y0 + 6.0).into()),
            None,
        );
        if editing {
            let (tw, _th) = text_renderer.measure(shown, input_size, None);
            let caret_x = input_rect.x0 + 12.0 + f64::from(tw) + 1.0;
            let caret = Rect::new(
                caret_x,
                input_rect.y0 + 6.0,
                caret_x + 2.0,
                input_rect.y0 + 6.0 + f64::from(input_size),
            );
            scene.fill(Fill::NonZero, transform, colors.accent[5], None, &caret);
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
        let invite_y = body.y0 + 202.0;
        let url_pill = Rect::new(
            sharing_card.x0 + 16.0,
            invite_y,
            sharing_card.x1 - 16.0 - 80.0,
            invite_y + 34.0,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &RoundedRect::from_rect(url_pill, f64::from(tokens.radii.sm)),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &RoundedRect::from_rect(url_pill, f64::from(tokens.radii.sm)),
        );
        let invite_url = if can_invite {
            "Choose a person and role; qOS signs a one-hour invite."
        } else {
            "Only a governing administrator can create workspace invites."
        };
        text_renderer.draw_text(
            scene,
            invite_url,
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
        let sharing_card = Rect::new(body.x0, body.y0 + 128.0, body.x1, body.y0 + 252.0);
        let sharing_rr = RoundedRect::from_rect(sharing_card, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &sharing_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &sharing_rr,
        );
        let share_icon = Rect::new(
            sharing_card.x0 + 16.0,
            sharing_card.y0 + 14.0,
            sharing_card.x0 + 48.0,
            sharing_card.y0 + 46.0,
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
            transform.pre_translate((sharing_card.x0 + 60.0, sharing_card.y0 + 13.0).into()),
            None,
        );
        text_renderer.draw_text(
            scene,
            "Invites bind the workspace, recipient role, expiry and this device's identity.",
            tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((sharing_card.x0 + 60.0, sharing_card.y0 + 36.0).into()),
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
        let body = Rect::new(
            rect.x0 + 16.0,
            rect.y0 + 160.0,
            rect.x1 - 16.0,
            rect.y1 - 16.0,
        );
        let summary_card = Rect::new(body.x0, body.y0 + 268.0, body.x1, body.y0 + 374.0);
        let summary_rr = RoundedRect::from_rect(summary_card, f64::from(tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.raised,
            None,
            &summary_rr,
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &summary_rr,
        );
        text_renderer.draw_text(
            scene,
            "At a glance",
            tokens.typography.scale.base,
            colors.text_primary,
            transform.pre_translate((summary_card.x0 + 16.0, summary_card.y0 + 13.0).into()),
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
        let stat_y0 = summary_card.y0 + 44.0;
        let stat_w = (summary_card.width() - 32.0 - stat_gap * 2.0) / 3.0;
        for (i, (value, label)) in stats.iter().enumerate() {
            let x0 = summary_card.x0 + 16.0 + usize_to_f64_lossy(i) * (stat_w + stat_gap);
            let stat = Rect::new(x0, stat_y0, x0 + stat_w, summary_card.y1 - 14.0);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.base,
                None,
                &RoundedRect::from_rect(stat, f64::from(tokens.radii.md)),
            );
            text_renderer.draw_text(
                scene,
                value,
                tokens.typography.scale.lg,
                colors.text_primary,
                transform.pre_translate((stat.x0 + 12.0, stat.y0 + 8.0).into()),
                None,
            );
            text_renderer.draw_text(
                scene,
                label,
                tokens.typography.scale.xs,
                colors.text_dim,
                transform.pre_translate((stat.x0 + 12.0, stat.y0 + 33.0).into()),
                None,
            );
        }
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "workspace-settings has at most a handful of rows"
)]
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
    let user = state.active_local_user();
    let tree = state.tree();
    tree.active_workspace().is_some_and(|workspace| {
        tree.governs_room(workspace.room) && workspace.may_administer(&user)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_transition_remains_visible_until_exit_settles() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        overlay.force_settled();
        overlay.transition.set_open(
            false,
            Duration::from_millis(MODAL_OPEN_ANIM_MS),
            ui::Curve::EaseOut,
        );
        let now = Instant::now();
        assert!(overlay.transition.is_visible(now));
        assert!(
            !overlay
                .transition
                .is_visible(now + Duration::from_millis(MODAL_OPEN_ANIM_MS + 500))
        );
    }

    #[test]
    fn typed_hits_prefer_policy_controls_over_tabs_and_name() {
        let mut overlay = WorkspaceSettingsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
        let hit_rect = Rect::new(700.0, 200.0, 760.0, 240.0);
        overlay.tab_pill_rects = [hit_rect, Rect::ZERO, Rect::ZERO, Rect::ZERO];
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
}
