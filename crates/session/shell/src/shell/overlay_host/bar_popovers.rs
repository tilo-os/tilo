//! The popovers the top bar opens: the workspace menu behind the
//! workspace's name and the profile card behind the person's own avatar.
//!
//! Both are the shared desktop widgets the browser desktop shows too; this
//! is what the native shell puts in them and how it carries out a choice.
//! The bar asks through [`BarPopoverRequest`]; the host opens one popover
//! at a time, closing People and the other popover as it does.

use taffy::NodeId;
use tilo_desktop::profile_card::{
    ProfileAction, ProfileCard, ProfileChoice, ProfileGroup, ProfileModel,
};
use tilo_desktop::workspace_menu::{
    WorkspaceAction, WorkspaceChoice, WorkspaceMenu, WorkspaceMenuChoice, WorkspaceMenuModel,
};
use ui::widget::Widget as _;
use vello::kurbo::Rect;

use super::ShellOverlayHost;
use crate::shell::chrome_state::{BarPopoverRequest, WsTab};
use crate::shell::i18n::t;
use crate::shell::state::WorkspaceId;

/// The workspace menu and the profile card, with the layout nodes they
/// hold while open.
#[derive(Default)]
pub(super) struct BarPopovers {
    pub(super) workspace_menu: WorkspaceMenu,
    pub(super) workspace_menu_node: Option<NodeId>,
    pub(super) profile: ProfileCard,
    pub(super) profile_node: Option<NodeId>,
}

impl BarPopovers {
    pub(super) fn any_open(&self) -> bool {
        self.workspace_menu.is_open() || self.profile.is_open()
    }

    /// Whether the layout nodes still match what is open.
    pub(super) fn structure_stale(&self) -> bool {
        self.workspace_menu.is_open() != self.workspace_menu_node.is_some()
            || self.profile.is_open() != self.profile_node.is_some()
    }
}

/// A command about workspaces the native shell lists in the workspace
/// menu, beside switching, renaming, the overview and a new desk. A command
/// added here is offered there and carried out in
/// [`ShellOverlayHost::run_native_workspace_action`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeWorkspaceAction {
    /// A new, empty workspace, shown at once.
    New,
    /// The workspace's members, permissions, apps and devices.
    Settings,
    /// "Join a workspace…": the directory of public workspaces.
    Join,
    /// Report this public workspace to the people who run it.
    Report,
    /// Leave this workspace, held through the web: it goes from this
    /// computer and is not joined again unasked.
    Leave,
}

impl NativeWorkspaceAction {
    const ALL: [Self; 5] = [
        Self::New,
        Self::Join,
        Self::Settings,
        Self::Report,
        Self::Leave,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::New => "new-workspace",
            Self::Settings => "workspace-settings",
            Self::Join => "join-workspace",
            Self::Report => "report-workspace",
            Self::Leave => "leave-workspace",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    fn menu_action(self) -> WorkspaceAction {
        let (title, destructive) = match self {
            Self::New => ("workspace-menu-new", false),
            Self::Settings => ("workspace-menu-settings", false),
            Self::Join => ("workspace-menu-join", false),
            Self::Report => ("workspace-menu-report", false),
            Self::Leave => ("workspace-menu-leave", true),
        };
        WorkspaceAction {
            id: self.id().to_owned(),
            title: t(title),
            destructive,
        }
    }

    /// Whether the menu offers it for the workspace shown, given the site
    /// it is held through (`None`: on this computer, or shared directly).
    fn offered(self, site: Option<&crate::shell::network::SiteRoom>) -> bool {
        match self {
            Self::New | Self::Settings | Self::Join => true,
            Self::Leave => site.is_some(),
            // Only a workspace joined from a directory has a door to
            // report it at.
            Self::Report => site.is_some_and(|site| site.public),
        }
    }
}

/// A command about the person the profile card offers: each opens the
/// Settings page where it is done, the way search reaches those pages,
/// except pairing, which opens its own sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeProfileAction {
    /// Say something beside the pointer to the people in the window under
    /// it (cursor chat), whatever has the keyboard.
    SaySomething,
    /// Their identities on this device and how each is protected.
    Identities,
    /// An encrypted backup of their identity, to recover it elsewhere.
    Backup,
    /// Pairing another device of theirs.
    PairDevice,
}

impl NativeProfileAction {
    const ALL: [Self; 4] = [
        Self::SaySomething,
        Self::Identities,
        Self::Backup,
        Self::PairDevice,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::SaySomething => "say-something",
            Self::Identities => "identities",
            Self::Backup => "identity-backup",
            Self::PairDevice => "pair-device",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    /// The Settings page it opens; none for pairing, which opens its sheet.
    fn settings_link(self) -> Option<&'static str> {
        match self {
            Self::Identities => Some("settings://users"),
            Self::Backup => Some("settings://about"),
            Self::PairDevice | Self::SaySomething => None,
        }
    }

    fn profile_action(self) -> ProfileAction {
        let (title, group, icon) = match self {
            Self::SaySomething => (
                "profile-action-say-something",
                ProfileGroup::Together,
                ui::icons::Icon::MessageCircle,
            ),
            Self::Identities => (
                "profile-action-identities",
                ProfileGroup::Identity,
                ui::icons::Icon::Fingerprint,
            ),
            Self::Backup => (
                "profile-action-backup",
                ProfileGroup::Identity,
                ui::icons::Icon::Download,
            ),
            Self::PairDevice => (
                "profile-action-pair-device",
                ProfileGroup::Devices,
                ui::icons::Icon::MonitorSmartphone,
            ),
        };
        ProfileAction {
            id: self.id().to_owned(),
            title: t(title),
            group,
            icon,
        }
    }
}

impl ShellOverlayHost {
    /// Open or close the popover a bar asked for since the last call. Run
    /// inside the dispatch that pressed the bar, so the popover is up when
    /// that event returns; `update` runs it again for semantic presses.
    pub(super) fn harvest_bar_popover_request(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let Some(request) = chrome.borrow_mut().bar_popover_request.take() else {
            return;
        };
        match request {
            BarPopoverRequest::WorkspaceMenu { anchor } => {
                self.close_bar_popovers_for(Some(request));
                let model = self.workspace_menu_model();
                let anchor = if anchor.is_zero_area() {
                    self.main_bar_rect(tilo_desktop::topbar::TopBarChrome::workspace_menu_rect)
                } else {
                    anchor
                };
                self.bar_popovers
                    .workspace_menu
                    .toggle(&model, anchor, self.rect);
            }
            BarPopoverRequest::Profile => {
                self.close_bar_popovers_for(Some(request));
                let model = self.profile_model();
                let anchor =
                    self.main_bar_rect(tilo_desktop::topbar::TopBarChrome::own_avatar_rect);
                self.bar_popovers.profile.toggle(&model, anchor, self.rect);
            }
        }
        self.bump_chrome();
    }

    /// One popover from the bar at a time: close People and whichever of
    /// the two `opening` is not (both for `None`).
    pub(super) fn close_bar_popovers_for(&mut self, opening: Option<BarPopoverRequest>) {
        if !matches!(opening, Some(BarPopoverRequest::WorkspaceMenu { .. })) {
            self.bar_popovers.workspace_menu.close();
        }
        if opening != Some(BarPopoverRequest::Profile) {
            self.bar_popovers.profile.close();
        }
        if opening.is_some()
            && let Some(chrome) = self.chrome.as_ref()
        {
            chrome.borrow_mut().overlays.collaborators = false;
        }
    }

    fn main_bar_rect(&mut self, rect: fn(&tilo_desktop::topbar::TopBarChrome) -> Rect) -> Rect {
        self.desktop_surface_mut()
            .and_then(|surface| surface.topbar().map(rect))
            .unwrap_or(Rect::ZERO)
    }

    /// What the workspace menu offers now: the workspaces on this desktop,
    /// what this person may change, and the shell's own workspace commands.
    pub(super) fn workspace_menu_model(&self) -> WorkspaceMenuModel {
        let Some(chrome) = self.chrome.as_ref() else {
            return WorkspaceMenuModel::default();
        };
        let state = chrome.borrow();
        let (workspaces, active_room) = {
            let tree = state.tree();
            let workspaces = tree
                .desktop
                .workspaces
                .iter()
                .map(|workspace| WorkspaceChoice {
                    key: workspace.id.0.to_string(),
                    name: workspace.name.clone(),
                    current: workspace.id == tree.desktop.active,
                })
                .collect();
            let active_room = tree
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room);
            (workspaces, active_room)
        };
        let site_rooms = self.site_rooms();
        let site = active_room.and_then(|room| site_rooms.iter().find(|site| site.room == room));
        WorkspaceMenuModel {
            workspaces,
            // A workspace held through the web is hosted by its site; the
            // rest are on the person's own computer.
            host: site.and_then(|site| site.site.clone()),
            actions: NativeWorkspaceAction::ALL
                .into_iter()
                .filter(|action| action.offered(site))
                .map(NativeWorkspaceAction::menu_action)
                .collect(),
            can_rename: state.may_rename_active_workspace(),
            can_add_desk: state.can_add_strip(),
        }
    }

    /// Who the profile card is about, and what the shell offers about them.
    pub(super) fn profile_model(&self) -> ProfileModel {
        let (name, identity, colour) = self.chrome.as_ref().map_or_else(
            || (String::new(), String::new(), None),
            |chrome| {
                let state = chrome.borrow();
                let (r, g, b) = crate::shell::topbar_chrome::own_rgb(&state);
                (
                    crate::shell::topbar_chrome::own_shown_name(&state),
                    state.active_local_user(),
                    Some(vello::peniko::Color::from_rgb8(r, g, b)),
                )
            },
        );
        ProfileModel {
            name,
            identity,
            colour,
            // The native shell does not ask the room's host for linked
            // accounts yet; none hides the section rather than showing a
            // service the person cannot link from here.
            linked: Vec::new(),

            // The name is the identity's own; no rename of it is reachable
            // from the shell, so the card shows it and Settings' identities
            // page is one press away.
            can_rename: false,
            actions: NativeProfileAction::ALL
                .into_iter()
                .map(NativeProfileAction::profile_action)
                .collect(),
            offers_presence: true,
            sharing: Some(self.status_sharing),
            availability: self.self_presence.chosen,
            status: self
                .local_status
                .clone()
                .filter(|_| matches!(self.local_status_origin, ui::StatusOrigin::Manual))
                .unwrap_or_default(),
            colour_id: ui::tokens::PresenceHue::chosen_in(&serde_json::Value::Object(
                settings_store::load(),
            ))
            .map(|hue| hue.id().to_owned()),
            sight: self.chrome.as_ref().and_then(|chrome| {
                let state = chrome.borrow();
                let workspace = state
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(|workspace| (workspace.room, workspace.name.clone()));
                drop(state);
                let (room, name) = workspace?;
                Some(tilo_desktop::profile_card::WorkspaceSight {
                    workspace: name,
                    visibility: self.presence_visibility(room),
                })
            }),
        }
    }

    /// Carry out what the person chose in either popover.
    pub(super) fn settle_bar_popovers(&mut self) -> bool {
        let mut changed = false;
        if let Some(choice) = self.bar_popovers.workspace_menu.take_choice() {
            self.apply_workspace_menu_choice(choice);
            changed = true;
        }
        if let Some(choice) = self.bar_popovers.profile.take_choice() {
            self.apply_profile_choice(&choice);
            changed = true;
        }
        if changed {
            self.bump_chrome();
        }
        changed
    }

    fn apply_workspace_menu_choice(&mut self, choice: WorkspaceMenuChoice) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        match choice {
            WorkspaceMenuChoice::Switch(key) => {
                if let Ok(id) = key.parse::<u64>() {
                    chrome.borrow_mut().switch_to_workspace(WorkspaceId(id));
                }
            }
            WorkspaceMenuChoice::Action(id) => {
                if let Some(action) = NativeWorkspaceAction::from_id(&id) {
                    self.run_native_workspace_action(action);
                }
            }
            WorkspaceMenuChoice::Overview => {
                {
                    let mut state = chrome.borrow_mut();
                    if !state.overlays.overview {
                        state.overlays.overview = true;
                        state.bump();
                    }
                }
                // A click on it is a whole-view change the windows fly
                // through; a key that chose it keeps the flight short.
                self.latch_overview_transition();
            }
            WorkspaceMenuChoice::NewDesk => {
                if let Some(bar) = self
                    .desktop_surface_mut()
                    .and_then(tilo_desktop::composition::DesktopSurface::topbar_mut)
                {
                    bar.begin_new_desk();
                }
            }
            WorkspaceMenuChoice::Rename => {
                if let Some(bar) = self
                    .desktop_surface_mut()
                    .and_then(tilo_desktop::composition::DesktopSurface::topbar_mut)
                {
                    bar.begin_workspace_rename();
                }
            }
        }
    }

    pub(crate) fn run_native_workspace_action(&mut self, action: NativeWorkspaceAction) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let mut state = chrome.borrow_mut();
        let active_room = state
            .tree()
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room);
        match action {
            NativeWorkspaceAction::New => {
                state.add_workspace(t("search-action-workspace-create-default-name"));
            }
            NativeWorkspaceAction::Settings => {
                state.open_workspace_settings(WsTab::General);
            }
            NativeWorkspaceAction::Join => {
                state.push_peer_sync_request(super::PeerSyncAction::BrowseDirectory);
            }
            NativeWorkspaceAction::Leave => {
                if let Some(room) = active_room {
                    state.push_peer_sync_request(super::PeerSyncAction::LeaveSite { room });
                }
            }
            NativeWorkspaceAction::Report => {
                // The reason is typed in the palette, where the report
                // action takes it like any parameter.
                drop(state);
                self.open_palette_with_scope(crate::shell::chrome_state::SearchScope::All);
                self.palette.set_query(&t("search-action-workspace-report"));
                self.last_search_query = "\0workspace-report\0".to_string();
                return;
            }
        }
        state.bump();
    }

    pub(super) fn apply_profile_choice(&mut self, choice: &ProfileChoice) {
        let id = match choice {
            ProfileChoice::Action(id) => id,
            ProfileChoice::Availability(chosen) => {
                self.set_chosen_availability(*chosen);
                return;
            }
            ProfileChoice::Status(text) => {
                self.set_manual_status(Some(text.clone()));
                return;
            }
            ProfileChoice::Sharing(mode) => {
                self.set_status_sharing(*mode);
                return;
            }
            ProfileChoice::Colour(id) => {
                if let Err(error) = settings_store::identity::COLOUR.set(Some(id.clone())) {
                    tracing::warn!(%error, "could not keep the chosen colour");
                }
                crate::shell::state::read_local_presence_hue();
                return;
            }
            ProfileChoice::Visibility(choice) => {
                let room = self.chrome.as_ref().and_then(|chrome| {
                    let state = chrome.borrow();
                    let room = state.tree().desktop.active_workspace().map(|w| w.room);
                    room
                });
                if let Some(room) = room {
                    self.set_presence_visibility(room, *choice);
                }
                return;
            }
            // The card offers no name field here.
            ProfileChoice::Rename(_) | ProfileChoice::Link(_) | ProfileChoice::Unlink(_) => {
                return;
            }
        };
        let Some(action) = NativeProfileAction::from_id(id) else {
            return;
        };
        if action == NativeProfileAction::SaySomething {
            self.open_cursor_chat_on_request();
            return;
        }
        let Some(link) = action.settings_link() else {
            // Pairing opens its guided sheet right here.
            self.open_pairing();
            return;
        };
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        if let Err(error) = crate::shell::uri_open::open_in_workspace(link, chrome) {
            tracing::warn!(?error, link, "could not open Settings");
        }
    }

    /// The bar popovers' layout nodes, built while open.
    pub(super) fn build_bar_popovers(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn ui::widget::Widget>,
    ) -> Vec<NodeId> {
        let popovers = &mut self.bar_popovers;
        popovers.workspace_menu_node = popovers
            .workspace_menu
            .is_open()
            .then(|| popovers.workspace_menu.build_layout(taffy, theme, node_map));
        popovers.profile_node = popovers
            .profile
            .is_open()
            .then(|| popovers.profile.build_layout(taffy, theme, node_map));
        popovers
            .workspace_menu_node
            .into_iter()
            .chain(popovers.profile_node)
            .collect()
    }

    pub(super) fn apply_bar_popovers_layout(&mut self, taffy: &taffy::TaffyTree<()>) {
        let origin = self.rect.origin();
        let popovers = &mut self.bar_popovers;
        if let Some(node) = popovers.workspace_menu_node {
            popovers.workspace_menu.apply_layout(taffy, node, origin);
        }
        if let Some(node) = popovers.profile_node {
            popovers.profile.apply_layout(taffy, node, origin);
        }
    }

    /// Offer `event` to the open popover: it takes what it covers, the
    /// keyboard and the press that dismisses it. `true` when it did.
    pub(super) fn route_bar_popover_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        // A press while one is open is its own: it chooses, or it dismisses
        // without also reaching what is under it (the bar's own control
        // that opened it would open it again).
        let pressed = self.bar_popovers.any_open()
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
            );
        if self.bar_popovers.workspace_menu.is_open()
            && let Some(node) = self.bar_popovers.workspace_menu_node
        {
            self.bar_popovers
                .workspace_menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            self.settle_bar_popovers();
            if *ctx.handled {
                return true;
            }
        }
        if self.bar_popovers.profile.is_open()
            && let Some(node) = self.bar_popovers.profile_node
        {
            self.bar_popovers
                .profile
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            let modal = self.bar_popovers.profile.modal_capture().is_some();
            self.settle_bar_popovers();
            if *ctx.handled || modal {
                return true;
            }
        }
        if pressed {
            // A press neither took (one outside it before its layout caught
            // up with its opening) still dismisses it.
            self.close_bar_popovers_for(None);
            *ctx.handled = true;
        }
        pressed
    }

    pub(super) fn draw_bar_popovers(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.bar_popovers.workspace_menu.is_open() {
            self.bar_popovers.workspace_menu.draw_overlay(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
        if self.bar_popovers.profile.is_open() {
            self.bar_popovers.profile.draw_overlay(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
    }

    /// The workspace menu's rows as shown, for tests and probes.
    pub fn workspace_menu_labels(&self) -> Option<Vec<Option<String>>> {
        self.bar_popovers
            .workspace_menu
            .is_open()
            .then(|| self.bar_popovers.workspace_menu.labels())
    }

    /// Choose the workspace menu's row labelled `label`, as a click would.
    pub fn choose_in_workspace_menu(&mut self, label: &str) -> bool {
        let chosen = self.bar_popovers.workspace_menu.choose(label);
        self.settle_bar_popovers();
        chosen
    }

    /// Whether the profile card is open.
    pub fn profile_card_open(&self) -> bool {
        self.bar_popovers.profile.is_open()
    }

    /// The open profile card's bounds.
    pub fn profile_card_rect(&self) -> Option<Rect> {
        self.profile_card_open()
            .then(|| self.bar_popovers.profile.rect())
    }
}
