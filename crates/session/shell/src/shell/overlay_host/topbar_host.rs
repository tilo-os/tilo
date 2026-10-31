//! The top bar, hosted: the top bar app ([`tilo_topbar`]) mounted in the
//! shell's own process over the desktop, with the feed of what the chrome
//! knows lent to it.
//!
//! The shell hands the bar its workspaces, status, time, people, tray items
//! and what its menus offer, and where each display's bar goes; it routes
//! the bar the pointer, the keys while the bar holds the keyboard or has a
//! menu open, and what only the desktop sees (a window maximizing under the
//! bar); and it carries out what the bar asks through the chrome's own
//! operations. What the bar shows (its bars, the controls menus hang from,
//! whether it holds the keyboard or itself shown) comes back as plain data
//! the shell's hit testing, reveal and anchors read. The desktop's column
//! keeps the bar's room with a [`tilo_desktop::topbar::BarSpace`].

use std::time::Instant as WallInstant;

use tilo_desktop::chrome::{ChromeIntent, ChromeOverlays, ClockActivity, DesktopChromeSnapshot};
use tilo_topbar::{
    Ask, BarFeed, BarPlace, BarService, Command, Link, MediaCommand, Placement, Shown, TopBarApp,
    TopBarSurface,
};
use ui::widget::AppWidget;
use vello::kurbo::Vec2;

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::shell::topbar_chrome::{self as bar_data, ClockPoll};

/// The instance the bar is mounted as: no tab of any workspace.
const TOPBAR_INSTANCE: u64 = u64::MAX - 8;

/// What the shell keeps of the bar it hosts.
#[derive(Default)]
pub(super) struct TopBarHost {
    link: Link,
    app: Option<Box<dyn AppWidget>>,
    /// The app's node in the host's layout, while it is laid out there.
    node: Option<NodeId>,
    /// Mounting failed; the session goes on without the bar.
    failed: bool,
    /// Where the bar's area starts in the desktop's coordinates.
    origin: Point,
    /// What the feed last published came from, to publish only a change.
    published: Option<FeedKey>,
    /// What the bar showed at its last frame or input, in the desktop's
    /// coordinates.
    shown: Shown,
    /// The Clocks app's live activity, taken in at most a few times a second.
    clock: ClockPoll,
    /// The last window motion handed to the bar: shown or not.
    with_window: Option<bool>,
    /// The pointer was over the bar at its last move: the move that takes it
    /// off reaches the bar too, so no control stays lifted.
    hovered: bool,
    /// The shell's own Tab moved keyboard focus onto the bar: reading it
    /// with the keyboard holds it shown, as a Tab inside it does. Focus the
    /// shell merely placed on it (a fresh desktop's first focus) does not.
    reached_by_keyboard: bool,
}

/// What of the chrome the feed is built from: a change of any of it
/// publishes the feed again.
#[derive(Clone, Debug, PartialEq)]
struct FeedKey {
    revision: u64,
    tray: u64,
    identity: u64,
    presence: tilo_desktop::chrome::TopBarPresence,
    fullscreen: bool,
    clock_overlay: bool,
    clock_activity: Option<ClockActivity>,
    continuity: Option<String>,
}

fn moved(rect: Rect, by: Vec2) -> Rect {
    if rect.is_zero_area() {
        rect
    } else {
        vello::kurbo::Affine::translate(by).transform_rect_bbox(rect)
    }
}

fn local(rect: Rect, origin: Point) -> Rect {
    vello::kurbo::Affine::translate(origin.to_vec2())
        .inverse()
        .transform_rect_bbox(rect)
}

impl TopBarHost {
    /// The bar as a child of the host, while it is laid out.
    pub(super) fn child(&self) -> Option<&dyn Widget> {
        self.node?;
        self.app.as_deref().map(|app| app as &dyn Widget)
    }

    pub(super) fn child_mut(&mut self) -> Option<&mut (dyn Widget + '_)> {
        self.node?;
        self.app
            .as_deref_mut()
            .map(|app| app as &mut (dyn Widget + '_))
    }

    /// Take what the bar shows now, moved into the desktop's coordinates.
    fn refresh_shown(&mut self) {
        let by = self.origin.to_vec2();
        let mut shown = self.link.shown();
        for (_, rect, _) in &mut shown.bars {
            *rect = moved(*rect, by);
        }
        for rect in [
            &mut shown.workspace_name,
            &mut shown.own_avatar,
            &mut shown.people,
            &mut shown.tray,
            &mut shown.clock,
            &mut shown.popover,
        ] {
            *rect = moved(*rect, by);
        }
        self.shown = shown;
    }

    /// Whether `p` is on something the bar draws: a bar, the floating
    /// clock or the open menu.
    fn covers(&self, p: Point) -> bool {
        self.shown.bars.iter().any(|(_, rect, _)| rect.contains(p))
            || self.shown.clock.contains(p)
            || self.shown.popover.contains(p)
    }

    /// Whether keyboard focus is in the bar: Tab moved the shell's focus
    /// onto its tile, or a control of the bar took it (its focus action, a
    /// click, the workspace's name being renamed).
    fn has_keyboard(&self) -> bool {
        self.shown.keyboard || self.natively_focused()
    }

    fn natively_focused(&self) -> bool {
        self.app.as_ref().is_some_and(|app| app.is_focused())
    }

    /// What the bar paints, in the desktop's coordinates: each bar with the
    /// focus ring and hairline around its controls, and what floats on it
    /// (its menu, the clock) with their shadows.
    fn painted_bounds(&self, theme: &Theme) -> Vec<Rect> {
        let ring = ui::widget::focus_ring_reach(theme);
        let mut bounds: Vec<Rect> = self
            .shown
            .bars
            .iter()
            .map(|(_, rect, _)| rect.inflate(ring, ring))
            .collect();
        for rect in [self.shown.clock, self.shown.popover] {
            if !rect.is_zero_area() {
                bounds.push(
                    ui::surface::floating_shadow_bounds(rect, theme)
                        .union(rect.inflate(ring, ring)),
                );
            }
        }
        bounds
    }
}

impl ShellOverlayHost {
    /// Mount the bar the first time the chrome is there to feed it: among
    /// the session's in-process apps on the session's broker, so its
    /// actions publish, or on its own where the session has none.
    fn mount_topbar(&mut self) {
        if self.topbar.app.is_some() || self.topbar.failed {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let link = self.topbar.link.clone();
        let apps = chrome.borrow().hosted_inputs.in_process().cloned();
        let mounted = match apps {
            Some(apps) => {
                let room = ::launcher::appdir::PERSONAL_ROOM;
                let component = apps.borrow_mut().mount(
                    tilo_topbar::definition(),
                    room,
                    TOPBAR_INSTANCE,
                    ui::record::RecordId::room_default(),
                    |context| context.lend(BarService { link }),
                );
                component.and_then(|component| {
                    let collaboration = apps.borrow().collaboration(&component, room);
                    tilo_desktop::resolver::app_tile(component, collaboration, |_| {
                        crate::shell::wake_compositor();
                    })
                })
            }
            None => tilo_topbar::prepare(link, "desktop", "person").and_then(|component| {
                tilo_desktop::resolver::app_tile(component, None, |_| {
                    crate::shell::wake_compositor();
                })
            }),
        };
        match mounted {
            Ok(app) => self.topbar.app = Some(app),
            // The loop never waits on the disk for the record: the store's
            // thread opens it, and a later frame mounts again.
            Err(error) if error.is_opening() => {}
            Err(error) => {
                tracing::warn!(%error, "top bar could not be mounted");
                self.topbar.failed = true;
            }
        }
    }

    /// What the bar shows of the chrome now, when it changed since the feed
    /// was last published.
    fn topbar_feed(&mut self, chrome: &crate::shell::chrome_state::Chrome) -> Option<BarFeed> {
        let fixture_clock = chrome.borrow().hosted_inputs.fixture_clock();
        let mut state = chrome.borrow_mut();
        bar_data::refresh_clock_activity(&mut state, &mut self.topbar.clock);
        let clock_activity = state.clock_activity.as_ref().map(|activity| ClockActivity {
            activity_id: activity.activity_id.clone(),
            label: activity.label.clone(),
            remaining_secs: activity.remaining_secs,
            stopwatch: activity.stopwatch,
            paused: activity.paused,
            overlay: activity.overlay,
        });
        let continuity = state
            .continuity_suggestion(WallInstant::now())
            .map(|suggestion| crate::shell::continuity_suggestion::continue_label(&suggestion));
        let key = FeedKey {
            revision: state.revision,
            tray: crate::tray::global().revision(),
            identity: ui::local_identity::generation(),
            presence: state.top_bar_presence(),
            fullscreen: state.fullscreen_covers_primary(),
            clock_overlay: state.overlays.clock,
            clock_activity: clock_activity.clone(),
            continuity: continuity.clone(),
        };
        if self.topbar.published.as_ref() == Some(&key) {
            return None;
        }
        self.topbar.published = Some(key);
        let app_tray = bar_data::app_tray_items();
        let app_badges = app_tray
            .iter()
            .filter_map(|item| item.owner_app.clone())
            .map(|app| {
                let badge = state.notification_badge_for(&app);
                (app, badge)
            })
            .collect();
        let own = tilo_desktop::chrome::OwnChip {
            name: bar_data::own_shown_name(&state),
            rgb: bar_data::own_rgb(&state),
            identity: String::new(),
        };
        let snapshot = DesktopChromeSnapshot {
            revision: state.revision,
            status: bar_data::system_status(&state.quick_settings.status),
            clock: tilo_desktop::chrome::ClockSnapshot::default(),
            overlays: ChromeOverlays {
                overview: state.overlays.overview,
                ws_settings: state.overlays.ws_settings,
                clock: state.overlays.clock,
            },
            // A fullscreen owns its display: over the primary one, the full
            // bar steps aside. User maximize still fills the workspace but
            // keeps the bar.
            fullscreen: state.fullscreen_covers_primary(),
            notification_badge: state.notification_badge_total(),
            notifications_quiet: state.quiet_status_now().is_some(),
            // The status cluster opens quick settings here.
            notifications_only: false,
            activity_glance: state.activity_glance.clone(),
            viewport_size: state.viewport_size(),
            clock_activity,
            active_exposures: state.active_exposures().to_vec(),
            // Native presence has no spectator role yet.
            watching: 0,
            top_bar: state.top_bar_presence(),
            own: Some(own),
            invite: tilo_desktop::chrome::Invite::when(state.may_invite_here()),
            waved: state.wave_notice.clone(),
            arrived: state.arrival_notice.clone(),
        };
        let tree = state.tree().desktop.clone();
        let local_user = state
            .explicit_local_user()
            .map_or_else(crate::shell::state::local_user_id, str::to_owned);
        drop(state);
        Some(BarFeed {
            tree,
            local_user,
            snapshot,
            // Owned fixtures paint at an owned clock value, and a capture
            // pins it: reading wall time would change an otherwise identical
            // scene at a minute boundary.
            clock: fixture_clock.or_else(|| {
                std::env::var("TILO_CLOCK")
                    .ok()
                    .filter(|pinned| !pinned.is_empty())
                    .map(|_| bar_data::clock_snapshot())
            }),
            continuity,
            // The device keeps who its person is: nothing is asked from the
            // avatar here.
            avatar_ask: None,
            app_tray,
            app_tray_revision: crate::tray::global().revision(),
            app_badges,
            workspace_menu: self.workspace_menu_model(),
            profile: self.profile_model(),
        })
    }

    /// Where each display's bar goes, in the bar's own coordinates: the full
    /// bar along the primary display, a compact one along every other.
    fn topbar_placement(&self) -> Placement {
        let origin = self.rect.origin();
        let Some(primary) = self.primary_output() else {
            return Placement::default();
        };
        let mut bars = vec![BarPlace {
            display: primary.id,
            rect: local(primary.rect(), origin),
            full: true,
        }];
        bars.extend(
            super::displays::distinct_outputs(&self.outputs)
                .into_iter()
                .filter(|output| output.id != primary.id)
                .map(|output| BarPlace {
                    display: output.id,
                    rect: local(output.rect(), origin),
                    full: false,
                }),
        );
        let fullscreen = self.chrome.as_ref().map_or_else(Vec::new, |chrome| {
            let state = chrome.borrow();
            bars.iter()
                .filter(|bar| !bar.full && state.fullscreen_covers_display(bar.display))
                .map(|bar| bar.display)
                .collect()
        });
        Placement { bars, fullscreen }
    }

    /// Hand the bar what the chrome knows, when it changed, where its bars
    /// go, and how a window over its display moves.
    fn feed_topbar(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        self.mount_topbar();
        if let Some(feed) = self.topbar_feed(&chrome) {
            self.topbar.link.publish(feed);
        }
        self.topbar.origin = self.rect.origin();
        self.topbar.link.place(self.topbar_placement());
        // The bar leaves with a window maximizing over it and comes back
        // with its restore, moved by what moved the window.
        let shown = !self.maximized_on_top_bar_display();
        if self.topbar.with_window != Some(shown) {
            self.topbar.with_window = Some(shown);
            self.topbar.link.command(Command::MoveWithWindow {
                shown,
                motion: self.layout_theme.motion(),
                source: self.input_source,
            });
        }
        // What opens over the bar from elsewhere closes the bar's own menu:
        // one thing open over the bar at a time.
        if self.topbar.shown.modal {
            let state = chrome.borrow();
            let other = state.overlays.quick_settings
                || state.overlays.collaborators
                || state.overlays.overview
                || state.overlays.ws_settings;
            drop(state);
            if other || self.palette_visible {
                self.topbar.link.command(Command::ClosePopovers);
            }
        }
    }

    /// Run the bar's work: its feed, its wake and a published action, and
    /// carry out what it asked. Whether it changed what it shows.
    pub(super) fn update_topbar(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        self.feed_topbar();
        let Some(app) = self.topbar.app.as_mut() else {
            return false;
        };
        let visible = app.update(doc, path);
        self.answer_topbar();
        visible
    }

    /// Paint the bars and what they opened into the top bar's section, as
    /// one damage-tracked part bounded by what the bar says it covers. The
    /// bounds of the section's drawing, `content` (the desktop's own) with
    /// the bar's.
    pub(super) fn draw_topbar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        content: Option<Rect>,
    ) -> Option<Rect> {
        if self.chrome.is_none() {
            return content;
        }
        self.feed_topbar();
        let Some(mut app) = self.topbar.app.take() else {
            return content;
        };
        if self.topbar.node.is_none() {
            // Mounted since the host was laid out: placed over the desktop
            // until the next layout takes it in.
            super::banners::place(app.as_mut(), self.rect, theme);
        }
        let mut painted = Vec::new();
        ui::scene_parts::part_drawn(
            scene,
            ui::scene_parts::PartKey::new("shell.topbar", ()),
            |scene| {
                // Drawn apart and appended whole: the app's own part spans
                // its whole area, the desktop; this part claims only what
                // the bar covers.
                let mut drawn = Scene::new();
                {
                    let _no_copies = ui::scroll_copy::HostCopies::begin(false);
                    app.draw(&mut drawn, transform, text_renderer, theme, presence);
                }
                self.topbar.refresh_shown();
                painted = self.topbar.painted_bounds(theme);
                if !painted.is_empty() {
                    scene.append(&drawn, None);
                }
                painted
                    .iter()
                    .map(|rect| transform.transform_rect_bbox(*rect))
                    .collect()
            },
        );
        self.topbar.app = Some(app);
        self.answer_topbar();
        painted
            .into_iter()
            .chain(content)
            .reduce(|all, rect| all.union(rect))
    }

    /// Carry out what the bar asked, through the chrome's own operations,
    /// and take what it shows now.
    fn answer_topbar(&mut self) {
        self.topbar.refresh_shown();
        let asks = self.topbar.link.take_asks();
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        if !self.topbar.shown.workspace_name.is_zero_area() {
            // Workspace settings open from the name the bar draws.
            let anchor = self.topbar.shown.workspace_name;
            let mut state = chrome.borrow_mut();
            if state.workspace_settings_anchor != anchor {
                state.workspace_settings_anchor = anchor;
            }
        }
        if asks.is_empty() {
            return;
        }
        for ask in asks {
            match ask {
                Ask::Chrome(intent) => {
                    if !self.topbar_intent(&chrome, intent.clone()) {
                        tracing::debug!(?intent, "the bar's ask was not carried out");
                    }
                }
                Ask::WorkspaceMenu(choice) => self.apply_workspace_menu_choice(choice),
                Ask::Profile(choice) => self.apply_profile_choice(&choice),
                Ask::Notifications => chrome
                    .borrow_mut()
                    .open_quick_settings_level(tilo_quick_settings::QsLevel::Notifications),
                Ask::Media(command) => self.send_topbar_media(command),
                // One thing over the bar at a time: People closes for the
                // bar's menu.
                Ask::Opened => chrome.borrow_mut().overlays.collaborators = false,
            }
        }
        self.topbar.refresh_shown();
        self.bump_chrome();
    }

    /// Hand a click on a tray item to its app, at the item's place on the
    /// desktop (the bar says where in its own area). Whether it was taken.
    fn forward_tray_request(
        &self,
        key: &str,
        request: tilo_desktop::chrome::AppTrayRequest,
    ) -> bool {
        use tilo_desktop::chrome::AppTrayRequest;
        let (dx, dy) = (
            ui::num::i32_of_f64(self.topbar.origin.x),
            ui::num::i32_of_f64(self.topbar.origin.y),
        );
        let at = |x: i32, y: i32| (x.saturating_add(dx), y.saturating_add(dy));
        let request = match request {
            AppTrayRequest::Activate { x, y } => {
                let (x, y) = at(x, y);
                AppTrayRequest::Activate { x, y }
            }
            AppTrayRequest::SecondaryActivate { x, y } => {
                let (x, y) = at(x, y);
                AppTrayRequest::SecondaryActivate { x, y }
            }
            AppTrayRequest::ContextMenu { x, y } => {
                let (x, y) = at(x, y);
                AppTrayRequest::ContextMenu { x, y }
            }
        };
        if let Err(error) = crate::tray::global().request(key, bar_data::tray_request(request)) {
            tracing::debug!(target: "shell::tray", %key, %error, "tray click not delivered");
            return false;
        }
        true
    }

    /// Carry out what a control on the bar asked for. Whether it was.
    fn topbar_intent(
        &self,
        chrome: &crate::shell::chrome_state::Chrome,
        intent: ChromeIntent,
    ) -> bool {
        let mut state = chrome.borrow_mut();
        match intent {
            ChromeIntent::SwitchWorkspace(dir) => return state.switch_workspace(dir),
            ChromeIntent::ToggleOverview => state.overlays.overview = !state.overlays.overview,
            ChromeIntent::ToggleQuickSettings => {
                state.toggle_quick_settings();
                return true;
            }
            ChromeIntent::ShowQuickSettings => {
                state.open_quick_settings();
                return true;
            }
            ChromeIntent::ToggleCollaborators => {
                state.overlays.collaborators = !state.overlays.collaborators;
                if state.overlays.collaborators {
                    state.overlays.room_activity = false;
                }
            }
            // The same invite flow the people panel's button starts.
            ChromeIntent::Invite => {
                state.push_peer_sync_request(super::PeerSyncAction::PromptInvite);
                state.overlays.collaborators = false;
            }
            // The overlay host opens the list at them on its next update.
            ChromeIntent::ShowPerson(id) => {
                state.push_presence_request(crate::shell::chrome_state::PresenceRequest::Show(id));
            }
            ChromeIntent::ShowActivity(focus) => {
                let focus = focus.or_else(|| state.activity_glance.focus.clone());
                state.open_room_activity(focus);
                return true;
            }
            ChromeIntent::HideClock => state.overlays.clock = false,
            // The overlay host opens the strip switcher's card on its next
            // update, as Super+S does, with the focused strip chosen.
            ChromeIntent::ShowStrips => state.request_strip_switcher(),
            ChromeIntent::ClockCommand {
                command,
                activity_id,
            } => {
                drop(state);
                bar_data::publish_clock_command(&command, activity_id.as_deref());
                return true;
            }
            ChromeIntent::AcceptContinuity => {
                return state
                    .accept_continuity_suggestion(WallInstant::now())
                    .is_some();
            }
            ChromeIntent::DismissContinuity => {
                return state
                    .dismiss_continuity_suggestion(WallInstant::now())
                    .is_some();
            }
            ChromeIntent::AppTray { key, request } => {
                drop(state);
                return self.forward_tray_request(&key, request);
            }
            ChromeIntent::RenameWorkspace { name } => {
                if !state.may_rename_active_workspace() {
                    return false;
                }
                state.rename_active_workspace(&name);
                return true;
            }
            // The bar opens its own menus; the device keeps who its person
            // is, so nothing is asked from the avatar, and the bar carries
            // no Tilo mark (its About is in Settings).
            ChromeIntent::ToggleWorkspaceMenu
            | ChromeIntent::ToggleProfile
            | ChromeIntent::ToggleTiloMenu
            | ChromeIntent::OpenAvatarAsk
            | ChromeIntent::CollapseAvatarAsk
            | ChromeIntent::AnswerAvatarAsk
            | ChromeIntent::PutOffAvatarAsk => return false,
        }
        state.bump();
        true
    }

    /// Send a transport command to the player the now-playing card shows.
    fn send_topbar_media(&self, command: MediaCommand) {
        use crate::shell::now_playing::PlayerCommand;
        let players = self.now_playing_source.players();
        let Some(bus) = crate::shell::now_playing::active_index(
            &players,
            self.now_playing_preferred.as_deref(),
        )
        .and_then(|active| players.get(active))
        .map(|player| player.bus_name.clone()) else {
            return;
        };
        let command = match command {
            MediaCommand::PlayPause => PlayerCommand::PlayPause,
            MediaCommand::Next => PlayerCommand::Next,
            MediaCommand::Previous => PlayerCommand::Previous,
        };
        if !self.now_playing_source.send(&bus, command) {
            tracing::debug!(target: "tilo::mpris", "the bar's media command was not dispatched");
        }
    }

    /// Route `command` to the bar, and carry out what it asked.
    pub(super) fn command_topbar(&mut self, command: Command) {
        self.topbar.link.command(command);
        self.answer_topbar();
    }

    /// Close the menu or card the bar opened.
    pub(super) fn close_topbar_popovers(&mut self) {
        if self.topbar.shown.modal {
            self.command_topbar(Command::ClosePopovers);
        }
    }

    /// Hand `event` to the bar. Whether it took it.
    fn deliver_topbar(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let node = self.topbar.node.unwrap_or(ctx.node_id);
        let Some(app) = self.topbar.app.as_mut() else {
            return false;
        };
        let before = std::mem::replace(ctx.handled, false);
        app.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        let taken = *ctx.handled;
        *ctx.handled = before || taken;
        self.answer_topbar();
        taken
    }

    /// While the bar has a menu or card open, it takes every event first: a
    /// press chooses or dismisses without also reaching what is under it
    /// (the control that opened it would open it again), and the keys move
    /// through it. Whether the bar took the event.
    pub(super) fn route_topbar_modal_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.topbar.shown.modal {
            return false;
        }
        let pressed = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        );
        let taken = self.deliver_topbar(event, ctx);
        if pressed && !taken {
            // A press the menu did not take (one outside it before its
            // layout caught up with its opening) still dismisses it.
            self.command_topbar(Command::ClosePopovers);
            *ctx.handled = true;
            return true;
        }
        taken || pressed
    }

    /// The pointer on a bar, and the keys while the bar holds the keyboard,
    /// where the bar stood among the desktop's own content. Whether the bar
    /// took the event.
    pub(super) fn route_topbar_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.topbar.app.is_none() {
            return false;
        }
        let at = match event {
            WindowEvent::CursorMoved { position, .. } => Some(ctx.logical(position)),
            WindowEvent::Touch(touch) => Some(ctx.logical(touch.location)),
            _ => self.last_cursor,
        };
        let over = at.is_some_and(|p| self.topbar.covers(p));
        let take = match event {
            WindowEvent::CursorMoved { .. } => {
                let left = std::mem::replace(&mut self.topbar.hovered, over);
                over || left
            }
            WindowEvent::CursorLeft { .. } => std::mem::take(&mut self.topbar.hovered),
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } => {
                over || self.topbar.shown.captures_pointer
            }
            WindowEvent::Touch(_) => over || self.topbar.shown.captures_pointer,
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::ModifiersChanged(_) => self.topbar.has_keyboard(),
            WindowEvent::Focused(false) => true,
            _ => false,
        };
        let pressed_elsewhere = !over
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            );
        if pressed_elsewhere && self.topbar.shown.keyboard && !self.topbar.shown.captures_pointer {
            // A press elsewhere takes the keyboard from the bar.
            self.command_topbar(Command::ReleaseKeyboard);
        }
        if !take {
            return false;
        }
        let taken = self.deliver_topbar(event, ctx);
        // A move is the bar's only while on it: what is under the move that
        // left it still hears it, as everything under the bar hears the
        // window losing focus.
        taken
            && !matches!(event, WindowEvent::CursorMoved { .. } if !over)
            && !matches!(event, WindowEvent::Focused(_))
    }

    /// The bar's node in the host's layout: its whole area, over the
    /// desktop.
    pub(super) fn build_topbar_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> Option<NodeId> {
        // Mounted before the first layout, so the bar has its place in the
        // tree from the first frame on.
        self.mount_topbar();
        self.topbar.node = None;
        let app = self.topbar.app.as_mut()?;
        let node = app.build_layout(taffy, theme, node_map);
        let style = Style {
            position: Position::Absolute,
            inset: TaffyRect {
                left: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::length(0.0),
                top: LengthPercentageAuto::length(0.0),
                bottom: LengthPercentageAuto::length(0.0),
            },
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let _ = taffy.set_style(node, style);
        self.topbar.node = Some(node);
        Some(node)
    }

    pub(super) fn apply_topbar_layout(&mut self, taffy: &TaffyTree<()>) {
        let origin = self.rect.origin();
        if let (Some(app), Some(node)) = (self.topbar.app.as_mut(), self.topbar.node) {
            app.apply_layout(taffy, node, origin);
        }
    }

    /// Whether the bar's node in the layout no longer matches the bar.
    pub(super) fn take_topbar_structure_dirty(&mut self) -> bool {
        let stale = self.topbar.app.is_some() != self.topbar.node.is_some();
        stale
            | self
                .topbar
                .app
                .as_mut()
                .is_some_and(|app| app.take_structure_dirty())
    }

    /// Whether the bar is moving or fading, or its app moves. A change the
    /// bar has yet to draw is not motion: [`Self::update_topbar`] answers it
    /// with one frame.
    pub(super) fn topbar_needs_frame(&self) -> bool {
        self.topbar.shown.animating
            || self
                .topbar
                .app
                .as_ref()
                .is_some_and(|app| app.needs_animation_frame())
    }

    /// When the bar next needs to run with nothing waking it.
    pub(super) fn topbar_deadline(&self) -> Option<Instant> {
        self.topbar
            .app
            .as_ref()
            .and_then(|app| app.next_wake_deadline())
            .into_iter()
            .chain(self.topbar.shown.deadline)
            .min()
    }

    /// What the bar showed at its last frame or input.
    pub(super) fn topbar_shown(&self) -> &Shown {
        &self.topbar.shown
    }

    /// The full bar as last drawn; zero before the bar has drawn.
    pub(super) fn full_top_bar_rect(&self) -> Rect {
        self.topbar
            .shown
            .bars
            .iter()
            .find(|(_, _, full)| *full)
            .map_or(Rect::ZERO, |(_, rect, _)| *rect)
    }

    /// Whether the pointer at `p` is on the bar's open menu or floating
    /// clock: those float over the windows like the bar shown over them.
    pub(super) fn topbar_floats_at(&self, p: Point) -> bool {
        self.topbar.shown.popover.contains(p) || self.topbar.shown.clock.contains(p)
    }

    /// Whether something the bar opened, or its own keyboard focus, holds it
    /// shown.
    pub(super) fn topbar_holds_shown(&self) -> bool {
        self.topbar.shown.holds_shown
            || (self.topbar.reached_by_keyboard && self.topbar.natively_focused())
    }

    /// The shell moved keyboard focus to `widget`, by the person's Tab with
    /// `by_keyboard`: whether that is the bar, reached by reading it.
    pub fn native_focus_moved(&mut self, widget: Option<*const ()>, by_keyboard: bool) {
        let onto_bar = widget.is_some_and(|widget| {
            self.topbar
                .app
                .as_deref()
                .is_some_and(|app| std::ptr::addr_eq(std::ptr::from_ref(app), widget))
        });
        self.topbar.reached_by_keyboard = onto_bar && by_keyboard;
    }

    /// The bar's surface in its app, for a test or a capture driving it.
    fn topbar_surface_mut(&mut self) -> Option<&mut TopBarSurface> {
        let app = self
            .topbar
            .app
            .as_mut()?
            .as_any_mut()?
            .downcast_mut::<ui::app::EmbeddedApp>()?;
        tilo_app::framework::testing::surface_in_root::<TopBarApp, TopBarSurface>(
            app.component_root_mut()?,
        )
    }

    /// Where each top bar is drawn: the full bar (flagged `true`) and every
    /// compact one, by display.
    pub fn top_bar_rects(&self) -> Vec<(u32, Rect, bool)> {
        self.topbar.shown.bars.clone()
    }

    /// Where the full bar draws the workspace's name, its menu's opener;
    /// zero before the bar has drawn.
    pub fn top_bar_workspace_name_rect(&self) -> Rect {
        self.topbar.shown.workspace_name
    }

    /// Where the full bar draws its status cluster, Quick Settings' opener.
    pub fn top_bar_tray_rect(&self) -> Rect {
        self.topbar.shown.tray
    }

    /// The workspace menu's rows as shown, for tests and probes.
    pub fn workspace_menu_labels(&self) -> Option<Vec<Option<String>>> {
        self.topbar.shown.workspace_menu.clone()
    }

    /// Choose the workspace menu's row labelled `label`, as a click would.
    pub fn choose_in_workspace_menu(&mut self, label: &str) -> bool {
        let chosen = self
            .topbar_surface_mut()
            .is_some_and(|surface| surface.choose_in_workspace_menu(label));
        self.answer_topbar();
        chosen
    }

    /// Open the workspace menu, or close it, as a press on the workspace's
    /// name does.
    pub fn toggle_workspace_menu(&mut self) {
        if let Some(surface) = self.topbar_surface_mut() {
            surface.toggle_workspace_menu();
        }
        self.answer_topbar();
    }

    /// Open the profile card, or close it, as a press on the person's own
    /// avatar does.
    pub fn toggle_profile_card(&mut self) {
        if let Some(surface) = self.topbar_surface_mut() {
            surface.toggle_profile();
        }
        self.answer_topbar();
    }

    pub fn profile_card_open(&self) -> bool {
        self.topbar.shown.profile
    }

    /// Whether keyboard focus is in the bar, or a menu it opened takes the
    /// keys: the window behind it must not get them.
    pub fn topbar_owns_keyboard(&self) -> bool {
        self.topbar.shown.modal || self.topbar.shown.holds_shown
    }
}

#[cfg(test)]
#[path = "topbar_host_tests.rs"]
mod tests;
