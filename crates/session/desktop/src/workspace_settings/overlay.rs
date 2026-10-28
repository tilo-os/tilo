//! Presentation state for the workspace-settings lens.
//!
//! Policy mutations deliberately remain with the host: this component
//! reports typed hits, while the host applies them through the existing
//! authenticated `Workspace` mutation methods.

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

use collab_types::role::Role as CapRole;
use tilo_desktop_model::{DesktopTree, UserId};

use super::{
    AppDirectory, DefaultAppsCtx, GrantDraft, WsTab, agents_add_rect, agents_pane_rows,
    default_app_rows, draw_agents_pane, draw_default_apps_pane, draw_permissions_pane,
    permission_profiles_rect, permissions_pane_layout,
};
use ui::TextRole;

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
    /// Whether this device governs the active workspace's room.
    pub governs_active_room: bool,
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
    /// The workspace's emoji, for the Emoji tab.
    pub emoji: super::emoji_pane::EmojiPaneModel<'a>,
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
    /// There is no network to reach its host through.
    Offline,
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
    /// Choose a picture and add it as the workspace's emoji.
    AddEmoji,
    /// Remove the workspace's emoji of this name, the way given.
    RemoveEmoji(String, super::emoji_pane::Removal),
    /// Put away the note about the last emoji not added or removed.
    DismissEmojiNotice,
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
    /// The General page's first group stands `s2` under the body's top: the
    /// other half of the strip's `s4` rhythm, in the theme's spacing.
    general_lead: f64,
    /// Measured footer plus its themed inset/gap, reserved outside the body clip.
    general_footer_height: f64,
    transition: ui::motion::Presence,
    tab_pill_rects: [Rect; WsTab::COUNT],
    /// The strip that switches between the lens's tabs, and the labels it
    /// was built with (the catalogue's, which follow the language).
    tabs: Option<(Vec<String>, ui::widget::ViewTabs)>,
    /// The Emoji tab as painted, and how far its grid is scrolled.
    emoji: super::emoji_pane::EmojiPaneLayout,
    emoji_scroll: f64,
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
    /// The General tab's body, laid out by the toolkit.
    general: general::GeneralPage,
    /// The People tab's body, laid out by the toolkit.
    people: people::PeoplePage,
    /// Shaped caret/scroll cache; the host owns the rename draft and selection.
    name_input: Option<ui::widget::TextInput>,
}

impl Default for WorkspaceSettingsOverlay {
    fn default() -> Self {
        Self {
            viewport: Rect::ZERO,
            general_scroll_y: 0.0,
            general_lead: f64::from(ui::tokens::Spacing::BASE.s2),
            general_footer_height: 48.0,
            transition: ui::motion::Presence::closed(),
            tab_pill_rects: [Rect::ZERO; WsTab::COUNT],
            tabs: None,
            emoji: super::emoji_pane::EmojiPaneLayout::default(),
            emoji_scroll: 0.0,
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
            general: general::GeneralPage::default(),
            people: people::PeoplePage::default(),
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

    /// The "At a glance" statistics, as last laid out (scroll applied).
    pub fn general_summary_rect(&self) -> Rect {
        self.general.rect(general::GLANCE)
    }

    /// Scroll only General's body; headers, tabs and footer remain anchored.
    pub fn scroll_general(&mut self, delta: f64) -> bool {
        if !delta.is_finite() || self.member_sheet.is_some() {
            return false;
        }
        let content = self.general_lead + self.general.height();
        let max = (content - self.general_body_rect().height()).max(0.0);
        let old = self.general_scroll_y;
        self.general_scroll_y = (old + delta).clamp(0.0, max);
        let body = self.general_body_rect();
        self.general.place(Point::new(
            body.x0,
            body.y0 + self.general_lead - self.general_scroll_y,
        ));
        (old - self.general_scroll_y).abs() > f64::EPSILON
    }

    /// The General control under `point`, by the id it carries in the page.
    fn general_hit(&self, point: Point) -> Option<EventOutcome> {
        let hit = |id: &str| {
            let rect = self.general.rect(id);
            rect.area() > 0.0 && rect.contains(point)
        };
        let rows = |id: fn(usize) -> String| {
            (0..)
                .map(id)
                .take_while(|id| self.general.rect(id).area() > 0.0)
        };
        if let Some(index) = rows(general::device_id).position(|id| hit(&id)) {
            return Some(EventOutcome::ToggleDevice(index));
        }
        if let Some(index) = rows(general::restore_id).position(|id| hit(&id)) {
            return Some(EventOutcome::RestoreArchived(index));
        }
        if let Some(index) = rows(general::delete_archived_id).position(|id| hit(&id)) {
            return Some(EventOutcome::DeleteArchived(index));
        }
        [
            (general::MOVE_UP, EventOutcome::MoveWorkspace(-1)),
            (general::MOVE_DOWN, EventOutcome::MoveWorkspace(1)),
            (general::PUBLIC_SWITCH, EventOutcome::TogglePublic),
            (general::PUBLIC_ROLE, EventOutcome::CycleDefaultRole),
            (general::PUBLIC_DESCRIBE, EventOutcome::EditDescription),
            (general::DELETE, EventOutcome::DeleteWorkspace),
            (general::INVITE, EventOutcome::CreateInvite),
            (general::NAME, EventOutcome::EditName),
        ]
        .into_iter()
        .find_map(|(id, outcome)| hit(id).then_some(outcome))
    }

    /// The workspace's name field, as last laid out (scroll applied).
    pub fn name_rect(&self) -> Rect {
        self.general.rect(general::NAME)
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

    /// The button that opens the invite composer, as last laid out.
    pub fn invite_rect(&self) -> Rect {
        self.general.rect(general::INVITE)
    }

    pub fn force_settled(&mut self) {
        self.transition = ui::motion::Presence::opened();
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.transition.is_moving(now)
    }

    pub fn tab_pill_rect(&self, tab: WsTab) -> Rect {
        self.tab_pill_rects
            .get(tab.index())
            .copied()
            .unwrap_or(Rect::ZERO)
    }

    /// The Emoji tab as last painted: its controls and cells.
    pub fn emoji_layout(&self) -> &super::emoji_pane::EmojiPaneLayout {
        &self.emoji
    }

    /// Scroll the Emoji tab's grid by `delta`; whether it moved. Nothing
    /// moves while the tab is not shown.
    pub fn scroll_emoji(&mut self, delta: f64) -> bool {
        if !delta.is_finite() || self.emoji.grid.area() <= 0.0 {
            return false;
        }
        let old = self.emoji_scroll;
        self.emoji_scroll = (old + delta).clamp(0.0, self.emoji.max_scroll);
        (old - self.emoji_scroll).abs() > f64::EPSILON
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
        self.tab_pill_rects = [Rect::ZERO; WsTab::COUNT];
        self.emoji = super::emoji_pane::EmojiPaneLayout::default();
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

    /// Forget every pane's controls, keeping the tab strip just painted.
    fn clear_hits_but_tabs(&mut self) {
        let tabs = self.tab_pill_rects;
        self.clear_hits();
        self.tab_pill_rects = tabs;
    }

    /// What a press at `point` on the Agents or Emoji tab means.
    fn pane_outcome(&self, point: Point, active_tab: WsTab) -> Option<EventOutcome> {
        match active_tab {
            WsTab::Emoji => self.emoji.hit(point),
            _ => self.agents_outcome(point, active_tab),
        }
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
        if let Some(outcome) = self.pane_outcome(point, active_tab) {
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
        if matches!(active_tab, WsTab::General) {
            return self.general_hit(point).unwrap_or(EventOutcome::CommitName);
        }
        EventOutcome::CommitName
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

        self.general_lead = f64::from(tokens.spacing.s2);
        let editing = model.owner_controls && model.editing_name.is_some();
        let hint = Self::footer_hints(editing);
        let footer = Self::footer_hint_rect(rect, &hint, text_renderer, theme);
        self.general_footer_height = rect.y1 - footer.y0 + f64::from(tokens.spacing.s2);
        let body = self.general_body_rect();
        let content = general::content(model);
        self.general
            .lay_out(&content, body.width(), theme, text_renderer);
        self.scroll_general(0.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);
        self.general.draw(scene, transform, text_renderer, theme);
        let shown = model
            .editing_name
            .filter(|_| model.owner_controls)
            .unwrap_or(active_name.as_str());
        self.paint_name_field(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            model,
            shown,
        );
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

    /// The tab strip: [`ui::widget::ViewTabs`] over the lens's five tabs,
    /// each a real destination (a host who has granted nothing yet still
    /// needs to reach the tab that grants). A narrow lens shows the short
    /// labels; the strip's tab rects are the click targets.
    fn draw_ws_settings_tab_strip(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        active_tab: WsTab,
    ) {
        let tabs = WsTab::all();
        let labels: Vec<String> = tabs.iter().map(WsTab::label).collect();
        if self.tabs.as_ref().is_none_or(|(built, _)| *built != labels) {
            let segments = tabs
                .iter()
                .zip(&labels)
                .map(|(tab, label)| {
                    ui::widget::Segment::new(label.clone()).short(tab.compact_label())
                })
                .collect();
            let strip = ui::widget::ViewTabs::new(segments, 0)
                .with_id("ws_settings_tabs")
                .with_accessible_name(t("workspace-settings-title"));
            self.tabs = Some((labels, strip));
        }
        let Some((_, strip)) = self.tabs.as_mut() else {
            return;
        };
        let selected = tabs.iter().position(|tab| *tab == active_tab).unwrap_or(0);
        if strip.selected() != selected {
            strip.set_selected(selected);
        }
        let nav = Self::nav_rect_for(rect);
        strip.apply_layout_external(nav);
        ui::widget::Widget::draw(
            strip,
            scene,
            transform,
            text_renderer,
            theme,
            &ui::presence::PresenceCtx::default(),
        );
        // The first draw measures the labels in this theme; place again so
        // the targets are where the tabs now stand.
        strip.apply_layout_external(nav);
        self.tab_pill_rects =
            std::array::from_fn(|index| strip.tab_rect(index).unwrap_or(Rect::ZERO));
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
        if active_tab != WsTab::Emoji {
            self.emoji = super::emoji_pane::EmojiPaneLayout::default();
        }
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
            WsTab::Emoji => {
                self.clear_hits_but_tabs();
                let body = Self::body_rect(rect);
                if let Some(ws) = model.desktop.active_workspace() {
                    self.emoji = super::emoji_pane::draw_emoji_pane(
                        &mut PaintCtx::new(scene, transform, text_renderer, theme),
                        body,
                        &model.emoji,
                        &super::emoji_pane::EmojiPaneContext {
                            ws,
                            people: model.people,
                        },
                        self.emoji_scroll,
                    );
                    self.emoji_scroll = self.emoji_scroll.min(self.emoji.max_scroll);
                }
                Self::draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
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
        let Some(ws) = model.desktop.active_workspace() else {
            Self::draw_ws_settings_close_hint(scene, transform, rect, text_renderer, theme);
            return;
        };
        let lead = f64::from(theme.tokens.spacing.s2);
        let hint = Self::footer_hints(false);
        let footer = Self::footer_hint_rect(rect, &hint, text_renderer, theme);
        let mut body = Self::body_rect(rect);
        body.y1 = (footer.y0 - lead).max(body.y0);
        let content = people::content(ws, model.people, model.acting_user, model.owner_controls);
        self.people
            .lay_out(&content, body.width(), theme, text_renderer);
        self.people.place(Point::new(body.x0, body.y0 + lead));
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &body);
        self.people.draw(scene, transform, text_renderer, theme);
        scene.pop_layer();
        // What the body clips away takes no press.
        let shown = |rect: Rect| {
            if rect.y0 >= body.y0 && rect.y1 <= body.y1 {
                rect
            } else {
                Rect::ZERO
            }
        };
        self.ownership_action_rect = shown(self.people.rect(people::OWNERSHIP));
        for (index, person) in content.people.iter().enumerate() {
            self.member_control_rects.push((
                person.user.clone(),
                shown(self.people.rect(&people::role_id(index))),
                shown(self.people.rect(&people::remove_id(index))),
            ));
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

/// How strongly the identity card's icon tile washes the raised card with
/// primary ink.
const IDENTITY_TILE_WASH: f32 = 0.18;

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
            {
                let wash = IDENTITY_TILE_WASH;
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
            governs_active_room: false,
            owner_controls: true,
            apps: &super::super::NoAppDirectory,
            devices: &[],
            archived: &[],
            public: &PublicCard::Asking,
            emoji: super::super::emoji_pane::EmojiPaneModel::unavailable(&local_user),
        };
        overlay.draw_contents(&mut scene, Affine::IDENTITY, &mut tr, &theme, &model);
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
            governs_active_room: false,
            owner_controls: true,
            apps: &super::super::NoAppDirectory,
            devices: &[],
            archived: &[],
            public: &PublicCard::Asking,
            emoji: super::super::emoji_pane::EmojiPaneModel::unavailable(&local_user),
        };
        let mut overlay = WorkspaceSettingsOverlay::default();
        let mut renderer = TextRenderer::new();
        for (width, scale) in [(1280.0, 1.0), (600.0, 2.0), (480.0, 2.0)] {
            let theme = Theme::dark().with_text_scale(scale);
            overlay.layout(Rect::new(0.0, 0.0, width, 800.0));
            let mut scene = Scene::new();
            overlay.draw_contents(&mut scene, Affine::IDENTITY, &mut renderer, &theme, &model);
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
            let model = WorkspaceSettingsModel {
                desktop: &desktop,
                tab: WsTab::General,
                grant_draft: GrantDraft::default(),
                acting_user: &user,
                people: &crate::collaborators::PeopleNames::default(),
                editing_name: editing.then_some(name.as_str()),
                name_selection: ui::text_selection::Selection::collapsed(name.len()),
                governs_active_room: false,
                owner_controls: true,
                apps: &super::super::NoAppDirectory,
                devices: &[],
                archived: &[],
                public: &PublicCard::Asking,
                emoji: super::super::emoji_pane::EmojiPaneModel::unavailable(&user),
            };
            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            overlay.draw_contents(&mut scene, Affine::IDENTITY, &mut text, &theme, &model);
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

    /// General at 1×: the name field keeps the same margin on both sides
    /// of the body and the "At a glance" statistics start a token step or
    /// more under it, inside the body's edges.
    #[test]
    fn general_name_field_stands_inside_the_body_and_clears_the_summary() {
        let s1 = f64::from(ui::tokens::Spacing::BASE.s1);
        let desktop = DesktopTree::default();
        let user = "owner".to_string();
        let model = WorkspaceSettingsModel {
            desktop: &desktop,
            tab: WsTab::General,
            grant_draft: GrantDraft::default(),
            acting_user: &user,
            people: &crate::collaborators::PeopleNames::default(),
            editing_name: None,
            name_selection: ui::text_selection::Selection::collapsed(0),
            governs_active_room: false,
            owner_controls: true,
            apps: &super::super::NoAppDirectory,
            devices: &[],
            archived: &[],
            public: &PublicCard::Asking,
            emoji: super::super::emoji_pane::EmojiPaneModel::unavailable(&user),
        };
        for viewport in [
            Rect::new(0.0, 0.0, 1600.0, 900.0),
            Rect::new(0.0, 0.0, 600.0, 900.0),
        ] {
            let mut overlay = WorkspaceSettingsOverlay::default();
            overlay.layout(viewport);
            overlay.draw_contents(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &model,
            );
            let body = overlay.general_body_rect();
            let name = overlay.name_rect();
            let summary = overlay.general_summary_rect();
            assert!(name.width() > 0.0 && summary.width() > 0.0);
            assert!(
                ((name.x0 - body.x0) - (body.x1 - name.x1)).abs() < 0.5,
                "the name field keeps one margin on both sides: {name:?} in {body:?}"
            );
            assert!(
                summary.y0 - name.y1 >= s1,
                "summary clears the name field (gap {})",
                summary.y0 - name.y1
            );
            assert!(summary.x0 >= body.x0 && summary.x1 <= body.x1);
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
    fn overlay_hits_are_typed_policy_controls_carrying_their_identity() {
        {
            let mut overlay = WorkspaceSettingsOverlay::default();
            overlay.layout(Rect::new(0.0, 0.0, 1280.0, 800.0));
            let hit_rect = Rect::new(700.0, 200.0, 760.0, 240.0);
            overlay.tab_pill_rects = [Rect::ZERO; WsTab::COUNT];
            overlay.tab_pill_rects[0] = hit_rect;
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

mod general;
mod page;
mod people;

#[cfg(test)]
mod general_scroll_tests;

#[cfg(test)]
mod agent_access_key_tests;
