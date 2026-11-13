//! The Tube window: the [`Tube`] the view shares with its effect handler (the catalogue,
//! navigation, playback and the network worker) and the [`TubeApp`] widget that lays it out in its
//! shell. Tube renders a single in-memory [`TubeModel`].

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use ui::widget::{AppModel, Element, ModelPath};

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::framework::{Actions, Dispatcher, Effected, Live, PersonRecord, Publishes, ViewCx};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::ToastKind;
use ui::widget::{FullscreenProvider, NavItem, NavList, NavSection, NavShell, VideoPlayer, Widget};

use crate::action::{self, Action, ActionBus};
use crate::actions::TubeEffect;
use crate::api::{Request, Worker};
use crate::history::WatchHistory;
use crate::i18n::t;
use crate::model::{Route, SearchType, TubeModel};
use crate::resource::Library;
use crate::screens;
use crate::widgets::SharedPlayer;
use ui::i18n::ArgValue;
use ui::tilo_resource::{Edit, Invoke};
use ui::widget::DrawChild;

mod playback;
mod requests;
mod staging;
mod window_bar;

pub(crate) const QUALITIES: [&str; 4] = ["1080p", "720p", "480p", "auto"];

/// The playback speeds the player's menu offers, slowest first.
pub(crate) const PLAYBACK_SPEEDS: [f32; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0];

type ScrollCell = Rc<Cell<(f32, f32)>>;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent render/interaction cache (rebuild-dirty flags, \
              --shots staging, sidebar bookkeeping), not a state machine"
)]
pub(crate) struct Tube {
    model: TubeModel,
    dispatcher: Option<Dispatcher<Library>>,
    actions: Option<Actions<Library>>,
    history: Option<PersonRecord<WatchHistory>>,
    preferences: Option<ui::framework::Setting<crate::model::Settings>>,
    local_history: Option<ui::tilo_resource::Replica<WatchHistory>>,
    bus: ActionBus,
    bar: ui::framework::BarCommands<Action>,
    worker: Option<Worker>,
    wake: ui::WakeHandle,
    #[cfg(feature = "grayjay-sync")]
    device_sync: crate::sync::DeviceSync,
    #[cfg(feature = "grayjay-sync")]
    device_sync_seeded: bool,

    structure_dirty: bool,
    search_query: ui::LocalState<String>,
    palette_query: String,
    suggested: crate::suggest::Suggested,

    online: bool,
    kicked_off: bool,

    playing_id: Option<String>,
    player: SharedPlayer,
    player_video: Option<String>,
    resume_at_ms: Option<u64>,
    progress_written: Option<(String, u64)>,

    images: Option<ui::ImageStore>,
    waiting: screens::Waiting,

    show_skip_end: Option<f64>,

    mpris_started: bool,

    nav: ui::NavStack<Route>,
    scroll_cells: RefCell<HashMap<String, ScrollCell>>,
    requested_channels: HashSet<String>,
    shell_asks: Vec<ShellAsk>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ShellAsk {
    ToggleSidebar,
    DismissOverlayRail,
    #[cfg(feature = "samples")]
    OpenSidebar,
    #[cfg(feature = "samples")]
    SidebarWidth(f64),
}

pub(crate) type Shared = Rc<RefCell<Tube>>;

pub struct TubeApp {
    tube: Shared,
    live: Option<Live<Library>>,
    presented: Option<(u64, u64)>,
    can_go_back: Rc<Cell<bool>>,
    shell: NavShell,
    shell_built: bool,
    toasted: u64,
    shell_stale: bool,
    shell_key: Option<u64>,
    child_node: Option<NodeId>,
    rect: Rect,
    back_gesture: ui::EdgeSwipeBack,
    last_pointer: Point,
}

impl Default for TubeApp {
    fn default() -> Self {
        Self::new()
    }
}

impl TubeApp {
    pub fn new() -> Self {
        Self::over(Tube::new())
    }

    pub(crate) fn over(tube: Tube) -> Self {
        let can_go_back = Rc::new(Cell::new(false));
        let shell = Self::new_shell(&tube.bus, &can_go_back);
        Self {
            tube: Rc::new(RefCell::new(tube)),
            live: None,
            presented: None,
            can_go_back,
            shell,
            shell_built: false,
            toasted: 0,
            shell_stale: true,
            shell_key: None,
            child_node: None,
            rect: Rect::ZERO,
            back_gesture: ui::EdgeSwipeBack::default_back(),
            last_pointer: Point::ZERO,
        }
    }

    pub(crate) fn tube(&self) -> std::cell::RefMut<'_, Tube> {
        self.tube.borrow_mut()
    }

    pub(crate) fn shared(&self) -> Shared {
        Rc::clone(&self.tube)
    }

    #[cfg(feature = "samples")]
    pub fn sample() -> Self {
        Self::over(Tube::sample())
    }

    #[must_use]
    pub fn online(self) -> Self {
        self.tube().go_online();
        self
    }

    fn follow_document(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        let Some(shown) = live.changed_since(&mut self.presented) else {
            return;
        };
        self.tube.borrow_mut().present_library(&shown);
    }

    fn answer_shell_asks(&mut self) {
        let asks = std::mem::take(&mut self.tube().shell_asks);
        for ask in asks {
            match ask {
                ShellAsk::ToggleSidebar => self.shell.toggle_sidebar(),
                ShellAsk::DismissOverlayRail => {
                    if !self.shell.geometry().side_by_side && self.shell.is_sidebar_open() {
                        self.shell.toggle_sidebar();
                    }
                }
                #[cfg(feature = "samples")]
                ShellAsk::OpenSidebar => {
                    self.shell.open_sidebar();
                    self.shell.snap();
                }
                #[cfg(feature = "samples")]
                ShellAsk::SidebarWidth(width) => self.shell.stage_sidebar_width(width),
            }
        }
    }

    fn new_shell(bus: &ActionBus, can_go_back: &Rc<std::cell::Cell<bool>>) -> NavShell {
        let can = Rc::clone(can_go_back);
        let bus = Rc::clone(bus);
        NavShell::new()
            .content_kind(ui::widget::ContentKind::Grid)
            .with_id("tube_shell")
            .with_window_toolbar()
            .on_back(move || can.get(), move || action::push(&bus, Action::Back))
    }
}

impl Tube {
    fn new() -> Self {
        let bus = action::new_bus();
        Self {
            model: TubeModel::default(),
            dispatcher: None,
            actions: None,
            history: None,
            preferences: None,
            local_history: None,
            bus,
            bar: ui::framework::BarCommands::new(),
            worker: None,
            wake: ui::WakeHandle::detached(),
            #[cfg(feature = "grayjay-sync")]
            device_sync: crate::sync::DeviceSync::default(),
            #[cfg(feature = "grayjay-sync")]
            device_sync_seeded: false,
            structure_dirty: false,
            search_query: ui::LocalState::new(String::new()),
            palette_query: String::new(),
            suggested: crate::suggest::Suggested::default(),
            online: false,
            kicked_off: false,
            playing_id: None,
            player: Rc::new(RefCell::new(VideoPlayer::new())),
            player_video: None,
            resume_at_ms: None,
            progress_written: None,
            images: None,
            waiting: screens::Waiting::default(),
            show_skip_end: None,
            mpris_started: false,
            nav: ui::NavStack::new(Route::Home),
            scroll_cells: RefCell::new(HashMap::new()),
            requested_channels: HashSet::new(),
            shell_asks: Vec::new(),
        }
    }

    fn apply_queued(&mut self) -> bool {
        let mut changed = false;
        for action in action::drain(&self.bus) {
            changed |= self.apply(action);
        }
        changed
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one short arm per effect Tube answers; splitting them would only scatter the \
                  vocabulary"
    )]
    pub(crate) fn perform(&mut self, effect: TubeEffect) -> Result<(), String> {
        self.structure_dirty = true;
        if let Some(settings) = effect.preferences(&self.model.settings) {
            let settings = settings?;
            if !self.change_preferences(|current| *current = settings) {
                return Err("Your playback preferences could not be saved".into());
            }
            return Ok(());
        }
        match effect {
            TubeEffect::ShowScreen { screen } => {
                let route = TubeEffect::screen(&screen)
                    .ok_or_else(|| format!("there is no screen `{screen}`"))?;
                self.navigate_to(route);
            }
            TubeEffect::FilterSearch { kind } => {
                let kind = SearchType::ALL
                    .into_iter()
                    .find(|candidate| candidate.id() == kind)
                    .ok_or_else(|| format!("there is no result type `{kind}`"))?;
                self.model.search_type_filter.click(kind);
            }
            TubeEffect::ToggleSidebar => self.shell_asks.push(ShellAsk::ToggleSidebar),
            TubeEffect::OpenVideo { video } => {
                self.adopt_suggested(&video);
                if self.model.video(&video).is_none() {
                    return Err(t("tube-notice-video-gone"));
                }
                self.open_video(video);
                self.shell_asks.push(ShellAsk::DismissOverlayRail);
            }
            TubeEffect::OpenCreator { creator } => {
                if self.model.channel(&creator).is_none() {
                    return Err(t("tube-notice-creator-gone"));
                }
                self.navigate_to(Route::Channel(creator.clone()));
                self.shell_asks.push(ShellAsk::DismissOverlayRail);
                self.request_live(Request::Channel(creator));
            }
            TubeEffect::OpenPlaylist { playlist } => {
                if self.model.playlist(&playlist).is_none() {
                    return Err(t("tube-notice-playlist-gone"));
                }
                self.navigate_to(Route::Playlist(playlist.clone()));
                self.shell_asks.push(ShellAsk::DismissOverlayRail);
                self.request_live(Request::PlaylistPage(playlist));
            }
            TubeEffect::Search { query } => {
                self.search_query.set(query.into_string());
                self.apply(Action::SubmitSearch);
                self.shell_asks.push(ShellAsk::DismissOverlayRail);
            }
            TubeEffect::TogglePlayback => {
                self.player.borrow_mut().toggle_play();
                self.model.playing = self.player.borrow().is_playing();
            }
            TubeEffect::Seek { seconds } => {
                let duration = self
                    .current_duration()
                    .ok_or("Open a video to seek in it")?;
                if !seconds.is_finite() || seconds < 0.0 {
                    return Err("Say a moment from the start of the video, in seconds".into());
                }
                self.seek_to(seconds.min(duration));
            }
            TubeEffect::SkipSegment => {
                let end = self
                    .current_show_skip_end()
                    .ok_or("No segment to skip is playing")?;
                self.skip_to(end);
            }
            TubeEffect::Share { video } => {
                tracing::debug!(target: "tube::share", %video, at = ?ui::time::now(), "share effect entered");
                let url = format!("https://youtu.be/{video}");
                ui::clipboard_copy(&url);
                self.model.push_notice(format!("Link copied: {url}"));
                tracing::debug!(target: "tube::share", %video, at = ?ui::time::now(), "share model notice created");
            }
            TubeEffect::Download { video } => {
                return Err(format!(
                    "Downloads are unavailable for {video}: no storage capability"
                ));
            }
            TubeEffect::MarkWatched { video } => {
                let at_ms = ui::time::unix_now_ms();
                self.edit_history(&WatchHistory::RECORD.with((video, at_ms)));
            }
            TubeEffect::ForgetWatched { video } => {
                self.edit_history(&WatchHistory::FORGET.with(video));
            }
            TubeEffect::ClearHistory => {
                if self.edit_history(&WatchHistory::CLEAR.invoke()) {
                    self.model.push_notice(t("tube-notice-history-cleared"));
                }
            }
            TubeEffect::PairDevice { invite } => {
                #[cfg(feature = "grayjay-sync")]
                if !self.device_sync.pair(invite) {
                    return Err(t("tube-notice-pairing-busy"));
                }
                #[cfg(not(feature = "grayjay-sync"))]
                let _ = invite;
            }
            TubeEffect::SyncDevices => {
                #[cfg(feature = "grayjay-sync")]
                if !self
                    .device_sync
                    .sync_now(crate::sync::lifecycle_state(&self.model))
                {
                    return Err(t("tube-notice-sync-busy"));
                }
            }
            TubeEffect::ApproveDevice { device } => {
                #[cfg(feature = "grayjay-sync")]
                if !self.device_sync.approve(&device) {
                    return Err("Could not queue device approval".into());
                }
                #[cfg(not(feature = "grayjay-sync"))]
                let _ = device;
            }
            TubeEffect::RevokeDevice { device } => {
                #[cfg(feature = "grayjay-sync")]
                if !self.device_sync.revoke(&device) {
                    return Err("Could not queue device revocation".into());
                }
                #[cfg(not(feature = "grayjay-sync"))]
                let _ = device;
            }
            TubeEffect::SetPreference { .. }
            | TubeEffect::SetPreferredQuality { .. }
            | TubeEffect::SetSegmentHandling { .. } => {}
        }
        Ok(())
    }

    fn edit(&self, edit: Edit<Library>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(edit);
        }
    }

    fn invoke(&mut self, effect: &TubeEffect) {
        match &self.dispatcher {
            Some(dispatcher) => dispatcher.effect(effect),
            None => {
                if let Err(error) = self.perform(effect.clone()) {
                    tracing::debug!(%error, "effect refused");
                }
            }
        }
    }

    fn edit_history(&mut self, edit: &Invoke<WatchHistory>) -> bool {
        let edited = if let Some(record) = &self.history {
            record
                .invoke(edit)
                .map(|()| record.with(WatchHistory::videos))
                .map_err(|error| error.to_string())
        } else {
            let shown = &self.model.history;
            let positions = &self.model.history_positions;
            let replica = self.local_history.get_or_insert_with(|| {
                let mut replica = ui::tilo_resource::Replica::<WatchHistory>::new();
                let count = shown.len();
                let watched: Vec<(String, u64)> = shown
                    .iter()
                    .enumerate()
                    .map(|(at, video)| (video.clone(), u64::try_from(count - at).unwrap_or(0)))
                    .collect();
                let _ = replica.apply(WatchHistory::IMPORT.with(watched));
                for (video, position) in positions {
                    let _ =
                        replica.apply(WatchHistory::PROGRESS.with((video.clone(), *position, 0)));
                }
                replica
            });
            let applied = replica
                .apply(edit.clone())
                .map(|_| ())
                .map_err(|error| error.to_string());
            applied.and_then(|()| {
                replica
                    .state()
                    .map(|state| state.videos())
                    .map_err(|error| error.to_string())
            })
        };
        match edited {
            Ok(videos) => {
                self.model.history_notice = None;
                self.model.history = videos;
                self.model.history_positions = self.history_positions();
                self.structure_dirty = true;
                true
            }
            Err(error) => {
                tracing::warn!(%error, "could not store Tube watch history");
                self.model.history_notice = Some(
                    "Watch history could not be saved. Your shared library was not changed.".into(),
                );
                false
            }
        }
    }

    fn follow_history(&mut self) -> bool {
        let Some(history) = &self.history else {
            return false;
        };
        let stored = history.with(WatchHistory::videos);
        let positions = history.with(WatchHistory::positions);
        if stored == self.model.history && positions == self.model.history_positions {
            return false;
        }
        self.model.history = stored;
        self.model.history_positions = positions;
        true
    }

    fn history_positions(&self) -> std::collections::BTreeMap<String, u64> {
        match (&self.history, &self.local_history) {
            (Some(record), _) => record.with(WatchHistory::positions),
            (None, Some(replica)) => replica
                .state()
                .map(|state| state.positions())
                .unwrap_or_default(),
            (None, None) => std::collections::BTreeMap::new(),
        }
    }

    #[cfg(feature = "grayjay-sync")]
    fn import_device_sync_state(&mut self, remote: crate::sync::SyncState) -> bool {
        const MAX_SYNC_ENTITIES: usize = 100;
        const MAX_SYNC_ID_BYTES: usize = 256;

        let mut edits = Vec::new();
        for subscription in remote.subscriptions.into_iter().take(MAX_SYNC_ENTITIES) {
            let Some(subscription) = crate::sync::normalized_subscription_url(&subscription) else {
                continue;
            };
            if subscription.len() > MAX_SYNC_ID_BYTES {
                continue;
            }
            let already_subscribed = self
                .model
                .subscriptions
                .iter()
                .any(|id| id == &subscription)
                || self.model.channels.iter().any(|channel| {
                    (subscription == channel.handle.as_str()
                        || subscription
                            .strip_prefix("https://youtube.com/")
                            .is_some_and(|handle| handle == channel.handle.trim_start_matches('/')))
                        && self.model.is_subscribed(&channel.id)
                });
            if !already_subscribed {
                edits.push(Library::subscribe(&subscription, true));
            }
        }

        let preferences_changed = remote.settings.is_some_and(|remote| {
            let mut settings = self.model.settings.clone();
            for flag in crate::action::SettingFlag::ALL {
                flag.set(&mut settings, flag.get(&remote));
            }
            if QUALITIES.contains(&remote.preferred_quality.as_str()) {
                settings.preferred_quality = remote.preferred_quality;
            }
            for category in crate::model::Category::ALL {
                if let Some(mode) = remote.categories.get(category.api_key()) {
                    settings
                        .categories
                        .insert(category.api_key().to_owned(), *mode);
                }
            }
            settings != self.model.settings
                && self.change_preferences(|current| *current = settings)
        });

        let watched_at_ms = ui::time::unix_now_ms();
        let pulled: Vec<(String, u64)> = remote
            .watch_history
            .into_iter()
            .take(MAX_SYNC_ENTITIES)
            .enumerate()
            .filter_map(|(offset, video)| {
                let video = video.trim();
                (!video.is_empty()
                    && video.len() <= MAX_SYNC_ID_BYTES
                    && !video.chars().any(char::is_control)
                    && !self.model.history.iter().any(|watched| watched == video))
                .then(|| {
                    let offset = u64::try_from(offset).unwrap_or(u64::MAX);
                    (video.to_owned(), watched_at_ms.saturating_sub(offset))
                })
            })
            .collect();
        let history_changed =
            !pulled.is_empty() && self.edit_history(&WatchHistory::IMPORT.with(pulled));

        let library_changed = !edits.is_empty();
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.group(|| {
                for edit in edits {
                    dispatcher.submit(edit);
                }
            });
        }
        library_changed || history_changed || preferences_changed
    }

    #[cfg(feature = "samples")]
    fn sample() -> Self {
        Self {
            model: crate::sample::model(),
            ..Self::new()
        }
    }

    fn go_online(&mut self) {
        self.online = true;
        self.model.feed_loading = true;
        self.images = Some(crate::images::image_store());
    }

    pub(crate) fn present_library(&mut self, state: &Library) {
        self.model.liked = state.likes.iter().cloned().collect();
        self.model.queue = state.queue.iter().cloned().collect();
        self.model.subscriptions = state.subscriptions.iter().cloned().collect();
        self.model.comment_likes = state.comment_likes.iter().cloned().collect();
        self.structure_dirty = true;
    }

    #[cfg(feature = "grayjay-sync")]
    pub(crate) fn start_device_sync(
        &mut self,
        secrets: &ui::secrets::SecretStore,
        paired: ui::storage::Settings<crate::sync::PairedDevices>,
    ) {
        self.device_sync = crate::sync::DeviceSync::production("Tilo Tube", secrets, paired);
    }

    pub(crate) fn follow_preferences(
        &mut self,
        setting: ui::framework::Setting<crate::model::Settings>,
    ) {
        self.model.settings = setting.get();
        self.preferences = Some(setting);
        self.structure_dirty = true;
    }

    fn refresh_preferences(&mut self) -> bool {
        let Some(setting) = &self.preferences else {
            return false;
        };
        let stored = setting.get();
        if stored == self.model.settings {
            return false;
        }
        self.model.settings = stored;
        true
    }

    fn change_preferences(&mut self, change: impl FnOnce(&mut crate::model::Settings)) -> bool {
        let mut settings = self.model.settings.clone();
        change(&mut settings);
        if let Some(setting) = &self.preferences
            && let Err(error) = setting.set(settings.clone())
        {
            tracing::warn!(%error, "could not keep the Tube preferences");
            self.model
                .push_notice("Your playback preferences could not be saved.");
            return false;
        }
        self.model.settings = settings;
        self.structure_dirty = true;
        true
    }

    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: Dispatcher<Library>,
        actions: Actions<Library>,
        history: Option<PersonRecord<WatchHistory>>,
    ) {
        self.dispatcher = Some(dispatcher);
        self.actions = Some(actions);
        self.history = history;
        self.follow_history();
    }

    #[cfg(feature = "grayjay-sync")]
    pub(crate) fn device_sync_view(&self) -> &crate::sync::SyncView {
        self.device_sync.view()
    }

    #[cfg(not(feature = "grayjay-sync"))]
    #[allow(
        clippy::unused_self,
        reason = "the same accessor as the build with device sync"
    )]
    pub(crate) fn device_sync_view(&self) -> &crate::sync::SyncView {
        static UNAVAILABLE: std::sync::LazyLock<crate::sync::SyncView> =
            std::sync::LazyLock::new(crate::sync::SyncView::default);
        &UNAVAILABLE
    }

    fn view_key(route: &Route) -> String {
        match route {
            Route::Home => "home".to_string(),
            Route::Trending => "trending".to_string(),
            Route::Subscriptions => "subs".to_string(),
            Route::Queue => "queue".to_string(),
            Route::History => "history".to_string(),
            Route::Settings => "settings".to_string(),
            Route::Search(query) => format!("search:{query}"),
            Route::Watch(id) => format!("watch:{id}"),
            Route::Channel(id) => format!("channel:{id}"),
            Route::Playlist(id) => format!("playlist:{id}"),
        }
    }

    fn scroll_cell_for(&self, route: &Route) -> ScrollCell {
        let key = Self::view_key(route);
        self.scroll_cells
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| Rc::new(Cell::new((0.0, 0.0))))
            .clone()
    }

    fn navigate_to(&mut self, route: Route) {
        if self.nav.current() != &route {
            self.nav.push(route.clone());
        }
        self.model.route = route;
    }

    fn go_back(&mut self) -> bool {
        if let Some(route) = self.nav.back().cloned() {
            self.model.route = route;
            true
        } else {
            false
        }
    }

    fn images_pending(&self) -> bool {
        self.images.as_ref().is_some_and(|s| s.pending() > 0)
    }

    fn apply(&mut self, action: Action) -> bool {
        match action {
            Action::Navigate(route) => {
                if let Route::Search(q) = &route {
                    q.clone_into(&mut self.model.search_query);
                    self.search_query.set(q.clone());
                }
                self.navigate_to(route);
                self.shell_asks.push(ShellAsk::DismissOverlayRail);
            }
            Action::OpenVideo(id) => {
                self.open_video(id);
                self.shell_asks.push(ShellAsk::DismissOverlayRail);
            }
            Action::OpenChannel(id) => {
                self.navigate_to(Route::Channel(id.clone()));
                self.shell_asks.push(ShellAsk::DismissOverlayRail);
                self.request_live(Request::Channel(id));
            }
            Action::Back => {
                self.go_back();
            }
            Action::Seek(frac) => {
                if let Some(d) = self.current_duration() {
                    self.seek_to((frac * d).clamp(0.0, d));
                }
            }
            Action::TogglePlay => {
                let _ = self.perform(TubeEffect::TogglePlayback);
            }
            Action::RetryPlayback => {
                self.model.unplayable = None;
                self.playing_id = None;
                self.ensure_playback();
            }
            Action::SubmitSearch => {
                let q = self.search_query.get();
                q.clone_into(&mut self.model.search_query);
                self.model.search_error = None;
                self.navigate_to(Route::Search(q.clone()));
                if !q.is_empty() {
                    if self.online {
                        self.model.search_loading = true;
                    }
                    self.request(Request::Search { query: q });
                }
            }
            Action::Share(id) => {
                tracing::debug!(target: "tube::share", video = %id, route = ?self.model.route, at = ?ui::time::now(), "share action dispatched");
                if matches!(&self.model.route, Route::Watch(current) if current == &id) {
                    let _ = self.perform(TubeEffect::Share { video: id });
                }
            }
            Action::Edit(edit) => {
                self.edit(edit);
                return false;
            }
            Action::Effect(effect) => {
                self.invoke(&effect);
                return false;
            }
            Action::PairDevice => {
                #[cfg(feature = "grayjay-sync")]
                let invite = self.device_sync.take_pairing_input();
                #[cfg(not(feature = "grayjay-sync"))]
                let invite = String::new();
                if invite.is_empty() {
                    self.model.push_notice(
                        "Paste a pairing invite first, or unlock the desktop secret broker",
                    );
                } else {
                    self.invoke(&TubeEffect::PairDevice { invite });
                }
            }
        }
        true
    }
}

impl TubeApp {
    fn build_screen(&self, theme: &Theme) -> Box<dyn Widget> {
        let tube = self.tube.borrow();
        tube.waiting.begin_build();
        let screen = self.build_route(&tube, theme);
        tube.waiting.end_build();
        screen
    }

    fn build_route(&self, tube: &Tube, theme: &Theme) -> Box<dyn Widget> {
        let ui = screens::Ui {
            model: &tube.model,
            theme,
            bus: &tube.bus,
            actions: tube.actions.as_ref(),
            imgs: crate::images::Imgs(tube.images.as_ref()),
            content_width: self.content_width(theme),
            waiting: &tube.waiting,
        };
        let content = match &tube.model.route {
            Route::Home => screens::home(ui),
            Route::Search(query) => screens::search_results(ui, query),
            Route::Trending => screens::trending(ui),
            Route::Subscriptions => screens::subscriptions(ui),
            Route::Queue => screens::queue(ui),
            Route::History => screens::history(ui),
            Route::Settings => {
                #[cfg(feature = "grayjay-sync")]
                {
                    screens::settings_with_sync(
                        ui,
                        tube.device_sync.view(),
                        tube.device_sync.pairing_input.clone(),
                    )
                }
                #[cfg(not(feature = "grayjay-sync"))]
                {
                    screens::settings(ui)
                }
            }
            Route::Watch(id) => screens::watch(
                ui,
                id,
                (self.content_width(theme), self.content_height(theme)),
                tube.player.clone(),
            ),
            Route::Channel(id) => screens::channel(ui, id),
            Route::Playlist(id) => screens::playlist(ui, id),
        };
        Box::new(content.with_persisted_offset(tube.scroll_cell_for(&tube.model.route)))
    }

    fn build_rail(tube: &Tube) -> NavList {
        use ui::icons::Icon;
        let item = |route: &Route, label: &str, icon: Icon| {
            NavItem::new(route.nav_section(), label).icon(icon)
        };
        let mut list = NavList::new(vec![NavSection::untitled(
            "destinations",
            vec![
                item(&Route::Home, &t("tube-nav-home"), Icon::Home),
                item(&Route::Trending, &t("tube-nav-trending"), Icon::Flame),
                item(
                    &Route::Subscriptions,
                    &t("tube-nav-subscriptions"),
                    Icon::GalleryVerticalEnd,
                ),
                item(&Route::Queue, &t("tube-nav-queue"), Icon::ListVideo),
                item(&Route::History, &t("tube-nav-history"), Icon::History),
                item(&Route::Settings, &t("tube-nav-settings"), Icon::Settings),
            ],
        )])
        .with_accessible_name(t("tube-nav-name"))
        .on_view("tube_rail", {
            let bus = tube.bus.clone();
            move |id| action::push(&bus, Action::Effect(TubeEffect::ShowScreen { screen: id }))
        });
        list.set_selected(Some(tube.model.route.nav_section()));
        list
    }

    fn rebuild_shell(&mut self, theme: &Theme) {
        let screen = self.build_screen(theme);
        let tube = self.tube.borrow();
        let can_go_back = tube.nav.can_go_back();
        self.can_go_back.set(can_go_back);
        self.shell.set_can_go_back(can_go_back);
        if let Some(rail) = self.shell.sidebar_mut::<NavList>() {
            rail.set_selected(Some(tube.model.route.nav_section()));
        } else {
            let rail = Self::build_rail(&tube);
            self.shell.set_sidebar(Some(Box::new(rail)));
        }
        drop(tube);
        self.shell.set_content(screen);
        self.shell_built = true;
    }

    fn content_width(&self, theme: &Theme) -> f64 {
        let column = self.shell.geometry().content_column.width();
        if column > 0.0 {
            return column;
        }
        let rail = if self.shell.is_sidebar_open() || !self.shell_built {
            f64::from(theme.tokens.metrics.rail_width)
        } else {
            0.0
        };
        (self.rect.width() - rail).max(0.0)
    }

    fn content_height(&self, theme: &Theme) -> f64 {
        (self.rect.height() - f64::from(theme.tokens.metrics.header_height)).max(1.0)
    }

    fn show_new_notices(&mut self) {
        use std::hash::{Hash, Hasher};
        let tube = self.tube.borrow();
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        tube.model.notices.first().hash(&mut identity);
        let current = identity.finish();
        if current != self.toasted {
            if let Some(notice) = tube.model.notices.first() {
                self.shell.toast(notice.clone(), ToastKind::Info);
                tracing::debug!(target: "tube::share", at = ?ui::time::now(), "notice toast created");
            }
            self.toasted = current;
        }
    }
}

impl Tube {
    fn settle_input(&mut self) -> bool {
        let mut changed = self.apply_queued();
        self.ensure_playback();
        changed |= self.pump();
        changed
    }

    fn sync_before_shell(&mut self) {
        if self.follow_history() | self.refresh_preferences() {
            self.structure_dirty = true;
        }
        #[cfg(feature = "grayjay-sync")]
        {
            if !self.device_sync_seeded {
                self.device_sync_seeded = true;
                let _ = self
                    .device_sync
                    .sync_now(crate::sync::lifecycle_state(&self.model));
            }
            let (view_changed, pulled) = self.device_sync.pump();
            if view_changed {
                self.structure_dirty = true;
            }
            for remote in pulled {
                if self.import_device_sync_state(remote) {
                    self.structure_dirty = true;
                }
            }
        }
        if self.online && !self.kicked_off {
            self.kicked_off = true;
            self.request_live(Request::Feed);
        }
        self.ensure_playback();
    }

    fn sync_after_shell(&mut self) {
        if self.apply_queued() {
            self.structure_dirty = true;
        }
        self.player.borrow_mut().pump_media_controls();
        let setting_events = self.player.borrow_mut().take_setting_events();
        for ev in setting_events {
            self.apply_player_setting_event(ev);
        }
        if self.tick_playback() {
            self.structure_dirty = true;
        }
    }

    fn resolving(&self) -> bool {
        self.online
            && matches!(&self.model.route, Route::Watch(id)
                if self.playing_id.as_deref() == Some(id.as_str())
                    && self.player_video.as_deref() != Some(id.as_str()))
    }
}

pub(crate) fn window(cx: &ViewCx<Library>, mut window: TubeApp) -> Element {
    window.live = Some(cx.live().clone());
    let tube = window.shared();
    {
        let mut tube = tube.borrow_mut();
        let history = cx
            .mode()
            .is_live()
            .then(|| cx.person::<WatchHistory>())
            .flatten();
        tube.follow_preferences(cx.settings::<crate::model::Settings>());
        #[cfg(feature = "grayjay-sync")]
        if cx.mode().is_live() {
            tube.start_device_sync(
                &cx.storage().secrets(),
                cx.storage().settings::<crate::sync::PairedDevices>(),
            );
        }
        tube.wake_with(cx.waker());
        tube.attach_framework(cx.dispatcher(), cx.actions(), history);
    }
    {
        let (answer, status) = (Rc::clone(&tube), Rc::clone(&tube));
        cx.effects(move |effect: TubeEffect, _| {
            answer
                .borrow_mut()
                .perform(effect)
                .map(|()| Effected::done())
        })
        .status(move || {
            let tube = status.borrow();
            crate::rows::effect_statuses(&tube.model, tube.device_sync_view())
        });
    }
    #[cfg(feature = "samples")]
    {
        let tube = Rc::clone(&tube);
        cx.on_stage(move |scenario| {
            if let Some(scenario) = scenario {
                tube.borrow_mut().stage_scenario(scenario);
            }
        });
    }
    let (rows, row, entries, query) = (
        Rc::clone(&tube),
        Rc::clone(&tube),
        Rc::clone(&tube),
        Rc::clone(&tube),
    );
    cx.publishing(
        Publishes::new()
            .live_rows(move || {
                let tube = rows.borrow();
                crate::rows::live_rows(&tube.model, tube.device_sync_view())
            })
            .live_row(move |id| {
                let tube = row.borrow();
                crate::rows::live_row(&tube.model, tube.device_sync_view(), id)
            })
            .search_entries(move || entries.borrow().search_entries_of_model())
            .search_query(move |asked| query.borrow_mut().follow_palette_query(asked.text()))
            .search_pending(move || tube.borrow().suggestions_pending()),
        Box::new(window),
    )
}

impl FullscreenProvider for TubeApp {
    fn fullscreen_intent(&self) -> Option<bool> {
        FullscreenProvider::fullscreen_intent(&*self.tube.borrow().player.borrow())
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        FullscreenProvider::fullscreen_params(&*self.tube.borrow().player.borrow())
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        FullscreenProvider::on_fullscreen_changed(
            &mut *self.tube.borrow().player.borrow_mut(),
            active,
        );
    }
}

impl Tube {
    pub(crate) fn search_entries_of_model(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        let suggested = self.suggested.videos_for(&self.palette_query);
        let mut entries = crate::search_type::model_entries(&self.model);
        entries.retain(|entry| {
            !suggested.iter().any(|video| {
                entry.anchor.as_deref() == Some(&*crate::search_type::video_anchor(&video.id))
            })
        });
        entries.extend(crate::search_type::query_entry(&self.palette_query));
        entries.extend(crate::search_type::suggested_entries(
            &self.palette_query,
            suggested,
            self.model.settings.dearrow_titles_on(),
        ));
        entries
    }

    pub(crate) fn follow_palette_query(&mut self, query: &str) {
        query.trim().clone_into(&mut self.palette_query);
        if !self.online || crate::search_type::query_entry(&self.palette_query).is_none() {
            self.suggested.awaiting = false;
            return;
        }
        if self.suggested.answered(&self.palette_query) {
            self.suggested.awaiting = false;
            return;
        }
        if self.worker.is_none() {
            self.worker = Some(crate::api::Worker::spawn(self.wake.clone()));
        }
        self.suggested.awaiting = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.suggest(&self.palette_query));
    }

    pub(crate) fn suggestions_pending(&self) -> bool {
        self.suggested.awaiting
    }

    pub(crate) fn take_suggestions(&mut self, query: String, videos: Vec<crate::model::Video>) {
        if query != self.palette_query {
            return;
        }
        self.suggested = crate::suggest::Suggested {
            query,
            videos,
            awaiting: false,
        };
    }

    fn adopt_suggested(&mut self, video: &str) {
        if self.model.video(video).is_some() {
            return;
        }
        let Some(found) = self.suggested.videos.iter().find(|v| v.id == video) else {
            return;
        };
        let found = found.clone();
        self.model
            .ensure_channel_stub(&found.channel_id, &found.channel_name);
        self.model.upsert_video(found);
    }
}

impl Widget for TubeApp {
    ui::provides!(dyn ui::widget::FullscreenProvider);

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow_document();
        self.take_window_toolbar_events();
        self.tube().sync_before_shell();
        self.answer_shell_asks();
        self.show_new_notices();
        self.shell.sync(state, path);
        self.tube().sync_after_shell();
        self.answer_shell_asks();
        self.declare_window_toolbar();
    }

    fn composition_follows_allocation(&self) -> bool {
        true
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let key = {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            format!("{theme:?}").hash(&mut hasher);
            self.rect.width().to_bits().hash(&mut hasher);
            self.content_width(theme).to_bits().hash(&mut hasher);
            hasher.finish()
        };
        self.answer_shell_asks();
        if !self.shell_built
            || self.shell_stale
            || self.tube.borrow().structure_dirty
            || self.shell_key != Some(key)
        {
            self.rebuild_shell(theme);
            self.shell_stale = false;
            self.tube().structure_dirty = false;
            self.shell_key = Some(key);
        }
        let node = self.shell.build_layout(taffy, theme, node_map);
        self.child_node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
        if rect.size() != self.rect.size() {
            self.shell_stale = true;
        }
        self.rect = rect;
        self.shell.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_child(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            let is_back = matches!(logical_key, Key::Named(NamedKey::BrowserBack))
                || (ctx.modifiers.state().alt_key()
                    && matches!(logical_key, Key::Named(NamedKey::ArrowLeft)));
            let tube = self.tube.borrow();
            if is_back && tube.nav.can_go_back() {
                action::push(&tube.bus, Action::Back);
                *ctx.handled = true;
            }
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_pointer = ctx.logical(position);
                let tube = self.tube.borrow();
                if self.back_gesture.pointer_move(self.last_pointer) == Some(ui::NavAction::Back)
                    && tube.nav.can_go_back()
                {
                    action::push(&tube.bus, Action::Back);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.back_gesture.pointer_down(self.last_pointer);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.back_gesture.pointer_up();
            }
            _ => {}
        }

        self.shell.handle_event(event, ctx);

        if !*ctx.handled
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            && self.tube.borrow().nav.can_go_back()
        {
            action::push(&self.tube.borrow().bus, Action::Back);
            *ctx.handled = true;
        }

        let mut tube = self.tube();
        if tube.settle_input() {
            tube.structure_dirty = true;
        }
        drop(tube);
        self.answer_shell_asks();
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut tube = self.tube();
        let structure_dirty = std::mem::take(&mut tube.structure_dirty);
        let dirty = tube.pump() || structure_dirty;
        drop(tube);
        self.shell_stale |= dirty;
        dirty | self.shell.take_structure_dirty()
    }

    fn needs_redraw(&self) -> bool {
        let tube = self.tube.borrow();
        tube.structure_dirty
            || tube.images_pending()
            || !self.shell_built
            || self.shell.needs_redraw()
            || self.shell.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.shell.mark_drawn();
    }

    fn is_layout_dirty(&self) -> bool {
        !self.shell_built || self.shell.is_layout_dirty()
    }

    fn needs_idle_sync(&self) -> bool {
        self.tube.borrow().images_pending()
    }

    fn needs_animation_frame(&self) -> bool {
        let tube = self.tube.borrow();
        tube.resolving() || tube.images_pending() || self.shell.needs_animation_frame()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.shell]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.shell]
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let tube = self.tube.borrow();
        let model = &tube.model;
        let route = match &model.route {
            Route::Home => t("tube-nav-home"),
            Route::Trending => t("tube-nav-trending"),
            Route::Subscriptions => t("tube-nav-subscriptions"),
            Route::Queue => t("tube-queue-shared"),
            Route::Watch(id) => model
                .video(id)
                .map_or_else(|| t("tube-video"), |video| video.title.clone()),
            Route::Channel(id) => model
                .channel(id)
                .map_or_else(|| t("tube-creator"), |channel| channel.name.clone()),
            Route::Playlist(id) => model
                .playlist(id)
                .map_or_else(|| t("tube-playlist"), |playlist| playlist.title.clone()),
            Route::History => t("tube-nav-history"),
            Route::Settings => t("tube-nav-settings"),
            Route::Search(query) => crate::i18n::t_args(
                "tube-search-heading",
                &[("query", ArgValue::Text(query.clone()))],
            ),
        };
        Some(
            AccessNode::new(Role::Window)
                .with_name(t("tube-nav-name"))
                .with_description(route)
                .with_state(AccessState::new().busy(model.feed_loading)),
        )
    }

    fn name(&self) -> &'static str {
        "TubeApp"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_search_request(&mut self) -> bool {
        self.shell.take_search_request()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests;
