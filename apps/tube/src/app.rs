//! `TubeApp` — the root widget.
//!
//! Tube renders a single in-memory [`TubeModel`]. The shared library in it
//! (likes, queue, subscriptions, comment likes) is what the resource was
//! last presented; the catalogue, navigation, playback and notices are this
//! window's own, and the watch history and playback preferences the
//! person's.
//! Navigation rebuilds the whole widget tree from the model in
//! `build_layout` (the `RebuildHarness` pattern), so the screen functions in
//! `screens.rs` can stay pure.
//!
//! Interaction flows through the [`ActionBus`]: leaf widgets push an
//! [`Action`]; `handle_event` drains the queue, submits library edits and
//! effects through the framework dispatcher, changes what this window shows
//! and flags a structural rebuild.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::framework::{Dispatcher, PersonRecord};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toast::{ToastKind, ToastStack};
use ui::widget::{
    FullscreenProvider, NavItem, NavList, NavMode, NavSection, NavShell, VideoPlayer, Widget,
};

use crate::action::{self, Action, ActionBus, Invocations};
use crate::actions::TubeEffect;
use crate::api::{Request, Update, Worker};
use crate::history::WatchHistory;
use crate::i18n::{arg, t, t_args};
use crate::model::{Route, TubeModel};
use crate::resource::Library;
use crate::screens;
use crate::widgets::SharedPlayer;
use ui::tilo_resource::{Edit, Invoke};

pub(crate) const QUALITIES: [&str; 4] = ["1080p", "720p", "480p", "auto"];

/// Pull the frames the player's engine decoded into its surface, as the
/// player's own `sync` does every frame; it reads no document.
fn pump_player(player: &mut VideoPlayer) {
    player.sync(&(), &());
}

/// A view's persisted `(x, y)` scroll offset, shared with its `Scrollable`.
type ScrollCell = Rc<Cell<(f32, f32)>>;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent render/interaction cache (rebuild-dirty flags, \
              --shots staging, sidebar bookkeeping), not a state machine"
)]
pub struct TubeApp {
    model: TubeModel,
    /// Where library edits and effects go: the framework's door for the
    /// resource. `None` until the framework attaches the surface; a bare
    /// widget shows a library it cannot change.
    dispatcher: Option<Dispatcher<Library>>,
    /// The person's watch history, a record in their own room, in a live
    /// window.
    history: Option<PersonRecord<WatchHistory>>,
    /// The person's playback preferences, a personal setting, once the
    /// framework attaches the surface; a bare widget keeps them in its model.
    preferences: Option<ui::framework::Setting<crate::model::Settings>>,
    /// The history a window without the person's record edits instead: a
    /// capture's sample, or a bare widget's. Kept in memory only.
    local_history: Option<ui::tilo_resource::Replica<WatchHistory>>,
    bus: ActionBus,
    /// Whether browsing can go back, read by the shell's Back chip.
    can_go_back: Rc<std::cell::Cell<bool>>,
    worker: Option<Worker>,
    #[cfg(feature = "grayjay-sync")]
    device_sync: crate::sync::DeviceSync,
    #[cfg(feature = "grayjay-sync")]
    device_sync_seeded: bool,

    /// The window shell: the navigation rail, the header row and the current
    /// screen. It lives as long as the app, so the person's sidebar choice
    /// and its motion survive navigation; `build_layout` swaps the screen
    /// into it when the structure, theme or width changed.
    shell: NavShell,
    /// The current screen has been built into `shell` at least once.
    shell_built: bool,
    /// Resource invocations raised by controls that report through a closure
    /// (the navigation rail, the watch toolbar), handed to the host.
    invocations: Invocations,
    /// Transient notices ("Link copied", a skipped segment) as toasts.
    toasts: ToastStack,
    /// `model.notice_count` already shown as toasts.
    toasted: u64,
    /// Live `YouTube` search answers handed to the platform search.
    /// A structural change the host took from `take_structure_dirty` that
    /// the next `build_layout` has not rebuilt yet.
    shell_stale: bool,
    /// Theme and width the current shell was built for.
    shell_key: Option<u64>,
    child_node: Option<NodeId>,
    rect: Rect,

    structure_dirty: bool,
    /// Process-local text for the top-bar search. `TextInput::from_state`
    /// supplies the toolkit's full local editing path (IME, selection,
    /// accessibility and keyboard shortcuts) without creating an Automerge
    /// object that another collaborator could overwrite.
    search_query: ui::LocalState<String>,
    /// Latched by the compact search button; the host drains it through
    /// [`Widget::take_search_request`] and opens its search palette.
    search_requested: bool,

    /// The user's explicit sidebar preference for this session, carried
    /// across the structural rebuilds that recreate the widget tree. `None`
    /// means "follow the layout default" (docked ⇒ open, overlay ⇒ closed);
    /// the hamburger / scrim / Esc set it to `Some(_)`. The live [`NavSplit`]
    /// owns the canonical state per frame; this mirrors it so a navigation
    /// rebuild doesn't snap the flap back to its default.
    sidebar_open: Option<bool>,
    /// `--shots` only: force the overlay flap open in the rendered still so a
    /// narrow screenshot captures the summoned rail + scrim rather than an
    /// empty closed flap. Never set by the live app.
    force_sidebar_open_shot: bool,
    /// Whether this instance should reach out to the live data backend. Set by
    /// [`TubeApp::online`] — the live binary turns it on; the `--shots` /
    /// offscreen path leaves it off so every screenshot stays hermetic on the
    /// bundled sample (no worker thread, no network).
    online: bool,
    /// Guards the one-shot startup fetch (home/trending) so we kick it off
    /// exactly once, on the first `sync`, rather than every frame.
    kicked_off: bool,

    /// The watch id we have already asked the worker to resolve a stream for, so
    /// a [`Request::PlayVideo`] is sent exactly once per watch-open (not every
    /// frame). Cleared implicitly by opening a *different* video.
    playing_id: Option<String>,
    /// The shared, persistent video player for the watch page — a real
    /// [`ui::widget::VideoPlayer`] backed by a [`media::MediaEngine`] (`GStreamer`
    /// decode + audio out with `--features media`, a synthesized frame otherwise).
    /// It lives here, behind an `Rc<RefCell<_>>`, so it survives the whole-tree
    /// rebuilds Tube does on every structural change — each rebuilt watch screen
    /// mounts it via a cheap [`crate::widgets::PlayerHost`] rather than recreating
    /// it (which would restart the video). The engine is opened on a resolved
    /// stream URL when [`Update::StreamResolved`] lands.
    player: SharedPlayer,
    /// The video id whose stream URL is currently loaded into the player engine,
    /// so we open a source exactly once per resolution and can mirror the engine
    /// playhead back onto the model only while it's the open video.
    player_video: Option<String>,

    /// Async image loader/cache for real thumbnails / avatars / banners, keyed
    /// by URL. `Some` when online (or seeded for a real-image `--shots` proof);
    /// `None` on the hermetic offline path, where every image lookup returns
    /// `None` and the procedural gradient/initials fallback shows.
    images: Option<ui::ImageStore>,

    /// End (seconds) of the show-mode `SponsorBlock` segment the playhead is
    /// currently inside, if any. The on-video "Skip" overlay is shown for it.
    /// Tracked here so a *transition* (playhead entering/leaving the segment)
    /// dirties the tree — the advancing playhead alone doesn't rebuild it.
    show_skip_end: Option<f64>,

    /// Whether the shared player's desktop-media (MPRIS) service has been brought
    /// up. Spawned once, on the first real stream load in the online app, so the
    /// system top-bar widget + media keys drive playback. Never in `--shots`/tests.
    mpris_started: bool,

    /// Toolkit navigation stack: the history of views. Every navigation pushes;
    /// [`Action::Back`] (a Back button, Escape, Alt-Left or an edge-swipe) pops.
    /// The stack is the source of truth for "can I go back" and "where to"; the
    /// live `model.route` mirrors its current entry.
    nav: ui::NavStack<Route>,
    /// The left-edge back-gesture recognizer (toolkit primitive). Pointer events
    /// feed it; a recognized swipe pushes [`Action::Back`], so swipe-nav works at
    /// the toolkit layer without app-specific gesture code.
    back_gesture: ui::EdgeSwipeBack,
    /// Per-view scroll positions, keyed by a route's view key, so returning to a
    /// view (via Back or re-navigation) restores exactly where it was scrolled —
    /// and a structural rebuild never snaps the scroll back to the top. Shared
    /// `Cell`s handed to each screen's `Scrollable` via `with_persisted_offset`.
    /// `RefCell` because it is populated from the `&self` shell builder.
    scroll_cells: RefCell<HashMap<String, ScrollCell>>,
    /// Channels whose avatar/subscriber fetch is already in flight, so opening a
    /// watch page (or re-rendering) never re-requests the same channel.
    requested_channels: HashSet<String>,
    /// Last pointer position (logical px), tracked so a mouse-button press can
    /// seed the edge-swipe back gesture with the press point.
    last_pointer: Point,
}

impl Default for TubeApp {
    fn default() -> Self {
        Self::new()
    }
}

impl TubeApp {
    pub fn new() -> Self {
        let bus = action::new_bus();
        let can_go_back = Rc::new(Cell::new(false));
        let shell = Self::new_shell(&bus, &can_go_back);
        Self {
            model: crate::sample::model(),
            dispatcher: None,
            history: None,
            preferences: None,
            local_history: None,
            bus,
            worker: None,
            #[cfg(feature = "grayjay-sync")]
            device_sync: crate::sync::DeviceSync::default(),
            #[cfg(feature = "grayjay-sync")]
            device_sync_seeded: false,
            shell,
            can_go_back,
            shell_built: false,
            invocations: action::new_invocations(),
            toasts: ToastStack::new(),
            toasted: 0,
            shell_stale: true,
            shell_key: None,
            child_node: None,
            rect: Rect::ZERO,
            structure_dirty: false,
            search_query: ui::LocalState::new(String::new()),
            search_requested: false,
            sidebar_open: None,
            force_sidebar_open_shot: false,
            online: false,
            kicked_off: false,
            playing_id: None,
            player: Rc::new(RefCell::new(
                VideoPlayer::new().with_engine(media::MediaEngine::new()),
            )),
            player_video: None,
            images: None,
            show_skip_end: None,
            mpris_started: false,
            nav: ui::NavStack::new(Route::Home),
            back_gesture: ui::EdgeSwipeBack::default_back(),
            scroll_cells: RefCell::new(HashMap::new()),
            requested_channels: HashSet::new(),
            last_pointer: Point::ZERO,
        }
    }

    /// Apply the actions the controls queued: from the event that queued
    /// them, and from the frame's sync for those the window's shell runs
    /// (its Back chip). Returns whether the view changed.
    fn apply_queued(&mut self) -> bool {
        let mut changed = false;
        for action in action::drain(&self.bus) {
            changed |= self.apply(action);
        }
        changed
    }

    /// The window shell: a docked icon rail of destinations beside the
    /// screen, with the screen's title and actions in the header row, and
    /// the shell's Back for the pages reached by browsing (a video, its
    /// creator, a playlist). Escape, Alt+Left, the mouse back button and an
    /// edge swipe do the same.
    fn sidebar_currently_open(&self) -> bool {
        self.shell.is_sidebar_open()
    }

    fn dismiss_overlay_rail(&mut self) {
        if !self.shell.geometry().side_by_side && self.shell.is_sidebar_open() {
            self.shell.toggle_sidebar();
        }
    }

    fn new_shell(bus: &ActionBus, can_go_back: &Rc<std::cell::Cell<bool>>) -> NavShell {
        let can = Rc::clone(can_go_back);
        let bus = Rc::clone(bus);
        NavShell::new()
            .with_id("tube_shell")
            .mode(NavMode::Overlay)
            .on_back(move || can.get(), move || action::push(&bus, Action::Back))
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
            TubeEffect::ToggleSidebar => {
                let now = self.sidebar_currently_open();
                self.sidebar_open = Some(!now);
                self.shell.toggle_sidebar();
            }
            TubeEffect::OpenVideo { video } => {
                if self.model.video(&video).is_none() {
                    return Err("That video is no longer available".into());
                }
                self.open_video(video);
                self.dismiss_overlay_rail();
            }
            TubeEffect::OpenCreator { creator } => {
                if self.model.channel(&creator).is_none() {
                    return Err("That creator is no longer available".into());
                }
                self.navigate_to(Route::Channel(creator.clone()));
                self.dismiss_overlay_rail();
                self.request_live(Request::Channel(creator));
            }
            TubeEffect::OpenPlaylist { playlist } => {
                if self.model.playlist(&playlist).is_none() {
                    return Err("That playlist is no longer available".into());
                }
                self.navigate_to(Route::Playlist(playlist.clone()));
                self.dismiss_overlay_rail();
                self.request_live(Request::PlaylistPage(playlist));
            }
            TubeEffect::TogglePlayback => {
                self.player.borrow_mut().toggle_play();
                self.model.playing = self.player.borrow().is_playing();
            }
            TubeEffect::Share { video } => {
                tracing::debug!(target: "tube::share", %video, at = ?ui::time::now(), "share effect entered");
                let url = format!("https://youtu.be/{video}");
                ui::clipboard_copy(&url);
                self.model.push_notice(format!("Link copied: {url}"));
                tracing::debug!(target: "tube::share", %video, at = ?ui::time::now(), "share model notice created");
            }
            TubeEffect::Download { video } => {
                // Published as unavailable; a caller that invokes it anyway
                // learns why.
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
                    self.model.push_notice("Watch history cleared");
                }
            }
            TubeEffect::PairDevice { invite } => {
                #[cfg(feature = "grayjay-sync")]
                if !self.device_sync.pair(invite) {
                    return Err("Device pairing is busy or unavailable".into());
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
                    return Err("Device sync is busy or unavailable".into());
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
            // Answered above, as preferences.
            TubeEffect::SetPreference { .. }
            | TubeEffect::SetPreferredQuality { .. }
            | TubeEffect::SetSegmentHandling { .. } => {}
        }
        Ok(())
    }

    /// Submit a library edit through the framework's door, which checks it,
    /// applies it, records the undo step and presents the result.
    fn edit(&self, edit: Edit<Library>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(edit);
        }
    }

    /// Invoke an effect through the framework's door, which asks for the
    /// confirmation a destructive one needs before it reaches
    /// [`Self::perform`].
    fn invoke(&self, effect: &TubeEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    /// Edit the watch history and show the result. In a live window the
    /// edit is an action on the person's own record, which carries it to
    /// their other devices; returns false (and says so) when it cannot be
    /// stored.
    fn edit_history(&mut self, edit: &Invoke<WatchHistory>) -> bool {
        let edited = if let Some(record) = &self.history {
            record
                .invoke(edit)
                .map(|()| record.with(WatchHistory::videos))
                .map_err(|error| error.to_string())
        } else {
            let shown = &self.model.history;
            let replica = self.local_history.get_or_insert_with(|| {
                let mut replica = ui::tilo_resource::Replica::<WatchHistory>::new();
                let count = shown.len();
                let watched: Vec<(String, u64)> = shown
                    .iter()
                    .enumerate()
                    .map(|(at, video)| (video.clone(), u64::try_from(count - at).unwrap_or(0)))
                    .collect();
                let _ = replica.apply(WatchHistory::IMPORT.with(watched));
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

    /// Show the history another window or device stored.
    fn follow_history(&mut self) -> bool {
        let Some(history) = &self.history else {
            return false;
        };
        let stored = history.with(WatchHistory::videos);
        if stored == self.model.history {
            return false;
        }
        self.model.history = stored;
        true
    }

    /// Bring in what an approved Grayjay device sent: its subscriptions as
    /// library edits through the same door as the visible controls, its
    /// watch history into the person's own, its settings into the person's
    /// preferences.
    ///
    /// The transport carries a compact, bounded metadata package; every
    /// imported identifier is validated here first.
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded validation per imported device-sync field keeps the import reviewable"
    )]
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

    /// An instance with no catalogue and no network.
    ///
    /// [`Self::new`] starts from the bundled `sample` catalogue, which is
    /// review/`--shots` material — invented channels, invented view counts.
    /// Anything that is not a shot or a test wants this instead: Home
    /// renders its "Nothing to show yet" empty state rather than a
    /// fabricated trending feed, and nothing is fetched.
    ///
    /// This is what a past version of the library is shown in.
    pub fn empty() -> Self {
        Self {
            model: TubeModel::default(),
            ..Self::new()
        }
    }

    /// Opt this instance into live data. The live binary calls this; the
    /// `--shots`/offscreen renderer and the interaction tests do not, so they
    /// stay fully offline and deterministic. When online, the first `sync`
    /// spawns the fetch worker and requests the home/trending feed; if the
    /// network is unreachable (or the `live` feature is off) the worker reports
    /// an offline state and Home explains that no live catalogue is available.
    #[must_use]
    pub fn online(mut self) -> Self {
        self.online = true;
        // The online app starts with an empty catalogue and populates it from
        // live results. The bundled `sample` catalogue is reserved for the
        // hermetic `--shots`/demo path; Home shows a loading skeleton first.
        self.model = TubeModel::default();
        self.model.feed_loading = true;
        // Real thumbnails / avatars / banners: an async image store backed by a
        // network fetcher. Kept off the offline/`--shots` path (which never
        // calls `online`) so screenshots stay hermetic.
        self.images = Some(crate::images::image_store());
        self
    }

    fn play_demo(&mut self) {
        let id = self
            .model
            .videos
            .first()
            .map_or_else(|| "vid_donut".to_string(), |v| v.id.clone());
        self.model.route = Route::Watch(id.clone());
        {
            let mut p = self.player.borrow_mut();
            // Pin the deterministic synthesized-frame backend for the shot: the
            // `media` feature (real GStreamer decode) is compiled in by default,
            // so `MediaEngine::new()` would otherwise build a real pipeline and
            // contact the placeholder `demo.local` host. The synthesized engine
            // seeds the same surface without a pipeline or network. (The real
            // decode path is exercised by `with_live_frame_for_shot` under the
            // separate `--live-shots` hook.)
            p.set_engine(media::MediaEngine::new_synthesized());
            p.open(media::Source::Remote {
                url: "https://demo.local/clip.mp4".into(),
            });
            // Seed a colourful mid-stream frame for the still, then pump the
            // engine so the frame lands in the surface. The offscreen renderer
            // syncs the app exactly once — *before* this player is mounted in the
            // tree — so without pumping here the single shot would draw an empty
            // (never-pumped) surface. A live run pumps every frame, so this is
            // shots-only.
            p.seek_to_ms(45_000);
            p.play();
            p.reveal_controls();
            pump_player(&mut p);
        }
        self.player_video = Some(id);
    }

    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "empty" => self.show_empty_home(),
            "loading" => self.show_loading_home(),
            #[cfg(feature = "grayjay-sync")]
            "device-sync" => self.show_device_sync_sample(),
            "play-demo" => self.play_demo(),
            "narrow" => {
                self.force_sidebar_open_shot = true;
                self.shell.open_sidebar();
                self.shell.snap();
            }
            "trending" => self.show_route(Route::Trending),
            "subscriptions" => self.show_route(Route::Subscriptions),
            "queue" => self.show_route(Route::Queue),
            "history" => self.show_route(Route::History),
            "settings" => self.show_route(Route::Settings),
            "search" => self.show_route(Route::Search("rust".into())),
            "watch" => self.show_route(Route::Watch("vid_donut".into())),
            _ => {}
        }
        self.structure_dirty = true;
        self.shell_stale = true;
    }
    /// Test/shots hook: attach an image store so an offscreen `--shots` render
    /// can paint fetched thumbnails, avatars, and banners. The live app gets
    /// its store via [`Self::online`].
    #[must_use]
    pub fn with_image_store(mut self, store: ui::ImageStore) -> Self {
        self.images = Some(store);
        self
    }

    /// Test/shots hook: replace the model wholesale (e.g. a channel page built
    /// from real yt-dlp data) so an offscreen render shows real content.
    #[must_use]
    pub fn with_model_for_test(mut self, model: TubeModel) -> Self {
        self.stage_model_for_test(model);
        self
    }

    /// Supply a deterministic catalogue in a hosted test without changing its
    /// dispatcher, effect admission, or shared library. Live launch stays empty.
    ///
    /// A live window has already spawned its fetch worker on its first frame.
    /// Retire it with the live catalogue: its startup notes, its status and
    /// the feed it fetched would otherwise land whenever the network answers,
    /// rebuild the staged page and drop a click made in between.
    pub fn stage_model_for_test(&mut self, model: TubeModel) {
        self.search_query.set(model.search_query.clone());
        self.model = model;
        self.online = false;
        self.worker = None;
        self.shell_stale = true;
    }

    /// Test hook: fetch nothing, and fold the results the returned sender
    /// sends as the network's, whenever the test lands one.
    #[doc(hidden)]
    pub fn script_worker_for_test(&mut self) -> std::sync::mpsc::SyncSender<crate::api::Update> {
        let (worker, results) = crate::api::Worker::scripted();
        self.worker = Some(worker);
        results
    }

    #[must_use]
    pub fn with_search_for_test(mut self, query: &str, video_ids: Vec<String>) -> Self {
        self.model.search_query = query.to_string();
        self.model.search_hits_query = query.to_string();
        self.model.search_hits = video_ids;
        self.model.route = Route::Search(query.to_string());
        self.nav.push(Route::Search(query.to_string()));
        self
    }

    /// Show the shared library the resource holds: likes, the queue,
    /// subscriptions and comment likes. Every presentation is the
    /// whole library, so an emptied one (everything unliked, the queue
    /// cleared) shows empty.
    pub(crate) fn present_library(&mut self, state: &Library) {
        self.model.liked = state.likes.iter().cloned().collect();
        self.model.queue = state.queue.iter().cloned().collect();
        self.model.subscriptions = state.subscriptions.iter().cloned().collect();
        self.model.comment_likes = state.comment_likes.iter().cloned().collect();
        self.structure_dirty = true;
    }

    /// Start Grayjay device sync over the app's secret store, which holds
    /// this device's key seed, and the devices paired on this device.
    #[cfg(feature = "grayjay-sync")]
    pub(crate) fn start_device_sync(
        &mut self,
        secrets: &ui::secrets::SecretStore,
        paired: ui::storage::Settings<crate::sync::PairedDevices>,
    ) {
        self.device_sync = crate::sync::DeviceSync::production("Tilo Tube", secrets, paired);
    }

    /// Follow the person's playback preferences: show them, keep what this
    /// window changes, and take what another window or device changed.
    pub(crate) fn follow_preferences(
        &mut self,
        setting: ui::framework::Setting<crate::model::Settings>,
    ) {
        self.model.settings = setting.get();
        self.preferences = Some(setting);
        self.structure_dirty = true;
    }

    /// Show preferences another window or device stored.
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

    /// Change the person's preferences and keep them; false (and says so)
    /// when they cannot be stored.
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

    /// Attach the framework's door for the resource and, in a live window,
    /// the person's watch history.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: Dispatcher<Library>,
        history: Option<PersonRecord<WatchHistory>>,
    ) {
        self.dispatcher = Some(dispatcher);
        self.history = history;
        self.follow_history();
    }

    fn show_loading_home(&mut self) {
        self.model = TubeModel::default();
        self.model.feed_loading = true;
        self.model.route = Route::Home;
    }

    fn show_empty_home(&mut self) {
        self.model = TubeModel::default();
        self.model.data_source.label = "offline — network unavailable".into();
        self.model.route = Route::Home;
    }

    /// `--shots` builder: record the render viewport up front so the first
    /// frame sizes the width-driven watch player from the true width. The
    /// live app learns its width from `apply_layout` each frame.
    #[must_use]
    pub fn with_viewport(mut self, w: f64, h: f64) -> Self {
        self.rect = Rect::new(0.0, 0.0, w.max(0.0), h.max(0.0));
        self
    }

    /// `--shots` builder: start on a specific [`Route`] so each screen can be
    /// captured for self-inspection. Pure presentation override — the live app
    /// starts from the sample/persisted route instead.
    #[must_use]
    pub fn with_route_shot(mut self, route: Route) -> Self {
        self.show_route(route);
        self
    }

    fn show_route(&mut self, route: Route) {
        if let Route::Search(q) = &route {
            q.clone_into(&mut self.model.search_query);
            self.search_query.set(q.clone());
        }
        // The shell uses `NavStack` as the navigation authority (Back state,
        // keyboard handling and the top bar all consult it), while the model
        // mirrors its current route.  A deterministic screenshot must seed
        // both sides together; changing only the presentation model created a
        // state that a user or an agent could never actually navigate to.
        // A page reached by browsing (a video, a creator, a playlist) is
        // opened from Home, so Back has somewhere to return to.
        if matches!(
            route,
            Route::Watch(_) | Route::Channel(_) | Route::Playlist(_)
        ) {
            self.nav = ui::NavStack::new(Route::Home);
            self.nav.push(route.clone());
        } else {
            self.nav = ui::NavStack::new(route.clone());
        }
        self.model.route = route;
    }

    #[cfg(feature = "grayjay-sync")]
    fn show_device_sync_sample(&mut self) {
        self.model.route = Route::Settings;
        self.device_sync = crate::sync::DeviceSync::visual_sample();
        // Scroll to the end of the form: the device section is the last one.
        self.scroll_cells
            .borrow_mut()
            .insert("settings".into(), Rc::new(Cell::new((0.0, f32::MAX))));
    }

    /// `--live-shots` hook: open the shared player on a real, directly-playable
    /// stream `url` and pump the media engine until a real frame decodes
    /// (bounded), so an offscreen still captures the ACTUAL decoded picture — the
    /// same `VideoPlayer` + `media::MediaEngine` the live app streams through.
    /// Needs the `media` decode backend to produce a real frame; if none arrives
    /// within the budget the surface stays as-is (the poster/backdrop shows).
    #[must_use]
    pub fn with_live_frame_for_shot(mut self, video_id: &str, url: String) -> Self {
        use ui::time::Duration;
        self.model.route = Route::Watch(video_id.to_string());
        {
            let mut p = self.player.borrow_mut();
            p.open(media::Source::Remote { url });
            p.play();
            p.reveal_controls();
            // The offscreen renderer syncs the app only once — before this player
            // is mounted in the tree — so the frame must already be in hand before
            // the shot draws. Poll at the live app's ~16ms cadence: the engine's
            // appsink pull is non-blocking and the sink paces/drops frames to the
            // clock, so a slow poll misses them.
            let pump = |p: &mut ui::widget::VideoPlayer,
                        until: &dyn Fn(&ui::widget::VideoPlayer) -> bool,
                        budget: Duration| {
                let deadline = ui::time::now() + budget;
                while ui::time::now() < deadline {
                    pump_player(p);
                    if until(p) {
                        return true;
                    }
                    let _ = ui::rt::sleep_thread(Duration::from_millis(15));
                }
                false
            };
            // Wait for the first decoded frame before capturing the player.
            pump(
                &mut p,
                &|p| p.latest_frame().is_some(),
                Duration::from_secs(25),
            );
            // Seek past the typical fade-from-black intro for a useful capture.
            if p.duration_ms() > 40_000 {
                p.seek_to_ms(32_000);
                pump(
                    &mut p,
                    &|p| p.engine().map_or(0, media::MediaEngine::position_ms) >= 30_000,
                    Duration::from_secs(15),
                );
            }
        }
        self.player_video = Some(video_id.to_string());
        self
    }

    /// Bench hook: put the app on a watch page with a **synthesized, paused-but-
    /// loaded** player — a real decoded frame sitting in the surface, playback
    /// paused. This reproduces the scroll-perf scenario (`--bench-scroll --watch
    /// --loaded`): the scroll loop re-syncs each frame, pumping the engine, so the
    /// bench measures the true per-frame cost of a loaded-but-paused watch page.
    #[must_use]
    pub fn with_bench_loaded_watch(mut self, video_id: &str) -> Self {
        self.model.route = Route::Watch(video_id.to_string());
        self.player = Rc::new(RefCell::new(
            VideoPlayer::new().with_engine(media::MediaEngine::new_synthesized()),
        ));
        {
            let mut p = self.player.borrow_mut();
            p.open(media::Source::Remote {
                url: "https://demo.local/clip.mp4".into(),
            });
            p.seek_to_ms(30_000);
            // Pull the first frame into the surface, then pause so it is
            // "loaded-but-paused" (the reported jank condition).
            pump_player(&mut p);
            p.pause();
        }
        self.player_video = Some(video_id.to_string());
        self
    }

    /// Test accessor: the current route (read-only).
    pub fn route_for_test(&self) -> &Route {
        &self.model.route
    }

    /// Test accessor: whether the navigation stack has somewhere to go back to.
    pub fn can_go_back_for_test(&self) -> bool {
        self.nav.can_go_back()
    }

    /// Test accessor: read-only view of the model, so integration tests can
    /// assert on real state (likes, notices, settings, playhead) after driving
    /// the widget tree.
    pub fn model_for_test(&self) -> &TubeModel {
        &self.model
    }

    /// Crate-internal read-only model view, so [`crate::actions`] can publish the
    /// open-resource tree (the playing video + its comments) for Ctrl+K.
    pub(crate) fn model_ref(&self) -> &TubeModel {
        &self.model
    }

    /// What device sync shows: its status and the paired devices.
    #[cfg(feature = "grayjay-sync")]
    pub(crate) fn device_sync_view(&self) -> &crate::sync::SyncView {
        self.device_sync.view()
    }

    /// What device sync shows in a build without it: unavailable.
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

    /// Test accessor: whether the navigation rail is currently shown (resolves
    /// the session preference against the layout default).
    pub fn sidebar_open_for_test(&self) -> bool {
        self.shell.is_sidebar_open()
    }

    /// The UI context the screens are built from.
    fn ui<'a>(&'a self, theme: &'a Theme) -> screens::Ui<'a> {
        screens::Ui {
            model: &self.model,
            theme,
            bus: &self.bus,
            imgs: crate::images::Imgs(self.images.as_ref()),
            invocations: &self.invocations,
            content_width: self.content_width(theme),
        }
    }

    /// The current screen, ready for the shell's content column.
    fn build_screen(&self, theme: &Theme) -> Box<dyn Widget> {
        let ui = self.ui(theme);
        let content = match &self.model.route {
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
                        self.device_sync.view(),
                        self.device_sync.pairing_input.clone(),
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
                self.player.clone(),
            ),
            Route::Channel(id) => screens::channel(ui, id),
            Route::Playlist(id) => screens::playlist(ui, id),
        };
        // Each view keeps its scroll position across rebuilds and returns to
        // it via Back.
        Box::new(content.with_persisted_offset(self.scroll_cell_for(&self.model.route)))
    }

    /// The navigation rail: Tube's destinations, the current one selected.
    /// Choosing one invokes the same published navigation action an agent or
    /// the palette uses.
    fn build_rail(&self) -> NavList {
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
        .rail()
        .with_id("tube_rail")
        .with_accessible_name(t("tube-nav-name"))
        .on_select({
            let bus = self.bus.clone();
            move |id| action::push(&bus, Action::Effect(TubeEffect::ShowScreen { screen: id }))
        });
        list.set_selected(Some(self.model.route.nav_section()));
        list
    }

    /// Put the current screen, the rail and the header into the shell.
    fn rebuild_shell(&mut self, theme: &Theme) {
        let screen = self.build_screen(theme);
        let title = screens::title(&self.model.route);
        let actions = screens::header_actions(self.ui(theme));
        self.shell.set_content_header(title, actions);
        self.can_go_back.set(self.nav.can_go_back());
        self.shell.set_can_go_back(self.nav.can_go_back());
        // Catalogue updates may arrive between pointer motion and press.
        // Keep the static destinations and their real input/focus state while
        // refreshing content; the shell still lays them out for theme/width.
        if let Some(rail) = self.shell.sidebar_mut::<NavList>() {
            rail.set_selected(Some(self.model.route.nav_section()));
        } else {
            let rail = self.build_rail();
            self.shell.set_sidebar(Some(Box::new(rail)));
        }
        self.shell.set_content(screen);
        self.shell_built = true;
    }

    /// Logical width of the shell's content column: the window less the rail
    /// while it shows. Sizes the width-driven watch player and picks the
    /// watch page's one- or two-column arrangement.
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

    /// Logical height under the header row, which caps the watch player.
    fn content_height(&self, theme: &Theme) -> f64 {
        (self.rect.height() - f64::from(theme.tokens.metrics.header_height)).max(1.0)
    }

    /// A stable key naming a route's *view*, for the per-view scroll-position
    /// map. Distinct dynamic views (this video, that search) get distinct keys.
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

    /// The shared scroll-offset cell for a route's view (created on first use).
    /// Handing it to the view's `Scrollable` preserves the scroll across
    /// rebuilds and restores it when the view is returned to via Back.
    fn scroll_cell_for(&self, route: &Route) -> ScrollCell {
        let key = Self::view_key(route);
        self.scroll_cells
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| Rc::new(Cell::new((0.0, 0.0))))
            .clone()
    }

    /// Navigate to `route`: record it on the toolkit nav stack (so Back can
    /// return) and mirror it onto the live route.
    fn navigate_to(&mut self, route: Route) {
        if self.nav.current() != &route {
            self.nav.push(route.clone());
        }
        self.model.route = route;
    }

    /// Pop the nav stack (Back). Returns true when it navigated somewhere.
    fn go_back(&mut self) -> bool {
        if let Some(route) = self.nav.back().cloned() {
            self.model.route = route;
            true
        } else {
            false
        }
    }

    /// Whether any real image (thumbnail / avatar / banner) is still loading, so
    /// the app keeps redrawing/idle-syncing until each decoded image lands.
    fn images_pending(&self) -> bool {
        self.images.as_ref().is_some_and(|s| s.pending() > 0)
    }

    /// If the current route is a watch page we haven't started playing yet, ask
    /// the worker to resolve + decode a real stream for it — exactly once per
    /// watch-open. Online-only, so the offline `--shots`/test paths never spawn
    /// the worker or reach the network. Opening a *different* video re-arms this
    /// and drops the previous video's frame so the poster shows until the new
    /// stream decodes.
    fn ensure_playback(&mut self) {
        if !self.online {
            return;
        }
        match self.model.route.clone() {
            Route::Watch(id) => {
                if self.playing_id.as_deref() == Some(id.as_str()) {
                    return;
                }
                self.playing_id = Some(id.clone());
                // A fresh resolution is pending; drop the previously-loaded
                // source so a new video starts clean once its URL lands.
                self.player_video = None;
                self.player.borrow_mut().pause();
                self.request_live(Request::PlayVideo(id));
            }
            // Left the watch page: pause the engine so audio stops and it isn't
            // still streaming a video the user is no longer watching.
            _ => {
                if self.playing_id.take().is_some() {
                    self.player_video = None;
                    self.player.borrow_mut().pause();
                }
            }
        }
    }

    /// Per-frame playback bookkeeping: mirror the engine's playhead onto the
    /// model (so `SponsorBlock` auto-skip + autoplay-next key off the real
    /// position), auto-skip enabled segments by seeking the engine, and advance
    /// to the next recommendation when the stream ends. Returns `true` only when
    /// something *visible* changed (a skip notice or an autoplay navigation) so
    /// it never dirties the tree merely because the playhead advanced. A no-op
    /// off the watch route or before a stream has loaded.
    fn tick_playback(&mut self) -> bool {
        let Route::Watch(id) = self.model.route.clone() else {
            return false;
        };
        if self.player_video.as_deref() != Some(id.as_str()) {
            return false;
        }
        // Until a source is open the engine has no playhead to follow: an
        // auto-skip from the zero position would seek nowhere and fire again
        // on every frame.
        if !self.player.borrow().has_media() {
            return false;
        }
        // Short borrow: read the engine's position/duration/playing, then drop
        // the borrow before any seek (auto-skip re-borrows).
        let (pos_ms, dur_ms, playing) = {
            let p = self.player.borrow();
            (p.position_ms(), p.duration_ms(), p.is_playing())
        };
        self.model.playhead = pos_ms as f64 / 1000.0;
        self.model.playing = playing;

        let mut changed = false;
        // Auto-skip runs against the real playhead and seeks the engine.
        if self.apply_auto_skip() {
            changed = true;
        }
        // On-video "Skip" overlay visibility: recompute which (if any) show-mode
        // segment the playhead sits in. A transition (enter/leave) must rebuild
        // the tree so the overlay appears/dismisses — the advancing playhead by
        // itself never dirties it.
        let show_end = self.current_show_skip_end();
        if show_end != self.show_skip_end {
            self.show_skip_end = show_end;
            changed = true;
        }
        // Reaching the end advances to the next "up next" when autoplay is on.
        // `duration_ms` is the media duration once known; treat within 250ms of
        // the end (or an explicitly non-playing engine at the end) as finished.
        let known_dur = dur_ms > 0;
        let at_end = known_dur && pos_ms + 250 >= dur_ms;
        if at_end && self.maybe_autoplay_next() {
            changed = true;
        }
        changed
    }

    /// Apply one player-setting interaction drained from the shared player. The
    /// resolution choice round-trips into the model's preferred quality; other
    /// settings are ignored for now. This is the production drain path (called
    /// from `sync`), factored out so it is directly testable.
    fn apply_player_setting_event(&mut self, ev: ui::widget::video_player::PlayerSettingEvent) {
        use ui::widget::video_player::PlayerSettingEvent as E;
        match ev {
            E::Chose { id, index }
                if id == "resolution"
                    && QUALITIES
                        .get(index)
                        .is_some_and(|q| self.model.settings.preferred_quality != *q) =>
            {
                if let Some(q) = QUALITIES.get(index) {
                    self.model.settings.preferred_quality = (*q).to_string();
                    self.structure_dirty = true;
                }
            }
            E::Activated { id } if id == screens::SKIP_SETTING => {
                if let Some(end) = self.current_show_skip_end() {
                    self.skip_to(end);
                    self.structure_dirty = true;
                }
            }
            _ => {}
        }
    }

    /// Load a resolved, directly-playable stream URL into the shared player
    /// engine and start it — the real path both the worker's
    /// [`Update::StreamResolved`] and the transport tests take. A resolution for
    /// a video that is no longer the open watch page is dropped (returns `false`)
    /// so audio never bleeds across pages.
    fn load_resolved_stream(
        &mut self,
        video: String,
        url: String,
        has_audio: bool,
        quality: &str,
    ) -> bool {
        if !matches!(&self.model.route, Route::Watch(id) if *id == video) {
            return false;
        }
        // Now-playing metadata for the desktop media widget (MPRIS): title,
        // channel and the real thumbnail, built before we take the player borrow.
        let now_playing = self.model.video(&video).map(|v| {
            let dearrow = self.model.settings.dearrow_titles_on();
            ui::widget::video_player::NowPlaying {
                track_id: v.id.clone(),
                title: v.display_title(dearrow).to_string(),
                artist: v.channel_name.clone(),
                art_url: v.thumbnail_url.clone(),
            }
        });
        {
            let mut p = self.player.borrow_mut();
            p.open(media::Source::Remote { url });
            p.play();
            if let Some(np) = now_playing {
                p.set_now_playing(np);
            }
        }
        // Bring up MPRIS once, in the live app, so media keys + the system media
        // widget can drive this player and show its now-playing.
        if self.online && !self.mpris_started {
            self.player
                .borrow_mut()
                .enable_mpris("tilo-tube", "Tilo Tube");
            self.mpris_started = true;
        }
        self.player_video = Some(video);
        self.model.playing = true;
        // No debug text under the player — the picture playing IS the feedback.
        // Quality/audio provenance is a developer diagnostic, so it goes to the
        // log, not an on-screen toast.
        tracing::debug!(%quality, has_audio, "stream loaded into player");
        true
    }

    /// Resolve the current watch video's duration, if any.
    fn current_duration(&self) -> Option<f64> {
        match &self.model.route {
            Route::Watch(id) => self.model.video(id).map(|v| f64::from(v.duration)),
            _ => None,
        }
    }

    /// The end (seconds) of the show-mode `SponsorBlock` segment the playhead is
    /// currently inside, if any — what the on-video "Skip" overlay offers.
    /// `None` off the watch route, when `SponsorBlock` is off, or when no show-mode
    /// segment contains the playhead.
    fn current_show_skip_end(&self) -> Option<f64> {
        if !self.model.settings.sponsorblock_enabled {
            return None;
        }
        let Route::Watch(id) = &self.model.route else {
            return None;
        };
        let v = self.model.video(id)?;
        v.sponsor
            .iter()
            .find(|s| {
                self.model.settings.should_show_skip(s.category) && s.contains(self.model.playhead)
            })
            .map(|s| s.end)
    }

    /// Skip a segment by hand: jump the model and engine playhead to `to` and
    /// confirm with a toast.
    fn skip_to(&mut self, to: f64) {
        if let Some(d) = self.current_duration() {
            let to = to.clamp(0.0, d);
            self.model.playhead = to;
            self.player.borrow_mut().seek_to_ms((to * 1000.0) as u64);
            self.model.push_notice(t_args(
                "tube-notice-skipped-to",
                &[arg("time", crate::util::clock(to))],
            ));
        }
    }

    /// Show each notice pushed since the last frame as a toast, oldest first,
    /// and let the toasts that have been read expire.
    fn show_new_notices(&mut self) {
        use std::hash::{Hash, Hasher};
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        self.model.notices.first().hash(&mut identity);
        let current = identity.finish();
        if current != self.toasted {
            if let Some(notice) = self.model.notices.first() {
                self.toasts.push(notice.clone(), ToastKind::Info);
                tracing::debug!(target: "tube::share", at = ?ui::time::now(), count = self.toasts.len(), "notice toast created");
            }
            self.toasted = current;
        }
        let before = self.toasts.len();
        let now = ui::time::now();
        self.toasts.advance(now);
        if self.toasts.len() < before {
            tracing::debug!(target: "tube::share", at = ?now, before, remaining = self.toasts.len(), "notice toast expired");
        }
    }

    /// If the playhead sits inside an enabled `SponsorBlock` segment, jump the
    /// engine (and the model) to its end and log the skip. Returns `true` when a
    /// skip actually happened. Runs both as the user scrubs and each frame from
    /// [`Self::tick_playback`] against the real engine playhead.
    fn apply_auto_skip(&mut self) -> bool {
        if !self.model.settings.sponsorblock_enabled {
            return false;
        }
        let Route::Watch(id) = self.model.route.clone() else {
            return false;
        };
        let (seg_end, label) = {
            let Some(v) = self.model.video(&id) else {
                return false;
            };
            let mut found = None;
            for s in &v.sponsor {
                // Only Auto-skip-mode categories skip on their own; Show-mode
                // segments wait for the manual skip button.
                if self.model.settings.should_auto_skip(s.category)
                    && s.contains(self.model.playhead)
                {
                    found = Some((
                        s.end,
                        t_args(
                            "tube-notice-skipped-segment",
                            &[
                                arg("segment", s.category.label()),
                                arg("start", crate::util::clock(s.start)),
                                arg("end", crate::util::clock(s.end)),
                            ],
                        ),
                    ));
                    break;
                }
            }
            match found {
                Some(f) => f,
                None => return false,
            }
        };
        self.model.playhead = seg_end;
        // Seek the real stream past the segment (no-op when no source is loaded).
        self.player
            .borrow_mut()
            .seek_to_ms((seg_end * 1000.0) as u64);
        self.model.push_notice(label);
        true
    }

    /// Open a video's watch page: record history, route to it, reset the
    /// playhead and start playing, and kick off metadata/branding fetches.
    /// Shared by the card and search-result open paths and by autoplay-next so they stay
    /// in lockstep.
    fn open_video(&mut self, id: String) {
        let watched_at_ms = ui::time::unix_now_ms();
        self.edit_history(&WatchHistory::RECORD.with((id.clone(), watched_at_ms)));
        self.navigate_to(Route::Watch(id.clone()));
        self.model.playhead = 0.0;
        self.model.playing = true;
        // SponsorBlock, DeArrow and dislikes come from the network, so an
        // offline window (a capture, a staged test) asks for none of them.
        self.request_live(Request::Branding(id.clone()));
        // Fetch the uploader's real avatar + subscriber count when we only hold a
        // name stub (feed/search items don't carry the channel's avatar or subs),
        // so the watch-page channel row shows a real photo + count rather than an
        // initials chip and "0 subscribers".
        if let Some(cid) = self.model.video(&id).map(|v| v.channel_id.clone()) {
            self.request_channel_if_stub(&cid);
        }
        self.request_live(Request::VideoMeta(id));
    }

    /// Ask the worker for a channel page (avatar + subscriber count + uploads)
    /// when the store only holds a bare name stub for it. Online-only.
    fn request_channel_if_stub(&mut self, channel_id: &str) {
        if channel_id.is_empty() || self.requested_channels.contains(channel_id) {
            return;
        }
        let stub = self
            .model
            .channel(channel_id)
            .is_none_or(|c| c.avatar_url.is_empty() || c.subscribers == 0);
        if stub {
            self.requested_channels.insert(channel_id.to_string());
            self.request_live(Request::Channel(channel_id.to_string()));
        }
    }

    /// If autoplay is on and the playhead has reached the end of the current
    /// video, advance to the first "up next" recommendation — the real code
    /// path that consumes the [`Settings::autoplay`] flag. Reaching the end
    /// happens when the engine's stream runs out (mirrored each frame by
    /// [`Self::tick_playback`]) or when a test seeks the playhead to the finish.
    /// Returns `true` when it navigated to a new video.
    fn maybe_autoplay_next(&mut self) -> bool {
        if !self.model.settings.autoplay {
            return false;
        }
        let Route::Watch(id) = self.model.route.clone() else {
            return false;
        };
        let Some(dur) = self.model.video(&id).map(|v| f64::from(v.duration)) else {
            return false;
        };
        if self.model.playhead < dur - 0.001 {
            return false;
        }
        let next = self
            .model
            .related_display(&id)
            .first()
            .map(|v| v.id.clone());
        if let Some(next) = next
            && next != id
        {
            self.open_video(next);
            return true;
        }
        false
    }

    /// Apply one drained action. Returns true if the model changed enough to
    /// warrant a structural rebuild.
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per Action variant; the length is the number of actions the \
                  screens push, not complexity in any one arm"
    )]
    fn apply(&mut self, action: Action) -> bool {
        match action {
            Action::ToggleSidebar => {
                // Flip the rail relative to its current resolved openness, so
                // the hamburger always does the visually-expected thing at the
                // current width.
                let now = self.sidebar_currently_open();
                self.sidebar_open = Some(!now);
                self.shell.toggle_sidebar();
            }
            Action::Navigate(route) => {
                if let Route::Search(q) = &route {
                    q.clone_into(&mut self.model.search_query);
                    self.search_query.set(q.clone());
                }
                self.navigate_to(route);
                self.dismiss_overlay_rail();
            }
            Action::OpenVideo(id) => {
                self.open_video(id);
                self.dismiss_overlay_rail();
            }
            Action::OpenChannel(id) => {
                self.navigate_to(Route::Channel(id.clone()));
                self.dismiss_overlay_rail();
                self.request_live(Request::Channel(id));
            }
            Action::OpenPlaylist(id) => {
                self.navigate_to(Route::Playlist(id.clone()));
                self.dismiss_overlay_rail();
                self.request_live(Request::PlaylistPage(id));
            }
            Action::ToggleSearchType(t) => {
                self.model.search_type_filter.click(t);
            }
            Action::Back => {
                self.go_back();
            }
            Action::Seek(frac) => {
                if let Some(d) = self.current_duration() {
                    self.model.playhead = (frac * d).clamp(0.0, d);
                    // Seek the real stream (no-op when no source is loaded yet).
                    self.player
                        .borrow_mut()
                        .seek_to_ms((self.model.playhead * 1000.0) as u64);
                    self.apply_auto_skip();
                    self.maybe_autoplay_next();
                }
            }
            Action::TogglePlay => {
                let _ = self.perform(TubeEffect::TogglePlayback);
            }
            Action::SkipSegment(to) => {
                // Manual skip button for a Show-mode segment: jump the model +
                // engine playhead to the segment end and confirm with a toast.
                if let Some(d) = self.current_duration() {
                    let to = to.clamp(0.0, d);
                    self.model.playhead = to;
                    self.player.borrow_mut().seek_to_ms((to * 1000.0) as u64);
                    self.model
                        .push_notice(format!("Skipped to {}", crate::util::clock(to)));
                }
            }
            Action::SubmitSearch => {
                let q = self.search_query.get();
                q.clone_into(&mut self.model.search_query);
                self.model.search_error = None;
                self.navigate_to(Route::Search(q.clone()));
                if !q.is_empty() {
                    // Show the results skeleton while the live fetch is in flight
                    // (online only — the hermetic path resolves locally at once).
                    if self.online {
                        self.model.search_loading = true;
                    }
                    self.request(Request::Search { query: q });
                }
            }
            Action::Share(id) => {
                tracing::debug!(target: "tube::share", video = %id, route = ?self.model.route, at = ?ui::time::now(), "share action dispatched");
                // Share belongs to the open watch page. Refuse a stale leaf
                // event rather than copying a link for a video no longer shown.
                if matches!(&self.model.route, Route::Watch(current) if current == &id) {
                    let _ = self.perform(TubeEffect::Share { video: id });
                }
            }
            Action::OpenSearch => {
                self.search_requested = true;
                return false;
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

    /// Start a bounded background request without ever stalling UI input.
    /// Returns false only when the worker could not accept the newest intent;
    /// in that case [`Self::request_rejected`] has already made the outcome
    /// visible and cleared any misleading loading state.
    fn request(&mut self, req: Request) -> bool {
        if self.worker.is_none() {
            self.worker = Some(Worker::spawn());
        }
        if let Some(w) = &self.worker
            && let Err(error) = w.request(req)
        {
            let (rejected, stopped) = match error {
                std::sync::mpsc::TrySendError::Full(request) => (request, false),
                std::sync::mpsc::TrySendError::Disconnected(request) => (request, true),
            };
            self.request_rejected(&rejected, stopped);
            return false;
        }
        true
    }

    /// Make bounded-worker saturation explicit. This is a local, retryable
    /// condition, not an offline/network failure, so never falsify the source
    /// provenance while reporting it.
    fn request_rejected(&mut self, request: &Request, stopped: bool) {
        const BUSY: &str = "Too many requests are pending. Please try again.";
        const STOPPED: &str = "Tube can't reach its sources here; it is offline.";
        let message = if stopped { STOPPED } else { BUSY };
        if stopped {
            self.model.data_source = crate::model::DataSource {
                live: false,
                label: "offline".into(),
            };
        }
        match request {
            Request::Search { query } => {
                if self.model.search_query == *query {
                    self.model.search_loading = false;
                    self.model.search_error = Some(message.to_string());
                }
            }
            Request::Feed => {
                self.model.feed_loading = false;
                self.model.feed_error = Some(message.to_string());
            }
            Request::Branding(_)
            | Request::VideoMeta(_)
            | Request::Channel(_)
            | Request::PlaylistPage(_)
            | Request::PlayVideo(_) => self.model.push_notice(message),
        }
        self.structure_dirty = true;
    }

    /// Request live `InnerTube` data (feed / video detail / channel), but only
    /// when this instance is online. The offline `--shots` and test paths call
    /// through the same navigation code, so gating here keeps them from ever
    /// spawning the worker or reaching the network.
    fn request_live(&mut self, req: Request) {
        if self.online {
            self.request(req);
        }
    }

    /// Fold any worker results into the model. Returns true if anything
    /// changed (→ rebuild).
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per worker Update variant; the length is the number of \
                  update kinds, not accidental complexity in any one arm"
    )]
    fn pump(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let updates = worker.poll();
        if updates.is_empty() {
            return false;
        }
        let mut changed = false;
        // Channels whose real avatar + subscriber count we still need to fetch
        // (a video-detail response tells us the uploader id but not its photo).
        // Collected here and fired after the borrow of `self.worker` ends.
        let mut fetch_channels: Vec<String> = Vec::new();
        for u in updates {
            match u {
                Update::Sponsor(id, segs) => {
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == id) {
                        v.sponsor = segs;
                        changed = true;
                    }
                }
                Update::DeArrow {
                    video,
                    title,
                    thumbnail_url,
                } => {
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == video) {
                        if title.is_some() {
                            v.dearrow_title = title;
                        }
                        v.dearrow_thumb = thumbnail_url.is_some();
                        changed = true;
                    }
                    if let Some(url) = thumbnail_url
                        && !url.is_empty()
                    {
                        self.model.dearrow_thumb_url.insert(video, url);
                        changed = true;
                    }
                }
                Update::Dislikes { video, count } => {
                    self.model.dislikes.insert(video, count);
                    changed = true;
                }
                Update::Comments { video, comments } => {
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == video) {
                        v.comments = comments;
                        changed = true;
                    }
                }
                Update::SearchResults {
                    query,
                    videos,
                    playlists,
                    channels,
                } => {
                    // A response landed (possibly empty) — drop the skeleton.
                    self.model.search_loading = false;
                    self.model.search_error = None;
                    // Record the backend's exact ordered result set for this
                    // query (so the search screen renders it verbatim), and
                    // fold the videos + uploader stubs into the catalogue.
                    let ids: Vec<String> = videos.iter().map(|v| v.id.clone()).collect();
                    for nv in videos {
                        self.model
                            .ensure_channel_stub(&nv.channel_id, &nv.channel_name);
                        self.model.adopt_channel_avatar(
                            &nv.channel_id,
                            &nv.channel_name,
                            &nv.channel_avatar_url,
                        );
                        self.model.upsert_video(nv);
                    }
                    let playlist_ids: Vec<String> =
                        playlists.iter().map(|p| p.id.clone()).collect();
                    for p in playlists {
                        self.model
                            .ensure_channel_stub(&p.channel_id, &p.channel_name);
                        self.model.upsert_playlist(p);
                    }
                    // Creator rows: fold in without clobbering a richer channel
                    // record (a full channel page carries a banner a search row
                    // doesn't), taking the avatar / subs / handle when missing.
                    let channel_ids: Vec<String> = channels.iter().map(|c| c.id.clone()).collect();
                    for c in channels {
                        // A creator row's avatar also belongs to every video card by
                        // that creator (matched by id OR normalized name) — stamp it
                        // there so the dominant channel's cards get the photo even
                        // when the video items didn't carry it.
                        self.model
                            .adopt_channel_avatar(&c.id, &c.name, &c.avatar_url);
                        if let Some(ex) = self.model.channels.iter_mut().find(|x| x.id == c.id) {
                            if ex.avatar_url.is_empty() {
                                ex.avatar_url = c.avatar_url;
                            }
                            if ex.subscribers == 0 {
                                ex.subscribers = c.subscribers;
                            }
                            if ex.handle == ex.id && !c.handle.is_empty() {
                                ex.handle = c.handle;
                            }
                            ex.verified = ex.verified || c.verified;
                        } else {
                            self.model.channels.push(c);
                        }
                    }
                    // Bridge any two records describing the same creator so every
                    // card resolves an avatar one of them carries.
                    self.model.reconcile_channel_avatars();
                    query.clone_into(&mut self.model.search_hits_query);
                    self.model.search_hits = ids;
                    self.model.search_playlist_hits = playlist_ids;
                    self.model.search_channel_hits = channel_ids;
                    if matches!(&self.model.route, Route::Search(_)) {
                        self.model.route = Route::Search(query);
                    }
                    // Progressive avatar coverage: for a bounded set of result
                    // uploaders that still lack a real avatar, pull their channel
                    // page (avatar + subs) so cards fill in real creator photos over
                    // the next frames. Guarded (`request_channel_if_stub`) so it
                    // never re-requests, and capped so a search can't fan out into
                    // dozens of channel fetches.
                    let mut seen = HashSet::new();
                    let stub_cids: Vec<String> = self
                        .model
                        .search_hits
                        .iter()
                        .filter_map(|id| self.model.video(id))
                        .map(|v| v.channel_id.clone())
                        .filter(|cid| {
                            !cid.is_empty()
                                && seen.insert(cid.clone())
                                && self
                                    .model
                                    .channel(cid)
                                    .is_none_or(|c| c.avatar_url.is_empty())
                        })
                        .take(6)
                        .collect();
                    for cid in stub_cids {
                        fetch_channels.push(cid);
                    }
                    changed = true;
                }
                Update::PlaylistPage { playlist, videos } => {
                    for v in videos {
                        self.model
                            .ensure_channel_stub(&v.channel_id, &v.channel_name);
                        self.model
                            .note_channel_avatar(&v.channel_id, &v.channel_avatar_url);
                        self.model.upsert_video(v);
                    }
                    self.model
                        .ensure_channel_stub(&playlist.channel_id, &playlist.channel_name);
                    self.model.upsert_playlist(*playlist);
                    changed = true;
                }
                Update::Feed(vids) => {
                    // Real home/trending catalogue — replace whatever was there.
                    self.model.adopt_live_catalogue(vids, "live (rustypipe)");
                    self.model.feed_loading = false;
                    self.model.feed_error = None;
                    changed = true;
                }
                Update::VideoMeta { video, related } => {
                    let id = video.id.clone();
                    let channel_id = video.channel_id.clone();
                    self.model
                        .ensure_channel_stub(&video.channel_id, &video.channel_name);
                    self.model.adopt_channel_avatar(
                        &video.channel_id,
                        &video.channel_name,
                        &video.channel_avatar_url,
                    );
                    self.model.upsert_video(*video);
                    // Now that we know the uploader, pull its avatar + subs if we
                    // only have a name stub (drives the watch-page channel row).
                    if !channel_id.is_empty()
                        && self
                            .model
                            .channel(&channel_id)
                            .is_none_or(|c| c.avatar_url.is_empty() || c.subscribers == 0)
                    {
                        fetch_channels.push(channel_id);
                    }
                    let ids: Vec<String> = related.iter().map(|v| v.id.clone()).collect();
                    for rv in related {
                        self.model
                            .ensure_channel_stub(&rv.channel_id, &rv.channel_name);
                        self.model.adopt_channel_avatar(
                            &rv.channel_id,
                            &rv.channel_name,
                            &rv.channel_avatar_url,
                        );
                        self.model.upsert_video(rv);
                    }
                    self.model.reconcile_channel_avatars();
                    if !ids.is_empty() {
                        self.model.related_of = id;
                        self.model.related_hits = ids;
                    }
                    changed = true;
                }
                Update::ChannelPage { channel, uploads } => {
                    // Stamp the fetched avatar onto every record for this creator
                    // (matched by id OR normalized name) BEFORE upsert, so the
                    // video cards resolve it even when the channel page's returned
                    // id differs from the search item's channel_id — the exact
                    // reason the dominant channel kept showing initials.
                    self.model.adopt_channel_avatar(
                        &channel.id,
                        &channel.name,
                        &channel.avatar_url,
                    );
                    self.model.upsert_channel(*channel);
                    for uv in uploads {
                        self.model
                            .ensure_channel_stub(&uv.channel_id, &uv.channel_name);
                        self.model.adopt_channel_avatar(
                            &uv.channel_id,
                            &uv.channel_name,
                            &uv.channel_avatar_url,
                        );
                        self.model.upsert_video(uv);
                    }
                    self.model.reconcile_channel_avatars();
                    changed = true;
                }
                Update::Status { live, label } => {
                    // A terminal offline/failure status means no live feed is
                    // coming — stop showing the Home skeleton and let the empty
                    // state explain it. "connecting…" keeps the skeleton up.
                    if !live && label.to_lowercase().contains("offline") {
                        self.model.feed_loading = false;
                        self.model.search_loading = false;
                    }
                    if self.model.data_source.live != live || self.model.data_source.label != label
                    {
                        self.model.data_source = crate::model::DataSource { live, label };
                        changed = true;
                    }
                }
                Update::StreamResolved {
                    video,
                    url,
                    has_audio,
                    quality,
                } => {
                    if self.load_resolved_stream(video, url, has_audio, &quality) {
                        changed = true;
                    }
                }
                Update::Note(n) => {
                    // Worker status notes (backend ready, stream host, seed
                    // failures, …) are developer diagnostics — log them, don't
                    // clutter the watch page with debug toasts. Genuine
                    // user-facing feedback (auto-skip, link copied) is pushed
                    // as a notice directly from its action, not via `Note`.
                    tracing::debug!(note = %n, "tube worker note");
                }
            }
        }
        // Fire the deferred channel fetches now the `self.worker` borrow is done.
        for cid in fetch_channels {
            self.request_channel_if_stub(&cid);
        }
        changed
    }
}

impl FullscreenProvider for TubeApp {
    fn fullscreen_intent(&self) -> Option<bool> {
        FullscreenProvider::fullscreen_intent(&*self.player.borrow())
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        FullscreenProvider::fullscreen_params(&*self.player.borrow())
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        FullscreenProvider::on_fullscreen_changed(&mut *self.player.borrow_mut(), active);
    }
}

impl TubeApp {
    /// The videos, creators, playlists and watch history shown here, as
    /// search entries.
    pub(crate) fn search_entries_of_model(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        crate::search_type::model_entries(&self.model)
    }
}

impl Widget for TubeApp {
    ui::provides!(dyn ui::widget::FullscreenProvider);

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
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
        // First live frame: spawn the fetch worker and pull the home/trending
        // feed. One-shot (guarded) and online-only, so the offscreen path never
        // reaches here with `online` set.
        if self.online && !self.kicked_off {
            self.kicked_off = true;
            self.request_live(Request::Feed);
        }
        // A persisted/boot route that is already a watch page starts playing on
        // the first live sync (guarded so it fires once).
        self.ensure_playback();
        self.show_new_notices();
        self.shell.sync(state, path);
        // The shell's Back chip queues on the bus; go back this frame.
        if self.apply_queued() {
            self.structure_dirty = true;
        }
        // External media-control commands (media keys / the system media widget)
        // → the shared player, so hardware keys drive playback.
        self.player.borrow_mut().pump_media_controls();
        // App-registered player settings the user changed on the player itself
        // (the resolution selector now lives ON the player). Round-trip each
        // back into the model so the preferred quality persists.
        let setting_events = self.player.borrow_mut().take_setting_events();
        for ev in setting_events {
            self.apply_player_setting_event(ev);
        }
        // The watch page's player pumped its engine in the sync above; now mirror
        // the engine playhead onto the model and run SponsorBlock auto-skip /
        // autoplay-next against the real position. Only flags a rebuild when a
        // skip notice or an autoplay navigation actually happens (not every
        // advancing frame), so a playing video repaints without churning the tree.
        if self.tick_playback() {
            self.structure_dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // A host may lay out after every input event. Rebuilding the screen
        // then would drop the hover and press a click is made of, so it is
        // rebuilt only when the structure, the theme or the width changed.
        let key = {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            format!("{theme:?}").hash(&mut hasher);
            self.rect.width().to_bits().hash(&mut hasher);
            self.content_width(theme).to_bits().hash(&mut hasher);
            hasher.finish()
        };
        if !self.shell_built
            || self.shell_stale
            || self.structure_dirty
            || self.shell_key != Some(key)
        {
            self.rebuild_shell(theme);
            self.shell_stale = false;
            self.structure_dirty = false;
            self.shell_key = Some(key);
        }
        let node = self.shell.build_layout(taffy, theme, node_map);
        self.child_node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = crate::widgets::node_layout(taffy, node);
        let rect = ui::widget::placed(layout, origin);
        // A new window size re-sizes the width-driven watch page.
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
            .draw(scene, transform, text_renderer, theme, presence);
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
        if !self.toasts.is_empty() {
            self.toasts
                .place_bottom_right(self.rect, f64::from(theme.tokens.spacing.s4));
            self.toasts
                .draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        self.shell.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the Widget::handle_event trait method fans out on WindowEvent kind; each \
                  branch is a short, independent dispatch to an already-extracted helper"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Back-navigation via the keyboard: Alt+Left / the BrowserBack key pop the
        // nav stack (the Back button, Escape below and the edge-swipe do too).
        // Only when there is history.
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
            if is_back && self.nav.can_go_back() {
                action::push(&self.bus, Action::Back);
                *ctx.handled = true;
            }
        }

        // Feed raw pointer events to the toolkit's edge-swipe back gesture. It
        // only observes (never consumes), so children still get the events; a
        // recognized left-edge rightward swipe pushes Back — swipe-nav lives at
        // the toolkit layer with no app-specific gesture code.
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_pointer =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if let Some(ui::NavAction::Back) = self.back_gesture.pointer_move(self.last_pointer)
                    && self.nav.can_go_back()
                {
                    action::push(&self.bus, Action::Back);
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

        if !self.toasts.is_empty() {
            self.toasts.handle_event(event, ctx);
        }
        if !*ctx.handled {
            self.shell.handle_event(event, ctx);
        }

        // Esc goes back, unless a control inside used it.
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
            && self.nav.can_go_back()
        {
            action::push(&self.bus, Action::Back);
            *ctx.handled = true;
        }

        // Drain & apply queued actions.
        let mut changed = self.apply_queued();
        // If a just-applied action opened a watch page, kick off real playback
        // (resolve + decode) for it — once. No-op off the watch route / offline.
        self.ensure_playback();
        // Fold any ready worker results (live data + decoded frames).
        changed |= self.pump();

        if changed {
            self.structure_dirty = true;
        }
    }

    fn on_external_event(
        &mut self,
        _payload: &str,
        _state: &mut ui::widget::Edits<'_>,
        _root: &ModelPath,
    ) {
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        // Fold any results that arrived between events so live data still
        // lands when the user is idle.
        if self.pump() {
            dirty = true;
        }
        // Image loads must not force a structural rebuild: thumbnails, avatars
        // and banners resolve their decoded image *lazily* in their own `sync`
        // (they hold an `ImageStore` handle + URL) and repaint via
        // `needs_redraw` when it lands, so a photo finishing download costs a
        // cheap redraw rather than a whole-tree rebuild + layout re-solve on
        // every one of ~160 completions. `needs_idle_sync` / `needs_animation_frame`
        // keep frames flowing while images are pending so each `sync` gets its
        // chance to fold the freshly-decoded image in.
        self.shell_stale |= dirty;
        // The shell's own structural changes (a swapped screen, the sidebar
        // toggle) need a new layout, not a new screen.
        dirty | self.shell.take_structure_dirty()
    }

    fn needs_redraw(&self) -> bool {
        self.structure_dirty
            || self.images_pending()
            || !self.toasts.is_empty()
            || !self.shell_built
            || self.shell.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.shell.mark_drawn();
    }

    fn is_layout_dirty(&self) -> bool {
        !self.shell_built || self.shell.is_layout_dirty()
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep syncing while a fetch is outstanding so results fold in, and
        // while images are still loading so freshly-decoded thumbnails/avatars
        // repaint the moment they land.
        self.worker.is_some() || self.images_pending()
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the event loop pumping redraws (and thus `sync`, which pumps the
        // media engine) ONLY while a watch stream is still *resolving* — armed
        // (`playing_id`) but with no source loaded into the engine yet
        // (`player_video` not this id) — so the worker's `StreamResolved` folds
        // in without waiting for stray input. Once a source is loaded, the
        // player's own `needs_animation_frame` (via the shell — is_playing or
        // controls fading) governs, so a **paused-but-loaded** video lets the
        // loop go idle instead of spinning at 60 Hz forever (which kept the app
        // needlessly busy and made scrolling feel heavy). Redraws are
        // vsync-throttled by the renderer regardless.
        let resolving = self.online
            && matches!(&self.model.route, Route::Watch(id)
                if self.playing_id.as_deref() == Some(id.as_str())
                    && self.player_video.as_deref() != Some(id.as_str()));
        resolving
            || self.images_pending()
            || !self.toasts.is_empty()
            || self.shell.needs_animation_frame()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.shell]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.shell]
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let route = match &self.model.route {
            Route::Home => "Home".to_string(),
            Route::Trending => "Trending".to_string(),
            Route::Subscriptions => "Subscriptions".to_string(),
            Route::Queue => "Shared watch queue".to_string(),
            Route::Watch(id) => self
                .model
                .video(id)
                .map_or_else(|| "Video".to_string(), |video| video.title.clone()),
            Route::Channel(id) => self
                .model
                .channel(id)
                .map_or_else(|| "Creator".to_string(), |channel| channel.name.clone()),
            Route::Playlist(id) => self
                .model
                .playlist(id)
                .map_or_else(|| "Playlist".to_string(), |playlist| playlist.title.clone()),
            Route::History => "Watch history".to_string(),
            Route::Settings => "Settings".to_string(),
            Route::Search(query) => format!("Search: {query}"),
        };
        let mut node = AccessNode::new(Role::Window)
            .with_name(format!("Tube — {route}"))
            .with_state(AccessState::new().busy(self.model.feed_loading));
        if !self.toasts.is_empty()
            && let Some(announcement) = self.toasts.accessibility()
        {
            node.children.push(announcement);
        }
        Some(node)
    }

    fn name(&self) -> &'static str {
        "TubeApp"
    }

    // Screenshot scenarios and tests reach the app through the framework's
    // surface to read what it shows.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_search_request(&mut self) -> bool {
        std::mem::take(&mut self.search_requested) || self.shell.take_search_request()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::model::Category;
    use automerge::{AutoCommit, ReadDoc};
    use ui::tilo_resource::Resource as _;

    fn fresh() -> (TubeApp, AutoCommit) {
        (TubeApp::new(), AutoCommit::new())
    }

    /// A fresh app whose player uses the deterministic **synthesized** media
    /// engine, so the transport-wiring tests behave identically whether or not
    /// the `media` (real `GStreamer`) backend is compiled in — a real pipeline
    /// can't preroll/seek the fake test URLs, so its position would stay 0.
    fn fresh_synth() -> (TubeApp, AutoCommit) {
        let (app, doc) = fresh();
        app.player
            .borrow_mut()
            .set_engine(media::MediaEngine::new_synthesized());
        (app, doc)
    }

    #[test]
    fn a_staged_catalogue_retires_the_live_worker_and_fetches_nothing() {
        // A live window has spawned its fetch worker by its first frame.
        let mut app = TubeApp::new().online();
        app.worker = Some(Worker::spawn());
        app.stage_model_for_test(crate::sample::model());
        assert!(
            app.worker.is_none(),
            "a staged catalogue must not keep the live worker, whose late results rebuild the page"
        );
        app.open_video("vid_rust_async".into());
        assert!(
            matches!(&app.model.route, Route::Watch(id) if id == "vid_rust_async"),
            "the staged watch page opens"
        );
        assert!(
            app.worker.is_none(),
            "an offline watch page asks the network for no SponsorBlock or DeArrow branding"
        );
    }

    #[test]
    fn a_fresh_share_notice_survives_a_delayed_previous_toast_frame() {
        let (mut app, _doc) = fresh();
        app.model.route = Route::Watch("vid_donut".into());
        app.toasts.push("Previous notice", ToastKind::Info);
        // A frame can resume after the old toast's lifetime elapsed. The new
        // share notice must not inherit the time before its own creation.
        let earlier = ui::time::now()
            .checked_sub(app.toasts.default_ttl() + ui::time::Duration::from_secs(1))
            .expect("the fixture can represent the previous toast frame");
        app.toasts.advance(earlier);
        app.perform(TubeEffect::Share {
            video: "vid_donut".into(),
        })
        .unwrap();
        app.show_new_notices();
        let published = app.accessibility().unwrap();
        assert!(
            published.children.iter().any(|node| {
                node.name.as_deref() == Some("Notifications")
                    && node.value.as_deref().is_some_and(|value| {
                        value.contains("Link copied") && value.contains("vid_donut")
                    })
            }),
            "the fresh share notice must reach accessibility after an old frame stalls"
        );
        assert_eq!(
            app.toasts.len(),
            1,
            "creating a new share notice must not renew the expired previous notice"
        );
    }

    #[test]
    fn controls_without_a_document_change_act_on_this_window() {
        // Opening a video shows it, starts it from the top and records it in
        // the watch history.
        {
            let mut app = TubeApp::new();
            app.apply(Action::OpenVideo("vid_donut".into()));
            assert_eq!(app.model.route, Route::Watch("vid_donut".into()));
            assert_eq!(
                app.model.history.first().map(String::as_str),
                Some("vid_donut")
            );
            assert_eq!(app.model.playhead, 0.0);
            // Forgetting and clearing change the history shown.
            app.perform(TubeEffect::ForgetWatched {
                video: "vid_donut".into(),
            })
            .unwrap();
            assert!(!app.model.history.iter().any(|id| id == "vid_donut"));
            app.perform(TubeEffect::ClearHistory).unwrap();
            assert!(app.model.history.is_empty());
        }
        // Share copies a link for the open video and likes nothing.
        {
            let mut app = TubeApp::new();
            app.apply(Action::OpenVideo("vid_donut".into()));
            app.apply(Action::Share("vid_donut".into()));
            assert!(!app.model.is_liked("vid_donut"));
            assert!(
                app.model
                    .notices
                    .iter()
                    .any(|notice| notice.contains("youtu.be/vid_donut"))
            );
        }
        // A screen an effect names opens; one it does not is refused.
        {
            let mut app = TubeApp::new();
            app.perform(TubeEffect::ShowScreen {
                screen: "settings".into(),
            })
            .unwrap();
            assert_eq!(app.model.route, Route::Settings);
            assert!(
                app.perform(TubeEffect::ShowScreen {
                    screen: "attic".into()
                })
                .is_err()
            );
            assert!(
                app.perform(TubeEffect::OpenVideo {
                    video: "no-such-video".into()
                })
                .is_err()
            );
        }
        // The player forwards its fullscreen intent to the root.
        {
            let (app, _) = fresh_synth();
            assert_eq!(FullscreenProvider::fullscreen_intent(&app), None);
            app.player.borrow_mut().toggle_fullscreen();
            assert_eq!(FullscreenProvider::fullscreen_intent(&app), Some(true));
        }
    }

    #[test]
    fn an_emptied_library_shows_empty() {
        let mut app = TubeApp::new();
        let mut library = Library::default();
        library.likes.insert("vid_donut".into());
        library.queue.insert("vid_donut".into());
        library.comment_likes.insert("vid_donut#1".into());
        app.present_library(&library);
        assert!(app.model.is_liked("vid_donut") && app.model.is_queued("vid_donut"));
        assert!(app.model.comment_likes.contains("vid_donut#1"));

        // A collaborator unliked and unqueued everything.
        library.likes.remove(&"vid_donut".into());
        library.queue.remove(&"vid_donut".into());
        app.present_library(&library);
        assert!(app.model.liked.is_empty(), "an emptied library is shown");
        assert!(app.model.queue.is_empty());
    }

    // --- media-engine transport wiring (the real playback path) ---

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn resolved_streams_drive_the_playback_engine() {
        // Case `resolved_stream_opens_and_plays_the_engine`.
        {
            // The worker's `StreamResolved` path (factored into `load_resolved_stream`)
            // must open the rangeable URL into the shared player's media engine and
            // start it — the exact code path the live app runs.
            let (mut app, _doc) = fresh_synth();
            app.model.route = Route::Watch("vid_donut".into());
            let ok = app.load_resolved_stream(
                "vid_donut".into(),
                "https://inv.example/latest_version?id=vid_donut&itag=18&local=true".into(),
                true,
                "360p",
            );
            assert!(ok, "the resolved stream loaded for the open watch video");
            assert!(
                app.player.borrow().is_playing(),
                "the engine is playing the stream"
            );
            let src = app
                .player
                .borrow()
                .engine()
                .and_then(|e| e.source().cloned());
            match src {
                Some(media::Source::Remote { url }) => {
                    assert!(
                        url.contains("local=true"),
                        "opened the rangeable proxy URL: {url}"
                    );
                }
                other => panic!("expected a remote source on the engine, got {other:?}"),
            }
        }
        // Case `resolved_stream_for_a_different_video_is_dropped`.
        {
            // A late resolution for a since-closed video must not hijack the engine.
            let (mut app, _doc) = fresh_synth();
            app.model.route = Route::Watch("vid_donut".into());
            let ok = app.load_resolved_stream(
                "some_other_id".into(),
                "https://inv.example/x?local=true".into(),
                true,
                "360p",
            );
            assert!(!ok, "a stream for a non-open video is dropped");
            assert!(
                app.player
                    .borrow()
                    .engine()
                    .and_then(|e| e.source())
                    .is_none()
            );
        }
        // Case `toggle_play_drives_the_engine`.
        {
            let (mut app, _) = fresh_synth();
            app.model.route = Route::Watch("vid_donut".into());
            app.load_resolved_stream(
                "vid_donut".into(),
                "https://inv.example/x?local=true".into(),
                true,
                "360p",
            );
            assert!(app.player.borrow().is_playing());
            app.apply(Action::TogglePlay);
            assert!(
                !app.player.borrow().is_playing(),
                "TogglePlay paused the engine"
            );
            assert!(!app.model.playing, "the model mirrors the paused engine");
            app.apply(Action::TogglePlay);
            assert!(
                app.player.borrow().is_playing(),
                "TogglePlay resumed the engine"
            );
            assert!(app.model.playing, "the model mirrors the playing engine");
        }
        // Case `opening_a_watch_page_arms_stream_resolution_on_open`.
        {
            // The resolve/buffer must start when the watch page OPENS — not on a
            // later Play click. `ensure_playback` (run right after the open action
            // drains, and on the boot-into-watch sync) arms it; `playing_id`
            // latching to the opened video proves the `PlayVideo` resolve request
            // went out, so playback is loading before any Play is pressed.
            let (mut app, _) = fresh_synth();
            app.online = true; // online so `ensure_playback` actually resolves
            assert!(app.playing_id.is_none(), "nothing armed before opening");
            app.apply(Action::OpenVideo("vid_donut".into()));
            app.ensure_playback();
            assert_eq!(
                app.playing_id.as_deref(),
                Some("vid_donut"),
                "opening a watch page arms stream resolution (no Play click needed)"
            );
        }
        // Case `watch_animates_while_resolving_then_idles_once_paused_and_loaded`.
        {
            // While the stream is still resolving (armed but no source loaded) the
            // loop must keep pumping so `StreamResolved` folds in. Once a source is
            // loaded and the video is PAUSED, the app must let the event loop idle
            // rather than spin at 60fps forever — the needless busy-loop that made
            // scrolling on a paused-but-loaded watch page feel heavy.
            let (mut app, _doc) = fresh_synth();
            app.online = true;
            app.model.route = Route::Watch("vid_donut".into());
            app.playing_id = Some("vid_donut".into());

            // Resolving: no source loaded yet → keep pumping.
            app.player_video = None;
            assert!(
                app.needs_animation_frame(),
                "while the stream is resolving the loop keeps pumping so it folds in"
            );

            // Loaded + paused → the loop may idle (the paused player asks for none).
            let ok = app.load_resolved_stream(
                "vid_donut".into(),
                "https://inv.example/x?local=true".into(),
                true,
                "360p",
            );
            assert!(
                ok && app.player_video.as_deref() == Some("vid_donut"),
                "source loaded"
            );
            app.player.borrow_mut().pause();
            app.model.playing = false;
            assert!(!app.player.borrow().is_playing(), "precondition: paused");
            assert!(
                !app.needs_animation_frame(),
                "a paused, loaded watch page lets the event loop idle"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn sponsor_segments_and_autoplay_steer_the_engine() {
        // Case `auto_skip_seeks_the_engine_past_a_sponsor_segment`.
        {
            // Auto-skip runs against the real playhead and must seek the engine (not
            // just the model). vid_rust_async has a Sponsor segment 1440..1570.
            let (mut app, _doc) = fresh_synth();
            app.model.route = Route::Watch("vid_rust_async".into());
            app.load_resolved_stream(
                "vid_rust_async".into(),
                "https://inv.example/x?local=true".into(),
                true,
                "360p",
            );
            // Playhead inside the sponsor band.
            app.model.playhead = 1500.0;
            let skipped = app.apply_auto_skip();
            assert!(skipped, "a skip happened");
            assert!(
                app.model.playhead >= 1570.0,
                "model playhead jumped past the segment"
            );
            // The seek reached the engine (its position advanced off the start). The
            // synthesized test engine caps its duration, so the exact target clamps —
            // proving it moved is the honest real-path assertion.
            let engine_pos = app
                .player
                .borrow()
                .engine()
                .map_or(0, media::MediaEngine::position_ms);
            assert!(
                engine_pos > 0,
                "the auto-skip seeked the engine (pos={engine_pos})"
            );
            assert!(app.model.notices.iter().any(|n| n.contains("Sponsor")));
        }
        // Case `seek_into_sponsor_auto_skips_to_segment_end`.
        {
            let (mut app, _) = fresh();
            // vid_rust_async has a Sponsor segment 1440..1570.
            app.apply(Action::OpenVideo("vid_rust_async".into()));
            let dur = f64::from(app.model.video("vid_rust_async").unwrap().duration);
            // Seek to ~1500s (inside the sponsor segment).
            app.apply(Action::Seek(1500.0 / dur));
            // Auto-skip should jump the playhead to the segment end (1570).
            assert!(
                app.model.playhead >= 1570.0,
                "playhead was {}",
                app.model.playhead
            );
            assert!(app.model.notices.iter().any(|l| l.contains("Sponsor")));
        }
        // Case `autoplay_advances_to_next_when_the_video_ends`.
        {
            // Autoplay is on by default; seeking to the very end must advance to
            // the first "up next" recommendation — the real code path that
            // consumes the autoplay flag.
            let (mut app, _) = fresh();
            assert!(app.model.settings.autoplay, "autoplay defaults on");
            app.apply(Action::OpenVideo("vid_rust_async".into()));
            app.apply(Action::Seek(1.0));
            match app.route_for_test() {
                Route::Watch(id) => assert_ne!(
                    id, "vid_rust_async",
                    "autoplay advanced to a different video"
                ),
                other => panic!("expected a watch route, got {other:?}"),
            }
        }
        // Case `autoplay_off_parks_at_the_end_without_advancing`.
        {
            let (mut app, _) = fresh();
            app.model.settings.autoplay = false;
            app.apply(Action::OpenVideo("vid_rust_async".into()));
            app.apply(Action::Seek(1.0));
            assert_eq!(
                app.route_for_test(),
                &Route::Watch("vid_rust_async".into()),
                "with autoplay off, reaching the end stays on the same video"
            );
        }
        // Case `choosing_a_category_mode_sets_it`.
        {
            use crate::model::SkipMode;
            let (mut app, _) = fresh();
            assert_eq!(app.model.settings.mode_of(Category::Intro), SkipMode::Skip);
            app.perform(TubeEffect::SetSegmentHandling {
                category: Category::Intro.api_key().into(),
                mode: SkipMode::Show.key().into(),
            })
            .unwrap();
            assert_eq!(app.model.settings.mode_of(Category::Intro), SkipMode::Show);
            app.perform(TubeEffect::SetSegmentHandling {
                category: Category::Intro.api_key().into(),
                mode: SkipMode::Disabled.key().into(),
            })
            .unwrap();
            assert_eq!(
                app.model.settings.mode_of(Category::Intro),
                SkipMode::Disabled
            );
        }
        // Case `show_mode_segment_does_not_auto_skip_but_manual_skip_seeks`.
        {
            use crate::model::SkipMode;
            let (mut app, _doc) = fresh_synth();
            app.model.route = Route::Watch("vid_rust_async".into());
            app.load_resolved_stream(
                "vid_rust_async".into(),
                "https://inv.example/x?local=true".into(),
                true,
                "360p",
            );
            // Put the Sponsor category into Show mode, then park inside its segment.
            app.model
                .settings
                .categories
                .insert(Category::Sponsor.api_key().to_string(), SkipMode::Show);
            app.model.playhead = 1500.0;
            assert!(!app.apply_auto_skip(), "Show mode must NOT auto-skip");
            assert_eq!(
                app.model.playhead, 1500.0,
                "playhead untouched by auto-skip"
            );
            // The manual skip button jumps to the segment end.
            app.apply(Action::SkipSegment(1570.0));
            assert!(
                app.model.playhead >= 1570.0,
                "manual skip advanced the playhead"
            );
        }
        // Case `the_player_offers_skip_inside_a_show_mode_segment_and_toasts_it`.
        {
            use crate::model::SkipMode;
            let (mut app, _doc) = fresh_synth();
            app.model.route = Route::Watch("vid_rust_async".into());
            app.load_resolved_stream(
                "vid_rust_async".into(),
                "https://inv.example/x?local=true".into(),
                true,
                "360p",
            );
            app.model
                .settings
                .categories
                .insert(Category::Sponsor.api_key().to_string(), SkipMode::Show);
            app.model.playhead = 1500.0;
            let theme = Theme::dark();
            let _ = app.build_screen(&theme);
            assert!(
                app.player.borrow().setting(screens::SKIP_SETTING).is_some(),
                "inside a Show-mode segment the player offers Skip"
            );
            app.apply_player_setting_event(
                ui::widget::video_player::PlayerSettingEvent::Activated {
                    id: screens::SKIP_SETTING.into(),
                },
            );
            assert!(app.model.playhead >= 1570.0, "Skip jumps past the segment");
            app.show_new_notices();
            assert_eq!(app.toasts.len(), 1, "the skip is confirmed by one toast");
            app.show_new_notices();
            assert_eq!(app.toasts.len(), 1, "a notice is toasted once");
            let _ = app.build_screen(&theme);
            assert!(
                app.player.borrow().setting(screens::SKIP_SETTING).is_none(),
                "past the segment the Skip command is gone"
            );
        }
        // Case `show_mode_segment_drives_the_in_video_skip_overlay`.
        {
            use crate::model::{Category, SkipMode};
            // The on-video "Skip" overlay is settings-driven: it appears only for a
            // Show-mode segment the playhead is inside, never for Auto-skip (which
            // jumps on its own) or Off, and never when SponsorBlock is disabled.
            let (mut app, _doc) = fresh();
            app.model.route = Route::Watch("vid_rust_async".into()); // Sponsor 1440..1570
            let key = Category::Sponsor.api_key().to_string();

            app.model
                .settings
                .categories
                .insert(key.clone(), SkipMode::Skip);
            app.model.playhead = 1500.0;
            assert_eq!(
                app.current_show_skip_end(),
                None,
                "auto-skip shows no manual overlay"
            );

            app.model.settings.categories.insert(key, SkipMode::Show);
            assert_eq!(
                app.current_show_skip_end(),
                Some(1570.0),
                "show-mode offers a Skip to the end"
            );

            app.model.playhead = 100.0;
            assert_eq!(
                app.current_show_skip_end(),
                None,
                "no overlay outside the segment"
            );

            app.model.playhead = 1500.0;
            app.model.settings.sponsorblock_enabled = false;
            assert_eq!(
                app.current_show_skip_end(),
                None,
                "master off suppresses the overlay"
            );
        }
    }

    #[test]
    fn library_and_queries_keep_shared_and_local_state_apart() {
        // Case `library_edits_reconcile_without_serializing_device_ui_state`.
        {
            let (mut app, mut doc) = fresh();
            app.model.route = Route::Watch("vid_donut".into());
            app.model.playhead = 42.0;
            ui::framework::testing::dispatcher::<Library>("tube.library")
                .submit_now(
                    &mut ui::widget::Edits::lend(&mut doc),
                    Library::like("vid_donut", true),
                )
                .expect("structured library action applies");
            app.present_library(&Library::read(&doc).unwrap());
            Widget::sync(&mut app, &doc, &automerge::ROOT);
            assert!(app.model.liked.iter().any(|id| id == "vid_donut"));
            assert!(
                doc.get(&automerge::ROOT, "tube_state")
                    .expect("document read")
                    .is_none(),
                "route, playback and fetched catalogue are never mirrored into collaboration"
            );
        }
        // Case `search_query_stays_local_to_each_tube_instance`.
        {
            use automerge::transaction::Transactable;

            let (mut first, doc) = fresh();
            first.search_query.set("rust async".into());
            Widget::sync(&mut first, &doc, &automerge::ROOT);

            assert!(
                doc.get(&automerge::ROOT, "search_query")
                    .expect("document read")
                    .is_none()
            );

            let mut second = TubeApp::new();
            Widget::sync(&mut second, &doc, &automerge::ROOT);
            assert!(second.search_query.get().is_empty());
            assert_eq!(first.search_query.get(), "rust async");

            // A document holding a search key must not hand its value to a new
            // app merely because a collaborator's client emitted it.
            let mut legacy_doc = AutoCommit::new();
            legacy_doc
                .put(&automerge::ROOT, "search_query", "remote query")
                .expect("legacy search seed");
            let mut third = TubeApp::new();
            Widget::sync(&mut third, &legacy_doc, &automerge::ROOT);
            assert!(third.search_query.get().is_empty());
        }
    }

    // --- navigation rail ---

    #[test]
    fn the_rail_adapts_and_navigates_through_published_actions() {
        // Case `route_shot_seeds_the_navigation_authority_with_the_visible_route`.
        {
            let app = TubeApp::new().with_route_shot(Route::Trending);
            assert_eq!(app.model.route, Route::Trending);
            assert_eq!(app.nav.current(), &Route::Trending);
            assert!(
                !app.can_go_back_for_test(),
                "a direct screenshot route has no fabricated prior destination"
            );
        }
        // Case `choosing_a_destination_invokes_its_navigation_action`.
        {
            let mut app = ui::prelude::TestApp::<crate::resource::TubeShell>::new().unwrap();
            app.effect(&TubeEffect::ShowScreen {
                screen: "settings".into(),
            })
            .unwrap();
            app.settle();
            app.with_surface::<TubeApp, _>(|viewer| {
                assert_eq!(viewer.model.route, Route::Settings);
                assert!(viewer.can_go_back_for_test(), "Back returns to Home");
            })
            .expect("the viewer is mounted");
        }
        // The shared shell docks the rail when it fits and exposes a real
        // hamburger-controlled drawer on narrower windows.
        {
            use ui::widget::SidebarPlacement::{Docked, Drawer};
            for (width, placement) in [(1280, Docked), (800, Drawer), (560, Drawer)] {
                let mut framed = ui::headless::Framed::new(
                    ui::State::new(),
                    TubeApp::new(),
                    (width, 800),
                    Theme::dark(),
                );
                assert_eq!(
                    framed.shell.geometry().sidebar,
                    placement,
                    "the shared shell adapts the rail at {width} px"
                );
                if placement == Drawer {
                    assert!(
                        !framed.sidebar_open_for_test(),
                        "the narrow drawer starts closed"
                    );
                    ui::headless::Framed::harness_mut(&mut framed)
                        .click_named("nav-shell-sidebar")
                        .expect("the hamburger opens the narrow rail");
                }
                assert!(
                    framed.sidebar_open_for_test(),
                    "the rail is reachable at {width} px"
                );
            }
        }
    }

    // --- no fabricated content + loading states ---

    #[test]
    fn framework_launch_mounts_the_live_app() {
        // The installed binary launches through the framework, so the surface
        // it mounts must be the online app: no bundled sample catalogue, and
        // a first `sync` that fetches the real feed.
        let app = crate::resource::TubeShell::surface(ui::framework::RunMode::Live);
        assert!(
            app.online,
            "the framework launch must reach the live backend"
        );
        assert!(
            app.model.videos.is_empty() && app.model.channels.is_empty(),
            "the framework launch must not show the bundled sample catalogue"
        );
        assert!(app.model.feed_loading, "cold Home shows a loading skeleton");
    }

    #[test]
    fn online_catalogue_starts_empty_and_tracks_requests() {
        // Case `online_starts_empty_with_no_fabricated_sample`.
        {
            // The real (online) app must NOT present the bundled sample catalogue as
            // if it were real data — it starts empty and shows a loading skeleton.
            let app = TubeApp::new().online();
            assert!(
                app.model.videos.is_empty(),
                "online start must seed no sample videos"
            );
            assert!(
                app.model.channels.is_empty(),
                "online start must seed no sample channels"
            );
            assert!(app.model.subscriptions.is_empty());
            assert!(
                app.model.feed_loading,
                "cold Home shows a loading skeleton, not a blank page"
            );
        }
        // Case `rejected_newest_request_settles_search_and_explains_retry`.
        {
            let mut app = TubeApp::empty();
            app.model.search_query = "rust".into();
            app.model.search_loading = true;
            app.request_rejected(
                &Request::Search {
                    query: "rust".into(),
                },
                false,
            );
            assert!(
                !app.model.search_loading,
                "a refused search must not leave the results waiting"
            );
            assert!(
                app.model
                    .search_error
                    .as_deref()
                    .is_some_and(|message| message.contains("try again"))
            );

            app.model.feed_loading = true;
            app.request_rejected(&Request::Feed, false);
            assert!(!app.model.feed_loading, "do not leave Home loading forever");
            assert!(
                app.model
                    .feed_error
                    .as_deref()
                    .is_some_and(|message| message.contains("try again"))
            );
        }
        // Case `a_palette_search_asks_the_worker`.
        {
            let mut app = TubeApp::new();
            app.online = true;
            app.kicked_off = true;
            app.search_query.set("rust".into());
            app.apply(Action::SubmitSearch);
            assert!(
                app.worker.is_some(),
                "the submitted query reaches the background worker"
            );
            assert!(
                app.model.search_loading,
                "the outstanding result is represented as loading"
            );
            assert_eq!(app.model.search_query, "rust");
            assert_eq!(app.model.route, Route::Search("rust".into()));
        }
        // Case `channel_stub_qualifies_for_an_uploader_fetch`.
        {
            // The watch page's creator avatar + subscriber count come from a channel
            // fetch kicked off when we only hold a name stub. Prove the gate: a bare
            // `ensure_channel_stub` record has no avatar and 0 subscribers (so it
            // qualifies), while a fetched channel would not.
            let mut m = crate::model::TubeModel::default();
            m.ensure_channel_stub("c1", "Chan");
            let stub = m.channel("c1").expect("stub inserted");
            assert!(
                stub.avatar_url.is_empty() && stub.subscribers == 0,
                "a name stub must be recognised as needing an avatar/subs fetch",
            );
        }
        // Case `image_loads_do_not_force_a_structural_rebuild`.
        {
            // Thumbnails / avatars / banners resolve their decoded image LAZILY in
            // their own `sync` and repaint via `needs_redraw` when it lands, so an
            // in-flight image load must NOT make the app report a structural
            // rebuild (a whole-tree rebuild + layout re-solve on every one of
            // ~160 image completions would make the grids churn).
            use std::sync::Arc;
            use std::time::Duration;
            // A fetcher that blocks briefly so a load is deterministically in
            // flight (pending ≥ 1) when we poll.
            let fetcher: ui::image_store::Fetcher = Arc::new(|_: &str| {
                std::thread::sleep(Duration::from_millis(200));
                None
            });
            let store = ui::ImageStore::new(&fetcher);
            let _ = store.get("http://x/a.png");
            assert!(store.pending() >= 1, "a load is in flight");

            let mut app = TubeApp::new().with_image_store(store);
            let doc = AutoCommit::new();
            // The first frame builds the shell; image loads arrive after it.
            let _ = Widget::take_structure_dirty(&mut app);
            app.sync(&doc, &automerge::ROOT);
            assert!(
                !Widget::take_structure_dirty(&mut app),
                "an in-flight image load must NOT queue a whole-tree rebuild — images \
             fold in lazily via sync + redraw, not via a structural rebuild",
            );
        }
    }
}
