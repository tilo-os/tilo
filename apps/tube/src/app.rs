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
//!
//! The window's other subjects sit beside it: the watch page's player
//! (`playback`), the network worker it asks (`requests`), what a still, a bench
//! run or a test stages (`staging`), the bar's slots (`window_bar`), and the
//! suite (`tests`).

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
use crate::model::{Route, TubeModel};
use crate::resource::Library;
use crate::screens;
use crate::widgets::SharedPlayer;
use ui::tilo_resource::{Edit, Invoke};

mod playback;
mod requests;
mod staging;
mod window_bar;

pub(crate) const QUALITIES: [&str; 4] = ["1080p", "720p", "480p", "auto"];

/// The playback speeds the player's menu offers, slowest first.
pub(crate) const PLAYBACK_SPEEDS: [f32; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0];

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
    /// Wakes this window when the worker posts a result; the framework's,
    /// from [`Self::wake_with`]. Detached (waking nothing) until attached.
    wake: ui::WakeHandle,
    #[cfg(feature = "grayjay-sync")]
    device_sync: crate::sync::DeviceSync,
    #[cfg(feature = "grayjay-sync")]
    device_sync_seeded: bool,

    /// The window shell: the navigation rail and the current screen. It lives as long as the app, so the person's sidebar choice
    /// and its motion survive navigation; `build_layout` swaps the screen
    /// into it when the structure, theme or width changed.
    shell: NavShell,
    /// The current screen has been built into `shell` at least once.
    shell_built: bool,
    /// The transient notice ("Link copied", a skipped segment) last shown
    /// as the shell's toast.
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
    /// What the platform search last asked this window for, so its results
    /// can offer to search `YouTube` for it.
    palette_query: String,

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
    /// Where the stream that is about to load should start, in milliseconds:
    /// the place the person left this video. Taken when the resolution lands,
    /// because a seek before the source exists would be lost.
    resume_at_ms: Option<u64>,
    /// The last position written to the person's watch history, as
    /// `(video, whole seconds)`: writes are throttled to a few seconds apart so
    /// a two-hour video is not two hours of history edits, and the final
    /// position is still written when playback stops or the page closes.
    progress_written: Option<(String, u64)>,

    /// Async image loader/cache for real thumbnails / avatars / banners, keyed
    /// by URL. `Some` when online (or seeded for a real-image `--shots` proof);
    /// `None` on the hermetic offline path, where every image lookup returns
    /// `None` and the procedural gradient/initials fallback shows.
    images: Option<ui::ImageStore>,
    /// When the videos the screens wait for were first waited for.
    waiting: screens::Waiting,

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
            wake: ui::WakeHandle::detached(),
            #[cfg(feature = "grayjay-sync")]
            device_sync: crate::sync::DeviceSync::default(),
            #[cfg(feature = "grayjay-sync")]
            device_sync_seeded: false,
            shell,
            can_go_back,
            shell_built: false,
            toasted: 0,
            shell_stale: true,
            shell_key: None,
            child_node: None,
            rect: Rect::ZERO,
            structure_dirty: false,
            search_query: ui::LocalState::new(String::new()),
            palette_query: String::new(),
            sidebar_open: None,
            force_sidebar_open_shot: false,
            online: false,
            kicked_off: false,
            playing_id: None,
            // No engine until something plays: see `playing_player`.
            player: Rc::new(RefCell::new(VideoPlayer::new())),
            player_video: None,
            resume_at_ms: None,
            progress_written: None,
            images: None,
            waiting: screens::Waiting::default(),
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

    /// The window shell: a docked sidebar of destinations beside the
    /// screen, the screen's title and actions in the window's bar, and
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
            .content_kind(ui::widget::ContentKind::Grid)
            .with_id("tube_shell")
            .with_window_toolbar()
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
            TubeEffect::Search { query } => {
                let query = query.trim();
                if query.is_empty() {
                    return Err("Say what to search for".into());
                }
                self.search_query.set(query.to_owned());
                self.apply(Action::SubmitSearch);
                self.dismiss_overlay_rail();
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
                // A window opened on a history that already knows where each
                // video was left keeps those places, so continuing works here
                // too.
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

    /// Show the history another window or device stored, including where
    /// each video was left: continuing a video on this device continues from
    /// the minute another one stopped at.
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

    /// Where each video the history knows about was left, from the person's
    /// record or (in a capture or a test) the window's own replica.
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

    /// Bring in what an approved Grayjay device sent: its subscriptions as
    /// library edits through the same door as the visible controls, its
    /// watch history into the person's own, its settings into the person's
    /// preferences.
    ///
    /// The transport carries a compact, bounded metadata package; every
    /// imported identifier is validated here first.
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
        // Device sync starts at framework attachment, using that host's secret
        // store and paired-device settings rather than opening a second store.
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

    /// The UI context the screens are built from.
    fn ui<'a>(&'a self, theme: &'a Theme) -> screens::Ui<'a> {
        screens::Ui {
            model: &self.model,
            theme,
            bus: &self.bus,
            imgs: crate::images::Imgs(self.images.as_ref()),
            content_width: self.content_width(theme),
            waiting: &self.waiting,
        }
    }

    /// The current screen, ready for the shell's content column.
    fn build_screen(&self, theme: &Theme) -> Box<dyn Widget> {
        self.waiting.begin_build();
        let screen = self.build_route(theme);
        self.waiting.end_build();
        screen
    }

    fn build_route(&self, theme: &Theme) -> Box<dyn Widget> {
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

    /// The navigation sidebar: Tube's destinations, the current one
    /// selected. The shell shows them as a rail by default and as a list
    /// once the person widens it.
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

    /// Show a notice pushed since the last frame as the shell's toast.
    fn show_new_notices(&mut self) {
        use std::hash::{Hash, Hasher};
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        self.model.notices.first().hash(&mut identity);
        let current = identity.finish();
        if current != self.toasted {
            if let Some(notice) = self.model.notices.first() {
                self.shell.toast(notice.clone(), ToastKind::Info);
                tracing::debug!(target: "tube::share", at = ?ui::time::now(), "notice toast created");
            }
            self.toasted = current;
        }
    }

    /// Apply one drained action. Returns true if the model changed enough to
    /// warrant a structural rebuild.
    fn apply(&mut self, action: Action) -> bool {
        match action {
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
            Action::ToggleSearchType(t) => {
                self.model.search_type_filter.click(t);
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
                // Forget the attempt, so the page asks for the stream anew.
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
        let mut entries = crate::search_type::model_entries(&self.model);
        entries.extend(crate::search_type::query_entry(&self.palette_query));
        entries
    }

    /// Take the platform search's latest query.
    pub(crate) fn follow_palette_query(&mut self, query: &str) {
        query.trim().clone_into(&mut self.palette_query);
    }
}

impl Widget for TubeApp {
    ui::provides!(dyn ui::widget::FullscreenProvider);

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.take_window_toolbar_events();
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
        let rect = ui::widget::layout_rect(taffy, node, origin);
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
    }

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
                self.last_pointer = ctx.logical(position);
                if self.back_gesture.pointer_move(self.last_pointer) == Some(ui::NavAction::Back)
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

        self.shell.handle_event(event, ctx);

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

    fn take_structure_dirty(&mut self) -> bool {
        let structure_dirty = std::mem::take(&mut self.structure_dirty);
        // Fold any results that arrived between events so live data still
        // lands when the user is idle.
        let dirty = self.pump() || structure_dirty;
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
        // Keep syncing while images are still loading so freshly-decoded
        // thumbnails/avatars repaint the moment they land. The worker's
        // results need no polling: each one wakes the window
        // (`api::Updates`). The worker lives as long as the window, so
        // asking for idle syncs while it existed asked for them forever.
        self.images_pending()
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
        resolving || self.images_pending() || self.shell.needs_animation_frame()
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
        Some(
            AccessNode::new(Role::Window)
                .with_name(format!("Tube — {route}"))
                .with_state(AccessState::new().busy(self.model.feed_loading)),
        )
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
        self.shell.take_search_request()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests;
