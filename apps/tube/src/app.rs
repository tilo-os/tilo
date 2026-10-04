//! `TubeApp` — the root widget.
//!
//! Tube renders a single in-memory [`TubeModel`]. The shared library in it
//! (likes, queue, subscriptions, comment likes, settings) is what the
//! resource was last presented; the catalogue, navigation, playback and
//! notices are this window's own, and the watch history the person's.
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
use ui::time::UNIX_EPOCH;
use ui::widget::{AppModel, ModelPath};

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::framework::{Dispatcher, Setting};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::ActionDecl;
use ui::widget::{AuthoredExtent, FullscreenProvider, NavSplit, SplitMode, VideoPlayer, Widget};

use crate::action::{self, Action, ActionBus};
use crate::actions::{TubeAction, TubeEffect};
use crate::api::{Request, Update, Worker};
use crate::history::WatchHistory;
use crate::model::{Route, TubeModel};
use crate::resource::{Tube, TubeShared};
use crate::screens;
use crate::widgets::{Flex, SharedPlayer};

pub(crate) const QUALITIES: [&str; 4] = ["1080p", "720p", "480p", "auto"];

fn history_now_ms() -> u64 {
    ui::time::wall()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Pull the frames the player's engine decoded into its surface, as the
/// player's own `sync` does every frame; it reads no document.
fn pump_player(player: &mut VideoPlayer) {
    player.sync(&mut (), &());
}

/// Below this allocated width (logical px) the navigation rail stops sitting
/// beside the content and collapses into an overlay flap summoned by the
/// hamburger. Chosen so a readable column of 300px video cards plus the rail
/// stop fitting comfortably side by side.
const NAV_BREAKPOINT: f64 = 760.0;
/// At large accessibility text a compact icon rail wastes less usable width
/// than it saves: the labelled drawer is clearer and preserves a meaningful
/// reading column. This threshold intentionally includes a normal desktop
/// window at 2× text.
const LARGE_TEXT_NAV_BREAKPOINT: f64 = 1_760.0;

/// Right-edge lane (logical px) reserved for the vertical scrollbar on every
/// scroll view, so the bar never draws over the content (the video player in
/// particular). Sized to the bar's footprint (8px thumb + 2px margins) plus a
/// little breathing room.
pub const SCROLL_GUTTER: f32 = 14.0;

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
    dispatcher: Option<Dispatcher<Tube>>,
    /// The person's watch history as their settings hold it, in a live
    /// window. A capture keeps its sample history in the model alone.
    history: Option<Setting<WatchHistory>>,
    bus: ActionBus,
    worker: Option<Worker>,
    #[cfg(feature = "grayjay-sync")]
    device_sync: crate::sync::DeviceSync,
    #[cfg(feature = "grayjay-sync")]
    device_sync_seeded: bool,

    /// The shell tree (top bar + nav rail + screen). Rebuilt in
    /// `build_layout` only when the structure, theme or width changed; raw
    /// pointers in the App's `node_map` point into it and stay valid until
    /// the next rebuild.
    root_child: Option<Box<dyn Widget>>,
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
    /// Process-local text for the transient Ctrl+K overlay. This intentionally
    /// survives shell rebuilds in this app instance but is never persisted or
    /// sent to another device.
    palette_query: ui::LocalState<String>,

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
    /// Last resolved docked/overlay state, to detect a breakpoint crossing on
    /// resize and trigger a rebuild that re-seeds the split + drops a stale
    /// user preference.
    last_nav_overlay: Option<bool>,
    /// The docked/overlay resolution the *current* widget tree was seeded
    /// with. The first `build_layout` runs before any width is known (rect
    /// ZERO ⇒ overlay assumed), so once the true width lands in
    /// `apply_layout` we compare against this and rebuild if the seed was
    /// wrong — a wide window re-seeds its rail open on the next frame instead
    /// of being stuck on the unknown-width default.
    built_overlay: Option<bool>,
    /// `NAV_BREAKPOINT` as the live split resolves it under the theme the
    /// tree was last built with (`AuthoredExtent::text`, HIG §9.6). The app's
    /// own narrow/wide decisions compare against this, never the constant,
    /// so they agree with the split at every text scale.
    nav_breakpoint: f64,
    /// `screens::NAV_RAIL_W` resolved like the split's icon sidebar
    /// (`AuthoredExtent::icon`: follows `ui_scale`, not the text scale).
    nav_rail_w: f64,

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
    /// Last theme text scale observed during layout. The root owns responsive
    /// chrome decisions because `NavSplit` needs the same effective breakpoint
    /// as the rest of the shell.
    text_scale: f32,
}

impl Default for TubeApp {
    fn default() -> Self {
        Self::new()
    }
}

impl TubeApp {
    pub fn new() -> Self {
        Self {
            model: crate::sample::model(),
            dispatcher: None,
            history: None,
            bus: action::new_bus(),
            worker: None,
            #[cfg(feature = "grayjay-sync")]
            device_sync: crate::sync::DeviceSync::default(),
            #[cfg(feature = "grayjay-sync")]
            device_sync_seeded: false,
            root_child: None,
            shell_stale: true,
            shell_key: None,
            child_node: None,
            rect: Rect::ZERO,
            structure_dirty: false,
            search_query: ui::LocalState::new(String::new()),
            palette_query: ui::LocalState::new(String::new()),
            sidebar_open: None,
            force_sidebar_open_shot: false,
            last_nav_overlay: None,
            built_overlay: None,
            nav_breakpoint: NAV_BREAKPOINT,
            nav_rail_w: screens::NAV_RAIL_W,
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
            text_scale: 1.0,
        }
    }

    /// Answer one of Tube's effects, however it was invoked: a control, the
    /// palette, an agent.
    ///
    /// # Errors
    ///
    /// Returns why the effect cannot run: a screen that does not exist, or
    /// device sync that refused the request.
    pub(crate) fn perform(&mut self, effect: TubeEffect) -> Result<(), String> {
        self.structure_dirty = true;
        match effect {
            TubeEffect::ShowScreen { screen } => {
                let route = TubeEffect::screen(&screen)
                    .ok_or_else(|| format!("there is no screen `{screen}`"))?;
                self.navigate_to(route);
                self.dismiss_overlay_rail();
            }
            TubeEffect::ToggleSidebar => {
                let now = self.sidebar_currently_open();
                self.sidebar_open = Some(!now);
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
                let url = format!("https://youtu.be/{video}");
                ui::clipboard_copy(&url);
                self.model.push_notice(format!("Link copied: {url}"));
            }
            TubeEffect::Download { video } => {
                // Published as unavailable; a caller that invokes it anyway
                // learns why.
                return Err(format!(
                    "Downloads are unavailable for {video}: no storage capability"
                ));
            }
            TubeEffect::MarkWatched { video } => {
                let at_ms = history_now_ms();
                self.change_history(|history| history.record(&video, at_ms));
            }
            TubeEffect::ForgetWatched { video } => {
                self.change_history(|history| history.forget(&video));
            }
            TubeEffect::ClearHistory => {
                if self.change_history(WatchHistory::clear) {
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
        }
        Ok(())
    }

    /// Submit a library edit through the framework's door, which checks it,
    /// applies it, records the undo step and presents the result.
    fn edit(&self, action: &TubeAction) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.apply(action);
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

    /// Change the watch history and show the result. In a live window the
    /// change is stored in the person's settings, which carry it to their
    /// other devices; returns false (and says so) when it cannot be stored.
    fn change_history(&mut self, change: impl FnOnce(&mut WatchHistory)) -> bool {
        let Some(history) = &self.history else {
            let mut shown = WatchHistory::default();
            for video in self.model.history.iter().rev() {
                shown.record(video, 0);
            }
            change(&mut shown);
            self.model.history = shown.videos();
            self.structure_dirty = true;
            return true;
        };
        match history.update(change) {
            Ok(()) => {
                self.model.history_notice = None;
                self.model.history = history.get().videos();
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
        let stored = history.get().videos();
        if stored == self.model.history {
            return false;
        }
        self.model.history = stored;
        true
    }

    /// Bring in what an approved Grayjay device sent: its subscriptions and
    /// settings as library edits through the same door as the visible
    /// controls, its watch history into the person's own.
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
                edits.push(TubeAction::SetSubscription {
                    channel_id: subscription,
                    subscribed: true,
                });
            }
        }

        if let Some(settings) = remote.settings {
            use crate::action::SettingFlag;
            let current = &self.model.settings;
            for (setting, enabled, now) in [
                (
                    SettingFlag::SponsorBlock,
                    settings.sponsorblock_enabled,
                    current.sponsorblock_enabled,
                ),
                (
                    SettingFlag::DeArrow,
                    settings.dearrow_enabled,
                    current.dearrow_enabled,
                ),
                (
                    SettingFlag::DeArrowTitles,
                    settings.dearrow_titles,
                    current.dearrow_titles,
                ),
                (
                    SettingFlag::DeArrowThumbnails,
                    settings.dearrow_thumbnails,
                    current.dearrow_thumbnails,
                ),
                (
                    SettingFlag::ReturnDislike,
                    settings.return_dislike_enabled,
                    current.return_dislike_enabled,
                ),
                (SettingFlag::Autoplay, settings.autoplay, current.autoplay),
                (
                    SettingFlag::ShowComments,
                    settings.show_comments,
                    current.show_comments,
                ),
            ] {
                if enabled != now {
                    edits.push(TubeAction::SetSetting { setting, enabled });
                }
            }
            if QUALITIES.contains(&settings.preferred_quality.as_str())
                && current.preferred_quality != settings.preferred_quality
            {
                edits.push(TubeAction::SetPreferredQuality {
                    quality: settings.preferred_quality,
                });
            }
            for category in crate::model::Category::ALL {
                if let Some(mode) = settings.categories.get(category.api_key())
                    && current.categories.get(category.api_key()) != Some(mode)
                {
                    edits.push(TubeAction::SetSponsorCategory {
                        category,
                        mode: *mode,
                    });
                }
            }
        }

        let watched_at_ms = history_now_ms();
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
        let history_changed = !pulled.is_empty()
            && self.change_history(|history| {
                for (video, at_ms) in &pulled {
                    history.import(video, *at_ms);
                }
            });

        let library_changed = !edits.is_empty();
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.group(|| {
                for edit in &edits {
                    dispatcher.apply(edit);
                }
            });
        }
        library_changed || history_changed
    }

    /// Edits bound to the open watch page: like it, queue it, subscribe to
    /// its creator. Each carries the state it sets, read from what the page
    /// shows now.
    pub(crate) fn watch_page_declarations(&self) -> Vec<ActionDecl<Tube>> {
        let Route::Watch(video_id) = &self.model.route else {
            return Vec::new();
        };
        let Some(video) = self.model.video(video_id) else {
            return Vec::new();
        };
        let bound = |id: &'static str, label: &str, action: TubeAction| {
            ActionDecl::new(id, label, Vec::new(), move |_, _| Ok(action.clone()))
        };
        let liked = self.model.is_liked(video_id);
        let queued = self.model.is_queued(video_id);
        let subscribed = self.model.is_subscribed(&video.channel_id);
        let creator = self
            .model
            .subscription_key(&video.channel_id)
            .unwrap_or(&video.channel_id)
            .to_owned();
        vec![
            bound(
                "like-this-video",
                if liked {
                    "Unlike this video"
                } else {
                    "Like this video"
                },
                TubeAction::SetVideoLiked {
                    video_id: video_id.clone(),
                    liked: !liked,
                },
            ),
            bound(
                "queue-this-video",
                if queued {
                    "Remove this video from the queue"
                } else {
                    "Add this video to the queue"
                },
                TubeAction::SetQueueMembership {
                    video_id: video_id.clone(),
                    queued: !queued,
                },
            ),
            bound(
                "subscribe-to-this-creator",
                if subscribed {
                    "Unsubscribe from this creator"
                } else {
                    "Subscribe to this creator"
                },
                TubeAction::SetSubscription {
                    channel_id: creator,
                    subscribed: !subscribed,
                },
            ),
        ]
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
        #[cfg(feature = "grayjay-sync")]
        {
            self.device_sync = crate::sync::DeviceSync::production("Tilo Tube");
        }
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
        self.search_query.set(model.search_query.clone());
        self.palette_query.set(model.palette_query.clone());
        self.model = model;
        self
    }

    /// Show the shared library the resource holds: likes, the queue,
    /// subscriptions, comment likes and settings. Every presentation is the
    /// whole library, so an emptied one (everything unliked, the queue
    /// cleared) shows empty.
    pub(crate) fn present_library(&mut self, state: &TubeShared) {
        self.model.liked = TubeShared::on(&state.likes);
        self.model.queue = TubeShared::on(&state.queue);
        self.model.subscriptions = TubeShared::on(&state.subscriptions);
        self.model.comment_likes.clone_from(&state.comment_likes);
        self.model.settings.clone_from(&state.settings);
        self.structure_dirty = true;
    }

    /// Attach the framework's door for the resource and, in a live window,
    /// the person's watch history.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: Dispatcher<Tube>,
        history: Option<Setting<WatchHistory>>,
    ) {
        self.dispatcher = Some(dispatcher);
        self.history = history;
        self.follow_history();
    }

    /// `--shots` builder: start on an EMPTY Home in its loading state, so a still
    /// captures the skeleton the real (online) app shows on a cold start before
    /// live results land — proving the no-fake-content default reads as
    /// "loading", not a blank page.
    #[must_use]
    pub fn with_loading_home_shot(mut self) -> Self {
        self.show_loading_home();
        self
    }

    fn show_loading_home(&mut self) {
        self.model = TubeModel::default();
        self.model.feed_loading = true;
        self.model.route = Route::Home;
    }

    /// `--shots` builder for the terminal offline/empty state after a live
    /// fetch failed. Kept distinct from the cold-start loading skeleton.
    #[must_use]
    pub fn with_empty_home_shot(mut self) -> Self {
        self.show_empty_home();
        self
    }

    fn show_empty_home(&mut self) {
        self.model = TubeModel::default();
        self.model.data_source.label = "offline — network unavailable".into();
        self.model.route = Route::Home;
    }

    /// `--shots` builder: render at the narrow breakpoint with the navigation
    /// rail summoned as an open overlay flap, so the offscreen still shows the
    /// collapsed-sidebar state. The live app never calls this.
    #[must_use]
    pub fn with_open_sidebar_shot(mut self) -> Self {
        self.force_sidebar_open_shot = true;
        self
    }

    /// Stage the view state of the screenshot scenario `scenario` (see
    /// [`crate::resource::SCENARIOS`]) over the bundled sample catalogue.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        match scenario {
            "empty" => self.show_empty_home(),
            "loading" => self.show_loading_home(),
            #[cfg(feature = "grayjay-sync")]
            "device-sync" => self.show_device_sync_sample(),
            "palette" => self.open_palette_with("rust"),
            "play-demo" => self.play_demo(),
            "narrow" => self.force_sidebar_open_shot = true,
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

    /// `--shots` builder: record the render viewport up front so the single
    /// offscreen frame resolves the docked-vs-overlay layout — and thus the
    /// rail's default open/closed state and the width-driven watch player —
    /// from the true width instead of the unknown-width fallback. Without this
    /// the one-and-only build would seed the split from a ZERO width. The live
    /// app never calls it; it learns its width from `apply_layout` each frame.
    #[must_use]
    pub fn with_viewport(mut self, w: f64, h: f64) -> Self {
        self.rect = Rect::new(0.0, 0.0, w.max(0.0), h.max(0.0));
        self.last_nav_overlay = Some(self.nav_is_overlay());
        self
    }

    /// `--shots` builder: start on a specific [`Route`] so each screen can be
    /// captured for self-inspection (e.g. the selected-nav-item / selected-chip
    /// contrast). Pure presentation override — the live app starts from the
    /// sample/persisted route instead. If the route is a search, the query is
    /// mirrored so the search header + active chip resolve.
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
        self.nav = ui::NavStack::new(route.clone());
        self.model.route = route;
    }

    /// Secret-free offscreen proof of the real device lifecycle surface. The
    /// route is scrolled to the card so wide and narrow screenshots exercise
    /// pending approval, active sync, revocation, and broker-key messaging.
    #[cfg(feature = "grayjay-sync")]
    #[must_use]
    pub fn with_device_sync_shot(mut self) -> Self {
        self.show_device_sync_sample();
        self
    }

    #[cfg(feature = "grayjay-sync")]
    fn show_device_sync_sample(&mut self) {
        self.model.route = Route::Settings;
        self.device_sync = crate::sync::DeviceSync::visual_sample();
        let y = if self.rect.width() < self.nav_breakpoint {
            1_330.0
        } else {
            930.0
        };
        self.scroll_cells
            .borrow_mut()
            .insert("settings".into(), Rc::new(Cell::new((0.0, y))));
    }

    /// Test/shots hook: start with the Ctrl+K command palette already open, and
    /// optionally pre-seed its query. Used by headless click-tests (which can't
    /// synthesise the Ctrl+K key event — winit's `KeyEvent` is not constructible
    /// outside the crate) and by the palette `--shots`.
    #[must_use]
    pub fn with_palette_open_for_test(mut self, query: &str) -> Self {
        self.open_palette_with(query);
        self
    }

    fn open_palette_with(&mut self, query: &str) {
        self.model.palette_open = true;
        self.model.palette_query = query.to_string();
        self.palette_query.set(query.to_string());
    }

    /// Test/shots hook: open a watch page with the shared [`VideoPlayer`] already
    /// playing a demo source, so a headless `--shots` still captures the real
    /// player surface (a frame + the transport controls) without a network or a
    /// live decoder. Uses the media engine's synthesized-frame backend, which
    /// seeds a frame on `open` — the exact surface the live decode path paints.
    #[must_use]
    pub fn with_playing_demo_for_test(mut self) -> Self {
        self.play_demo();
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

    /// Test hook: arrive on a search-results view exactly as a real search would
    /// leave the app — the route, the query, the ordered video hits, and the nav
    /// stack pushed to `[Home, Search]` so opening a result and pressing Back
    /// returns here. Used to drive the search → video → Back round trip.
    #[must_use]
    pub fn with_search_for_test(mut self, query: &str, video_ids: Vec<String>) -> Self {
        self.model.search_query = query.to_string();
        self.model.search_hits_query = query.to_string();
        self.model.search_hits = video_ids;
        self.model.route = Route::Search(query.to_string());
        self.nav.push(Route::Search(query.to_string()));
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

    /// Test accessor: whether the command palette is open.
    pub fn palette_open_for_test(&self) -> bool {
        self.model.palette_open
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
        self.sidebar_currently_open()
    }

    fn build_shell_themed(&self, theme: &Theme) -> Box<dyn Widget> {
        let m = &self.model;
        let bus = &self.bus;
        let imgs = crate::images::Imgs(self.images.as_ref());
        let content = match &m.route {
            Route::Home => screens::home(m, theme, bus, imgs),
            Route::Trending => screens::trending(m, theme, bus, imgs),
            Route::Subscriptions => screens::subscriptions(m, theme, bus, imgs),
            Route::Queue => screens::queue(m, theme, bus, imgs, self.content_width()),
            Route::History => screens::history(m, theme, bus, imgs),
            Route::Settings => {
                #[cfg(feature = "grayjay-sync")]
                {
                    screens::settings_with_sync(
                        m,
                        theme,
                        bus,
                        self.device_sync.view(),
                        self.device_sync.pairing_input.clone(),
                    )
                }
                #[cfg(not(feature = "grayjay-sync"))]
                {
                    screens::settings(m, theme, bus)
                }
            }
            Route::Watch(id) => screens::watch(
                m,
                theme,
                bus,
                imgs,
                id,
                (self.content_width(), self.content_height()),
                self.player.clone(),
            ),
            Route::Channel(id) => screens::channel(m, theme, bus, imgs, id),
            Route::Playlist(id) => screens::playlist(m, theme, bus, imgs, id),
            Route::Search(q) => screens::search(m, theme, bus, imgs, q),
        };

        // Preserve each view's scroll position across the whole-tree rebuilds
        // Tube does on every change (and restore it when the view is returned to
        // via Back), and reserve the right-edge scrollbar lane so the bar never
        // draws over the content (the video player in particular).
        let content = content
            .with_persisted_offset(self.scroll_cell_for(&m.route))
            .with_content_gutter(SCROLL_GUTTER);

        // `fill_w` pins the content pane to a *definite* width (the split lays
        // each pane out in its own tree, so without this the column would size
        // to its max-content and the card grid inside would never learn a line
        // width to wrap against — cards would overflow and clip). With a
        // definite width the wrapping grid reflows to fewer columns as the
        // window narrows.
        let content_area = Flex::col().grow(1.0).fill_w().fill_h().clip().push(content);

        // The navigation rail and the content share the body via a responsive
        // split: docked beside the content on a wide window, collapsed into an
        // overlay flap (behind a dimming scrim, summoned by the top-bar
        // hamburger) on a narrow one. The split self-routes the scrim-click /
        // Esc dismissal and resolves docked-vs-overlay from its own allocated
        // width against `NAV_BREAKPOINT`.
        let mut split = NavSplit::new(
            Box::new(screens::nav_rail(m, theme, bus, self.nav_rail_width())),
            Box::new(content_area),
        )
        .with_mode(SplitMode::Adaptive)
        .with_breakpoint(self.nav_breakpoint())
        .with_sidebar_width(self.nav_rail_width());

        // Seed the freshly-built split's open state deterministically. The
        // split is recreated each time the tree is rebuilt (navigation), and
        // the live `ui::App` never drives its eased `update()` tick, so we snap
        // it (no ease) to the state resolved from the current width + the
        // session preference. Without this the recreated split would render
        // from a stale reveal — e.g. the flap drawn open on a fresh narrow
        // window — because nothing advances the animation toward the default.
        let open = if self.force_sidebar_open_shot {
            true
        } else {
            self.sidebar_currently_open()
        };
        split.snap_open(open);

        let mut root = Flex::col()
            .fill_w()
            .fill_h()
            .push(screens::top_bar(
                m,
                theme,
                bus,
                self.nav.can_go_back(),
                &self.search_query,
                self.text_scale >= 1.5 && self.rect.width() <= 480.0,
            ))
            .push(split);

        // The Ctrl+K command palette overlays the whole shell when open.
        if let Some(palette) = screens::command_palette(m, theme, bus, &self.palette_query) {
            root = root.push(palette.overlay());
        }

        Box::new(root)
    }

    /// Whether the navigation rail is collapsed to an overlay flap at the
    /// current allocated width. Mirrors `SplitLayout::from_width` so the
    /// hamburger toggle and the selection-dismiss resolve the rail's default
    /// the same way the live split does.
    fn nav_is_overlay(&self) -> bool {
        let w = self.rect.width();
        // Width 0 = first build, before the first `apply_layout`. Default to
        // the overlay (narrow) resolution so the rail starts CLOSED when the
        // width is not yet known — never assume the wide layout and flash the
        // flap open over the content. A real width self-corrects on its first
        // laid-out frame: a wide window rebuilds once the true width lands (see
        // `built_overlay` in `apply_layout`).
        w <= 0.0 || w < self.nav_breakpoint()
    }

    fn nav_breakpoint(&self) -> f64 {
        if self.text_scale >= 1.5 {
            LARGE_TEXT_NAV_BREAKPOINT
        } else {
            NAV_BREAKPOINT
        }
    }

    fn nav_rail_width(&self) -> f64 {
        if self.text_scale >= 1.5 && self.rect.width() <= 480.0 {
            // At phone width with 200% text, an overlay that leaves a thin
            // strip of clipped content is visually noisy and inaccessible.
            // The drawer owns the entire content region until dismissed.
            return self.rect.width();
        }
        if self.text_scale >= 1.5 {
            264.0
        } else {
            screens::NAV_RAIL_W
        }
    }

    /// Logical width available to the screen content, i.e. the split's content
    /// pane. In overlay mode the flap draws over the content, so the content
    /// spans the full width; docked, an open rail insets the content by
    /// [`screens::NAV_RAIL_W`]. Used to size the width-driven watch player and
    /// pick the watch page's one-vs-two-column arrangement.
    fn content_width(&self) -> f64 {
        let w = self.rect.width();
        if self.nav_is_overlay() {
            w
        } else if self.sidebar_currently_open() {
            (w - self.nav_rail_width()).max(0.0)
        } else {
            w
        }
    }

    /// Logical viewport height that sizes the height-driven watch-page player
    /// (capped at a fraction of this in `screens::watch`). The full window
    /// height — the player is deliberately allowed to occupy a large share of the
    /// viewport so the actual video is big, with metadata scrolling below it.
    fn content_height(&self) -> f64 {
        self.rect.height().max(1.0)
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
            Route::Watch(id) => format!("watch:{id}"),
            Route::Channel(id) => format!("channel:{id}"),
            Route::Playlist(id) => format!("playlist:{id}"),
            Route::Search(q) => format!("search:{q}"),
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
    /// return) and mirror it onto the live route. A search refining into another
    /// search replaces the current entry rather than stacking each keystroke.
    fn navigate_to(&mut self, route: Route) {
        let refine = matches!(
            (self.nav.current(), &route),
            (Route::Search(_), Route::Search(_))
        );
        if refine {
            self.nav.replace_current(route.clone());
        } else {
            self.nav.push(route.clone());
        }
        self.model.route = route;
    }

    /// Pop the nav stack (Back). Returns true when it navigated somewhere.
    fn go_back(&mut self) -> bool {
        if let Some(route) = self.nav.back().cloned() {
            self.model.route = route;
            self.dismiss_overlay_rail();
            true
        } else {
            false
        }
    }

    /// Resolve whether the rail is currently shown, given the session
    /// preference and the layout default (overlay ⇒ closed, docked ⇒ open).
    fn sidebar_currently_open(&self) -> bool {
        self.sidebar_open.unwrap_or_else(|| !self.nav_is_overlay())
    }

    /// Close the rail when it is an overlay flap (narrow window) — selecting a
    /// destination should dismiss the summoned menu, the standard flap
    /// behaviour. A no-op while the rail is docked beside the content.
    fn dismiss_overlay_rail(&mut self) {
        if self.nav_is_overlay() {
            self.sidebar_open = Some(false);
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
    fn apply_player_setting_event(&self, ev: ui::widget::video_player::PlayerSettingEvent) {
        use ui::widget::video_player::PlayerSettingEvent as E;
        if let E::Chose { id, index } = ev
            && id == "resolution"
            && let Some(q) = QUALITIES.get(index)
            && self.model.settings.preferred_quality != *q
        {
            self.edit(&TubeAction::SetPreferredQuality {
                quality: (*q).to_owned(),
            });
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
                        format!(
                            "Skipped {} ({}–{})",
                            s.category.label(),
                            crate::util::clock(s.start),
                            crate::util::clock(s.end)
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
    /// Shared by the card/palette open paths and by autoplay-next so they stay
    /// in lockstep.
    fn open_video(&mut self, id: String) {
        let watched_at_ms = history_now_ms();
        self.change_history(|history| history.record(&id, watched_at_ms));
        self.navigate_to(Route::Watch(id.clone()));
        self.model.playhead = 0.0;
        self.model.playing = true;
        self.request(Request::Branding(id.clone()));
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
                // Share belongs to the open watch page. Refuse a stale leaf
                // event rather than copying a link for a video no longer shown.
                if matches!(&self.model.route, Route::Watch(current) if current == &id) {
                    let _ = self.perform(TubeEffect::Share { video: id });
                }
            }
            Action::OpenPalette => {
                self.model.palette_open = true;
            }
            Action::ClosePalette => {
                self.model.palette_open = false;
            }
            Action::ActivateHit(link) => {
                use crate::search_type::SearchHit;
                if let Some(id) = SearchHit::parse_watch_link(&link) {
                    self.open_video(id);
                } else if let Some(id) = SearchHit::parse_channel_link(&link) {
                    self.navigate_to(Route::Channel(id.clone()));
                    self.request_live(Request::Channel(id));
                }
                self.model.palette_open = false;
                self.dismiss_overlay_rail();
            }
            Action::Edit(edit) => {
                self.edit(&edit);
                return false;
            }
            Action::Confirm(effect) => {
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

impl Widget for TubeApp {
    ui::provides!(
        dyn ui::widget::FullscreenProvider,
        dyn ui::search::capability::SearchSource
    );

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        if self.follow_history() {
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
        // Reflect the process-local palette query into the rendering model so
        // the overlay filters while typing. This is intentionally not a CRDT
        // read: a collaborator's document cannot open or rewrite local UI.
        if self.model.palette_open {
            let live = self.palette_query.get();
            if live != self.model.palette_query {
                self.model.palette_query = live;
                self.structure_dirty = true;
            }
        }
        if let Some(c) = &mut self.root_child {
            c.sync(state, path);
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
        self.text_scale = theme.text_scale;
        // Resolve the split's authored breakpoint and rail width under this
        // theme before judging narrow/wide, so the seed below and the split
        // built from it read the same numbers.
        self.nav_breakpoint = AuthoredExtent::text(NAV_BREAKPOINT).resolve(theme);
        self.nav_rail_w = AuthoredExtent::icon(screens::NAV_RAIL_W).resolve(theme);
        // Remember which layout resolution this tree was seeded with, so a
        // later `apply_layout` at the true width can detect a wrong seed (the
        // first build runs before any width is known) and rebuild once.
        // A host may lay out after every input event. Rebuilding then would
        // drop the hover and press a click is made of.
        let key = {
            use std::hash::{Hash as _, Hasher as _};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            format!("{theme:?}").hash(&mut hasher);
            self.rect.width().to_bits().hash(&mut hasher);
            hasher.finish()
        };
        if self.root_child.is_none()
            || self.shell_stale
            || self.structure_dirty
            || self.shell_key != Some(key)
        {
            self.built_overlay = Some(self.nav_is_overlay());
            let shell = self.build_shell_themed(theme);
            self.root_child = Some(shell);
            self.shell_stale = false;
            self.shell_key = Some(key);
        }
        #[allow(
            clippy::unwrap_used,
            reason = "root_child was just set to Some(shell) on the line above; \
                      Widget::build_layout returns NodeId directly with no fallible path"
        )]
        let node = self
            .root_child
            .as_mut()
            .unwrap()
            .build_layout(taffy, theme, node_map);
        self.child_node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = crate::widgets::node_layout(taffy, node);
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        if let Some(c) = &mut self.root_child {
            c.apply_layout(taffy, node, origin);
        }

        // Detect a breakpoint crossing on resize (which does not otherwise
        // dirty the tree). On a crossing, drop a stale per-session preference
        // so the rail adopts the new layout's default, and request a rebuild
        // so the split is re-seeded at the new width.
        if self.rect.width() > 0.0 {
            let overlay = self.nav_is_overlay();
            if self.last_nav_overlay.is_some() && self.last_nav_overlay != Some(overlay) {
                self.sidebar_open = None;
                self.structure_dirty = true;
            }
            self.last_nav_overlay = Some(overlay);
            // The first build ran before the width was known and seeded the
            // tree from the unknown-width (overlay) default. Now that the true
            // width has landed, rebuild if the resolved layout differs from
            // what the current tree was built with — this re-seeds a wide
            // window's rail open. It converges in one frame (the rebuilt tree
            // is seeded from the now-known width) and never loops.
            if self.built_overlay != Some(overlay) {
                self.structure_dirty = true;
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        if let Some(c) = &mut self.root_child {
            c.draw(scene, transform, text_renderer, theme, presence);
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
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        if let Some(c) = &mut self.root_child {
            c.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
            );
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the Widget::handle_event trait method fans out on WindowEvent kind; each \
                  branch is a short, independent dispatch to an already-extracted helper"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Ctrl+K toggles the global command palette, regardless of focus. This
        // is intercepted before children so a focused text field can't swallow
        // it. `Ctrl+K` opens; a second `Ctrl+K` (or Esc, below) closes.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && ctx.modifiers.state().control_key()
            && c.eq_ignore_ascii_case("k")
        {
            action::push(
                &self.bus,
                if self.model.palette_open {
                    Action::ClosePalette
                } else {
                    Action::OpenPalette
                },
            );
            *ctx.handled = true;
        }

        // Enter submits a search regardless of which field has focus.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Enter),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            action::push(&self.bus, Action::SubmitSearch);
        }

        // Back-navigation via the keyboard: Alt+Left / the BrowserBack key pop the
        // nav stack (the Back button, Escape below and the edge-swipe do too).
        // Only when there is history and no overlay owns the key.
        if !self.model.palette_open
            && let WindowEvent::KeyboardInput {
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

        if let Some(c) = &mut self.root_child {
            c.handle_event(event, ctx);
        }

        // Esc dismisses an open overlay flap. The live split closes itself on
        // Esc / scrim-click; mirror that into our session preference so the
        // next structural rebuild re-seeds the rail closed rather than
        // snapping it back open.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            // Esc closes the command palette first (it's the topmost overlay),
            // then an open overlay flap, then finally acts as Back.
            if self.model.palette_open {
                action::push(&self.bus, Action::ClosePalette);
                *ctx.handled = true;
            } else if self.nav_is_overlay() && self.sidebar_currently_open() {
                self.sidebar_open = Some(false);
            } else if self.nav.can_go_back() {
                action::push(&self.bus, Action::Back);
                *ctx.handled = true;
            }
        }

        // Drain & apply queued actions.
        let actions = action::drain(&self.bus);
        let mut changed = false;
        for a in actions {
            changed |= self.apply(a);
        }
        // If a just-applied action opened a watch page, kick off real playback
        // (resolve + decode) for it — once. No-op off the watch route / offline.
        self.ensure_playback();
        // Fold any ready worker results (live data + decoded frames).
        changed |= self.pump();

        if changed {
            self.structure_dirty = true;
        }
    }

    fn on_external_event(&mut self, _payload: &str, _state: &mut AppModel, _root: &ModelPath) {}

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
        if let Some(c) = &mut self.root_child {
            dirty |= c.take_structure_dirty();
        }
        self.shell_stale |= dirty;
        dirty
    }

    fn needs_redraw(&self) -> bool {
        self.structure_dirty
            || self.images_pending()
            || self.root_child.as_ref().is_none_or(|c| c.needs_redraw())
    }

    fn mark_drawn(&mut self) {
        if let Some(c) = &mut self.root_child {
            c.mark_drawn();
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.root_child.as_ref().is_none_or(|c| c.is_layout_dirty())
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
        // player's own `needs_animation_frame` (via `root_child` — is_playing or
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
            || self
                .root_child
                .as_ref()
                .is_some_and(|c| c.needs_animation_frame())
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.root_child
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.root_child
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
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
            Route::Search(query) => format!("Search results for {query}"),
            Route::History => "Watch history".to_string(),
            Route::Settings => "Settings".to_string(),
        };
        let mut node = AccessNode::new(Role::Window)
            .with_name(format!("Tube — {route}"))
            .with_description(if self.model.data_source.live {
                "Privacy-respecting video client; live catalogue".to_string()
            } else {
                "Privacy-respecting video client; offline".to_string()
            })
            .with_state(
                AccessState::new().busy(self.model.feed_loading || self.model.search_loading),
            );
        if let Some(notice) = self.model.notices.first() {
            node.live = Some(Politeness::Polite);
            node.value = Some(notice.clone());
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
}

impl ui::search::capability::SearchSource for TubeApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_type::TubeSearchProvider::new(
            &self.model,
        ))]
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::model::Category;
    use automerge::{AutoCommit, ReadDoc};
    use ui::tilo_resource::Resource;

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
        let mut library = TubeShared::default();
        library.likes.insert("vid_donut".into(), true);
        library.queue.insert("vid_donut".into(), true);
        library.comment_likes.insert("vid_donut#1".into(), true);
        app.present_library(&library);
        assert!(app.model.is_liked("vid_donut") && app.model.is_queued("vid_donut"));
        assert_eq!(app.model.comment_likes.get("vid_donut#1"), Some(&true));

        // A collaborator unliked and unqueued everything.
        library.likes.insert("vid_donut".into(), false);
        library.queue.insert("vid_donut".into(), false);
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
    fn settings_and_queries_keep_shared_and_local_state_apart() {
        // Case `collaborative_settings_reconcile_without_serializing_device_ui_state`.
        {
            let (mut app, mut doc) = fresh();
            app.model.route = Route::Watch("vid_donut".into());
            app.model.playhead = 42.0;
            tilo_actions::Action::apply(
                &crate::actions::TubeAction::SetSetting {
                    setting: crate::action::SettingFlag::DeArrow,
                    enabled: false,
                },
                &mut doc,
            )
            .expect("structured setting action applies");
            app.present_library(&crate::resource::Tube::read(&doc).unwrap());
            Widget::sync(&mut app, &mut doc, &automerge::ROOT);
            assert!(!app.model.settings.dearrow_enabled);
            assert!(
                doc.get(&automerge::ROOT, "tube_state")
                    .expect("document read")
                    .is_none(),
                "route, playback and fetched catalogue are never mirrored into collaboration"
            );
        }
        // Case `search_and_palette_queries_stay_local_to_each_tube_instance`.
        {
            use automerge::transaction::Transactable;

            let (mut first, mut doc) = fresh();
            first.search_query.set("rust async".into());
            first.palette_query.set("compositor".into());
            first.model.palette_open = true;
            Widget::sync(&mut first, &mut doc, &automerge::ROOT);

            assert_eq!(first.model.palette_query, "compositor");
            assert!(
                doc.get(&automerge::ROOT, "search_query")
                    .expect("document read")
                    .is_none()
            );
            assert!(
                doc.get(&automerge::ROOT, "palette_query")
                    .expect("document read")
                    .is_none()
            );

            let mut second = TubeApp::new();
            Widget::sync(&mut second, &mut doc, &automerge::ROOT);
            assert!(second.search_query.get().is_empty());
            assert!(second.palette_query.get().is_empty());
            assert!(second.model.palette_query.is_empty());
            assert_eq!(first.search_query.get(), "rust async");
            assert_eq!(first.palette_query.get(), "compositor");

            // Legacy documents may still contain the old keys. A new app must not
            // adopt either value merely because a collaborator's older client
            // emitted it.
            let mut legacy_doc = AutoCommit::new();
            legacy_doc
                .put(&automerge::ROOT, "search_query", "remote query")
                .expect("legacy search seed");
            legacy_doc
                .put(&automerge::ROOT, "palette_query", "remote palette")
                .expect("legacy palette seed");
            let mut third = TubeApp::new();
            Widget::sync(&mut third, &mut legacy_doc, &automerge::ROOT);
            assert!(third.search_query.get().is_empty());
            assert!(third.palette_query.get().is_empty());
        }
    }

    // --- responsive navigation rail ---

    /// Pretend the app was allocated `w` logical px wide (as `apply_layout`
    /// would record), so width-driven layout decisions can be exercised.
    fn at_width(w: f64) -> TubeApp {
        let mut app = TubeApp::new();
        app.rect = Rect::new(0.0, 0.0, w, 800.0);
        app
    }

    #[test]
    fn navigation_rail_docks_wide_and_overlays_narrow() {
        // Case `rail_is_docked_wide_and_overlay_narrow`.
        {
            assert!(
                !at_width(1280.0).nav_is_overlay(),
                "wide window docks the rail"
            );
            assert!(
                !at_width(NAV_BREAKPOINT).nav_is_overlay(),
                "breakpoint is inclusive → docked"
            );
            assert!(
                at_width(NAV_BREAKPOINT - 1.0).nav_is_overlay(),
                "below breakpoint overlays the rail"
            );
        }
        // Case `fresh_unknown_width_defaults_closed`.
        {
            // Before the first laid-out frame the width is unknown (rect ZERO).
            // The rail must NOT assume the wide/docked layout and flash open over
            // the content — it defaults to the overlay resolution (closed).
            let app = TubeApp::new();
            assert_eq!(
                app.rect.width(),
                0.0,
                "a fresh app has no measured width yet"
            );
            assert!(
                app.nav_is_overlay(),
                "unknown width resolves to overlay, not docked"
            );
            assert!(
                !app.sidebar_currently_open(),
                "with the width unknown the rail defaults closed (never assume wide)"
            );
        }
        // Case `shot_viewport_seeds_layout_before_first_frame`.
        {
            // The `--shots` viewport hint lets the single offscreen frame resolve
            // the layout from the true width up front: wide docks + opens, narrow
            // overlays + closes.
            assert!(
                TubeApp::new()
                    .with_viewport(1280.0, 720.0)
                    .sidebar_currently_open(),
                "a wide shot viewport docks and opens the rail"
            );
            assert!(
                !TubeApp::new()
                    .with_viewport(560.0, 720.0)
                    .sidebar_currently_open(),
                "a narrow shot viewport overlays and closes the rail"
            );
        }
        // Case `route_shot_seeds_the_navigation_authority_with_the_visible_route`.
        {
            let app = TubeApp::new().with_route_shot(Route::Search("rust".into()));
            assert!(matches!(&app.model.route, Route::Search(query) if query == "rust"));
            assert!(matches!(app.nav.current(), Route::Search(query) if query == "rust"));
            assert!(
                !app.can_go_back_for_test(),
                "a direct screenshot route has no fabricated prior destination"
            );
        }
        // Case `rail_default_open_when_docked_closed_when_overlay`.
        {
            assert!(
                at_width(1280.0).sidebar_currently_open(),
                "docked rail shows by default"
            );
            assert!(
                !at_width(560.0).sidebar_currently_open(),
                "overlay rail hides by default"
            );
        }
        // Case `toggle_summons_overlay_rail`.
        {
            let (mut app, _) = (at_width(560.0), AutoCommit::new());
            assert!(!app.sidebar_currently_open(), "narrow rail starts closed");
            app.apply(Action::ToggleSidebar);
            assert!(
                app.sidebar_currently_open(),
                "hamburger summons the overlay rail"
            );
            app.apply(Action::ToggleSidebar);
            assert!(
                !app.sidebar_currently_open(),
                "a second toggle dismisses it"
            );
        }
        // Case `selecting_a_destination_dismisses_overlay_rail`.
        {
            let (mut app, _) = (at_width(560.0), AutoCommit::new());
            app.apply(Action::ToggleSidebar); // open it
            assert!(app.sidebar_currently_open());
            app.apply(Action::Navigate(Route::Trending));
            assert!(
                !app.sidebar_currently_open(),
                "navigating closes the overlay flap"
            );
        }
        // Case `docked_rail_is_not_dismissed_on_navigation`.
        {
            let (mut app, _) = (at_width(1280.0), AutoCommit::new());
            app.apply(Action::Navigate(Route::History));
            assert!(
                app.sidebar_currently_open(),
                "the docked rail stays put when navigating"
            );
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
        // Case `rejected_newest_request_clears_loading_and_explains_retry`.
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
            assert!(!app.model.search_loading, "do not leave a stuck skeleton");
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
        // Case `submitting_a_search_enters_the_loading_state`.
        {
            // A live search in flight shows a skeleton (search_loading) rather than a
            // premature "No results".
            let mut app = TubeApp::new();
            app.online = true; // online without spawning the image store's threads
            app.search_query.set("rust".into());
            assert!(!app.model.search_loading);
            app.apply(Action::SubmitSearch);
            assert!(
                app.model.search_loading,
                "an in-flight search must show the skeleton"
            );
            assert!(matches!(&app.model.route, Route::Search(q) if q == "rust"));
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
            let mut doc = AutoCommit::new();
            let r = automerge::ROOT;
            app.sync(&mut doc, &r);
            assert!(
                !Widget::take_structure_dirty(&mut app),
                "an in-flight image load must NOT queue a whole-tree rebuild — images \
             fold in lazily via sync + redraw, not via a structural rebuild",
            );
        }
    }
}
