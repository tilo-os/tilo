//! Presentation state for the workspace-settings lens.
//!
//! Policy mutations deliberately remain with the host: this component
//! reports typed hits, while the host applies them through the existing
//! authenticated `Workspace` mutation methods.

use crate::authority::WorkspaceAuthority as _;
use crate::i18n::{t, t_args};
use ui::num::{f32_of_f64, f64_of_usize};
use ui::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use ui::ColorCompat;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::ButtonRole;

use collab_types::role::Role as CapRole;
use tilo_desktop_model::{DesktopTree, UserId};

use super::{
    AppDirectory, DefaultAppsCtx, GrantDraft, WsTab, agents_add_rect, agents_pane_rows,
    default_app_rows, draw_agents_pane, draw_default_apps_pane, draw_members_pane,
    draw_permissions_pane, members_pane_rows, ownership_action_rect, permission_profiles_rect,
    permissions_pane_layout,
};
use ui::TextRole;
use ui::text_render::TextStyle;

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

/// What the lens paints, read from the host once per frame. Everything is
/// borrowed, so building it costs no copy of the desktop.
pub struct WorkspaceSettingsModel<'a> {
    pub desktop: &'a DesktopTree,
    pub tab: WsTab,
    pub grant_draft: GrantDraft,
    /// The person acting in the active workspace: who "you" is in every
    /// pane, and whose authority the controls follow.
    pub acting_user: &'a UserId,
    /// What each person here is called, as the roster calls them.
    pub people: &'a crate::collaborators::PeopleNames,
    /// The in-progress name, while the person is renaming the workspace.
    pub editing_name: Option<&'a str>,
    pub name_selection: ui::text_selection::Selection,
    /// Whether policy controls are live rather than painted disabled.
    pub owner_controls: bool,
    pub apps: &'a dyn AppDirectory,
    /// The person's devices and whether this workspace shows on each.
    /// Empty for a workspace that is not the person's own.
    pub devices: &'a [WorkspaceDevice],
    /// Whether the workspace is public, as its host answered.
    pub public: &'a PublicCard,
    /// The workspaces this device keeps archived: off its switcher and
    /// desktop, each restorable or deletable from here.
    pub archived: &'a [ArchivedWorkspace],
}

/// A workspace this device keeps archived, as the General tab lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchivedWorkspace {
    pub room: tilo_desktop_model::RoomId,
    pub name: String,
}

/// The General tab's Public card: whether the workspace is listed in the
/// directory of the host it lives on, what the listing says, and the role
/// everyone who joins from it gets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PublicCard {
    #[default]
    Asking,
    /// What its host answered.
    Known {
        public: bool,
        description: String,
        default_role: tilo_action_policy::Role,
    },
    /// It cannot be public here, or its host did not answer: why.
    Unavailable(String),
}

/// One of the person's devices as the General tab offers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceDevice {
    pub label: String,
    pub shown: bool,
    /// The device this lens runs on.
    pub here: bool,
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
    /// Opens this agent's Allow / Ask first / Deny answers, app by app.
    ReviewAgent(String),
    /// Back from one agent's answers to the roster.
    CloseAgentReview,
    ToggleAgentApp(tilo_action_policy::agent_answers::AnswerScope),
    /// Opens or closes the models the reviewed agent may plan with.
    ToggleAgentModels,
    /// Has the agent plan with this model of the person's, or with `None`
    /// the default.
    SetAgentModel {
        agent: String,
        model: Option<super::agent_access::AgentModelOption>,
    },
    /// Sets (or, with `None`, returns to the admission's) the agent's
    /// answers, each for a kind at a scope.
    SetAgentAnswers {
        agent: String,
        answers: Vec<(
            tilo_action_policy::agent_answers::AnswerScope,
            tilo_action_policy::ActionKind,
            Option<tilo_action_policy::Answer>,
        )>,
    },
    RemoveAgent(String),
    /// Find a bot for this workspace (the Store's bots).
    AddBot,
    /// Take back this workspace's request for the bot of this directory
    /// entry: its host stops and removes it.
    RemoveBot(String),
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
    /// Show the workspace on the `n`th listed device, or keep it off.
    ToggleDevice(usize),
    /// List the workspace in its host's directory, or take it out.
    TogglePublic,
    /// Hand out the next role to people who join from the directory.
    CycleDefaultRole,
    /// Write what the public workspace is, in the palette's typed field.
    EditDescription,
    /// Move the workspace one place earlier (`-1`) or later (`1`).
    MoveWorkspace(i32),
    DeleteWorkspace,
    /// Bring the `n`th archived workspace back to the switcher.
    RestoreArchived(usize),
    /// Delete the `n`th archived workspace, after the usual confirmation.
    DeleteArchived(usize),
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

/// Keyboard operations of the agent access view, mapped from platform
/// keys by the host like [`MemberSheetKey`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentAccessKey {
    Previous,
    Next,
    /// The answer to the left (Allow side) of the focused control.
    Left,
    /// The answer to the right (Deny side) of the focused control.
    Right,
    Activate,
    Back,
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
    /// General body geometry reserves both OS UI and text scaling.
    general_text_scale: f64,
    /// Measured footer plus its themed inset/gap, reserved outside the body clip.
    general_footer_height: f64,
    transition: ui::motion::Presence,
    tab_pill_rects: [Rect; 5],
    default_app_rects: Vec<(desktop_policy::ResourceKind, Rect, Rect)>,
    revoke_rects: Vec<(String, Rect)>,
    grant_composer_rects: Option<GrantComposerRects>,
    permission_profiles_rect: Rect,
    member_control_rects: Vec<(UserId, Rect, Rect)>,
    ownership_action_rect: Rect,
    agent_add_rect: Rect,
    agent_control_rects: Vec<(String, Rect, Rect)>,
    /// The Bots section's Add a bot, and each requested bot's Remove by its
    /// directory entry.
    bot_add_rect: Rect,
    bot_remove_rects: Vec<(String, Rect)>,
    /// The agent whose answers the Agents tab shows instead of the roster,
    /// and which of its apps is open.
    agent_review: Option<(
        String,
        Option<tilo_action_policy::agent_answers::AnswerScope>,
    )>,
    agent_access: super::agent_access::AgentAccessLayout,
    /// The models the person may choose for the reviewed agent, as the
    /// host read them from Settings, and whether they are shown.
    agent_models: Option<super::agent_access::AgentModels>,
    agent_models_open: bool,
    /// How far the agent view's app list is scrolled.
    agent_access_scroll: f64,
    /// The agent view's control with the keyboard, once keys have moved it.
    agent_access_focus: Option<super::agent_access::AccessFocus>,
    member_sheet: Option<MemberSheet>,
    member_sheet_focus: usize,
    member_sheet_option_rects: Vec<Rect>,
    /// The General tab's devices card: one toggle per device, the two move
    /// buttons and Delete, as painted (scroll applied).
    device_toggle_rects: Vec<Rect>,
    move_rects: [Rect; 2],
    delete_rect: Rect,
    /// Rows the devices card had when last painted, for the scroll range.
    device_rows: usize,
    /// The Public card's switch, role and description controls, as
    /// painted (scroll applied); zero while they take no input.
    public_rects: [Rect; 3],
    /// The archived workspaces card: each row's Restore and Delete, as
    /// painted (scroll applied).
    archived_rects: Vec<[Rect; 2]>,
    /// Rows the archived card had when last painted, for the scroll range.
    archived_rows: usize,
    /// Shaped caret/scroll cache; the host owns the rename draft and selection.
    name_input: Option<ui::widget::TextInput>,
}

impl Default for WorkspaceSettingsOverlay {
    fn default() -> Self {
        Self {
            viewport: Rect::ZERO,
            general_scroll_y: 0.0,
            general_text_scale: 1.0,
            general_footer_height: 48.0,
            transition: ui::motion::Presence::closed(),
            tab_pill_rects: [Rect::ZERO; 5],
            default_app_rects: Vec::new(),
            revoke_rects: Vec::new(),
            grant_composer_rects: None,
            permission_profiles_rect: Rect::ZERO,
            member_control_rects: Vec::new(),
            ownership_action_rect: Rect::ZERO,
            agent_add_rect: Rect::ZERO,
            agent_control_rects: Vec::new(),
            bot_add_rect: Rect::ZERO,
            bot_remove_rects: Vec::new(),
            agent_review: None,
            agent_access: super::agent_access::AgentAccessLayout::default(),
            agent_models: None,
            agent_models_open: false,
            agent_access_scroll: 0.0,
            agent_access_focus: None,
            member_sheet: None,
            member_sheet_focus: 0,
            member_sheet_option_rects: Vec::new(),
            device_toggle_rects: Vec::new(),
            move_rects: [Rect::ZERO; 2],
            delete_rect: Rect::ZERO,
            device_rows: 0,
            public_rects: [Rect::ZERO; 3],
            archived_rects: Vec::new(),
            archived_rows: 0,
            name_input: None,
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
        let spacing = ui::tokens::Spacing::BASE;
        let metrics = ui::tokens::Metrics::BASE;
        // A floating application surface, not edge chrome: the group step
        // (`s6`) of air from the bezel on every side, under the top bar.
        let margin = f64::from(spacing.s6);
        let bar_bottom = viewport.y0 + f64::from(crate::topbar::TOPBAR_HEIGHT_PX);
        let top = bar_bottom + margin;
        // Shorter than the header, the tab strip and a first card need, the
        // lens becomes the whole page under the bar (HIG 7c: a compact
        // sheet fills the window) rather than running past the screen's
        // bottom edge.
        let min_card = Self::IDENTITY_H
            + Self::NAV_H
            + 2.0 * Self::HEADER_PAD
            + Self::BODY_LEAD
            + f64::from(metrics.panel_row_height)
            + Self::FOOTER_H;
        if viewport.y1 - margin - top < min_card {
            return Rect::new(
                viewport.x0,
                bar_bottom.min(viewport.y1),
                viewport.x1,
                viewport.y1,
            );
        }
        let bottom = viewport.y1 - margin;
        // A sheet's width beside the workspace, which keeps the larger part
        // of the screen; narrower than that and the workspace, the lens is
        // one inset sheet.
        let sheet = f64::from(metrics.panel_width_sheet);
        if viewport.width() < f64::from(metrics.compact_breakpoint) {
            return Rect::new(viewport.x0 + margin, top, viewport.x1 - margin, bottom);
        }
        let lens_w = (viewport.width() * 0.42).clamp(sheet, f64::from(metrics.readable_width));
        // The workspace-name control that summons this lens lives on the
        // left. Keep the contextual surface on that same side instead of
        // making the pointer cross the entire display after clicking Home.
        let left = viewport.x0 + margin;
        Rect::new(left, top, left + lens_w, bottom)
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
    /// The footer band the close hint sits in, under every tab's body.
    const FOOTER_H: f64 = 44.0;

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
            card.y1 - Self::FOOTER_H,
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
        let hints = [ui::widget::KeyHint::new(
            ["Esc"],
            t("workspace-settings-hint-close"),
        )];
        let face = ui::widget::KeyHintsFace::new(&hints);
        let width = face.width(text_renderer, theme);
        let height = ui::widget::KeyHintsFace::height(theme);
        let inset = f64::from(theme.tokens.spacing.s6);
        let rect = Rect::new(
            card.x1 - inset - width,
            card.y1 - inset - height,
            card.x1 - inset,
            card.y1 - inset,
        );
        face.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            rect,
        );
    }

    pub fn general_body_rect(&self) -> Rect {
        let mut body = Self::body_rect_for(self.rect());
        body.y1 = (self.rect().y1 - self.general_footer_height).max(body.y0);
        body
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
        let content = 382.0
            + Self::devices_card_height(self.device_rows)
            + 16.0
            + PUBLIC_CARD_HEIGHT
            + 16.0
            + Self::archived_card_height(self.archived_rows);
        let max = (content * self.general_text_scale - self.general_body_rect().height()).max(0.0);
        let old = self.general_scroll_y;
        self.general_scroll_y = (old + delta).clamp(0.0, max);
        (old - self.general_scroll_y).abs() > f64::EPSILON
    }

    fn devices_card_height(rows: usize) -> f64 {
        let rows = f64::from(u32::try_from(rows).unwrap_or(u32::MAX));
        48.0 + rows * 40.0 + 56.0
    }

    /// The devices card under "At a glance": where this workspace shows,
    /// its place in the list, and Delete. Scroll applied.
    pub fn devices_card_rect(&self) -> Rect {
        let body = self.general_body_rect();
        let scale = self.general_text_scale;
        let top = body.y0 + 390.0 * scale - self.general_scroll_y;
        Rect::new(
            body.x0,
            top,
            body.x1,
            top + Self::devices_card_height(self.device_rows) * scale,
        )
    }

    /// The Public card under the devices card. Scroll applied.
    pub fn public_card_rect(&self) -> Rect {
        let devices = self.devices_card_rect();
        let scale = self.general_text_scale;
        let top = devices.y1 + 16.0 * scale;
        Rect::new(
            devices.x0,
            top,
            devices.x1,
            top + PUBLIC_CARD_HEIGHT * scale,
        )
    }

    /// Unscaled height of the archived card for `rows` workspaces, its gap
    /// above included; none when nothing is archived.
    fn archived_card_height(rows: usize) -> f64 {
        if rows == 0 {
            return 0.0;
        }
        let rows = f64::from(u32::try_from(rows).unwrap_or(u32::MAX));
        48.0 + rows * 40.0 + 8.0 + 16.0
    }

    /// The archived workspaces card under the Public card. Scroll applied.
    pub fn archived_card_rect(&self) -> Rect {
        let public = self.public_card_rect();
        let scale = self.general_text_scale;
        let height = Self::archived_card_height(self.archived_rows);
        if height <= 0.0 {
            return Rect::new(public.x0, public.y1, public.x1, public.y1);
        }
        let top = public.y1 + 16.0 * scale;
        Rect::new(public.x0, top, public.x1, top + (height - 16.0) * scale)
    }

    fn archived_card_hit(&self, point: Point) -> Option<EventOutcome> {
        let hit = |rect: &Rect| rect.area() > 0.0 && rect.contains(point);
        self.archived_rects
            .iter()
            .enumerate()
            .find_map(|(index, [restore, delete])| {
                if hit(restore) {
                    Some(EventOutcome::RestoreArchived(index))
                } else {
                    hit(delete).then_some(EventOutcome::DeleteArchived(index))
                }
            })
    }

    pub fn archived_rects(&self) -> &[[Rect; 2]] {
        &self.archived_rects
    }

    fn public_card_hit(&self, point: Point) -> Option<EventOutcome> {
        let hit = |rect: &Rect| rect.area() > 0.0 && rect.contains(point);
        let [switch, role, description] = &self.public_rects;
        if hit(switch) {
            return Some(EventOutcome::TogglePublic);
        }
        if hit(role) {
            return Some(EventOutcome::CycleDefaultRole);
        }
        hit(description).then_some(EventOutcome::EditDescription)
    }

    fn devices_card_hit(&self, point: Point) -> Option<EventOutcome> {
        let hit = |rect: &Rect| rect.area() > 0.0 && rect.contains(point);
        if let Some(index) = self.device_toggle_rects.iter().position(hit) {
            return Some(EventOutcome::ToggleDevice(index));
        }
        if hit(&self.move_rects[0]) {
            return Some(EventOutcome::MoveWorkspace(-1));
        }
        if hit(&self.move_rects[1]) {
            return Some(EventOutcome::MoveWorkspace(1));
        }
        hit(&self.delete_rect).then_some(EventOutcome::DeleteWorkspace)
    }

    pub fn device_toggle_rects(&self) -> &[Rect] {
        &self.device_toggle_rects
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
        let target = f64::from(ui::tokens::Metrics::BASE.min_pointer_target);
        let inset = f64::from(ui::tokens::Spacing::BASE.s3);
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
        self.transition = ui::motion::Presence::opened();
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.transition.is_moving(now)
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
        self.bot_add_rect = Rect::ZERO;
        self.bot_remove_rects.clear();
        self.agent_access = super::agent_access::AgentAccessLayout::default();
        self.member_sheet_option_rects.clear();
    }

    /// What a press at `point` on the Agents tab means: the agent view's
    /// controls, adding an agent, or a roster row's.
    fn agents_outcome(&self, point: Point, active_tab: WsTab) -> Option<EventOutcome> {
        if matches!(active_tab, WsTab::Agents) {
            if let Some(outcome) = self.agent_access_outcome(point) {
                return Some(outcome);
            }
            if self.agent_add_rect.area() > 0.0 && self.agent_add_rect.contains(point) {
                return Some(EventOutcome::AddAgent);
            }
            if self.bot_add_rect.area() > 0.0 && self.bot_add_rect.contains(point) {
                return Some(EventOutcome::AddBot);
            }
            if let Some((entry, _)) = self
                .bot_remove_rects
                .iter()
                .find(|(_, rect)| rect.area() > 0.0 && rect.contains(point))
            {
                return Some(EventOutcome::RemoveBot(entry.clone()));
            }
        }
        self.agent_control_rects
            .iter()
            .find_map(|(agent, review, remove)| {
                if review.area() > 0.0 && review.contains(point) {
                    Some(EventOutcome::ReviewAgent(agent.clone()))
                } else if remove.area() > 0.0 && remove.contains(point) {
                    Some(EventOutcome::RemoveAgent(agent.clone()))
                } else {
                    None
                }
            })
    }

    /// What a press at `point` in the agent view means, when it is shown.
    fn agent_access_outcome(&self, point: Point) -> Option<EventOutcome> {
        use super::agent_access::AccessTarget;
        let (agent, _) = self.agent_review.as_ref()?;
        let set = |answers| EventOutcome::SetAgentAnswers {
            agent: agent.clone(),
            answers,
        };
        Some(match self.agent_access.hit(point)? {
            AccessTarget::Back => EventOutcome::CloseAgentReview,
            AccessTarget::Expand(scope) => EventOutcome::ToggleAgentApp(scope),
            AccessTarget::Answer(scope, kind, answer) => set(vec![(scope, kind, Some(answer))]),
            AccessTarget::Reset(scope) => set(super::agent_access::KINDS
                .into_iter()
                .map(|kind| (scope.clone(), kind, None))
                .collect()),
            AccessTarget::RemoveException(scope, kind) => set(vec![(scope, kind, None)]),
            AccessTarget::ExpandModels => EventOutcome::ToggleAgentModels,
            AccessTarget::Model(model) => EventOutcome::SetAgentModel {
                agent: agent.clone(),
                model,
            },
        })
    }

    /// Show `agent`'s answers in the Agents tab instead of the roster.
    pub fn open_agent_review(&mut self, agent: String) {
        self.agent_review = Some((agent, None));
        self.agent_models_open = false;
        self.agent_access_scroll = 0.0;
        self.agent_access_focus = None;
    }

    /// The models the person may choose for the reviewed agent; `None`
    /// while they have no model provider.
    pub fn set_agent_models(&mut self, models: Option<super::agent_access::AgentModels>) {
        self.agent_models = models;
    }

    /// Show or hide the models the reviewed agent may plan with.
    pub fn toggle_agent_models(&mut self) {
        self.agent_models_open = !self.agent_models_open;
    }

    /// Back to the roster.
    pub fn close_agent_review(&mut self) {
        self.agent_review = None;
        self.agent_models = None;
        self.agent_models_open = false;
        self.agent_access = super::agent_access::AgentAccessLayout::default();
        self.agent_access_scroll = 0.0;
        self.agent_access_focus = None;
    }

    /// Scroll the agent view's app list by `delta`, within what it holds.
    /// Whether anything moved.
    pub fn scroll_agent_access(&mut self, delta: f64) -> bool {
        if !delta.is_finite() || self.agent_review.is_none() {
            return false;
        }
        let old = self.agent_access_scroll;
        self.agent_access_scroll = (old + delta).clamp(0.0, self.agent_access.max_scroll());
        (old - self.agent_access_scroll).abs() > f64::EPSILON
    }

    /// The agent view's keyboard: Tab walks its controls, the arrows change
    /// the focused Allow / Ask first / Deny answer, Enter or Space presses
    /// the focused button, and Escape goes back to the roster. `None` when
    /// the view is not shown or the key changes nothing but focus.
    pub fn handle_agent_access_key(&mut self, key: AgentAccessKey) -> Option<EventOutcome> {
        use super::agent_access::{ANSWERS, AccessFocus, KINDS};
        let (agent, _) = self.agent_review.clone()?;
        let stops: Vec<AccessFocus> = self
            .agent_access
            .stops
            .iter()
            .map(|(stop, _)| stop.clone())
            .collect();
        let current = self
            .agent_access_focus
            .as_ref()
            .and_then(|focus| stops.iter().position(|stop| stop == focus));
        let set = |answers| EventOutcome::SetAgentAnswers {
            agent: agent.clone(),
            answers,
        };
        match key {
            AgentAccessKey::Next | AgentAccessKey::Previous => {
                let next = match (current, key == AgentAccessKey::Previous) {
                    (Some(0) | None, true) => stops.len().checked_sub(1),
                    (Some(index), true) => index.checked_sub(1),
                    (Some(index), false) => index.checked_add(1).filter(|next| *next < stops.len()),
                    (None, false) => None,
                }
                .unwrap_or_default();
                self.agent_access_focus = stops.get(next).cloned();
                self.reveal_agent_access_focus();
                None
            }
            AgentAccessKey::Back => Some(EventOutcome::CloseAgentReview),
            AgentAccessKey::Left | AgentAccessKey::Right => {
                let Some(AccessFocus::Answers(scope, kind)) = self.agent_access_focus.clone()
                else {
                    return None;
                };
                let now = self.agent_access.answer_shown(&scope, kind)?;
                let index = ANSWERS.iter().position(|answer| *answer == now)?;
                let next = if key == AgentAccessKey::Left {
                    index.checked_sub(1)?
                } else {
                    index.checked_add(1)?
                };
                let answer = *ANSWERS.get(next)?;
                Some(set(vec![(scope, kind, Some(answer))]))
            }
            AgentAccessKey::Activate => match self.agent_access_focus.clone()? {
                AccessFocus::Back => Some(EventOutcome::CloseAgentReview),
                AccessFocus::Expand(scope) => Some(EventOutcome::ToggleAgentApp(scope)),
                AccessFocus::Answers(..) => None,
                AccessFocus::Reset(scope) => Some(set(KINDS
                    .into_iter()
                    .map(|kind| (scope.clone(), kind, None))
                    .collect())),
                AccessFocus::RemoveException(scope, kind) => Some(set(vec![(scope, kind, None)])),
                AccessFocus::ExpandModels => Some(EventOutcome::ToggleAgentModels),
                AccessFocus::Model(model) => Some(EventOutcome::SetAgentModel {
                    agent: agent.clone(),
                    model,
                }),
            },
        }
    }

    /// Give `focus` the keyboard, as Tab would reach it.
    pub fn focus_agent_access(&mut self, focus: super::agent_access::AccessFocus) {
        self.agent_access_focus = Some(focus);
        self.reveal_agent_access_focus();
    }

    /// Scroll the focused control fully into the list's region.
    fn reveal_agent_access_focus(&mut self) {
        let Some(rect) = self
            .agent_access_focus
            .as_ref()
            .and_then(|focus| self.agent_access.stop_rect(focus))
        else {
            return;
        };
        let view = self.agent_access.viewport;
        if matches!(
            self.agent_access_focus,
            Some(super::agent_access::AccessFocus::Back)
        ) {
            return;
        }
        let delta = if rect.y0 < view.y0 {
            rect.y0 - view.y0 - 4.0
        } else if rect.y1 > view.y1 {
            rect.y1 - view.y1 + 4.0
        } else {
            0.0
        };
        self.scroll_agent_access(delta);
    }

    /// Open `scope`'s answers in the agent view, or close them when open.
    pub fn toggle_agent_app(&mut self, scope: tilo_action_policy::agent_answers::AnswerScope) {
        if let Some((_, open)) = &mut self.agent_review {
            *open = if open.as_ref() == Some(&scope) {
                None
            } else {
                Some(scope)
            };
        }
    }

    /// The agent whose answers are shown, if any.
    #[must_use]
    pub fn reviewing_agent(&self) -> Option<&str> {
        self.agent_review.as_ref().map(|(agent, _)| agent.as_str())
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
        let now = ui::time::now();
        // A click on the workspace pill opens it; a shortcut lands at once
        // (the host has no source here, and a click is the common case).
        self.transition.show(
            open,
            ui::motion::Appear::SPEC,
            theme.motion(),
            ui::tokens::MotionSource::Pointer,
            now,
        );
        if !self.transition.is_visible(now) {
            return None;
        }
        let rect = self.rect();
        // The one Panel entrance (HIG §9.7, `ui::motion::Appear`): the sheet
        // comes from the control that opened it and goes back to it.
        let origin = anchor.filter(|anchor| anchor.area() > 0.0).map_or_else(
            || rect.center(),
            |anchor| ui::motion::Appear::origin(rect, anchor.center()),
        );
        let pose = ui::motion::Appear::pose(self.transition.progress(now), origin, theme.motion());
        let in_flight = !pose.is_rest();
        if in_flight {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                pose.opacity,
                transform,
                &self.viewport,
            );
        }
        let panel_transform = pose.applied_to(transform);
        // Panel role (HIG §9.1–§9.3): the Panel radius, an elevation-token
        // shadow that follows the sheet's own corners and fades with it,
        // `panel_ground` and the derived Panel border.
        // `draw_contents` draws on it with `ui::chrome::shell::on_panel`.
        let _ = ui::chrome::shell::paint_panel(scene, panel_transform, rect, theme, 1.0);
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
        if let Some(outcome) = self.agents_outcome(point, active_tab) {
            return outcome;
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
        if matches!(active_tab, WsTab::General)
            && let Some(outcome) = self.devices_card_hit(point)
        {
            return outcome;
        }
        if matches!(active_tab, WsTab::General)
            && let Some(outcome) = self.public_card_hit(point)
        {
            return outcome;
        }
        if matches!(active_tab, WsTab::General)
            && let Some(outcome) = self.archived_card_hit(point)
        {
            return outcome;
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

    pub fn draw_contents(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        model: &WorkspaceSettingsModel<'_>,
    ) {
        // Everything here stands on the sheet's panel ground, not the
        // window's: fields and secondary buttons step off the panel as they
        // step off a window, in dark as in light (`Theme::on_ground`).
        let theme = &ui::chrome::shell::on_panel(theme);
        let tokens = &theme.tokens;

        // `ws_settings_rect()` is the single source of truth for the
        // centered-modal geometry; hit-tests reach for the same helper
        // so a width bump can't desync the card vs. the clickable bounds.
        let rect = self.rect();

        let active_name =
            self.draw_ws_settings_header_text(scene, transform, text_renderer, theme, rect, model);

        // The selected tab is kept orthogonal to the name-edit buffer.
        let active_tab = model.tab;
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
            model,
        );

        if fully_painted {
            self.draw_member_sheet(scene, transform, text_renderer, theme, rect, model);
            return;
        }

        // Unreachable today (every non-General arm returns `true` above),
        // but guards against a new `WsTab` variant falling through to
        // paint the General body over its pane.
        if !matches!(active_tab, WsTab::General) {
            return;
        }

        self.general_text_scale = f64::from(theme.text_scale * theme.ui_scale);
        let editing = model.owner_controls && model.editing_name.is_some();
        let hint = Self::footer_hints(editing);
        let footer = Self::footer_hint_rect(rect, &hint, text_renderer, theme);
        self.general_footer_height = rect.y1 - footer.y0 + f64::from(tokens.spacing.s2);
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
        self.draw_ws_settings_name_input(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            content_rect,
            model,
            active_name.as_str(),
        );
        let can_invite = model
            .desktop
            .active_workspace()
            .is_some_and(|workspace| workspace.may_administer(model.acting_user));
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
            model,
            collab_y,
        );
        self.draw_ws_settings_devices(scene, transform, text_renderer, theme, model);
        self.draw_ws_settings_public(scene, transform, text_renderer, theme, model);
        self.draw_ws_settings_archived(scene, transform, text_renderer, theme, model);

        scene.pop_layer();

        ui::widget::KeyHintsFace::new(&hint).paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            footer,
        );
        self.draw_member_sheet(scene, transform, text_renderer, theme, rect, model);
    }

    /// What the keys do in the General pane's footer: save or cancel while
    /// a name is being edited, close otherwise.
    pub fn footer_hints(editing: bool) -> Vec<ui::widget::KeyHint> {
        use ui::widget::KeyHint;
        if editing {
            vec![
                KeyHint::new(["Enter"], t("workspace-settings-hint-save")),
                KeyHint::new(["Esc"], t("workspace-settings-hint-cancel")),
            ]
        } else {
            vec![KeyHint::new(["Esc"], t("workspace-settings-hint-close"))]
        }
    }

    /// Where the footer's key hints stand: at the pane's trailing bottom
    /// corner, one content gutter in.
    pub fn footer_hint_rect(
        rect: Rect,
        hints: &[ui::widget::KeyHint],
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Rect {
        let width = ui::widget::KeyHintsFace::new(hints).width(text, theme);
        let height = ui::widget::KeyHintsFace::height(theme);
        let inset = f64::from(theme.tokens.spacing.s6);
        Rect::new(
            rect.x1 - inset - width,
            rect.y1 - inset - height,
            rect.x1 - inset,
            rect.y1 - inset,
        )
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
        model: &WorkspaceSettingsModel<'_>,
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
        let painted = ui::chrome::shell::paint_panel(scene, transform, card, theme, 1.0);
        let theme = &painted.theme;

        let (title, description, subject): (String, String, Option<String>) = match &sheet {
            MemberSheet::Role(user) => {
                let current = model
                    .desktop
                    .active_workspace()
                    .and_then(|workspace| workspace.role_of(user))
                    .unwrap_or(CapRole::Viewer);
                (
                    t("workspace-settings-role-sheet-title"),
                    t_args(
                        "workspace-settings-role-sheet-description",
                        &[
                            ("person", super::short_id(user).into()),
                            ("role", super::role_label(current).into()),
                        ],
                    ),
                    None,
                )
            }
            MemberSheet::Remove(user) => (
                t("workspace-settings-remove-sheet-title"),
                t("workspace-settings-remove-sheet-description"),
                Some(super::short_id(user)),
            ),
            MemberSheet::Revoke(cap_id) => (
                t("workspace-settings-revoke-sheet-title"),
                t("workspace-settings-revoke-sheet-description"),
                Some(super::short_id(cap_id)),
            ),
        };
        text_renderer.draw_style(
            scene,
            &title,
            colors.text_primary,
            transform.pre_translate((card.x0 + 20.0, card.y0 + 20.0).into()),
            None,
            &tokens.typography.style(TextRole::Heading),
        );
        if let Some(subject) = subject {
            text_renderer.draw_style(
                scene,
                &subject,
                accent_text_on(theme, theme.tokens.colors.surfaces.content),
                transform.pre_translate((card.x0 + 20.0, card.y0 + 48.0).into()),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
        }
        let desc_y = if role_sheet {
            card.y0 + 50.0
        } else {
            card.y0 + 72.0
        };
        let description = text_renderer.elide_to_width_style(
            &description,
            f32_of_f64(card.width() - 40.0),
            &tokens.typography.style(TextRole::Caption),
        );
        text_renderer.draw_style(
            scene,
            &description,
            colors.text_dim,
            transform.pre_translate((card.x0 + 20.0, desc_y).into()),
            None,
            &tokens.typography.style(TextRole::Caption),
        );

        self.member_sheet_option_rects.clear();
        let option_x0 = card.x0 + 20.0;
        let option_x1 = card.x1 - 20.0;
        let (labels, previews): (Vec<String>, Vec<String>) = match &sheet {
            MemberSheet::Role(_) => {
                let roles = [CapRole::Viewer, CapRole::Commenter, CapRole::Editor];
                (
                    roles.iter().map(|role| super::role_label(*role)).collect(),
                    roles
                        .iter()
                        .map(|role| super::role_capability_preview(*role))
                        .collect(),
                )
            }
            MemberSheet::Remove(_) => (
                vec![
                    t("workspace-settings-keep-access"),
                    t("workspace-settings-review-activity"),
                    t("workspace-settings-remove-person"),
                ],
                vec![
                    t("workspace-settings-no-changes"),
                    t("workspace-settings-open-history"),
                    t("workspace-settings-remove-person-detail"),
                ],
            ),
            MemberSheet::Revoke(_) => (
                vec![
                    t("workspace-settings-keep-permission"),
                    t("workspace-settings-review-activity"),
                    t("workspace-settings-revoke-permission"),
                ],
                vec![
                    t("workspace-settings-no-changes"),
                    t("workspace-settings-open-history"),
                    t("workspace-settings-revoke-permission-detail"),
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
            let y0 = start_y + f64_of_usize(index) * (row_h + gap);
            let option = Rect::new(option_x0, y0, option_x1, y0 + row_h);
            self.member_sheet_option_rects.push(option);
            let focused = index == self.member_sheet_focus;
            let destructive = index == 2 && !role_sheet;
            let tint = if destructive {
                colors.semantic.danger
            } else {
                colors.intent.accent
            };
            let rr = RoundedRect::from_rect(option, f64::from(tokens.radii.control));
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
            text_renderer.draw_style(
                scene,
                label,
                colors.text_primary,
                transform.pre_translate((option.x0 + 12.0, option.y0 + 9.0).into()),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
            if role_sheet {
                text_renderer.draw_style(
                    scene,
                    preview,
                    colors.text_dim,
                    transform.pre_translate((option.x0 + 12.0, option.y0 + 29.0).into()),
                    None,
                    &tokens.typography.style(TextRole::Caption),
                );
            }
        }
        let hints = [
            ui::widget::KeyHint::new(["Up", "Down"], t("workspace-settings-hint-choose")),
            ui::widget::KeyHint::new(["Enter"], t("workspace-settings-hint-apply")),
            ui::widget::KeyHint::new(["Esc"], t("workspace-settings-hint-cancel")),
        ];
        let height = ui::widget::KeyHintsFace::height(theme);
        let inset = f64::from(tokens.spacing.s4);
        ui::widget::KeyHintsFace::new(&hints).paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            Rect::new(
                card.x0 + inset,
                card.y1 - inset - height,
                card.x1 - inset,
                card.y1 - inset,
            ),
        );
    }

    /// Title plus the "Active workspace: …" line; returns the active
    /// workspace's name so the General tab's name input can fall back
    /// to it while nothing is being edited.
    fn draw_ws_settings_header_text(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        model: &WorkspaceSettingsModel<'_>,
    ) -> String {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let (identity, close) =
            self.draw_ws_settings_identity_card_chrome(scene, transform, theme, rect);
        if theme.text_scale > 1.0 {
            let name = model.desktop.active_workspace().map_or_else(
                || t("workspace-settings-workspace-fallback"),
                |workspace| workspace.name.clone(),
            );
            let role = model
                .desktop
                .active_workspace()
                .and_then(|workspace| workspace.role_of(model.acting_user))
                .map_or_else(|| t("workspace-settings-role-guest"), super::role_label);
            let width = f32_of_f64(close.x0 - identity.x0 - 88.0);
            let caption = text_renderer.elide_to_width_style(
                &role,
                width,
                &tokens.typography.style(TextRole::Caption),
            );
            let name = text_renderer.elide_to_width_style(
                &name,
                width,
                &tokens.typography.style(TextRole::Body),
            );
            text_renderer.draw_style(
                scene,
                &caption,
                accent_text_on(theme, colors.surfaces.raised),
                transform.pre_translate((identity.x0 + 76.0, identity.y0 + 8.0).into()),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                &name,
                colors.text_primary,
                transform.pre_translate((identity.x0 + 76.0, identity.y0 + 38.0).into()),
                None,
                &tokens.typography.style(TextRole::Body),
            );
            return model.desktop.active_workspace().map_or_else(
                || t("workspace-settings-workspace-fallback"),
                |workspace| workspace.name.clone(),
            );
        }
        text_renderer.draw_style(
            scene,
            &t("workspace-settings-current-workspace"),
            accent_text_on(theme, colors.surfaces.raised),
            transform.pre_translate((identity.x0 + 76.0, identity.y0 + 12.0).into()),
            None,
            &tokens.typography.style(TextRole::Caption),
        );
        let active_name = model.desktop.active_workspace().map_or_else(
            || t("workspace-settings-workspace-fallback"),
            |w| w.name.clone(),
        );
        let tsize = tokens.typography.style(TextRole::Title);
        let shown_name = text_renderer.elide_to_width_style(
            &active_name,
            f32_of_f64(close.x0 - (identity.x0 + 76.0) - 12.0),
            &tsize,
        );
        text_renderer.draw_style(
            scene,
            &shown_name,
            colors.text_primary,
            transform.pre_translate((identity.x0 + 76.0, identity.y0 + 34.0).into()),
            None,
            &tsize,
        );
        let active_role = model
            .desktop
            .active_workspace()
            .and_then(|workspace| workspace.role_of(model.acting_user))
            .map_or_else(|| t("workspace-settings-role-guest"), super::role_label);
        let label = text_renderer.elide_to_width_style(
            &active_role,
            f32_of_f64(identity.x1 - identity.x0 - 92.0),
            &tokens.typography.style(TextRole::Caption),
        );
        text_renderer.draw_style(
            scene,
            &label,
            colors.text_dim,
            transform.pre_translate((identity.x0 + 76.0, identity.y0 + 58.0).into()),
            None,
            &tokens.typography.style(TextRole::Caption),
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
            f64::from(tokens.radii.control),
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
            colors.text_primary.with_alpha_factor(IDENTITY_TILE_WASH),
            None,
            &RoundedRect::from_rect(mark, f64::from(tokens.radii.control)),
        );
        ui::icons::draw_icon(
            scene,
            transform,
            mark.inset(-14.0),
            ui::icons::Icon::LayoutGrid,
            accent_icon_on_tile(theme, IDENTITY_TILE_WASH),
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
            close.inset(-8.0),
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
        let label_style = tokens.typography.style(TextRole::Body);
        let tabs = WsTab::all();
        let nav = Self::nav_rect_for(rect);
        // The strip's track: a quiet lift under the Selector pills and a
        // hairline derived against the panel ground (HIG §9.2/§9.5), not a
        // ramp slot inside a faded declared border.
        let nav_rr = RoundedRect::from_rect(nav, f64::from(tokens.radii.control));
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
        let segment_w = nav.width() / f64_of_usize(tabs.len());
        // Every pill gets the same treatment: the plan is measured once for
        // the whole strip, so the strip never shows one elided label next to
        // four intact ones or one icon next to four bare labels.
        let plan = tab_strip_plan(
            text_renderer,
            &label_style,
            segment_w - 2.0 * Self::PILL_INSET,
        );
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
                f64::from(text_renderer.measure_style(&label, None, &label_style).0) + reserved
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
            let label = text_renderer.elide_to_width_style(
                &label,
                f32_of_f64(pill_rect.width() - icon_size - icon_gap - 8.0),
                &label_style,
            );
            let (w, h) = text_renderer.measure_style(&label, None, &label_style);
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
                    if active {
                        colors.intent.accent_hover
                    } else {
                        color
                    },
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
            text_renderer.draw_style(
                scene,
                &label,
                color,
                transform.pre_translate(
                    (
                        content_x + icon_size + icon_gap,
                        pill_rect.center().y - f64::from(h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &label_style,
            );
        }
        self.tab_pill_rects = pill_rects;
    }

    /// Dispatches to the Members/Permissions/Apps tab body when one of
    /// those is active and fully paints it; returns `true` when it did,
    /// so the caller knows to stop. Returns `false` on `General`, whose
    /// clearing happens here too, but whose body the caller continues
    /// painting below.
    fn draw_ws_settings_dispatch_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        active_tab: WsTab,
        model: &WorkspaceSettingsModel<'_>,
    ) -> bool {
        match active_tab {
            WsTab::Members => {
                self.draw_ws_settings_members_tab(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    rect,
                    model,
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
                    model,
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
                    model,
                );
                true
            }
            WsTab::Apps => {
                self.draw_ws_settings_apps_tab(scene, transform, text_renderer, theme, rect, model);
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
    fn draw_ws_settings_members_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        model: &WorkspaceSettingsModel<'_>,
    ) {
        self.revoke_rects.clear();
        self.grant_composer_rects = None;
        self.permission_profiles_rect = Rect::ZERO;
        self.member_control_rects.clear();
        self.ownership_action_rect = Rect::ZERO;
        self.agent_add_rect = Rect::ZERO;
        self.agent_control_rects.clear();
        let body_rect = Self::body_rect(rect);
        let is_owner = model.owner_controls;
        if let Some(ws) = model.desktop.active_workspace() {
            let actor = model.acting_user;
            if super::ownership_button(ws, actor).is_some() {
                self.ownership_action_rect = ownership_action_rect(body_rect);
            }
            for row in members_pane_rows(ws, body_rect) {
                let (role, remove) = super::member_row_controls(&row, actor);
                self.member_control_rects
                    .push((row.user.clone(), role, remove));
            }
            draw_members_pane(
                scene,
                transform,
                body_rect,
                (ws, model.people),
                model.acting_user,
                is_owner,
                text_renderer,
                theme,
            );
        }
        Self::draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
    }

    /// The Agents tab body: an explicitly non-human roster backed by the
    /// same signed membership and revocation policy as the broker.
    fn draw_ws_settings_agents_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        model: &WorkspaceSettingsModel<'_>,
    ) {
        self.revoke_rects.clear();
        self.grant_composer_rects = None;
        self.member_control_rects.clear();
        self.ownership_action_rect = Rect::ZERO;
        self.agent_control_rects.clear();
        self.bot_add_rect = Rect::ZERO;
        self.bot_remove_rects.clear();
        self.default_app_rects.clear();
        self.agent_access = super::agent_access::AgentAccessLayout::default();
        let body_rect = Self::body_rect(rect);
        let is_owner = model.owner_controls;
        if let Some(ws) = model.desktop.active_workspace()
            && let Some((agent, open)) = &self.agent_review
            && ws
                .agent_members_at(tilo_clock::unix_now())
                .iter()
                .any(|(member, _)| member == agent)
        {
            self.agent_add_rect = Rect::ZERO;
            self.agent_access = super::agent_access::draw_agent_access(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                body_rect,
                (ws, model.apps),
                &super::agent_access::AgentAccessView {
                    agent,
                    open: open.as_ref(),
                    editable: is_owner,
                    scroll: self.agent_access_scroll,
                    focus: self.agent_access_focus.as_ref(),
                    models: self.agent_models.as_ref(),
                    models_open: self.agent_models_open,
                },
            );
            // Rows can close or answers go away between frames: keep the
            // scroll within what the list now holds.
            self.agent_access_scroll = self.agent_access_scroll.min(self.agent_access.max_scroll());
            Self::draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
            return;
        }
        self.agent_add_rect = agents_add_rect(body_rect);
        if let Some(ws) = model.desktop.active_workspace() {
            self.bot_add_rect = super::bots_add_rect(ws, body_rect);
            for row in super::bots_pane_rows(ws, body_rect) {
                if let Some(entry) = row.bot.entry.clone() {
                    self.bot_remove_rects.push((entry, row.remove_rect));
                }
            }
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
    fn draw_ws_settings_permissions_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        model: &WorkspaceSettingsModel<'_>,
    ) {
        let body_rect = Self::body_rect(rect);
        let is_owner = model.owner_controls;
        let draft = model.grant_draft;
        self.revoke_rects.clear();
        self.grant_composer_rects = None;
        self.permission_profiles_rect = Rect::ZERO;
        self.member_control_rects.clear();
        self.ownership_action_rect = Rect::ZERO;
        self.agent_add_rect = Rect::ZERO;
        if let Some(ws) = model.desktop.active_workspace() {
            if is_owner {
                self.permission_profiles_rect = permission_profiles_rect(body_rect);
            }
            let layout = permissions_pane_layout(ws, body_rect, model.apps, model.acting_user);
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
                (ws, model.apps, model.acting_user),
                &draft,
                is_owner,
                text_renderer,
                theme,
            );
        }
        Self::draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
    }

    /// The Apps tab body: default-app rows, then the shared close hint.
    fn draw_ws_settings_apps_tab(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        model: &WorkspaceSettingsModel<'_>,
    ) {
        self.revoke_rects.clear();
        self.grant_composer_rects = None;
        self.member_control_rects.clear();
        self.ownership_action_rect = Rect::ZERO;
        self.agent_add_rect = Rect::ZERO;
        self.default_app_rects.clear();
        let body_rect = Self::body_rect(rect);
        let candidates = model.apps.default_app_candidates();
        if let Some(ws) = model.desktop.active_workspace() {
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
                    local_user: model.acting_user,
                    candidates: &candidates,
                },
                text_renderer,
                theme,
            );
        }
        Self::draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
    }

    fn draw_ws_settings_name_input(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        model: &WorkspaceSettingsModel<'_>,
        active_name: &str,
    ) -> bool {
        let (scene, transform, text_renderer, theme) =
            (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body = Self::body_rect(rect);
        let identity_card = Rect::new(
            body.x0,
            body.y0 + 8.0 * f64::from(theme.text_scale * theme.ui_scale),
            body.x1,
            body.y0 + 112.0 * f64::from(theme.text_scale * theme.ui_scale),
        );
        ui::surface::card(
            scene,
            transform,
            identity_card,
            f64::from(tokens.radii.control),
            colors.surfaces.raised,
        );
        text_renderer.draw_style(
            scene,
            &t("workspace-settings-identity-title"),
            colors.text_primary,
            transform.pre_translate(
                (
                    identity_card.x0 + 16.0,
                    identity_card.y0 + 13.0 * f64::from(theme.text_scale * theme.ui_scale),
                )
                    .into(),
            ),
            None,
            &tokens.typography.style(TextRole::Body),
        );
        let description = text_renderer.elide_to_width_style(
            &t("workspace-settings-identity-description"),
            f32_of_f64(identity_card.width() - 32.0),
            &tokens.typography.style(TextRole::Caption),
        );
        text_renderer.draw_style(
            scene,
            &description,
            colors.text_dim,
            transform.pre_translate(
                (
                    identity_card.x0 + 16.0,
                    identity_card.y0 + 36.0 * f64::from(theme.text_scale * theme.ui_scale),
                )
                    .into(),
            ),
            None,
            &tokens.typography.style(TextRole::Caption),
        );
        let can_rename = model.owner_controls;
        let editing = can_rename && model.editing_name.is_some();
        let shown = if editing {
            model.editing_name.unwrap_or(active_name)
        } else {
            active_name
        };
        self.paint_name_field(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            model,
            shown,
        );
        if !editing && !can_rename {
            text_renderer.draw_style(
                scene,
                &t("workspace-settings-rename-admin-only"),
                colors.text_dim,
                transform.pre_translate((identity_card.x0 + 16.0, identity_card.y1 - 12.0).into()),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
        }
        editing
    }

    fn paint_name_field(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        model: &WorkspaceSettingsModel<'_>,
        shown: &str,
    ) {
        use ui::widget::{FieldFace, TextInput, Widget};
        let editing = model.owner_controls && model.editing_name.is_some();
        let face = FieldFace::new(self.name_rect(), shown)
            .focused(editing)
            .disabled(!model.owner_controls);
        if !editing {
            if let Some(input) = &mut self.name_input {
                input.set_focus(false);
            }
            face.paint(ctx.scene, ctx.transform, ctx.text, ctx.theme);
            return;
        }
        face.paint_chrome(ctx.scene, ctx.transform, ctx.theme);
        let input = self.name_input.get_or_insert_with(|| {
            TextInput::new(Vec::new())
                .with_plain_chrome()
                .with_spellcheck(false)
                .with_autocorrect(false)
        });
        input.set_text(shown);
        let painted_selection = input
            .selection_anchor_focus_public()
            .unwrap_or_else(|| (input.cursor_pos(), input.cursor_pos()));
        if painted_selection != (model.name_selection.anchor, model.name_selection.cursor) {
            input.set_selection(model.name_selection.anchor, model.name_selection.cursor);
        }
        input.set_focus(true);
        input.apply_layout_external(face.text_rect(ctx.theme));
        // Embedded TextInput shapes its stock role at text_scale; unlike
        // FieldFace it does not read the UI-scaled typography token.
        let input_theme = ctx
            .theme
            .clone()
            .with_text_scale(ctx.theme.text_scale * ctx.theme.ui_scale);
        input.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            &input_theme,
            &ui::presence::PresenceCtx::default(),
        );
    }

    /// Sharing card that opens the signed invite composer.
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
        let invite_y = body.y0 + 202.0 * f64::from(theme.text_scale * theme.ui_scale);
        let url_pill = Rect::new(
            sharing_card.x0 + 16.0,
            invite_y,
            sharing_card.x1 - 24.0 - 72.0 * f64::from(theme.text_scale * theme.ui_scale),
            invite_y + 34.0 * f64::from(theme.text_scale * theme.ui_scale),
        );
        ui::surface::card(
            scene,
            transform,
            url_pill,
            f64::from(tokens.radii.detail),
            colors.surfaces.base,
        );
        let invite_url = t(if can_invite {
            "workspace-settings-invite-hint"
        } else {
            "workspace-settings-invite-admin-only"
        });
        let invite_url = text_renderer.elide_to_width_style(
            &invite_url,
            f32_of_f64(url_pill.width() - 24.0),
            &tokens.typography.style(TextRole::Caption),
        );
        text_renderer.draw_style(
            scene,
            &invite_url,
            colors.text_primary,
            transform.pre_translate((url_pill.x0 + 12.0, url_pill.y0 + 9.0).into()),
            None,
            &tokens.typography.style(TextRole::Caption),
        );
        let invite_btn = self.invite_rect();
        let invite_label = t("workspace-settings-invite-create");
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(invite_btn, &invite_label)
                .role(ui::widget::ButtonRole::Primary)
                .disabled(!can_invite),
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
            body.y0 + 128.0 * f64::from(theme.text_scale * theme.ui_scale),
            body.x1,
            body.y0 + 252.0 * f64::from(theme.text_scale * theme.ui_scale),
        );
        ui::surface::card(
            scene,
            transform,
            sharing_card,
            f64::from(tokens.radii.control),
            colors.surfaces.raised,
        );
        let share_icon = Rect::new(
            sharing_card.x0 + 16.0,
            sharing_card.y0 + 14.0 * f64::from(theme.text_scale * theme.ui_scale),
            sharing_card.x0 + 48.0,
            sharing_card.y0 + 46.0 * f64::from(theme.text_scale * theme.ui_scale),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.text_primary.with_alpha_factor(SHARE_TILE_WASH),
            None,
            &RoundedRect::from_rect(share_icon, f64::from(tokens.radii.control)),
        );
        ui::icons::draw_icon(
            scene,
            transform,
            share_icon.inset(-8.0),
            ui::icons::Icon::Copy,
            accent_icon_on_tile(theme, SHARE_TILE_WASH),
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        text_renderer.draw_style(
            scene,
            &t("workspace-settings-invite-title"),
            colors.text_primary,
            transform.pre_translate(
                (
                    sharing_card.x0 + 60.0,
                    sharing_card.y0 + 13.0 * f64::from(theme.text_scale * theme.ui_scale),
                )
                    .into(),
            ),
            None,
            &tokens.typography.style(TextRole::Body),
        );
        let description = text_renderer.elide_to_width_style(
            &t("workspace-settings-invite-description"),
            f32_of_f64(sharing_card.width() - 76.0),
            &tokens.typography.style(TextRole::Caption),
        );
        text_renderer.draw_style(
            scene,
            &description,
            colors.text_dim,
            transform.pre_translate(
                (
                    sharing_card.x0 + 60.0,
                    sharing_card.y0 + 36.0 * f64::from(theme.text_scale * theme.ui_scale),
                )
                    .into(),
            ),
            None,
            &tokens.typography.style(TextRole::Caption),
        );
        sharing_card
    }

    /// Paint the devices card and record its controls' rects. A workspace
    /// that is not the person's own lists no devices but still offers its
    /// place in the list and Delete.
    #[allow(
        clippy::too_many_lines,
        reason = "one card painted top to bottom from shared running offsets, recording each control's rect as it goes"
    )]
    fn draw_ws_settings_devices(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        model: &WorkspaceSettingsModel<'_>,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let scale = f64::from(theme.text_scale * theme.ui_scale);
        self.device_rows = model.devices.len();
        let card = self.devices_card_rect();
        ui::surface::card(
            scene,
            transform,
            card,
            f64::from(tokens.radii.control),
            colors.surfaces.raised,
        );
        let style = theme.text(TextRole::Caption);
        let text = |text_renderer: &mut TextRenderer,
                    scene: &mut Scene,
                    label: &str,
                    color,
                    x: f64,
                    y: f64| {
            text_renderer.draw_style(
                scene,
                label,
                color,
                transform.pre_translate((x, y).into()),
                None,
                &style,
            );
        };
        let title = t("workspace-settings-devices-title");
        text_renderer.draw_style(
            scene,
            &title,
            colors.text_primary,
            transform.pre_translate((card.x0 + 16.0, card.y0 + 16.0 * scale).into()),
            None,
            &theme.text(TextRole::Body),
        );
        // What stays on each device, beside the devices it is about: the
        // workspace's content is shared, its arrangement here is not.
        let (title_w, _) = text_renderer.measure_style(&title, None, &theme.text(TextRole::Body));
        let local_x = card.x0 + 16.0 + f64::from(title_w) + 12.0;
        let local = text_renderer.elide_to_width_style(
            &t("workspace-settings-devices-local"),
            f32_of_f64((card.x1 - 16.0 - local_x).max(0.0)),
            &style,
        );
        text(
            text_renderer,
            scene,
            &local,
            colors.text_dim,
            local_x,
            card.y0 + 18.0 * scale,
        );
        let button = |scene: &mut Scene, text_renderer: &mut TextRenderer, face: CardButton<'_>| {
            paint_card_button(scene, transform, text_renderer, theme, face);
        };
        self.device_toggle_rects.clear();
        let mut y = card.y0 + 48.0 * scale;
        for device in model.devices {
            let label = if device.here {
                t_args(
                    "workspace-settings-device-here",
                    &[("device", device.label.clone().into())],
                )
            } else {
                device.label.clone()
            };
            text(
                text_renderer,
                scene,
                &label,
                colors.text_primary,
                card.x0 + 16.0,
                y + 10.0 * scale,
            );
            let toggle = Rect::new(
                card.x1 - 16.0 - 96.0 * scale,
                y,
                card.x1 - 16.0,
                y + 32.0 * scale,
            );
            // A shown device is the toggle pressed in, never an accent.
            let label = t(if device.shown {
                "workspace-settings-device-shown"
            } else {
                "workspace-settings-device-hidden"
            });
            button(
                scene,
                text_renderer,
                CardButton {
                    toggled: device.shown,
                    ..CardButton::new(toggle, &label)
                },
            );
            self.device_toggle_rects.push(toggle);
            y += 40.0 * scale;
        }
        let row = y + 8.0 * scale;
        let first = model
            .desktop
            .workspaces
            .first()
            .is_some_and(|workspace| workspace.id == model.desktop.active);
        let last = model
            .desktop
            .workspaces
            .last()
            .is_some_and(|workspace| workspace.id == model.desktop.active);
        let earlier = Rect::new(
            card.x0 + 16.0,
            row,
            card.x0 + 16.0 + 96.0 * scale,
            row + 32.0 * scale,
        );
        let later = Rect::new(
            earlier.x1 + 8.0,
            row,
            earlier.x1 + 8.0 + 96.0 * scale,
            row + 32.0 * scale,
        );
        for (rect, key, enabled) in [
            (earlier, "workspace-settings-move-earlier", !first),
            (later, "workspace-settings-move-later", !last),
        ] {
            let label = t(key);
            button(
                scene,
                text_renderer,
                CardButton {
                    enabled,
                    ..CardButton::new(rect, &label)
                },
            );
        }
        self.move_rects = [
            if first { Rect::ZERO } else { earlier },
            if last { Rect::ZERO } else { later },
        ];
        let deletable = model.desktop.workspaces.len() > 1;
        let delete_label = t("workspace-settings-delete");
        // As wide as its label needs on the shared face, never narrower
        // than the row's other buttons.
        let delete_w = ui::widget::ButtonFace::new(Rect::ZERO, &delete_label)
            .role(ButtonRole::Destructive)
            .intrinsic_width(text_renderer, theme)
            .max(120.0 * scale);
        let delete = Rect::new(
            card.x1 - 16.0 - delete_w,
            row,
            card.x1 - 16.0,
            row + 32.0 * scale,
        );
        button(
            scene,
            text_renderer,
            CardButton {
                role: ButtonRole::Destructive,
                enabled: deletable,
                ..CardButton::new(delete, &delete_label)
            },
        );
        self.delete_rect = if deletable { delete } else { Rect::ZERO };
    }

    /// The archived workspaces card: each workspace this device keeps off
    /// its switcher, with Restore and Delete. Painted only while there are
    /// any.
    fn draw_ws_settings_archived(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        model: &WorkspaceSettingsModel<'_>,
    ) {
        self.archived_rows = model.archived.len();
        self.archived_rects.clear();
        if model.archived.is_empty() {
            return;
        }
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let scale = f64::from(theme.text_scale * theme.ui_scale);
        let card = self.archived_card_rect();
        ui::surface::card(
            scene,
            transform,
            card,
            f64::from(tokens.radii.control),
            colors.surfaces.raised,
        );
        let style = theme.text(TextRole::Caption);
        text_renderer.draw_style(
            scene,
            &t("workspace-settings-archived-title"),
            colors.text_primary,
            transform.pre_translate((card.x0 + 16.0, card.y0 + 16.0 * scale).into()),
            None,
            &theme.text(TextRole::Body),
        );
        let button = |scene: &mut Scene, text_renderer: &mut TextRenderer, face: CardButton<'_>| {
            paint_card_button(scene, transform, text_renderer, theme, face);
        };
        let mut y = card.y0 + 48.0 * scale;
        for archived in model.archived {
            let delete = Rect::new(
                card.x1 - 16.0 - 96.0 * scale,
                y,
                card.x1 - 16.0,
                y + 32.0 * scale,
            );
            let restore = Rect::new(
                delete.x0 - 8.0 - 96.0 * scale,
                y,
                delete.x0 - 8.0,
                y + 32.0 * scale,
            );
            let name = text_renderer.elide_to_width_style(
                &archived.name,
                f32_of_f64((restore.x0 - 12.0 - card.x0 - 16.0).max(0.0)),
                &style,
            );
            text_renderer.draw_style(
                scene,
                &name,
                colors.text_primary,
                transform.pre_translate((card.x0 + 16.0, y + 10.0 * scale).into()),
                None,
                &style,
            );
            let restore_label = t("workspace-settings-archived-restore");
            button(
                scene,
                text_renderer,
                CardButton::new(restore, &restore_label),
            );
            let delete_label = t("workspace-settings-archived-delete");
            button(
                scene,
                text_renderer,
                CardButton {
                    role: ButtonRole::Destructive,
                    ..CardButton::new(delete, &delete_label)
                },
            );
            self.archived_rects.push([restore, delete]);
            y += 40.0 * scale;
        }
    }

    /// The Public card: a switch that lists the workspace in its host's
    /// directory, what the listing says, and the role people who join get.
    /// Its host answers the listing only to whoever may change the
    /// workspace's settings; everyone else sees why not.
    #[expect(
        clippy::too_many_lines,
        reason = "a paint routine that lays the card out top to bottom from shared running offsets"
    )]
    fn draw_ws_settings_public(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        model: &WorkspaceSettingsModel<'_>,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let scale = f64::from(theme.text_scale * theme.ui_scale);
        let card = self.public_card_rect();
        self.public_rects = [Rect::ZERO; 3];
        ui::surface::card(
            scene,
            transform,
            card,
            f64::from(tokens.radii.control),
            colors.surfaces.raised,
        );
        let caption = theme.text(TextRole::Caption);
        text_renderer.draw_style(
            scene,
            &t("workspace-settings-public-title"),
            colors.text_primary,
            transform.pre_translate((card.x0 + 16.0, card.y0 + 16.0 * scale).into()),
            None,
            &theme.text(TextRole::Body),
        );
        let (known, line) = match model.public {
            PublicCard::Asking => (None, t("workspace-settings-public-asking")),
            PublicCard::Unavailable(why) => (None, why.clone()),
            PublicCard::Known {
                public,
                description,
                default_role,
            } => (
                Some((*public, *default_role)),
                if *public && !description.is_empty() {
                    description.replace('\n', " ")
                } else if *public {
                    t("workspace-settings-public-on")
                } else {
                    t("workspace-settings-public-off")
                },
            ),
        };
        let width = ui::num::f32_of_f64((card.width() - 32.0).max(0.0));
        let line = text_renderer.elide_to_width_style(&line, width, &caption);
        text_renderer.draw_style(
            scene,
            &line,
            colors.text_dim,
            transform.pre_translate((card.x0 + 16.0, card.y0 + 44.0 * scale).into()),
            None,
            &caption,
        );
        let Some((public, role)) = known else {
            return;
        };
        // The host answers the listing only to someone the room lets change
        // its settings, and decides each change again: a known listing has
        // working controls.
        let button = |scene: &mut Scene, text_renderer: &mut TextRenderer, face: CardButton<'_>| {
            paint_card_button(scene, transform, text_renderer, theme, face);
        };
        // The switch, at the title's end: pressed in while public.
        let switch = Rect::new(
            card.x1 - 16.0 - 96.0 * scale,
            card.y0 + 12.0 * scale,
            card.x1 - 16.0,
            card.y0 + 44.0 * scale,
        );
        let switch_label = t(if public {
            "workspace-settings-public-switch-on"
        } else {
            "workspace-settings-public-switch-off"
        });
        button(
            scene,
            text_renderer,
            CardButton {
                toggled: public,
                ..CardButton::new(switch, &switch_label)
            },
        );
        // Who joins as what, and what it says, once it is public.
        let row = card.y0 + 76.0 * scale;
        let role_label = t_args(
            "workspace-settings-public-role",
            &[("role", super::role_label(role).into())],
        );
        let role_rect = Rect::new(
            card.x0 + 16.0,
            row,
            card.x0 + 16.0 + 200.0 * scale,
            row + 32.0 * scale,
        );
        let description_rect = Rect::new(
            card.x1 - 16.0 - 160.0 * scale,
            row,
            card.x1 - 16.0,
            row + 32.0 * scale,
        );
        button(
            scene,
            text_renderer,
            CardButton::new(role_rect, &role_label),
        );
        let describe_label = t("workspace-settings-public-describe");
        button(
            scene,
            text_renderer,
            CardButton::new(description_rect, &describe_label),
        );
        self.public_rects = [switch, role_rect, description_rect];
    }

    /// Live workspace summary. Detailed member management belongs to the
    /// Members tab; General stays an honest overview rather than inventing
    /// a placeholder collaborator.
    #[expect(
        clippy::too_many_lines,
        reason = "a paint routine that lays the summary out top to bottom from shared running offsets"
    )]
    fn draw_ws_settings_collaborators(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        model: &WorkspaceSettingsModel<'_>,
        _collab_y: f64,
    ) {
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let body = Self::body_rect(rect);
        let summary_card =
            Self::summary_card_rect_for(body, f64::from(theme.text_scale * theme.ui_scale), 0.0);
        ui::surface::card(
            scene,
            transform,
            summary_card,
            f64::from(tokens.radii.control),
            colors.surfaces.raised,
        );
        text_renderer.draw_style(
            scene,
            &t("workspace-settings-glance-title"),
            colors.text_primary,
            transform.pre_translate(
                (
                    summary_card.x0 + 16.0,
                    summary_card.y0 + 13.0 * f64::from(theme.text_scale * theme.ui_scale),
                )
                    .into(),
            ),
            None,
            &tokens.typography.style(TextRole::Body),
        );
        let (members, grants, app_rules) =
            model.desktop.active_workspace().map_or((0, 0, 0), |ws| {
                let members = ws.roster().len();
                let sandbox = ws
                    .sandbox
                    .iter()
                    .filter(|cap| cap.effect == collab_types::caps::Effect::Grant);
                let given =
                    ws.policy.grants.iter().filter(|grant| {
                        !matches!(grant.subject, tilo_action_policy::Subject::Role(_))
                    });
                let grants = sandbox.count().saturating_add(given.count());
                (members, grants, ws.default_apps.policy.len())
            });
        let stats = [
            (
                members.to_string(),
                t_args(
                    "workspace-settings-stat-members",
                    &[("count", members.into())],
                ),
            ),
            (
                grants.to_string(),
                t_args(
                    "workspace-settings-stat-grants",
                    &[("count", grants.into())],
                ),
            ),
            (
                app_rules.to_string(),
                t_args(
                    "workspace-settings-stat-app-rules",
                    &[("count", app_rules.into())],
                ),
            ),
        ];
        let stat_gap = 8.0;
        let stat_y0 = summary_card.y0 + 44.0 * f64::from(theme.text_scale * theme.ui_scale);
        let stat_w = (summary_card.width() - 32.0 - stat_gap * 2.0) / 3.0;
        for (i, (value, label)) in stats.iter().enumerate() {
            let x0 = summary_card.x0 + 16.0 + f64_of_usize(i) * (stat_w + stat_gap);
            let stat = Rect::new(
                x0,
                stat_y0,
                x0 + stat_w,
                summary_card.y1 - 14.0 * f64::from(theme.text_scale * theme.ui_scale),
            );
            let stat_rr = RoundedRect::from_rect(stat, f64::from(tokens.radii.control));
            let (stat_fill, stat_edge) = stat_tile_face(theme);
            scene.fill(Fill::NonZero, transform, stat_fill, None, &stat_rr);
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                stat_edge,
                None,
                &stat_rr,
            );
            text_renderer.draw_style(
                scene,
                value,
                colors.text_primary,
                transform.pre_translate(
                    (
                        stat.x0 + 12.0,
                        stat.y0 + 8.0 * f64::from(theme.text_scale * theme.ui_scale),
                    )
                        .into(),
                ),
                None,
                &tokens.typography.style(TextRole::Heading),
            );
            text_renderer.draw_style(
                scene,
                label,
                colors.text_dim,
                transform.pre_translate(
                    (
                        stat.x0 + 12.0,
                        stat.y0 + 33.0 * f64::from(theme.text_scale * theme.ui_scale),
                    )
                        .into(),
                ),
                None,
                &tokens.typography.style(TextRole::Caption),
            );
        }
    }
}

/// Glyph size and gap for a tab pill's leading icon.
/// The Public card: its title and switch, the listing's line, and the role
/// and description controls.
const PUBLIC_CARD_HEIGHT: f64 = 124.0;
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

/// A card's button: the shared [`ui::widget::ButtonFace`] of `role`,
/// pressed in while `toggled`, dimmed and inert while not `enabled`.
#[derive(Clone, Copy)]
struct CardButton<'a> {
    rect: Rect,
    label: &'a str,
    role: ButtonRole,
    toggled: bool,
    enabled: bool,
}

impl<'a> CardButton<'a> {
    fn new(rect: Rect, label: &'a str) -> Self {
        Self {
            rect,
            label,
            role: ButtonRole::Secondary,
            toggled: false,
            enabled: true,
        }
    }
}

/// Paints one [`CardButton`] the way every General-tab card draws its
/// buttons, so the cards cannot drift apart: the shared button face, drawn
/// with the theme as seen from the card's raised ground, so a secondary
/// button steps off the card as it steps off a window.
fn paint_card_button(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    button: CardButton<'_>,
) {
    let on_card = theme.on_ground(theme.tokens.colors.surfaces.raised);
    ui::widget::Button::paint(
        scene,
        transform,
        text_renderer,
        &on_card,
        &ui::widget::ButtonFace::new(button.rect, button.label)
            .role(button.role)
            .toggled(button.toggled)
            .disabled(!button.enabled),
    );
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
    style: &TextStyle,
    pill_w: f64,
) -> TabStripPlan {
    let mut widest = |labels: &[String]| -> f64 {
        labels
            .iter()
            .map(|label| f64::from(text_renderer.measure_style(label, None, style).0))
            .fold(0.0, f64::max)
    };
    let full: Vec<String> = WsTab::all().iter().map(WsTab::label).collect();
    let compact: Vec<String> = WsTab::all().iter().map(WsTab::compact_label).collect();
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
    let equal = nav_width / f64_of_usize(count);
    let total: f64 = needs.iter().sum();
    if needs.iter().all(|need| *need <= equal) || total <= 0.0 {
        return vec![equal; count];
    }
    needs.iter().map(|need| need / total * nav_width).collect()
}

/// Accent-toned text on `ground`: the accent as ink, stepped out until it
/// reads there as text. `intent.accent_hover` is a fill: on the dark theme
/// it read at 2.7:1 on the raised card.
fn accent_text_on(theme: &Theme, ground: vello::peniko::Color) -> vello::peniko::Color {
    use ui::tokens::contrast::{BODY_TEXT_MIN_CONTRAST, HIGH_CONTRAST_TEXT_MIN_CONTRAST};
    let colors = &theme.tokens.colors;
    let floor = if colors.palette.high_contrast {
        HIGH_CONTRAST_TEXT_MIN_CONTRAST
    } else {
        BODY_TEXT_MIN_CONTRAST
    };
    colors.ink_on(ground, colors.intent.accent_ink, floor)
}

/// How strongly the identity card's and the sharing card's icon tiles wash
/// the raised card with primary ink.
const IDENTITY_TILE_WASH: f32 = 0.18;
const SHARE_TILE_WASH: f32 = 0.16;

/// An accent icon on a tile washed `wash` over a raised card: the accent as
/// ink, stepped out until it stands there as a mark (WCAG 1.4.11).
fn accent_icon_on_tile(theme: &Theme, wash: f32) -> vello::peniko::Color {
    let colors = &theme.tokens.colors;
    let tile = ui::tokens::colors::over(colors.surfaces.raised, colors.text_primary, wash);
    colors.ink_on(
        tile,
        colors.intent.accent_ink,
        ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_captions_read_as_text_on_their_ground_in_every_theme() {
        use ui::theme::Preset;
        use ui::tokens::contrast::{BODY_TEXT_MIN_CONTRAST, HIGH_CONTRAST_TEXT_MIN_CONTRAST};
        for preset in Preset::ALL {
            let theme = Theme::preset(preset);
            let floor = if theme.tokens.colors.palette.high_contrast {
                HIGH_CONTRAST_TEXT_MIN_CONTRAST
            } else {
                BODY_TEXT_MIN_CONTRAST
            };
            // The identity card's caption on the raised card; a member
            // sheet's subject on the panel, through the panel's own theme.
            let panel = ui::chrome::shell::on_panel(&theme);
            for (what, theme, ground) in [
                (
                    "identity caption",
                    &theme,
                    theme.tokens.colors.surfaces.raised,
                ),
                (
                    "sheet subject",
                    &panel,
                    panel.tokens.colors.surfaces.content,
                ),
            ] {
                let ratio = ui::a11y::contrast_ratio(accent_text_on(theme, ground), ground);
                assert!(
                    ratio >= floor,
                    "{}: {what} reads at {ratio:.2}:1, needs {floor}:1",
                    preset.id()
                );
            }
            let colors = &theme.tokens.colors;
            for wash in [IDENTITY_TILE_WASH, SHARE_TILE_WASH] {
                let tile =
                    ui::tokens::colors::over(colors.surfaces.raised, colors.text_primary, wash);
                let ratio = ui::a11y::contrast_ratio(accent_icon_on_tile(&theme, wash), tile);
                assert!(
                    ratio >= ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
                    "{}: an icon on a {wash} tile stands at {ratio:.2}:1",
                    preset.id()
                );
            }
        }
    }

    #[test]
    fn rename_selection_paint_tracks_selected_range_and_caret() {
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let mut overlay = WorkspaceSettingsOverlay::default();
        overlay.layout(Rect::new(0.0, 0.0, 600.0, 700.0));
        let theme = Theme::dark().with_text_scale(2.0);
        overlay.general_text_scale = 2.0;
        let mut scene = Scene::new();
        let desktop = DesktopTree::default();
        let local_user = "owner".to_string();
        let model = WorkspaceSettingsModel {
            desktop: &desktop,
            tab: WsTab::General,
            grant_draft: GrantDraft::default(),
            acting_user: &local_user,
            people: &crate::collaborators::PeopleNames::default(),
            editing_name: Some("Home"),
            name_selection: ui::text_selection::Selection {
                anchor: 4,
                cursor: 0,
            },
            owner_controls: true,
            apps: &super::super::NoAppDirectory,
            devices: &[],
            archived: &[],
            public: &PublicCard::Asking,
        };
        overlay.paint_name_field(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
            &model,
            "Home",
        );
        let input = overlay.name_input.as_ref().expect("shared field painted");
        let selected = input.selection_rect(&theme, 0, 4);
        let caret = input.caret_rect(&theme, 0);
        assert!(
            selected.width() > 20.0,
            "select-all needs a painted highlight"
        );
        assert_eq!(
            caret.x0,
            overlay.name_rect().x0 + f64::from(theme.padding_standard)
        );
        assert!(caret.height() >= 32.0);
    }

    #[test]
    fn workspace_name_field_keeps_long_carets_visible_and_resets_on_cancel() {
        use ui::widget::Widget;
        let desktop = DesktopTree::default();
        let local_user = "owner".to_string();
        let name = "W".repeat(64);
        let mut model = WorkspaceSettingsModel {
            desktop: &desktop,
            tab: WsTab::General,
            grant_draft: GrantDraft::default(),
            acting_user: &local_user,
            people: &crate::collaborators::PeopleNames::default(),
            editing_name: Some(&name),
            name_selection: ui::text_selection::Selection::collapsed(name.len()),
            owner_controls: true,
            apps: &super::super::NoAppDirectory,
            devices: &[],
            archived: &[],
            public: &PublicCard::Asking,
        };
        let mut overlay = WorkspaceSettingsOverlay::default();
        let mut renderer = TextRenderer::new();
        for (width, scale) in [(1280.0, 1.0), (600.0, 2.0), (480.0, 2.0)] {
            let theme = Theme::dark().with_text_scale(scale);
            overlay.layout(Rect::new(0.0, 0.0, width, 800.0));
            overlay.general_text_scale = f64::from(scale);
            let mut scene = Scene::new();
            overlay.paint_name_field(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut renderer, &theme),
                &model,
                &name,
            );
            let input = overlay.name_input.as_ref().expect("shared field painted");
            let caret = input.caret_rect(&theme, name.len());
            assert!(input.scroll_x() > 0.0, "long-name editing must scroll");
            // The caret spans the text's line, which at a large text scale
            // can stand a pixel past the field's edge: its middle is in
            // the field.
            assert!(
                overlay.name_rect().contains(caret.center()),
                "the caret {caret:?} is in the field {:?}",
                overlay.name_rect()
            );
            assert!(
                caret.x1 <= overlay.name_rect().x1,
                "full caret must remain reachable"
            );
            model.name_selection = ui::text_selection::Selection::collapsed(0);
            overlay.paint_name_field(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut renderer, &theme),
                &model,
                &name,
            );
            assert_eq!(overlay.name_input.as_ref().unwrap().scroll_x(), 0.0);
            model.name_selection = ui::text_selection::Selection::collapsed(name.len());
        }
        model.editing_name = None;
        overlay.paint_name_field(
            &mut PaintCtx::new(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut renderer,
                &Theme::dark(),
            ),
            &model,
            "Home",
        );
        assert!(
            !overlay.name_input.as_ref().unwrap().is_focused(),
            "cancel removes editing focus"
        );
        assert_eq!(
            model.name_selection.cursor,
            name.len(),
            "painting cannot mutate the owner selection"
        );
    }

    #[test]
    fn workspace_name_production_render_matrix() {
        let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../target/workspace-name-review");
        std::fs::create_dir_all(&out).expect("capture directory");
        let desktop = DesktopTree::default();
        let user = "owner".to_string();
        let name = format!("Project start {} final name", "W".repeat(38));
        #[allow(
            clippy::useless_vec,
            reason = "the themes are too large to sit on the stack as an array"
        )]
        for (label, width, height, theme, editing) in vec![
            ("dark-edit", 1280, 800, Theme::dark(), true),
            ("light-edit", 1280, 800, Theme::light(), true),
            (
                "high-contrast-edit",
                1280,
                800,
                Theme::high_contrast(),
                true,
            ),
            (
                "large-text-edit",
                600,
                800,
                Theme::dark().with_text_scale(2.0),
                true,
            ),
            (
                "ui-text-edit",
                1280,
                800,
                Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
                true,
            ),
            ("narrow-rest", 480, 700, Theme::dark(), false),
        ] {
            let mut overlay = WorkspaceSettingsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
            overlay.general_text_scale = f64::from(theme.text_scale * theme.ui_scale);
            let model = WorkspaceSettingsModel {
                desktop: &desktop,
                tab: WsTab::General,
                grant_draft: GrantDraft::default(),
                acting_user: &user,
                people: &crate::collaborators::PeopleNames::default(),
                editing_name: editing.then_some(name.as_str()),
                name_selection: ui::text_selection::Selection::collapsed(name.len()),
                owner_controls: true,
                apps: &super::super::NoAppDirectory,
                devices: &[],
                archived: &[],
                public: &PublicCard::Asking,
            };
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            let clip = overlay.general_body_rect();
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                Affine::IDENTITY,
                &clip,
            );
            let rect = overlay.rect();
            overlay.draw_ws_settings_name_input(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
                rect,
                &model,
                &name,
            );
            scene.pop_layer();
            let mut renderer = ui::offscreen::OffscreenRenderer::new(width, height);
            renderer.render_png(
                &scene,
                vello::peniko::Color::BLACK,
                out.join(format!("{label}.png")),
            );
            assert!(overlay.name_rect().width() > 0.0);
            if editing {
                let input = overlay.name_input.as_ref().expect("editing field painted");
                let caret = input.caret_rect(&theme, name.len());
                // The caret spans the text's line, which at a large text scale
                // can stand a pixel past the field's edge: its middle is in
                // the field.
                assert!(
                    overlay.name_rect().contains(caret.center()),
                    "the caret {caret:?} is in the field {:?}",
                    overlay.name_rect()
                );
                assert!(caret.x1 <= overlay.name_rect().x1);
            }
            assert!(
                std::fs::metadata(out.join(format!("{label}.png")))
                    .unwrap()
                    .len()
                    > 1024
            );
        }
    }

    #[test]
    fn overlay_tiles_cards_and_fields_keep_an_edge_on_every_palette() {
        // Phase `stat_tiles_keep_an_edge_on_every_palette`.
        // HIG §9.3's caveat: where a palette makes `base` and `raised` the same
        // colour, a tile filled with `base` inside a `raised` card has no lift
        // left to separate it. The derived hairline must therefore move
        // against the card on every shipped palette — measured as the
        // composite of the edge over the card, not asserted from its alpha.
        {
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

        // Phase `lens_cards_and_fields_keep_an_edge_on_every_palette`.
        // Every card class the lens paints — raised cards (identity, sharing,
        // summary), base-filled fields (name, invite link) and the base-filled
        // close chip — gets its edge from `ui::surface::edge_on(fill)`; that
        // edge must move against its fill on every shipped palette.
        {
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
    }

    #[test]
    fn lens_layout_keeps_its_rhythm_bezel_and_footer_on_every_output() {
        // Phase `lens_header_keeps_one_rhythm_and_centres_the_close_chip`.
        // One rhythm across the header (HIG §7b): `s4` above the identity
        // card, `s4` between card and tab strip, `s4` between strip and the
        // first content card; the close chip is centred on the card's line.
        {
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

        // Phase `general_name_field_sits_on_the_card_pad_and_clears_the_summary_card`.
        // General at 1×: the name field sits on the card pad (`s4`) inside the
        // body and the "At a glance" card starts a token step or more under it,
        // sharing the body's left edge. ux-bench cannot measure this pair at
        // 200 % text, where the two are never co-visible on an 800 px body, so
        // the production geometry helpers carry the assertion.
        {
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

        {
            let viewport = Rect::new(0.0, 0.0, 1280.0, 720.0);
            let lens = WorkspaceSettingsOverlay::rect_for(viewport);
            assert_eq!(lens.x0, viewport.x0 + 24.0);
            assert!(lens.width() <= f64::from(ui::tokens::Metrics::BASE.panel_width_sheet));
            assert!(
                lens.x1 <= viewport.width() / 2.0,
                "the workspace keeps a usable majority"
            );
        }

        {
            for viewport in [
                Rect::new(0.0, 0.0, 1600.0, 900.0),
                Rect::new(0.0, 0.0, 960.0, 540.0),
                Rect::new(0.0, 0.0, 600.0, 700.0),
            ] {
                let lens = WorkspaceSettingsOverlay::rect_for(viewport);
                if viewport.width() >= 840.0 {
                    assert_eq!(lens.x0 - viewport.x0, 24.0);
                } else {
                    assert_eq!(viewport.x1 - lens.x1, 24.0);
                }
                assert_eq!(viewport.y1 - lens.y1, 24.0);
                assert!(lens.y0 >= viewport.y0 + 24.0);
            }
        }

        {
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
    }

    #[test]
    fn closing_is_one_pointer_target_and_stays_visible_until_exit_settles() {
        {
            let mut overlay = WorkspaceSettingsOverlay::default();
            overlay.force_settled();
            // The lens leaves on the standard dismiss (`ui::motion::Appear`).
            let now = ui::time::now();
            overlay.transition.show(
                false,
                ui::motion::Appear::SPEC,
                Theme::dark().motion(),
                ui::tokens::MotionSource::Pointer,
                now,
            );
            assert!(overlay.transition.is_visible(now));
            let gone = overlay.transition.settles_at().expect("closing");
            assert!(!overlay.transition.is_visible(gone));
        }

        {
            let close = WorkspaceSettingsOverlay::close_rect_for(Rect::new(0.0, 0.0, 540.0, 700.0));
            assert!(close.width() >= 32.0 && close.height() >= 32.0);
        }
    }

    #[test]
    fn tab_strip_compacts_labels_before_eliding_and_measures_only_when_needed() {
        {
            let mut text = TextRenderer::new();
            // The 540 px laptop lens at 100 %: five ~95 px pills.
            let base = Theme::dark();
            let plan = tab_strip_plan(&mut text, &base.text(TextRole::Body), 95.6);
            assert!(plan.icons, "at 100% every pill has room for its icon");
            // The same lens at 200 % text: no pill can hold a full label, so the
            // whole strip drops to compact labels (and icons only if they all
            // fit) instead of eliding the active tab alone.
            let large = Theme::dark().with_text_scale(2.0);
            let label_style = large.text(TextRole::Body);
            // In the 540 px lens even compact labels overflow a 95.6 px pill at
            // 28 px type, so the plan drops to compact labels without icons and
            // every pill elides by the same rule — never the active tab alone.
            let plan = tab_strip_plan(&mut text, &label_style, 95.6);
            assert_eq!(
                plan,
                TabStripPlan {
                    compact: true,
                    icons: false
                }
            );
            // In the 800 px lens (145.6 px pills) compact labels fit with icons.
            let plan = tab_strip_plan(&mut text, &label_style, 145.6);
            assert_eq!(
                plan,
                TabStripPlan {
                    compact: true,
                    icons: true
                }
            );
            let widest = WsTab::all()
                .iter()
                .map(|tab| {
                    f64::from(
                        text.measure_style(&tab.compact_label(), None, &label_style)
                            .0,
                    )
                })
                .fold(0.0, f64::max);
            assert!(
                widest + TAB_LABEL_INSET + TAB_ICON_PX + TAB_ICON_GAP <= 145.6,
                "the chosen plan fits its widest label ({widest}) into the pill"
            );
        }

        {
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
    }

    #[test]
    fn overlay_hits_are_typed_policy_controls_carrying_their_identity() {
        {
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

        {
            let mut overlay = WorkspaceSettingsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            let add = Rect::new(900.0, 260.0, 1010.0, 288.0);
            let remove = Rect::new(900.0, 300.0, 980.0, 334.0);
            overlay.bot_add_rect = add;
            overlay
                .bot_remove_rects
                .push(("so.tilo.bots.standup".into(), remove));
            assert_eq!(
                overlay.hit_test(add.center(), WsTab::Agents),
                EventOutcome::AddBot
            );
            assert_eq!(
                overlay.hit_test(remove.center(), WsTab::Agents),
                EventOutcome::RemoveBot("so.tilo.bots.standup".into()),
            );
        }

        {
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

        {
            let mut overlay = WorkspaceSettingsOverlay::default();
            let action = Rect::new(900.0, 220.0, 1_020.0, 258.0);
            overlay.ownership_action_rect = action;
            assert_eq!(
                overlay.hit_test(action.center(), WsTab::Members),
                EventOutcome::ManageOwnership
            );
        }

        {
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

        {
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
    }

    #[test]
    fn role_and_destructive_sheets_need_explicit_choices() {
        {
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

        {
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
}

#[cfg(test)]
mod general_scroll_tests;

#[cfg(test)]
mod agent_access_key_tests;
