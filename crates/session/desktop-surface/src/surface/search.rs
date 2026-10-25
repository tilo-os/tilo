//! Desktop search on the surface: the shared [`DesktopSearch`] palette over
//! what this desktop has open, drawn above the shown strip.
//!
//! Search opens from the same chords as the native shell's (Super+K and
//! Super+Space; Ctrl+K where the platform's command key is Ctrl), from a
//! `global-search`/`commands` navigation command, or from
//! [`DesktopHandle::toggle_search`]. A window's new-tab control opens it
//! over the applications only, and the one chosen opens as a tab of that
//! window, as the native shell's does. While open it takes every input event.
//! Choosing an application launches it, a window focuses it, and an
//! application's action or content runs through the palette's control
//! invocation, which the frame host resolves against the live resource
//! tree like any bound control.

use tilo_desktop_model::{DesktopTree, GroupId, StripId, TabId, TileContent};
use tilo_desktop_search::providers::{CommandSpec, WindowEntry};
use tilo_desktop_search::{SearchData, Target};
use ui::automerge::AutoCommit;
use ui::kurbo::{Point, Rect};
use ui::taffy::{NodeId, TaffyTree};
use ui::widget::Widget;
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};

use super::{DesktopHandle, DesktopSurface, holder_refusal, route_of};
use crate::navigation::{self, Admitted};

/// What a host asked of search, taken on the next update pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SearchRequest {
    Open(Option<String>),
    Toggle,
    Applications,
    /// Window `GroupId`'s new-tab control: which application to open there.
    NewTab(StripId, GroupId),
    /// Window `GroupId`'s search control, or its app's own search button:
    /// search what its shown tab holds, narrowed and prefilled as the app
    /// asked.
    Window(StripId, GroupId, ui::search::SearchRequest),
}

const CLOSE_WINDOW: &str = "desktop.close-window";
const MAXIMIZE_WINDOW: &str = "desktop.maximize-window";
const WORKSPACE_PREFIX: &str = "desktop.workspace:";
/// Search command ids and banner action keys of the host's own actions.
const HOST_PREFIX: &str = "host:";
/// The least room the palette keeps from the surface's sides.
const PALETTE_EDGE: f64 = 8.0;

/// An action only the host that embeds the desktop can perform (in a
/// browser: keeping the person's identity with a passkey). The desktop
/// offers it in search beside its own commands and on banners the host
/// posts, and hands the id back when the person chooses it
/// ([`DesktopHandle::take_host_choices`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct HostAction {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: Option<String>,
    /// What the action is about, named as the source of the notices that
    /// offer it.
    #[serde(default)]
    pub source: Option<HostActionSource>,
    /// What a menu listing it calls it, when that differs from `title`
    /// (the workspace menu lists "Lounge", search says "Open Lounge").
    #[serde(default)]
    pub label: Option<String>,
}

/// The part of the system a host action belongs to, in the desktop's
/// language: the source line of a notice that offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostActionSource {
    /// Who the person is: keeping, recovering or naming their identity.
    Identity,
    /// The person's other devices: pairing one.
    Devices,
    /// Another of the person's workspaces, to go to.
    Workspaces,
    /// Workspaces themselves: joining one, leaving this one, a new one.
    Workspace,
}

impl HostActionSource {
    fn label(self) -> String {
        crate::i18n::t(match self {
            Self::Identity => "host-action-source-identity",
            Self::Devices => "host-action-source-devices",
            Self::Workspaces | Self::Workspace => "host-action-source-workspace",
        })
    }
}

impl HostAction {
    /// A banner that offers this action: choosing it performs the action.
    #[must_use]
    ///
    /// An offer is an ask: it stays until the person answers it or puts it
    /// off ("Not now", which keeps it in the notification centre), so
    /// nothing is decided by waiting.
    pub fn notice(
        &self,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> crate::notifications::Notification {
        let mut notice = crate::notifications::Notification::new(title, body);
        // The action first: it is what choosing the ask itself does, from
        // the banner or later from the centre.
        notice.actions = vec![
            (self.key(), self.title.clone()),
            (
                crate::notifications::NOT_NOW_ACTION.to_owned(),
                crate::i18n::t("notification-not-now"),
            ),
        ];
        notice.app_name = self.source.map(HostActionSource::label);
        notice.dismiss_after = crate::notifications::PERSISTENT_DISMISS;
        // What a host asks is on the person's behalf: who they are and
        // which devices are theirs.
        notice.icon_kind = crate::notifications::NotificationIconKind::Permission;
        notice
    }

    /// The banner action key this action is offered under.
    fn key(&self) -> String {
        format!("{HOST_PREFIX}{}", self.id)
    }
}

pub fn is_host_offer(notification: &crate::notifications::Notification) -> bool {
    notification
        .actions
        .iter()
        .any(|(key, _)| host_choice(key).is_some())
}

/// Whether `notification` offers host action `id`.
pub(crate) fn offers(notification: &crate::notifications::Notification, id: &str) -> bool {
    notification
        .actions
        .iter()
        .any(|(key, _)| host_choice(key) == Some(id))
}

/// The host action a banner action key or search command id names.
pub(super) fn host_choice(key: &str) -> Option<&str> {
    key.strip_prefix(HOST_PREFIX)
}

impl DesktopHandle {
    /// Offer these host actions from now on, in place of earlier ones.
    /// Returns the ids no longer offered: a banner still asking for one of
    /// them asks for something already done, so the host withdraws it
    /// ([`crate::DesktopNotifications::withdraw_host_action`]).
    pub fn set_host_actions(&self, actions: Vec<HostAction>) -> Vec<String> {
        let mut shared = self.shared.borrow_mut();
        let withdrawn: Vec<String> = shared
            .host_actions
            .iter()
            .filter(|old| actions.iter().all(|new| new.id != old.id))
            .map(|old| old.id.clone())
            .collect();
        shared.host_actions = actions;
        drop(shared);
        self.withdraw_personal_ask(&withdrawn);
        withdrawn
    }

    /// The host actions offered now, in the host's order.
    pub fn host_actions(&self) -> Vec<HostAction> {
        self.shared.borrow().host_actions.clone()
    }

    /// Whether the host offers the action `id` (see [`Self::set_host_actions`]).
    pub fn offers_host_action(&self, id: &str) -> bool {
        self.shared
            .borrow()
            .host_actions
            .iter()
            .any(|action| action.id == id)
    }

    /// The host actions the person chose since the last call, by id.
    pub fn take_host_choices(&self) -> Vec<String> {
        std::mem::take(&mut self.shared.borrow_mut().host_choices)
    }

    /// Open search, or close it when it is open. The host schedules a frame.
    pub fn toggle_search(&self) {
        self.shared.borrow_mut().search_request = Some(SearchRequest::Toggle);
    }

    /// Open search with `query` typed. The host schedules a frame.
    pub fn open_search(&self, query: Option<String>) {
        self.shared.borrow_mut().search_request = Some(SearchRequest::Open(query));
    }

    /// Open search over every application, each listed (an empty desk's
    /// "Open an app"). The host schedules a frame.
    pub fn open_applications(&self) {
        self.shared.borrow_mut().search_request = Some(SearchRequest::Applications);
    }

    /// Whether the search palette is open, as of the last update pass.
    pub fn search_open(&self) -> bool {
        self.shared.borrow().search_open
    }
}

/// The search chords: Super+K and Super+Space as in the native shell, and
/// Ctrl+K where Ctrl is the platform's command key.
fn is_search_chord(
    event: &WindowEvent,
    modifiers: ui::winit::event::Modifiers,
    platform: ui::platform::PlatformConventions,
) -> bool {
    let WindowEvent::KeyboardInput { event: key, .. } = event else {
        return false;
    };
    if key.state != ElementState::Pressed || key.repeat {
        return false;
    }
    let mods = modifiers.state();
    if mods.alt_key() || mods.shift_key() {
        return false;
    }
    let command = mods.super_key()
        || (mods.control_key() && platform.platform() != ui::platform::Platform::MacOs);
    match &key.logical_key {
        Key::Character(text) => command && text.eq_ignore_ascii_case("k"),
        Key::Named(NamedKey::Space) => mods.super_key() && !mods.control_key(),
        _ => false,
    }
}

fn is_escape(event: &WindowEvent) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput { event: key, .. }
            if key.state == ElementState::Pressed
                && key.logical_key == Key::Named(NamedKey::Escape)
    )
}

/// Every open window as search lists it: the app it shows (for its glyph),
/// and where it is. Windows titled alike say what tells them apart: who
/// holds the window, or else which of them it is, counted left to right.
/// The name of the app window `group` shows, as the desktop labels it.
fn window_app(tree: &DesktopTree, group: GroupId) -> Option<String> {
    let window = tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .find(|window| window.id == group)?;
    let tab = window.tabs.iter().find(|tab| tab.id == window.active_tab)?;
    match &tab.tile {
        TileContent::LocalApp { label, .. } => Some(label.clone()),
        _ => None,
    }
}

fn windows(
    tree: &DesktopTree,
    holders: &std::collections::HashMap<TabId, ui::widget::TabHolder>,
) -> Vec<WindowEntry> {
    let mut out = Vec::new();
    for workspace in &tree.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    let app_title = match &tab.tile {
                        TileContent::LocalApp { label, .. } => label.clone(),
                        _ => String::new(),
                    };
                    let app = tilo_desktop_model::apps::canonical_app_id(&tab.tile);
                    let place = holders
                        .get(&tab.id)
                        .map(|holder| holder.name.clone())
                        .into_iter()
                        .chain([workspace.name.clone(), strip.name.clone()])
                        .collect();
                    out.push(WindowEntry {
                        tab: tab.id,
                        title: tab.title.clone(),
                        app_title,
                        app,
                        place,
                    });
                }
            }
        }
    }
    let alike = |a: &WindowEntry, b: &WindowEntry| a.title == b.title && a.place == b.place;
    let counted: Vec<(usize, usize)> = out
        .iter()
        .enumerate()
        .map(|(at, entry)| {
            // Counting itself: the first of them is the first.
            let nth = out
                .iter()
                .take(at.saturating_add(1))
                .filter(|other| alike(other, entry))
                .count();
            let all = out.iter().filter(|other| alike(other, entry)).count();
            (nth, all)
        })
        .collect();
    for (entry, (nth, all)) in out.iter_mut().zip(counted) {
        if all > 1 {
            entry.place.push(crate::i18n::t_args(
                "search-window-nth",
                &[("nth", nth.into()), ("count", all.into())],
            ));
        }
    }
    out
}

/// What search offers to do: the host's actions, then what the focused
/// window allows (no close for a window someone else holds, `held`), then
/// the other workspaces.
fn commands(
    tree: &DesktopTree,
    host: &[HostAction],
    held: &dyn Fn(TabId) -> bool,
) -> Vec<CommandSpec> {
    let mut out: Vec<CommandSpec> = host
        .iter()
        .map(|action| CommandSpec {
            id: format!("{HOST_PREFIX}{}", action.id),
            title: action.title.clone(),
            subtitle: action.subtitle.clone(),
        })
        .collect();
    if let Some(ids) = navigation::active_route(tree)
        && let Some(group) = navigation::locate(tree, ids)
    {
        let title = group
            .tabs
            .iter()
            .find(|tab| tab.id == ids.tab)
            .map(|tab| tab.title.clone());
        if !held(ids.tab) {
            out.push(CommandSpec {
                id: CLOSE_WINDOW.into(),
                title: "Close window".into(),
                subtitle: title.clone(),
            });
        }
        out.push(CommandSpec {
            id: MAXIMIZE_WINDOW.into(),
            title: if group.maximized {
                "Restore window size".into()
            } else {
                "Maximize window".into()
            },
            subtitle: title,
        });
    }
    for workspace in &tree.workspaces {
        if workspace.id != tree.active {
            out.push(CommandSpec {
                id: format!("{WORKSPACE_PREFIX}{}", workspace.id.0),
                title: format!("Switch to {}", workspace.name),
                subtitle: Some("Workspace".into()),
            });
        }
    }
    out
}

impl DesktopSurface {
    /// Open search over what the desktop has open now.
    pub(super) fn open_search(&mut self, doc: &AutoCommit, query: Option<String>) {
        let (windows, commands) = {
            let shared = self.shared.borrow();
            (
                windows(&shared.tree, &shared.holders),
                commands(&shared.tree, &shared.host_actions, &|tab| {
                    shared.holders.contains_key(&tab)
                }),
            )
        };
        let mut data = SearchData {
            applications: self.tiles.search_applications(),
            windows,
            commands,
            ..SearchData::default()
        };
        let mut owners = std::collections::HashMap::new();
        for (strip, row) in &self.rows {
            data.resources
                .extend(ui::widget::published_resources(row, doc));
            data.providers
                .extend(ui::widget::published_search_providers(row));
            for window in &row.groups {
                let Some(host) = window.active_host() else {
                    continue;
                };
                let mut open = ui::widget::published_resources(host, doc);
                while let Some(resource) = open.pop() {
                    owners
                        .entry(resource.id.as_str().to_owned())
                        .or_insert((*strip, window.id));
                    open.extend(resource.children);
                }
            }
        }
        self.search.open(data);
        self.search
            .palette_mut()
            .set_platform_conventions(self.platform);
        self.new_tab_into = None;
        self.search_window = None;
        self.search_owners = owners;
        if let Some(query) = query {
            self.search.set_query(&query);
        }
        self.structure_dirty = true;
    }

    fn close_search(&mut self) {
        self.search.close();
        self.structure_dirty = true;
    }

    /// Open search over the applications only, with nothing typed: every app
    /// is listed, a group of one kind being shown whole. Aimed at a window
    /// (`into`), what is chosen opens as its tab; else as a window of its
    /// own. Opens even while search is already open, as another press of a
    /// window's new-tab control re-aims it.
    fn open_applications_search(&mut self, into: Option<(StripId, GroupId)>) {
        let data = SearchData {
            applications: self.tiles.search_applications(),
            ..SearchData::default()
        };
        self.search.open(data);
        self.search
            .palette_mut()
            .set_platform_conventions(self.platform);
        self.search.set_query("");
        self.new_tab_into = into;
        self.search_window = None;
        self.search_owners.clear();
        self.structure_dirty = true;
    }

    /// Open search over what window `group` shows: its shown tab's
    /// resources and search providers, the in-app search its search control
    /// and the app's own search button offer, narrowed to the part of the
    /// app `request` names and with its query typed. Opens over everything
    /// when the window is gone.
    fn open_window_search(
        &mut self,
        doc: &AutoCommit,
        strip: StripId,
        group: GroupId,
        request: ui::search::SearchRequest,
    ) {
        let host = self
            .rows
            .get(&strip)
            .and_then(|row| row.groups.iter().find(|candidate| candidate.id == group))
            .and_then(|window| window.active_host());
        let Some(host) = host else {
            self.open_search(doc, request.query);
            return;
        };
        // The field says what it searches: the part of the app asked for,
        // else the app itself.
        let scope = request.resource_scope();
        let searched = scope
            .map(|scope| scope.label.clone())
            .or_else(|| window_app(&self.shared.borrow().tree, group))
            .filter(|name| !name.is_empty());
        let data = SearchData {
            resources: ui::widget::published_resources(host, doc),
            providers: ui::widget::published_search_providers(host),
            content_only: true,
            scope: scope.map(|scope| scope.anchor.clone()),
            placeholder: searched.map(|name| {
                crate::i18n::t_args("search-window-placeholder", &[("searched", name.into())])
            }),
            ..SearchData::default()
        };
        self.search.open(data);
        self.search
            .palette_mut()
            .set_platform_conventions(self.platform);
        self.search
            .set_query(request.query.as_deref().unwrap_or_default());
        self.new_tab_into = None;
        self.search_window = Some((strip, group));
        self.search_owners.clear();
        self.structure_dirty = true;
    }

    /// Take a host's search request, follow the palette, and perform what
    /// was chosen. Runs every update pass, before queued commands apply.
    pub(super) fn step_search(&mut self, doc: &AutoCommit) {
        let request = self.shared.borrow_mut().search_request.take();
        match request {
            Some(SearchRequest::Toggle) if self.search.is_open() => self.close_search(),
            Some(SearchRequest::Toggle) => self.open_search(doc, None),
            Some(SearchRequest::Open(query)) => self.open_search(doc, query),
            Some(SearchRequest::Applications) => self.open_applications_search(None),
            Some(SearchRequest::NewTab(strip, group)) => {
                self.open_applications_search(Some((strip, group)));
            }
            Some(SearchRequest::Window(strip, group, request)) => {
                self.open_window_search(doc, strip, group, request);
            }
            None => {}
        }
        self.follow_search();
    }

    /// After the palette handled input (an event, or an assistive
    /// technology action), take its outcome.
    fn follow_search(&mut self) {
        if self.search.is_open() {
            let closed = self.search.observe();
            let refreshed = self.search.refresh();
            self.structure_dirty |= closed || refreshed;
        }
        if (self.search_window.is_some() || !self.search_owners.is_empty())
            && let Some(invocation) =
                ui::widget::controls::take_control_invocation(self.search.palette_mut())
        {
            match self.search_window {
                Some((strip, group)) => self.run_in_window(strip, group, invocation),
                None => match self
                    .search_owners
                    .get(invocation.address.resource.as_str())
                    .copied()
                {
                    // Chosen in search over everything: the app's window
                    // comes forward and runs it.
                    Some((strip, group)) => {
                        self.bring_forward(strip, group);
                        self.run_in_window(strip, group, invocation);
                    }
                    // Not an app's: the desktop's own door runs it.
                    None => self.unrouted_control = Some(invocation),
                },
            }
        }
        let choice = self.search.take_choice();
        if matches!(choice, Some(Target::Launch { .. })) {
            // The person opened something from search themselves: what
            // the desktop opens on its own (a room's home app) teaches
            // nothing.
            ui::control_hints::performed::note(
                ui::control_hints::performed::Performed::OpenedWindow,
            );
        }
        match (choice, self.new_tab_into.take()) {
            (Some(Target::Launch { app }), Some((strip, group))) => {
                self.open_tab_in_group(strip, group, &app);
            }
            (Some(target), _) => self.perform(target),
            (None, into) => self.new_tab_into = into,
        }
        let open = self.search.is_open();
        if open {
            ui::control_hints::performed::note(
                ui::control_hints::performed::Performed::OpenedSearch,
            );
        } else {
            // The aim is for one choice; search opened otherwise opens
            // applications as their own windows again.
            self.new_tab_into = None;
            self.search_window = None;
            self.search_owners.clear();
        }
        self.shared.borrow_mut().search_open = open;
    }

    /// Focus window `group`, as choosing it in search does.
    fn bring_forward(&self, strip: StripId, group: GroupId) {
        let tab = self
            .rows
            .get(&strip)
            .and_then(|row| row.groups.iter().find(|candidate| candidate.id == group))
            .map(ui::widget::WindowGroup::active_tab_id);
        if let Some(tab) = tab {
            self.perform(Target::Window { tab, app: None });
        }
    }

    /// Run `invocation`, chosen in the search over window `group`, inside
    /// the app that window shows, as its own control would: the desktop's
    /// door never reaches into an app's actions by address.
    fn run_in_window(
        &mut self,
        strip: StripId,
        group: GroupId,
        invocation: ui::tilo_actions::ControlInvocation,
    ) {
        let host = self
            .rows
            .get_mut(&strip)
            .and_then(|row| {
                row.groups
                    .iter_mut()
                    .find(|candidate| candidate.id == group)
            })
            .and_then(|window| window.active_host_mut());
        match host {
            Some(host) => {
                if let Err(invocation) = host.queue_control(invocation) {
                    self.unrouted_control = Some(invocation);
                }
            }
            None => self.unrouted_control = Some(invocation),
        }
        self.structure_dirty = true;
    }

    /// Queue what a chosen row does, as the matching navigation command.
    fn perform(&self, target: Target) {
        let mut shared = self.shared.borrow_mut();
        let admitted = match target {
            // An app already open is focused, not opened twice: a new
            // window is asked for through a window's new tab instead.
            Target::Launch { app } => Some(
                navigation::window_of_app(&shared.tree, &app)
                    .map_or(Admitted::Launch(app), Admitted::Select),
            ),
            Target::Window { tab, .. } => route_of(&shared.tree, tab).map(Admitted::Select),
            Target::Command { id } => {
                let active = navigation::active_route(&shared.tree);
                if let Some(choice) = host_choice(&id) {
                    let choice = choice.to_owned();
                    shared.host_choices.push(choice);
                    None
                } else if id == CLOSE_WINDOW {
                    match active {
                        Some(ids) if shared.holders.contains_key(&ids.tab) => {
                            let refusal = holder_refusal(&shared.holders, ids.tab);
                            shared.acknowledger.say(refusal);
                            None
                        }
                        active => active.map(Admitted::Close),
                    }
                } else if id == MAXIMIZE_WINDOW {
                    active.and_then(|ids| {
                        let group = navigation::locate(&shared.tree, ids)?;
                        Some(Admitted::Maximize(ids, !group.maximized))
                    })
                } else {
                    id.strip_prefix(WORKSPACE_PREFIX)
                        .and_then(|id| id.parse::<u64>().ok())
                        .map(tilo_desktop_model::WorkspaceId)
                        .filter(|id| shared.tree.workspaces.iter().any(|w| w.id == *id))
                        .map(Admitted::Workspace)
                }
            }
            // Runs through the palette's control invocation instead.
            Target::Action(_) => None,
        };
        if let Some(admitted) = admitted {
            shared.queue.push(admitted);
        }
    }

    /// Route one event while search is open, or open it on its chord.
    /// Returns whether search took the event.
    pub(super) fn search_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let modifiers = *ctx.modifiers;
        if !self.search.is_open() {
            if is_search_chord(event, modifiers, self.platform) {
                self.open_search(&ctx.state, None);
                self.shared.borrow_mut().search_open = true;
                return true;
            }
            return false;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            let scale = ctx.scale_factor.max(f64::MIN_POSITIVE);
            self.search_pointer = Point::new(position.x / scale, position.y / scale);
        }
        let outside_press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && !self
            .search
            .palette()
            .get_rect()
            .contains(self.search_pointer);
        if outside_press || is_search_chord(event, modifiers, self.platform) {
            self.close_search();
        } else {
            if is_escape(event) {
                self.search.dismissing();
            }
            let path = ctx.path.clone();
            let node_id = ctx.node_id;
            self.search
                .palette_mut()
                .handle_event(event, &mut ctx.reborrow(&path, node_id));
        }
        self.follow_search();
        true
    }

    /// Dim the desktop and draw the palette above it.
    pub(super) fn draw_search(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if !self.search.is_open() {
            return;
        }
        scene.fill(
            ui::vello::peniko::Fill::NonZero,
            transform,
            ui::vello::peniko::Color::from_rgba8(0, 0, 0, 110),
            None,
            &self.rect,
        );
        self.search
            .palette_mut()
            .draw(scene, transform, text, theme, presence);
    }

    /// Where the open palette's card stands.
    #[cfg(test)]
    pub(super) fn search_palette_rect(&self) -> Rect {
        self.search.palette().get_rect()
    }

    /// The window a search over one window (`search_window`) stands over,
    /// under its bar, which the palette keeps clear.
    fn searched_window_rect(&self) -> Option<Rect> {
        let (strip, group) = self.search_window?;
        self.rows
            .get(&strip)?
            .groups
            .iter()
            .find(|candidate| candidate.id == group)
            .map(|group| {
                let window = group.get_rect();
                Rect::new(
                    window.x0,
                    group.bar_rect().y1.clamp(window.y0, window.y1),
                    window.x1,
                    window.y1,
                )
            })
            .filter(|rect| rect.area() > 0.0)
    }

    /// Place the palette's layout node: over the window a search of one
    /// window was opened from, as an app's own palette stands over its
    /// window, else centred near the top of the surface. It keeps its own
    /// width where the surface has room for it, narrows to fit where it
    /// has not (a phone), and stays on the surface.
    pub(super) fn search_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> Option<NodeId> {
        if !self.search.is_open() {
            return None;
        }
        let surface = self.rect;
        let palette = self.search.palette_mut();
        palette
            .set_max_width((surface.width() > 0.0).then(|| surface.width() - 2.0 * PALETTE_EDGE));
        let node = palette.build_layout(taffy, theme, node_map);
        let (width, _) = palette.desired_size();
        let mut style = taffy.style(node).cloned().unwrap_or_default();
        style.position = ui::taffy::Position::Absolute;
        if let Some(window) = self.searched_window_rect() {
            let (origin, _) = ui::action_palette::placement(window, theme);
            let x0 = (window.center().x - width / 2.0)
                .min(surface.x1 - width - PALETTE_EDGE)
                .max(surface.x0 + PALETTE_EDGE);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a place on the surface is a few thousand logical pixels"
            )]
            let at = |v: f64| ui::taffy::LengthPercentageAuto::length(v as f32);
            style.inset = ui::taffy::Rect {
                left: at(x0 - surface.x0),
                right: ui::taffy::LengthPercentageAuto::auto(),
                top: at(origin.y - surface.y0),
                bottom: ui::taffy::LengthPercentageAuto::auto(),
            };
            style.margin.left = ui::taffy::LengthPercentageAuto::length(0.0);
            let _ = taffy.set_style(node, style);
            return Some(node);
        }
        // Centred, and within the surface's edges even where it is barely
        // wider than the card.
        let x0 = (surface.center().x - width / 2.0)
            .min(surface.x1 - width - PALETTE_EDGE)
            .max(surface.x0 + PALETTE_EDGE);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a place on the surface is a few thousand logical pixels"
        )]
        let left = (x0 - surface.x0) as f32;
        style.inset = ui::taffy::Rect {
            left: ui::taffy::LengthPercentageAuto::length(left),
            right: ui::taffy::LengthPercentageAuto::auto(),
            top: ui::taffy::LengthPercentageAuto::percent(0.1),
            bottom: ui::taffy::LengthPercentageAuto::auto(),
        };
        style.margin.left = ui::taffy::LengthPercentageAuto::length(0.0);
        let _ = taffy.set_style(node, style);
        Some(node)
    }
}

/// The ids of the commands search offers over `tree`, with the windows
/// `held` by someone else.
#[cfg(test)]
pub(super) fn offered_commands(tree: &DesktopTree, held: &dyn Fn(TabId) -> bool) -> Vec<String> {
    commands(tree, &[], held)
        .into_iter()
        .map(|spec| spec.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::winit::event::{KeyEvent, Modifiers, RawKeyEvent};
    use ui::winit::keyboard::{KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey};

    fn press(key: Key) -> WindowEvent {
        let event = KeyEvent::synthetic(
            RawKeyEvent {
                physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
                state: ElementState::Pressed,
            },
            key.clone(),
            None,
            KeyLocation::Standard,
            false,
            None,
            key,
        );
        ui::synthetic_input::keyboard_input(event)
    }

    fn mods(state: ModifiersState) -> Modifiers {
        Modifiers::from(state)
    }

    #[test]
    fn a_host_actions_notice_names_what_it_is_about() {
        let pair: HostAction = serde_json::from_str(
            r#"{"id":"pair-device","title":"Pair a new device","source":"devices"}"#,
        )
        .unwrap();
        let notice = pair.notice("Pair “Phone”?", "");
        assert_eq!(
            crate::notifications::source_label(&notice),
            Some(crate::i18n::t("host-action-source-devices"))
        );
        let plain: HostAction = serde_json::from_str(r#"{"id":"x","title":"X"}"#).unwrap();
        assert_eq!(
            crate::notifications::source_label(&plain.notice("X", "")),
            None
        );
    }

    #[test]
    fn search_chords_match_the_native_shell() {
        let linux = ui::platform::PlatformConventions::new(ui::platform::Platform::Linux);
        let mac = ui::platform::PlatformConventions::new(ui::platform::Platform::MacOs);
        let k = press(Key::Character("k".into()));
        let space = press(Key::Named(NamedKey::Space));
        assert!(is_search_chord(&k, mods(ModifiersState::SUPER), linux));
        assert!(is_search_chord(&k, mods(ModifiersState::SUPER), mac));
        assert!(is_search_chord(&k, mods(ModifiersState::CONTROL), linux));
        assert!(!is_search_chord(&k, mods(ModifiersState::CONTROL), mac));
        assert!(is_search_chord(&space, mods(ModifiersState::SUPER), linux));
        assert!(!is_search_chord(
            &space,
            mods(ModifiersState::CONTROL),
            linux
        ));
        assert!(!is_search_chord(
            &k,
            mods(ModifiersState::SUPER | ModifiersState::SHIFT),
            linux
        ));
        assert!(!is_search_chord(&k, mods(ModifiersState::empty()), linux));
    }
}
